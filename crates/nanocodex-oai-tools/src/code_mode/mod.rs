//! Code Mode execution results, notifications, and nested-tool observation.

mod audio;
mod journal;
pub use journal::{CodeJournalAdmission, CodeModeJournal};
mod embedded;
mod output;
use crate::code_mode_spec as spec;

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Instant,
};

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::Deserialize;
use serde_json::Value;
#[cfg(test)]
use tokio::sync::Semaphore;
use tokio::{
    sync::{Mutex, OwnedMutexGuard, mpsc, oneshot, watch},
    task::JoinHandle,
    time::Duration,
};
use tracing::{Instrument, info_span};

use super::{ToolContext, ToolOutputBody, ToolOutputContent};
pub use crate::embedded::{
    CodeModeCell, CodeModeExecution, CodeModeNotification, CodeModeObserver, CodeModeUpdate,
    NestedToolCall,
};
use crate::runtime::{OwnedToolContext, ToolRegistry};
use embedded::EmbeddedHost;
pub(crate) use spec::{exec_spec, wait_spec};

const INITIAL_YIELD: Duration = Duration::from_secs(10);
const DEFAULT_WAIT_YIELD: Duration = Duration::from_secs(10);
const OBSERVER_YIELD_GRACE: Duration = Duration::from_secs(1);
const MIN_YIELD_FOR_OBSERVER_GRACE: Duration = Duration::from_secs(10);
const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const EXEC_PRAGMA_PREFIX: &str = "// @exec:";
const CELL_RUNNING: u8 = 0;
const CELL_TERMINATING: u8 = 1;
const CELL_COMPLETION_CLAIMED: u8 = 2;
const CELL_CLOSED: u8 = 3;

pub(crate) struct CodeModeRuntime {
    admission: Arc<Mutex<()>>,
    admission_epoch: Arc<AtomicU64>,
    #[cfg(test)]
    admission_attempts: Arc<Semaphore>,
    cells: Arc<Mutex<CellRegistry>>,
    stored: Arc<Mutex<HashMap<String, Value>>>,
    journal: Option<Arc<dyn CodeModeJournal>>,
    host: Arc<Mutex<SharedJsHost>>,
    current_turn: Arc<AtomicU64>,
    preempt: watch::Sender<(u64, u64)>,
}

#[derive(Clone)]
pub(crate) struct CodeModeControl {
    current_turn: Arc<AtomicU64>,
    preempt: watch::Sender<(u64, u64)>,
    admission: Arc<Mutex<()>>,
    admission_epoch: Arc<AtomicU64>,
    #[cfg(test)]
    admission_attempts: Arc<Semaphore>,
    cells: Arc<Mutex<CellRegistry>>,
    host: Arc<Mutex<SharedJsHost>>,
}

pub(super) struct CodeModeQuiescence {
    _admission: OwnedMutexGuard<()>,
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
                    target: "nanocodex_oai_tools",
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
        target: "nanocodex_oai_tools",
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
    live_cells: HashMap<u64, Arc<LiveCell>>,
}

struct LiveCell {
    id: u64,
    origin_call_id: String,
    turn_id: AtomicU64,
    observation: Arc<Mutex<CellObservationState>>,
    lifecycle: Arc<CellLifecycle>,
    terminate: StdMutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

// The session owns this state for the cell's full lifetime. An observation
// holds the mutex as an exclusive lease, so dropping its future releases the
// lease while preserving both unread updates and already-consumed output.
struct CellObservationState {
    updates: mpsc::UnboundedReceiver<CellUpdate>,
    buffered: ObservationBuffer,
}

#[derive(Default)]
struct ObservationBuffer {
    content: Vec<ToolOutputContent>,
    nested_calls: Vec<ObservedNestedCall>,
    notifications: Vec<CodeModeNotification>,
}

// One compare-exchange decides whether termination or completion owns the
// terminal transition. Stored values are committed only after completion wins.
struct CellLifecycle {
    phase: AtomicU8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CellError {
    Busy,
}

enum CellUpdate {
    NestedCallStarted {
        call_id: String,
        name: String,
        input: Value,
    },
    NestedCall(ObservedNestedCall),
    Notification(CodeModeNotification),
    Content(ToolOutputContent),
    Yielded,
    Completed,
    Terminated,
    ScriptFailed {
        message: String,
    },
    HostFailed(String),
}

struct IgnoreCodeModeUpdates;

impl CodeModeObserver for IgnoreCodeModeUpdates {
    fn update(&mut self, _update: CodeModeUpdate<'_>) {}
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
    Content {
        cell_id: u64,
        content: ToolOutputContent,
    },
    Yielded {
        cell_id: u64,
    },
    Done {
        cell_id: u64,
        stored: HashMap<String, Value>,
    },
    Error {
        cell_id: u64,
        message: String,
        stored: HashMap<String, Value>,
    },
}

impl RuntimeEvent {
    const fn cell_id(&self) -> u64 {
        match self {
            Self::ToolCall { cell_id, .. }
            | Self::Notify { cell_id, .. }
            | Self::Content { cell_id, .. }
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
    shell_session_id: Option<i64>,
}

struct ObservedNestedCall {
    id: u64,
    call: NestedToolCall,
    shell_session_id: Option<i64>,
}

// Every observed start gets one terminal receipt, including root completion or
// cancellation while its future is still pending. Dropping work cannot establish
// whether an external effect happened, so never present interruption as rollback.
#[derive(Default)]
struct JournalOutput {
    content: Vec<ToolOutputContent>,
    calls: std::collections::BTreeMap<u64, Value>,
    notifications: Vec<Value>,
}

fn nested_receipt(call: &NestedToolCall) -> Value {
    serde_json::json!({
        "call_id": call.call_id, "name": call.name, "input": call.input,
        "output": call.output, "structured_result": call.structured_result,
        "success": call.success, "started_after_ns": call.started_after_ns,
        "duration_ns": call.duration_ns, "metadata": call.metadata,
    })
}

fn record_nested(output: &StdMutex<JournalOutput>, id: u64, call: &NestedToolCall) {
    output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .calls
        .insert(id, nested_receipt(call));
}

struct PendingCallReceipts {
    captured: Arc<StdMutex<JournalOutput>>,
    calls: HashMap<u64, (NestedToolCall, Instant)>,
    updates: mpsc::UnboundedSender<CellUpdate>,
}

impl Drop for PendingCallReceipts {
    fn drop(&mut self) {
        for (id, (mut call, started)) in self.calls.drain() {
            call.duration_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            record_nested(&self.captured, id, &call);
            let _ = self
                .updates
                .send(CellUpdate::NestedCall(ObservedNestedCall {
                    id,
                    call,
                    shell_session_id: None,
                }));
        }
    }
}

enum CellTerminal {
    Completed { stored: HashMap<String, Value> },
    ScriptFailed { message: String },
    Terminated,
}

struct HostFailure {
    message: String,
}

impl CodeModeRuntime {
    pub(super) fn new_with_turn(_workspace: PathBuf, current_turn: Arc<AtomicU64>) -> Self {
        Self {
            admission: Arc::new(Mutex::new(())),
            admission_epoch: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            admission_attempts: Arc::new(Semaphore::new(0)),
            cells: Arc::new(Mutex::new(CellRegistry {
                next_cell_id: 1,
                live_cells: HashMap::new(),
            })),
            stored: Arc::new(Mutex::new(HashMap::new())),
            journal: None,
            host: Arc::new(Mutex::new(SharedJsHost::prewarmed())),
            current_turn,
            preempt: watch::channel((0, 0)).0,
        }
    }

    pub(super) fn set_journal(&mut self, journal: Arc<dyn CodeModeJournal>) {
        self.journal = Some(journal);
    }

    pub(super) fn has_journal(&self) -> bool {
        self.journal.is_some()
    }

    pub(super) fn control(&self) -> CodeModeControl {
        CodeModeControl {
            current_turn: Arc::clone(&self.current_turn),
            preempt: self.preempt.clone(),
            admission: Arc::clone(&self.admission),
            admission_epoch: Arc::clone(&self.admission_epoch),
            #[cfg(test)]
            admission_attempts: Arc::clone(&self.admission_attempts),
            cells: Arc::clone(&self.cells),
            host: Arc::clone(&self.host),
        }
    }

    #[cfg(test)]
    pub(super) async fn hold_admission(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.admission).lock_owned().await
    }

    #[cfg(test)]
    pub(super) async fn wait_for_admission_attempt(&self) {
        self.admission_attempts
            .acquire()
            .await
            .expect("test admission semaphore should remain open")
            .forget();
    }

    pub(super) async fn execute(
        &self,
        source: &str,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
    ) -> CodeModeExecution {
        self.execute_with_updates(source, tools, context, &mut IgnoreCodeModeUpdates)
            .await
    }

    pub(super) async fn execute_with_updates(
        &self,
        source: &str,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
        observer: &mut dyn CodeModeObserver,
    ) -> CodeModeExecution {
        let started_at = Instant::now();
        let span = info_span!(
            target: "nanocodex_oai_tools",
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
            .execute_inner(source, tools, context, started_at, observer)
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
        observer: &mut dyn CodeModeObserver,
    ) -> CodeModeExecution {
        // Subscribe before admission: steering can arrive while cell startup awaits a lock.
        // A fresh subscription never inherits an earlier observation's preemption.
        let preempt = (
            self.preempt.subscribe(),
            self.current_turn.load(Ordering::Acquire),
        );
        let admission_epoch = self.admission_epoch.load(Ordering::Acquire);
        let source = match parse_exec_source(source) {
            Ok(source) => source,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        let output_token_budget = source
            .max_output_tokens
            .unwrap_or(crate::contract::DEFAULT_TOOL_OUTPUT_TOKENS);
        tracing::Span::current().record("output.max_tokens", output_token_budget);
        let context = context.with_output_token_budget(output_token_budget);
        #[cfg(test)]
        self.admission_attempts.add_permits(1);
        let admission = self.admission.lock().await;
        if self.admission_epoch.load(Ordering::Acquire) != admission_epoch {
            return observed_execution(
                "Script terminated",
                true,
                started_at,
                Vec::new(),
                Some(output_token_budget),
                Vec::new(),
                Vec::new(),
            );
        }
        let (stored, document_version) = if let Some(journal) = &self.journal {
            match journal
                .admit_cell(
                    &context.session_id,
                    context.journal_scope.as_deref().unwrap_or(&context.call_id),
                    source.code.as_str(),
                )
                .await
            {
                Ok(CodeJournalAdmission::Execute { stored, version }) => (stored, version),
                Ok(CodeJournalAdmission::Replay(receipt)) => {
                    return serde_json::from_value(receipt).unwrap_or_else(|error| {
                        failed_execution(
                            started_at,
                            &format!("Invalid durable Code Mode receipt: {error}"),
                            Vec::new(),
                        )
                    });
                }
                Ok(CodeJournalAdmission::Unknown) => {
                    return failed_execution(
                        started_at,
                        "Code Mode cell has an unfinished durable attempt; execution outcome unknown. External effects will not be redispatched.",
                        Vec::new(),
                    );
                }
                Err(error) => {
                    return failed_execution(
                        started_at,
                        &format!("Code Mode journal admission failed: {error}"),
                        Vec::new(),
                    );
                }
            }
        } else {
            (self.stored.lock().await.clone(), 0)
        };
        let cell = {
            let mut registry = self.cells.lock().await;
            let cell_id = registry.allocate_cell_id();
            tracing::Span::current().record("cell.id", cell_id);
            let cell = Arc::new(LiveCell::spawn(
                cell_id,
                self.current_turn.load(Ordering::Acquire),
                source.code,
                tools,
                context,
                stored,
                Arc::clone(&self.stored),
                Arc::clone(&self.host),
                self.journal.clone(),
                document_version,
            ));
            registry.live_cells.insert(cell_id, Arc::clone(&cell));
            cell
        };
        drop(admission);
        let observation = Arc::clone(&cell.observation).lock_owned().await;
        let yield_after = source
            .yield_time_ms
            .map_or(INITIAL_YIELD, Duration::from_millis);
        let yield_after = observer_yield_timeout(yield_after);
        let (mut execution, running) = observe_cell(
            &cell,
            observation,
            started_at,
            ObservationMode::YieldAfter(yield_after, preempt),
            Some(output_token_budget),
            observer,
        )
        .await;
        execution.cell = Some(CodeModeCell {
            origin_call_id: cell.origin_call_id.clone(),
            running,
        });
        tracing::Span::current().record("running", running);
        if !running {
            self.remove_and_join(&cell).await;
        }
        execution
    }

    pub(super) async fn wait(&self, input: &str, _context: ToolContext<'_>) -> CodeModeExecution {
        self.wait_with_updates(input, &mut IgnoreCodeModeUpdates)
            .await
    }

    pub(super) async fn wait_with_updates(
        &self,
        input: &str,
        observer: &mut dyn CodeModeObserver,
    ) -> CodeModeExecution {
        let preempt = (
            self.preempt.subscribe(),
            self.current_turn.load(Ordering::Acquire),
        );
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
        let Some(cell) = self.cells.lock().await.live_cells.get(&cell_id).cloned() else {
            return failed_execution(
                started_at,
                &format!("exec cell {cell_id} not found"),
                Vec::new(),
            );
        };
        cell.turn_id
            .store(self.current_turn.load(Ordering::Acquire), Ordering::Release);
        let observation = match cell.begin_observation() {
            Ok(observation) => observation,
            Err(CellError::Busy) => {
                return failed_execution(
                    started_at,
                    &format!("exec cell {cell_id} already has an active observer"),
                    Vec::new(),
                );
            }
        };
        let output_token_budget = arguments.max_tokens;
        if arguments.terminate {
            cell.request_terminate();
            let (mut execution, running) = observe_cell(
                &cell,
                observation,
                started_at,
                ObservationMode::Terminate,
                output_token_budget,
                observer,
            )
            .await;
            if !running {
                self.remove_and_join(&cell).await;
            }
            execution.cell = Some(CodeModeCell {
                origin_call_id: cell.origin_call_id.clone(),
                running,
            });
            return execution;
        }
        let yield_time = Duration::from_millis(arguments.yield_time_ms);
        let yield_time = observer_yield_timeout(yield_time);
        let (mut execution, running) = observe_cell(
            &cell,
            observation,
            started_at,
            ObservationMode::YieldAfter(yield_time, preempt),
            output_token_budget,
            observer,
        )
        .await;
        if !running {
            self.remove_and_join(&cell).await;
        }
        execution.cell = Some(CodeModeCell {
            origin_call_id: cell.origin_call_id.clone(),
            running,
        });
        execution
    }

    async fn remove_and_join(&self, cell: &Arc<LiveCell>) {
        {
            let mut registry = self.cells.lock().await;
            if registry
                .live_cells
                .get(&cell.id)
                .is_some_and(|registered| Arc::ptr_eq(registered, cell))
            {
                registry.live_cells.remove(&cell.id);
            }
        }
        cell.join().await;
    }
}

fn observer_yield_timeout(yield_time: Duration) -> Duration {
    if yield_time >= MIN_YIELD_FOR_OBSERVER_GRACE {
        yield_time.saturating_add(OBSERVER_YIELD_GRACE)
    } else {
        yield_time
    }
}

impl CodeModeControl {
    /// Wake current observers without cancelling cells, nested calls, or their host.
    pub(super) fn preempt_turn(&self) {
        let turn = self.current_turn.load(Ordering::Acquire);
        self.preempt
            .send_modify(|generation| *generation = (turn, generation.1.wrapping_add(1)));
    }

    pub(super) async fn terminate_turn(
        &self,
        turn_id: u64,
        mut observer: Option<&mut dyn CodeModeObserver>,
    ) {
        #[cfg(test)]
        self.admission_attempts.add_permits(1);
        let _admission = self.admission.lock().await;
        self.admission_epoch.fetch_add(1, Ordering::AcqRel);
        let cells = {
            let mut registry = self.cells.lock().await;
            let ids = registry
                .live_cells
                .iter()
                .filter_map(|(id, cell)| {
                    (cell.turn_id.load(Ordering::Acquire) == turn_id).then_some(*id)
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| registry.live_cells.remove(&id))
                .collect::<Vec<_>>()
        };
        for cell in &cells {
            cell.request_terminate();
        }
        for cell in cells {
            if let Some(observer) = observer.as_mut() {
                cell.join_and_observe(*observer).await;
            } else {
                cell.join().await;
            }
        }
    }

    pub(super) async fn terminate_all(
        &self,
        mut observer: Option<&mut dyn CodeModeObserver>,
    ) -> CodeModeQuiescence {
        #[cfg(test)]
        self.admission_attempts.add_permits(1);
        let admission = Arc::clone(&self.admission).lock_owned().await;
        self.admission_epoch.fetch_add(1, Ordering::AcqRel);
        let cells = {
            let mut registry = self.cells.lock().await;
            std::mem::take(&mut registry.live_cells)
                .into_values()
                .collect::<Vec<_>>()
        };
        for cell in &cells {
            cell.request_terminate();
        }
        for cell in cells {
            if let Some(observer) = observer.as_mut() {
                cell.join_and_observe(*observer).await;
            } else {
                cell.join().await;
            }
        }

        let mut shared_host = self.host.lock().await;
        if let Some(mut host) = shared_host.host.take() {
            host.terminate().await;
        }
        drop(shared_host);
        CodeModeQuiescence {
            _admission: admission,
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
struct WaitArguments {
    cell_id: String,
    #[serde(default = "default_wait_yield_time_ms")]
    yield_time_ms: u64,
    #[serde(default)]
    max_tokens: Option<usize>,
    #[serde(default)]
    terminate: bool,
}

const fn default_wait_yield_time_ms() -> u64 {
    DEFAULT_WAIT_YIELD.as_millis() as u64
}

impl CellRegistry {
    const fn allocate_cell_id(&mut self) -> u64 {
        let cell_id = self.next_cell_id;
        self.next_cell_id = self.next_cell_id.saturating_add(1);
        cell_id
    }
}

impl LiveCell {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        id: u64,
        turn_id: u64,
        source: String,
        tools: Arc<ToolRegistry>,
        context: OwnedToolContext,
        stored: HashMap<String, Value>,
        shared_stored: Arc<Mutex<HashMap<String, Value>>>,
        host: Arc<Mutex<SharedJsHost>>,
        journal: Option<Arc<dyn CodeModeJournal>>,
        document_version: u64,
    ) -> Self {
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let (terminate, terminate_rx) = oneshot::channel();
        let lifecycle = Arc::new(CellLifecycle::new());
        let actor_span = info_span!(
            target: "nanocodex_oai_tools",
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
        let origin_call_id = context.call_id.clone();
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
                Arc::clone(&lifecycle),
                journal,
                document_version,
            )
            .instrument(actor_span),
        );
        Self {
            id,
            origin_call_id,
            turn_id: AtomicU64::new(turn_id),
            observation: Arc::new(Mutex::new(CellObservationState {
                updates,
                buffered: ObservationBuffer::default(),
            })),
            lifecycle,
            terminate: StdMutex::new(Some(terminate)),
            task: Mutex::new(Some(task)),
        }
    }

    fn begin_observation(&self) -> Result<OwnedMutexGuard<CellObservationState>, CellError> {
        Arc::clone(&self.observation)
            .try_lock_owned()
            .map_err(|_| CellError::Busy)
    }

    fn request_terminate(&self) {
        if self.lifecycle.request_termination() {
            let terminate = self
                .terminate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(terminate) = terminate {
                let _ = terminate.send(());
            }
        }
    }

    async fn join_and_observe(&self, observer: &mut dyn CodeModeObserver) {
        self.join().await;
        // Observation owns the queue cursor; an active observer releases its
        // lease before cancellation delivers only the remaining updates.
        let observation = Arc::clone(&self.observation).lock_owned().await;
        let _ = observe_cell(
            self,
            observation,
            Instant::now(),
            ObservationMode::Terminate,
            None,
            observer,
        )
        .await;
    }

    async fn join(&self) {
        let mut task = self.task.lock().await;
        if let Some(task) = task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for LiveCell {
    fn drop(&mut self) {
        self.request_terminate();
    }
}

impl CellLifecycle {
    const fn new() -> Self {
        Self {
            phase: AtomicU8::new(CELL_RUNNING),
        }
    }

    fn request_termination(&self) -> bool {
        self.phase
            .compare_exchange(
                CELL_RUNNING,
                CELL_TERMINATING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn claim_completion(&self) -> bool {
        self.phase
            .compare_exchange(
                CELL_RUNNING,
                CELL_COMPLETION_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn close(&self) {
        self.phase.store(CELL_CLOSED, Ordering::Release);
    }
}

enum ObservationMode {
    YieldAfter(Duration, (watch::Receiver<(u64, u64)>, u64)),
    Terminate,
}

// Keep every lifecycle update in one exhaustive, order-preserving observation loop.
#[allow(clippy::too_many_lines)]
async fn observe_cell(
    cell: &LiveCell,
    mut observation: OwnedMutexGuard<CellObservationState>,
    started_at: Instant,
    mode: ObservationMode,
    max_output_tokens: Option<usize>,
    observer: &mut dyn CodeModeObserver,
) -> (CodeModeExecution, bool) {
    let (yield_after, terminating, mut preempt) = match mode {
        ObservationMode::YieldAfter(yield_after, preempt) => {
            (Some(yield_after), false, Some(preempt))
        }
        ObservationMode::Terminate => (None, true, None),
    };
    let mut yield_timer = yield_after.map(|yield_after| Box::pin(tokio::time::sleep(yield_after)));
    let mut preempt_remaining = None;
    loop {
        let yield_deadline_elapsed = yield_timer
            .as_ref()
            .is_some_and(|yield_timer| yield_timer.deadline() <= tokio::time::Instant::now());
        if preempt_remaining.is_none()
            && preempt.as_ref().is_some_and(|(signal, turn)| {
                signal.has_changed().unwrap_or(false) && signal.borrow().0 == *turn
            })
        {
            // Drain only the already queued prefix, not an unbounded busy producer.
            preempt_remaining = Some(observation.updates.len());
        }
        let preempted = preempt_remaining.is_some();
        if preempt_remaining == Some(0) {
            let buffered = std::mem::take(&mut observation.buffered);
            return running_observation(
                cell.id,
                started_at,
                buffered.content,
                max_output_tokens,
                buffered.nested_calls,
                buffered.notifications,
            );
        }
        let update = if preempted {
            // The snapshot prefix is already queued and has a single receiver.
            // Drain it without borrowing that receiver in two select branches.
            observation.updates.recv().await
        } else {
            tokio::select! {
            biased;
            () = async {
                match preempt.as_mut() {
                    Some((signal, turn)) => loop {
                        if signal.changed().await.is_err() { std::future::pending::<()>().await; }
                        if signal.borrow().0 == *turn { break; }
                    },
                    None => std::future::pending().await,
                }
            }, if !preempted => { preempt_remaining = Some(observation.updates.len()); continue; }

            () = async {
                match yield_timer.as_mut() {
                    Some(timer) => timer.as_mut().await,
                    None => std::future::pending().await,
                }
            } => {
                let buffered = std::mem::take(&mut observation.buffered);
                return running_observation(
                    cell.id,
                    started_at,
                    buffered.content,
                    max_output_tokens,
                    buffered.nested_calls,
                    buffered.notifications,
                );
            }
            update = observation.updates.recv(), if !yield_deadline_elapsed => update,
            }
        };
        if let Some(remaining) = &mut preempt_remaining {
            *remaining = remaining.saturating_sub(1);
        }
        match update {
            Some(CellUpdate::NestedCallStarted {
                call_id,
                name,
                input,
            }) => {
                observer.update(CodeModeUpdate::NestedCallStarted {
                    call_id: &call_id,
                    name: &name,
                    input: &input,
                });
            }
            Some(CellUpdate::NestedCall(call)) => {
                observer.update(CodeModeUpdate::NestedCallCompleted(&call.call));
                observation.buffered.nested_calls.push(call);
            }
            Some(CellUpdate::Notification(notification)) => {
                observation.buffered.notifications.push(notification);
            }
            Some(CellUpdate::Content(item)) => observation.buffered.content.push(item),
            Some(CellUpdate::Yielded) if terminating => {}
            Some(CellUpdate::Yielded) => {
                let buffered = std::mem::take(&mut observation.buffered);
                return running_observation(
                    cell.id,
                    started_at,
                    buffered.content,
                    max_output_tokens,
                    buffered.nested_calls,
                    buffered.notifications,
                );
            }
            Some(CellUpdate::Completed) => {
                let buffered = std::mem::take(&mut observation.buffered);
                return (
                    observed_execution(
                        "Script completed",
                        true,
                        started_at,
                        buffered.content,
                        max_output_tokens,
                        buffered.nested_calls,
                        buffered.notifications,
                    ),
                    false,
                );
            }
            Some(CellUpdate::Terminated) => {
                let buffered = std::mem::take(&mut observation.buffered);
                return (
                    observed_execution(
                        "Script terminated",
                        true,
                        started_at,
                        buffered.content,
                        max_output_tokens,
                        buffered.nested_calls,
                        buffered.notifications,
                    ),
                    false,
                );
            }
            Some(CellUpdate::ScriptFailed { message }) => {
                observation
                    .buffered
                    .content
                    .push(ToolOutputContent::InputText {
                        text: format!("Script error:\n{message}"),
                    });
                let buffered = std::mem::take(&mut observation.buffered);
                return (
                    observed_execution(
                        "Script failed",
                        false,
                        started_at,
                        buffered.content,
                        max_output_tokens,
                        buffered.nested_calls,
                        buffered.notifications,
                    ),
                    false,
                );
            }
            Some(CellUpdate::HostFailed(message)) => {
                observation
                    .buffered
                    .content
                    .push(ToolOutputContent::InputText { text: message });
                let buffered = std::mem::take(&mut observation.buffered);
                return (
                    observed_execution(
                        "Script failed",
                        false,
                        started_at,
                        buffered.content,
                        max_output_tokens,
                        buffered.nested_calls,
                        buffered.notifications,
                    ),
                    false,
                );
            }
            None => {
                observation
                    .buffered
                    .content
                    .push(ToolOutputContent::InputText {
                        text: "local code-mode cell ended before a result".to_owned(),
                    });
                let buffered = std::mem::take(&mut observation.buffered);
                return (
                    observed_execution(
                        "Script failed",
                        false,
                        started_at,
                        buffered.content,
                        max_output_tokens,
                        buffered.nested_calls,
                        buffered.notifications,
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
    nested_calls: Vec<ObservedNestedCall>,
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
    nested_calls: Vec<ObservedNestedCall>,
    notifications: Vec<CodeModeNotification>,
) -> CodeModeExecution {
    expose_running_shell_sessions(&mut content, &nested_calls);
    let content = output::truncate_content(content, max_output_tokens);
    CodeModeExecution {
        cell: None,
        output: with_status(status, started_at.elapsed().as_secs_f64(), content),
        success,
        nested_calls: ordered_calls(nested_calls),
        notifications,
    }
}

fn expose_running_shell_sessions(
    content: &mut Vec<ToolOutputContent>,
    nested_calls: &[ObservedNestedCall],
) {
    let mut running = BTreeSet::new();
    for observed in nested_calls {
        let call = &observed.call;
        if !matches!(call.name.as_str(), "exec_command" | "write_stdin") {
            continue;
        }
        if call.name == "write_stdin"
            && let Some(input_session_id) = call.input.get("session_id").and_then(Value::as_i64)
        {
            running.remove(&input_session_id);
        }
        if let Some(session_id) = observed.shell_session_id {
            running.insert(session_id);
        }
    }
    for session_id in running {
        if content
            .iter()
            .filter_map(|item| match item {
                ToolOutputContent::InputText { text } => Some(text),
                ToolOutputContent::InputImage { .. }
                | ToolOutputContent::InputImageFile { .. }
                | ToolOutputContent::InputAudio { .. }
                | ToolOutputContent::EncryptedContent { .. } => None,
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

struct DriveCellContext<'a> {
    parent_call_id: &'a str,
    tools: &'a ToolRegistry,
    context: &'a OwnedToolContext,
    updates: &'a mpsc::UnboundedSender<CellUpdate>,
    actor_started_at: Instant,
    journal: Option<&'a dyn CodeModeJournal>,
    captured: Arc<StdMutex<JournalOutput>>,
}

impl EmbeddedHost {
    async fn drive_cell(
        &mut self,
        cell_id: u64,
        drive: DriveCellContext<'_>,
    ) -> Result<CellTerminal, HostFailure> {
        let DriveCellContext {
            parent_call_id,
            tools,
            context,
            updates,
            actor_started_at,
            journal,
            captured,
        } = drive;
        let mut pending_calls: FuturesUnordered<BoxFuture<'_, CompletedNestedCall>> =
            FuturesUnordered::new();
        let mut pending_receipts = PendingCallReceipts {
            captured: Arc::clone(&captured),
            calls: HashMap::new(),
            updates: updates.clone(),
        };
        let mut event_count = 0_u64;
        loop {
            tokio::select! {
                biased;
                completed = pending_calls.next(), if !pending_calls.is_empty() => {
                    let Some(completed) = completed else {
                        continue;
                    };
                    // A completed result wins over a simultaneously ready root terminal.
                    if let Some(journal) = journal {
                        journal.complete_effect(parent_call_id, &completed.call.call_id, &nested_receipt(&completed.call))
                            .await.map_err(HostFailure::new)?;
                    }
                    self.send_completed_call(cell_id, completed, &mut pending_receipts)?;
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
                            let nested_call_id = format!("{}/code-{id}", context.call_id);
                            if let Some(journal) = journal {
                                journal.begin_effect(parent_call_id, &nested_call_id, &name, &input)
                                    .await.map_err(HostFailure::new)?;
                            }
                            let started = Instant::now();
                            let message = "Code Mode cell ended before the tool returned; execution outcome unknown";
                            pending_receipts.calls.insert(id, (NestedToolCall {
                                call_id: nested_call_id.clone(), name: name.clone(), input: input.clone(),
                                output: ToolOutputBody::Text(message.to_owned()),
                                structured_result: serde_json::json!({
                                    "error": message, "code": "CODE_MODE_CALL_INTERRUPTED", "outcome": "unknown",
                                }),
                                success: false,
                                started_after_ns: u64::try_from(started.duration_since(actor_started_at).as_nanos()).unwrap_or(u64::MAX),
                                duration_ns: 0, metadata: None,
                            }, started));
                            let _ = updates.send(CellUpdate::NestedCallStarted {
                                call_id: nested_call_id,
                                name: name.clone(),
                                input: input.clone(),
                            });
                            let nested_call = execute_nested_call(
                                tools,
                                id,
                                name,
                                input,
                                context,
                                actor_started_at,
                            );
                            pending_calls.push(nested_call.boxed());
                        }
                        RuntimeEvent::Notify { text, .. } => {
                            captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                                .notifications.push(serde_json::json!({"call_id": parent_call_id, "text": text}));
                            let _ = updates.send(CellUpdate::Notification(
                                CodeModeNotification::new(parent_call_id, text),
                            ));
                        }
                        RuntimeEvent::Content { content, .. } => {
                            captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                                .content.push(content.clone());
                            let _ = updates.send(CellUpdate::Content(content));
                        }
                        RuntimeEvent::Yielded { .. } => {
                            let _ = updates.send(CellUpdate::Yielded);
                        }
                        RuntimeEvent::Done {
                            stored,
                            ..
                        } => {
                            tracing::Span::current().record("runtime.event_count", event_count);
                            return Ok(CellTerminal::Completed { stored });
                        }
                        RuntimeEvent::Error {
                            message,
                            stored,
                            ..
                        } => {
                            tracing::Span::current().record("runtime.event_count", event_count);
                            let _ = stored;
                            return Ok(CellTerminal::ScriptFailed { message });
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
        pending_receipts: &mut PendingCallReceipts,
    ) -> Result<(), HostFailure> {
        let CompletedNestedCall {
            id,
            value,
            call,
            shell_session_id,
        } = completed;
        let success = call.success;
        // Host execution is already known. Publish it independently of guest
        // delivery, which may fail after the cell or its runtime has closed.
        pending_receipts.calls.remove(&id);
        record_nested(&pending_receipts.captured, id, &call);
        let _ = pending_receipts
            .updates
            .send(CellUpdate::NestedCall(ObservedNestedCall {
                id,
                call,
                shell_session_id,
            }));
        self.send_tool_result(cell_id, id, value, success)
            .map_err(HostFailure::new)
    }
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
    lifecycle: Arc<CellLifecycle>,
    journal: Option<Arc<dyn CodeModeJournal>>,
    document_version: u64,
) {
    let started_at = Instant::now();
    let host_wait_started_at = Instant::now();
    let (mut host, reused) = {
        let mut shared_host = shared_host.lock().await;
        let reused = shared_host.host.is_some();
        let host = match shared_host.host.take() {
            Some(host) => host,
            None => match spawn_host() {
                Ok(host) => host,
                Err(message) => {
                    tracing::Span::current().record("status", "failed");
                    tracing::Span::current().record("otel.status_code", "ERROR");
                    record_elapsed("duration_ns", started_at);
                    let update = if lifecycle.claim_completion() {
                        CellUpdate::HostFailed(message)
                    } else {
                        CellUpdate::Terminated
                    };
                    let _ = updates.send(update);
                    lifecycle.close();
                    return;
                }
            },
        };
        (host, reused)
    };
    record_elapsed("host.wait_ns", host_wait_started_at);
    tracing::Span::current().record("host.reused", reused);
    let captured = Arc::new(StdMutex::new(JournalOutput::default()));
    let initial_stored = stored.clone();
    let run = async {
        host.start_cell(cell_id, &source, stored, tools.nested_tool_metadata())
            .map_err(HostFailure::new)?;
        host.drive_cell(
            cell_id,
            DriveCellContext {
                parent_call_id: context.journal_scope.as_deref().unwrap_or(&context.call_id),
                tools: tools.as_ref(),
                context: &context,
                updates: &updates,
                actor_started_at: started_at,
                journal: journal.as_deref(),
                captured: Arc::clone(&captured),
            },
        )
        .await
    };
    let selected = tokio::select! {
        biased;
        _ = &mut terminate => {
            None
        }
        terminal = run => Some(terminal),
    };
    let terminal = match selected {
        Some(terminal) if lifecycle.claim_completion() => terminal,
        Some(_) | None => {
            let termination_started_at = Instant::now();
            host.terminate().await;
            record_elapsed("host.termination_ns", termination_started_at);
            Ok(CellTerminal::Terminated)
        }
    };
    let (status, otel_status) = match &terminal {
        Ok(CellTerminal::Completed { .. }) => ("completed", "OK"),
        Ok(CellTerminal::Terminated) => ("cancelled", "ERROR"),
        Ok(CellTerminal::ScriptFailed { .. }) | Err(_) => ("failed", "ERROR"),
    };
    tracing::Span::current().record("status", status);
    tracing::Span::current().record("otel.status_code", otel_status);
    let terminated = matches!(&terminal, Ok(CellTerminal::Terminated));
    let host_healthy = matches!(
        &terminal,
        Ok(CellTerminal::Completed { .. } | CellTerminal::ScriptFailed { .. })
    );
    let journal_result = if let Some(journal) = &journal {
        let (status, success, message) = match &terminal {
            Ok(CellTerminal::Completed { .. }) => ("Script completed", true, None),
            Ok(CellTerminal::ScriptFailed { message, .. }) => {
                ("Script failed", false, Some(message.clone()))
            }
            Ok(CellTerminal::Terminated) => ("Script terminated", true, None),
            Err(failure) => ("Script failed", false, Some(failure.message.clone())),
        };
        let (receipt, unknown) = {
            let mut output = captured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(message) = message {
                output
                    .content
                    .push(ToolOutputContent::InputText { text: message });
            }
            let nested_calls = output.calls.values().cloned().collect::<Vec<_>>();
            let unknown = nested_calls
                .iter()
                .filter(|call| call["structured_result"]["outcome"] == "unknown")
                .cloned()
                .collect::<Vec<_>>();
            let content = output::truncate_content(
                std::mem::take(&mut output.content),
                Some(context.output_token_budget),
            );
            let receipt = serde_json::json!({
                "cell": {"origin_call_id": context.call_id, "running": false},
                "output": with_status(status, started_at.elapsed().as_secs_f64(), content),
                "success": success, "nested_calls": nested_calls,
                "notifications": output.notifications,
            });
            (receipt, unknown)
        };
        let mut result = Ok(());
        for call in unknown {
            result = journal
                .complete_effect(
                    context.journal_scope.as_deref().unwrap_or(&context.call_id),
                    call["call_id"].as_str().unwrap_or_default(),
                    &call,
                )
                .await;
            if result.is_err() {
                break;
            }
        }
        if result.is_ok() {
            let next_stored = match &terminal {
                Ok(CellTerminal::Completed { stored }) => {
                    let mut next = initial_stored;
                    next.extend(stored.clone());
                    Some(next)
                }
                _ => None,
            };
            result = journal
                .complete_cell(
                    context.journal_scope.as_deref().unwrap_or(&context.call_id),
                    document_version,
                    next_stored,
                    &receipt,
                )
                .await;
        }
        result
    } else {
        Ok(())
    };
    match (terminal, journal_result) {
        (_, Err(error)) => {
            let _ = updates.send(CellUpdate::HostFailed(format!(
                "Code Mode journal commit failed; execution outcome unknown: {error}"
            )));
        }
        (Ok(CellTerminal::Completed { stored }), Ok(())) => {
            shared_stored.lock().await.extend(stored);
            let _ = updates.send(CellUpdate::Completed);
        }
        (Ok(CellTerminal::ScriptFailed { message, .. }), Ok(())) => {
            let _ = updates.send(CellUpdate::ScriptFailed { message });
        }
        (Ok(CellTerminal::Terminated), Ok(())) => {
            let _ = updates.send(CellUpdate::Terminated);
        }
        (Err(failure), Ok(())) => {
            let _ = updates.send(CellUpdate::HostFailed(failure.message));
        }
    }
    if host_healthy {
        let mut shared_host = shared_host.lock().await;
        if shared_host.host.is_none() {
            shared_host.host = Some(host);
        } else {
            drop(shared_host);
            host.terminate().await;
        }
    } else if !terminated {
        let termination_started_at = Instant::now();
        host.terminate().await;
        record_elapsed("host.termination_ns", termination_started_at);
    }
    lifecycle.close();
    record_elapsed("duration_ns", started_at);
}

fn record_elapsed(field: &'static str, started_at: Instant) {
    tracing::Span::current().record(
        field,
        u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
    );
}

impl HostFailure {
    const fn new(message: String) -> Self {
        Self { message }
    }
}

fn ordered_calls(mut calls: Vec<ObservedNestedCall>) -> Vec<NestedToolCall> {
    calls.sort_unstable_by_key(|call| call.id);
    calls.into_iter().map(|call| call.call).collect()
}

async fn execute_nested_call(
    tools: &ToolRegistry,
    id: u64,
    name: String,
    input: Value,
    context: &OwnedToolContext,
    cell_started_at: Instant,
) -> CompletedNestedCall {
    let started_at = Instant::now();
    let started_after_ns =
        u64::try_from(started_at.duration_since(cell_started_at).as_nanos()).unwrap_or(u64::MAX);
    let call_id = format!("{}/code-{id}", context.call_id);
    let context = context.as_context();
    let context = ToolContext::new(
        context.model(),
        context.session_id(),
        &call_id,
        context.history(),
        context.output_token_budget(),
    )
    .with_instruction_revision(context.instruction_revision())
    .with_host_context(context.host_context())
    .with_turn_id(context.turn_id());
    let execution = tools.execute_nested(&name, input.clone(), context).await;
    let duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let value = execution.structured_result();
    let shell_session_id = execution
        .process_trace()
        .and_then(|process| process.session_id)
        .or_else(|| value.get("session_id").and_then(Value::as_i64));
    CompletedNestedCall {
        id,
        value: value.clone(),
        shell_session_id,
        call: NestedToolCall {
            call_id,
            name,
            input,
            output: execution.output,
            structured_result: value,
            success: execution.success,
            started_after_ns,
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
        cell: None,
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
