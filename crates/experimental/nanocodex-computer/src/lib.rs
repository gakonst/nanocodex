//! Persistent tools from an official external Sky MCP provider.
//!
//! Each conversation owns its JavaScript process. Conversations execute in
//! parallel; calls within one persistent JavaScript scope remain ordered.
//! Protocol and cancellation failures discard only the affected
//! process. Model arguments cannot choose an executable, inherit credentials,
//! or change trusted runtime configuration.

pub mod provision;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_oai_api::{
    ImageDetail,
    tools::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolInput, ToolOutput, ToolOutputContent,
        ToolResult,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, ffi::OsString, path::PathBuf, process::Stdio, sync::Arc, time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot},
    task::{AbortHandle, JoinSet},
};

/// Trusted launch configuration, supplied by the embedding application.
#[derive(Clone, Debug)]
pub struct ComputerConfig {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    provider_catalog: Option<Vec<ProviderTool>>,
    /// Host UI callback. No elicitation capability is advertised without it.
    pub elicitation_handler: Option<Arc<dyn ComputerElicitationHandler>>,
    /// Maximum time for a host response; expiry returns `cancel`, never consent.
    pub elicitation_timeout: Duration,
}

impl ComputerConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            environment: BTreeMap::new(),
            provider_catalog: None,
            elicitation_handler: None,
            elicitation_timeout: Duration::from_secs(300),
        }
    }

    /// Configure an external CUA MCP provider with its exact host-supplied args.
    pub fn mcp(executable: impl Into<PathBuf>) -> Self {
        Self::new(executable)
    }

    /// Provision the platform's upstream runtime on first use, then discover it.
    /// Explicit provider settings (including off) never trigger installation.
    pub async fn discover_or_install() -> Result<Option<Self>, String> {
        if std::env::var_os("NANOCODEX_COMPUTER").is_none_or(|value| value.is_empty())
            && cfg!(any(target_os = "macos", target_os = "windows"))
        {
            return provision::provision_upstream(false)
                .await
                .and_then(|receipt| provision::config_from_receipt(&receipt))
                .map(Some);
        }
        Ok(Self::discover())
    }

    /// Discover only an explicitly configured or managed upstream MCP launcher.
    /// There is no custom runtime, sibling executable, or PATH fallback.
    pub fn discover() -> Option<Self> {
        if let Some(path) = std::env::var_os("NANOCODEX_COMPUTER").filter(|value| !value.is_empty())
        {
            if path == "off" || path == "none" || path == "0" {
                return None;
            }
            return Some(Self::mcp(path));
        }
        provision::managed_provider_path().map(Self::mcp)
    }
}

/// Trusted call identity for routing a form to the correct conversation UI.
#[derive(Clone, Debug)]
pub struct ComputerElicitationContext {
    pub session_id: String,
    pub call_id: String,
    pub model: String,
}

/// Provider-owned form, including the complete original `params._meta`.
/// Provider text and persistence suggestions are untrusted UI data, not consent.
#[derive(Clone, Debug)]
pub struct ComputerElicitationRequest {
    /// Opaque process lifetime. A session permission must never outlive or cross it.
    pub provider_session: std::sync::Weak<()>,
    pub id: Value,
    pub params: Value,
    /// Absent during initial provider discovery.
    pub context: Option<ComputerElicitationContext>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ComputerElicitationAction {
    Accept,
    Decline,
    Cancel,
}

/// Only the host's explicit response is sent; no content or persistence is added.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ComputerElicitationResponse {
    pub action: ComputerElicitationAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// Implemented by the embedding application's approval/form UI, never a tool.
/// The future is dropped on timeout, provider cancellation, connection closure,
/// requesting call completion, or caller cancellation. Hosts must dismiss pending
/// UI when it is dropped.
#[async_trait]
pub trait ComputerElicitationHandler: Send + Sync + std::fmt::Debug + 'static {
    async fn elicit(
        &self,
        request: ComputerElicitationRequest,
    ) -> Result<ComputerElicitationResponse, ToolError>;
}

/// An upstream execution capability bound by the host to one actual computer.
#[async_trait]
pub trait ComputerExecutor: Send + Sync + 'static {
    async fn invoke_tool(
        &self,
        name: &str,
        arguments: Value,
        context: ToolContext<'_>,
    ) -> ToolResult;
}

/// An MCP tool declaration. The provider owns its schema and documentation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ProviderTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional MCP metadata, including annotations, outputSchema and UI visibility.
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

impl ProviderTool {
    pub fn model_visible(&self) -> bool {
        self.metadata
            .get("_meta")
            .and_then(|meta| meta.pointer("/ui/visibility"))
            .and_then(Value::as_array)
            .is_none_or(|visibility| visibility.iter().any(|value| value == "model"))
    }
}

#[derive(Clone)]
pub struct ComputerTools {
    executor: Arc<dyn ComputerExecutor>,
    catalog: Arc<Vec<ProviderTool>>,
}
impl ComputerTools {
    /// Discover every MCP tool before publishing its exact description and schema.
    pub async fn connect(mut config: ComputerConfig) -> Result<Self, ToolError> {
        let process = Process::start(&config).await?;
        config.provider_catalog = Some(process.catalog.clone());
        Ok(Self::local(config))
    }
    fn local(config: ComputerConfig) -> Self {
        let catalog = Arc::new(
            config
                .provider_catalog
                .clone()
                .expect("connect discovers the upstream catalog before registration"),
        );
        let (dispatch, requests) = mpsc::unbounded_channel();
        tokio::spawn(route_sessions(config, requests));
        Self {
            executor: Arc::new(LocalComputer { dispatch }),
            catalog,
        }
    }
    /// Bind a remote transport to its discovered upstream catalog.
    pub fn new(executor: impl ComputerExecutor, catalog: Vec<ProviderTool>) -> Self {
        Self {
            executor: Arc::new(executor),
            catalog: Arc::new(catalog),
        }
    }
    /// Complete provider catalog, including tools reserved for trusted lifecycle hooks.
    pub fn catalog(&self) -> &[ProviderTool] {
        &self.catalog
    }
    /// Model-visible tools only. Hidden hooks remain available through `tool`.
    pub fn tools(&self) -> impl Iterator<Item = ComputerTool> + '_ {
        self.catalog
            .iter()
            .filter(|definition| definition.model_visible())
            .cloned()
            .map(|definition| ComputerTool {
                executor: self.executor.clone(),
                definition,
            })
    }
    pub fn tool(&self, name: &str) -> Option<ComputerTool> {
        self.catalog
            .iter()
            .find(|definition| definition.name == name)
            .cloned()
            .map(|definition| ComputerTool {
                executor: self.executor.clone(),
                definition,
            })
    }
    pub fn js(&self) -> ComputerTool {
        self.tool("js").expect("CUA provider does not publish js")
    }
    pub fn reset(&self) -> ComputerTool {
        self.tool("js_reset")
            .expect("CUA provider does not publish js_reset")
    }
}

#[derive(Clone)]
pub struct ComputerTool {
    executor: Arc<dyn ComputerExecutor>,
    definition: ProviderTool,
}

impl ComputerTool {
    pub fn provider_definition(&self) -> &ProviderTool {
        &self.definition
    }
}

#[async_trait]
impl Tool for ComputerTool {
    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }
    fn definition(&self) -> ToolDefinition {
        let definition = ToolDefinition::function(
            format!("mcp__cua_repl__{}", self.definition.name),
            self.definition.description.as_deref().unwrap_or(""),
            self.definition.input_schema.clone(),
        );
        match self.definition.metadata.get("outputSchema") {
            Some(schema) => definition.with_output_schema(schema.clone()),
            None => definition,
        }
    }
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        self.executor
            .invoke_tool(
                &self.definition.name,
                input.decode_json::<Value>()?,
                context,
            )
            .await
    }
}

struct LocalComputer {
    dispatch: mpsc::UnboundedSender<SessionRequest>,
}

struct SessionRequest {
    session: String,
    turn_id: Option<String>,
    call_id: String,
    model: String,
    name: String,
    arguments: Value,
    response: oneshot::Sender<ToolResult>,
}

#[async_trait]
impl ComputerExecutor for LocalComputer {
    async fn invoke_tool(
        &self,
        name: &str,
        arguments: Value,
        context: ToolContext<'_>,
    ) -> ToolResult {
        let session = context.session_id().to_owned();
        let (response, result) = oneshot::channel();
        self.dispatch
            .send(SessionRequest {
                session,
                turn_id: context.turn_id().map(str::to_owned),
                call_id: context.call_id().to_owned(),
                model: context.model().to_owned(),
                name: name.into(),
                arguments,
                response,
            })
            .map_err(|_| "CUA attachment is closed")?;
        result.await.map_err(|_| "CUA attachment is closed")?
    }
}

/// Route only by conversation identity. Each spawned owner has its own process
/// and queue, so an unrelated long-running cell never blocks this map or any
/// other conversation.
async fn route_sessions(
    config: ComputerConfig,
    mut requests: mpsc::UnboundedReceiver<SessionRequest>,
) {
    let mut sessions = BTreeMap::<String, mpsc::UnboundedSender<SessionRequest>>::new();
    while let Some(request) = requests.recv().await {
        let session = request.session.clone();
        let owner = sessions
            .entry(session.clone())
            .or_insert_with(|| {
                let (sender, receiver) = mpsc::unbounded_channel();
                tokio::spawn(run_session(config.clone(), session.clone(), receiver));
                sender
            })
            .clone();
        if let Err(error) = owner.send(request) {
            // A panicked owner must not permanently poison the route. Replace
            // the dead mailbox and let the request observe a fresh owner.
            let (sender, receiver) = mpsc::unbounded_channel();
            tokio::spawn(run_session(config.clone(), session.clone(), receiver));
            let _ = sender.send(error.0);
            sessions.insert(session, sender);
        }
    }
}

// Forward host context; never fabricate a turn from a per-call identifier.
fn turn_metadata(session: &str, turn: Option<&str>, call: &str, model: &str) -> Value {
    let mut metadata =
        json!({"session_id":session,"thread_id":session,"call_id":call,"model":model});
    if let Some(turn) = turn.filter(|turn| !turn.trim().is_empty()) {
        metadata["turn_id"] = json!(turn);
    }
    metadata
}

async fn run_session(
    config: ComputerConfig,
    session: String,
    mut requests: mpsc::UnboundedReceiver<SessionRequest>,
) {
    let mut process = None;
    let mut interrupted = false;
    while let Some(request) = requests.recv().await {
        let SessionRequest {
            turn_id,
            call_id,
            model,
            name,
            arguments,
            mut response,
            ..
        } = request;
        if interrupted && name != "js_reset" {
            let _ = response.send(Err("CUA session ended during cancellation or transport failure. Call cua_repl.js_reset, then select the surface again.".into()));
            continue;
        }
        // Taking ownership ensures cancellation drops and kills the process.
        // The interrupted flag prevents continuation in a silently fresh scope.
        let previous = process.take();
        let execution = async {
            let mut process = match previous {
                Some(process) => process,
                None => Process::start(&config).await?,
            };
            process.elicitation_context = Some(ComputerElicitationContext {
                session_id: session.clone(),
                call_id: call_id.clone(),
                model: model.clone(),
            });
            let value = process.rpc("tools/call", json!({"name":name,"arguments":arguments,
                "_meta":{"x-codex-turn-metadata":turn_metadata(&session, turn_id.as_deref(), &call_id, &model)}})).await?;
            let output = output(value)?;
            Ok::<_, ToolError>((process, output))
        };
        let outcome = tokio::select! {
            biased;
            () = response.closed() => {
                interrupted = true;
                continue;
            }
            outcome = execution => outcome,
        };
        match outcome {
            Ok((owned, output)) => {
                process = Some(owned);
                interrupted = false;
                if response.send(Ok(output)).is_err() {
                    // The caller disappeared at the completion boundary. Its
                    // state transition is ambiguous, so discard the process
                    // instead of silently retaining a mutated realm.
                    process = None;
                    interrupted = true;
                }
            }
            Err(error) => {
                interrupted = true;
                let _ = response.send(Err(error));
            }
        }
    }
}

struct Process {
    elicitation_session: Arc<()>,
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    catalog: Vec<ProviderTool>,
    elicitation_handler: Option<Arc<dyn ComputerElicitationHandler>>,
    elicitation_timeout: Duration,
    elicitation_context: Option<ComputerElicitationContext>,
}
impl Process {
    async fn start(config: &ComputerConfig) -> Result<Self, ToolError> {
        let mut command = Command::new(&config.executable);
        command.args(&config.args).env_clear();
        // Desktop connection and OS home variables only. Account/API tokens do
        // not cross into a model-controlled JavaScript process.
        for name in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "TMPDIR",
            "TEMP",
            "SystemRoot",
            "LOCALAPPDATA",
            "DISPLAY",
            "XAUTHORITY",
            "WAYLAND_DISPLAY",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "LANG",
            "SKY_ENABLE_AUDIO",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.envs(&config.environment);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            format!(
                "Cannot start upstream Sky MCP provider {}: {error}. Check the host provider installation.",
                config.executable.display()
            )
        })?;
        let input = child.stdin.take().ok_or("CUA stdin unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("CUA stdout unavailable")?);
        let mut process = Self {
            elicitation_session: Arc::new(()),
            _child: child,
            input,
            output,
            next_id: 0,
            catalog: Vec::new(),
            elicitation_handler: config.elicitation_handler.clone(),
            elicitation_timeout: config.elicitation_timeout,
            elicitation_context: None,
        };
        let capabilities = if config.elicitation_handler.is_some() {
            json!({"elicitation":{"form":{}}})
        } else {
            json!({})
        };
        process.rpc("initialize", json!({"protocolVersion":"2025-06-18","capabilities":capabilities,"clientInfo":{"name":"nanocodex-computer","version":env!("CARGO_PKG_VERSION")}})).await?;
        process
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        let catalog = process.discover().await?;
        if config
            .provider_catalog
            .as_ref()
            .is_some_and(|expected| expected != &catalog)
        {
            return Err(
                "CUA provider catalog changed; reconnect the attachment before invoking it".into(),
            );
        }
        process.catalog = catalog;
        Ok(process)
    }
    async fn discover(&mut self) -> Result<Vec<ProviderTool>, ToolError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut cursors = std::collections::BTreeSet::new();
        loop {
            let page = self
                .rpc(
                    "tools/list",
                    cursor
                        .as_ref()
                        .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor})),
                )
                .await?;
            tools.extend(
                page["tools"]
                    .as_array()
                    .ok_or("CUA provider did not return an MCP tools/list catalog")?
                    .iter()
                    .cloned(),
            );
            match page.get("nextCursor") {
                None => break,
                Some(value) => {
                    let next = value
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .ok_or("CUA provider returned an invalid tools/list cursor")?;
                    if !cursors.insert(next.to_owned()) {
                        return Err("CUA provider returned a repeated tools/list cursor".into());
                    }
                    cursor = Some(next.to_owned());
                }
            }
        }
        let mut names = std::collections::BTreeSet::new();
        let catalog: Vec<ProviderTool> = tools
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()?;
        for tool in &catalog {
            if tool.name.is_empty() || !names.insert(tool.name.clone()) {
                return Err("CUA provider tool names must be non-empty and unique".into());
            }
            if !tool.input_schema.is_object() {
                return Err("CUA provider tool inputSchema must be a JSON schema object".into());
            }
        }
        Ok(catalog)
    }
    async fn send(&mut self, value: Value) -> Result<(), ToolError> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value, ToolError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        // Owned tasks are aborted when this RPC is dropped or its transport closes.
        // Continue reading while the host is deciding so EOF/cancellation cannot
        // leave approval UI running against a dead provider.
        let mut elicitations = JoinSet::<(Value, Value)>::new();
        let mut pending = BTreeMap::<String, AbortHandle>::new();
        let mut line = Vec::new();
        loop {
            tokio::select! {
                completed = elicitations.join_next(), if !elicitations.is_empty() => {
                    if completed.as_ref().is_some_and(|result| result.as_ref().is_err_and(|error| !error.is_cancelled())) {
                        return Err("CUA host elicitation task failed".into());
                    }
                    if let Some(Ok((request_id, response))) = completed {
                        let key = request_id.to_string();
                        if pending.remove(&key).is_some() {
                            self.send(response).await?;
                        }
                    }
                    continue;
                }
                read = self.output.read_until(b'\n', &mut line) => {
                    if read? == 0 {
                        return Err("CUA runtime closed its output".into());
                    }
                }
            }
            let value: Value = serde_json::from_slice(&line)?;
            line.clear();
            if let Some(method) = value.get("method") {
                if method == "notifications/cancelled" {
                    if let Some(request_id) = value.pointer("/params/requestId")
                        && let Some(task) = pending.remove(&request_id.to_string())
                    {
                        task.abort();
                        self.send(
                            json!({"jsonrpc":"2.0","id":request_id,"result":{"action":"cancel"}}),
                        )
                        .await?;
                    }
                } else if let Some(request_id) = value.get("id") {
                    let params = value.get("params").cloned().unwrap_or(Value::Null);
                    let handler = self.elicitation_handler.clone();
                    if (method != "elicitation/create" && method != "openai/elicitation/create")
                        || handler.is_none()
                    {
                        self.send(json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"No host handler for this server request"}})).await?;
                        continue;
                    }
                    if !params.is_object()
                        || !params.get("message").is_some_and(Value::is_string)
                        || !params.get("requestedSchema").is_some_and(Value::is_object)
                        || params.get("mode").is_some_and(|mode| mode != "form")
                        || params.get("_meta").is_some_and(|meta| !meta.is_object())
                    {
                        self.send(json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32602,"message":"Expected a form elicitation request"}})).await?;
                        continue;
                    }
                    let key = request_id.to_string();
                    if pending.contains_key(&key) || pending.len() >= 32 {
                        return Err(
                            "CUA provider sent duplicate or excessive pending elicitations".into(),
                        );
                    }
                    let request = ComputerElicitationRequest {
                        provider_session: Arc::downgrade(&self.elicitation_session),
                        id: request_id.clone(),
                        params,
                        context: self.elicitation_context.clone(),
                    };
                    let timeout = self.elicitation_timeout;
                    let handler = handler.expect("checked above");
                    let task = elicitations.spawn(async move {
                        let response = tokio::time::timeout(timeout, handler.elicit(request.clone())).await;
                        let response = match response {
                            Ok(Ok(result)) if result.content.as_ref().is_some_and(|value| !value.is_object())
                                || result.meta.as_ref().is_some_and(|value| !value.is_object()) =>
                                json!({"jsonrpc":"2.0","id":request.id,"error":{"code":-32602,"message":"Invalid host form elicitation response"}}),
                            Ok(Ok(result)) => json!({"jsonrpc":"2.0","id":request.id,"result":result}),
                            Ok(Err(_)) => json!({"jsonrpc":"2.0","id":request.id,"error":{"code":-32603,"message":"Host form elicitation failed"}}),
                            Err(_) => json!({"jsonrpc":"2.0","id":request.id,"result":{"action":"cancel"}}),
                        };
                        (request.id, response)
                    });
                    pending.insert(key, task);
                }
                continue;
            }
            if value["id"] != id {
                return Err("CUA response belongs to a different call".into());
            }
            if let Some(error) = value.get("error") {
                return Err(format!(
                    "CUA: {}",
                    error["message"].as_str().unwrap_or("runtime error")
                )
                .into());
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| "CUA response is missing its result".into());
        }
    }
}

/// Translate MCP content into the same multimodal function output used by
/// Nanocodex's Codex/OAuth and Responses API paths.
pub fn output(value: Value) -> ToolResult {
    let mut content = Vec::new();
    for item in value["content"]
        .as_array()
        .ok_or("CUA result has no content array")?
    {
        match item["type"].as_str() {
            Some("text") => content.push(ToolOutputContent::InputText {
                text: item["text"].as_str().ok_or("Invalid CUA text")?.into(),
            }),
            Some("image") => {
                let mime = item["mimeType"].as_str().ok_or("Invalid CUA image type")?;
                if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
                    return Err("Unsupported CUA image type".into());
                }
                let data = item["data"].as_str().ok_or("Invalid CUA image")?;
                let bytes = STANDARD.decode(data)?;
                let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    "image/png"
                } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                    "image/jpeg"
                } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
                    "image/webp"
                } else {
                    return Err("Invalid CUA image bytes".into());
                };
                content.push(ToolOutputContent::InputImage {
                    image_url: format!("data:{mime};base64,{data}"),
                    detail: ImageDetail::Original,
                });
            }
            Some("audio") => {
                let mime = item["mimeType"].as_str().ok_or("Invalid CUA audio type")?;
                if !matches!(mime, "audio/wav" | "audio/mpeg") {
                    return Err("Unsupported CUA audio type".into());
                }
                let data = item["data"].as_str().ok_or("Invalid CUA audio")?;
                STANDARD.decode(data)?;
                content.push(ToolOutputContent::InputAudio {
                    audio_url: format!("data:{mime};base64,{data}"),
                });
            }
            _ => return Err("Unsupported CUA output content".into()),
        }
    }
    let success = value["isError"] != true;
    let metadata = value.get("_meta").cloned();
    let mut output = ToolOutput::content(content).with_structured_result(value);
    if let Some(metadata) = metadata {
        output = output.with_metadata(metadata);
    }
    output.success = success;
    Ok(output)
}

#[cfg(test)]
mod provider_contract_tests {
    use super::*;

    #[test]
    fn metadata_uses_host_turn_identity_instead_of_call_identity() {
        for call in ["call-a", "call-b"] {
            assert_eq!(
                turn_metadata("session", Some("session:7"), call, "fixture"),
                json!({"session_id":"session", "thread_id":"session", "turn_id":"session:7", "call_id":call, "model":"fixture"})
            );
        }
        assert!(
            turn_metadata("session", None, "call-a", "fixture")
                .get("turn_id")
                .is_none()
        );
        assert_eq!(
            turn_metadata("session", Some("session:8"), "call-c", "fixture")["turn_id"],
            "session:8"
        );
    }

    #[test]
    fn model_visibility_matches_pinned_codex_catalog_filter() {
        // codex-mcp/src/connection_manager/tool_catalog.rs::tool_is_model_visible
        for (metadata, expected) in [
            (json!({}), true),
            (json!({"_meta":{}}), true),
            (json!({"_meta":{"ui":{}}}), true),
            (json!({"_meta":{"ui":{"visibility":"model"}}}), true),
            (json!({"_meta":{"ui":{"visibility":null}}}), true),
            (json!({"_meta":{"ui":{"visibility":[]}}}), false),
            (json!({"_meta":{"ui":{"visibility":["app"]}}}), false),
            (json!({"_meta":{"ui":{"visibility":["model"]}}}), true),
            (json!({"_meta":{"ui":{"visibility":["app","model"]}}}), true),
            (
                json!({"_meta":{"ui":{"visibility":[null,3,{"model":true}]}}}),
                false,
            ),
        ] {
            let mut raw = metadata;
            raw["name"] = json!("fixture");
            raw["inputSchema"] = json!({"type":"object"});
            let definition: ProviderTool = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(definition.model_visible(), expected, "{raw}");
        }
    }

    struct EchoProvider;
    #[async_trait]
    impl ComputerExecutor for EchoProvider {
        async fn invoke_tool(
            &self,
            name: &str,
            arguments: Value,
            _: ToolContext<'_>,
        ) -> ToolResult {
            output(
                json!({"content": [{"type": "text", "text": name}], "structuredContent": arguments}),
            )
        }
    }

    #[tokio::test]
    async fn preserves_arbitrary_provider_contracts_and_routes_hidden_hooks_only_for_trusted_callers()
     {
        let catalog_json = json!([
            {"name":"js_add_node_module_dir", "description":"Provider-owned instructions", "inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}, "annotations":{"readOnlyHint":false}, "outputSchema":{"type":"object"}, "_meta":{"custom":[1,2]}},
            {"name":"future_tool", "inputSchema":{"type":"object","additionalProperties":true}},
            {"name":"turn_ended", "inputSchema":{"type":"object"}, "_meta":{"ui":{"visibility":[]}}},
            {"name":"app_only", "inputSchema":{"type":"object"}, "_meta":{"ui":{"visibility":["app"]}}}
        ]);
        let catalog: Vec<ProviderTool> = serde_json::from_value(catalog_json.clone()).unwrap();
        let tools = ComputerTools {
            executor: Arc::new(EchoProvider),
            catalog: Arc::new(catalog),
        };
        assert_eq!(serde_json::to_value(tools.catalog()).unwrap(), catalog_json);
        assert_eq!(
            tools
                .tools()
                .map(|tool| tool.definition().name().to_owned())
                .collect::<Vec<_>>(),
            [
                "mcp__cua_repl__js_add_node_module_dir",
                "mcp__cua_repl__future_tool"
            ]
        );
        let module = tools.tool("js_add_node_module_dir").unwrap();
        assert_eq!(
            module.provider_definition().metadata["annotations"],
            json!({"readOnlyHint":false})
        );
        assert!(module.definition().output_schema().is_some());
        for name in ["js_add_node_module_dir", "future_tool", "turn_ended"] {
            let args = json!({"path":"/fixture/node_modules", "timeout_ms":"provider-owned", "nested":{"value":1}});
            let result = tools
                .tool(name)
                .unwrap()
                .execute(
                    ToolInput::Function(serde_json::value::to_raw_value(&args).unwrap()),
                    ToolContext::new("fixture", "session", "call", &[], 16000),
                )
                .await
                .unwrap();
            assert_eq!(result.structured_result()["structuredContent"], args);
            assert_eq!(result.structured_result()["content"][0]["text"], name);
        }
    }
}
