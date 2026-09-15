//! Persistent CUA tools over the independent computer runtime.
//!
//! Each conversation owns its JavaScript process. Conversations execute in
//! parallel; calls within one persistent JavaScript scope remain ordered.
//! Protocol, timeout and cancellation failures discard only the affected
//! process. Model arguments cannot choose an executable, inherit credentials,
//! or change trusted runtime configuration.

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
};

// The shared JSON contract cannot represent integers above JavaScript's range.
const MAX_TIMEOUT_MS: u64 = 9_007_199_254_740_991;

/// Trusted launch configuration, supplied by the embedding application.
#[derive(Clone, Debug)]
pub struct ComputerConfig {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    /// Private Linux Hand desktop directory; resolved when a session starts.
    pub desktop_runtime: Option<PathBuf>,
}

impl ComputerConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        let mut args = Vec::new();
        for (name, flag) in [
            ("NANOCODEX_COMPUTER_SECURITY_CONFIG", "--security-config"),
            ("NANOCODEX_COMPUTER_CDP", "--cdp"),
            (
                "NANOCODEX_COMPUTER_BROWSER_PREFERENCES",
                "--browser-preferences",
            ),
            ("NANOCODEX_COMPUTER_IAB_CONFIG", "--iab-config"),
            ("NANOCODEX_COMPUTER_RUNTIME_CONFIG", "--runtime-config"),
            ("NANOCODEX_COMPUTER_PLATFORM_CONFIG", "--platform-config"),
        ] {
            if let Some(value) = std::env::var_os(name) {
                args.extend([flag.into(), value]);
            }
        }
        Self {
            executable: executable.into(),
            args,
            environment: BTreeMap::new(),
            desktop_runtime: None,
        }
    }

    /// Discover the installed companion. An explicit setting never silently
    /// falls back to a different executable.
    pub fn discover() -> Option<Self> {
        if let Some(path) = std::env::var_os("NANOCODEX_COMPUTER") {
            if path == "off" || path == "none" || path == "0" {
                return None;
            }
            return Some(Self::new(path));
        }
        let name = if cfg!(windows) {
            "nanocodex-computer.exe"
        } else {
            "nanocodex-computer"
        };
        let sibling = std::env::current_exe().ok()?.with_file_name(name);
        if sibling.is_file() {
            return Some(Self::new(sibling));
        }
        if cfg!(debug_assertions) {
            // Source builds keep the isolated runtime in its own target directory.
            for profile in ["debug", "release"] {
                let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("runtime/target")
                    .join(profile)
                    .join(name);
                if path.is_file() {
                    return Some(Self::new(path));
                }
            }
        }
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|directory| directory.join(name))
                    .find(|path| path.is_file())
            })
            .map(Self::new)
    }
}

/// Serializable invocation shared by native and VM transports.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerRequest {
    pub code: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_timeout")]
    pub timeout_ms: u64,
}
fn deserialize_timeout<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or_else(default_timeout))
}
fn default_timeout() -> u64 {
    MAX_TIMEOUT_MS
}

impl ComputerRequest {
    pub fn validate(&self) -> Result<(), ToolError> {
        if !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err("CUA timeout must be a positive safe integer in milliseconds".into());
        }
        if self
            .title
            .as_ref()
            .is_some_and(|title| title.trim().is_empty())
        {
            return Err("CUA title must be non-empty".into());
        }
        Ok(())
    }
}

/// An execution capability bound by the host to one actual computer.
#[async_trait]
pub trait ComputerExecutor: Send + Sync + 'static {
    async fn invoke(
        &self,
        request: Option<ComputerRequest>,
        context: ToolContext<'_>,
    ) -> ToolResult;
}

#[derive(Clone)]
pub struct ComputerTools {
    executor: Arc<dyn ComputerExecutor>,
}
impl ComputerTools {
    pub fn local(config: ComputerConfig) -> Self {
        let (dispatch, requests) = mpsc::unbounded_channel();
        tokio::spawn(route_sessions(config, requests));
        Self::new(LocalComputer { dispatch })
    }
    pub fn new(executor: impl ComputerExecutor) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }
    pub fn js(&self) -> ComputerTool {
        ComputerTool {
            executor: self.executor.clone(),
            reset: false,
        }
    }
    pub fn reset(&self) -> ComputerTool {
        ComputerTool {
            executor: self.executor.clone(),
            reset: true,
        }
    }
}

#[derive(Clone)]
pub struct ComputerTool {
    executor: Arc<dyn ComputerExecutor>,
    reset: bool,
}

#[async_trait]
impl Tool for ComputerTool {
    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn definition(&self) -> ToolDefinition {
        if self.reset {
            ToolDefinition::function(
                "mcp__cua_repl__js_reset",
                include_str!("../runtime/src/cua_reset_description.md"),
                json!({"type":"object","properties":{},"additionalProperties":false}),
            )
        } else {
            ToolDefinition::function(
                "mcp__cua_repl__js",
                include_str!("description.md"),
                serde_json::from_str::<Value>(include_str!("js-schema.json"))
                    .expect("embedded CUA schema is valid JSON"),
            )
        }
    }
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let request = if self.reset {
            let value = input.decode_json::<Value>()?;
            if !value.is_null() && !value.as_object().is_some_and(|object| object.is_empty()) {
                return Err("cua_repl.js_reset expects an empty object".into());
            }
            None
        } else {
            let request = input.decode_json::<ComputerRequest>()?;
            request.validate()?;
            Some(request)
        };
        self.executor.invoke(request, context).await
    }
}

struct LocalComputer {
    dispatch: mpsc::UnboundedSender<SessionRequest>,
}

struct SessionRequest {
    session: String,
    call_id: String,
    model: String,
    request: Option<ComputerRequest>,
    response: oneshot::Sender<ToolResult>,
}

#[async_trait]
impl ComputerExecutor for LocalComputer {
    async fn invoke(
        &self,
        request: Option<ComputerRequest>,
        context: ToolContext<'_>,
    ) -> ToolResult {
        if let Some(request) = &request {
            request.validate()?;
        }
        let session = context.session_id().to_owned();
        let (response, result) = oneshot::channel();
        self.dispatch
            .send(SessionRequest {
                session,
                call_id: context.call_id().to_owned(),
                model: context.model().to_owned(),
                request,
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

async fn run_session(
    config: ComputerConfig,
    session: String,
    mut requests: mpsc::UnboundedReceiver<SessionRequest>,
) {
    let mut process = None;
    let mut interrupted = false;
    while let Some(request) = requests.recv().await {
        let SessionRequest {
            call_id,
            model,
            request,
            mut response,
            ..
        } = request;
        if interrupted && request.is_some() {
            let _ = response.send(Err("CUA session ended during cancellation or transport failure. Call cua_repl.js_reset, then select the surface again.".into()));
            continue;
        }
        let timeout = Duration::from_millis(
            request
                .as_ref()
                .map_or(MAX_TIMEOUT_MS, |request| request.timeout_ms)
                + 5_000,
        );
        // Taking ownership ensures cancellation drops and kills the process.
        // The interrupted flag prevents continuation in a silently fresh scope.
        let previous = process.take();
        let execution = async {
            let mut process = match previous {
                Some(process) => process,
                None => Process::start(&config).await?,
            };
            let (name, args) = match request {
                Some(request) => ("js", serde_json::to_value(request)?),
                None => ("js_reset", json!({})),
            };
            let value = process.rpc("tools/call", json!({"name":name,"arguments":args,
                "_meta":{"x-codex-turn-metadata":{"thread_id":session,"call_id":call_id,"model":model}}})).await?;
            let output = output(value)?;
            Ok::<_, ToolError>((process, output))
        };
        let outcome = tokio::select! {
            biased;
            () = response.closed() => {
                interrupted = true;
                continue;
            }
            outcome = tokio::time::timeout(timeout, execution) => {
                outcome.map_err(|_| "CUA runtime timed out; its process was stopped. Call cua_repl.js_reset before continuing.".into()).and_then(|result| result)
            }
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
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}
impl Process {
    async fn start(config: &ComputerConfig) -> Result<Self, ToolError> {
        let mut command = Command::new(&config.executable);
        command
            .args(&config.args)
            .arg("--allow-native-control")
            .arg("serve")
            .env_clear();
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
        if let Some(directory) = &config.desktop_runtime {
            let ready: Value =
                serde_json::from_slice(&std::fs::read(directory.join("ready")).map_err(
                    |_| "This Hand's desktop is unavailable; start its screen before using CUA.",
                )?)?;
            let display = ready["display"]
                .as_str()
                .filter(|display| display.starts_with(':'))
                .ok_or("Hand desktop did not publish a local X display")?;
            command
                .env("DISPLAY", display)
                .env("XAUTHORITY", directory.join("Xauthority"))
                .env_remove("WAYLAND_DISPLAY");
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            format!(
                "Cannot start CUA companion {}: {error}. Run pnpm build:computer first.",
                config.executable.display()
            )
        })?;
        let input = child.stdin.take().ok_or("CUA stdin unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("CUA stdout unavailable")?);
        let mut process = Self {
            _child: child,
            input,
            output,
            next_id: 0,
        };
        process.rpc("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"nanocodex-computer","version":env!("CARGO_PKG_VERSION")}})).await?;
        process
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await?;
        Ok(process)
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
        loop {
            let mut line = Vec::new();
            if self.output.read_until(b'\n', &mut line).await? == 0 {
                return Err("CUA runtime closed its output".into());
            }
            let value: Value = serde_json::from_slice(&line)?;
            if value.get("method").is_some() {
                // Approval requests need an embedding-owned approval channel;
                // never synthesize acceptance from model or page content.
                if let Some(id) = value.get("id") {
                    self.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"No interactive approval channel; configure host-approved surfaces."}})).await?;
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
