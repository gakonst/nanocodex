mod description;
mod output;
mod protocol;
mod spec;

use std::{collections::HashMap, process::Stdio, sync::Arc, time::Instant};

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
    time::Duration,
};

use nanocodex_core::ResponseItem;
use serde_json::value::RawValue;

use super::{ToolContext, ToolOutputBody, ToolOutputContent};
use crate::runtime::ToolRegistry;
use protocol::{read_protocol_line, write_json_line};
pub(crate) use spec::{exec_spec, wait_spec};

const RUNTIME: &str = include_str!("runtime.js");
const INITIAL_YIELD: Duration = if cfg!(test) {
    Duration::from_secs(30)
} else {
    Duration::from_secs(10)
};
const DEFAULT_WAIT_YIELD: Duration = Duration::from_secs(10);
const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const EXEC_PRAGMA_PREFIX: &str = "// @exec:";
pub(crate) struct CodeModeRuntime {
    cells: Mutex<CellRegistry>,
    stored: Arc<Mutex<HashMap<String, Value>>>,
}

struct CellRegistry {
    next_cell_id: u64,
    live_cells: HashMap<u64, LiveCell>,
}

struct LiveCell {
    id: u64,
    output_token_budget: usize,
    updates: mpsc::UnboundedReceiver<CellUpdate>,
    terminate: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

struct OwnedToolContext {
    model: String,
    session_id: String,
    call_id: String,
    history: Vec<ResponseItem>,
    output_token_budget: usize,
}

enum CellUpdate {
    NestedCall {
        id: u64,
        call: NestedToolCall,
    },
    Notification(CodeModeNotification),
    Yielded {
        content: Vec<ToolOutputContent>,
    },
    Completed {
        content: Vec<ToolOutputContent>,
    },
    ScriptFailed {
        message: String,
        content: Vec<ToolOutputContent>,
    },
    HostFailed(String),
}

struct NodeHost {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct CodeModeExecution {
    pub output: ToolOutputBody,
    pub success: bool,
    pub nested_calls: Vec<NestedToolCall>,
    pub notifications: Vec<CodeModeNotification>,
}

pub struct CodeModeNotification {
    pub call_id: String,
    pub text: String,
}

pub struct NestedToolCall {
    pub call_id: String,
    pub name: String,
    pub input: Value,
    pub output: ToolOutputBody,
    pub success: bool,
    pub duration_ns: u64,
    pub metadata: Option<Box<RawValue>>,
}

#[derive(Serialize)]
struct ExecuteMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    cell_id: u64,
    source: &'a str,
    tools: Vec<Value>,
    stored: HashMap<String, Value>,
}

#[derive(Serialize)]
struct ToolResultMessage {
    #[serde(rename = "type")]
    kind: &'static str,
    cell_id: u64,
    id: u64,
    value: Value,
    success: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RuntimeEvent {
    ToolCall {
        cell_id: u64,
        id: u64,
        name: String,
        input: Value,
    },
    Notify {
        cell_id: u64,
        text: String,
    },
    Yielded {
        cell_id: u64,
        #[serde(default)]
        content: Vec<ToolOutputContent>,
    },
    Done {
        cell_id: u64,
        #[serde(default)]
        content: Vec<ToolOutputContent>,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
    Error {
        cell_id: u64,
        message: String,
        #[serde(default)]
        content: Vec<ToolOutputContent>,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
}

impl RuntimeEvent {
    fn cell_id(&self) -> u64 {
        match self {
            Self::ToolCall { cell_id, .. }
            | Self::Notify { cell_id, .. }
            | Self::Yielded { cell_id, .. }
            | Self::Done { cell_id, .. }
            | Self::Error { cell_id, .. } => *cell_id,
        }
    }
}

struct CompletedNestedCall {
    id: u64,
    value: Value,
    call: NestedToolCall,
}

enum CellTerminal {
    Completed {
        content: Vec<ToolOutputContent>,
        stored: HashMap<String, Value>,
    },
    ScriptFailed {
        message: String,
        content: Vec<ToolOutputContent>,
        stored: HashMap<String, Value>,
    },
}

struct HostFailure {
    message: String,
}

impl CodeModeRuntime {
    pub(super) fn new() -> Self {
        Self {
            cells: Mutex::new(CellRegistry {
                next_cell_id: 1,
                live_cells: HashMap::new(),
            }),
            stored: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(super) async fn execute(
        &self,
        source: &str,
        tools: Arc<ToolRegistry>,
        context: ToolContext<'_>,
    ) -> CodeModeExecution {
        let started_at = Instant::now();
        let source = match parse_exec_source(source) {
            Ok(source) => source,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        let output_token_budget = source
            .max_output_tokens
            .unwrap_or(context.output_token_budget)
            .max(1);
        let context = ToolContext {
            output_token_budget,
            ..context
        };
        let cell_id = self.cells.lock().await.allocate_cell_id();
        let stored = self.stored.lock().await.clone();
        let mut cell = match LiveCell::spawn(
            cell_id,
            source.code,
            tools,
            OwnedToolContext::from(context),
            stored,
            Arc::clone(&self.stored),
            output_token_budget,
        ) {
            Ok(cell) => cell,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        let yield_after = source
            .yield_time_ms
            .map_or(INITIAL_YIELD, Duration::from_millis);
        let (execution, running) = observe_cell(
            &mut cell,
            started_at,
            yield_after,
            Some(output_token_budget),
        )
        .await;
        if running {
            self.cells.lock().await.live_cells.insert(cell_id, cell);
        } else {
            cell.join().await;
        }
        execution
    }

    pub(super) async fn wait(&self, input: &str, _context: ToolContext<'_>) -> CodeModeExecution {
        let started_at = Instant::now();
        let arguments = match serde_json::from_str::<WaitArguments>(input) {
            Ok(arguments) => arguments,
            Err(error) => {
                return failed_execution(
                    started_at,
                    &format!("failed to parse wait arguments: {error}"),
                    Vec::new(),
                );
            }
        };
        let cell_id = match arguments.cell_id.parse::<u64>() {
            Ok(cell_id) => cell_id,
            Err(error) => {
                return failed_execution(
                    started_at,
                    &format!("invalid exec cell ID `{}`: {error}", arguments.cell_id),
                    Vec::new(),
                );
            }
        };
        let Some(mut live_cell) = self.cells.lock().await.live_cells.remove(&cell_id) else {
            return failed_execution(
                started_at,
                &format!("exec cell {cell_id} was not found"),
                Vec::new(),
            );
        };
        let continued_output_token_budget = live_cell.output_token_budget;
        if arguments.terminate {
            live_cell.terminate().await;
            return CodeModeExecution {
                output: ToolOutputBody::Text(format!(
                    "Script terminated\nWall time {:.1} seconds\nOutput:\nexec cell {cell_id} was terminated",
                    started_at.elapsed().as_secs_f64()
                )),
                success: true,
                nested_calls: Vec::new(),
                notifications: Vec::new(),
            };
        }
        let yield_time = Duration::from_millis(
            arguments
                .yield_time_ms
                .unwrap_or(u64::try_from(DEFAULT_WAIT_YIELD.as_millis()).unwrap_or(u64::MAX)),
        );
        let output_token_budget = arguments
            .max_tokens
            .unwrap_or(continued_output_token_budget)
            .max(1);
        let (execution, running) = observe_cell(
            &mut live_cell,
            started_at,
            yield_time,
            Some(output_token_budget),
        )
        .await;
        if running {
            live_cell.output_token_budget = continued_output_token_budget;
            self.cells
                .lock()
                .await
                .live_cells
                .insert(cell_id, live_cell);
        } else {
            live_cell.join().await;
        }
        execution
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecPragma {
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

struct ParsedExecSource {
    code: String,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}

fn parse_exec_source(input: &str) -> Result<ParsedExecSource, String> {
    if input.trim().is_empty() {
        return Err(
            "exec expects raw JavaScript source text (non-empty). Provide JS only, optionally with first-line `// @exec: {\"yield_time_ms\": 10000, \"max_output_tokens\": 1000}`."
                .to_owned(),
        );
    }
    let mut source = ParsedExecSource {
        code: input.to_owned(),
        yield_time_ms: None,
        max_output_tokens: None,
    };
    let mut lines = input.splitn(2, '\n');
    let first_line = lines.next().unwrap_or_default();
    let rest = lines.next().unwrap_or_default();
    let Some(pragma) = first_line.trim_start().strip_prefix(EXEC_PRAGMA_PREFIX) else {
        return Ok(source);
    };
    if rest.trim().is_empty() {
        return Err(
            "exec pragma must be followed by JavaScript source on subsequent lines".to_owned(),
        );
    }
    let directive = pragma.trim();
    if directive.is_empty() {
        return Err(
            "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
                .to_owned(),
        );
    }
    let value: Value = serde_json::from_str(directive).map_err(|error| {
        format!(
            "exec pragma must be valid JSON with supported fields `yield_time_ms` and `max_output_tokens`: {error}"
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
            .to_owned()
    })?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "yield_time_ms" | "max_output_tokens"))
    {
        return Err(format!(
            "exec pragma only supports `yield_time_ms` and `max_output_tokens`; got `{key}`"
        ));
    }
    let pragma: ExecPragma = serde_json::from_value(value).map_err(|error| {
        format!(
            "exec pragma fields `yield_time_ms` and `max_output_tokens` must be non-negative safe integers: {error}"
        )
    })?;
    if pragma
        .yield_time_ms
        .is_some_and(|yield_time_ms| yield_time_ms > MAX_JS_SAFE_INTEGER)
    {
        return Err(
            "exec pragma field `yield_time_ms` must be a non-negative safe integer".to_owned(),
        );
    }
    if pragma.max_output_tokens.is_some_and(|max_output_tokens| {
        u64::try_from(max_output_tokens)
            .map(|max_output_tokens| max_output_tokens > MAX_JS_SAFE_INTEGER)
            .unwrap_or(true)
    }) {
        return Err(
            "exec pragma field `max_output_tokens` must be a non-negative safe integer".to_owned(),
        );
    }
    rest.clone_into(&mut source.code);
    source.yield_time_ms = pragma.yield_time_ms;
    source.max_output_tokens = pragma.max_output_tokens;
    Ok(source)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArguments {
    cell_id: String,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_tokens: Option<usize>,
    #[serde(default)]
    terminate: bool,
}

impl CellRegistry {
    fn allocate_cell_id(&mut self) -> u64 {
        let cell_id = self.next_cell_id;
        self.next_cell_id = self.next_cell_id.saturating_add(1);
        cell_id
    }
}

impl OwnedToolContext {
    fn from(context: ToolContext<'_>) -> Self {
        Self {
            model: context.model.to_owned(),
            session_id: context.session_id.to_owned(),
            call_id: context.call_id.to_owned(),
            history: context.history.to_vec(),
            output_token_budget: context.output_token_budget,
        }
    }

    fn borrowed(&self) -> ToolContext<'_> {
        ToolContext {
            model: &self.model,
            session_id: &self.session_id,
            call_id: &self.call_id,
            history: &self.history,
            output_token_budget: self.output_token_budget,
        }
    }
}

impl LiveCell {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        id: u64,
        source: String,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
        stored: HashMap<String, Value>,
        shared_stored: Arc<Mutex<HashMap<String, Value>>>,
        output_token_budget: usize,
    ) -> Result<Self, String> {
        let host = NodeHost::spawn(&tools.workspace)?;
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let (terminate, terminate_rx) = oneshot::channel();
        let task = tokio::spawn(run_cell_actor(
            host,
            id,
            source,
            tools,
            context,
            stored,
            shared_stored,
            updates_tx,
            terminate_rx,
        ));
        Ok(Self {
            id,
            output_token_budget,
            updates,
            terminate: Some(terminate),
            task: Some(task),
        })
    }

    async fn terminate(&mut self) {
        if let Some(terminate) = self.terminate.take() {
            let _ = terminate.send(());
        }
        self.join().await;
    }

    async fn join(&mut self) {
        self.terminate = None;
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for LiveCell {
    fn drop(&mut self) {
        if let Some(terminate) = self.terminate.take() {
            let _ = terminate.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn observe_cell(
    cell: &mut LiveCell,
    started_at: Instant,
    yield_after: Duration,
    max_output_tokens: Option<usize>,
) -> (CodeModeExecution, bool) {
    let mut nested_calls = Vec::new();
    let mut notifications = Vec::new();
    let yield_timer = tokio::time::sleep(yield_after);
    tokio::pin!(yield_timer);
    loop {
        let update = tokio::select! {
            () = &mut yield_timer => {
                return running_observation(
                    cell.id,
                    started_at,
                    Vec::new(),
                    max_output_tokens,
                    nested_calls,
                    notifications,
                );
            }
            update = cell.updates.recv() => update,
        };
        match update {
            Some(CellUpdate::NestedCall { id, call }) => nested_calls.push((id, call)),
            Some(CellUpdate::Notification(notification)) => notifications.push(notification),
            Some(CellUpdate::Yielded { content }) => {
                return running_observation(
                    cell.id,
                    started_at,
                    content,
                    max_output_tokens,
                    nested_calls,
                    notifications,
                );
            }
            Some(CellUpdate::Completed { content }) => {
                return (
                    observed_execution(
                        "Script completed",
                        true,
                        started_at,
                        content,
                        max_output_tokens,
                        nested_calls,
                        notifications,
                    ),
                    false,
                );
            }
            Some(CellUpdate::ScriptFailed {
                message,
                mut content,
            }) => {
                content.push(ToolOutputContent::InputText {
                    text: format!("Script error:\n{message}"),
                });
                return (
                    observed_execution(
                        "Script failed",
                        false,
                        started_at,
                        content,
                        max_output_tokens,
                        nested_calls,
                        notifications,
                    ),
                    false,
                );
            }
            Some(CellUpdate::HostFailed(message)) => {
                return (
                    observed_execution(
                        "Script failed",
                        false,
                        started_at,
                        vec![ToolOutputContent::InputText { text: message }],
                        max_output_tokens,
                        nested_calls,
                        notifications,
                    ),
                    false,
                );
            }
            None => {
                return (
                    observed_execution(
                        "Script failed",
                        false,
                        started_at,
                        vec![ToolOutputContent::InputText {
                            text: "local code-mode cell ended before a result".to_owned(),
                        }],
                        max_output_tokens,
                        nested_calls,
                        notifications,
                    ),
                    false,
                );
            }
        }
    }
}

fn running_observation(
    cell_id: u64,
    started_at: Instant,
    content: Vec<ToolOutputContent>,
    max_output_tokens: Option<usize>,
    nested_calls: Vec<(u64, NestedToolCall)>,
    notifications: Vec<CodeModeNotification>,
) -> (CodeModeExecution, bool) {
    (
        observed_execution(
            &format!("Script running with cell ID {cell_id}"),
            true,
            started_at,
            content,
            max_output_tokens,
            nested_calls,
            notifications,
        ),
        true,
    )
}

fn observed_execution(
    status: &str,
    success: bool,
    started_at: Instant,
    content: Vec<ToolOutputContent>,
    max_output_tokens: Option<usize>,
    nested_calls: Vec<(u64, NestedToolCall)>,
    notifications: Vec<CodeModeNotification>,
) -> CodeModeExecution {
    let content = output::truncate_content(content, max_output_tokens);
    CodeModeExecution {
        output: with_status(status, started_at.elapsed().as_secs_f64(), content),
        success,
        nested_calls: ordered_calls(nested_calls),
        notifications,
    }
}

impl NodeHost {
    fn spawn(workspace: &std::path::Path) -> Result<Self, String> {
        let mut child = Command::new("node")
            .args(["--input-type=module", "--eval", RUNTIME])
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("failed to start local Node.js code-mode host: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Node code-mode host stdin was unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Node code-mode host stdout was unavailable".to_owned())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    async fn start_cell(
        &mut self,
        cell_id: u64,
        source: &str,
        stored: HashMap<String, Value>,
        tools: &ToolRegistry,
    ) -> Result<(), HostFailure> {
        let request = ExecuteMessage {
            kind: "execute",
            cell_id,
            source,
            tools: tools.nested_tool_metadata(),
            stored,
        };
        write_json_line(&mut self.stdin, &request)
            .await
            .map_err(|error| {
                HostFailure::new(format!(
                    "failed to initialize local code-mode cell: {error}"
                ))
            })?;
        Ok(())
    }

    async fn drive_cell(
        &mut self,
        cell_id: u64,
        parent_call_id: &str,
        tools: &ToolRegistry,
        context: &OwnedToolContext,
        updates: &mpsc::UnboundedSender<CellUpdate>,
    ) -> Result<CellTerminal, HostFailure> {
        let mut pending_calls: FuturesUnordered<BoxFuture<'_, CompletedNestedCall>> =
            FuturesUnordered::new();
        loop {
            tokio::select! {
                completed = pending_calls.next(), if !pending_calls.is_empty() => {
                    let Some(completed) = completed else {
                        continue;
                    };
                    let id = completed.id;
                    let call = self.send_completed_call(cell_id, completed).await?;
                    let _ = updates.send(CellUpdate::NestedCall { id, call });
                }
                event = self.read_event() => {
                    let event = event?;
                    let event_cell_id = event.cell_id();
                    if event_cell_id != cell_id {
                        return Err(HostFailure::new(format!(
                            "local code-mode host returned cell {event_cell_id} while executing cell {cell_id}"
                        )));
                    }
                    match event {
                        RuntimeEvent::ToolCall {
                            id, name, input, ..
                        } => {
                            pending_calls
                                .push(execute_nested_call(tools, id, name, input, context).boxed());
                        }
                        RuntimeEvent::Notify { text, .. } => {
                            let _ = updates.send(CellUpdate::Notification(
                                CodeModeNotification::new(parent_call_id, text),
                            ));
                        }
                        RuntimeEvent::Yielded {
                            content,
                            ..
                        } => {
                            let _ = updates.send(CellUpdate::Yielded { content });
                        }
                        RuntimeEvent::Done {
                            content,
                            stored,
                            ..
                        } => {
                            return Ok(CellTerminal::Completed {
                                content,
                                stored,
                            });
                        }
                        RuntimeEvent::Error {
                            message,
                            content,
                            stored,
                            ..
                        } => {
                            return Ok(CellTerminal::ScriptFailed {
                                message,
                                content,
                                stored,
                            });
                        }
                    }
                }
            }
        }
    }

    async fn read_event(&mut self) -> Result<RuntimeEvent, HostFailure> {
        let line = match read_protocol_line(&mut self.stdout).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                let status = self.child.wait().await;
                return Err(HostFailure::new(format!(
                    "local code-mode host ended before a result: {status:?}"
                )));
            }
            Err(error) => {
                return Err(HostFailure::new(format!(
                    "failed to read local code-mode host: {error}"
                )));
            }
        };
        serde_json::from_slice::<RuntimeEvent>(&line).map_err(|error| {
            HostFailure::new(format!(
                "local code-mode host emitted invalid JSON: {error}"
            ))
        })
    }

    async fn send_completed_call(
        &mut self,
        cell_id: u64,
        completed: CompletedNestedCall,
    ) -> Result<NestedToolCall, HostFailure> {
        let response = ToolResultMessage {
            kind: "tool_result",
            cell_id,
            id: completed.id,
            value: completed.value,
            success: completed.call.success,
        };
        write_json_line(&mut self.stdin, &response)
            .await
            .map_err(|error| {
                HostFailure::new(format!("failed to return a nested tool result: {error}"))
            })?;
        Ok(completed.call)
    }

    async fn terminate(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_cell_actor(
    mut host: NodeHost,
    cell_id: u64,
    source: String,
    tools: Arc<ToolRegistry>,
    context: OwnedToolContext,
    stored: HashMap<String, Value>,
    shared_stored: Arc<Mutex<HashMap<String, Value>>>,
    updates: mpsc::UnboundedSender<CellUpdate>,
    mut terminate: oneshot::Receiver<()>,
) {
    let run = async {
        host.start_cell(cell_id, &source, stored, tools.as_ref())
            .await?;
        host.drive_cell(
            cell_id,
            &context.call_id,
            tools.as_ref(),
            &context,
            &updates,
        )
        .await
    };
    let terminal = tokio::select! {
        biased;
        _ = &mut terminate => {
            host.terminate().await;
            return;
        }
        terminal = run => terminal,
    };
    match terminal {
        Ok(CellTerminal::Completed { content, stored }) => {
            shared_stored.lock().await.extend(stored);
            let _ = updates.send(CellUpdate::Completed { content });
        }
        Ok(CellTerminal::ScriptFailed {
            message,
            content,
            stored,
        }) => {
            shared_stored.lock().await.extend(stored);
            let _ = updates.send(CellUpdate::ScriptFailed { message, content });
        }
        Err(failure) => {
            let _ = updates.send(CellUpdate::HostFailed(failure.message));
        }
    }
    host.terminate().await;
}

impl CodeModeNotification {
    fn new(call_id: &str, text: String) -> Self {
        Self {
            call_id: call_id.to_owned(),
            text,
        }
    }
}

impl HostFailure {
    fn new(message: String) -> Self {
        Self { message }
    }
}

fn ordered_calls(mut calls: Vec<(u64, NestedToolCall)>) -> Vec<NestedToolCall> {
    calls.sort_unstable_by_key(|(id, _)| *id);
    calls.into_iter().map(|(_, call)| call).collect()
}

async fn execute_nested_call(
    tools: &ToolRegistry,
    id: u64,
    name: String,
    input: Value,
    context: &OwnedToolContext,
) -> CompletedNestedCall {
    let started_at = Instant::now();
    let call_id = format!("code-{id}");
    let context = context.borrowed();
    let execution = tools
        .execute_nested(
            &name,
            input.clone(),
            ToolContext {
                call_id: &call_id,
                ..context
            },
        )
        .await;
    let duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let value = execution.value();
    CompletedNestedCall {
        id,
        value,
        call: NestedToolCall {
            call_id,
            name,
            input,
            output: execution.output,
            success: execution.success,
            duration_ns,
            metadata: execution.metadata,
        },
    }
}

fn failed_execution(
    started_at: Instant,
    message: &str,
    nested_calls: Vec<NestedToolCall>,
) -> CodeModeExecution {
    let wall_time = started_at.elapsed().as_secs_f64();
    CodeModeExecution {
        output: ToolOutputBody::Text(format!(
            "Script failed\nWall time {wall_time:.1} seconds\nOutput:\n{message}"
        )),
        success: false,
        nested_calls,
        notifications: Vec::new(),
    }
}

fn with_status(
    status: &str,
    wall_time: f64,
    mut content: Vec<ToolOutputContent>,
) -> ToolOutputBody {
    let header = format!("{status}\nWall time {wall_time:.1} seconds\nOutput:\n");
    if content.is_empty() {
        return ToolOutputBody::Text(header);
    }
    content.insert(0, ToolOutputContent::InputText { text: header });
    ToolOutputBody::Content(content)
}

#[cfg(test)]
mod tests;
