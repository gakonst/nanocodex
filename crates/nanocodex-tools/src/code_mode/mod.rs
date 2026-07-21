mod description;
mod embedded;
mod output;
mod spec;

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
    time::Duration,
};
use tracing::{Instrument, info_span};

use serde_json::value::RawValue;

use super::{ToolContext, ToolOutputBody, ToolOutputContent};
use crate::runtime::{OwnedToolContext, ToolRegistry};
use embedded::EmbeddedHost;
pub(crate) use spec::{exec_spec, wait_spec};

const INITIAL_YIELD: Duration = if cfg!(test) {
    Duration::from_secs(30)
} else {
    Duration::from_secs(10)
};
const DEFAULT_WAIT_YIELD: Duration = Duration::from_secs(10);
const NESTED_YIELD_GRACE: Duration = Duration::from_secs(5);
const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const EXEC_PRAGMA_PREFIX: &str = "// @exec:";
pub(crate) struct CodeModeRuntime {
    cells: Arc<Mutex<CellRegistry>>,
    stored: Arc<Mutex<HashMap<String, Value>>>,
    host: Arc<Mutex<SharedJsHost>>,
}

#[derive(Clone)]
pub(crate) struct CodeModeControl {
    cells: Arc<Mutex<CellRegistry>>,
    host: Arc<Mutex<SharedJsHost>>,
}

struct SharedJsHost {
    host: Option<EmbeddedHost>,
}

impl SharedJsHost {
    fn prewarmed() -> Self {
        let host = match spawn_host() {
            Ok(host) => Some(host),
            Err(error) => {
                tracing::warn!(
                    target: "nanocodex_tools",
                    %error,
                    "embedded QuickJS code mode prewarm failed; the first cell will retry"
                );
                None
            }
        };
        Self { host }
    }
}

fn spawn_host() -> Result<EmbeddedHost, String> {
    let started_at = Instant::now();
    let span = info_span!(
        target: "nanocodex_tools",
        "code_mode.host_spawn",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let result = span.in_scope(EmbeddedHost::spawn);
    span.record(
        "status",
        if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
    );
    span.record(
        "otel.status_code",
        if result.is_ok() { "OK" } else { "ERROR" },
    );
    span.record(
        "duration_ns",
        u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
    result
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

enum CellUpdate {
    NestedCallStarted {
        name: String,
        yield_after: Duration,
    },
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
        content: Vec<ToolOutputContent>,
    },
    Done {
        cell_id: u64,
        content: Vec<ToolOutputContent>,
        stored: HashMap<String, Value>,
    },
    Error {
        cell_id: u64,
        message: String,
        content: Vec<ToolOutputContent>,
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
    pub(super) fn new(_workspace: PathBuf) -> Self {
        Self {
            cells: Arc::new(Mutex::new(CellRegistry {
                next_cell_id: 1,
                live_cells: HashMap::new(),
            })),
            stored: Arc::new(Mutex::new(HashMap::new())),
            host: Arc::new(Mutex::new(SharedJsHost::prewarmed())),
        }
    }

    pub(super) fn control(&self) -> CodeModeControl {
        CodeModeControl {
            cells: Arc::clone(&self.cells),
            host: Arc::clone(&self.host),
        }
    }

    pub(super) async fn execute(
        &self,
        source: &str,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
    ) -> CodeModeExecution {
        let started_at = Instant::now();
        let span = info_span!(
            target: "nanocodex_tools",
            "code_mode.cell",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            cell.id = tracing::field::Empty,
            source.bytes = source.len(),
            source.lines = source.lines().count(),
            output.max_tokens = tracing::field::Empty,
            nested.count = tracing::field::Empty,
            running = tracing::field::Empty,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let execution = self
            .execute_inner(source, tools, context, started_at)
            .instrument(span.clone())
            .await;
        span.record(
            "status",
            if execution.success {
                "completed"
            } else {
                "failed"
            },
        );
        span.record(
            "otel.status_code",
            if execution.success { "OK" } else { "ERROR" },
        );
        span.record("nested.count", execution.nested_calls.len());
        span.record(
            "duration_ns",
            u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        execution
    }

    async fn execute_inner(
        &self,
        source: &str,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
        started_at: Instant,
    ) -> CodeModeExecution {
        let source = match parse_exec_source(source) {
            Ok(source) => source,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        let output_token_budget = source
            .max_output_tokens
            .unwrap_or(context.output_token_budget)
            .max(1);
        let extend_for_nested_calls = source.yield_time_ms.is_none();
        tracing::Span::current().record("output.max_tokens", output_token_budget);
        let context = context.with_output_token_budget(output_token_budget);
        let cell_id = self.cells.lock().await.allocate_cell_id();
        tracing::Span::current().record("cell.id", cell_id);
        let stored = self.stored.lock().await.clone();
        let mut cell = LiveCell::spawn(
            cell_id,
            source.code,
            tools,
            context,
            stored,
            Arc::clone(&self.stored),
            Arc::clone(&self.host),
            output_token_budget,
        );
        let yield_after = source
            .yield_time_ms
            .map_or(INITIAL_YIELD, Duration::from_millis);
        let (execution, running) = observe_cell(
            &mut cell,
            started_at,
            yield_after,
            Some(output_token_budget),
            extend_for_nested_calls,
        )
        .await;
        tracing::Span::current().record("running", running);
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
            false,
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

impl CodeModeControl {
    pub(super) async fn terminate_all(&self) {
        let cells = {
            let mut registry = self.cells.lock().await;
            std::mem::take(&mut registry.live_cells)
                .into_values()
                .collect::<Vec<_>>()
        };
        for mut cell in cells {
            cell.terminate().await;
        }

        let mut shared_host = self.host.lock().await;
        if let Some(mut host) = shared_host.host.take() {
            host.terminate().await;
        }
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
        u64::try_from(max_output_tokens).map_or(true, |max_output_tokens| {
            max_output_tokens > MAX_JS_SAFE_INTEGER
        })
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

impl LiveCell {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        id: u64,
        source: String,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
        stored: HashMap<String, Value>,
        shared_stored: Arc<Mutex<HashMap<String, Value>>>,
        host: Arc<Mutex<SharedJsHost>>,
        output_token_budget: usize,
    ) -> Self {
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let (terminate, terminate_rx) = oneshot::channel();
        let actor_span = info_span!(
            target: "nanocodex_tools",
            "code_mode.cell_actor",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            cell.id = id,
            runtime.first_event_ns = tracing::field::Empty,
            runtime.event_count = tracing::field::Empty,
            host.reused = tracing::field::Empty,
            host.wait_ns = tracing::field::Empty,
            host.termination_ns = tracing::field::Empty,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let task = tokio::spawn(
            run_cell_actor(
                host,
                id,
                source,
                tools,
                context,
                stored,
                shared_stored,
                updates_tx,
                terminate_rx,
            )
            .instrument(actor_span),
        );
        Self {
            id,
            output_token_budget,
            updates,
            terminate: Some(terminate),
            task: Some(task),
        }
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
            // The detached actor observes `terminate` and shuts down the shared
            // host. Aborting here could leave JavaScript running in an isolate
            // that a later cell is about to reuse.
            drop(task);
        }
    }
}

// Keep every lifecycle update in one exhaustive, order-preserving observation loop.
#[allow(clippy::too_many_lines)]
async fn observe_cell(
    cell: &mut LiveCell,
    started_at: Instant,
    yield_after: Duration,
    max_output_tokens: Option<usize>,
    extend_for_nested_calls: bool,
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
            Some(CellUpdate::NestedCallStarted {
                name,
                yield_after: nested_yield_after,
            }) => {
                maybe_extend_cell_yield(
                    yield_timer.as_mut(),
                    extend_for_nested_calls,
                    yield_after,
                    nested_yield_after,
                    &name,
                );
            }
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

fn maybe_extend_cell_yield(
    mut timer: std::pin::Pin<&mut tokio::time::Sleep>,
    enabled: bool,
    initial_yield_after: Duration,
    nested_yield_after: Duration,
    tool_name: &str,
) {
    if !enabled || nested_yield_after <= initial_yield_after {
        return;
    }
    let Some(extended_deadline) = tokio::time::Instant::now()
        .checked_add(nested_yield_after)
        .and_then(|deadline| deadline.checked_add(NESTED_YIELD_GRACE))
    else {
        return;
    };
    if extended_deadline <= timer.deadline() {
        return;
    }
    timer.as_mut().reset(extended_deadline);
    tracing::info!(
        target: "nanocodex_tools",
        stage = "code_mode.yield_extended",
        tool.name = tool_name,
        nested.yield_ms = nested_yield_after.as_millis(),
        "extended Code Mode yield for nested tool wait"
    );
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
    mut content: Vec<ToolOutputContent>,
    max_output_tokens: Option<usize>,
    nested_calls: Vec<(u64, NestedToolCall)>,
    notifications: Vec<CodeModeNotification>,
) -> CodeModeExecution {
    expose_running_shell_sessions(&mut content, &nested_calls);
    let content = output::truncate_content(content, max_output_tokens);
    CodeModeExecution {
        output: with_status(status, started_at.elapsed().as_secs_f64(), content),
        success,
        nested_calls: ordered_calls(nested_calls),
        notifications,
    }
}

fn expose_running_shell_sessions(
    content: &mut Vec<ToolOutputContent>,
    nested_calls: &[(u64, NestedToolCall)],
) {
    for (_, call) in nested_calls {
        if !matches!(call.name.as_str(), "exec_command" | "write_stdin") {
            continue;
        }
        let ToolOutputBody::Text(result) = &call.output else {
            continue;
        };
        let Some(session_id) = serde_json::from_str::<Value>(result)
            .ok()
            .and_then(|result| result.get("session_id")?.as_i64())
        else {
            continue;
        };
        if content
            .iter()
            .filter_map(|item| match item {
                ToolOutputContent::InputText { text } => Some(text),
                ToolOutputContent::InputImage { .. } => None,
            })
            .any(|text| text_exposes_session_id(text, session_id))
        {
            continue;
        }
        content.push(ToolOutputContent::InputText {
            text: format!(
                "Nested shell process is still running with session ID {session_id}. Resume it with tools.write_stdin({{ session_id: {session_id}, chars: \"\" }})."
            ),
        });
    }
}

fn text_exposes_session_id(text: &str, session_id: i64) -> bool {
    serde_json::from_str::<Value>(text).is_ok_and(|value| {
        value.as_i64() == Some(session_id)
            || value.get("session_id").and_then(Value::as_i64) == Some(session_id)
    })
}

impl EmbeddedHost {
    async fn drive_cell(
        &mut self,
        cell_id: u64,
        parent_call_id: &str,
        tools: &ToolRegistry,
        context: &OwnedToolContext,
        updates: &mpsc::UnboundedSender<CellUpdate>,
        actor_started_at: Instant,
    ) -> Result<CellTerminal, HostFailure> {
        let mut pending_calls: FuturesUnordered<BoxFuture<'_, CompletedNestedCall>> =
            FuturesUnordered::new();
        let mut event_count = 0_u64;
        loop {
            tokio::select! {
                completed = pending_calls.next(), if !pending_calls.is_empty() => {
                    let Some(completed) = completed else {
                        continue;
                    };
                    let id = completed.id;
                    let call = self.send_completed_call(cell_id, completed)?;
                    let _ = updates.send(CellUpdate::NestedCall { id, call });
                }
                event = self.read_event() => {
                    let event = event.map_err(HostFailure::new)?;
                    event_count = event_count.saturating_add(1);
                    if event_count == 1 {
                        tracing::Span::current().record(
                            "runtime.first_event_ns",
                            u64::try_from(actor_started_at.elapsed().as_nanos())
                                .unwrap_or(u64::MAX),
                        );
                    }
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
                            if let Some(yield_after) = nested_tool_yield_after(&name, &input) {
                                let _ = updates.send(CellUpdate::NestedCallStarted {
                                    name: name.clone(),
                                    yield_after,
                                });
                            }
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
                            tracing::Span::current().record("runtime.event_count", event_count);
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
                            tracing::Span::current().record("runtime.event_count", event_count);
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

    fn send_completed_call(
        &mut self,
        cell_id: u64,
        completed: CompletedNestedCall,
    ) -> Result<NestedToolCall, HostFailure> {
        self.send_tool_result(
            cell_id,
            completed.id,
            completed.value,
            completed.call.success,
        )
        .map_err(HostFailure::new)?;
        Ok(completed.call)
    }
}

fn nested_tool_yield_after(name: &str, input: &Value) -> Option<Duration> {
    let input = input.as_object()?;
    let requested = input.get("yield_time_ms")?.as_u64()?;
    let (minimum, maximum) = match name {
        "write_stdin"
            if input
                .get("chars")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .is_empty() =>
        {
            (5_000, 300_000)
        }
        "exec_command" | "write_stdin" => (250, 30_000),
        _ => return None,
    };
    Some(Duration::from_millis(requested.clamp(minimum, maximum)))
}

#[allow(clippy::too_many_arguments)]
async fn run_cell_actor(
    shared_host: Arc<Mutex<SharedJsHost>>,
    cell_id: u64,
    source: String,
    tools: Arc<ToolRegistry>,
    context: OwnedToolContext,
    stored: HashMap<String, Value>,
    shared_stored: Arc<Mutex<HashMap<String, Value>>>,
    updates: mpsc::UnboundedSender<CellUpdate>,
    mut terminate: oneshot::Receiver<()>,
) {
    let started_at = Instant::now();
    let host_wait_started_at = Instant::now();
    let mut shared_host = shared_host.lock().await;
    tracing::Span::current().record(
        "host.wait_ns",
        u64::try_from(host_wait_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
    let reused = shared_host.host.is_some();
    tracing::Span::current().record("host.reused", reused);
    let mut host = match shared_host.host.take() {
        Some(host) => host,
        None => match spawn_host() {
            Ok(host) => host,
            Err(message) => {
                tracing::Span::current().record("status", "failed");
                tracing::Span::current().record("otel.status_code", "ERROR");
                tracing::Span::current().record(
                    "duration_ns",
                    u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                );
                let _ = updates.send(CellUpdate::HostFailed(message));
                return;
            }
        },
    };
    let run = async {
        host.start_cell(cell_id, &source, stored, tools.nested_tool_metadata())
            .map_err(HostFailure::new)?;
        host.drive_cell(
            cell_id,
            &context.call_id,
            tools.as_ref(),
            &context,
            &updates,
            started_at,
        )
        .await
    };
    let terminal = tokio::select! {
        biased;
        _ = &mut terminate => {
            let termination_started_at = Instant::now();
            host.terminate().await;
            tracing::Span::current().record(
                "host.termination_ns",
                u64::try_from(termination_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            );
            tracing::Span::current().record("status", "cancelled");
            tracing::Span::current().record("otel.status_code", "ERROR");
            tracing::Span::current().record(
                "duration_ns",
                u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            );
            return;
        }
        terminal = run => terminal,
    };
    let success = matches!(terminal, Ok(CellTerminal::Completed { .. }));
    tracing::Span::current().record("status", if success { "completed" } else { "failed" });
    tracing::Span::current().record("otel.status_code", if success { "OK" } else { "ERROR" });
    let host_healthy = terminal.is_ok();
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
    if host_healthy {
        shared_host.host = Some(host);
    } else {
        let termination_started_at = Instant::now();
        host.terminate().await;
        tracing::Span::current().record(
            "host.termination_ns",
            u64::try_from(termination_started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
    }
    tracing::Span::current().record(
        "duration_ns",
        u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
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
    let call_id = format!("{}/code-{id}", context.call_id);
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
