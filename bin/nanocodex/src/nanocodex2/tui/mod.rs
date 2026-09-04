//! Tact-derived terminal presentation adapted to the managed Nanocodex driver.
//!
//! Portions of this module tree derive from clabby/tact at revision
//! e20b1584642339546bb2310aad6968edeec66a53 and are modified for Nanocodex2.
//! They remain available under Apache-2.0. The managed service owns agent
//! orchestration and hosted tools; this module owns only presentation, terminal
//! interaction, and the caller-local shell convenience.

mod clipboard;
mod components;
mod context;
mod editor;
mod format;
mod history;
mod pane;
mod prompt;
mod scheduler;
mod session;
mod shell;
mod spinner;
mod terminal;
mod theme;
mod transcript;

use self::{
    components::{
        AppEffect, AppEvent, AppNode, ComponentUpdate, DraftReset, RenderRequest,
        RestoredSessionProjection, RootEffect, RootNode,
    },
    history::{
        HistoryPrefetch, HistoryWindow, history_projection, history_projection_with_sequences,
        live_managed_projection, older_history_projection_with_sequences, unix_ms,
    },
    pane::PaneId,
    prompt::Submission,
    scheduler::{RenderScheduler, STREAM_FRAME_INTERVAL},
    session::{RecentPrompt, SessionSummary},
    shell::ShellExecution,
    terminal::TerminalSession,
    theme::{Theme, detect_system_scheme},
    transcript::{LocalEvent, ShellId, TranscriptRecord, TurnId},
};
use crate::{config::ReasoningEffort, config::ReasoningMode, host::HostConfig};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use nanocodex::Model;
use nanocodex_agent::{Nanocodex, NanocodexError, PromptRequest, Turn, TurnControl, TurnResult};
use nanocodex_managed::{
    AgentList, AgentSettings, AgentState, EventCursor, EventHistoryPage, ManagedClient,
    ManagedError, ManagedEvent, ManagedEventData, ReasoningMode as ManagedReasoningMode, Thinking,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::pending,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::{sync::mpsc, task::JoinSet};

type Admission = (PaneId, TurnId, Result<Turn, NanocodexError>);
type Completion = (PaneId, TurnId, Result<TurnResult, NanocodexError>);
type SteerCompletion = (
    PaneId,
    components::QueueId,
    u64,
    SteerTarget,
    Result<(), SteerFailure>,
);
type WaitingSteer = (PaneId, components::QueueId, Submission);
enum CancelTarget {
    Local {
        generation: u64,
        agent_id: String,
        id: TurnId,
        turn_id: String,
    },
    Managed {
        generation: u64,
        agent_id: String,
        turn_id: String,
    },
}

impl CancelTarget {
    fn agent_id(&self) -> &str {
        match self {
            Self::Local { agent_id, .. } | Self::Managed { agent_id, .. } => agent_id,
        }
    }

    fn turn_id(&self) -> &str {
        match self {
            Self::Local { turn_id, .. } | Self::Managed { turn_id, .. } => turn_id,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum SteerTarget {
    Local(TurnId),
    Managed { agent_id: String, turn_id: String },
}

#[derive(Debug, Eq, PartialEq)]
enum SteerResolution {
    Admitted,
    Failed(Option<String>),
    Stale,
}

enum SteerFailure {
    Inactive,
    Other(String),
}

impl SteerFailure {
    fn managed(error: ManagedError) -> Self {
        if matches!(&error, ManagedError::Http { status, code, .. }
            if status.as_u16() == 409 && matches!(code.as_str(), "turn_not_active" | "turn_not_steerable"))
        {
            Self::Inactive
        } else {
            Self::Other(error.to_string())
        }
    }

    fn backend(error: NanocodexError) -> Self {
        if matches!(error, NanocodexError::TurnNotSteerable) {
            return Self::Inactive;
        }
        if let NanocodexError::Backend { source, .. } = &error
            && let Some(ManagedError::Http { status, code, .. }) =
                source.downcast_ref::<ManagedError>()
            && status.as_u16() == 409
            && matches!(code.as_str(), "turn_not_active" | "turn_not_steerable")
        {
            return Self::Inactive;
        }
        Self::Other(error.to_string())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CancelDisposition {
    Accepted,
    Terminal,
}

type CancelCompletion = (PaneId, CancelTarget, Result<CancelDisposition, String>);
type SettingsCompletion = (
    PaneId,
    String,
    &'static str,
    Result<AgentSettings, ManagedError>,
);
type HistoryCompletion = (
    PaneId,
    String,
    u64,
    String,
    Result<EventHistoryPage, ManagedError>,
);
type HistoryReplayCompletion = (
    PaneId,
    String,
    u64,
    String,
    Result<PreparedHistoryReplay, ManagedError>,
);
type ConnectedAgent = (
    Nanocodex,
    mpsc::UnboundedReceiver<ManagedEvent>,
    String,
    PathBuf,
    HistoryWindow,
    Option<String>,
    AgentSettings,
    bool,
    ManagedActiveTurns,
);

#[derive(Clone, Debug, Default)]
struct ManagedActiveTurns {
    ids: HashSet<String>,
    order: Vec<String>,
    live_steer: bool,
    live_cancel: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedObservation {
    active_changed: bool,
    external: bool,
}

#[derive(Debug, Eq, PartialEq)]
enum CancellationResolution {
    Accepted,
    Failed(String),
    Stale,
}

#[derive(Debug)]
struct CancellationFence<Id> {
    in_flight: HashSet<Id>,
    accepted: HashSet<Id>,
    terminal_observed: HashSet<Id>,
}

impl<Id> Default for CancellationFence<Id> {
    fn default() -> Self {
        Self {
            in_flight: HashSet::new(),
            accepted: HashSet::new(),
            terminal_observed: HashSet::new(),
        }
    }
}

impl<Id> CancellationFence<Id>
where
    Id: Clone + Eq + std::hash::Hash,
{
    fn begin(&mut self, id: Id) -> bool {
        if self.accepted.contains(&id) {
            return false;
        }
        self.in_flight.insert(id)
    }

    fn finish(
        &mut self,
        id: Id,
        outcome: Result<CancelDisposition, String>,
        active: bool,
    ) -> CancellationResolution {
        self.in_flight.remove(&id);
        let terminal_observed = self.terminal_observed.remove(&id);
        resolve_cancellation(
            &mut self.accepted,
            id,
            outcome,
            active && !terminal_observed,
        )
    }

    fn terminal(&mut self, id: &Id) {
        self.accepted.remove(id);
        if self.in_flight.contains(id) {
            self.terminal_observed.insert(id.clone());
        } else {
            self.terminal_observed.remove(id);
        }
    }

    fn reset(&mut self) {
        self.in_flight.clear();
        self.accepted.clear();
        self.terminal_observed.clear();
    }
}

#[derive(Debug, Default)]
struct CancellationFences {
    local: CancellationFence<TurnId>,
    managed: CancellationFence<String>,
}

impl CancellationFences {
    fn has_in_flight(&self) -> bool {
        !self.local.in_flight.is_empty() || !self.managed.in_flight.is_empty()
    }

    fn begin_local(&mut self, id: TurnId) -> bool {
        self.local.begin(id)
    }

    fn begin_managed(&mut self, id: &str) -> bool {
        self.managed.begin(id.to_owned())
    }

    fn finish_local(
        &mut self,
        id: TurnId,
        outcome: Result<CancelDisposition, String>,
        active: bool,
    ) -> CancellationResolution {
        self.local.finish(id, outcome, active)
    }

    fn finish_managed(
        &mut self,
        id: String,
        outcome: Result<CancelDisposition, String>,
        active: bool,
    ) -> CancellationResolution {
        self.managed.finish(id, outcome, active)
    }

    fn local_terminal(&mut self, id: TurnId) {
        self.local.terminal(&id);
    }

    fn managed_terminal(&mut self, id: &str) {
        self.managed.terminal(&id.to_owned());
    }

    fn reset(&mut self) {
        self.local.reset();
        self.managed.reset();
    }
}

fn resolve_cancellation<Id>(
    accepted: &mut HashSet<Id>,
    id: Id,
    outcome: Result<CancelDisposition, String>,
    active: bool,
) -> CancellationResolution
where
    Id: Eq + std::hash::Hash,
{
    match outcome {
        Ok(CancelDisposition::Accepted) => {
            if active && accepted.insert(id) {
                CancellationResolution::Accepted
            } else {
                CancellationResolution::Stale
            }
        }
        Ok(CancelDisposition::Terminal) => CancellationResolution::Stale,
        Err(error) => {
            if active && !accepted.contains(&id) {
                CancellationResolution::Failed(error)
            } else {
                CancellationResolution::Stale
            }
        }
    }
}

fn cancel_disposition(
    expected_turn_id: &str,
    returned_turn_id: &str,
    state: &str,
) -> Result<CancelDisposition, String> {
    if returned_turn_id != expected_turn_id {
        return Err("managed cancel acknowledged a different turn".to_owned());
    }
    match state {
        "cancelling" => Ok(CancelDisposition::Accepted),
        "completed" | "cancelled" | "failed" => Ok(CancelDisposition::Terminal),
        other => Err(format!(
            "managed cancel returned unexpected state `{other}`"
        )),
    }
}

impl ManagedActiveTurns {
    fn from_state(state: &AgentState) -> Self {
        Self {
            ids: state.active_turns.iter().cloned().collect(),
            order: state.active_turns.clone(),
            live_steer: state.capabilities.live_steer,
            live_cancel: state.capabilities.live_cancel,
        }
    }

    fn observe(
        &mut self,
        event: &ManagedEvent,
        local_ids: &HashMap<TurnId, String>,
    ) -> ManagedObservation {
        let before = self.ids.len();
        let external = event.turn_id.as_ref().is_some_and(|id| {
            self.ids.contains(id) && !local_ids.values().any(|local_id| local_id == id)
        });
        match &event.data {
            ManagedEventData::TurnAccepted { id, .. }
                if !local_ids.values().any(|local_id| local_id == id) =>
            {
                if self.ids.insert(id.clone()) {
                    self.order.push(id.clone());
                }
            }
            ManagedEventData::TurnCompleted { id, .. }
            | ManagedEventData::TurnCancelled { id }
            | ManagedEventData::TurnFailed { id, .. } => {
                self.remove(id);
            }
            _ => {}
        }
        ManagedObservation {
            active_changed: self.ids.len() != before,
            external,
        }
    }

    fn remove(&mut self, id: &str) -> bool {
        self.order.retain(|retained| retained != id);
        self.ids.remove(id)
    }

    fn steer_target(&self) -> Result<&str, &'static str> {
        if !self.live_steer {
            return Err("this managed agent does not allow live steering");
        }
        match self.ids.len() {
            1 => Ok(self.ids.iter().next().expect("one active managed turn")),
            0 => Err("no attached managed turn is active"),
            // Durable state and turn_accepted events carry admission order.
            // Later accepted turns may still be queued behind this one.
            _ => self
                .order
                .iter()
                .find(|id| self.ids.contains(*id))
                .map(String::as_str)
                .ok_or("managed turn admission order is unavailable"),
        }
    }
}

const HISTORY_PAGE_SIZE: u16 = 256;

#[derive(Clone)]
enum RetryTarget {
    Create(AgentSettings),
    Agent(String),
}

struct ConnectionFailure {
    error: ManagedError,
    retry: RetryTarget,
}

#[derive(Clone, Copy)]
enum ConnectionPurpose {
    Startup,
    Resume(PaneId),
    New(PaneId),
}

enum ConnectionResult {
    Agent {
        purpose: ConnectionPurpose,
        result: Result<ConnectedAgent, ConnectionFailure>,
    },
    Sessions {
        pane: PaneId,
        result: Result<AgentList, ManagedError>,
    },
    Disconnected(Result<(), NanocodexError>),
}

#[derive(Clone, Copy)]
enum SettingsMutation {
    Complete(AgentSettings),
    Thinking(Thinking),
    FastMode(bool),
}

impl SettingsMutation {
    fn failure_subject(self) -> &'static str {
        match self {
            Self::Complete(_) => "select model",
            Self::Thinking(_) => "change thinking effort",
            Self::FastMode(_) => "change fast mode",
        }
    }
}

struct DriverRuntime {
    client: ManagedClient,
    agent: Option<Nanocodex>,
    managed_events: Option<mpsc::UnboundedReceiver<ManagedEvent>>,
    managed_events_open: bool,
    connection_generation: u64,
    agent_id: String,
    settings: AgentSettings,
    pending_settings: Option<AgentSettings>,
    workspace: PathBuf,
    sequence: u64,
    next_turn: u64,
    next_shell: u64,
    controls: HashMap<TurnId, TurnControl>,
    local_managed_turns: HashMap<TurnId, String>,
    managed_active_turns: ManagedActiveTurns,
    admitting: HashSet<TurnId>,
    cancel_after_admission: HashSet<TurnId>,
    cancellation_fences: CancellationFences,
    cancellation_failed: bool,
    cancellation_had_effect: bool,
    admissions: JoinSet<Admission>,
    completions: JoinSet<Completion>,
    steers: JoinSet<SteerCompletion>,
    waiting_steers: VecDeque<WaitingSteer>,
    cancellations: JoinSet<CancelCompletion>,
    settings_updates: JoinSet<SettingsCompletion>,
    settings_queue: VecDeque<(PaneId, String, SettingsMutation)>,
    shells: JoinSet<(PaneId, ShellExecution)>,
    history_loads: JoinSet<HistoryCompletion>,
    history_replays: JoinSet<HistoryReplayCompletion>,
    history_prefetch: HistoryPrefetch,
    history_generation: u64,
    history: HistoryWindow,
    history_sequences: HashMap<String, u64>,
    history_records: Vec<Arc<TranscriptRecord>>,
    live_records: Vec<Arc<TranscriptRecord>>,
    active_shells: usize,
    shell_context: Vec<String>,
    pending_submission: Option<(PaneId, TurnId, Submission)>,
    recent_prompts: Vec<RecentPrompt>,
    connection: JoinSet<ConnectionResult>,
    retry_target: Option<RetryTarget>,
}

struct PreparedHistoryReplay {
    older_history: HistoryWindow,
    sequences: HashMap<String, u64>,
    next_sequence: u64,
    history_records: Vec<Arc<TranscriptRecord>>,
    older_prompts: Vec<RecentPrompt>,
    projection: Box<RestoredSessionProjection>,
}

fn project_open_history(
    effort: ReasoningEffort,
    history_records: &[Arc<TranscriptRecord>],
    live_records: &[Arc<TranscriptRecord>],
) -> RestoredSessionProjection {
    let mut records = Vec::with_capacity(history_records.len().saturating_add(live_records.len()));
    records.extend(history_records.iter().cloned());
    records.extend(live_records.iter().cloned());
    RootNode::project_open_session(effort, records)
}

fn prepare_history_replay(
    page: EventHistoryPage,
    mut sequences: HashMap<String, u64>,
    next_sequence: u64,
    mut history_records: Vec<Arc<TranscriptRecord>>,
    live_records: Vec<Arc<TranscriptRecord>>,
    coherent_tail: bool,
    agent_id: &str,
    workspace: &Path,
    effort: ReasoningEffort,
) -> Result<PreparedHistoryReplay, ManagedError> {
    let mut older_history = HistoryWindow::default();
    older_history.prepend(page)?;
    let mut projected_next_sequence = next_sequence;
    let (mut older_records, older_prompts) = older_history_projection_with_sequences(
        &older_history.events,
        coherent_tail,
        agent_id,
        workspace,
        &mut sequences,
        &mut projected_next_sequence,
    )?;
    older_records.append(&mut history_records);
    let history_records = older_records;
    let projection = Box::new(project_open_history(
        effort,
        &history_records,
        &live_records,
    ));
    Ok(PreparedHistoryReplay {
        older_history,
        sequences,
        next_sequence: projected_next_sequence,
        history_records,
        older_prompts,
        projection,
    })
}

fn history_replay_matches(
    agent_id: &str,
    generation: u64,
    requested_before: &str,
    runtime_agent_id: &str,
    runtime_generation: u64,
    runtime_before: Option<&str>,
) -> bool {
    agent_id == runtime_agent_id
        && generation == runtime_generation
        && runtime_before == Some(requested_before)
}

impl DriverRuntime {
    fn resolve_steer(
        &self,
        generation: u64,
        target: &SteerTarget,
        outcome: Result<(), SteerFailure>,
    ) -> SteerResolution {
        if generation != self.connection_generation {
            return SteerResolution::Stale;
        }
        let current = match target {
            SteerTarget::Local(id) => self.controls.contains_key(id),
            SteerTarget::Managed { agent_id, turn_id } => {
                agent_id == &self.agent_id && self.managed_active_turns.ids.contains(turn_id)
            }
        };
        match outcome {
            // An acknowledgement means the input was accepted even if the
            // terminal event won the race with the control response. Requeuing
            // it here would submit the same instruction a second time.
            Ok(()) => SteerResolution::Admitted,
            Err(SteerFailure::Other(error)) if current => SteerResolution::Failed(Some(error)),
            Err(_) => SteerResolution::Failed(None),
        }
    }

    fn finish_cancellation(
        &mut self,
        target: CancelTarget,
        outcome: Result<CancelDisposition, String>,
    ) -> CancellationResolution {
        match target {
            CancelTarget::Local {
                generation,
                agent_id,
                id,
                turn_id,
            } if generation == self.connection_generation => {
                let active = agent_id == self.agent_id
                    && self.controls.contains_key(&id)
                    && self.local_managed_turns.get(&id) == Some(&turn_id);
                self.cancellation_fences.finish_local(id, outcome, active)
            }
            CancelTarget::Managed {
                generation,
                agent_id,
                turn_id,
            } if generation == self.connection_generation => {
                let active = agent_id == self.agent_id
                    && (!self.managed_events_open
                        || self.managed_active_turns.ids.contains(&turn_id));
                self.cancellation_fences
                    .finish_managed(turn_id, outcome, active)
            }
            _ => CancellationResolution::Stale,
        }
    }

    fn start_history_prefetch(&mut self, pane: PaneId) {
        if !self.history_loads.is_empty() || !self.history_replays.is_empty() {
            return;
        }
        let Some(before) = self.history_prefetch.claim(&self.history) else {
            return;
        };
        debug_assert!(self.history_loads.is_empty());
        debug_assert!(self.history_replays.is_empty());
        let client = self.client.clone();
        let agent_id = self.agent_id.clone();
        let generation = self.history_generation;
        self.history_loads.spawn(async move {
            let result = client
                .history(&agent_id, Some(&before), HISTORY_PAGE_SIZE)
                .await;
            (pane, agent_id, generation, before, result)
        });
    }

    fn start_requested_history_replay(&mut self, pane: PaneId) {
        if !self.history_replays.is_empty() {
            return;
        }
        let Some((requested_before, page)) = self
            .history_prefetch
            .take_requested(self.history.before.as_deref())
        else {
            return;
        };
        let coherent_tail = self
            .history
            .events
            .iter()
            .any(|event| matches!(event.data, ManagedEventData::TurnAccepted { .. }));
        let sequences = self.history_sequences.clone();
        let history_records = self.history_records.clone();
        let live_records = self.live_records.clone();
        let next_sequence = self.sequence;
        let workspace = self.workspace.clone();
        let effort = effort_from_thinking(self.settings.thinking);
        let agent_id = self.agent_id.clone();
        let generation = self.history_generation;
        self.history_replays.spawn_blocking(move || {
            let result = prepare_history_replay(
                page,
                sequences,
                next_sequence,
                history_records,
                live_records,
                coherent_tail,
                &agent_id,
                &workspace,
                effort,
            );
            (pane, agent_id, generation, requested_before, result)
        });
    }

    fn finish_history_replay(
        &mut self,
        pane: PaneId,
        result: Result<PreparedHistoryReplay, ManagedError>,
    ) -> Result<Box<RestoredSessionProjection>, ManagedError> {
        let mut prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                // The requested page has already left the prefetch queue. Every later buffered
                // page depends on its cursor, so none of them can be reached from the unchanged
                // history window after a projection failure.
                self.history_prefetch.reset();
                self.start_history_prefetch(pane);
                return Err(error);
            }
        };
        self.history.prepend_window(prepared.older_history);
        self.history_sequences = prepared.sequences;
        self.sequence = self.sequence.max(prepared.next_sequence);
        self.next_turn = self.next_turn.max(self.sequence);
        self.history_records = prepared.history_records;
        self.recent_prompts.append(&mut prepared.older_prompts);
        self.start_history_prefetch(pane);
        Ok(prepared.projection)
    }

    fn local_record(&mut self, event: LocalEvent) -> Result<Arc<TranscriptRecord>, ManagedError> {
        let record =
            TranscriptRecord::from_local(self.sequence, unix_ms(), event).map_err(|error| {
                ManagedError::Configuration(format!("TUI transcript error: {error}"))
            })?;
        self.sequence = self.sequence.saturating_add(1);
        let record = Arc::new(record);
        self.live_records.push(Arc::clone(&record));
        Ok(record)
    }

    fn start_submission(&mut self, pane: PaneId, id: TurnId, prompt: Submission) {
        let prompt = inject_shell_context(&mut self.shell_context, prompt);
        if !self.settings_updates.is_empty() || !self.settings_queue.is_empty() {
            self.pending_submission = Some((pane, id, prompt));
            return;
        }
        let Some(agent) = self.agent.clone() else {
            self.pending_submission = Some((pane, id, prompt));
            if self.connection.is_empty()
                && let Some(target) = self.retry_target.take()
            {
                self.spawn_connection(ConnectionPurpose::Startup, target);
            }
            return;
        };
        let managed_request_id = uuid::Uuid::now_v7().to_string();
        self.local_managed_turns
            .insert(id, managed_request_id.clone());
        self.admitting.insert(id);
        self.admissions.spawn(async move {
            let turn = agent
                .prompt(PromptRequest::new(prompt.agent_prompt()).request_id(managed_request_id))
                .await;
            (pane, id, turn)
        });
    }

    fn queue_settings(&mut self, pane: PaneId, mutation: SettingsMutation) {
        self.settings_queue
            .push_back((pane, self.agent_id.clone(), mutation));
        self.start_next_settings_update();
    }

    fn start_next_settings_update(&mut self) {
        if !self.settings_updates.is_empty() {
            return;
        }
        let Some((pane, agent_id, mutation)) = self.settings_queue.pop_front() else {
            return;
        };
        let client = self.client.clone();
        self.settings_updates.spawn(async move {
            let result = match mutation {
                SettingsMutation::Complete(settings) => {
                    client.set_settings(&agent_id, settings).await
                }
                SettingsMutation::Thinking(thinking) => {
                    client.set_thinking(&agent_id, thinking).await
                }
                SettingsMutation::FastMode(enabled) => {
                    client.set_fast_mode(&agent_id, enabled).await
                }
            };
            (pane, agent_id, mutation.failure_subject(), result)
        });
    }

    fn spawn_connection(&mut self, purpose: ConnectionPurpose, target: RetryTarget) {
        if matches!(purpose, ConnectionPurpose::Startup) {
            self.retry_target = Some(target.clone());
        }
        let client = self.client.clone();
        let (agent_id, settings) = match target {
            RetryTarget::Create(settings) => (None, settings),
            RetryTarget::Agent(agent_id) => (Some(agent_id), AgentSettings::default()),
        };
        self.connection.spawn(async move {
            ConnectionResult::Agent {
                purpose,
                result: connect_agent(client, agent_id, settings).await,
            }
        });
    }

    fn idle(&self) -> bool {
        self.controls.is_empty()
            && self.managed_active_turns.ids.is_empty()
            && self.admissions.is_empty()
            && self.completions.is_empty()
            && self.steers.is_empty()
            && self.waiting_steers.is_empty()
            && self.cancellations.is_empty()
            && self.settings_updates.is_empty()
            && self.settings_queue.is_empty()
            && self.active_shells == 0
            && self.pending_submission.is_none()
            && self.cancel_after_admission.is_empty()
            && !self.cancellation_fences.has_in_flight()
            && self.connection.is_empty()
    }

    fn cancel_local_turns(&mut self, pane: PaneId, turns: Vec<(TurnId, String)>) {
        for (id, turn_id) in turns {
            if !self.cancellation_fences.begin_local(id) {
                continue;
            }
            self.spawn_cancellation(
                pane,
                CancelTarget::Local {
                    generation: self.connection_generation,
                    agent_id: self.agent_id.clone(),
                    id,
                    turn_id,
                },
            );
        }
    }

    fn cancel_managed_turns(&mut self, pane: PaneId, turn_ids: Vec<String>) {
        for turn_id in turn_ids {
            if !self.cancellation_fences.begin_managed(&turn_id) {
                continue;
            }
            self.spawn_cancellation(
                pane,
                CancelTarget::Managed {
                    generation: self.connection_generation,
                    agent_id: self.agent_id.clone(),
                    turn_id,
                },
            );
        }
    }

    fn spawn_cancellation(&mut self, pane: PaneId, target: CancelTarget) {
        let client = self.client.clone();
        self.cancellations.spawn(async move {
            let outcome = {
                let agent_id = target.agent_id();
                let turn_id = target.turn_id();
                client
                    .cancel(agent_id, turn_id)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|action| cancel_disposition(turn_id, &action.turn_id, &action.state))
            };
            (pane, target, outcome)
        });
    }
}

fn take_waiting_steer_failures(
    waiting: &mut VecDeque<WaitingSteer>,
) -> Vec<(PaneId, components::QueueId)> {
    // Queue failure recovery reinserts each item at the front of the ready lane,
    // so notify newest-first to preserve the user's original FIFO order.
    waiting
        .drain(..)
        .rev()
        .map(|(pane, id, _)| (pane, id))
        .collect()
}

async fn connect_agent(
    client: ManagedClient,
    agent_id: Option<String>,
    create_settings: AgentSettings,
) -> Result<ConnectedAgent, ConnectionFailure> {
    let created = agent_id.is_none();
    let (managed_event_sender, managed_events) = mpsc::unbounded_channel();
    let (opened, history, history_before, retry, settings, active_turns) = match agent_id {
        None => {
            let opened = super::open_workspace_agent_with_settings(
                &client,
                None,
                None,
                create_settings,
                Some(managed_event_sender),
            )
            .await;
            (
                opened,
                None,
                None,
                RetryTarget::Create(create_settings),
                create_settings,
                ManagedActiveTurns::default(),
            )
        }
        Some(agent_id) => {
            let state = client
                .state(&agent_id)
                .await
                .map_err(|error| ConnectionFailure {
                    error,
                    retry: RetryTarget::Agent(agent_id.clone()),
                })?;
            let cursor =
                EventCursor::parse(state.latest_event_cursor.clone()).map_err(|error| {
                    ConnectionFailure {
                        error,
                        retry: RetryTarget::Agent(agent_id.clone()),
                    }
                })?;
            let settings = state.settings;
            let active_turns = ManagedActiveTurns::from_state(&state);
            let opening = super::open_workspace_agent_from(
                &client,
                Some(agent_id.clone()),
                Some(state),
                Some(managed_event_sender),
            );
            let before = decimal_successor(cursor.as_str());
            let history = async {
                let page = client
                    .history(&agent_id, Some(&before), HISTORY_PAGE_SIZE)
                    .await?;
                HistoryWindow::from_page(before.clone(), page)
            };
            let (opened, history) = tokio::join!(opening, history);
            (
                opened,
                Some(history),
                Some(before),
                RetryTarget::Agent(agent_id),
                settings,
                active_turns,
            )
        }
    };
    let (agent, _events, agent_id, workspace) =
        opened.map_err(|error| ConnectionFailure { error, retry })?;
    let (history, warning) = match history {
        None => (HistoryWindow::default(), None),
        Some(Ok(history)) => (history, None),
        Some(Err(error)) => {
            let before = history_before.expect("existing agents have a history cursor");
            (
                HistoryWindow::retry_from(before),
                Some(format!("Durable event history is unavailable: {error}")),
            )
        }
    };
    Ok((
        agent,
        managed_events,
        agent_id,
        workspace,
        history,
        warning,
        settings,
        created,
        active_turns,
    ))
}

pub(crate) async fn run(
    client: &ManagedClient,
    agent_id: Option<String>,
) -> Result<(), ManagedError> {
    run_inner(client, Some(agent_id)).await
}

pub(crate) async fn run_new(client: &ManagedClient) -> Result<(), ManagedError> {
    run_inner(client, None).await
}

/// `Some(id)` attaches, `Some(None)` opens the in-TUI picker, and `None` creates.
async fn run_inner(
    client: &ManagedClient,
    attach: Option<Option<String>>,
) -> Result<(), ManagedError> {
    let workspace = HostConfig::load()
        .map_err(|error| ManagedError::Configuration(error.to_string()))?
        .workspace()
        .to_path_buf();
    let initial_settings = if matches!(attach, Some(Some(_))) {
        AgentSettings::default()
    } else {
        new_agent_settings()
    };
    let initial_effort = effort_from_thinking(initial_settings.thinking);
    let initial_reasoning_mode = reasoning_mode_from_managed(initial_settings.reasoning_mode);
    let mut root = RootNode::new(&workspace, initial_effort);
    root.set_fork_available(false);
    root.set_reasoning_modes(initial_reasoning_mode, initial_reasoning_mode);
    root.set_fast_mode(initial_settings.fast_mode);
    root.set_model(initial_settings.model);

    let mut theme = Theme::default();
    if let Some(scheme) = detect_system_scheme() {
        theme.set_system_scheme(scheme);
    }
    let mut app = AppNode::new(theme, workspace.clone(), root);
    let mut terminal = TerminalSession::enter().map_err(terminal_error)?;
    let mut input = EventStream::new();
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut runtime = DriverRuntime {
        client: client.clone(),
        agent: None,
        managed_events: None,
        managed_events_open: false,
        connection_generation: 0,
        agent_id: String::new(),
        settings: initial_settings,
        pending_settings: None,
        workspace: workspace.clone(),
        sequence: 1,
        next_turn: 1,
        next_shell: 1,
        controls: HashMap::new(),
        local_managed_turns: HashMap::new(),
        managed_active_turns: ManagedActiveTurns::default(),
        admitting: HashSet::new(),
        cancel_after_admission: HashSet::new(),
        cancellation_fences: CancellationFences::default(),
        cancellation_failed: false,
        cancellation_had_effect: false,
        admissions: JoinSet::new(),
        completions: JoinSet::new(),
        steers: JoinSet::new(),
        waiting_steers: VecDeque::new(),
        cancellations: JoinSet::new(),
        settings_updates: JoinSet::new(),
        settings_queue: VecDeque::new(),
        shells: JoinSet::new(),
        history_loads: JoinSet::new(),
        history_replays: JoinSet::new(),
        history_prefetch: HistoryPrefetch::default(),
        history_generation: 0,
        history: HistoryWindow::default(),
        history_sequences: HashMap::new(),
        history_records: Vec::new(),
        live_records: Vec::new(),
        active_shells: 0,
        shell_context: Vec::new(),
        pending_submission: None,
        recent_prompts: Vec::new(),
        connection: JoinSet::new(),
        retry_target: None,
    };
    // Put the complete interface on screen before any managed request starts.
    terminal
        .draw(|frame| app.render(frame))
        .map_err(terminal_error)?;
    scheduler.presented(Instant::now());
    match attach {
        Some(None) => {
            let update = app.open_resume_selector();
            let _ = apply_update(
                update,
                &mut app,
                &mut runtime,
                &mut terminal,
                &mut scheduler,
            )
            .await?;
        }
        Some(Some(agent_id)) => {
            runtime.spawn_connection(ConnectionPurpose::Startup, RetryTarget::Agent(agent_id));
        }
        None => {
            runtime.spawn_connection(
                ConnectionPurpose::Startup,
                RetryTarget::Create(initial_settings),
            );
        }
    }
    let mut stopping = false;

    while !stopping {
        // Keep runtime and component state stable while the pure replay projection runs. Events
        // remain queued in their receivers and JoinSets, then are applied once after the replay is
        // installed; rebuilding the entire Root for every arrival would duplicate work, while
        // applying already-observed records again would duplicate component side effects.
        if !runtime.history_replays.is_empty() {
            let Some(result) = runtime.history_replays.join_next().await else {
                continue;
            };
            match result {
                Err(error) => {
                    runtime.history_prefetch.reset();
                    runtime.start_history_prefetch(PaneId::Main);
                    request_render(
                        app.update(AppEvent::NotifyError {
                            pane: PaneId::Main,
                            error: format!(
                                "Older durable history replay task stopped unexpectedly: {error}"
                            ),
                        }),
                        &mut scheduler,
                    );
                }
                Ok((pane, agent_id, generation, requested_before, result))
                    if history_replay_matches(
                        &agent_id,
                        generation,
                        &requested_before,
                        &runtime.agent_id,
                        runtime.history_generation,
                        runtime.history.before.as_deref(),
                    ) =>
                {
                    match runtime.finish_history_replay(pane, result) {
                        Err(error) => {
                            request_render(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!(
                                        "Could not replay older durable history: {error}"
                                    ),
                                }),
                                &mut scheduler,
                            );
                        }
                        Ok(projection) => {
                            request_render(
                                app.update(AppEvent::HistoryReplayed { pane, projection }),
                                &mut scheduler,
                            );
                        }
                    }
                }
                Ok(_) => {}
            }
            continue;
        }

        if scheduler.is_due(Instant::now()) {
            terminal
                .draw(|frame| app.render(frame))
                .map_err(terminal_error)?;
            scheduler.presented(Instant::now());
        }

        let render_deadline = scheduler.deadline();
        let animation_deadline = app.animation_deadline();
        tokio::select! {
            input_event = input.next() => {
                let event = input_event
                    .transpose()
                    .map_err(terminal_error)?
                    .ok_or_else(|| terminal_error(io::Error::new(io::ErrorKind::UnexpectedEof, "terminal input closed")))?;
                let refresh_cursor = matches!(&event, Event::FocusGained | Event::Mouse(_));
                if refresh_cursor {
                    terminal.invalidate_cursor_visibility();
                }
                let update = if is_image_paste(&event)
                    && let Some(data) = clipboard::image_data_url()
                {
                    app.update(AppEvent::PasteImage(data))
                } else {
                    app.update(AppEvent::Terminal(event))
                };
                stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
            }
            event = async {
                match runtime.managed_events.as_mut() {
                    Some(events) => events.recv().await,
                    None => pending().await,
                }
            }, if runtime.managed_events_open => {
                match event {
                    Some(event) => {
                        match &event.data {
                            ManagedEventData::TurnCompleted { id, .. }
                            | ManagedEventData::TurnCancelled { id }
                            | ManagedEventData::TurnFailed { id, .. } => {
                                runtime.cancellation_fences.managed_terminal(id);
                                if let Some(local_id) = runtime
                                    .local_managed_turns
                                    .iter()
                                    .find_map(|(local_id, managed_id)| {
                                        (managed_id == id).then_some(*local_id)
                                    })
                                {
                                    runtime.cancellation_fences.local_terminal(local_id);
                                }
                            }
                            _ => {}
                        }
                        let observation = runtime
                            .managed_active_turns
                            .observe(&event, &runtime.local_managed_turns);
                        if observation.active_changed {
                            request_render(
                                app.update(AppEvent::ManagedActiveTurns {
                                    pane: PaneId::Main,
                                    count: runtime.managed_active_turns.ids.len(),
                                }),
                                &mut scheduler,
                            );
                        }
                        if let Some((record, prompt)) = live_managed_projection(
                            event,
                            &runtime.agent_id,
                            &runtime.workspace,
                            &mut runtime.sequence,
                        )? {
                            runtime.live_records.push(Arc::clone(&record));
                            if let Some(prompt) = prompt {
                                runtime.recent_prompts.insert(0, prompt);
                                runtime.recent_prompts.truncate(100);
                            }
                            let update = if observation.external {
                                app.update(AppEvent::ExternalTranscript {
                                    pane: PaneId::Main,
                                    record,
                                })
                            } else {
                                app.update(AppEvent::Transcript {
                                    pane: PaneId::Main,
                                    record,
                                })
                            };
                            stopping = apply_update(
                                update,
                                &mut app,
                                &mut runtime,
                                &mut terminal,
                                &mut scheduler,
                            )
                            .await?;
                        }
                    }
                    None => {
                        runtime.managed_events_open = false;
                        runtime.managed_active_turns.ids.clear();
                        runtime.managed_active_turns.live_steer = false;
                        runtime.managed_active_turns.live_cancel = false;
                        // Losing the observer does not prove any durable cancellation target
                        // terminated. Keep same-generation requests fenced until their HTTP
                        // response or a replacement connection resolves them.
                        runtime.cancellation_had_effect = false;
                        runtime.cancellation_failed = false;
                        request_render(
                            app.update(AppEvent::AgentStreamClosed(PaneId::Main)),
                            &mut scheduler,
                        );
                    }
                }
            }
            result = runtime.connection.join_next(), if !runtime.connection.is_empty() => {
                if let Some(result) = result {
                    let result = match result {
                        Ok(result) => result,
                        Err(error) => {
                            let message =
                                format!("Managed connection task stopped unexpectedly: {error}");
                            request_render(app.update(AppEvent::NotifyError {
                                pane: PaneId::Main,
                                error: message.clone(),
                            }), &mut scheduler);
                            for (pane, id) in
                                take_waiting_steer_failures(&mut runtime.waiting_steers)
                            {
                                let _ = apply_update(
                                    app.update(AppEvent::SteerFailed { pane, id }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                            }
                            if let Some((pane, id, _)) = runtime.pending_submission.take() {
                                let record = runtime.local_record(LocalEvent::WorkerTurnFinished {
                                    id,
                                    error: Some(message),
                                })?;
                                stopping |= apply_update(
                                    app.update(AppEvent::Transcript { pane, record }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                                stopping |= apply_update(
                                    app.update(AppEvent::WorkerTurnFinished {
                                        pane,
                                        terminal_expected: false,
                                    }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                            }
                            continue;
                        }
                    };
                    match result {
                        ConnectionResult::Sessions { pane, result } => {
                            let update = match result {
                                Ok(list) => app.update(AppEvent::SessionsLoaded {
                                    pane,
                                    sessions: session_summaries(&list, &runtime.workspace),
                                }),
                                Err(error) => app.update(AppEvent::SessionLoadFailed {
                                    pane,
                                    error: format!("Could not load managed sessions: {error}"),
                                }),
                            };
                            stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                        ConnectionResult::Agent { purpose, result: Ok((agent, managed_events, agent_id, workspace, history, warning, settings, created, active_turns)) } => {
                            runtime.retry_target = None;
                            let requested_startup_settings = if created {
                                runtime.pending_settings.take()
                            } else {
                                None
                            };
                            let display_settings = requested_startup_settings.unwrap_or(settings);
                            runtime.history_generation = runtime.history_generation.wrapping_add(1);
                            runtime.history_loads = JoinSet::new();
                            runtime.history_replays = JoinSet::new();
                            runtime.history_prefetch.reset();
                            if !matches!(purpose, ConnectionPurpose::Startup) {
                                runtime.history_sequences.clear();
                                runtime.history_records.clear();
                                runtime.live_records.clear();
                                runtime.sequence = 1;
                            }
                            let (history_records, mut prompts) =
                                match history_projection_with_sequences(
                                    &history.events,
                                    &agent_id,
                                    &workspace,
                                    &mut runtime.history_sequences,
                                    &mut runtime.sequence,
                                ) {
                                    Ok(projection) => projection,
                                    Err(error) => {
                                        request_render(app.update(AppEvent::NotifyError {
                                            pane: PaneId::Main,
                                            error: format!("Durable event history is unavailable: {error}"),
                                        }), &mut scheduler);
                                        (Vec::new(), Vec::new())
                                    }
                                };
                            let mut records = history_records.clone();
                            if matches!(purpose, ConnectionPurpose::Startup) {
                                records.extend(runtime.live_records.iter().cloned());
                                let mut live_prompts = std::mem::take(&mut runtime.recent_prompts);
                                live_prompts.append(&mut prompts);
                                prompts = live_prompts;
                            }
                            if let Some(previous) = runtime.agent.replace(agent) {
                                runtime.connection.spawn(async move {
                                    ConnectionResult::Disconnected(previous.disconnect().await)
                                });
                            }
                            runtime.connection_generation =
                                runtime.connection_generation.wrapping_add(1);
                            runtime.managed_events = Some(managed_events);
                            runtime.managed_events_open = true;
                            runtime.agent_id = agent_id;
                            runtime.settings = settings;
                            runtime.workspace = workspace;
                            runtime.managed_active_turns = active_turns;
                            runtime.cancellation_fences.reset();
                            runtime.cancellation_had_effect = false;
                            runtime.cancellation_failed = false;
                            runtime.next_turn = runtime.next_turn.max(runtime.sequence);
                            runtime.recent_prompts = prompts;
                            runtime.history = history;
                            runtime.history_records = history_records;
                            let pane = match purpose {
                                ConnectionPurpose::Startup => PaneId::Main,
                                ConnectionPurpose::Resume(pane) | ConnectionPurpose::New(pane) => pane,
                            };
                            let update = match purpose {
                                ConnectionPurpose::New(pane) => app.update(AppEvent::NewSessionReady {
                                    pane,
                                    effort: effort_from_thinking(display_settings.thinking),
                                    reasoning_mode: reasoning_mode_from_managed(display_settings.reasoning_mode),
                                    fast_mode: display_settings.fast_mode,
                                    model: display_settings.model,
                                    draft_reset: DraftReset::Clear,
                                    skills: Arc::from([]),
                                }),
                                ConnectionPurpose::Startup
                                    if created && runtime.pending_submission.is_none() =>
                                {
                                    app.update(AppEvent::NewSessionReady {
                                        pane,
                                        effort: effort_from_thinking(display_settings.thinking),
                                        reasoning_mode: reasoning_mode_from_managed(
                                            display_settings.reasoning_mode,
                                        ),
                                        fast_mode: display_settings.fast_mode,
                                        model: display_settings.model,
                                        draft_reset: DraftReset::Preserve,
                                        skills: Arc::from([]),
                                    })
                                }
                                ConnectionPurpose::Startup | ConnectionPurpose::Resume(_) => {
                                    let effort = effort_from_thinking(settings.thinking);
                                    let reasoning_mode =
                                        reasoning_mode_from_managed(settings.reasoning_mode);
                                    let projection = RootNode::project_session(effort, records);
                                    app.update(AppEvent::SessionRestored {
                                        pane,
                                        projection: Box::new(projection),
                                        effort,
                                        reasoning_mode,
                                        preferred_reasoning_mode: reasoning_mode,
                                        fast_mode: settings.fast_mode,
                                        model: settings.model,
                                        skills: Arc::from([]),
                                    })
                                }
                            };
                            request_render(update, &mut scheduler);
                            request_render(
                                app.update(AppEvent::ManagedActiveTurns {
                                    pane,
                                    count: runtime.managed_active_turns.ids.len(),
                                }),
                                &mut scheduler,
                            );
                            if let Some(requested) = requested_startup_settings
                                && requested != settings
                            {
                                runtime.queue_settings(
                                    pane,
                                    SettingsMutation::Complete(requested),
                                );
                            }
                            if let Some(warning) = warning {
                                request_render(app.update(AppEvent::NotifyError {
                                    pane: PaneId::Main,
                                    error: warning,
                                }), &mut scheduler);
                            }
                            if matches!(purpose, ConnectionPurpose::Startup)
                                && runtime.active_shells == 0
                                && let Some((pane, id, prompt)) = runtime.pending_submission.take()
                            {
                                runtime.start_submission(pane, id, prompt);
                            }
                            runtime.start_history_prefetch(pane);
                        }
                        ConnectionResult::Agent { purpose, result: Err(failure) } => {
                            let message = format!("Could not connect to the managed agent: {}", failure.error);
                            if matches!(purpose, ConnectionPurpose::Startup) {
                                runtime.retry_target = Some(failure.retry);
                                if runtime.pending_settings.is_some() {
                                    request_render(
                                        app.update(AppEvent::SettingsHydrated {
                                            pane: PaneId::Main,
                                            effort: effort_from_thinking(
                                                runtime.settings.thinking,
                                            ),
                                            fast_mode: runtime.settings.fast_mode,
                                            model: runtime.settings.model,
                                        }),
                                        &mut scheduler,
                                    );
                                }
                            }
                            let update = match purpose {
                                ConnectionPurpose::Startup => app.update(AppEvent::NotifyError {
                                    pane: PaneId::Main,
                                    error: message.clone(),
                                }),
                                ConnectionPurpose::Resume(pane) => app.update(AppEvent::SessionLoadFailed {
                                    pane,
                                    error: message.clone(),
                                }),
                                ConnectionPurpose::New(pane) => app.update(AppEvent::NewSessionFailed {
                                    pane,
                                    error: message.clone(),
                                }),
                            };
                            request_render(update, &mut scheduler);
                            for (pane, id) in
                                take_waiting_steer_failures(&mut runtime.waiting_steers)
                            {
                                stopping |= apply_update(
                                    app.update(AppEvent::SteerFailed { pane, id }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                            }
                            if matches!(purpose, ConnectionPurpose::Startup)
                                && let Some((pane, id, _)) = runtime.pending_submission.take()
                            {
                                let record = runtime.local_record(LocalEvent::WorkerTurnFinished {
                                    id,
                                    error: Some(message),
                                })?;
                                stopping |= apply_update(
                                    app.update(AppEvent::Transcript { pane, record }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                                stopping |= apply_update(
                                    app.update(AppEvent::WorkerTurnFinished {
                                        pane,
                                        terminal_expected: false,
                                    }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                            }
                        }
                        ConnectionResult::Disconnected(Err(error)) => {
                            request_render(app.update(AppEvent::NotifyError {
                                pane: PaneId::Main,
                                error: format!("Previous managed connection did not detach cleanly: {error}"),
                            }), &mut scheduler);
                        }
                        ConnectionResult::Disconnected(Ok(())) => {}
                    }
                }
            }
            result = runtime.settings_updates.join_next(), if !runtime.settings_updates.is_empty() => {
                if let Some(result) = result {
                    let (pane, agent_id, failure_subject, outcome) = result.map_err(|error| {
                        ManagedError::Configuration(format!("settings task failed: {error}"))
                    })?;
                    if agent_id == runtime.agent_id {
                        match outcome {
                            Ok(settings) => runtime.settings = settings,
                            Err(error) => request_render(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!("Could not {failure_subject}: {error}"),
                                }),
                                &mut scheduler,
                            ),
                        }
                    }
                    runtime.start_next_settings_update();
                    if runtime.settings_updates.is_empty() && runtime.settings_queue.is_empty() {
                        request_render(
                            app.update(AppEvent::SettingsHydrated {
                                pane,
                                effort: effort_from_thinking(runtime.settings.thinking),
                                fast_mode: runtime.settings.fast_mode,
                                model: runtime.settings.model,
                            }),
                            &mut scheduler,
                        );
                        if runtime.active_shells == 0
                            && let Some((pane, id, prompt)) = runtime.pending_submission.take()
                        {
                            runtime.start_submission(pane, id, prompt);
                        }
                    }
                }
            }
            result = runtime.admissions.join_next(), if !runtime.admissions.is_empty() => {
                if let Some(result) = result {
                    let (pane, id, admission) = match result {
                        Ok(result) => result,
                        Err(error) => {
                            for (pane, id) in
                                take_waiting_steer_failures(&mut runtime.waiting_steers)
                            {
                                let _ = apply_update(
                                    app.update(AppEvent::SteerFailed { pane, id }),
                                    &mut app,
                                    &mut runtime,
                                    &mut terminal,
                                    &mut scheduler,
                                )
                                .await?;
                            }
                            return Err(ManagedError::Configuration(format!(
                                "prompt task failed: {error}"
                            )));
                        }
                    };
                    runtime.admitting.remove(&id);
                    let cancelled_after_admission = runtime.cancel_after_admission.remove(&id);
                    let mut updates = Vec::new();
                    match admission {
                        Ok(turn) => {
                            let control = turn.control();
                            if let Some(managed_turn_id) = turn.request_id() {
                                debug_assert_eq!(
                                    runtime.local_managed_turns.get(&id).map(String::as_str),
                                    Some(managed_turn_id),
                                    "managed prompt must preserve its caller-owned request ID"
                                );
                                if runtime.managed_active_turns.remove(managed_turn_id) {
                                    updates.push(app.update(AppEvent::ManagedActiveTurns {
                                        pane,
                                        count: runtime.managed_active_turns.ids.len(),
                                    }));
                                }
                            }
                            runtime.controls.insert(id, control.clone());
                            let record = runtime.local_record(LocalEvent::WorkerTurnAccepted { id })?;
                            updates.push(app.update(AppEvent::Transcript { pane, record }));
                            runtime.completions.spawn(async move { (pane, id, turn.await) });
                            if cancelled_after_admission {
                                for (pane, id) in
                                    take_waiting_steer_failures(&mut runtime.waiting_steers)
                                {
                                    updates.push(app.update(AppEvent::SteerFailed { pane, id }));
                                }
                                let managed_turn_id = runtime
                                    .local_managed_turns
                                    .get(&id)
                                    .expect("admitted managed turn must retain its request ID")
                                    .clone();
                                runtime.cancel_local_turns(pane, vec![(id, managed_turn_id)]);
                            } else {
                                let local_turn_id = id;
                                while let Some((pane, id, prompt)) =
                                    runtime.waiting_steers.pop_front()
                                {
                                    let control = control.clone();
                                    let generation = runtime.connection_generation;
                                    let target = SteerTarget::Local(local_turn_id);
                                    runtime.steers.spawn(async move {
                                        let result = control
                                            .steer(prompt.agent_prompt())
                                            .await
                                            .map_err(SteerFailure::backend);
                                        (pane, id, generation, target, result)
                                    });
                                }
                            }
                        }
                        Err(error) => {
                            runtime.local_managed_turns.remove(&id);
                            let record = runtime.local_record(LocalEvent::WorkerTurnFinished {
                                id,
                                error: Some(error.to_string()),
                            })?;
                            updates.push(app.update(AppEvent::Transcript { pane, record }));
                            updates.push(app.update(AppEvent::WorkerTurnFinished { pane, terminal_expected: false }));
                            for (pane, id) in
                                take_waiting_steer_failures(&mut runtime.waiting_steers)
                            {
                                updates.push(app.update(AppEvent::SteerFailed { pane, id }));
                            }
                        }
                    }
                    for update in updates {
                        stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                    }
                    if cancelled_after_admission
                        && runtime.cancel_after_admission.is_empty()
                        && runtime.cancellations.is_empty()
                        && !runtime.cancellation_fences.has_in_flight()
                    {
                        let update = if runtime.cancellation_failed {
                            runtime.cancellation_failed = false;
                            app.update(AppEvent::NotifyError {
                                pane,
                                error: "One or more managed cancellation requests failed."
                                    .to_owned(),
                            })
                        } else {
                            app.update(AppEvent::TurnsCancelled(pane))
                        };
                        stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                    }
                }
            }
            result = runtime.completions.join_next(), if !runtime.completions.is_empty() => {
                if let Some(result) = result {
                    let (pane, id, outcome) = result.map_err(|error| ManagedError::Configuration(format!("turn task failed: {error}")))?;
                    runtime.controls.remove(&id);
                    runtime.local_managed_turns.remove(&id);
                    runtime.cancellation_fences.local_terminal(id);
                    let error = outcome.err().map(|error| error.to_string());
                    let record = runtime.local_record(LocalEvent::WorkerTurnFinished { id, error })?;
                    request_render(app.update(AppEvent::Transcript { pane, record }), &mut scheduler);
                    let update = app.update(AppEvent::WorkerTurnFinished { pane, terminal_expected: true });
                    stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                }
            }
            result = runtime.steers.join_next(), if !runtime.steers.is_empty() => {
                if let Some(result) = result {
                    let (pane, id, generation, target, outcome) = result.map_err(|error| ManagedError::Configuration(format!("steer task failed: {error}")))?;
                    let update = match runtime.resolve_steer(generation, &target, outcome) {
                        SteerResolution::Admitted => app.update(AppEvent::SteerAdmitted { pane, id }),
                        SteerResolution::Failed(Some(error)) => {
                            request_render(app.update(AppEvent::NotifyError { pane, error: format!("Could not steer turn: {error}") }), &mut scheduler);
                            app.update(AppEvent::SteerFailed { pane, id })
                        }
                        SteerResolution::Failed(None) => app.update(AppEvent::SteerFailed { pane, id }),
                        SteerResolution::Stale => continue,
                    };
                    stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                }
            }
            result = runtime.cancellations.join_next(), if !runtime.cancellations.is_empty() => {
                if let Some(result) = result {
                    let (pane, target, outcome) = result.map_err(|error| ManagedError::Configuration(format!("cancel task failed: {error}")))?;
                    let resolution = runtime.finish_cancellation(target, outcome);
                    let final_error = match resolution {
                        CancellationResolution::Accepted => {
                            runtime.cancellation_had_effect = true;
                            let record = runtime.local_record(
                                LocalEvent::WorkerTurnsInterrupted {
                                    count: 1,
                                    error: None,
                                },
                            )?;
                            request_render(
                                app.update(AppEvent::Transcript { pane, record }),
                                &mut scheduler,
                            );
                            None
                        }
                        CancellationResolution::Failed(error) => {
                            runtime.cancellation_failed = true;
                            let record = runtime.local_record(
                                LocalEvent::WorkerTurnsInterrupted {
                                    count: 0,
                                    error: Some(error.clone()),
                                },
                            )?;
                            request_render(
                                app.update(AppEvent::Transcript { pane, record }),
                                &mut scheduler,
                            );
                            Some(error)
                        }
                        CancellationResolution::Stale => None,
                    };
                    if runtime.cancel_after_admission.is_empty()
                        && runtime.cancellations.is_empty()
                        && !runtime.cancellation_fences.has_in_flight()
                    {
                        let update = if runtime.cancellation_failed {
                            runtime.cancellation_failed = false;
                            Some(app.update(AppEvent::NotifyError {
                                pane,
                                error: final_error.unwrap_or_else(|| {
                                    "One or more managed cancellation requests failed.".to_owned()
                                }),
                            }))
                        } else if runtime.cancellation_had_effect {
                            Some(app.update(AppEvent::TurnsCancelled(pane)))
                        } else {
                            None
                        };
                        runtime.cancellation_had_effect = false;
                        if let Some(update) = update {
                            stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                    }
                }
            }
            result = runtime.shells.join_next(), if !runtime.shells.is_empty() => {
                if let Some(result) = result {
                    let (pane, execution) = result.map_err(|error| ManagedError::Configuration(format!("shell task failed: {error}")))?;
                    runtime.active_shells = runtime.active_shells.saturating_sub(1);
                    runtime.shell_context.push(execution.model_context());
                    let record = runtime.local_record(LocalEvent::ShellFinished {
                        id: execution.id,
                        output: execution.output,
                        exit_code: execution.exit_code,
                        duration_ns: execution.duration_ns,
                        truncated: execution.truncated,
                        error: execution.error,
                    })?;
                    request_render(app.update(AppEvent::Transcript { pane, record }), &mut scheduler);
                    request_render(app.update(AppEvent::ShellFinished(pane)), &mut scheduler);
                    if runtime.active_shells == 0
                        && let Some((pane, id, prompt)) = runtime.pending_submission.take()
                    {
                        runtime.start_submission(pane, id, prompt);
                    }
                }
            }
            result = runtime.history_loads.join_next(), if !runtime.history_loads.is_empty() => {
                if let Some(result) = result {
                    match result {
                        Err(error) => {
                            runtime.history_prefetch.reset();
                            request_render(app.update(AppEvent::NotifyError {
                                pane: PaneId::Main,
                                error: format!("Older durable history task stopped unexpectedly: {error}"),
                            }), &mut scheduler);
                        }
                        Ok((pane, agent_id, generation, requested_before, result))
                            if agent_id == runtime.agent_id
                                && generation == runtime.history_generation
                                && runtime.history_prefetch.owns(&requested_before) => match result {
                            Err(error) => {
                                let _ = runtime.history_prefetch.fail(&requested_before);
                                request_render(app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!("Could not load older durable history: {error}"),
                                }), &mut scheduler);
                            }
                            Ok(page) => {
                                if let Err(error) = runtime
                                    .history_prefetch
                                    .store(&requested_before, page)
                                {
                                    let _ = runtime.history_prefetch.fail(&requested_before);
                                    request_render(app.update(AppEvent::NotifyError {
                                        pane,
                                        error: format!("Could not buffer older durable history: {error}"),
                                    }), &mut scheduler);
                                } else {
                                    runtime.start_requested_history_replay(pane);
                                    runtime.start_history_prefetch(pane);
                                }
                            }
                            },
                        Ok(_) => {}
                    }
                }
            }
            () = wait_until(render_deadline), if render_deadline.is_some() => {}
            () = wait_until(animation_deadline), if animation_deadline.is_some() => {
                let update = app.update(AppEvent::AnimationFrame(Instant::now()));
                stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
            }
        }
    }

    drop(terminal);
    let Some(agent) = runtime.agent.take() else {
        return Ok(());
    };
    if runtime.idle() {
        agent.shutdown().await.map_err(super::agent_error)
    } else {
        agent.disconnect().await.map_err(super::agent_error)
    }
}

fn new_agent_settings() -> AgentSettings {
    AgentSettings {
        model: Model::Astra,
        thinking: Thinking::High,
        reasoning_mode: ManagedReasoningMode::Standard,
        fast_mode: false,
    }
}

async fn apply_update(
    update: ComponentUpdate<AppEffect>,
    app: &mut AppNode,
    runtime: &mut DriverRuntime,
    terminal: &mut TerminalSession,
    scheduler: &mut RenderScheduler,
) -> Result<bool, ManagedError> {
    let mut effects = VecDeque::from(update.effects);
    request_render_only(update.render, scheduler);
    let mut stopping = false;
    while let Some(effect) = effects.pop_front() {
        match effect {
            AppEffect::Shutdown => stopping = true,
            AppEffect::SetTheme(_) => scheduler.request_immediate(Instant::now()),
            AppEffect::OpenFork { pane, .. } => {
                absorb(
                    app.update(AppEvent::ForkFailed {
                        pane,
                        error: "Hosted agents do not expose client-side forks.".to_owned(),
                    }),
                    &mut effects,
                    scheduler,
                );
            }
            AppEffect::ClosePane(_) => {}
            AppEffect::Pane { pane, effect } => {
                // Keep the hosted effect boundary visually separate from app-level routing.
                match effect {
                    RootEffect::Submit(prompt) | RootEffect::ContinueSubagent(prompt) => {
                        let id = TurnId::new(runtime.next_turn);
                        runtime.next_turn = runtime.next_turn.saturating_add(1);
                        if runtime.active_shells == 0 {
                            runtime.start_submission(pane, id, prompt);
                        } else {
                            runtime.pending_submission = Some((pane, id, prompt));
                        }
                    }
                    RootEffect::RunShell(command) => {
                        let id = ShellId::new(runtime.next_shell);
                        runtime.next_shell = runtime.next_shell.saturating_add(1);
                        runtime.active_shells = runtime.active_shells.saturating_add(1);
                        let record = runtime.local_record(LocalEvent::ShellStarted {
                            id,
                            command: command.clone(),
                            workspace: runtime.workspace.clone(),
                        })?;
                        absorb(
                            app.update(AppEvent::Transcript { pane, record }),
                            &mut effects,
                            scheduler,
                        );
                        let workspace = runtime.workspace.clone();
                        runtime.shells.spawn(async move {
                            (pane, shell::execute(id, command, workspace).await)
                        });
                    }
                    RootEffect::Steer { id, prompt } => {
                        if let Some((turn_id, control)) = runtime
                            .controls
                            .iter()
                            .next()
                            .map(|(turn_id, control)| (*turn_id, control.clone()))
                        {
                            let generation = runtime.connection_generation;
                            let target = SteerTarget::Local(turn_id);
                            runtime.steers.spawn(async move {
                                let result = control
                                    .steer(prompt.agent_prompt())
                                    .await
                                    .map_err(SteerFailure::backend);
                                (pane, id, generation, target, result)
                            });
                        } else if !runtime.managed_active_turns.ids.is_empty() {
                            let target = runtime
                                .managed_active_turns
                                .steer_target()
                                .map(ToOwned::to_owned);
                            let outcome = match target {
                                Ok(turn_id) => {
                                    let client = runtime.client.clone();
                                    let agent_id = runtime.agent_id.clone();
                                    let input = prompt.managed_prompt();
                                    let generation = runtime.connection_generation;
                                    let target = SteerTarget::Managed {
                                        agent_id: agent_id.clone(),
                                        turn_id: turn_id.clone(),
                                    };
                                    runtime.steers.spawn(async move {
                                        let result = client
                                            .steer(&agent_id, &turn_id, &input)
                                            .await
                                            .map_err(SteerFailure::managed)
                                            .and_then(|action| {
                                                (action.turn_id == turn_id).then_some(()).ok_or_else(
                                                    || {
                                                        SteerFailure::Other("managed steer acknowledged a different turn".to_owned())
                                                    },
                                                )
                                            });
                                        (pane, id, generation, target, result)
                                    });
                                    Ok(())
                                }
                                Err(error) => Err(error.to_owned()),
                            };
                            if let Err(error) = outcome {
                                absorb(
                                    app.update(AppEvent::NotifyError {
                                        pane,
                                        error: format!("Could not steer turn: {error}"),
                                    }),
                                    &mut effects,
                                    scheduler,
                                );
                                absorb(
                                    app.update(AppEvent::SteerFailed { pane, id }),
                                    &mut effects,
                                    scheduler,
                                );
                            }
                        } else if !runtime.admitting.is_empty()
                            || runtime.pending_submission.is_some()
                        {
                            runtime.waiting_steers.push_back((pane, id, prompt));
                        } else {
                            let turn_id = TurnId::new(runtime.next_turn);
                            runtime.next_turn = runtime.next_turn.saturating_add(1);
                            absorb(
                                app.update(AppEvent::SteerPromoted { pane, id }),
                                &mut effects,
                                scheduler,
                            );
                            runtime.start_submission(pane, turn_id, prompt);
                        }
                    }
                    RootEffect::PersistSteer(text) => {
                        let record = runtime.local_record(LocalEvent::UserSteered { text })?;
                        absorb(
                            app.update(AppEvent::Transcript { pane, record }),
                            &mut effects,
                            scheduler,
                        );
                    }
                    RootEffect::CancelTurns => {
                        if runtime.cancellations.is_empty()
                            && !runtime.cancellation_fences.has_in_flight()
                        {
                            runtime.cancellation_failed = false;
                            runtime.cancellation_had_effect = false;
                        }
                        let waiting_steers =
                            take_waiting_steer_failures(&mut runtime.waiting_steers);
                        runtime.cancellation_had_effect |= !waiting_steers.is_empty();
                        for (steer_pane, id) in waiting_steers {
                            absorb(
                                app.update(AppEvent::SteerFailed {
                                    pane: steer_pane,
                                    id,
                                }),
                                &mut effects,
                                scheduler,
                            );
                        }
                        if let Some((pending_pane, id, _)) = runtime.pending_submission.take() {
                            runtime.cancellation_had_effect = true;
                            let record = runtime.local_record(LocalEvent::WorkerTurnFinished {
                                id,
                                error: Some("cancelled before managed admission".to_owned()),
                            })?;
                            absorb(
                                app.update(AppEvent::Transcript {
                                    pane: pending_pane,
                                    record,
                                }),
                                &mut effects,
                                scheduler,
                            );
                            absorb(
                                app.update(AppEvent::WorkerTurnFinished {
                                    pane: pending_pane,
                                    terminal_expected: false,
                                }),
                                &mut effects,
                                scheduler,
                            );
                        }
                        runtime
                            .cancel_after_admission
                            .extend(runtime.admitting.iter().copied());
                        let local_turns = runtime
                            .controls
                            .keys()
                            .filter_map(|id| {
                                runtime
                                    .local_managed_turns
                                    .get(id)
                                    .map(|managed_id| (*id, managed_id.clone()))
                            })
                            .collect::<Vec<_>>();
                        runtime.cancel_local_turns(pane, local_turns);
                        if runtime.managed_active_turns.live_cancel {
                            let managed_turns =
                                runtime.managed_active_turns.ids.iter().cloned().collect();
                            runtime.cancel_managed_turns(pane, managed_turns);
                        } else if !runtime.managed_active_turns.ids.is_empty() {
                            runtime.cancellation_failed = true;
                            absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: "This managed agent does not allow live cancellation."
                                        .to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                        }
                        if runtime.cancel_after_admission.is_empty()
                            && runtime.cancellations.is_empty()
                            && !runtime.cancellation_fences.has_in_flight()
                        {
                            absorb(
                                app.update(AppEvent::TurnsCancelled(pane)),
                                &mut effects,
                                scheduler,
                            );
                        }
                    }
                    RootEffect::Copy(text) => {
                        if terminal.copy_to_clipboard(&text).is_err() {
                            let _ = clipboard::copy_text(&text);
                        }
                    }
                    RootEffect::SetTheme(_) => {}
                    RootEffect::LoadSessions(_) => {
                        let client = runtime.client.clone();
                        runtime.connection.spawn(async move {
                            ConnectionResult::Sessions {
                                pane,
                                result: client.list().await,
                            }
                        });
                    }
                    RootEffect::LoadRecentPrompts(_) => {
                        absorb(
                            app.update(AppEvent::RecentPromptsLoaded {
                                pane,
                                session_id: runtime.agent_id.clone(),
                                prompts: runtime.recent_prompts.clone(),
                            }),
                            &mut effects,
                            scheduler,
                        );
                    }
                    RootEffect::LoadOlderHistory => {
                        runtime.history_prefetch.request_replay();
                        runtime.start_requested_history_replay(pane);
                        runtime.start_history_prefetch(pane);
                    }
                    RootEffect::ResumeSession(agent_id) => {
                        if !runtime.idle() {
                            absorb(
                            app.update(AppEvent::SessionLoadFailed {
                                pane,
                                error:
                                    "Finish or interrupt the active work before switching agents."
                                        .to_owned(),
                            }),
                            &mut effects,
                            scheduler,
                        );
                            continue;
                        }
                        let client = runtime.client.clone();
                        runtime.connection.spawn(async move {
                            ConnectionResult::Agent {
                                purpose: ConnectionPurpose::Resume(pane),
                                result: connect_agent(
                                    client,
                                    Some(agent_id),
                                    AgentSettings::default(),
                                )
                                .await,
                            }
                        });
                    }
                    RootEffect::NewSession(model) => {
                        if !runtime.idle() {
                            absorb(
                                app.update(AppEvent::NewSessionFailed {
                                    pane,
                                    error: "Finish or interrupt the active work first.".to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        }
                        let root = app.root(pane).expect("new-session pane must exist");
                        let settings = AgentSettings {
                            model,
                            thinking: thinking_from_effort(root.composer().effort()),
                            reasoning_mode: managed_reasoning_mode(root.preferred_reasoning_mode()),
                            fast_mode: root.composer().fast_mode(),
                        };
                        let client = runtime.client.clone();
                        runtime.connection.spawn(async move {
                            ConnectionResult::Agent {
                                purpose: ConnectionPurpose::New(pane),
                                result: connect_agent(client, None, settings).await,
                            }
                        });
                    }
                    RootEffect::Reflect(prompt) => {
                        let id = TurnId::new(runtime.next_turn);
                        runtime.next_turn = runtime.next_turn.saturating_add(1);
                        let prompt = prompt.prepend_text(
                        "Reflect on this managed conversation and return a concise, actionable report.".to_owned(),
                    );
                        runtime.start_submission(pane, id, prompt);
                    }
                    RootEffect::OpenLink(destination) => open_link(&destination),
                    RootEffect::OpenDraftEditor => {
                        if !runtime.idle() {
                            absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error:
                                        "Finish or interrupt active work before opening $EDITOR."
                                            .to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        }
                        if app
                            .root(pane)
                            .is_some_and(|root| root.composer().has_images())
                        {
                            absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: "$EDITOR is unavailable for drafts containing images."
                                        .to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        }
                        let draft = app
                            .root(pane)
                            .expect("editor pane must exist")
                            .composer()
                            .draft()
                            .to_owned();
                        terminal.suspend().map_err(terminal_error)?;
                        let outcome = editor::edit(&draft, &runtime.workspace).await;
                        terminal.resume().map_err(terminal_error)?;
                        terminal.invalidate_cursor_visibility();
                        app.refresh_terminal_images();
                        match outcome {
                            Ok(editor::EditorOutcome::Updated(draft)) => absorb(
                                app.update(AppEvent::EditorDraft { pane, draft }),
                                &mut effects,
                                scheduler,
                            ),
                            Ok(editor::EditorOutcome::Unchanged) => {
                                scheduler.request_immediate(Instant::now());
                            }
                            Err(error) => absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!("Could not edit draft: {error}"),
                                }),
                                &mut effects,
                                scheduler,
                            ),
                        }
                    }
                    RootEffect::OpenConfigEditor | RootEffect::ReloadConfig => {
                        absorb(
                        app.update(AppEvent::ConfigReloadFailed {
                            pane,
                            error:
                                "Nanocodex2 is configured by the hosted account and environment."
                                    .to_owned(),
                        }),
                        &mut effects,
                        scheduler,
                    );
                    }
                    RootEffect::SetModel(model) => {
                        let root = app.root(pane).expect("model-selection pane must exist");
                        let requested = AgentSettings {
                            model,
                            thinking: thinking_from_effort(root.composer().effort()),
                            reasoning_mode: managed_reasoning_mode(root.preferred_reasoning_mode()),
                            fast_mode: root.composer().fast_mode(),
                        };
                        if runtime.agent.is_none() {
                            runtime.settings = requested;
                            runtime.pending_settings = Some(requested);
                            if let Some(RetryTarget::Create(settings)) =
                                runtime.retry_target.as_mut()
                            {
                                *settings = requested;
                            }
                            continue;
                        }
                        runtime.queue_settings(pane, SettingsMutation::Complete(requested));
                    }
                    RootEffect::SetEffort { effort, .. } => {
                        let thinking = thinking_from_effort(effort);
                        if runtime.agent.is_none() {
                            runtime.settings.thinking = thinking;
                            runtime.pending_settings = Some(runtime.settings);
                            if let Some(RetryTarget::Create(settings)) =
                                runtime.retry_target.as_mut()
                            {
                                settings.thinking = thinking;
                            }
                            continue;
                        }
                        runtime.queue_settings(pane, SettingsMutation::Thinking(thinking));
                    }
                    RootEffect::SetFastMode(enabled) => {
                        if runtime.agent.is_none() {
                            runtime.settings.fast_mode = enabled;
                            runtime.pending_settings = Some(runtime.settings);
                            if let Some(RetryTarget::Create(settings)) =
                                runtime.retry_target.as_mut()
                            {
                                settings.fast_mode = enabled;
                            }
                            continue;
                        }
                        runtime.queue_settings(pane, SettingsMutation::FastMode(enabled));
                    }
                    RootEffect::SetMaxSubagents(_) => {
                        absorb(
                            app.update(AppEvent::NotifyError {
                                pane,
                                error: "Hosted subagent limits are not exposed by this client."
                                    .to_owned(),
                            }),
                            &mut effects,
                            scheduler,
                        );
                    }
                    RootEffect::Handoff => absorb(
                        app.update(AppEvent::HandoffFailed {
                            pane,
                            error: "Hosted handoff is not exposed by this client.".to_owned(),
                        }),
                        &mut effects,
                        scheduler,
                    ),
                    RootEffect::Review { .. } => absorb(
                        app.update(AppEvent::ReviewFailed {
                            pane,
                            error: "Hosted review is not exposed by this client.".to_owned(),
                        }),
                        &mut effects,
                        scheduler,
                    ),
                    RootEffect::CancelReview => absorb(
                        app.update(AppEvent::ReviewCancelled(pane)),
                        &mut effects,
                        scheduler,
                    ),
                    RootEffect::CancelHandoff => absorb(
                        app.update(AppEvent::HandoffCancelled(pane)),
                        &mut effects,
                        scheduler,
                    ),
                    RootEffect::Fork | RootEffect::Shutdown => {
                        unreachable!("application-level effects are mapped by AppNode")
                    }
                }
            }
        }
    }
    Ok(stopping)
}

fn absorb(
    update: ComponentUpdate<AppEffect>,
    effects: &mut VecDeque<AppEffect>,
    scheduler: &mut RenderScheduler,
) {
    effects.extend(update.effects);
    request_render_only(update.render, scheduler);
}

fn request_render(update: ComponentUpdate<AppEffect>, scheduler: &mut RenderScheduler) {
    debug_assert!(update.effects.is_empty());
    request_render_only(update.render, scheduler);
}

fn request_render_only(request: RenderRequest, scheduler: &mut RenderScheduler) {
    match request {
        RenderRequest::None => {}
        RenderRequest::Streaming => scheduler.request_streaming(Instant::now()),
        RenderRequest::Immediate => scheduler.request_immediate(Instant::now()),
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => pending::<()>().await,
    }
}

fn decimal_successor(cursor: &str) -> String {
    let mut digits = cursor.as_bytes().to_vec();
    for digit in digits.iter_mut().rev() {
        if *digit < b'9' {
            *digit += 1;
            return String::from_utf8(digits).expect("decimal cursor remains UTF-8");
        }
        *digit = b'0';
    }
    let mut successor = String::with_capacity(digits.len().saturating_add(1));
    successor.push('1');
    successor.extend(digits.into_iter().map(char::from));
    successor
}

fn cursor_at_or_before(cursor: &str, through: &str) -> bool {
    through == "latest"
        || cursor.len() < through.len()
        || (cursor.len() == through.len() && cursor <= through)
}

fn session_summaries(list: &AgentList, workspace: &Path) -> Vec<SessionSummary> {
    list.data
        .iter()
        .filter_map(|agent_id| {
            let summary = list.summaries.get(agent_id)?;
            let timestamp = if summary.created_at < 10_000_000_000.0 {
                summary.created_at * 1_000.0
            } else {
                summary.created_at
            };
            Some(SessionSummary {
                session_id: agent_id.clone(),
                started_at_unix_ms: timestamp.max(0.0) as u64,
                model: Model::Sol.to_string(),
                effort: ReasoningEffort::Medium,
                reasoning_mode: ReasoningMode::Standard,
                workspace: workspace.to_path_buf(),
                preview: summary.title.clone(),
            })
        })
        .collect()
}

fn inject_shell_context(context: &mut Vec<String>, prompt: Submission) -> Submission {
    if context.is_empty() {
        return prompt;
    }
    let prefix = context.join("\n\n");
    context.clear();
    prompt.prepend_text(prefix)
}

const fn thinking_from_effort(effort: ReasoningEffort) -> Thinking {
    match effort {
        ReasoningEffort::Low => Thinking::Low,
        ReasoningEffort::Medium => Thinking::Medium,
        ReasoningEffort::High => Thinking::High,
        ReasoningEffort::Xhigh => Thinking::Xhigh,
        ReasoningEffort::Max => Thinking::Max,
    }
}

const fn effort_from_thinking(thinking: Thinking) -> ReasoningEffort {
    match thinking {
        Thinking::None | Thinking::Low => ReasoningEffort::Low,
        Thinking::Medium => ReasoningEffort::Medium,
        Thinking::High => ReasoningEffort::High,
        Thinking::Xhigh => ReasoningEffort::Xhigh,
        Thinking::Max => ReasoningEffort::Max,
    }
}

const fn managed_reasoning_mode(mode: ReasoningMode) -> ManagedReasoningMode {
    match mode {
        ReasoningMode::Standard => ManagedReasoningMode::Standard,
        ReasoningMode::Pro => ManagedReasoningMode::Pro,
    }
}

const fn reasoning_mode_from_managed(mode: ManagedReasoningMode) -> ReasoningMode {
    match mode {
        ManagedReasoningMode::Standard => ReasoningMode::Standard,
        ManagedReasoningMode::Pro => ReasoningMode::Pro,
    }
}

fn is_image_paste(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && key.code == KeyCode::Char('v')
                && key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
    )
}

fn open_link(destination: &str) {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return;
    let _ = command.arg(destination).spawn();
}

fn terminal_error(error: io::Error) -> ManagedError {
    ManagedError::Configuration(format!("terminal error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        CancelDisposition, CancelTarget, CancellationFences, CancellationResolution, DriverRuntime,
        HistoryPrefetch, HistoryWindow, ManagedActiveTurns, SteerResolution, SteerTarget,
        cursor_at_or_before, decimal_successor, history_projection,
        history_projection_with_sequences, history_replay_matches, live_managed_projection,
        new_agent_settings, prepare_history_replay, session_summaries, take_waiting_steer_failures,
    };
    use crate::config::ReasoningEffort;
    use crate::tui::{components::QueueId, pane::PaneId, prompt::Submission, transcript::TurnId};
    use nanocodex::Model;
    use nanocodex_managed::{
        AgentList, AgentSettings, AgentSummary, EventHistoryPage, ManagedApiKey, ManagedClient,
        ManagedEvent, ManagedEventData, PromptInput, ReasoningMode as ManagedReasoningMode,
        Thinking,
    };
    use serde_json::{json, value::to_raw_value};
    use std::{
        collections::{BTreeMap, HashMap, HashSet, VecDeque},
        path::Path,
    };
    use tokio::task::JoinSet;

    #[test]
    fn new_agents_select_astra_without_an_entitlement_probe() {
        assert_eq!(
            new_agent_settings(),
            AgentSettings {
                model: Model::Astra,
                thinking: Thinking::High,
                reasoning_mode: ManagedReasoningMode::Standard,
                fast_mode: false,
            }
        );
    }

    fn history_runtime(history: HistoryWindow) -> DriverRuntime {
        let mut history_sequences = HashMap::new();
        let mut sequence = 1;
        let (history_records, recent_prompts) = history_projection_with_sequences(
            &history.events,
            "agent-1",
            Path::new("/workspace"),
            &mut history_sequences,
            &mut sequence,
        )
        .unwrap();
        let api_key =
            ManagedApiKey::parse(format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43)))
                .unwrap();
        DriverRuntime {
            client: ManagedClient::new("http://127.0.0.1:9", api_key).unwrap(),
            agent: None,
            managed_events: None,
            managed_events_open: false,
            connection_generation: 1,
            agent_id: "agent-1".to_owned(),
            settings: AgentSettings::default(),
            pending_settings: None,
            workspace: Path::new("/workspace").to_path_buf(),
            sequence,
            next_turn: sequence,
            next_shell: 1,
            controls: HashMap::new(),
            local_managed_turns: HashMap::new(),
            managed_active_turns: ManagedActiveTurns::default(),
            admitting: HashSet::new(),
            cancel_after_admission: HashSet::new(),
            cancellation_fences: CancellationFences::default(),
            cancellation_failed: false,
            cancellation_had_effect: false,
            admissions: JoinSet::new(),
            completions: JoinSet::new(),
            steers: JoinSet::new(),
            waiting_steers: VecDeque::new(),
            cancellations: JoinSet::new(),
            settings_updates: JoinSet::new(),
            settings_queue: VecDeque::new(),
            shells: JoinSet::new(),
            history_loads: JoinSet::new(),
            history_replays: JoinSet::new(),
            history_prefetch: HistoryPrefetch::default(),
            history_generation: 1,
            history,
            history_sequences,
            history_records,
            live_records: Vec::new(),
            active_shells: 0,
            shell_context: Vec::new(),
            pending_submission: None,
            recent_prompts,
            connection: JoinSet::new(),
            retry_target: None,
        }
    }

    #[test]
    fn accepted_local_cancel_stays_fenced_until_terminal_completion() {
        let mut fences = CancellationFences::default();
        let id = TurnId::new(7);

        assert!(fences.begin_local(id));
        assert_eq!(
            fences.finish_local(id, Ok(CancelDisposition::Accepted), true),
            CancellationResolution::Accepted
        );
        assert!(!fences.begin_local(id));

        fences.local_terminal(id);
        assert!(fences.begin_local(id));
    }

    #[test]
    fn accepted_managed_cancel_stays_fenced_until_terminal_event() {
        let mut fences = CancellationFences::default();

        assert!(fences.begin_managed("turn-7"));
        assert_eq!(
            fences.finish_managed("turn-7".to_owned(), Ok(CancelDisposition::Accepted), true,),
            CancellationResolution::Accepted
        );
        assert!(!fences.begin_managed("turn-7"));

        fences.managed_terminal("turn-7");
        assert!(fences.begin_managed("turn-7"));
    }

    #[test]
    fn terminal_before_cancel_response_makes_late_error_stale() {
        let mut fences = CancellationFences::default();

        assert!(fences.begin_managed("turn-7"));
        assert_eq!(
            fences.finish_managed(
                "turn-7".to_owned(),
                Err("503 Service Unavailable".to_owned()),
                false,
            ),
            CancellationResolution::Stale
        );
        assert!(fences.begin_managed("turn-7"));
    }

    #[test]
    fn stream_close_preserves_same_target_cancel_failure() {
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime.managed_events_open = false;

        assert!(runtime.cancellation_fences.begin_managed("turn-7"));
        assert_eq!(
            runtime.finish_cancellation(
                CancelTarget::Managed {
                    generation: 1,
                    agent_id: "agent-1".to_owned(),
                    turn_id: "turn-7".to_owned(),
                },
                Err("503 Service Unavailable".to_owned()),
            ),
            CancellationResolution::Failed("503 Service Unavailable".to_owned())
        );
    }

    #[test]
    fn replaced_connection_cannot_consume_same_target_cancel_fence() {
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime.connection_generation = 7;
        assert!(runtime.cancellation_fences.begin_managed("turn-7"));

        runtime.connection_generation = 8;
        runtime.cancellation_fences.reset();
        assert!(runtime.cancellation_fences.begin_managed("turn-7"));
        assert_eq!(
            runtime.finish_cancellation(
                CancelTarget::Managed {
                    generation: 7,
                    agent_id: "agent-1".to_owned(),
                    turn_id: "turn-7".to_owned(),
                },
                Err("old connection failed".to_owned()),
            ),
            CancellationResolution::Stale
        );
        assert!(!runtime.cancellation_fences.begin_managed("turn-7"));
    }

    #[test]
    fn terminal_target_preserves_acknowledged_steer() {
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime.managed_active_turns.ids.insert("turn-7".to_owned());
        let target = SteerTarget::Managed {
            agent_id: "agent-1".to_owned(),
            turn_id: "turn-7".to_owned(),
        };

        assert_eq!(
            runtime.resolve_steer(1, &target, Ok(())),
            SteerResolution::Admitted
        );
        assert_eq!(
            runtime.resolve_steer(1, &target, Err(super::SteerFailure::Inactive)),
            SteerResolution::Failed(None)
        );
        runtime.managed_active_turns.ids.remove("turn-7");
        assert_eq!(
            runtime.resolve_steer(1, &target, Ok(())),
            SteerResolution::Admitted
        );
        runtime.connection_generation += 1;
        assert_eq!(
            runtime.resolve_steer(1, &target, Ok(())),
            SteerResolution::Stale
        );
    }

    #[test]
    fn local_managed_terminal_before_cancel_response_makes_late_error_stale() {
        let mut fences = CancellationFences::default();
        let id = TurnId::new(7);

        assert!(fences.begin_local(id));
        fences.local_terminal(id);
        assert_eq!(
            fences.finish_local(id, Err("503 Service Unavailable".to_owned()), true),
            CancellationResolution::Stale
        );
    }

    #[test]
    fn terminal_cancel_action_is_stale_even_before_the_event_arrives() {
        let mut fences = CancellationFences::default();

        assert!(fences.begin_managed("turn-7"));
        assert_eq!(
            fences.finish_managed("turn-7".to_owned(), Ok(CancelDisposition::Terminal), true,),
            CancellationResolution::Stale
        );
    }

    #[test]
    fn first_active_cancel_failure_is_reported_and_retryable() {
        let mut fences = CancellationFences::default();
        let id = TurnId::new(7);

        assert!(fences.begin_local(id));
        assert_eq!(
            fences.finish_local(id, Err("backend unavailable".to_owned()), true),
            CancellationResolution::Failed("backend unavailable".to_owned())
        );
        assert!(fences.begin_local(id));
    }

    #[test]
    fn cancellation_fences_are_scoped_to_the_exact_target() {
        let mut fences = CancellationFences::default();

        assert!(fences.begin_managed("turn-7"));
        assert_eq!(
            fences.finish_managed("turn-7".to_owned(), Ok(CancelDisposition::Accepted), true,),
            CancellationResolution::Accepted
        );
        assert!(!fences.begin_managed("turn-7"));
        assert!(fences.begin_managed("turn-8"));
    }

    #[test]
    fn terminal_failures_drain_waiting_steers_in_queue_safe_order() {
        let mut waiting = VecDeque::from([
            (
                PaneId::Main,
                QueueId::new(7),
                Submission::text("first".to_owned()),
            ),
            (
                PaneId::Main,
                QueueId::new(8),
                Submission::text("second".to_owned()),
            ),
        ]);

        let failures = take_waiting_steer_failures(&mut waiting);

        assert_eq!(
            failures.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
            [QueueId::new(8), QueueId::new(7)]
        );
        assert!(waiting.is_empty());
    }

    #[test]
    fn managed_active_turns_reconcile_cursor_order_idempotently() {
        let mut active = ManagedActiveTurns {
            ids: HashSet::from(["attached-1".to_owned()]),
            order: vec!["attached-1".to_owned()],
            live_steer: true,
            live_cancel: true,
        };
        let accepted = managed_turn("1", "new prompt");

        assert!(active.observe(&accepted, &HashMap::new()).active_changed);
        assert_eq!(
            active.ids,
            HashSet::from(["attached-1".to_owned(), "turn-1".to_owned()])
        );
        assert!(!active.observe(&accepted, &HashMap::new()).active_changed);

        let terminal = ManagedEvent {
            cursor: "2".to_owned(),
            created_at: Some(1_750_000_001.0),
            turn_id: Some("attached-1".to_owned()),
            data: ManagedEventData::TurnCancelled {
                id: "attached-1".to_owned(),
            },
        };
        assert!(active.observe(&terminal, &HashMap::new()).active_changed);
        assert_eq!(active.ids, HashSet::from(["turn-1".to_owned()]));
        assert!(!active.observe(&terminal, &HashMap::new()).active_changed);
    }

    #[test]
    fn managed_active_turns_do_not_claim_known_local_admissions() {
        let mut active = ManagedActiveTurns {
            ids: HashSet::new(),
            order: Vec::new(),
            live_steer: true,
            live_cancel: true,
        };
        let local = HashMap::from([(TurnId::new(1), "turn-1".to_owned())]);

        let observation = active.observe(&managed_turn("1", "local"), &local);
        assert!(!observation.active_changed);
        assert!(!observation.external);
        assert!(active.ids.is_empty());
    }

    #[test]
    fn local_request_fence_survives_terminal_event_before_admission_returns() {
        let mut active = ManagedActiveTurns {
            ids: HashSet::new(),
            order: Vec::new(),
            live_steer: true,
            live_cancel: true,
        };
        let local = HashMap::from([(TurnId::new(1), "turn-1".to_owned())]);
        let accepted = managed_turn("1", "local");
        let failed = ManagedEvent {
            cursor: "2".to_owned(),
            created_at: Some(1_750_000_001.0),
            turn_id: Some("turn-1".to_owned()),
            data: ManagedEventData::TurnFailed {
                id: "turn-1".to_owned(),
                error: "failed before admission returned".to_owned(),
            },
        };

        assert!(!active.observe(&accepted, &local).active_changed);
        let observation = active.observe(&failed, &local);
        assert!(!observation.active_changed);
        assert!(!observation.external);
        assert!(active.ids.is_empty());
    }

    #[test]
    fn external_ownership_is_captured_before_turn_failed_removes_active_id() {
        let mut active = ManagedActiveTurns {
            ids: HashSet::from(["attached-1".to_owned()]),
            order: vec!["attached-1".to_owned()],
            live_steer: true,
            live_cancel: true,
        };
        let failed = ManagedEvent {
            cursor: "2".to_owned(),
            created_at: Some(1_750_000_001.0),
            turn_id: Some("attached-1".to_owned()),
            data: ManagedEventData::TurnFailed {
                id: "attached-1".to_owned(),
                error: "attached failure".to_owned(),
            },
        };

        let observation = active.observe(&failed, &HashMap::new());
        assert!(observation.external);
        assert!(observation.active_changed);
        assert!(!active.ids.contains("attached-1"));
    }

    #[test]
    fn attached_steering_uses_durable_admission_order() {
        let mut active = ManagedActiveTurns {
            ids: HashSet::from(["attached-1".to_owned()]),
            order: vec!["attached-1".to_owned()],
            live_steer: true,
            live_cancel: true,
        };
        assert_eq!(active.steer_target().unwrap(), "attached-1");

        active.ids.insert("attached-2".to_owned());
        active.order.push("attached-2".to_owned());
        assert_eq!(active.steer_target().unwrap(), "attached-1");
        active.remove("attached-1");
        assert_eq!(active.steer_target().unwrap(), "attached-2");
        active.live_steer = false;
        assert_eq!(
            active.steer_target().unwrap_err(),
            "this managed agent does not allow live steering"
        );
    }

    #[test]
    fn managed_history_projects_into_tact_user_and_assistant_records() {
        let agent_event = to_raw_value(&json!({
            "protocol_version": 1,
            "request_id": "request-1",
            "seq": 2,
            "type": "assistant.message",
            "payload": {
                "model_call_index": 0,
                "item_id": null,
                "phase": null,
                "text": "done"
            }
        }))
        .unwrap();
        let history = vec![
            ManagedEvent {
                cursor: "1".to_owned(),
                created_at: Some(1_750_000_000.0),
                turn_id: Some("turn-1".to_owned()),
                data: ManagedEventData::TurnAccepted {
                    id: "turn-1".to_owned(),
                    input: PromptInput::Text("inspect the tree".to_owned()),
                    replayed: false,
                },
            },
            ManagedEvent {
                cursor: "2".to_owned(),
                created_at: Some(1_750_000_001.0),
                turn_id: Some("turn-1".to_owned()),
                data: ManagedEventData::Event {
                    event: agent_event,
                    agent_id: None,
                },
            },
        ];

        let (records, next_sequence, recent) =
            history_projection(history, "agent-1", Path::new("/workspace")).unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(
            (records[0].source(), records[0].kind()),
            ("tact", "user.submitted")
        );
        assert_eq!(
            (records[1].source(), records[1].kind()),
            ("agent", "assistant.message")
        );
        assert_eq!(next_sequence, 3);
        assert_eq!(recent[0].text, "inspect the tree");
    }

    #[test]
    fn live_managed_acceptance_projects_the_remote_user_prompt() {
        let mut next_sequence = 7;
        let (record, prompt) = live_managed_projection(
            managed_turn("42", "sent from another client"),
            "agent-1",
            Path::new("/workspace"),
            &mut next_sequence,
        )
        .unwrap()
        .expect("turn acceptance should project");

        let prompt = prompt.expect("turn acceptance should update prompt history");
        assert_eq!((record.source(), record.kind()), ("tact", "user.submitted"));
        assert_eq!(record.sequence(), 7);
        assert_eq!(prompt.text, "sent from another client");
        assert_eq!(prompt.session_id, "agent-1");
        assert_eq!(prompt.workspace, Path::new("/workspace"));
        assert_eq!(next_sequence, 8);
    }

    #[test]
    fn live_managed_projection_preserves_agent_output_after_the_prompt() {
        let mut next_sequence = 7;
        let event = ManagedEvent {
            cursor: "43".to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: Some("turn-42".to_owned()),
            data: ManagedEventData::Event {
                event: to_raw_value(&json!({
                    "protocol_version": 1,
                    "request_id": "request-1",
                    "seq": 1,
                    "type": "assistant.message",
                    "payload": {
                        "model_call_index": 0,
                        "item_id": null,
                        "phase": null,
                        "text": "done"
                    }
                }))
                .unwrap(),
                agent_id: None,
            },
        };

        let (record, prompt) = live_managed_projection(
            event,
            "agent-1",
            Path::new("/workspace"),
            &mut next_sequence,
        )
        .unwrap()
        .expect("agent output should project");

        assert_eq!(
            (record.source(), record.kind()),
            ("agent", "assistant.message")
        );
        assert!(prompt.is_none());
        assert_eq!(next_sequence, 8);
    }

    #[test]
    fn durable_history_is_fenced_through_the_live_stream_cursor() {
        assert!(cursor_at_or_before("9", "10"));
        assert!(cursor_at_or_before("10", "10"));
        assert!(!cursor_at_or_before("11", "10"));
        assert!(cursor_at_or_before("999", "latest"));
    }

    #[test]
    fn snapshot_cursor_successor_is_unbounded_and_canonical() {
        assert_eq!(decimal_successor("0"), "1");
        assert_eq!(decimal_successor("1299"), "1300");
        assert_eq!(
            decimal_successor("99999999999999999999"),
            "100000000000000000000"
        );
    }

    #[test]
    fn replay_keeps_loaded_event_sequences_stable_when_older_events_arrive() {
        let mut sequences = HashMap::new();
        let mut next_sequence = 1;
        let recent_history = vec![managed_turn("5", "recent")];
        let (recent, _) = history_projection_with_sequences(
            &recent_history,
            "agent-1",
            Path::new("/workspace"),
            &mut sequences,
            &mut next_sequence,
        )
        .unwrap();
        let recent_sequence = recent[0].sequence();

        let replayed_history = vec![managed_turn("3", "older"), managed_turn("5", "recent")];
        let (replayed, _) = history_projection_with_sequences(
            &replayed_history,
            "agent-1",
            Path::new("/workspace"),
            &mut sequences,
            &mut next_sequence,
        )
        .unwrap();

        assert_eq!(replayed[1].sequence(), recent_sequence);
        assert_ne!(replayed[0].sequence(), recent_sequence);
        assert_eq!(next_sequence, 3);
    }

    #[test]
    fn history_replay_fence_requires_exact_agent_generation_and_cursor() {
        assert!(history_replay_matches(
            "agent-1",
            7,
            "42",
            "agent-1",
            7,
            Some("42"),
        ));
        assert!(!history_replay_matches(
            "agent-old",
            7,
            "42",
            "agent-1",
            7,
            Some("42"),
        ));
        assert!(!history_replay_matches(
            "agent-1",
            6,
            "42",
            "agent-1",
            7,
            Some("42"),
        ));
        assert!(!history_replay_matches(
            "agent-1",
            7,
            "41",
            "agent-1",
            7,
            Some("42"),
        ));
    }

    #[test]
    fn history_page_replay_combines_retained_and_live_records_with_disjoint_sequences() {
        let history = HistoryWindow {
            events: vec![managed_turn("5", "retained")],
            before: Some("5".to_owned()),
            has_more: true,
        };
        let mut sequences = HashMap::new();
        let mut next_sequence = 1;
        let (history_records, _) = history_projection_with_sequences(
            &history.events,
            "agent-1",
            Path::new("/workspace"),
            &mut sequences,
            &mut next_sequence,
        )
        .unwrap();
        let (live_before, _) = live_managed_projection(
            managed_turn("6", "before replay"),
            "agent-1",
            Path::new("/workspace"),
            &mut next_sequence,
        )
        .unwrap()
        .unwrap();
        let prepared = match prepare_history_replay(
            EventHistoryPage {
                data: vec![managed_turn("3", "older")],
                has_more: false,
                latest_cursor: "6".to_owned(),
            },
            sequences,
            next_sequence,
            history_records,
            vec![live_before],
            true,
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        ) {
            Ok(prepared) => prepared,
            Err(error) => panic!("history replay failed: {error}"),
        };

        let projected_sequences = prepared
            .history_records
            .iter()
            .map(|record| record.sequence())
            .chain(std::iter::once(2))
            .collect::<Vec<_>>();
        assert_eq!(projected_sequences.len(), 3);
        assert_eq!(
            projected_sequences
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len(),
            projected_sequences.len(),
        );
        assert_eq!(
            prepared
                .older_prompts
                .iter()
                .map(|prompt| prompt.text.as_str())
                .collect::<Vec<_>>(),
            ["older"],
        );
    }

    #[test]
    fn failed_page_projection_restores_window_and_sequence_assignments() {
        let history = HistoryWindow {
            events: vec![managed_turn("5", "retained")],
            before: Some("5".to_owned()),
            has_more: true,
        };
        let sequences = HashMap::from([("5".to_owned(), 1)]);
        let invalid = ManagedEvent {
            cursor: "4".to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: Some("turn-3".to_owned()),
            data: ManagedEventData::Event {
                event: to_raw_value(&json!({ "not": "an agent event" })).unwrap(),
                agent_id: None,
            },
        };

        let error = match prepare_history_replay(
            EventHistoryPage {
                data: vec![managed_turn("3", "older"), invalid],
                has_more: false,
                latest_cursor: "5".to_owned(),
            },
            sequences.clone(),
            2,
            Vec::new(),
            Vec::new(),
            true,
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        ) {
            Ok(_) => panic!("invalid page unexpectedly projected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("invalid retained agent event"));
        assert_eq!(history.before.as_deref(), Some("5"));
        assert!(history.has_more);
        assert_eq!(history.events.len(), 1);
        assert_eq!(history.events[0].cursor, "5");
        assert_eq!(sequences, HashMap::from([("5".to_owned(), 1)]));
    }

    #[tokio::test]
    async fn driver_runtime_consumes_buffered_pages_before_refilling() {
        let mut runtime = history_runtime(HistoryWindow {
            events: vec![managed_turn("9", "retained")],
            before: Some("9".to_owned()),
            has_more: true,
        });
        for (cursor, prompt) in [
            ("7", "older"),
            ("5", "older 2"),
            ("3", "older 3"),
            ("1", "oldest"),
        ] {
            let before = runtime.history_prefetch.claim(&runtime.history).unwrap();
            runtime
                .history_prefetch
                .store(
                    &before,
                    EventHistoryPage {
                        data: vec![managed_turn(cursor, prompt)],
                        has_more: true,
                        latest_cursor: "9".to_owned(),
                    },
                )
                .unwrap();
        }
        assert!(runtime.history_prefetch.claim(&runtime.history).is_none());

        runtime.history_prefetch.request_replay();
        runtime.start_requested_history_replay(PaneId::Main);
        assert_eq!(runtime.history_replays.len(), 1);
        runtime.start_history_prefetch(PaneId::Main);
        assert!(runtime.history_loads.is_empty());

        let (_, _, _, requested_before, result) =
            runtime.history_replays.join_next().await.unwrap().unwrap();
        assert_eq!(requested_before, "9");
        drop(runtime.finish_history_replay(PaneId::Main, result).unwrap());
        assert_eq!(runtime.history.before.as_deref(), Some("7"));
        assert!(runtime.history_prefetch.owns("1"));
        assert_eq!(runtime.history_loads.len(), 1);

        runtime.history_prefetch.request_replay();
        runtime.start_requested_history_replay(PaneId::Main);
        assert_eq!(runtime.history_replays.len(), 1);
        runtime.start_history_prefetch(PaneId::Main);
        assert_eq!(runtime.history_loads.len(), 1);
        let (_, _, _, requested_before, result) =
            runtime.history_replays.join_next().await.unwrap().unwrap();
        assert_eq!(requested_before, "7");
        drop(runtime.finish_history_replay(PaneId::Main, result).unwrap());
        assert_eq!(runtime.history.before.as_deref(), Some("5"));
        assert!(runtime.history_prefetch.owns("1"));
        runtime.history_loads.abort_all();
    }

    #[tokio::test]
    async fn driver_runtime_projection_failure_refetches_from_the_current_cursor() {
        let mut runtime = history_runtime(HistoryWindow {
            events: vec![managed_turn("9", "retained")],
            before: Some("9".to_owned()),
            has_more: true,
        });
        let malformed = ManagedEvent {
            cursor: "8".to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: Some("turn-7".to_owned()),
            data: ManagedEventData::Event {
                event: to_raw_value(&json!({ "not": "an agent event" })).unwrap(),
                agent_id: None,
            },
        };
        let first_before = runtime.history_prefetch.claim(&runtime.history).unwrap();
        runtime
            .history_prefetch
            .store(
                &first_before,
                EventHistoryPage {
                    data: vec![managed_turn("7", "older"), malformed],
                    has_more: true,
                    latest_cursor: "9".to_owned(),
                },
            )
            .unwrap();
        let dependent_before = runtime.history_prefetch.claim(&runtime.history).unwrap();
        runtime
            .history_prefetch
            .store(
                &dependent_before,
                EventHistoryPage {
                    data: vec![managed_turn("5", "unreachable")],
                    has_more: true,
                    latest_cursor: "9".to_owned(),
                },
            )
            .unwrap();

        runtime.history_prefetch.request_replay();
        runtime.start_requested_history_replay(PaneId::Main);
        let (_, _, _, requested_before, result) =
            runtime.history_replays.join_next().await.unwrap().unwrap();
        assert_eq!(requested_before, "9");
        assert!(runtime.finish_history_replay(PaneId::Main, result).is_err());
        assert_eq!(runtime.history.before.as_deref(), Some("9"));
        assert!(runtime.history_prefetch.owns("9"));
        assert_eq!(runtime.history_loads.len(), 1);
        runtime.history_loads.abort_all();
    }

    #[test]
    fn successive_older_pages_project_only_the_new_page_and_keep_sequences_stable() {
        let retained = vec![managed_turn("5", "retained")];
        let mut sequences = HashMap::new();
        let mut next_sequence = 1;
        let (history_records, _) = history_projection_with_sequences(
            &retained,
            "agent-1",
            Path::new("/workspace"),
            &mut sequences,
            &mut next_sequence,
        )
        .unwrap();
        let retained_sequence = history_records[0].sequence();

        let first = prepare_history_replay(
            EventHistoryPage {
                data: vec![managed_turn("3", "older")],
                has_more: true,
                latest_cursor: "5".to_owned(),
            },
            sequences,
            next_sequence,
            history_records,
            Vec::new(),
            true,
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        )
        .unwrap();
        let first_older_sequence = first.history_records[0].sequence();
        let second = prepare_history_replay(
            EventHistoryPage {
                data: vec![managed_turn("1", "oldest")],
                has_more: false,
                latest_cursor: "5".to_owned(),
            },
            first.sequences,
            first.next_sequence,
            first.history_records,
            Vec::new(),
            true,
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        )
        .unwrap();

        assert_eq!(second.history_records.len(), 3);
        assert_eq!(second.history_records[1].sequence(), first_older_sequence);
        assert_eq!(second.history_records[2].sequence(), retained_sequence);
        assert_eq!(
            second
                .older_prompts
                .iter()
                .map(|prompt| prompt.text.as_str())
                .collect::<Vec<_>>(),
            ["oldest"],
        );
    }

    #[test]
    fn older_page_without_a_prompt_is_ignored_after_a_coherent_tail() {
        let retained = vec![managed_turn("5", "retained")];
        let mut sequences = HashMap::new();
        let mut next_sequence = 1;
        let (history_records, _) = history_projection_with_sequences(
            &retained,
            "agent-1",
            Path::new("/workspace"),
            &mut sequences,
            &mut next_sequence,
        )
        .unwrap();
        let partial = ManagedEvent {
            cursor: "4".to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: Some("older-turn".to_owned()),
            data: ManagedEventData::Event {
                event: to_raw_value(&json!({ "not": "an agent event" })).unwrap(),
                agent_id: None,
            },
        };

        let prepared = prepare_history_replay(
            EventHistoryPage {
                data: vec![partial],
                has_more: false,
                latest_cursor: "5".to_owned(),
            },
            sequences,
            next_sequence,
            history_records,
            Vec::new(),
            true,
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        )
        .unwrap();

        assert_eq!(prepared.history_records.len(), 1);
        assert!(prepared.older_prompts.is_empty());
        assert_eq!(prepared.sequences.len(), 1);
    }

    #[test]
    fn replay_ignores_a_partial_turn_before_the_first_loaded_prompt() {
        let partial = ManagedEvent {
            cursor: "4".to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: Some("older-turn".to_owned()),
            data: ManagedEventData::Event {
                event: to_raw_value(&json!({ "not": "an agent event" })).unwrap(),
                agent_id: None,
            },
        };

        let (records, _, recent) = history_projection(
            vec![partial, managed_turn("5", "complete turn")],
            "agent-1",
            Path::new("/workspace"),
        )
        .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(recent[0].text, "complete turn");
    }

    #[test]
    fn history_window_prepends_one_page_and_stops_at_exhaustion() {
        let mut window = HistoryWindow::from_page(
            "7".to_owned(),
            EventHistoryPage {
                data: vec![managed_created("5"), managed_created("6")],
                has_more: true,
                latest_cursor: "6".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(window.before.as_deref(), Some("5"));
        assert!(window.has_more);

        window
            .prepend(EventHistoryPage {
                data: vec![managed_created("3"), managed_created("4")],
                has_more: false,
                latest_cursor: "6".to_owned(),
            })
            .unwrap();

        assert_eq!(
            window
                .events
                .iter()
                .map(|event| event.cursor.as_str())
                .collect::<Vec<_>>(),
            ["3", "4", "5", "6"]
        );
        assert_eq!(window.before.as_deref(), Some("3"));
        assert!(!window.has_more);
    }

    #[test]
    fn empty_nonterminal_history_page_does_not_advance_the_retry_cursor() {
        let mut window = HistoryWindow::retry_from("9".to_owned());
        let result = window.prepend(EventHistoryPage {
            data: Vec::new(),
            has_more: true,
            latest_cursor: "8".to_owned(),
        });

        assert!(result.is_err());
        assert_eq!(window.before.as_deref(), Some("9"));
        assert!(window.has_more);
    }

    fn managed_created(cursor: &str) -> ManagedEvent {
        ManagedEvent {
            cursor: cursor.to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: None,
            data: ManagedEventData::AgentCreated {
                agent_id: "agent-1".to_owned(),
                capabilities: json!({}),
            },
        }
    }

    fn managed_turn(cursor: &str, prompt: &str) -> ManagedEvent {
        ManagedEvent {
            cursor: cursor.to_owned(),
            created_at: Some(1_750_000_000.0),
            turn_id: Some(format!("turn-{cursor}")),
            data: ManagedEventData::TurnAccepted {
                id: format!("turn-{cursor}"),
                input: PromptInput::Text(prompt.to_owned()),
                replayed: false,
            },
        }
    }

    #[test]
    fn managed_agent_ids_remain_the_resume_picker_identity() {
        let list = AgentList {
            data: vec!["agent-1".to_owned()],
            summaries: BTreeMap::from([(
                "agent-1".to_owned(),
                AgentSummary {
                    title: "A durable task".to_owned(),
                    created_at: 1_750_000_000.0,
                    updated_at: 1_750_000_100.0,
                    turn_count: 2,
                },
            )]),
        };

        let sessions = session_summaries(&list, Path::new("/workspace"));

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "agent-1");
        assert_eq!(sessions[0].preview, "A durable task");
        assert_eq!(sessions[0].started_at_unix_ms, 1_750_000_000_000);
    }
}
