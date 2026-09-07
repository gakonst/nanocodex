//! Code Mode execution results, notifications, and nested-tool observation.

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
use tokio::{
    sync::{Mutex, OwnedMutexGuard, RwLock, Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time::Duration,
};
use tracing::{Instrument, info_span};

use super::{ToolContext, ToolOutputBody, ToolOutputContent};
pub use crate::embedded::{
    CodeModeExecution, CodeModeNotification, CodeModeObserver, CodeModeUpdate, NestedToolCall,
};
use crate::runtime::{OwnedToolContext, ToolRegistry};
use embedded::EmbeddedHost;
pub(crate) use spec::{exec_spec, wait_spec};

const INITIAL_YIELD: Duration = if cfg!(test) {
    Duration::from_secs(30)
} else {
    Duration::from_secs(10)
};
const DEFAULT_WAIT_YIELD: Duration = Duration::from_secs(10);
const OBSERVER_YIELD_GRACE: Duration = Duration::from_secs(1);
const MIN_YIELD_FOR_OBSERVER_GRACE: Duration = Duration::from_secs(10);
const MAX_CONCURRENT_NESTED_CALLS: usize = 128;
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
    host: Arc<Mutex<SharedJsHost>>,
    current_turn: Arc<AtomicU64>,
}

#[derive(Clone)]
pub(crate) struct CodeModeControl {
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
    live_cells: HashMap<u64, Arc<LiveCell>>,
}

struct LiveCell {
    id: u64,
    turn_id: AtomicU64,
    output_token_budget: usize,
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

enum CellTerminal {
    Completed {
        stored: HashMap<String, Value>,
    },
    ScriptFailed {
        message: String,
        stored: HashMap<String, Value>,
    },
    Terminated,
}

struct HostFailure {
    message: String,
}

impl CodeModeRuntime {
    #[cfg(test)]
    pub(super) fn new(workspace: PathBuf) -> Self {
        Self::new_with_turn(workspace, Arc::new(AtomicU64::new(0)))
    }

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
            host: Arc::new(Mutex::new(SharedJsHost::prewarmed())),
            current_turn,
        }
    }

    pub(super) fn control(&self) -> CodeModeControl {
        CodeModeControl {
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
        let admission_epoch = self.admission_epoch.load(Ordering::Acquire);
        let source = match parse_exec_source(source) {
            Ok(source) => source,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        let output_token_budget = source
            .max_output_tokens
            .unwrap_or(context.output_token_budget)
            .max(1);
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
        let stored = self.stored.lock().await.clone();
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
                output_token_budget,
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
        let (execution, running) = observe_cell(
            &cell,
            observation,
            started_at,
            ObservationMode::YieldAfter(yield_after),
            Some(output_token_budget),
            observer,
        )
        .await;
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
        let continued_output_token_budget = cell.output_token_budget;
        if arguments.terminate {
            cell.request_terminate();
            let (execution, running) = observe_cell(
                &cell,
                observation,
                started_at,
                ObservationMode::Terminate,
                Some(continued_output_token_budget),
                observer,
            )
            .await;
            if !running {
                self.remove_and_join(&cell).await;
            }
            return execution;
        }
        let yield_time = Duration::from_millis(
            arguments
                .yield_time_ms
                .unwrap_or(u64::try_from(DEFAULT_WAIT_YIELD.as_millis()).unwrap_or(u64::MAX)),
        );
        let yield_time = observer_yield_timeout(yield_time);
        let output_token_budget = arguments
            .max_tokens
            .unwrap_or(continued_output_token_budget)
            .max(1);
        let (execution, running) = observe_cell(
            &cell,
            observation,
            started_at,
            ObservationMode::YieldAfter(yield_time),
            Some(output_token_budget),
            observer,
        )
        .await;
        if !running {
            self.remove_and_join(&cell).await;
        }
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
    pub(super) async fn terminate_turn(&self, turn_id: u64) {
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
            cell.join().await;
        }
    }

    pub(super) async fn terminate_all(&self) -> CodeModeQuiescence {
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
            cell.join().await;
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
        output_token_budget: usize,
    ) -> Self {
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let (terminate, terminate_rx) = oneshot::channel();
        let lifecycle = Arc::new(CellLifecycle::new());
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
                Arc::clone(&lifecycle),
            )
            .instrument(actor_span),
        );
        Self {
            id,
            turn_id: AtomicU64::new(turn_id),
            output_token_budget,
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
    YieldAfter(Duration),
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
    let (yield_after, terminating) = match mode {
        ObservationMode::YieldAfter(yield_after) => (Some(yield_after), false),
        ObservationMode::Terminate => (None, true),
    };
    let mut yield_timer = yield_after.map(|yield_after| Box::pin(tokio::time::sleep(yield_after)));
    loop {
        let yield_deadline_elapsed = yield_timer
            .as_ref()
            .is_some_and(|yield_timer| yield_timer.deadline() <= tokio::time::Instant::now());
        let update = tokio::select! {
            biased;
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
        };
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
        let nested_call_permits = Arc::new(Semaphore::new(MAX_CONCURRENT_NESTED_CALLS));
        let parallel_execution = Arc::new(RwLock::new(()));
        let mut event_count = 0_u64;
        loop {
            tokio::select! {
                completed = pending_calls.next(), if !pending_calls.is_empty() => {
                    let Some(completed) = completed else {
                        continue;
                    };
                    let call = self.send_completed_call(cell_id, completed)?;
                    let _ = updates.send(CellUpdate::NestedCall(call));
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
                            let _ = updates.send(CellUpdate::NestedCallStarted {
                                call_id: nested_call_id,
                                name: name.clone(),
                                input: input.clone(),
                            });
                            let permit = Arc::clone(&nested_call_permits);
                            let supports_parallel = tools.supports_parallel_tool_calls(&name);
                            let parallel_execution = Arc::clone(&parallel_execution);
                            let nested_call = async move {
                                let _permit = permit.acquire_owned().await;
                                if supports_parallel {
                                    let _guard = parallel_execution.read().await;
                                    execute_nested_call(
                                        tools,
                                        id,
                                        name,
                                        input,
                                        context,
                                        actor_started_at,
                                    )
                                    .await
                                } else {
                                    let _guard = parallel_execution.write().await;
                                    execute_nested_call(
                                        tools,
                                        id,
                                        name,
                                        input,
                                        context,
                                        actor_started_at,
                                    )
                                    .await
                                }
                            };
                            pending_calls.push(nested_call.boxed());
                        }
                        RuntimeEvent::Notify { text, .. } => {
                            let _ = updates.send(CellUpdate::Notification(
                                CodeModeNotification::new(parent_call_id, text),
                            ));
                        }
                        RuntimeEvent::Content { content, .. } => {
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
                            return Ok(CellTerminal::ScriptFailed {
                                message,
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
    ) -> Result<ObservedNestedCall, HostFailure> {
        self.send_tool_result(
            cell_id,
            completed.id,
            completed.value,
            completed.call.success,
        )
        .map_err(HostFailure::new)?;
        Ok(ObservedNestedCall {
            id: completed.id,
            call: completed.call,
            shell_session_id: completed.shell_session_id,
        })
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
    match terminal {
        Ok(CellTerminal::Completed { stored }) => {
            shared_stored.lock().await.extend(stored);
            let _ = updates.send(CellUpdate::Completed);
        }
        Ok(CellTerminal::ScriptFailed { message, stored }) => {
            shared_stored.lock().await.extend(stored);
            let _ = updates.send(CellUpdate::ScriptFailed { message });
        }
        Ok(CellTerminal::Terminated) => {
            let _ = updates.send(CellUpdate::Terminated);
        }
        Err(failure) => {
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
    );
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
