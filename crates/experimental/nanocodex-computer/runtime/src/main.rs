mod connection_output;

use clap::{Parser, Subcommand};
use connection_output::{ConnectionOutput, OwnedOutput};
use serde_json::{Value, json};
use skyre::{
    Error, Result,
    engine::Engine,
    fixture::Fixture,
    native, protocol,
    worker::{Event, Worker},
};
#[cfg(test)]
use std::io::Write;
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, BufRead, Read},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Use deterministic synthetic controls; never touches OS UI or clipboard.
    #[arg(long, global = true)]
    fixture: bool,
    /// The trusted embedding host authorizes native app control; OS grants and configured app restrictions still apply.
    #[arg(long, global = true)]
    allow_native_control: bool,
    /// Explicit browser endpoint, ID=ws://host:port/devtools/browser/...
    #[arg(long, global = true)]
    cdp: Vec<String>,
    /// Trusted IAB routes, owned CDP endpoints, and current host turn metadata.
    #[arg(long, global = true)]
    iab_config: Option<PathBuf>,
    /// Private capability and durable browser bindings for the trusted host/turn channel.
    #[arg(long, global = true)]
    host_turns_config: Option<PathBuf>,
    /// Private directory for durable host services; disabled when omitted.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Stable owner for durable history; otherwise use an ephemeral process session.
    #[arg(long, global = true)]
    session_id: Option<String>,
    /// Local application/origin policy and explicit broker/reviewer executables.
    #[arg(long, global = true)]
    security_config: Option<PathBuf>,
    /// Trusted runtime documentation environment and confirmation metadata.
    #[arg(long, global = true)]
    runtime_config: Option<PathBuf>,
    /// JavaScript engine (default: quickjs); v8 requires the v8 Cargo feature.
    #[arg(long, global = true)]
    runtime: Option<skyre::runtime::RuntimeBackend>,
    /// JSON array of explicitly trusted platform provider configurations.
    #[arg(long, global = true)]
    platform_config: Option<PathBuf>,
    /// Messaging broker JSON: executable, args, attachment_root.
    #[arg(long, global = true)]
    messaging_config: Option<PathBuf>,
    /// Require this own signing policy before accepting a Unix-socket client.
    #[arg(long, global = true)]
    peer_policy: Option<PathBuf>,
    /// Trusted local lock/intervention monitor; enables required control leases.
    #[arg(long, global = true)]
    guardian_provider: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Inspect this executable's own native grants; --request opens OS consent prompts.
    Permissions {
        #[arg(long)]
        request: bool,
    },
    /// Run a private loopback CDP bridge for the independent Chrome extension.
    ExtensionBridge {
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: String,
        #[arg(long)]
        socket: PathBuf,
    },
    /// Chrome native-messaging stdio endpoint for an existing private bridge.
    ExtensionHost {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Prepare a new local native-messaging manifest and executable wrapper.
    ExtensionManifest {
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        extension_id: String,
    },
    /// Persistent MCP / JSON-RPC on standard input/output.
    Serve {
        #[arg(long)]
        framed: bool,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Evaluate a JavaScript cell in a fresh runtime.
    Eval {
        #[arg(long, conflicts_with = "file")]
        code: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Execute one provider method with JSON arguments.
    Call {
        method: String,
        #[arg(default_value = "{}")]
        args: String,
    },
    /// Print implemented capabilities without accessing UI.
    Capabilities,
}
struct DownloadCell(skyre::download_elicitation::Ticket);
impl Drop for DownloadCell {
    fn drop(&mut self) {
        self.0.finish();
    }
}
struct DownloadApproval(skyre::download_elicitation::Ticket);
impl skyre::security::DownloadApproval for DownloadApproval {
    fn ready_to_prompt(&self) -> Result<bool> {
        self.0.provider_ready()
    }
    fn validate(&self) -> Result<()> {
        self.0.validate()
    }
    fn suspended_duration(&self) -> Result<Duration> {
        self.0.suspended_duration()
    }
    fn request(&self, request: Value, deadline: Option<Instant>) -> Result<Value> {
        self.0.request(
            request,
            deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(300)),
        )
    }
}
struct Server {
    engine: Rc<RefCell<Engine>>,
    host: Option<Worker>,
    host_options: skyre::runtime::HostOptions,
    host_kernel_route: Option<skyre::host_turns::Route>,
    inactive_route_hosts: std::collections::BTreeMap<String, Worker>,
    pending_kernel_cleanup: std::collections::BTreeSet<String>,
    input: Option<Receiver<Result<Option<Value>>>>,
    connection_output: Option<ConnectionOutput>,
    pending: VecDeque<Result<Option<Value>>>,
    active_id: Value,
    peer_policy: Option<skyre::peer::Policy>,
    shutdown: bool,
    mcp_initialized: bool,
    elicitation_supported: bool,
    next_elicitation: u64,
    download_elicitation: Option<skyre::download_elicitation::DownloadElicitationBroker>,
    origin_elicitation: Option<skyre::origin_elicitation::OriginElicitationBroker>,
}
impl Server {
    fn check_connection_output(&self) -> Result<()> {
        let result = self
            .connection_output
            .as_ref()
            .map_or(Ok(()), ConnectionOutput::status);
        if let Err(error) = &result {
            if let Some(host) = &self.host {
                host.cancel();
            }
            if let Some(broker) = &self.origin_elicitation {
                broker.abort_active(error.clone());
            }
            if let Some(broker) = &self.download_elicitation {
                broker.abort_active(error.clone());
            }
        }
        result
    }
    fn reset_kernel(&mut self) -> Result<()> {
        self.pending_kernel_cleanup
            .insert(self.engine.borrow().selected_kernel_scope().into());
        self.host = None;
        self.reap_kernel_losses()
    }
    fn reap_kernel_losses(&mut self) -> Result<()> {
        let selected = self.engine.borrow().selected_kernel_scope().to_owned();
        if self.host.as_ref().is_some_and(Worker::kernel_reset_pending) {
            self.pending_kernel_cleanup.insert(selected.clone());
        }
        for (scope, host) in &self.inactive_route_hosts {
            if host.kernel_reset_pending() {
                self.pending_kernel_cleanup.insert(scope.clone());
            }
        }
        let mut failure = None;
        for scope in self.pending_kernel_cleanup.clone() {
            match self
                .engine
                .borrow_mut()
                .reset_kernel_scope_resources(&scope)
            {
                Ok(()) => {
                    self.pending_kernel_cleanup.remove(&scope);
                    if scope == selected {
                        if let Some(host) = &self.host {
                            host.take_kernel_reset();
                        }
                    } else if let Some(host) = self.inactive_route_hosts.get(&scope) {
                        host.take_kernel_reset();
                    }
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn eval(
        &mut self,
        code: &str,
        timeout: Duration,
        emit: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        self.eval_with_completion(code, timeout, emit, true)
    }
    fn eval_without_completion(
        &mut self,
        code: &str,
        timeout: Duration,
        emit: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        self.eval_with_completion(code, timeout, emit, false)
    }
    fn eval_with_completion(
        &mut self,
        code: &str,
        timeout: Duration,
        emit: &mut dyn FnMut(&Value) -> Result<()>,
        completion: bool,
    ) -> Result<Value> {
        self.check_connection_output()?;
        self.reap_kernel_losses()?;
        if self.host.is_none() {
            let executable = std::env::current_exe().map_err(Error::from)?;
            #[cfg(test)]
            let executable = executable.parent().unwrap().parent().unwrap().join("skyre");
            self.host = Some(Worker::with_options_and_executable(
                self.host_options.clone(),
                executable,
            ));
        }
        let origin_ticket = self.origin_elicitation.as_ref().map(|broker| {
            broker.activate(skyre::origin_elicitation::Context {
                request_id: self.active_id.clone(),
                mcp_initialized: self.mcp_initialized,
                elicitation_supported: self.elicitation_supported,
                elicitation_timeout: Duration::from_millis(
                    self.host_options.elicitation_timeout_ms.unwrap_or(300000),
                ),
            })
        });
        self.engine.borrow_mut().set_origin_approval(origin_ticket);
        let _download_cell = self.download_elicitation.as_ref().map(|broker| {
            let ticket = broker.activate(skyre::download_elicitation::Context {
                request_id: self.active_id.clone(),
                mcp_initialized: self.mcp_initialized,
                elicitation_supported: self.elicitation_supported,
                elicitation_timeout: Duration::from_millis(
                    self.host_options.elicitation_timeout_ms.unwrap_or(300000),
                ),
            });
            self.engine
                .borrow_mut()
                .security
                .set_download_approval(Some(Arc::new(DownloadApproval(ticket.clone()))));
            DownloadCell(ticket)
        });
        // The captured CUA tool ignores the final expression. Request this per
        // cell so mixed trusted RPC/MCP use preserves one persistent kernel
        // without observing getters, Proxy traps or toJSON on unused values.
        let ticket = if completion {
            self.host.as_mut().unwrap().start(code, timeout)?
        } else {
            self.host
                .as_mut()
                .unwrap()
                .start_without_completion(code, timeout)?
        };
        let chooser_scope = self.engine.borrow_mut().begin_chooser_cell(ticket);
        let result = (|| {
            loop {
                self.check_connection_output()?;
                // A parked kernel can fail while this cell runs. Failed cleanup
                // stays pending; dispatch below must not bypass that failure.
                let _ = self.reap_kernel_losses();
                self.engine.borrow_mut().tick();
                if let Some(input) = &self.input {
                    // Keep reading only while bounded space remains; cancellation does not
                    // jump over an unbounded flood of ordinary requests.
                    while self.pending.len() < 256 {
                        match input.try_recv() {
                            Ok(Ok(Some(message)))
                                if message["method"] == "notifications/cancelled"
                                    && message.get("id").is_none()
                                    && protocol::validate_request(&message).is_ok() =>
                            {
                                if message["params"]["requestId"] == self.active_id {
                                    self.host.as_ref().unwrap().cancel();
                                }
                            }
                            Ok(Ok(Some(message)))
                                if matches!(
                                    message["method"].as_str(),
                                    Some("host/turn" | "host/recover")
                                ) && protocol::validate_request(&message).is_ok()
                                    && self
                                        .engine
                                        .borrow()
                                        .authorize_host_turn_request(&message["params"])
                                        .is_ok() =>
                            {
                                self.host.as_ref().unwrap().cancel();
                                self.pending.push_back(Ok(Some(message)));
                            }
                            Ok(Ok(Some(message)))
                                if valid_reset_request(&message, self.mcp_initialized) =>
                            {
                                self.host.as_ref().unwrap().cancel();
                                self.pending.push_front(Ok(Some(message)));
                            }
                            Ok(message) => self.pending.push_back(message),
                            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                        }
                    }
                }
                match self
                    .host
                    .as_mut()
                    .unwrap()
                    .event(Duration::from_millis(10))?
                {
                    Some(Event::Call {
                        method,
                        args,
                        reply,
                        control,
                    }) => {
                        // A prompt can fail asynchronously while the runtime is
                        // still active. Do not dispatch another provider call or
                        // wait for the model's ordinary execution timeout.
                        if let Err(error) = self.check_connection_output() {
                            let _ = reply.send(Err(error.clone()));
                            return Err(error);
                        }
                        let result = if let Err(error) = self.reap_kernel_losses() {
                            Err(error)
                        } else if self.host.as_ref().unwrap().cancelled() {
                            Err(Error::new(-32800, "Evaluation cancelled"))
                        } else {
                            (|| {
                                let _provider = _download_cell
                                    .as_ref()
                                    .map(|cell| cell.0.bind_provider(control.clone()))
                                    .transpose()?;
                                if method == "host.elicitation" {
                                    self.elicit(&args, emit)
                                } else {
                                    self.engine
                                        .borrow_mut()
                                        .execute_from_js_controlled(&method, &args, &control)
                                }
                            })()
                        };
                        let _ = reply.send(result);
                    }
                    Some(Event::Done {
                        ticket: finished,
                        result,
                    }) if finished == ticket => {
                        self.check_connection_output()?;
                        self.reap_kernel_losses()?;
                        return if self.host.as_ref().unwrap().cancelled() {
                            Err(Error::new(-32800, "Evaluation cancelled"))
                        } else {
                            result
                        };
                    }
                    Some(Event::Done { .. }) => {
                        return Err(Error::action("Unexpected runtime ticket"));
                    }
                    None => (),
                }
            }
        })();
        let cancelled = self.host.as_ref().is_some_and(Worker::cancelled) || result.is_err();
        self.engine
            .borrow_mut()
            .finish_chooser_cell(&chooser_scope, ticket, cancelled);
        self.check_connection_output()?;
        if cancelled && self.pending.iter().any(|value|matches!(value,Ok(Some(message)) if valid_reset_request(message, self.mcp_initialized))) {
            // The installed MCP host reports an active reset as tool content,
            // without execution-duration metadata, rather than cancellation RPC error.
            Ok(json!({"error":{"message":"js execution reset"}}))
        } else {
            result
        }
    }
    fn elicit(
        &mut self,
        request: &Value,
        emit: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        self.check_connection_output()?;
        let (canonical, response) = self.engine.borrow_mut().prepare_elicitation(request)?;
        if let Some(response) = response {
            return Ok(response);
        }
        if !self.elicitation_supported || self.input.is_none() {
            return Err(Error::unsupported(
                "Computer Use requires a host that supports elicitations",
            ));
        }
        let response = self.await_elicitation(&canonical, emit)?;
        self.check_connection_output()?;
        self.engine
            .borrow_mut()
            .resolve_elicitation(&canonical, &response)
    }
    fn await_elicitation(
        &mut self,
        canonical: &Value,
        emit: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        self.next_elicitation = self
            .next_elicitation
            .checked_add(1)
            .ok_or_else(|| Error::action("Elicitation ID exhausted"))?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::action("Cannot generate an elicitation request ID"))?;
        let nonce = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let id = json!(format!(
            "skyre-elicitation-{}-{nonce}",
            self.next_elicitation
        ));
        let timeout =
            Duration::from_millis(self.host_options.elicitation_timeout_ms.unwrap_or(300000));
        if timeout.is_zero() {
            return Err(Error::invalid("elicitation_timeout_ms must be positive"));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::invalid("Elicitation timeout exceeds the clock range"))?;
        // No response received before this request is sent can authorize it.
        let mut params = canonical.clone();
        params["requestedSchema"] = json!({"type":"object","properties":{}});
        if let Some(meta) = params.as_object_mut().unwrap().remove("meta") {
            params["_meta"] = meta;
        }
        emit(&json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":params}))?;
        loop {
            self.check_connection_output()?;
            self.reap_kernel_losses()?;
            if self.host.as_ref().is_some_and(Worker::cancelled) {
                return Err(Error::new(-32800, "Evaluation cancelled"));
            }
            if self.pending.iter().any(|value| matches!(value,Ok(Some(message)) if valid_reset_request(message, self.mcp_initialized))) {
                if let Some(host)=&self.host {host.cancel();}
                return Err(Error::new(-32800,"Evaluation cancelled by kernel reset"));
            }
            if Instant::now() >= deadline {
                return Err(Error::new(-32002, "Elicitation response timed out"));
            }
            self.engine.borrow_mut().tick();
            let incoming = self
                .input
                .as_ref()
                .ok_or_else(|| Error::action("Elicitation client disconnected"))?
                .recv_timeout(Duration::from_millis(20));
            match incoming {
                Ok(Ok(Some(message))) if message.get("method").is_none() && message["id"] == id => {
                    protocol::validate_response(&message)?;
                    if let Some(error) = message.get("error") {
                        return Err(Error::new(
                            -32004,
                            error["message"].as_str().unwrap_or("Elicitation failed"),
                        ));
                    }
                    return Ok(message["result"].clone());
                }
                Ok(Ok(Some(message)))
                    if message["method"] == "notifications/cancelled"
                        && message.get("id").is_none()
                        && protocol::validate_request(&message).is_ok() =>
                {
                    if message["params"]["requestId"] == self.active_id {
                        if let Some(host) = &self.host {
                            host.cancel();
                        }
                        return Err(Error::new(-32800, "Evaluation cancelled"));
                    }
                }
                Ok(Ok(Some(message))) => {
                    let reset = valid_reset_request(&message, self.mcp_initialized);
                    let host_transition = matches!(
                        message["method"].as_str(),
                        Some("host/turn" | "host/recover")
                    ) && protocol::validate_request(&message).is_ok()
                        && self
                            .engine
                            .borrow()
                            .authorize_host_turn_request(&message["params"])
                            .is_ok();
                    if self.pending.len() >= 256 {
                        return Err(Error::new(
                            -32004,
                            "Too many queued requests while awaiting elicitation",
                        ));
                    }
                    if reset {
                        self.pending.push_front(Ok(Some(message)));
                    } else {
                        self.pending.push_back(Ok(Some(message)));
                    }
                    if reset || host_transition {
                        if let Some(host) = &self.host {
                            host.cancel();
                        }
                        return Err(Error::new(
                            -32800,
                            if host_transition {
                                "Evaluation cancelled by trusted host transition"
                            } else {
                                "Evaluation cancelled by kernel reset"
                            },
                        ));
                    }
                }
                Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(host) = &self.host {
                        host.cancel();
                    }
                    return Err(Error::action("Elicitation client disconnected"));
                }
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
        }
    }
    fn handle(
        &mut self,
        message: &Value,
        emit: &mut dyn FnMut(&Value) -> Result<()>,
    ) -> Option<Value> {
        // Correlated replies are consumed by an active elicitation waiter.
        // Never answer a late/unclaimed response with another response.
        if message.get("method").is_none()
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            return None;
        }
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        self.active_id = id.clone();
        if let Err(error) = protocol::validate_request(message) {
            return Some(protocol::response(id, Err(error)));
        }
        let method = message["method"].as_str().unwrap();
        let args = message.get("params").cloned().unwrap_or(json!({}));
        message.get("id")?;
        if method == "initialize" {
            if !args["protocolVersion"].is_string()
                || !args["capabilities"].is_object()
                || !args["clientInfo"]["name"].is_string()
                || !args["clientInfo"]["version"].is_string()
            {
                return Some(protocol::response(
                    id,
                    Err(Error::invalid(
                        "initialize requires protocolVersion, capabilities and clientInfo",
                    )),
                ));
            }
            if self.mcp_initialized {
                return Some(protocol::response(
                    id,
                    Err(Error::new(-32600, "Already initialized")),
                ));
            }
            self.mcp_initialized = true;
            self.elicitation_supported = args["capabilities"]["elicitation"].is_object();
        }
        if method.starts_with("tools/") && !self.mcp_initialized {
            return Some(protocol::response(
                id,
                Err(Error::new(-32001, "Initialize MCP before using tools")),
            ));
        }
        if let Err(error) = self.reap_kernel_losses() {
            return Some(protocol::response(id, Err(error)));
        }
        if method == "tools/call" {
            let name = args["name"].as_str();
            if name.is_none()
                || args
                    .get("arguments")
                    .is_some_and(|a| !a.is_null() && !a.is_object())
            {
                return Some(protocol::response(
                    id,
                    Err(Error::new(-32601, "tools/call")),
                ));
            }
            let name = name.unwrap();
            if ["js", "js_reset"].contains(&name)
                && let Err(error) =
                    validate_js_arguments(name, args.get("arguments").unwrap_or(&Value::Null))
            {
                return Some(protocol::response(id, Err(error)));
            }
        }
        let result = match method {
            "host/status" | "host/recover" => self
                .engine
                .borrow_mut()
                .host_recovery(&args, method == "host/recover"),
            "host/turn" => {
                let result = self.engine.borrow_mut().host_turn_event(&args);
                result.and_then(|receipt| {
                    if !receipt["requestMeta"].is_null() {
                        let route: skyre::host_turns::Route =
                            serde_json::from_value(receipt["route"].clone())?;
                        if self
                            .host_kernel_route
                            .as_ref()
                            .is_none_or(|previous| previous != &route)
                        {
                            if self.host_kernel_route.is_none() {
                                self.reset_kernel()?;
                            }
                            // Preserve each route's kernel without exposing its
                            // cached values or handles to another conversation.
                            if let Some(previous) = &self.host_kernel_route
                                && let Some(host) = self.host.take()
                            {
                                self.inactive_route_hosts.insert(previous.key(), host);
                            }
                            self.host = self.inactive_route_hosts.remove(&route.key());
                        }
                        self.engine.borrow_mut().select_kernel_scope(&route);
                        self.host_kernel_route = Some(route);
                    }
                    let mut metadata = self.host_options.request_meta.clone().unwrap_or(json!({}));
                    if let Some(object) = metadata.as_object_mut() {
                        object.remove("x-codex-turn-metadata");
                        if let Some(turn) = receipt.pointer("/requestMeta/x-codex-turn-metadata") {
                            object.insert("x-codex-turn-metadata".into(), turn.clone());
                        }
                    }
                    if let Some(host) = self.host.as_mut() {
                        host.set_request_meta(Some(metadata.clone()))?;
                    }
                    self.host_options.request_meta = Some(metadata);
                    Ok(receipt)
                })
            }
            "shutdown" => {
                self.shutdown = true;
                Ok(Value::Null)
            }
            "initialize" => Ok(
                json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"skyre","version":env!("CARGO_PKG_VERSION")},"instructions":"UI automation through a persistent JavaScript session using the initialized CUA API."}),
            ),
            "ping" => {
                if let Some(version) = args.get("clientApiVersion") {
                    if version == protocol::IPC_VERSION {
                        Ok(json!({"serverApiVersion":protocol::IPC_VERSION}))
                    } else {
                        Err(Error::new(-32001, "Incompatible client API version"))
                    }
                } else {
                    Ok(json!({}))
                }
            }
            "tools/list" => Ok(cua_tools()),
            "tools/call" => {
                let name = args["name"].as_str().unwrap_or_default();
                let a = &args["arguments"];
                let result = match name {
                    "js" => a["code"]
                        .as_str()
                        .ok_or_else(|| Error::invalid("code is required"))
                        .and_then(|code| {
                            if code.trim().is_empty() {
                                return Err(Error::invalid("js expects non-empty JavaScript source"));
                            }
                            self.eval_without_completion(code, timeout_duration(a)?, emit)
                        }),
                    "js_reset" => {
                        self.reset_kernel().map(|()|
                            json!({"outputs":[{"channel":"output","value":"js kernel reset"}],"value":null}),
                        )
                    }
                    _ => Err(Error::unsupported(format!("unknown tool: {name}"))),
                };
                Ok(match result {
                    Ok(value) => tool_output(value),
                    Err(error) => {
                        json!({"content":[{"type":"text","text":error.message}],"isError":true})
                    }
                })
            }
            "js" => validate_js_arguments("js", &args).and_then(|_| {
                self.eval(
                    args["code"].as_str().unwrap(),
                    timeout_duration(&args)?,
                    emit,
                )
            }),
            "js_reset" => {
                if let Err(error) = validate_js_arguments("js_reset", &args) {
                    return Some(protocol::response(id, Err(error)));
                }
                self.reset_kernel().map(|()| json!({"reset":true}))
            }
            "request" => self.engine.borrow_mut().native_request(&args),
            _ => self.engine.borrow_mut().execute(method, &args),
        };
        Some(protocol::response(id, result))
    }
    #[cfg(test)]
    fn serve(
        &mut self,
        input: impl BufRead + Send + 'static,
        output: &mut (impl Write + Send),
        framed: bool,
    ) -> Result<()> {
        connection_output::with_finite_writer(output, framed, |output| {
            self.serve_output(input, output, framed)
        })
    }
    fn serve_owned(
        &mut self,
        input: impl BufRead + Send + 'static,
        mut output: OwnedOutput,
        framed: bool,
    ) -> Result<()> {
        let result = self.serve_output(input, &output.output, framed);
        let joined = output.join();
        result.and(joined)
    }
    fn serve_output(
        &mut self,
        input: impl BufRead + Send + 'static,
        output: &ConnectionOutput,
        framed: bool,
    ) -> Result<()> {
        self.connection_output = Some(output.clone());
        let result = self.serve_connection(input, output, framed);
        // Revoke authority before joining any output worker. Closing is an
        // out-of-band signal, independent of queue capacity and blocked writes.
        output.close();
        self.end_connection();
        result
    }
    fn end_connection(&mut self) {
        self.connection_output = None;
        // Broken output, parse errors and normal EOF all end the same authority
        // scope. Never carry a previous client's grants into another connection.
        self.input = None;
        self.engine
            .borrow_mut()
            .security
            .set_download_approval(None);
        self.engine.borrow_mut().set_origin_approval(None);
        if let Some(broker) = self.origin_elicitation.take() {
            broker.disconnect();
        }
        if let Some(broker) = self.download_elicitation.take() {
            broker.disconnect();
        }
        self.pending.clear();
        self.host = None;
        self.host_kernel_route = None;
        self.inactive_route_hosts.clear();
        self.pending_kernel_cleanup.clear();
        self.mcp_initialized = false;
        self.elicitation_supported = false;
        self.active_id = Value::Null;
        self.engine.borrow_mut().end_session();
        if self.engine.borrow().host_turns.is_some()
            && let Some(metadata) = self
                .host_options
                .request_meta
                .as_mut()
                .and_then(Value::as_object_mut)
        {
            metadata.remove("x-codex-turn-metadata");
        }
    }
    fn serve_connection(
        &mut self,
        input: impl BufRead + Send + 'static,
        writer: &ConnectionOutput,
        framed: bool,
    ) -> Result<()> {
        let emit = writer.clone();
        let authority = self
            .engine
            .borrow()
            .host_turns
            .as_ref()
            .map(|controller| controller.authorization());
        let broker = skyre::download_elicitation::DownloadElicitationBroker::new(
            Arc::new(move |value, deadline| emit.emit(value, deadline)),
            Arc::new(valid_reset_request),
            authority.clone().map(|authority| {
                Arc::new(move |message: &Value| {
                    authority
                        .authorize(message["params"]["authorityToken"].as_str().unwrap_or(""))
                        .is_ok()
                }) as skyre::download_elicitation::TransitionPredicate
            }),
        );
        let origin_writer = writer.clone();
        let origin_health = writer.clone();
        let origin_broker = skyre::origin_elicitation::OriginElicitationBroker::new(
            Arc::new(move |value, deadline| origin_writer.enqueue(value, deadline)),
            Arc::new(valid_reset_request),
            authority.map(|authority| {
                Arc::new(move |message: &Value| {
                    authority
                        .authorize(message["params"]["authorityToken"].as_str().unwrap_or(""))
                        .is_ok()
                }) as skyre::origin_elicitation::TransitionPredicate
            }),
        );
        let origin_broker =
            origin_broker.with_output_health(Arc::new(move || origin_health.status()));
        self.origin_elicitation = Some(origin_broker.clone());
        self.download_elicitation = Some(broker.clone());
        let result =
            self.serve_messages(input, writer, broker.clone(), origin_broker.clone(), framed);
        broker.disconnect();
        origin_broker.disconnect();
        result
    }
    fn serve_messages(
        &mut self,
        mut input: impl BufRead + Send + 'static,
        output: &ConnectionOutput,
        broker: skyre::download_elicitation::DownloadElicitationBroker,
        origin_broker: skyre::origin_elicitation::OriginElicitationBroker,
        framed: bool,
    ) -> Result<()> {
        let (sender, receiver) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            loop {
                let result = if framed {
                    protocol::read_frame(&mut input)
                } else {
                    read_line(&mut input)
                };
                let done = matches!(&result, Ok(None)) || (framed && result.is_err());
                if let Ok(Some(message)) = &result
                    && (origin_broker.route(message) | broker.route(message))
                {
                    continue;
                }
                if done {
                    origin_broker.disconnect();
                    broker.disconnect();
                }
                if let Err(error) = &result {
                    origin_broker.abort_active(error.clone());
                    broker.abort_active(error.clone());
                }
                match sender.try_send(result) {
                    Ok(()) => (),
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                    Err(mpsc::TrySendError::Full(value)) => {
                        origin_broker.abort_active(Error::new(
                            -32004,
                            "Too many queued requests while awaiting origin elicitation",
                        ));
                        broker.abort_active(Error::new(
                            -32004,
                            "Too many queued requests while awaiting elicitation",
                        ));
                        if sender.send(value).is_err() {
                            break;
                        }
                    }
                }
                if done {
                    break;
                }
            }
        });
        self.input = Some(receiver);
        self.pending.clear();
        while !self.shutdown {
            output.status()?;
            // Idle failures have no tool response to attach to. Keep retryable
            // cleanup pending and surface any failure on the next request.
            let _ = self.reap_kernel_losses();
            let message = match self.pending.pop_front() {
                Some(value) => value,
                None => match self
                    .input
                    .as_ref()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(50))
                {
                    Ok(value) => value,
                    Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        self.engine.borrow_mut().tick();
                        continue;
                    }
                },
            };
            self.check_connection_output()?;
            let response = match message {
                Ok(Some(message)) => self.handle(&message, &mut |value| {
                    output.emit(value, Instant::now() + Duration::from_secs(30))
                }),
                Ok(None) => break,
                Err(error) => Some(protocol::response(Value::Null, Err(error))),
            };
            if let Some(response) = response {
                output.emit(&response, Instant::now() + Duration::from_secs(30))?;
            }
        }
        Ok(())
    }
}
fn read_line(input: &mut impl BufRead) -> Result<Option<Value>> {
    let mut line = Vec::new();
    let count = (&mut *input)
        .take((protocol::MAX_FRAME + 2) as u64)
        .read_until(b'\n', &mut line)?;
    if count == 0 {
        return Ok(None);
    }
    if count > protocol::MAX_FRAME {
        // Discard the remainder without allocating an unbounded input line.
        if !line.ends_with(b"\n") {
            loop {
                let buf = input.fill_buf()?;
                if buf.is_empty() {
                    break;
                }
                let end = buf.iter().position(|b| *b == b'\n');
                let len = end.map_or(buf.len(), |i| i + 1);
                input.consume(len);
                if end.is_some() {
                    break;
                }
            }
        }
        return Err(Error::invalid("JSON line exceeds 8 MiB"));
    }
    Ok(Some(serde_json::from_slice(&line)?))
}
fn timeout_duration(args: &Value) -> Result<Duration> {
    match args.get("timeout_ms") {
        None | Some(Value::Null) => Ok(Duration::from_secs(30)),
        Some(v) => v
            .as_u64()
            .filter(|v| *v >= 1)
            .map(Duration::from_millis)
            .ok_or_else(|| Error::invalid("timeout_ms must be a positive integer")),
    }
}
fn valid_reset_request(message: &Value, mcp_initialized: bool) -> bool {
    if protocol::validate_request(message).is_err()
        || message.get("id").is_none()
        || (message["method"] == "tools/call" && !mcp_initialized)
    {
        return false;
    }
    let args = if message["method"] == "js_reset" {
        message.get("params")
    } else if message["method"] == "tools/call" && message["params"]["name"] == "js_reset" {
        message["params"].get("arguments")
    } else {
        return false;
    };
    validate_js_arguments("js_reset", args.unwrap_or(&json!({}))).is_ok()
}
fn validate_js_arguments(name: &str, args: &Value) -> Result<()> {
    // The installed kernel uses serde validation independently of its advertised
    // JSON Schema. Optional nulls are absent, unknown fields are rejected, and
    // title has a non-whitespace check but no enforced 80-character limit.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JsArguments {
        title: Option<String>,
        #[serde(rename = "code")]
        _code: String,
        #[serde(rename = "timeout_ms")]
        _timeout_ms: Option<std::num::NonZeroU64>,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ResetArguments {}
    let args = if args.is_null() {
        json!({})
    } else {
        args.clone()
    };
    if !args.is_object() {
        return Err(Error::invalid("Tool arguments must be an object"));
    }
    let invalid = |error: serde_json::Error| Error::invalid(format!("{name}: {error}"));
    if name == "js" {
        let parsed: JsArguments = serde_json::from_value(args).map_err(invalid)?;
        if parsed.title.is_some_and(|title| title.trim().is_empty()) {
            return Err(Error::invalid("js: title must be non-empty"));
        }
    } else {
        serde_json::from_value::<ResetArguments>(args).map_err(invalid)?;
    }
    Ok(())
}

fn cua_tools() -> Value {
    json!({"tools":[
        {"name":"js","description":include_str!("cua_tool_description.md"),"inputSchema":{"additionalProperties":false,"type":"object","properties":{
            "code":{"description":"JavaScript to execute using the initialized CUA runtime.","type":"string"},
            "title":{"description":"Short user-facing description of what the code does.","type":"string","minLength":1,"maxLength":80},
            "timeout_ms":{"description":"Optional execution timeout in milliseconds. Defaults to 30000 (30 seconds) when omitted.","type":"integer","minimum":1}
        },"required":["code"]}},
        {"name":"js_reset","description":include_str!("cua_reset_description.md"),"inputSchema":{"additionalProperties":false,"type":"object","properties":{}},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}
    ]})
}
fn tool_output(value: Value) -> Value {
    let mut content = vec![];
    if let Some(error) = value.get("error") {
        content.push(json!({"type":"text","text":value["exceptionMessage"].as_str().or_else(||error["message"].as_str()).unwrap_or("JavaScript evaluation failed")}));
    }
    if let Some(outputs) = value["outputs"].as_array() {
        // The installed host composes named text, combined default text, then
        // images, regardless of the order in which images were emitted.
        for output in outputs
            .iter()
            .filter(|output| output["kind"] != "image" && output["named"] == true)
            .chain(outputs.iter().filter(|output| {
                output["kind"] != "image" && output["named"] != true && output["value"] != ""
            }))
            .chain(outputs.iter().filter(|output| output["kind"] == "image"))
        {
            if value.get("error").is_some() && output["kind"] == "image" {
                continue;
            }
            let v = &output["value"];
            if output["kind"] == "image" && v["data"].is_string() && v["mime_type"].is_string() {
                content.push(json!({"type":"image","data":v["data"],"mimeType":v["mime_type"],"_meta":{"codex/imageDetail":"original"}}));
            } else {
                content.push(json!({"type":"text","text":v.as_str().map(String::from).unwrap_or_else(||v.to_string())}));
            }
        }
    }
    if content.is_empty() {
        content.push(json!({"type":"text","text":if value.get("outputs").is_some(){String::new()}else{value.to_string()}}));
    }
    let mut result = json!({"content":content,"isError":value.get("error").is_some()});
    if let Some(duration) = value.get("executionDurationMs") {
        result["_meta"] = json!({"codex/nodeReplExecutionDurationMs":duration});
    }
    if let Some(meta) = value["responseMeta"]
        .as_object()
        .filter(|meta| !meta.is_empty())
    {
        let output_meta = result
            .as_object_mut()
            .unwrap()
            .entry("_meta")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        for (key, value) in meta {
            if key != "codex/nodeReplExecutionDurationMs" {
                output_meta.insert(key.clone(), value.clone());
            }
        }
    }
    result
}
fn bounded_config(path: &std::path::Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::invalid("Configuration exceeds 1 MiB"));
    }
    Ok(bytes)
}
fn run() -> Result<()> {
    let args = Cli::parse();
    match &args.command {
        Command::Permissions { request } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&native::permissions(*request)?)?
            );
            return Ok(());
        }
        Command::ExtensionBridge { listen, socket } => {
            return skyre::browser_extension::serve(listen, socket);
        }
        Command::ExtensionHost { socket } => return skyre::browser_extension::native_host(socket),
        Command::ExtensionManifest {
            destination,
            socket,
            extension_id,
        } => {
            println!(
                "{}",
                skyre::browser_extension::prepare_manifest(
                    destination,
                    &std::env::current_exe()?,
                    socket,
                    extension_id
                )?
            );
            return Ok(());
        }
        _ => (),
    }
    let desktop: Box<dyn native::Desktop> = if args.fixture {
        Box::new(Fixture::default())
    } else {
        native::create()?
    };
    let mut engine = Engine::new(desktop);
    if let Some(owner) = &args.session_id {
        engine.owner = owner.clone();
    }
    engine
        .platforms
        .set_turn_context(&engine.owner, "initial")?;
    if let Some(path) = args.guardian_provider {
        engine.guardian_monitor = Some(skyre::process_rpc::Program::new(
            path,
            Duration::from_secs(1),
        )?);
    }
    if let Some(path) = args.security_config {
        engine.security = skyre::security::Security::load(path)?;
    }
    if args.allow_native_control {
        engine.security.authorize_native_control();
    }
    if let Some(path) = args.data_dir {
        engine.services = Some(skyre::host_services::Services::open(
            path,
            &args
                .session_id
                .unwrap_or_else(|| format!("skyre-{}", std::process::id())),
        )?);
    }
    if let Some(path) = args.platform_config {
        let configs: Vec<Value> = serde_json::from_slice(&bounded_config(&path)?)?;
        for config in configs {
            engine.platforms.configure(&config)?;
        }
    }
    if let Some(path) = args.messaging_config {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Config {
            executable: PathBuf,
            #[serde(default)]
            args: Vec<String>,
            attachment_root: PathBuf,
        }
        let config: Config = serde_json::from_slice(&bounded_config(&path)?)?;
        engine
            .messaging
            .configure(config.executable, config.args, config.attachment_root)?;
    }
    let peer_policy = args
        .peer_policy
        .as_deref()
        .map(skyre::peer::Policy::load)
        .transpose()?;
    for endpoint in args.cdp {
        let (id, url) = endpoint
            .split_once('=')
            .ok_or_else(|| Error::invalid("--cdp expects ID=ws://endpoint"))?;
        engine.browsers.register(id, url)?;
    }
    if let Some(path) = args.iab_config {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Config {
            id: String,
            endpoint: String,
            route: skyre::browser::iab::RouteConfig,
            context: Value,
        }
        let configs: Vec<Config> = serde_json::from_slice(&bounded_config(&path)?)?;
        if configs.len() > 128 {
            return Err(Error::invalid("IAB configuration exceeds 128 routes"));
        }
        for config in configs {
            if config.id.is_empty() || config.id.len() > 256 {
                return Err(Error::invalid("IAB browser IDs must contain 1–256 bytes"));
            }
            engine
                .browsers
                .register_iab(&config.id, &config.endpoint, config.route)?;
            engine
                .browsers
                .set_iab_context(&config.id, Some(&config.context))?;
        }
    }
    if let Some(path) = args.host_turns_config {
        let config: skyre::host_turns::Config = serde_json::from_slice(
            &skyre::browser::persistence::read_private(&path, 1024 * 1024)?,
        )?;
        engine.host_turns = Some(skyre::host_turns::Controller::open(
            config,
            &mut engine.browsers,
        )?);
    }
    let mut server = Server {
        engine: Rc::new(RefCell::new(engine)),
        host: None,
        host_options: args
            .runtime_config
            .as_deref()
            .map(bounded_config)
            .transpose()?
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?
            .unwrap_or_default(),
        input: None,
        connection_output: None,
        pending: VecDeque::new(),
        active_id: Value::Null,
        peer_policy,
        shutdown: false,
        mcp_initialized: false,
        elicitation_supported: false,
        next_elicitation: 0,
        download_elicitation: None,
        origin_elicitation: None,
        host_kernel_route: None,
        inactive_route_hosts: Default::default(),
        pending_kernel_cleanup: Default::default(),
    };
    if server.engine.borrow().host_turns.is_some()
        && let Some(metadata) = server
            .host_options
            .request_meta
            .as_mut()
            .and_then(Value::as_object_mut)
    {
        metadata.remove("x-codex-turn-metadata");
    }
    if let Some(runtime) = args.runtime {
        server.host_options.runtime = runtime;
    }
    server.host_options.runtime.require_available()?;
    server.engine.borrow_mut().runtime_backend = server.host_options.runtime;
    match args.command {
        Command::Permissions { .. }
        | Command::ExtensionBridge { .. }
        | Command::ExtensionHost { .. }
        | Command::ExtensionManifest { .. } => {
            unreachable!("Extension command handled before engine creation")
        }
        Command::Capabilities => println!(
            "{}",
            serde_json::to_string_pretty(
                &server
                    .engine
                    .borrow_mut()
                    .execute("capabilities", &json!({}))?
            )?
        ),
        Command::Call { method, args } => println!(
            "{}",
            serde_json::to_string_pretty(
                &server
                    .engine
                    .borrow_mut()
                    .execute(&method, &serde_json::from_str(&args)?)?
            )?
        ),
        Command::Eval {
            code,
            file,
            timeout,
        } => {
            let code = match (code, file) {
                (Some(code), _) => code,
                (_, Some(file)) => std::fs::read_to_string(file)?,
                _ => return Err(Error::invalid("Supply --code or --file")),
            };
            let result = server.eval(&code, Duration::from_secs(timeout), &mut |_| {
                Err(Error::unsupported(
                    "CLI eval has no interactive elicitation client",
                ))
            })?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            if let Some(error) = result.get("error") {
                return Err(Error::new(
                    -32004,
                    error["message"]
                        .as_str()
                        .unwrap_or("JavaScript evaluation failed"),
                ));
            }
        }
        Command::Serve {
            socket: None,
            framed,
        } => {
            let output = OwnedOutput::spawn(
                &std::env::current_exe()?,
                std::process::Stdio::inherit(),
                framed,
                None,
            )?;
            server.serve_owned(io::BufReader::new(io::stdin()), output, framed)?;
        }
        Command::Serve {
            socket: Some(path), ..
        } => serve_socket(&mut server, path)?,
    }
    Ok(())
}
#[cfg(unix)]
fn serve_socket(server: &mut Server, path: PathBuf) -> Result<()> {
    use std::os::unix::{
        fs::{MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::UnixListener,
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::invalid("Socket needs a private parent directory"))?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let meta = std::fs::symlink_metadata(parent)?;
    let uid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(Error::invalid(
            "Socket directory must be owned by this user and mode 0700",
        ));
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    struct SocketFile(PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _socket_file = SocketFile(path);
    for client in listener.incoming() {
        let client = client?;
        client.set_read_timeout(Some(Duration::from_secs(30)))?;
        client.set_write_timeout(Some(Duration::from_secs(30)))?;
        // Separate JavaScript scopes across local client connections.
        server.host = None;
        server.mcp_initialized = false;
        server.elicitation_supported = false;
        #[cfg(target_os = "macos")]
        {
            let mut peer_uid = 0;
            let mut peer_gid = 0;
            if unsafe { libc::getpeereid(client.as_raw_fd(), &mut peer_uid, &mut peer_gid) } != 0
                || peer_uid != uid
            {
                continue;
            }
        }
        #[cfg(target_os = "linux")]
        {
            let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    client.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    &mut peer as *mut _ as _,
                    &mut len,
                )
            } != 0
                || peer.uid != uid
            {
                continue;
            }
        }
        if let Some(policy) = &server.peer_policy {
            match policy.authorize_socket(client.as_raw_fd()) {
                Ok(decision) if decision["authorized"] == true => (),
                Ok(_) | Err(_) => continue,
            }
        }
        let reader = io::BufReader::new(client.try_clone()?);
        let control = client.try_clone()?;
        let output = OwnedOutput::spawn(
            &std::env::current_exe()?,
            std::process::Stdio::from(std::os::fd::OwnedFd::from(client)),
            true,
            Some(Arc::new(move || {
                let _ = control.shutdown(std::net::Shutdown::Both);
            })),
        )?;
        if let Err(error) = server.serve_owned(reader, output, true) {
            eprintln!("Connection closed: {error}");
        }
        if server.shutdown {
            break;
        }
    }
    Ok(())
}
#[cfg(not(unix))]
fn serve_socket(_: &mut Server, _: PathBuf) -> Result<()> {
    Err(Error::unsupported("Unix socket mode unavailable"))
}
fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == connection_output::CHILD_ARGUMENT)
    {
        // stderr is the private acknowledgement pipe in this child. Never put
        // diagnostic text into it or into the inherited client output stream.
        connection_output::run_child();
    }
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "__runtime-worker")
    {
        if let Err(error) = skyre::worker::run_child() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod elicitation_tests {
    use super::*;
    #[test]
    fn successful_cell_composes_named_default_then_images() {
        let result = tool_output(json!({"outputs":[
            {"kind":"image","value":{"data":"AA==","mime_type":"image/png"}},
            {"value":"firstsecond"},
            {"named":true,"channel":"one","value":"named1named2"},
            {"kind":"image","value":{"data":"AQ==","mime_type":"image/jpeg"}},
            {"named":true,"channel":"two","value":"last named"}
        ]}));
        assert_eq!(
            result,
            json!({"content":[
            {"type":"text","text":"named1named2"},
            {"type":"text","text":"last named"},
            {"type":"text","text":"firstsecond"},
            {"type":"image","data":"AA==","mimeType":"image/png","_meta":{"codex/imageDetail":"original"}},
            {"type":"image","data":"AQ==","mimeType":"image/jpeg","_meta":{"codex/imageDetail":"original"}}
        ],"isError":false})
        );

        let image_and_empty = tool_output(json!({"outputs":[
            {"kind":"image","value":{"data":"AA==","mime_type":"image/png"}},
            {"value":""}
        ]}));
        assert_eq!(image_and_empty["content"].as_array().unwrap().len(), 1);
        assert_eq!(image_and_empty["content"][0]["type"], "image");
        assert_eq!(
            tool_output(json!({"outputs":[{"value":""}]})),
            json!({"content":[{"type":"text","text":""}],"isError":false})
        );
    }
    #[test]
    fn failed_cell_omits_images_but_preserves_text_and_original_error() {
        let result = tool_output(json!({
            "outputs":[
                {"kind":"image","value":{"data":"AA==","mime_type":"image/png"}},
                {"named":true,"channel":"cua.core","value":"documentation"},
                {"value":"before error"}
            ],
            "error":{"message":"original error"},
            "executionDurationMs":7
        }));
        assert_eq!(
            result,
            json!({"content":[
            {"type":"text","text":"original error"},
            {"type":"text","text":"documentation"},
            {"type":"text","text":"before error"}
        ],"isError":true,"_meta":{"codex/nodeReplExecutionDurationMs":7}})
        );
        let success = tool_output(json!({"outputs":[
            {"kind":"image","value":{"data":"AA==","mime_type":"image/png"}}
        ]}));
        assert_eq!(success["content"][0]["type"], "image");
    }
    #[test]
    #[cfg(feature = "v8")]
    fn lost_v8_kernel_discards_only_its_owned_recording_before_recovery() {
        use std::cell::Cell;
        struct Recorder {
            active: Rc<Cell<bool>>,
            cancelled: Rc<Cell<usize>>,
            ended: Rc<Cell<usize>>,
        }
        impl native::Desktop for Recorder {
            fn synthetic(&self) -> bool {
                true
            }
            fn apps(&mut self) -> Result<Vec<native::App>> {
                Ok(vec![])
            }
            fn snapshot(&mut self, _: &native::App) -> Result<skyre::ax::Node> {
                Err(Error::unsupported("unused synthetic capture"))
            }
            fn action(&mut self, _: &native::App, _: native::Action) -> Result<()> {
                Err(Error::unsupported("unused synthetic input"))
            }
            fn capabilities(&self) -> Vec<&'static str> {
                vec![]
            }
            fn audio(&mut self, method: &str, _: &str, _: &Value) -> Result<Value> {
                assert_eq!(method, "start");
                self.active.set(true);
                Ok(Value::Null)
            }
            fn cancel_audio(&mut self, _: &str) -> Result<()> {
                self.active.set(false);
                self.cancelled.set(self.cancelled.get() + 1);
                Ok(())
            }
            fn end_session(&mut self, _: &str) -> Result<()> {
                self.ended.set(self.ended.get() + 1);
                Ok(())
            }
        }
        let active = Rc::new(Cell::new(false));
        let cancelled = Rc::new(Cell::new(0));
        let ended = Rc::new(Cell::new(0));
        let mut server = server();
        server.host_options.runtime = skyre::runtime::RuntimeBackend::V8;
        server.engine = Rc::new(RefCell::new(Engine::new(Box::new(Recorder {
            active: active.clone(),
            cancelled: cancelled.clone(),
            ended: ended.clone(),
        }))));
        server
            .engine
            .borrow_mut()
            .execute("audio.start", &json!({"scope":"system"}))
            .unwrap();
        assert!(active.get());
        server
            .eval(
                "let valueBeforeFatal=1",
                Duration::from_secs(5),
                &mut |_| Ok(()),
            )
            .unwrap();
        // This allocation runs only in the supervised Rust child, never in the
        // test process or a native audio provider.
        let failed = server.eval(
            "new Array(64*1024*1024).fill(42)",
            Duration::from_secs(10),
            &mut |_| Ok(()),
        );
        assert!(failed.is_err() || failed.unwrap().get("error").is_some());
        assert!(!active.get());
        assert_eq!(cancelled.get(), 1);
        assert_eq!(
            ended.get(),
            0,
            "external native session must survive kernel loss"
        );
        let recovered = server
            .eval(
                "[typeof valueBeforeFatal, 42]",
                Duration::from_secs(5),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(recovered["value"], json!(["undefined", 42]));
        assert_eq!(cancelled.get(), 1);
    }
    fn server() -> Server {
        Server {
            engine: Rc::new(RefCell::new(Engine::new(Box::new(Fixture::default())))),
            host: None,
            host_options: skyre::runtime::HostOptions::default(),
            input: None,
            connection_output: None,
            pending: VecDeque::new(),
            active_id: json!(7),
            peer_policy: None,
            shutdown: false,
            mcp_initialized: true,
            elicitation_supported: true,
            next_elicitation: 0,
            download_elicitation: None,
            origin_elicitation: None,
            host_kernel_route: None,
            inactive_route_hosts: Default::default(),
            pending_kernel_cleanup: Default::default(),
        }
    }
    fn request() -> Value {
        json!({"message":"Owned synthetic review","meta":{"connector_id":"computer-use"}})
    }
    #[derive(Default)]
    struct BackgroundRecording {
        active: bool,
        fail_cancel: bool,
        cancelled: usize,
        ended: usize,
    }
    struct BackgroundRecorder(Rc<RefCell<BackgroundRecording>>);
    impl native::Desktop for BackgroundRecorder {
        fn synthetic(&self) -> bool {
            true
        }
        fn apps(&mut self) -> Result<Vec<native::App>> {
            Ok(vec![])
        }
        fn snapshot(&mut self, _: &native::App) -> Result<skyre::ax::Node> {
            Err(Error::unsupported("unused synthetic capture"))
        }
        fn action(&mut self, _: &native::App, _: native::Action) -> Result<()> {
            Err(Error::unsupported("unused synthetic input"))
        }
        fn capabilities(&self) -> Vec<&'static str> {
            vec![]
        }
        fn audio(&mut self, method: &str, _: &str, _: &Value) -> Result<Value> {
            let mut recording = self.0.borrow_mut();
            match method {
                "start" => {
                    assert!(!recording.active);
                    recording.active = true;
                    Ok(Value::Null)
                }
                "status" => Ok(json!({"active":recording.active})),
                _ => Err(Error::unsupported("unused synthetic audio")),
            }
        }
        fn cancel_audio(&mut self, _: &str) -> Result<()> {
            let mut recording = self.0.borrow_mut();
            recording.cancelled += 1;
            if recording.fail_cancel {
                return Err(Error::action("owned background cleanup failed"));
            }
            recording.active = false;
            Ok(())
        }
        fn end_session(&mut self, _: &str) -> Result<()> {
            self.0.borrow_mut().ended += 1;
            Ok(())
        }
    }
    fn background_server(
        runtime: skyre::runtime::RuntimeBackend,
    ) -> (Server, Rc<RefCell<BackgroundRecording>>) {
        let state = Rc::new(RefCell::new(BackgroundRecording::default()));
        let mut server = server();
        server.host_options.runtime = runtime;
        server.engine = Rc::new(RefCell::new(Engine::new(Box::new(BackgroundRecorder(
            state.clone(),
        )))));
        (server, state)
    }
    fn runtime_backends() -> Vec<skyre::runtime::RuntimeBackend> {
        use skyre::runtime::RuntimeBackend;
        #[cfg(feature = "v8")]
        {
            vec![RuntimeBackend::Quickjs, RuntimeBackend::V8]
        }
        #[cfg(not(feature = "v8"))]
        {
            vec![RuntimeBackend::Quickjs]
        }
    }
    fn wait_for_background_loss(host: &Worker) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !host.kernel_reset_pending() {
            assert!(
                Instant::now() < deadline,
                "owned background timer did not fail"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn background_kernel_loss_retries_failed_cleanup_before_any_new_cell() {
        for runtime in runtime_backends() {
            let (mut server, state) = background_server(runtime);
            server
                .engine
                .borrow_mut()
                .execute("audio.start", &json!({"scope":"system"}))
                .unwrap();
            let armed = server.eval(
                "let beforeBackgroundLoss=7;setTimeout(()=>{throw new Error('owned background failure')},100);42",
                Duration::from_secs(3), &mut |_| Ok(()),
            ).unwrap();
            assert_eq!(armed["value"], 42);
            wait_for_background_loss(server.host.as_ref().unwrap());
            state.borrow_mut().fail_cancel = true;
            let failure = server
                .eval(
                    "let forbiddenBeforeCleanup=1",
                    Duration::from_secs(3),
                    &mut |_| Ok(()),
                )
                .unwrap_err();
            assert_eq!(failure.message, "owned background cleanup failed");
            assert!(server.host.as_ref().unwrap().kernel_reset_pending());
            assert!(state.borrow().active);
            assert_eq!(state.borrow().cancelled, 1);
            state.borrow_mut().fail_cancel = false;
            let recovered = server
                .eval(
                    "[typeof beforeBackgroundLoss,typeof forbiddenBeforeCleanup]",
                    Duration::from_secs(3),
                    &mut |_| Ok(()),
                )
                .unwrap();
            assert_eq!(recovered["value"], json!(["undefined", "undefined"]));
            assert!(!state.borrow().active);
            assert_eq!(state.borrow().cancelled, 2);
            assert_eq!(state.borrow().ended, 0);
            assert!(!server.host.as_ref().unwrap().kernel_reset_pending());
        }
    }
    #[test]
    fn parked_background_kernel_loss_cleans_only_its_resource_scope() {
        for runtime in runtime_backends() {
            for recording_is_parked in [false, true] {
                let (mut server, state) = background_server(runtime);
                let route = |name: &str| skyre::host_turns::Route {
                    conversation_id: name.into(),
                    thread_id: None,
                };
                let first = route("owned-first");
                let second = route("owned-second");
                server.engine.borrow_mut().select_kernel_scope(&first);
                server.host_kernel_route = Some(first.clone());
                if recording_is_parked {
                    server
                        .engine
                        .borrow_mut()
                        .execute("audio.start", &json!({"scope":"system"}))
                        .unwrap();
                }
                server
                    .eval(
                        "setTimeout(()=>{throw new Error('owned parked failure')},100);42",
                        Duration::from_secs(3),
                        &mut |_| Ok(()),
                    )
                    .unwrap();
                server
                    .inactive_route_hosts
                    .insert(first.key(), server.host.take().unwrap());
                server.engine.borrow_mut().select_kernel_scope(&second);
                server.host_kernel_route = Some(second.clone());
                if !recording_is_parked {
                    server
                        .engine
                        .borrow_mut()
                        .execute("audio.start", &json!({"scope":"system"}))
                        .unwrap();
                }
                wait_for_background_loss(&server.inactive_route_hosts[&first.key()]);
                // This is also the idle transport hook: no new JavaScript cell
                // or native end_session call is needed to release lost resources.
                server.reap_kernel_losses().unwrap();
                assert_eq!(state.borrow().active, !recording_is_parked);
                assert_eq!(state.borrow().cancelled, usize::from(recording_is_parked));
                assert_eq!(state.borrow().ended, 0);
                assert_eq!(server.engine.borrow().selected_kernel_scope(), second.key());
                assert!(!server.inactive_route_hosts[&first.key()].kernel_reset_pending());
                assert_eq!(
                    server
                        .eval("42", Duration::from_secs(3), &mut |_| Ok(()))
                        .unwrap()["value"],
                    42
                );
                server.reset_kernel().unwrap();
                assert!(!state.borrow().active);
                assert_eq!(state.borrow().cancelled, 1);
            }
        }
    }
    #[test]
    fn failed_explicit_reset_keeps_cleanup_pending_after_worker_drop() {
        let (mut server, state) = background_server(skyre::runtime::RuntimeBackend::Quickjs);
        server
            .engine
            .borrow_mut()
            .execute("audio.start", &json!({"scope":"system"}))
            .unwrap();
        server
            .eval(
                "let beforeFailedReset=7",
                Duration::from_secs(3),
                &mut |_| Ok(()),
            )
            .unwrap();
        state.borrow_mut().fail_cancel = true;
        assert_eq!(
            server.reset_kernel().unwrap_err().message,
            "owned background cleanup failed"
        );
        assert!(server.host.is_none());
        assert!(state.borrow().active);
        assert_eq!(server.pending_kernel_cleanup.len(), 1);
        state.borrow_mut().fail_cancel = false;
        let result = server
            .eval(
                "typeof beforeFailedReset",
                Duration::from_secs(3),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert_eq!(result["value"], "undefined");
        assert!(!state.borrow().active);
        assert_eq!(state.borrow().cancelled, 2);
        assert_eq!(state.borrow().ended, 0);
        assert!(server.pending_kernel_cleanup.is_empty());
    }
    #[test]
    fn elicitation_correlates_random_request_and_preserves_decline_and_queued_requests() {
        let mut server = server();
        let (send, receive) = mpsc::sync_channel(8);
        server.input = Some(receive);
        let result = server
            .await_elicitation(&request(), &mut |outgoing| {
                assert_eq!(outgoing["method"], "elicitation/create");
                assert_eq!(outgoing["params"]["message"], "Owned synthetic review");
                assert_eq!(outgoing["params"]["_meta"]["connector_id"], "computer-use");
                assert!(outgoing["id"].as_str().unwrap().len() > 40);
                send.send(Ok(Some(
                    json!({"jsonrpc":"2.0","id":"unrelated","result":{"action":"accept"}}),
                )))
                .unwrap();
                send.send(Ok(Some(json!({"jsonrpc":"2.0","id":8,"method":"ping"}))))
                    .unwrap();
                send.send(Ok(Some(
                    json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"action":"decline"}}),
                )))
                .unwrap();
                Ok(())
            })
            .unwrap();
        assert_eq!(result, json!({"action":"decline"}));
        assert_eq!(server.pending.len(), 2);
    }
    #[test]
    #[cfg(unix)]
    fn host_routes_preserve_separate_kernels_and_reset_only_the_selected_route() {
        use std::os::unix::fs::PermissionsExt;
        let mut server = server();
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut engine = server.engine.borrow_mut();
        for id in ["a", "b"] {
            engine
                .browsers
                .register_iab(
                    id,
                    "ws://127.0.0.1:9/owned",
                    skyre::browser::iab::RouteConfig {
                        conversation_id: id.into(),
                        thread_id: None,
                        window_id: id.into(),
                    },
                )
                .unwrap();
        }
        engine.host_turns=Some(skyre::host_turns::Controller::open(serde_json::from_value(json!({"authorityToken":"a".repeat(64),"stateDirectory":directory.path(),"bindings":[{"browserId":"a","route":{"conversationId":"a"}},{"browserId":"b","route":{"conversationId":"b"}}]})).unwrap(),&mut engine.browsers).unwrap());
        drop(engine);
        let start = |route: &str, sequence: u64| json!({"authorityToken":"a".repeat(64),"event":{"eventId":route,"sequence":sequence,"phase":"started","route":{"conversationId":route},"turnId":"turn"}});
        fn call(server: &mut Server, method: &str, args: Value) -> Value {
            let reply = server
                .handle(
                    &json!({"jsonrpc":"2.0","id":1,"method":method,"params":args}),
                    &mut |_| Ok(()),
                )
                .unwrap();
            assert!(reply.get("error").is_none(), "{reply}");
            reply["result"].clone()
        }
        fn js(server: &mut Server, code: &str) -> Value {
            call(server, "js", json!({"code":code}))["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|output| output["channel"] == "output")
                .unwrap()["value"]
                .clone()
        }
        call(&mut server, "host/turn", start("a", 1));
        assert_eq!(js(&mut server, "let onlyA=41;nodeRepl.write(onlyA);"), "41");
        call(&mut server, "host/turn", start("b", 2));
        assert_eq!(
            js(&mut server, "let onlyB=99;nodeRepl.write(typeof onlyA);"),
            "undefined"
        );
        call(&mut server, "host/turn", start("a", 1));
        assert_eq!(
            js(&mut server, "nodeRepl.write(onlyA+':'+typeof onlyB);"),
            "41:undefined"
        );
        call(&mut server, "js_reset", json!({}));
        assert_eq!(
            js(&mut server, "nodeRepl.write(typeof onlyA);"),
            "undefined"
        );
        call(&mut server, "host/turn", start("b", 2));
        assert_eq!(js(&mut server, "nodeRepl.write(onlyB);"), "99");
        drop(server);
    }
    #[test]
    #[cfg(unix)]
    fn authenticated_host_transition_cancels_pending_elicitation_without_applying_early() {
        use std::os::unix::fs::PermissionsExt;
        let mut server = server();
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut engine = server.engine.borrow_mut();
        engine
            .browsers
            .register_iab(
                "iab",
                "ws://127.0.0.1:9/owned",
                skyre::browser::iab::RouteConfig {
                    conversation_id: "conversation".into(),
                    thread_id: None,
                    window_id: "window".into(),
                },
            )
            .unwrap();
        engine.host_turns=Some(skyre::host_turns::Controller::open(serde_json::from_value(json!({"authorityToken":"a".repeat(64),"stateDirectory":directory.path(),"bindings":[{"browserId":"iab","route":{"conversationId":"conversation"}}]})).unwrap(),&mut engine.browsers).unwrap());
        drop(engine);
        let (send, receive) = mpsc::sync_channel(8);
        server.input = Some(receive);
        send.send(Ok(Some(json!({"jsonrpc":"2.0","id":8,"method":"host/turn","params":{"authorityToken":"forged"}})))).unwrap();
        send.send(Ok(Some(json!({"jsonrpc":"2.0","id":9,"method":"host/turn","params":{"authorityToken":"a".repeat(64)}})))).unwrap();
        assert_eq!(
            server
                .await_elicitation(&request(), &mut |_| Ok(()))
                .unwrap_err()
                .code,
            -32800
        );
        assert_eq!(server.pending.len(), 2);
        assert!(
            server
                .engine
                .borrow_mut()
                .browsers
                .execute("info", &json!({"browser":"iab"}))
                .is_err()
        );
    }
    #[test]
    fn elicitation_timeout_disconnect_cancel_and_reset_are_bounded() {
        let mut server = server();
        server.host_options.elicitation_timeout_ms = Some(15);
        let (send, receive) = mpsc::sync_channel(8);
        server.input = Some(receive);
        let start = Instant::now();
        assert_eq!(
            server
                .await_elicitation(&request(), &mut |_| Ok(()))
                .unwrap_err()
                .code,
            -32002
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        send.send(Ok(Some(
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}),
        )))
        .unwrap();
        assert_eq!(
            server
                .await_elicitation(&request(), &mut |_| Ok(()))
                .unwrap_err()
                .code,
            -32800
        );
        send.send(Ok(Some(
            json!({"jsonrpc":"2.0","id":9,"method":"js_reset"}),
        )))
        .unwrap();
        assert_eq!(
            server
                .await_elicitation(&request(), &mut |_| Ok(()))
                .unwrap_err()
                .code,
            -32800
        );
        assert_eq!(server.pending.len(), 1);
        server.pending.clear();
        drop(send);
        assert!(
            server
                .await_elicitation(&request(), &mut |_| Ok(()))
                .unwrap_err()
                .message
                .contains("disconnected")
        );
    }
    #[test]
    fn elicitation_rejects_malformed_matching_response() {
        let mut server = server();
        let (send, receive) = mpsc::sync_channel(1);
        server.input = Some(receive);
        let error=server.await_elicitation(&request(),&mut|outgoing|{send.send(Ok(Some(json!({"jsonrpc":"2.0","id":outgoing["id"],"result":{"action":"accept"},"error":{"code":1,"message":"ambiguous"}})))).unwrap();Ok(())}).unwrap_err();
        assert!(error.message.contains("Invalid JSON-RPC response"));
    }
    #[test]
    fn broken_output_unconditionally_clears_client_capabilities_and_approval_grants() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "owned broken writer",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut server = server();
        let policy = server
            .engine
            .borrow_mut()
            .execute("sky.app_policy", &json!({"app":"fixture://native"}))
            .unwrap();
        let (_,approval)=server.engine.borrow_mut().prepare_elicitation(&json!({"meta":{"connector_id":"computer-use","tool_name":"get_app_state","tool_params":{"app":policy["target"]["bundleIdentifier"]}}})).unwrap();
        assert_eq!(approval.unwrap()["action"], "accept");
        let action = json!({"method":"get_app_state","args":[{"app":policy["target"]["appPath"]}]});
        assert!(
            server
                .engine
                .borrow_mut()
                .execute("sky.execute", &action)
                .is_ok()
        );
        let input =
            io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".to_vec());
        assert!(server.serve(input, &mut BrokenWriter, false).is_err());
        assert!(!server.mcp_initialized && !server.elicitation_supported);
        assert!(server.input.is_none() && server.host.is_none() && server.pending.is_empty());
        assert_eq!(
            server
                .engine
                .borrow_mut()
                .execute("sky.execute", &action)
                .unwrap_err()
                .code,
            -32003
        );
    }

    #[test]
    fn connection_authority_is_revoked_before_waiting_for_a_finite_writer() {
        struct HeldWriter {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
        }
        impl Write for HeldWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        struct Release(Option<mpsc::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(release) = self.0.take() {
                    let _ = release.send(());
                }
            }
        }
        let (entered, entered_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let mut writer = HeldWriter {
            entered: Some(entered),
            release: release_rx,
        };
        let mut server = server();
        let policy = server
            .engine
            .borrow_mut()
            .execute("sky.app_policy", &json!({"app":"fixture://native"}))
            .unwrap();
        server.engine.borrow_mut().prepare_elicitation(&json!({"meta":{"connector_id":"computer-use","tool_name":"get_app_state","tool_params":{"app":policy["target"]["bundleIdentifier"]}}})).unwrap();
        let action = json!({"method":"get_app_state","args":[{"app":policy["target"]["appPath"]}]});
        assert!(
            server
                .engine
                .borrow_mut()
                .execute("sky.execute", &action)
                .is_ok()
        );
        server.mcp_initialized = true;
        server.elicitation_supported = true;
        connection_output::with_finite_writer(&mut writer, false, |output| {
            // Release on both success and assertion unwinding; this embedding
            // intentionally supplies a cooperative, finite writer.
            let _release = Release(Some(release));
            let pending = output
                .enqueue(
                    &json!({"owned":true}),
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap();
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            server
                .serve_output(io::Cursor::new(Vec::<u8>::new()), output, false)
                .unwrap();
            assert!(!server.mcp_initialized && !server.elicitation_supported);
            assert!(server.input.is_none() && server.host.is_none() && server.pending.is_empty());
            assert_eq!(
                server
                    .engine
                    .borrow_mut()
                    .execute("sky.execute", &action)
                    .unwrap_err()
                    .code,
                -32003
            );
            assert!(
                pending
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            // The actual writer is still waiting for _release until here.
        });
    }
}
