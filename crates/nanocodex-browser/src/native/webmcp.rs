use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use chromiumoxide::{
    Connection, Method,
    cdp::browser_protocol::target::{SessionId, TargetId},
    error::CdpError,
    types::{CallId, EventMessage, Message, MethodId, Response},
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{sleep_until, timeout},
};
use tracing::warn;
use url::Url;

use crate::{BrowserWebMcpInvocation, BrowserWebMcpTool};

pub(super) const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub(super) const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub(super) const MAX_TOOL_RECORD_BYTES: usize = 256 * 1024;
pub(super) const MAX_TOOL_LIST_BYTES: usize = 2 * 1024 * 1024;
pub(super) const MAX_TOOL_COUNT: usize = 512;
pub(super) const MAX_INVOCATION_HISTORY: usize = 128;
pub(super) const MAX_EARLY_RESPONSES: usize = 128;

const MAX_ERROR_BYTES: usize = 64 * 1024;
const COMMAND_CAPACITY: usize = 32;
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(5);
const DISCOVERY_DELAY: Duration = Duration::from_millis(250);
const IDLE_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, thiserror::Error)]
pub(super) enum WebMcpError {
    #[error("the WebMCP observer failed: {message}")]
    Observer { message: String },
    #[error("this browser does not expose the experimental CDP WebMCP domain: {detail}")]
    Unsupported { detail: String },
    #[error("WebMCP input must be a JSON object")]
    InvalidInput,
    #[error("WebMCP input is {bytes} bytes; maximum is {maximum} bytes")]
    InputTooLarge { bytes: usize, maximum: usize },
    #[error("invalid WebMCP tool: {message}")]
    InvalidTool { message: String },
    #[error("WebMCP output is too large: {message}")]
    OutputTooLarge { message: String },
    #[error("WebMCP tool `{name}` in frame `{frame_id}` is no longer registered")]
    ToolNotFound { name: String, frame_id: String },
    #[error("unknown WebMCP invocation `{invocation_id}`")]
    InvocationNotFound { invocation_id: String },
    #[error("WebMCP invocation `{invocation_id}` is no longer active")]
    InvocationNotActive { invocation_id: String },
    #[error("WebMCP invocation history is full with {maximum} active calls")]
    TooManyInvocations { maximum: usize },
    #[error("WebMCP invocation failed: {message}")]
    InvokeFailed { message: String },
    #[error("WebMCP cancellation failed: {message}")]
    CancelFailed { message: String },
    #[error("WebMCP {operation} timed out")]
    Timeout { operation: &'static str },
}

pub(super) struct WebMcpObserver {
    commands: mpsc::Sender<ObserverCommand>,
    task: JoinHandle<()>,
}

impl WebMcpObserver {
    pub(super) async fn start(
        websocket_address: &str,
        target_id: &TargetId,
    ) -> Result<Self, WebMcpError> {
        start(websocket_address, target_id).await
    }

    pub(super) async fn activate(&self, target_id: String) -> Result<(), WebMcpError> {
        let (response, activated) = oneshot::channel();
        self.send(ObserverCommand::Activate {
            target_id,
            response,
        })
        .await?;
        timeout(INITIALIZATION_TIMEOUT, activated)
            .await
            .map_err(|_| WebMcpError::Timeout {
                operation: "target activation",
            })?
            .map_err(|_| stopped())?
    }

    pub(super) async fn list(&self) -> Result<Vec<BrowserWebMcpTool>, WebMcpError> {
        let (response, listed) = oneshot::channel();
        self.send(ObserverCommand::List { response }).await?;
        listed.await.map_err(|_| stopped())?
    }

    pub(super) async fn invoke(
        &self,
        tool: BrowserWebMcpTool,
        input: Value,
        detach: bool,
        timeout: Duration,
    ) -> Result<BrowserWebMcpInvocation, WebMcpError> {
        validate_input(&input)?;
        let (response, invoked) = oneshot::channel();
        self.send(ObserverCommand::Invoke {
            tool,
            input,
            detach,
            timeout: nonzero(timeout),
            response,
        })
        .await?;
        invoked.await.map_err(|_| stopped())?
    }

    pub(super) async fn result(
        &self,
        invocation_id: &str,
        timeout: Duration,
    ) -> Result<BrowserWebMcpInvocation, WebMcpError> {
        let (response, result) = oneshot::channel();
        self.send(ObserverCommand::Result {
            invocation_id: invocation_id.to_owned(),
            timeout: nonzero(timeout),
            response,
        })
        .await?;
        result.await.map_err(|_| stopped())?
    }

    pub(super) async fn cancel(
        &self,
        invocation_id: &str,
        timeout: Duration,
    ) -> Result<BrowserWebMcpInvocation, WebMcpError> {
        let (response, canceled) = oneshot::channel();
        self.send(ObserverCommand::Cancel {
            invocation_id: invocation_id.to_owned(),
            timeout: nonzero(timeout),
            response,
        })
        .await?;
        canceled.await.map_err(|_| stopped())?
    }

    pub(super) fn abort(&self) {
        self.task.abort();
    }

    pub(super) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    async fn send(&self, command: ObserverCommand) -> Result<(), WebMcpError> {
        self.commands.send(command).await.map_err(|_| stopped())
    }
}

impl Drop for WebMcpObserver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn start(
    websocket_address: &str,
    target_id: &TargetId,
) -> Result<WebMcpObserver, WebMcpError> {
    let mut connection = Connection::<RawCdpEvent>::connect(websocket_address)
        .await
        .map_err(observer_error)?;
    let attach_call = submit_raw(
        &mut connection,
        None,
        "Target.setAutoAttach",
        json!({
            "autoAttach": true,
            "waitForDebuggerOnStart": true,
            "flatten": true,
            "filter": [
                {"type": "page", "exclude": false},
                {"exclude": true}
            ]
        }),
    )?;
    let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (ready, ready_rx) = oneshot::channel();
    let task = tokio::spawn(run(
        connection,
        attach_call,
        target_id.as_ref().to_owned(),
        command_rx,
        ready,
    ));
    match timeout(INITIALIZATION_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(WebMcpObserver { commands, task }),
        Ok(Ok(Err(error))) => {
            task.abort();
            Err(error)
        }
        Ok(Err(_)) => {
            task.abort();
            Err(stopped())
        }
        Err(_) => {
            task.abort();
            Err(WebMcpError::Timeout {
                operation: "initialization",
            })
        }
    }
}

enum ObserverCommand {
    Activate {
        target_id: String,
        response: oneshot::Sender<Result<(), WebMcpError>>,
    },
    List {
        response: oneshot::Sender<Result<Vec<BrowserWebMcpTool>, WebMcpError>>,
    },
    Invoke {
        tool: BrowserWebMcpTool,
        input: Value,
        detach: bool,
        timeout: Duration,
        response: InvocationSender,
    },
    Result {
        invocation_id: String,
        timeout: Duration,
        response: InvocationSender,
    },
    Cancel {
        invocation_id: String,
        timeout: Duration,
        response: InvocationSender,
    },
}

type InvocationSender = oneshot::Sender<Result<BrowserWebMcpInvocation, WebMcpError>>;

#[derive(Debug, Deserialize)]
struct RawCdpEvent {
    method: MethodId,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    params: Value,
}

impl Method for RawCdpEvent {
    fn identifier(&self) -> MethodId {
        self.method.clone()
    }
}

impl EventMessage for RawCdpEvent {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

#[derive(Default)]
struct AttachedTargets {
    configured_sessions: HashSet<String>,
    session_targets: HashMap<String, String>,
    target_sessions: HashMap<String, String>,
    session_roots: HashMap<String, String>,
    activation_waiters: HashMap<String, Vec<oneshot::Sender<Result<(), WebMcpError>>>>,
}

impl AttachedTargets {
    fn root_for_session(&self, session_id: &str) -> Option<&str> {
        self.session_roots.get(session_id).map(String::as_str)
    }

    fn sessions_for_root(&self, root: &str) -> Vec<String> {
        self.session_roots
            .iter()
            .filter(|(_, candidate)| *candidate == root)
            .map(|(session, _)| session.clone())
            .collect()
    }

    fn invocation_session(&self, root: &str, tool: &ToolRecord) -> Option<String> {
        self.root_for_session(&tool.session_id)
            .filter(|candidate| *candidate == root)
            .map(|_| tool.session_id.clone())
    }
}

#[derive(Clone, Debug)]
struct ToolRecord {
    // Local child frames have no target of their own. Their registering session
    // owns invocation routing, including when that session belongs to an OOPIF.
    session_id: String,
    name: String,
    description: String,
    input_schema: Value,
    annotations: Value,
    origin: String,
    frame_id: String,
    backend_node_id: Option<i64>,
}

impl ToolRecord {
    fn to_public(&self) -> Result<BrowserWebMcpTool, WebMcpError> {
        serde_json::from_value(json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": self.annotations,
            "origin": self.origin,
            "frameId": self.frame_id,
            "backendNodeId": self.backend_node_id,
        }))
        .map_err(|error| WebMcpError::Observer {
            message: format!("failed to encode WebMCP tool: {error}"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InvocationStatus {
    Pending,
    Completed,
    Failed,
    Canceled,
    TimedOut,
}

impl InvocationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::TimedOut => "timed_out",
        }
    }

    const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Clone, Debug)]
struct InvocationRecord {
    invocation_id: String,
    tool_name: String,
    frame_id: String,
    origin: String,
    root_target_id: String,
    session_id: String,
    status: InvocationStatus,
    raw_status: Option<String>,
    output: Option<Value>,
    output_truncated: bool,
    original_output_bytes: Option<usize>,
    error: Option<String>,
    started_at: Instant,
    deadline: tokio::time::Instant,
    finished_at: Option<Instant>,
}

impl InvocationRecord {
    fn pending(
        invocation_id: String,
        tool: ToolRecord,
        root_target_id: String,
        session_id: String,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            invocation_id,
            tool_name: tool.name,
            frame_id: tool.frame_id,
            origin: tool.origin,
            root_target_id,
            session_id,
            status: InvocationStatus::Pending,
            raw_status: None,
            output: None,
            output_truncated: false,
            original_output_bytes: None,
            error: None,
            started_at: Instant::now(),
            deadline,
            finished_at: None,
        }
    }

    fn apply_completion(&mut self, completion: InvocationCompletion) {
        if self.status.is_terminal() {
            return;
        }
        self.status = completion.status;
        self.raw_status = Some(completion.raw_status);
        self.output = completion.output;
        self.output_truncated = completion.output_truncated;
        self.original_output_bytes = completion.original_output_bytes;
        self.error = completion.error;
        self.finished_at = Some(Instant::now());
    }

    fn mark_timed_out(&mut self) {
        if !self.status.is_terminal() {
            self.status = InvocationStatus::TimedOut;
            self.error = Some("WebMCP invocation timed out".to_owned());
            self.finished_at = Some(Instant::now());
        }
    }

    fn mark_context_changed(&mut self) {
        if !self.status.is_terminal() {
            self.status = InvocationStatus::Failed;
            self.error = Some(
                "webmcp_context_changed: the page context changed before the invocation completed"
                    .to_owned(),
            );
            self.finished_at = Some(Instant::now());
        }
    }

    fn to_public(&self) -> Result<BrowserWebMcpInvocation, WebMcpError> {
        serde_json::from_value(json!({
            "invocationId": self.invocation_id,
            "toolName": self.tool_name,
            "frameId": self.frame_id,
            "origin": self.origin,
            "status": self.status.as_str(),
            "rawStatus": self.raw_status,
            "output": self.output,
            "outputTruncated": self.output_truncated,
            "originalOutputBytes": self.original_output_bytes,
            "error": self.error,
            "durationMs": self.finished_at.unwrap_or_else(Instant::now).duration_since(self.started_at).as_millis() as u64,
        }))
        .map_err(|error| WebMcpError::Observer {
            message: format!("failed to encode WebMCP invocation: {error}"),
        })
    }
}

#[derive(Clone, Debug)]
struct InvocationCompletion {
    raw_status: String,
    status: InvocationStatus,
    output: Option<Value>,
    output_truncated: bool,
    original_output_bytes: Option<usize>,
    error: Option<String>,
}

impl InvocationCompletion {
    fn from_event(params: &Value) -> Self {
        let raw_status = params
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("Error")
            .to_owned();
        let status = match raw_status.as_str() {
            "Completed" => InvocationStatus::Completed,
            "Canceled" => InvocationStatus::Canceled,
            _ => InvocationStatus::Failed,
        };
        let (output, output_truncated, original_output_bytes) =
            params.get("output").map_or((None, false, None), |output| {
                let (bounded, truncated, original_bytes) = bounded_value(output, MAX_OUTPUT_BYTES);
                (
                    Some(bounded),
                    truncated,
                    truncated.then_some(original_bytes),
                )
            });
        let error = params
            .get("errorText")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| params.get("exception").map(Value::to_string))
            .map(|error| bounded_string(&error, MAX_ERROR_BYTES));
        Self {
            raw_status,
            status,
            output,
            output_truncated,
            original_output_bytes,
            error,
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    active_target_id: String,
    tools: HashMap<String, HashMap<(String, String), ToolRecord>>,
    frame_origins: HashMap<String, HashMap<String, String>>,
    tool_errors: HashMap<String, WebMcpError>,
    unsupported: HashMap<String, WebMcpError>,
    invocations: HashMap<String, InvocationRecord>,
    invocation_order: VecDeque<String>,
    early_responses: HashMap<String, InvocationCompletion>,
    early_order: VecDeque<String>,
}

impl RuntimeState {
    fn tools(&self) -> Result<Vec<BrowserWebMcpTool>, WebMcpError> {
        let root = &self.active_target_id;
        if let Some(error) = self.unsupported.get(root) {
            return Err(error.clone());
        }
        if let Some(error) = self.tool_errors.get(root) {
            return Err(error.clone());
        }
        let mut tools = self
            .tools
            .get(root)
            .into_iter()
            .flat_map(HashMap::values)
            .cloned()
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.frame_id.cmp(&right.frame_id))
        });
        tools.iter().map(ToolRecord::to_public).collect()
    }

    fn registered_tool(&self, name: &str, frame_id: &str) -> Result<ToolRecord, WebMcpError> {
        self.tools
            .get(&self.active_target_id)
            .and_then(|tools| tools.get(&(frame_id.to_owned(), name.to_owned())))
            .cloned()
            .ok_or_else(|| WebMcpError::ToolNotFound {
                name: name.to_owned(),
                frame_id: frame_id.to_owned(),
            })
    }

    fn update_frame_origin(&mut self, root: &str, frame_id: &str, origin: &str) {
        self.frame_origins
            .entry(root.to_owned())
            .or_default()
            .insert(frame_id.to_owned(), origin.to_owned());
        if let Some(tools) = self.tools.get_mut(root) {
            for tool in tools.values_mut() {
                if tool.frame_id == frame_id {
                    tool.origin = origin.to_owned();
                }
            }
            if tools.values().map(tool_encoded_len).sum::<usize>() > MAX_TOOL_LIST_BYTES {
                self.quarantine_tools(root, WebMcpError::OutputTooLarge {
                    message: "frame origins exceed the WebMCP metadata limit; navigate to reset discovery".to_owned(),
                });
            }
        }
    }

    fn apply_tools_added(&mut self, root: &str, session_id: &str, params: &Value) {
        // A partial registry cannot safely recover from removals: discarded tools
        // may still be registered. Quarantine it until the document changes.
        if self.tool_errors.contains_key(root) {
            return;
        }
        let mut bytes = self.tools.get(root).map_or(0, |tools| {
            tools.values().map(tool_encoded_len).sum::<usize>()
        });
        for raw in params
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let origin = raw
                .get("frameId")
                .and_then(Value::as_str)
                .and_then(|frame_id| {
                    self.frame_origins
                        .get(root)
                        .and_then(|origins| origins.get(frame_id))
                })
                .map_or("null", String::as_str);
            match normalize_tool(raw, origin, session_id) {
                Ok(tool) => {
                    let key = (tool.frame_id.clone(), tool.name.clone());
                    let tools = self.tools.entry(root.to_owned()).or_default();
                    let previous_bytes = tools.get(&key).map_or(0, tool_encoded_len);
                    let next_bytes = bytes
                        .saturating_sub(previous_bytes)
                        .saturating_add(tool_encoded_len(&tool));
                    if (!tools.contains_key(&key) && tools.len() >= MAX_TOOL_COUNT)
                        || next_bytes > MAX_TOOL_LIST_BYTES
                    {
                        self.quarantine_tools(root, WebMcpError::OutputTooLarge {
                            message: format!(
                                "current page exceeds {MAX_TOOL_COUNT} tools or {MAX_TOOL_LIST_BYTES} bytes of WebMCP metadata; navigate to reset discovery"
                            ),
                        });
                        return;
                    }
                    tools.insert(key, tool);
                    bytes = next_bytes;
                }
                Err(error) => {
                    self.quarantine_tools(root, error);
                    return;
                }
            }
        }
    }

    fn quarantine_tools(&mut self, root: &str, error: WebMcpError) {
        self.tools.remove(root);
        self.tool_errors.insert(root.to_owned(), error);
    }

    fn apply_tools_removed(&mut self, root: &str, params: &Value) {
        for raw in params
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(frame_id), Some(name)) = (
                raw.get("frameId").and_then(Value::as_str),
                raw.get("name").and_then(Value::as_str),
            ) && let Some(tools) = self.tools.get_mut(root)
            {
                tools.remove(&(frame_id.to_owned(), name.to_owned()));
            }
        }
    }

    fn clear_page_scope(&mut self, root: &str) {
        for invocation in self.invocations.values_mut() {
            if invocation.root_target_id == root {
                invocation.mark_context_changed();
            }
        }
        self.tools.remove(root);
        self.frame_origins.remove(root);
        self.tool_errors.remove(root);
        self.unsupported.remove(root);
    }

    fn clear_frame_scope(&mut self, root: &str, frame_id: &str) {
        for invocation in self.invocations.values_mut() {
            if invocation.root_target_id == root && invocation.frame_id == frame_id {
                invocation.mark_context_changed();
            }
        }
        if let Some(tools) = self.tools.get_mut(root) {
            tools.retain(|(candidate, _), _| candidate != frame_id);
        }
        if let Some(origins) = self.frame_origins.get_mut(root) {
            origins.remove(frame_id);
        }
    }

    fn expire_invocations(&mut self, now: tokio::time::Instant) -> Vec<(String, String)> {
        self.invocations
            .values_mut()
            .filter_map(|record| {
                if !record.status.is_terminal() && record.deadline <= now {
                    record.mark_timed_out();
                    Some((record.session_id.clone(), record.invocation_id.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    fn ensure_invocation_capacity(&mut self) -> Result<(), WebMcpError> {
        while self.invocations.len() >= MAX_INVOCATION_HISTORY {
            let terminal = self.invocation_order.iter().find_map(|id| {
                self.invocations
                    .get(id)
                    .is_some_and(|record| record.status.is_terminal())
                    .then(|| id.clone())
            });
            let Some(id) = terminal else {
                return Err(WebMcpError::TooManyInvocations {
                    maximum: MAX_INVOCATION_HISTORY,
                });
            };
            self.invocation_order.retain(|candidate| candidate != &id);
            self.invocations.remove(&id);
        }
        Ok(())
    }

    fn insert_invocation(&mut self, mut invocation: InvocationRecord) -> Result<(), WebMcpError> {
        self.ensure_invocation_capacity()?;
        if let Some(completion) = self.early_responses.remove(&invocation.invocation_id) {
            self.early_order
                .retain(|candidate| candidate != &invocation.invocation_id);
            invocation.apply_completion(completion);
        }
        let id = invocation.invocation_id.clone();
        self.invocations.insert(id.clone(), invocation);
        self.invocation_order.retain(|candidate| candidate != &id);
        self.invocation_order.push_back(id);
        Ok(())
    }

    fn apply_response(&mut self, params: &Value) -> Option<String> {
        let id = params
            .get("invocationId")
            .and_then(Value::as_str)?
            .to_owned();
        let completion = InvocationCompletion::from_event(params);
        if let Some(record) = self.invocations.get_mut(&id) {
            record.apply_completion(completion);
        } else {
            if !self.early_responses.contains_key(&id) {
                while self.early_responses.len() >= MAX_EARLY_RESPONSES {
                    let Some(oldest) = self.early_order.pop_front() else {
                        break;
                    };
                    self.early_responses.remove(&oldest);
                }
                self.early_order.push_back(id.clone());
            }
            self.early_responses.insert(id.clone(), completion);
        }
        Some(id)
    }
}

struct PendingInvoke {
    root_target_id: String,
    session_id: String,
    tool: ToolRecord,
    detach: bool,
    deadline: tokio::time::Instant,
    response: InvocationSender,
}

struct PendingCancel {
    invocation_id: String,
    deadline: tokio::time::Instant,
    response: InvocationSender,
}

struct InvocationWaiter {
    invocation_id: String,
    deadline: tokio::time::Instant,
    response: InvocationSender,
}

struct PendingList {
    deadline: tokio::time::Instant,
    response: oneshot::Sender<Result<Vec<BrowserWebMcpTool>, WebMcpError>>,
}

#[allow(
    clippy::too_many_lines,
    reason = "one actor loop owns raw CDP ordering, target ancestry, invocation races, and deadlines"
)]
async fn run(
    mut connection: Connection<RawCdpEvent>,
    attach_call: CallId,
    initial_target_id: String,
    mut commands: mpsc::Receiver<ObserverCommand>,
    ready: oneshot::Sender<Result<(), WebMcpError>>,
) {
    let mut ready = Some(ready);
    let mut attach_ready = false;
    let mut targets = AttachedTargets::default();
    let mut state = RuntimeState {
        active_target_id: initial_target_id.clone(),
        ..RuntimeState::default()
    };
    let mut enable_calls = HashMap::<CallId, (String, String)>::new();
    let mut frame_tree_calls = HashMap::<CallId, (String, String)>::new();
    let mut invokes = HashMap::<CallId, PendingInvoke>::new();
    let mut cancels = HashMap::<CallId, PendingCancel>::new();
    let mut waiters = Vec::<InvocationWaiter>::new();
    let mut lists = Vec::<PendingList>::new();
    let mut stop_error = stopped();

    loop {
        waiters.retain(|waiter| !waiter.response.is_closed());
        lists.retain(|pending| !pending.response.is_closed());
        invokes.retain(|_, pending| !pending.response.is_closed());
        cancels.retain(|_, pending| !pending.response.is_closed());
        let next_deadline = next_deadline(&state, &invokes, &cancels, &waiters, &lists)
            .unwrap_or_else(|| tokio::time::Instant::now() + IDLE_DEADLINE);

        tokio::select! {
            message = connection.next() => {
                let Some(message) = message else {
                    stop_error = WebMcpError::Observer {
                        message: "the DevTools connection closed".to_owned(),
                    };
                    break;
                };
                let message = match message {
                    Ok(message) => message,
                    Err(CdpError::InvalidMessage(_, error)) => {
                        warn!(target: "nanocodex_browser", %error, "WebMCP observer skipped a malformed DevTools message");
                        continue;
                    }
                    Err(error) => {
                        stop_error = observer_error(error);
                        break;
                    }
                };
                match message {
                    Message::Response(response) => {
                        if response.id == attach_call {
                            if let Some(error) = response.error {
                                stop_error = WebMcpError::Observer { message: error.to_string() };
                                break;
                            }
                            attach_ready = true;
                            complete_ready(&mut ready, attach_ready, &targets, &initial_target_id);
                        } else if let Some((root, session_id)) = enable_calls.remove(&response.id) {
                            if let Some(error) = response.error
                                && targets.root_for_session(&session_id) == Some(root.as_str())
                            {
                                state.unsupported.insert(root, unsupported_error(&error, "WebMCP.enable"));
                            }
                        } else if let Some((root, session_id)) = frame_tree_calls.remove(&response.id) {
                            if let Some(result) = response.result
                                && let Some(tree) = result.get("frameTree")
                            {
                                if targets.root_for_session(&session_id) == Some(root.as_str()) {
                                    apply_frame_tree(&mut state, &root, tree);
                                }
                            } else if let Some(error) = response.error {
                                warn!(target: "nanocodex_browser", %session_id, %error, "failed to read WebMCP frame origins");
                            }
                        } else if let Some(pending) = invokes.remove(&response.id) {
                            complete_invoke_response(
                                response,
                                pending,
                                &mut state,
                                &mut waiters,
                            );
                        } else if let Some(pending) = cancels.remove(&response.id) {
                            complete_cancel_response(response, pending, &mut state, &mut waiters);
                        }
                    }
                    Message::Event(event) => {
                        handle_event(
                            event,
                            &mut connection,
                            &mut targets,
                            &mut state,
                            &mut enable_calls,
                            &mut frame_tree_calls,
                        );
                        invalidate_detached_commands(&targets, &state, &mut invokes, &mut cancels);
                        complete_terminal_waiters(&state, &mut waiters);
                        complete_ready(&mut ready, attach_ready, &targets, &initial_target_id);
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                handle_command(
                    command,
                    &mut connection,
                    &mut targets,
                    &mut state,
                    &mut enable_calls,
                    &mut invokes,
                    &mut cancels,
                    &mut waiters,
                    &mut lists,
                );
            }
            () = sleep_until(next_deadline) => {
                expire_deadlines(
                    &mut connection,
                    &targets,
                    &mut state,
                    &mut invokes,
                    &mut cancels,
                    &mut waiters,
                    &mut lists,
                );
            }
        }
    }

    if let Some(ready) = ready.take() {
        let _ = ready.send(Err(stop_error.clone()));
    }
    fail_pending(
        stop_error, commands, targets, invokes, cancels, waiters, lists,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "the observer actor passes each independently bounded pending set explicitly"
)]
fn handle_command(
    command: ObserverCommand,
    connection: &mut Connection<RawCdpEvent>,
    targets: &mut AttachedTargets,
    state: &mut RuntimeState,
    enable_calls: &mut HashMap<CallId, (String, String)>,
    invokes: &mut HashMap<CallId, PendingInvoke>,
    cancels: &mut HashMap<CallId, PendingCancel>,
    waiters: &mut Vec<InvocationWaiter>,
    lists: &mut Vec<PendingList>,
) {
    match command {
        ObserverCommand::Activate {
            target_id,
            response,
        } => {
            let previous = std::mem::replace(&mut state.active_target_id, target_id.clone());
            if previous != target_id {
                for invocation in state.invocations.values_mut() {
                    if invocation.root_target_id == previous {
                        invocation.mark_context_changed();
                    }
                }
                complete_terminal_waiters(state, waiters);
            }
            if targets.target_sessions.contains_key(&target_id) {
                enable_root(connection, targets, &target_id, enable_calls);
                let _ = response.send(Ok(()));
            } else {
                targets
                    .activation_waiters
                    .entry(target_id)
                    .or_default()
                    .push(response);
            }
        }
        ObserverCommand::List { response } => {
            let root = state.active_target_id.clone();
            if let Some(error) = state.unsupported.get(&root) {
                let _ = response.send(Err(error.clone()));
                return;
            }
            enable_root(connection, targets, &root, enable_calls);
            lists.push(PendingList {
                deadline: tokio::time::Instant::now() + DISCOVERY_DELAY,
                response,
            });
        }
        ObserverCommand::Invoke {
            tool,
            input,
            detach,
            timeout,
            response,
        } => {
            let result = (|| {
                state.ensure_invocation_capacity()?;
                let (name, frame_id) = public_tool_identity(&tool)?;
                let tool = state.registered_tool(&name, &frame_id)?;
                let root = state.active_target_id.clone();
                let session_id = targets.invocation_session(&root, &tool).ok_or_else(|| {
                    WebMcpError::Observer {
                        message: format!(
                            "the DevTools session for WebMCP frame `{frame_id}` is unavailable"
                        ),
                    }
                })?;
                let call = submit_raw(
                    connection,
                    Some(SessionId::new(session_id.clone())),
                    "WebMCP.invokeTool",
                    json!({
                        "frameId": frame_id,
                        "toolName": name,
                        "input": input,
                    }),
                )?;
                Ok::<_, WebMcpError>((call, root, session_id, tool))
            })();
            match result {
                Ok((call, root_target_id, session_id, tool)) => {
                    invokes.insert(
                        call,
                        PendingInvoke {
                            root_target_id,
                            session_id,
                            tool,
                            detach,
                            deadline: tokio::time::Instant::now() + timeout,
                            response,
                        },
                    );
                }
                Err(error) => {
                    let _ = response.send(Err(error));
                }
            }
        }
        ObserverCommand::Result {
            invocation_id,
            timeout,
            response,
        } => match state.invocations.get(&invocation_id) {
            Some(record) if record.status.is_terminal() => {
                let _ = response.send(record.to_public());
            }
            Some(_) => waiters.push(InvocationWaiter {
                invocation_id,
                deadline: tokio::time::Instant::now() + timeout,
                response,
            }),
            None => {
                let _ = response.send(Err(WebMcpError::InvocationNotFound { invocation_id }));
            }
        },
        ObserverCommand::Cancel {
            invocation_id,
            timeout,
            response,
        } => {
            let Some(record) = state.invocations.get(&invocation_id) else {
                let _ = response.send(Err(WebMcpError::InvocationNotFound { invocation_id }));
                return;
            };
            if record.status.is_terminal() {
                let _ = response.send(Err(WebMcpError::InvocationNotActive { invocation_id }));
                return;
            }
            let submitted = submit_raw(
                connection,
                Some(SessionId::new(record.session_id.clone())),
                "WebMCP.cancelInvocation",
                json!({ "invocationId": invocation_id }),
            );
            match submitted {
                Ok(call) => {
                    cancels.insert(
                        call,
                        PendingCancel {
                            invocation_id,
                            deadline: tokio::time::Instant::now() + timeout,
                            response,
                        },
                    );
                }
                Err(error) => {
                    let _ = response.send(Err(error));
                }
            }
        }
    }
}

fn complete_invoke_response(
    response: Response,
    pending: PendingInvoke,
    state: &mut RuntimeState,
    waiters: &mut Vec<InvocationWaiter>,
) {
    if let Some(error) = response.error {
        let mapped = if method_is_unsupported(&error) {
            unsupported_error(&error, "WebMCP.invokeTool")
        } else {
            WebMcpError::InvokeFailed {
                message: error.to_string(),
            }
        };
        let _ = pending.response.send(Err(mapped));
        return;
    }
    let invocation_id = response
        .result
        .as_ref()
        .and_then(|result| result.get("invocationId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(invocation_id) = invocation_id else {
        let _ = pending.response.send(Err(WebMcpError::InvokeFailed {
            message: "DevTools omitted invocationId".to_owned(),
        }));
        return;
    };
    let record = InvocationRecord::pending(
        invocation_id.clone(),
        pending.tool,
        pending.root_target_id,
        pending.session_id,
        pending.deadline,
    );
    if let Err(error) = state.insert_invocation(record) {
        let _ = pending.response.send(Err(error));
        return;
    }
    let terminal = state
        .invocations
        .get(&invocation_id)
        .is_some_and(|record| record.status.is_terminal());
    if pending.detach || terminal {
        let result = state.invocations[&invocation_id].to_public();
        let _ = pending.response.send(result);
    } else {
        waiters.push(InvocationWaiter {
            invocation_id,
            deadline: pending.deadline,
            response: pending.response,
        });
    }
}

fn complete_cancel_response(
    response: Response,
    pending: PendingCancel,
    state: &mut RuntimeState,
    waiters: &mut Vec<InvocationWaiter>,
) {
    if let Some(error) = response.error {
        if let Some(record) = state.invocations.get(&pending.invocation_id)
            && record.status.is_terminal()
        {
            let _ = pending.response.send(record.to_public());
            return;
        }
        let mapped = if method_is_unsupported(&error) {
            unsupported_error(&error, "WebMCP.cancelInvocation")
        } else if error
            .message
            .to_ascii_lowercase()
            .contains("no pending execution")
        {
            WebMcpError::InvocationNotActive {
                invocation_id: pending.invocation_id,
            }
        } else {
            WebMcpError::CancelFailed {
                message: error.to_string(),
            }
        };
        let _ = pending.response.send(Err(mapped));
        return;
    }
    if let Some(record) = state.invocations.get(&pending.invocation_id)
        && record.status.is_terminal()
    {
        let _ = pending.response.send(record.to_public());
    } else {
        waiters.push(InvocationWaiter {
            invocation_id: pending.invocation_id,
            deadline: pending.deadline,
            response: pending.response,
        });
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "deadline cleanup owns all bounded pending operation classes"
)]
fn expire_deadlines(
    connection: &mut Connection<RawCdpEvent>,
    _targets: &AttachedTargets,
    state: &mut RuntimeState,
    invokes: &mut HashMap<CallId, PendingInvoke>,
    cancels: &mut HashMap<CallId, PendingCancel>,
    waiters: &mut Vec<InvocationWaiter>,
    lists: &mut Vec<PendingList>,
) {
    let now = tokio::time::Instant::now();
    let expired_invokes = invokes
        .iter()
        .filter_map(|(id, pending)| (pending.deadline <= now).then_some(*id))
        .collect::<Vec<_>>();
    for id in expired_invokes {
        if let Some(pending) = invokes.remove(&id) {
            let _ = pending.response.send(Err(WebMcpError::Timeout {
                operation: "invoke command",
            }));
        }
    }
    let expired_cancels = cancels
        .iter()
        .filter_map(|(id, pending)| (pending.deadline <= now).then_some(*id))
        .collect::<Vec<_>>();
    for id in expired_cancels {
        if let Some(pending) = cancels.remove(&id) {
            let _ = pending.response.send(Err(WebMcpError::Timeout {
                operation: "cancel command",
            }));
        }
    }

    for (session_id, invocation_id) in state.expire_invocations(now) {
        let _ = submit_raw(
            connection,
            Some(SessionId::new(session_id)),
            "WebMCP.cancelInvocation",
            json!({ "invocationId": invocation_id }),
        );
    }

    let expired_ids = waiters
        .iter()
        .filter(|waiter| waiter.deadline <= now)
        .map(|waiter| waiter.invocation_id.clone())
        .collect::<HashSet<_>>();
    for invocation_id in expired_ids {
        if let Some(record) = state.invocations.get_mut(&invocation_id)
            && !record.status.is_terminal()
        {
            let _ = submit_raw(
                connection,
                Some(SessionId::new(record.session_id.clone())),
                "WebMCP.cancelInvocation",
                json!({ "invocationId": invocation_id }),
            );
            record.mark_timed_out();
        }
    }
    complete_terminal_waiters(state, waiters);

    let mut index = 0;
    while index < lists.len() {
        if lists[index].deadline <= now {
            let pending = lists.swap_remove(index);
            let _ = pending.response.send(state.tools());
        } else {
            index += 1;
        }
    }
}

fn complete_terminal_waiters(state: &RuntimeState, waiters: &mut Vec<InvocationWaiter>) {
    let mut index = 0;
    while index < waiters.len() {
        let terminal = state
            .invocations
            .get(&waiters[index].invocation_id)
            .is_some_and(|record| record.status.is_terminal());
        if terminal {
            let waiter = waiters.swap_remove(index);
            let result = state.invocations[&waiter.invocation_id].to_public();
            let _ = waiter.response.send(result);
        } else {
            index += 1;
        }
    }
}

fn next_deadline(
    state: &RuntimeState,
    invokes: &HashMap<CallId, PendingInvoke>,
    cancels: &HashMap<CallId, PendingCancel>,
    waiters: &[InvocationWaiter],
    lists: &[PendingList],
) -> Option<tokio::time::Instant> {
    invokes
        .values()
        .map(|pending| pending.deadline)
        .chain(cancels.values().map(|pending| pending.deadline))
        .chain(waiters.iter().map(|pending| pending.deadline))
        .chain(lists.iter().map(|pending| pending.deadline))
        .chain(
            state
                .invocations
                .values()
                .filter(|record| !record.status.is_terminal())
                .map(|record| record.deadline),
        )
        .min()
}

fn handle_event(
    event: RawCdpEvent,
    connection: &mut Connection<RawCdpEvent>,
    targets: &mut AttachedTargets,
    state: &mut RuntimeState,
    enable_calls: &mut HashMap<CallId, (String, String)>,
    frame_tree_calls: &mut HashMap<CallId, (String, String)>,
) {
    if event.method == "Target.attachedToTarget" {
        handle_attached(
            event,
            connection,
            targets,
            state,
            enable_calls,
            frame_tree_calls,
        );
        return;
    }
    if event.method == "Target.detachedFromTarget" {
        if let Some(session_id) = event.params.get("sessionId").and_then(Value::as_str) {
            detach_session(session_id, targets, state);
        }
        return;
    }
    let Some(session_id) = event.session_id.as_deref() else {
        return;
    };
    let Some(root) = targets.root_for_session(session_id).map(str::to_owned) else {
        return;
    };
    match event.method.as_ref() {
        "WebMCP.toolsAdded" => state.apply_tools_added(&root, session_id, &event.params),
        "WebMCP.toolsRemoved" => state.apply_tools_removed(&root, &event.params),
        "WebMCP.toolResponded" => {
            state.apply_response(&event.params);
        }
        "Page.frameNavigated" => {
            if let Some(frame) = event.params.get("frame") {
                let frame_id = frame.get("id").and_then(Value::as_str);
                let is_root_frame =
                    frame.get("parentId").is_none() && frame_id.is_some_and(|id| id == root);
                if is_root_frame {
                    state.clear_page_scope(&root);
                } else if let Some(frame_id) = frame_id {
                    state.clear_frame_scope(&root, frame_id);
                }
                if let (Some(frame_id), Some(origin)) = (frame_id, frame_origin(frame)) {
                    state.update_frame_origin(&root, frame_id, &origin);
                }
            }
        }
        "Page.frameDetached" => {
            if let Some(frame_id) = event.params.get("frameId").and_then(Value::as_str) {
                state.clear_frame_scope(&root, frame_id);
            }
        }
        _ => {}
    }
}

fn handle_attached(
    event: RawCdpEvent,
    connection: &mut Connection<RawCdpEvent>,
    targets: &mut AttachedTargets,
    state: &mut RuntimeState,
    enable_calls: &mut HashMap<CallId, (String, String)>,
    frame_tree_calls: &mut HashMap<CallId, (String, String)>,
) {
    let parent_session = event.session_id;
    let Some(session_id) = event
        .params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let Some(target_info) = event.params.get("targetInfo") else {
        return;
    };
    let Some(target_id) = target_info
        .get("targetId")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let target_type = target_info
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let root = if target_type == "page" {
        target_id.clone()
    } else if target_type == "iframe" {
        let Some(root) = parent_session
            .as_deref()
            .and_then(|parent| targets.root_for_session(parent))
            .map(str::to_owned)
        else {
            resume_target(connection, &session_id);
            return;
        };
        root
    } else {
        resume_target(connection, &session_id);
        return;
    };

    targets.configured_sessions.insert(session_id.clone());
    targets
        .session_targets
        .insert(session_id.clone(), target_id.clone());
    targets
        .target_sessions
        .insert(target_id.clone(), session_id.clone());
    targets
        .session_roots
        .insert(session_id.clone(), root.clone());
    if let Err(error) = configure_session(
        connection,
        &session_id,
        &root,
        enable_calls,
        frame_tree_calls,
    ) {
        state.unsupported.insert(root.clone(), error);
    }
    if let Some(waiters) = targets.activation_waiters.remove(&target_id) {
        for waiter in waiters {
            let _ = waiter.send(Ok(()));
        }
    }
}

fn configure_session(
    connection: &mut Connection<RawCdpEvent>,
    session_id: &str,
    root: &str,
    enable_calls: &mut HashMap<CallId, (String, String)>,
    frame_tree_calls: &mut HashMap<CallId, (String, String)>,
) -> Result<(), WebMcpError> {
    let session = SessionId::new(session_id.to_owned());
    submit_raw(
        connection,
        Some(session.clone()),
        "Target.setAutoAttach",
        json!({
            "autoAttach": true,
            "waitForDebuggerOnStart": true,
            "flatten": true,
            "filter": [
                {"type": "iframe", "exclude": false},
                {"exclude": true}
            ]
        }),
    )?;
    submit_raw(connection, Some(session.clone()), "Page.enable", json!({}))?;
    let frame_tree = submit_raw(
        connection,
        Some(session.clone()),
        "Page.getFrameTree",
        json!({}),
    )?;
    frame_tree_calls.insert(frame_tree, (root.to_owned(), session_id.to_owned()));
    let enable = submit_raw(
        connection,
        Some(session.clone()),
        "WebMCP.enable",
        json!({}),
    )?;
    enable_calls.insert(enable, (root.to_owned(), session_id.to_owned()));
    submit_raw(
        connection,
        Some(session),
        "Runtime.runIfWaitingForDebugger",
        json!({}),
    )?;
    Ok(())
}

fn enable_root(
    connection: &mut Connection<RawCdpEvent>,
    targets: &AttachedTargets,
    root: &str,
    enable_calls: &mut HashMap<CallId, (String, String)>,
) {
    for session_id in targets.sessions_for_root(root) {
        match submit_raw(
            connection,
            Some(SessionId::new(session_id.clone())),
            "WebMCP.enable",
            json!({}),
        ) {
            Ok(call) => {
                enable_calls.insert(call, (root.to_owned(), session_id));
            }
            Err(error) => {
                warn!(target: "nanocodex_browser", %error, "failed to submit WebMCP discovery command");
            }
        }
    }
}

fn resume_target(connection: &mut Connection<RawCdpEvent>, session_id: &str) {
    if let Err(error) = submit_raw(
        connection,
        Some(SessionId::new(session_id.to_owned())),
        "Runtime.runIfWaitingForDebugger",
        json!({}),
    ) {
        warn!(target: "nanocodex_browser", %error, "failed to resume an irrelevant WebMCP child target");
    }
}

fn detach_session(session_id: &str, targets: &mut AttachedTargets, state: &mut RuntimeState) {
    let Some(root) = targets.session_roots.get(session_id).cloned() else {
        return;
    };
    let is_root = targets.session_targets.get(session_id) == Some(&root);
    let detached = if is_root {
        state.clear_page_scope(&root);
        targets.sessions_for_root(&root)
    } else {
        vec![session_id.to_owned()]
    };
    for session in detached {
        targets.configured_sessions.remove(&session);
        targets.session_roots.remove(&session);
        if let Some(target_id) = targets.session_targets.remove(&session)
            && targets.target_sessions.get(&target_id) == Some(&session)
        {
            targets.target_sessions.remove(&target_id);
        }
        if !is_root {
            let frames = state
                .tools
                .get(&root)
                .into_iter()
                .flat_map(HashMap::values)
                .filter(|tool| tool.session_id == session)
                .map(|tool| tool.frame_id.clone())
                .collect::<Vec<_>>();
            for frame_id in frames {
                state.clear_frame_scope(&root, &frame_id);
            }
            for invocation in state.invocations.values_mut() {
                if invocation.session_id == session {
                    invocation.mark_context_changed();
                }
            }
        }
    }
}

fn invalidate_detached_commands(
    targets: &AttachedTargets,
    state: &RuntimeState,
    invokes: &mut HashMap<CallId, PendingInvoke>,
    cancels: &mut HashMap<CallId, PendingCancel>,
) {
    let detached = invokes
        .iter()
        .filter(|(_, pending)| targets.root_for_session(&pending.session_id).is_none())
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    for id in detached {
        if let Some(pending) = invokes.remove(&id) {
            let _ = pending.response.send(Err(WebMcpError::InvokeFailed {
                message: "webmcp_context_changed: the DevTools target detached".to_owned(),
            }));
        }
    }
    let terminal = cancels
        .iter()
        .filter(|(_, pending)| {
            state
                .invocations
                .get(&pending.invocation_id)
                .is_some_and(|record| record.status.is_terminal())
        })
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    for id in terminal {
        if let Some(pending) = cancels.remove(&id) {
            let _ = pending
                .response
                .send(state.invocations[&pending.invocation_id].to_public());
        }
    }
}

fn complete_ready(
    ready: &mut Option<oneshot::Sender<Result<(), WebMcpError>>>,
    attach_ready: bool,
    targets: &AttachedTargets,
    target_id: &str,
) {
    if attach_ready
        && targets.target_sessions.contains_key(target_id)
        && let Some(ready) = ready.take()
    {
        let _ = ready.send(Ok(()));
    }
}

fn apply_frame_tree(state: &mut RuntimeState, root: &str, tree: &Value) {
    if let Some(frame) = tree.get("frame")
        && let (Some(frame_id), Some(origin)) =
            (frame.get("id").and_then(Value::as_str), frame_origin(frame))
    {
        state.update_frame_origin(root, frame_id, &origin);
    }
    for child in tree
        .get("childFrames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        apply_frame_tree(state, root, child);
    }
}

fn frame_origin(frame: &Value) -> Option<String> {
    if let Some(origin) = frame
        .get("securityOrigin")
        .and_then(Value::as_str)
        .filter(|origin| !origin.is_empty() && *origin != "://")
    {
        return Some(origin.to_owned());
    }
    frame.get("url").and_then(Value::as_str).map(|value| {
        Url::parse(value)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| value.to_owned())
    })
}

fn normalize_tool(raw: &Value, origin: &str, session_id: &str) -> Result<ToolRecord, WebMcpError> {
    let bytes = serde_json::to_vec(raw)
        .map_err(|error| WebMcpError::InvalidTool {
            message: error.to_string(),
        })?
        .len();
    if bytes > MAX_TOOL_RECORD_BYTES {
        return Err(WebMcpError::OutputTooLarge {
            message: format!(
                "one tool record is {bytes} bytes; maximum is {MAX_TOOL_RECORD_BYTES} bytes"
            ),
        });
    }
    Ok(ToolRecord {
        session_id: session_id.to_owned(),
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| WebMcpError::InvalidTool {
                message: "record is missing name".to_owned(),
            })?
            .to_owned(),
        description: raw
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        input_schema: raw.get("inputSchema").cloned().unwrap_or_else(|| json!({})),
        annotations: raw.get("annotations").cloned().unwrap_or_else(|| json!({})),
        origin: origin.to_owned(),
        frame_id: raw
            .get("frameId")
            .and_then(Value::as_str)
            .ok_or_else(|| WebMcpError::InvalidTool {
                message: "record is missing frameId".to_owned(),
            })?
            .to_owned(),
        backend_node_id: raw.get("backendNodeId").and_then(Value::as_i64),
    })
}

fn tool_encoded_len(tool: &ToolRecord) -> usize {
    serde_json::to_vec(&json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": tool.annotations,
        "origin": tool.origin,
        "frameId": tool.frame_id,
        "backendNodeId": tool.backend_node_id,
    }))
    .map_or(0, |encoded| encoded.len())
}

fn public_tool_identity(tool: &BrowserWebMcpTool) -> Result<(String, String), WebMcpError> {
    let value = serde_json::to_value(tool).map_err(|error| WebMcpError::InvalidTool {
        message: error.to_string(),
    })?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| WebMcpError::InvalidTool {
            message: "public tool is missing name".to_owned(),
        })?
        .to_owned();
    let frame_id = value
        .get("frameId")
        .and_then(Value::as_str)
        .ok_or_else(|| WebMcpError::InvalidTool {
            message: "public tool is missing frameId".to_owned(),
        })?
        .to_owned();
    Ok((name, frame_id))
}

fn validate_input(input: &Value) -> Result<(), WebMcpError> {
    if !input.is_object() {
        return Err(WebMcpError::InvalidInput);
    }
    let bytes = serde_json::to_vec(input)
        .map_err(|error| WebMcpError::Observer {
            message: error.to_string(),
        })?
        .len();
    if bytes > MAX_INPUT_BYTES {
        return Err(WebMcpError::InputTooLarge {
            bytes,
            maximum: MAX_INPUT_BYTES,
        });
    }
    Ok(())
}

fn bounded_value(value: &Value, maximum: usize) -> (Value, bool, usize) {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    if encoded.len() <= maximum {
        return (value.clone(), false, encoded.len());
    }
    let preview_bytes = maximum.min(64 * 1024);
    (
        json!({
            "truncated": true,
            "preview": String::from_utf8_lossy(&encoded[..preview_bytes]),
        }),
        true,
        encoded.len(),
    )
}

fn bounded_string(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!(
        "{}\n[truncated: showing {} of {} bytes]",
        &value[..boundary],
        boundary,
        value.len()
    )
}

fn submit_raw(
    connection: &mut Connection<RawCdpEvent>,
    session_id: Option<SessionId>,
    method: &'static str,
    params: Value,
) -> Result<CallId, WebMcpError> {
    connection
        .submit_command(method.into(), session_id, params)
        .map_err(|error| WebMcpError::Observer {
            message: error.to_string(),
        })
}

fn unsupported_error(error: &chromiumoxide::types::Error, method: &str) -> WebMcpError {
    if method_is_unsupported(error) {
        WebMcpError::Unsupported {
            detail: format!("{method}: {error}"),
        }
    } else {
        WebMcpError::Observer {
            message: format!("{method}: {error}"),
        }
    }
}

fn method_is_unsupported(error: &chromiumoxide::types::Error) -> bool {
    error.code == -32601
        || error.message.contains("wasn't found")
        || error.message.contains("Method not found")
}

fn nonzero(duration: Duration) -> Duration {
    duration.max(Duration::from_millis(1))
}

fn stopped() -> WebMcpError {
    WebMcpError::Observer {
        message: "the observer task stopped".to_owned(),
    }
}

fn observer_error(error: impl std::fmt::Display) -> WebMcpError {
    WebMcpError::Observer {
        message: error.to_string(),
    }
}

fn fail_pending(
    error: WebMcpError,
    mut commands: mpsc::Receiver<ObserverCommand>,
    mut targets: AttachedTargets,
    invokes: HashMap<CallId, PendingInvoke>,
    cancels: HashMap<CallId, PendingCancel>,
    waiters: Vec<InvocationWaiter>,
    lists: Vec<PendingList>,
) {
    for waiters in targets
        .activation_waiters
        .drain()
        .map(|(_, waiters)| waiters)
    {
        for waiter in waiters {
            let _ = waiter.send(Err(error.clone()));
        }
    }
    for pending in invokes.into_values() {
        let _ = pending.response.send(Err(error.clone()));
    }
    for pending in cancels.into_values() {
        let _ = pending.response.send(Err(error.clone()));
    }
    for pending in waiters {
        let _ = pending.response.send(Err(error.clone()));
    }
    for pending in lists {
        let _ = pending.response.send(Err(error.clone()));
    }
    while let Ok(command) = commands.try_recv() {
        match command {
            ObserverCommand::Activate { response, .. } => {
                let _ = response.send(Err(error.clone()));
            }
            ObserverCommand::List { response } => {
                let _ = response.send(Err(error.clone()));
            }
            ObserverCommand::Invoke { response, .. }
            | ObserverCommand::Result { response, .. }
            | ObserverCommand::Cancel { response, .. } => {
                let _ = response.send(Err(error.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_tool(name: &str, frame_id: &str) -> Value {
        json!({
            "name": name,
            "description": "fixture",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnly": true},
            "frameId": frame_id,
        })
    }

    fn pending(id: &str, frame_id: &str) -> InvocationRecord {
        InvocationRecord::pending(
            id.to_owned(),
            normalize_tool(
                &raw_tool("tool", frame_id),
                "https://example.test",
                "session",
            )
            .unwrap(),
            "root".to_owned(),
            "session".to_owned(),
            tokio::time::Instant::now() + Duration::from_secs(30),
        )
    }

    #[test]
    fn validates_input_shape_and_bytes() {
        assert!(matches!(
            validate_input(&json!([])),
            Err(WebMcpError::InvalidInput)
        ));
        assert!(validate_input(&json!({"value": "ok"})).is_ok());
        assert!(matches!(
            validate_input(&json!({"value": "x".repeat(MAX_INPUT_BYTES)})),
            Err(WebMcpError::InputTooLarge { .. })
        ));
    }

    #[test]
    fn bounds_output_and_error_without_splitting_utf8() {
        let (output, truncated, original) = bounded_value(
            &json!({"value": "x".repeat(MAX_OUTPUT_BYTES)}),
            MAX_OUTPUT_BYTES,
        );
        assert!(truncated);
        assert!(original > MAX_OUTPUT_BYTES);
        assert!(serde_json::to_vec(&output).unwrap().len() < MAX_OUTPUT_BYTES);

        let error = bounded_string(&"é".repeat(MAX_ERROR_BYTES), MAX_ERROR_BYTES + 1);
        assert!(error.contains("[truncated:"));
        assert!(error.is_char_boundary(error.len()));
    }

    #[test]
    fn tool_registry_is_frame_scoped_sorted_and_origin_aware() {
        let mut state = RuntimeState {
            active_target_id: "root".to_owned(),
            ..RuntimeState::default()
        };
        state.update_frame_origin("root", "main", "https://main.test");
        state.update_frame_origin("root", "child", "https://child.test");
        state.apply_tools_added(
            "root",
            "session",
            &json!({"tools": [raw_tool("search", "main"), raw_tool("search", "child")]}),
        );
        let tools = state.tools.get("root").unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[&("child".to_owned(), "search".to_owned())].origin,
            "https://child.test"
        );
        state.clear_frame_scope("root", "child");
        assert_eq!(state.tools.get("root").unwrap().len(), 1);
    }

    #[test]
    fn aggregate_tool_bound_quarantines_until_navigation() {
        let mut state = RuntimeState {
            active_target_id: "root".to_owned(),
            ..RuntimeState::default()
        };
        let tools = (0..=MAX_TOOL_COUNT)
            .map(|index| raw_tool(&format!("tool-{index}"), "main"))
            .collect::<Vec<_>>();
        state.apply_tools_added("root", "session", &json!({"tools": tools}));
        assert!(matches!(
            state.tools(),
            Err(WebMcpError::OutputTooLarge { .. })
        ));
        state.apply_tools_removed(
            "root",
            &json!({"tools": [{"name": format!("tool-{MAX_TOOL_COUNT}"), "frameId": "main"}]}),
        );
        assert!(!state.tools.contains_key("root"));
        assert!(state.tools().is_err());
        for index in 0..MAX_TOOL_COUNT {
            state.apply_tools_added(
                "root",
                "session",
                &json!({
                    "tools": [raw_tool(&format!("extra-{index}"), "main")]
                }),
            );
        }
        assert!(!state.tools.contains_key("root"));
        state.clear_page_scope("root");
        state.apply_tools_added(
            "root",
            "session",
            &json!({"tools": [raw_tool("fresh", "main")]}),
        );
        assert_eq!(state.tools().unwrap().len(), 1);
    }

    #[test]
    fn aggregate_metadata_bytes_are_bounded_before_retention() {
        let mut state = RuntimeState {
            active_target_id: "root".to_owned(),
            ..RuntimeState::default()
        };
        for index in 0..32 {
            let mut tool = raw_tool(&format!("large-{index}"), "main");
            tool["description"] = json!("x".repeat(MAX_TOOL_RECORD_BYTES / 2));
            state.apply_tools_added("root", "session", &json!({"tools": [tool]}));
            assert!(
                state
                    .tools
                    .get("root")
                    .into_iter()
                    .flat_map(HashMap::values)
                    .map(tool_encoded_len)
                    .sum::<usize>()
                    <= MAX_TOOL_LIST_BYTES
            );
        }
        assert!(matches!(
            state.tools(),
            Err(WebMcpError::OutputTooLarge { .. })
        ));
        assert!(!state.tools.contains_key("root"));
    }

    fn attach_fixture(targets: &mut AttachedTargets, root: &str, target: &str, session: &str) {
        targets.configured_sessions.insert(session.to_owned());
        targets
            .session_targets
            .insert(session.to_owned(), target.to_owned());
        targets
            .target_sessions
            .insert(target.to_owned(), session.to_owned());
        targets
            .session_roots
            .insert(session.to_owned(), root.to_owned());
    }

    #[test]
    fn local_child_of_oopif_routes_to_its_registering_session() {
        let mut targets = AttachedTargets::default();
        attach_fixture(&mut targets, "root", "root", "root-session");
        attach_fixture(&mut targets, "root", "oopif", "oopif-session");
        let mut state = RuntimeState {
            active_target_id: "root".to_owned(),
            ..RuntimeState::default()
        };
        state.apply_tools_added(
            "root",
            "oopif-session",
            &json!({
                "tools": [raw_tool("nested", "local-child")]
            }),
        );
        let tool = state.registered_tool("nested", "local-child").unwrap();
        assert_eq!(
            targets.invocation_session("root", &tool).as_deref(),
            Some("oopif-session")
        );
        assert_eq!(targets.invocation_session("another-root", &tool), None);
        detach_session("oopif-session", &mut targets, &mut state);
        assert_eq!(targets.invocation_session("root", &tool), None);
        assert!(state.tools().unwrap().is_empty());
    }

    #[test]
    fn detached_invocation_keeps_deadline_without_waiters() {
        let mut state = RuntimeState::default();
        let now = tokio::time::Instant::now();
        let (response, mut result) = oneshot::channel();
        let mut waiters = Vec::new();
        complete_invoke_response(
            serde_json::from_value(json!({"id": 1, "result": {"invocationId": "detached"}}))
                .unwrap(),
            PendingInvoke {
                root_target_id: "root".to_owned(),
                session_id: "session".to_owned(),
                tool: normalize_tool(&raw_tool("tool", "main"), "https://example.test", "session")
                    .unwrap(),
                detach: true,
                deadline: now,
                response,
            },
            &mut state,
            &mut waiters,
        );
        assert_eq!(
            result.try_recv().unwrap().unwrap().status,
            crate::BrowserWebMcpInvocationStatus::Pending
        );
        assert!(waiters.is_empty());
        assert_eq!(
            next_deadline(&state, &HashMap::new(), &HashMap::new(), &waiters, &[]),
            Some(now)
        );
        assert_eq!(
            state.expire_invocations(now),
            vec![("session".to_owned(), "detached".to_owned())]
        );
        assert_eq!(
            state.invocations["detached"].status,
            InvocationStatus::TimedOut
        );
        assert!(state.expire_invocations(now).is_empty());
        assert_eq!(
            next_deadline(&state, &HashMap::new(), &HashMap::new(), &waiters, &[]),
            None
        );
    }

    #[test]
    fn root_detachment_clears_descendants_and_pending_commands_only_for_that_page() {
        let mut targets = AttachedTargets::default();
        attach_fixture(&mut targets, "root", "root", "session");
        attach_fixture(&mut targets, "root", "oopif", "child-session");
        attach_fixture(&mut targets, "other", "other", "other-session");
        let mut state = RuntimeState {
            active_target_id: "other".to_owned(),
            ..RuntimeState::default()
        };
        state.update_frame_origin("root", "main", "https://example.test");
        state.apply_tools_added(
            "root",
            "session",
            &json!({"tools": [raw_tool("tool", "main")]}),
        );
        state.apply_tools_added(
            "root",
            "child-session",
            &json!({"tools": [raw_tool("child", "nested")]}),
        );
        state.apply_tools_added(
            "other",
            "other-session",
            &json!({"tools": [raw_tool("other", "other")]}),
        );
        state.insert_invocation(pending("active", "main")).unwrap();
        let (response, mut invoked) = oneshot::channel();
        let mut invokes = HashMap::from([(
            serde_json::from_value::<Response>(json!({"id": 1, "result": {}}))
                .unwrap()
                .id,
            PendingInvoke {
                root_target_id: "root".to_owned(),
                session_id: "session".to_owned(),
                tool: normalize_tool(&raw_tool("tool", "main"), "https://example.test", "session")
                    .unwrap(),
                detach: true,
                deadline: tokio::time::Instant::now() + Duration::from_secs(30),
                response,
            },
        )]);
        let (response, mut canceled) = oneshot::channel();
        let mut cancels = HashMap::from([(
            serde_json::from_value::<Response>(json!({"id": 2, "result": {}}))
                .unwrap()
                .id,
            PendingCancel {
                invocation_id: "active".to_owned(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(30),
                response,
            },
        )]);
        detach_session("session", &mut targets, &mut state);
        invalidate_detached_commands(&targets, &state, &mut invokes, &mut cancels);
        assert!(!state.tools.contains_key("root"));
        assert!(!state.frame_origins.contains_key("root"));
        assert!(targets.sessions_for_root("root").is_empty());
        assert!(!targets.target_sessions.contains_key("oopif"));
        assert_eq!(state.invocations["active"].status, InvocationStatus::Failed);
        assert!(invokes.is_empty());
        assert!(matches!(
            invoked.try_recv().unwrap(),
            Err(WebMcpError::InvokeFailed { .. })
        ));
        assert!(cancels.is_empty());
        assert_eq!(
            canceled.try_recv().unwrap().unwrap().status,
            crate::BrowserWebMcpInvocationStatus::Failed
        );
        assert_eq!(state.tools().unwrap().len(), 1);
        assert_eq!(targets.sessions_for_root("other"), vec!["other-session"]);
    }

    #[test]
    fn oversized_tool_record_error_is_sticky() {
        let mut state = RuntimeState {
            active_target_id: "root".to_owned(),
            ..RuntimeState::default()
        };
        state.apply_tools_added(
            "root",
            "session",
            &json!({"tools": [{
                "name": "oversized",
                "description": "x".repeat(MAX_TOOL_RECORD_BYTES),
                "frameId": "main"
            }]}),
        );
        state.apply_tools_removed(
            "root",
            &json!({"tools": [{"name": "missing", "frameId": "main"}]}),
        );
        assert!(matches!(
            state.tools(),
            Err(WebMcpError::OutputTooLarge { .. })
        ));
    }

    #[test]
    fn early_completion_is_applied_when_invocation_arrives() {
        let mut state = RuntimeState::default();
        state.apply_response(&json!({
            "invocationId": "i1",
            "status": "Completed",
            "output": {"ok": true}
        }));
        state.insert_invocation(pending("i1", "main")).unwrap();
        let record = &state.invocations["i1"];
        assert_eq!(record.status, InvocationStatus::Completed);
        assert_eq!(record.output, Some(json!({"ok": true})));
    }

    #[test]
    fn early_completion_queue_is_bounded() {
        let mut state = RuntimeState::default();
        for index in 0..=MAX_EARLY_RESPONSES {
            state.apply_response(&json!({
                "invocationId": format!("i{index}"),
                "status": "Completed"
            }));
        }
        assert_eq!(state.early_responses.len(), MAX_EARLY_RESPONSES);
        assert!(!state.early_responses.contains_key("i0"));
    }

    #[test]
    fn invocation_history_evicts_only_terminal_records() {
        let mut state = RuntimeState::default();
        for index in 0..MAX_INVOCATION_HISTORY {
            let mut record = pending(&format!("i{index}"), "main");
            record.status = InvocationStatus::Completed;
            state.insert_invocation(record).unwrap();
        }
        state.insert_invocation(pending("latest", "main")).unwrap();
        assert_eq!(state.invocations.len(), MAX_INVOCATION_HISTORY);

        let mut active = RuntimeState::default();
        for index in 0..MAX_INVOCATION_HISTORY {
            active
                .insert_invocation(pending(&format!("active-{index}"), "main"))
                .unwrap();
        }
        assert!(matches!(
            active.ensure_invocation_capacity(),
            Err(WebMcpError::TooManyInvocations { .. })
        ));
    }

    #[test]
    fn navigation_invalidation_respects_page_and_frame_scope() {
        let mut state = RuntimeState::default();
        state.insert_invocation(pending("main", "main")).unwrap();
        state.insert_invocation(pending("child", "child")).unwrap();
        state.clear_frame_scope("root", "child");
        assert_eq!(state.invocations["main"].status, InvocationStatus::Pending);
        assert_eq!(state.invocations["child"].status, InvocationStatus::Failed);
        state.clear_page_scope("root");
        assert_eq!(state.invocations["main"].status, InvocationStatus::Failed);
    }

    #[test]
    fn raw_event_preserves_unknown_webmcp_payload_and_session() {
        let event: RawCdpEvent = serde_json::from_value(json!({
            "method": "WebMCP.toolResponded",
            "sessionId": "session-1",
            "params": {"invocationId": "i1", "futureField": true}
        }))
        .unwrap();
        assert_eq!(event.method, "WebMCP.toolResponded");
        assert_eq!(event.session_id(), Some("session-1"));
        assert_eq!(event.params["futureField"], true);
    }
}
