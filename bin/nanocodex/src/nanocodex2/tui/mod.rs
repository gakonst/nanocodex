//! Tact-derived terminal presentation adapted to the managed Nanocodex driver.
//!
//! Portions of this module tree derive from clabby/tact at revision
//! e20b1584642339546bb2310aad6968edeec66a53 and are modified for Nanocodex2.
//! They remain available under Apache-2.0. The managed service owns agent
//! orchestration and hosted tools; this module owns only presentation, terminal
//! interaction, and the caller-local shell convenience.

pub(crate) mod backend;
#[cfg(feature = "tui-bench")]
pub(crate) mod bench;
mod btw;
mod bug;
mod clipboard;
mod components;
mod context;
mod control;
mod editor;
pub(crate) mod features;
mod format;
mod history;
mod links;
pub(crate) mod local;
mod notification;
mod pane;
mod private_input;
mod prompt;
mod prompt_cache;
mod review;
mod scheduler;
mod screen;
mod secure_input;
mod session;
mod share;
mod shared;
mod shell;
mod sites;
mod spinner;
mod sudo_input;
mod telemetry;
mod terminal;
mod theme;
mod tmux;
pub(crate) mod tool_calls;
mod transcript;
mod vault;
mod voice_clone;
pub(crate) mod voice_keys;

pub(crate) use self::shared::run_shared;

use self::{
    components::{
        AppEffect, AppEvent, AppNode, ComponentUpdate, DraftReset, RenderRequest,
        RestoredSessionProjection, RootEffect, RootNode,
    },
    history::{
        HistoryPrefetch, HistoryWindow, history_projection, history_projection_with_sequences,
        live_managed_projection, unix_ms,
    },
    pane::PaneId,
    prompt::Submission,
    prompt_cache::PromptCache,
    scheduler::{RenderScheduler, STREAM_FRAME_INTERVAL},
    session::{RecentPrompt, SessionSummary},
    shell::ShellExecution,
    terminal::TerminalSession,
    theme::{Theme, detect_system_scheme},
    transcript::{LocalEvent, ShellId, TranscriptRecord, TurnId},
};
use crate::nanocodex2::{config::ReasoningEffort, config::ReasoningMode, host::HostConfig};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use nanocodex::Model;
use nanocodex_agent::{Nanocodex, NanocodexError, PromptRequest, Turn, TurnControl, TurnResult};
use nanocodex_managed::{
    AgentList, AgentSettings, AgentState, EventCursor, EventHistoryPage, ManagedClient,
    ManagedError, ManagedEvent, ManagedEventData, ManagedModel,
    ReasoningMode as ManagedReasoningMode, Thinking,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::pending,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

type Admission = (PaneId, TurnId, Result<Turn, NanocodexError>);
type Completion = (PaneId, TurnId, Result<TurnResult, NanocodexError>);
type SteerCompletion = (
    PaneId,
    components::QueueId,
    u64,
    SteerTarget,
    Result<(), SteerFailure>,
);
type WithdrawalCompletion = (PaneId, components::QueueId, u64, Result<bool, String>);
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum SteerTarget {
    Local(TurnId),
    Managed { agent_id: String, turn_id: String },
}

#[derive(Debug, Eq, PartialEq)]
enum SteerResolution {
    Admitted,
    Failed,
    Rejected(String),
    Unconfirmed { error: String, active: bool },
    Stale,
}

enum SteerFailure {
    Inactive,
    Rejected(String),
    Other(String),
}

impl SteerFailure {
    fn managed(error: ManagedError) -> Self {
        if matches!(&error, ManagedError::Http { status, code, .. }
            if status.as_u16() == 409 && matches!(code.as_str(), "turn_not_active" | "turn_not_steerable"))
        {
            Self::Inactive
        } else if Self::known_rejection(&error) {
            Self::Rejected(error.to_string())
        } else {
            Self::Other(error.to_string())
        }
    }

    fn known_rejection(error: &ManagedError) -> bool {
        matches!(error, ManagedError::Configuration(_))
            || matches!(error, ManagedError::Http { status, code, .. }
                if status.is_client_error() && code != "command_delivery_unknown")
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
        if let NanocodexError::Backend { source, .. } = &error
            && let Some(managed) = source.downcast_ref::<ManagedError>()
            && Self::known_rejection(managed)
        {
            return Self::Rejected(error.to_string());
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
    SettingsMutation,
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
    Default,
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
    Bug(PaneId),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecoveryPhase {
    Connecting,
    Replaying,
    Disconnected,
}

struct ReviewBranchesCompletion {
    pane: PaneId,
    request_id: uuid::Uuid,
    result: Result<Vec<review::Branch>, String>,
}

struct SessionSearchCompletion {
    pane: PaneId,
    picker_id: u64,
    request_id: u64,
    query: String,
    result: Result<Vec<nanocodex_managed::SessionSearchHit>, String>,
}

enum ConnectionResult {
    Recovered(Result<ConnectedAgent, ConnectionFailure>),
    Agent {
        purpose: ConnectionPurpose,
        result: Result<ConnectedAgent, ConnectionFailure>,
    },
    Sessions {
        pane: PaneId,
        request_id: u64,
        result: Option<Result<AgentList, ManagedError>>,
    },
    /// Local session discovery, read off the input loop.
    LocalSessions {
        pane: PaneId,
        request_id: u64,
        result: Option<Result<Vec<SessionSummary>, String>>,
    },
    RecentPrompts {
        pane: PaneId,
        request_id: u64,
        session_id: String,
        local: Vec<RecentPrompt>,
        result: Result<Vec<RecentPrompt>, String>,
    },
    Disconnected(Result<(), NanocodexError>),
}

#[derive(Clone, Copy)]
enum SettingsMutation {
    AutoRoute,
    Complete(AgentSettings),
    Thinking(Thinking),
    FastMode(bool),
}

impl SettingsMutation {
    fn failure_subject(self) -> &'static str {
        match self {
            Self::AutoRoute => "enable automatic routing",
            Self::Complete(_) => "select model",
            Self::Thinking(_) => "change thinking effort",
            Self::FastMode(_) => "change fast mode",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingVoice {
    pane: PaneId,
    selection: crate::nanocodex2::voice::Selection,
    muted: bool,
}

struct BtwConnection {
    pane: PaneId,
    agent_id: Option<String>,
    commands: mpsc::UnboundedSender<btw::Request>,
    task: tokio::task::JoinHandle<()>,
}

struct DriverRuntime {
    control_bridge: Option<nanocodex_tui_control::Bridge>,
    btw: Option<BtwConnection>,
    btw_events: mpsc::UnboundedSender<btw::Event>,
    screen: screen::Controller,
    pending_voice: Option<PendingVoice>,
    voice_selection: crate::nanocodex2::voice::Selection,
    clone_panel: Option<voice_clone::Panel>,
    voice_tasks: JoinSet<(PaneId, Result<String, String>)>,
    voice: Option<crate::nanocodex2::voice::Session>,
    client: ManagedClient,
    /// The local backend for ncl; None for managed sessions.
    local: Option<local::LocalState>,
    feature_overlay: Option<Box<dyn features::FeatureOverlay>>,
    // Feature-submitted turns and their receipts.
    feature_instructions: HashMap<TurnId, String>,
    feature_completions: HashMap<TurnId, Vec<tokio::sync::oneshot::Sender<bool>>>,
    feature_turns: HashSet<TurnId>,
    local_voice_status: Option<crate::nanocodex2::voice_state::Status>,
    /// A harness relaunch is queued or connecting; prompts wait for it.
    harness_relaunching: bool,
    /// The newest harness launch waiting for the connection slot.
    queued_relaunch: Option<Box<local::agent::LocalLaunch>>,
    agent: Option<Nanocodex>,
    startup_attach: bool,
    pending_resume: Option<(tokio::task::AbortHandle, PaneId)>,
    managed_events: Option<mpsc::UnboundedReceiver<ManagedEvent>>,
    managed_events_open: bool,
    recovery: Option<RecoveryPhase>,
    recovery_events: VecDeque<ManagedEvent>,
    observed_cursor: String,
    last_recovery: Option<Instant>,
    connection_generation: u64,
    agent_id: String,
    settings: AgentSettings,
    pending_settings: Option<AgentSettings>,
    pending_autoroute: Option<PaneId>,
    routing_enabled: bool,
    routing_resolved: bool,
    routing_generation: u64,
    routing_updates: JoinSet<(
        String,
        u64,
        Result<nanocodex_managed::RoutingStatus, ManagedError>,
    )>,
    workspace: PathBuf,
    sequence: u64,
    next_turn: u64,
    next_shell: u64,
    controls: HashMap<TurnId, TurnControl>,
    local_managed_turns: HashMap<TurnId, String>,
    local_terminal_turns: HashSet<TurnId>,
    submitted_turns: HashSet<String>,
    detached_submissions: HashSet<String>,
    unacknowledged_inputs: HashMap<TurnId, (PaneId, String, Submission)>,
    confirmed_requests: HashSet<String>,
    managed_active_turns: ManagedActiveTurns,
    admitting: HashSet<TurnId>,
    cancel_after_admission: HashSet<TurnId>,
    cancellation_fences: CancellationFences,
    cancellation_failed: bool,
    cancellation_had_effect: bool,
    admissions: JoinSet<Admission>,
    completions: JoinSet<Completion>,
    steers: JoinSet<SteerCompletion>,
    receipt_reconciliations: JoinSet<(
        PaneId,
        components::QueueId,
        String,
        String,
        nanocodex_managed::SteerReceiptState,
    )>,
    unresolved_steers: HashMap<(PaneId, components::QueueId), CancellationToken>,
    vault_tasks: JoinSet<vault::Completion>,
    done_updates: JoinSet<(
        String,
        u64,
        bool,
        Result<nanocodex_managed::SessionDoneState, ManagedError>,
    )>,
    secure_input: Option<secure_input::Flow>,
    secure_input_tasks: JoinSet<secure_input::Completion>,
    secure_input_attempted: HashSet<String>,
    share_tasks: JoinSet<(PaneId, String, u64, Result<share::Outcome, ManagedError>)>,
    site_tasks: JoinSet<(PaneId, String, u64, Result<sites::Outcome, ManagedError>)>,
    vault_attempted: HashSet<(String, String)>,
    steer_receipts: HashMap<(PaneId, components::QueueId), (u64, SteerTarget, String)>,
    pending_withdrawals: HashSet<(PaneId, components::QueueId)>,
    withdrawals: JoinSet<WithdrawalCompletion>,
    pending_steer_target: Option<(components::QueueId, SteerTarget)>,
    waiting_steers: VecDeque<WaitingSteer>,
    unconfirmed_steer: Option<(components::QueueId, u64, SteerTarget)>,
    cancellations: JoinSet<CancelCompletion>,
    settings_updates: JoinSet<SettingsCompletion>,
    settings_queue: VecDeque<(PaneId, String, SettingsMutation)>,
    shells: JoinSet<(PaneId, ShellExecution)>,
    links: JoinSet<(PaneId, Result<(), String>)>,
    history_loads: JoinSet<HistoryCompletion>,
    history_replays: JoinSet<HistoryReplayCompletion>,
    history_prefetch: HistoryPrefetch,
    history_tree_open: bool,
    history_tree_failed: bool,
    history_generation: u64,
    history: HistoryWindow,
    history_sequences: HashMap<String, u64>,
    history_records: Vec<Arc<TranscriptRecord>>,
    live_records: Vec<Arc<TranscriptRecord>>,
    active_shells: usize,
    shell_cancellation: CancellationToken,
    shell_context: Vec<String>,
    pending_submission: Option<(PaneId, TurnId, Submission)>,
    recent_prompts: Vec<RecentPrompt>,
    prompt_cache: Result<PromptCache, String>,
    prompt_cache_writes: JoinSet<(Vec<RecentPrompt>, Result<(), String>)>,
    prompt_cache_pending: Vec<RecentPrompt>,
    prompt_cache_retry_at: Option<Instant>,
    prompt_cache_retries: u8,
    prompt_cache_warned: bool,
    recent_prompt_request: u64,
    recent_prompt_loads: HashMap<PaneId, u64>,
    connection: JoinSet<ConnectionResult>,
    session_list_cancellations: HashMap<(PaneId, u64), CancellationToken>,
    review_branch_loads: JoinSet<ReviewBranchesCompletion>,
    session_searches: JoinSet<SessionSearchCompletion>,
    session_search_tasks: HashMap<PaneId, tokio::task::AbortHandle>,
    retry_target: Option<RetryTarget>,
}

struct PreparedHistoryReplay {
    older_history: HistoryWindow,
    sequences: HashMap<String, u64>,
    next_sequence: u64,
    history_records: Vec<Arc<TranscriptRecord>>,
    older_prompts: Vec<RecentPrompt>,
    live_records_len: usize,
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
    agent_id: &str,
    workspace: &Path,
    effort: ReasoningEffort,
) -> Result<PreparedHistoryReplay, ManagedError> {
    let mut older_history = HistoryWindow::default();
    older_history.prepend(page)?;
    let mut projected_next_sequence = next_sequence;
    let (mut older_records, older_prompts) = history_projection_with_sequences(
        &older_history.events,
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
        live_records_len: live_records.len(),
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

fn voice_settings(
    selection: &crate::nanocodex2::voice::Selection,
) -> nanocodex_voice_protocol::VoiceSettings {
    use nanocodex_voice_protocol::{VoiceOutputProvider, VoiceSettings};
    match selection {
        crate::nanocodex2::voice::Selection::Chatgpt(name) => VoiceSettings {
            voice: (*name).into(),
            ..Default::default()
        },
        crate::nanocodex2::voice::Selection::ElevenLabs(id) => VoiceSettings {
            output_provider: VoiceOutputProvider::Elevenlabs,
            eleven_labs_voice_id: Some(id.clone()),
            ..Default::default()
        },
    }
}

async fn list_elevenlabs_voices() -> Result<String, String> {
    let client =
        crate::nanocodex2::voice::elevenlabs::Client::from_env().map_err(|e| e.to_string())?;
    let voices = client.voices().await.map_err(|e| e.to_string())?;
    let mut lines = vec!["ElevenLabs voices (use /voice elevenlabs VOICE_ID):".to_owned()];
    for voice in voices {
        lines.push(format!(
            "{} — {}{}",
            voice.voice_id,
            voice.name,
            if voice.category.is_empty() {
                String::new()
            } else {
                format!(" ({})", voice.category)
            }
        ));
    }
    if lines.len() == 1 {
        lines.push("No voices found. Use /voice clone NAME PATH --consent.".into());
    }
    Ok(lines.join("\n"))
}

fn resolve_voice_sample_path(
    workspace: &Path,
    path: PathBuf,
    home: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Ok(relative) = path.strip_prefix("~") {
        return home.map(|home| home.join(relative)).ok_or_else(|| {
            "Cannot expand ~/ audio path: HOME is not set. Use an absolute path.".into()
        });
    }
    Ok(if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    })
}

async fn clone_elevenlabs_voice(name: String, path: PathBuf) -> Result<String, String> {
    // The provider currently reads samples synchronously. Keep even slow local files
    // off the terminal's executor; the transport itself remains asynchronous.
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        handle.block_on(async move {
            let client = crate::nanocodex2::voice::elevenlabs::Client::from_env().map_err(|e| e.to_string())?;
            let voice = client
                .clone_voice(&name, &[path], true)
                .await
                .map_err(|e| e.to_string())?;
            if voice.requires_verification {
                Ok(format!("Voice created ({}). Complete verification in ElevenLabs before using this voice.", voice.voice_id))
            } else {
                Ok(format!("Voice cloned. Select it with /voice elevenlabs {}", voice.voice_id))
            }
        })
    })
    .await
    .map_err(|_| "Voice clone task failed".to_owned())?
}

impl DriverRuntime {
    fn pane_session_id(&self, pane: PaneId) -> String {
        if pane == PaneId::Main {
            self.agent_id.clone()
        } else {
            self.btw
                .as_ref()
                .filter(|btw| btw.pane == pane)
                .and_then(|btw| btw.agent_id.clone())
                .unwrap_or_default()
        }
    }

    fn cache_prompts(&mut self, prompts: Vec<RecentPrompt>) {
        if prompts.is_empty() || self.prompt_cache.is_err() {
            return;
        }
        self.prompt_cache_pending.extend(prompts);
        self.prompt_cache_pending =
            prompt_cache::bounded(std::mem::take(&mut self.prompt_cache_pending));
        self.prompt_cache_retries = 1;
        self.prompt_cache_retry_at = None;
        self.start_prompt_cache_write();
    }

    fn start_prompt_cache_write(&mut self) {
        if !self.prompt_cache_writes.is_empty() || self.prompt_cache_pending.is_empty() {
            return;
        }
        let Ok(cache) = self.prompt_cache.clone() else {
            return;
        };
        let batch = std::mem::take(&mut self.prompt_cache_pending);
        self.prompt_cache_writes.spawn(async move {
            let result = cache.merge(batch.clone()).await.map(|_| ());
            (batch, result)
        });
    }

    fn load_prompt_cache(&mut self, pane: PaneId, drafts: Vec<components::RecentPromptDraft>) {
        self.recent_prompt_request = self.recent_prompt_request.wrapping_add(1);
        let request_id = self.recent_prompt_request;
        self.recent_prompt_loads.insert(pane, request_id);
        let session_id = self.pane_session_id(pane);
        let mut local = if pane == PaneId::Main {
            self.recent_prompts.clone()
        } else {
            Vec::new()
        };
        local.extend(drafts.into_iter().map(|draft| RecentPrompt {
            text: draft.text,
            recorded_at_unix_ms: draft.recorded_at_unix_ms,
            session_id: session_id.clone(),
            workspace: self.workspace.clone(),
        }));
        let local = prompt_cache::bounded(local);
        let cache = self.prompt_cache.clone();
        self.connection.spawn(async move {
            let result = match cache {
                Ok(cache) => cache.merge(local.clone()).await,
                Err(error) => Err(error),
            };
            ConnectionResult::RecentPrompts {
                pane,
                request_id,
                session_id,
                local,
                result,
            }
        });
    }

    fn active_managed_turn_ids(&self) -> Vec<&str> {
        // Attached turns and locally controlled turns are counted separately by
        // the UI, but control clients need the complete durable active set.
        let mut ids = self
            .managed_active_turns
            .ids
            .iter()
            .map(String::as_str)
            .chain(
                self.local_managed_turns
                    .iter()
                    .filter_map(|(local, managed)| {
                        (!self.local_terminal_turns.contains(local)
                            && (self.controls.contains_key(local)
                                || (self.admitting.contains(local)
                                    && self.confirmed_requests.contains(managed))))
                        .then_some(managed.as_str())
                    }),
            )
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn voice_status(&self) -> Option<crate::nanocodex2::voice_state::Status> {
        if let Some(panel) = &self.clone_panel {
            return Some(crate::nanocodex2::voice_state::Status {
                text: panel.text(),
                microphone: panel.microphone_peak(),
                ..Default::default()
            });
        }
        self.voice
            .as_ref()
            .map(|voice| voice.status.borrow().clone())
            .or_else(|| {
                self.pending_voice
                    .as_ref()
                    .map(|pending| crate::nanocodex2::voice_state::Status {
                        text: "Voice connecting…".into(),
                        muted: pending.muted,
                        ..Default::default()
                    })
            })
            // Local Realtime voice status.
            .or_else(|| self.local_voice_status.clone())
    }

    fn take_ready_voice(&mut self) -> Option<PendingVoice> {
        if self.agent_id.is_empty()
            || !self.managed_events_open
            || self.pending_resume.is_some()
            || self.recovery.is_some()
            || self.voice.is_some()
        {
            return None;
        }
        self.pending_voice.take()
    }

    fn voice_command(
        &mut self,
        pane: PaneId,
        command: crate::nanocodex2::voice::Command,
    ) -> Result<Option<String>, String> {
        use crate::nanocodex2::voice::Command;
        if self.clone_panel.is_some()
            && matches!(
                command,
                Command::Toggle | Command::Start(_) | Command::Select(_) | Command::Clone { .. }
            )
        {
            return Err(
                "Finish or cancel /voice clone before starting realtime voice or another clone."
                    .into(),
            );
        }
        let command = match command {
            Command::Toggle if self.voice.is_some() || self.pending_voice.is_some() => {
                Command::Stop
            }
            Command::Toggle => Command::Start(None),
            other => other,
        };
        match command {
            Command::Start(None) if self.voice.is_some() => Ok(None),
            Command::Start(name) => {
                let selection = name
                    .map(crate::nanocodex2::voice::Selection::Chatgpt)
                    .unwrap_or_else(|| self.voice_selection.clone());
                self.voice_command(pane, Command::Select(selection))
            }
            Command::Select(selection) => {
                self.voice_selection = selection.clone();
                let muted = self
                    .voice
                    .as_ref()
                    .map(|voice| voice.is_muted())
                    .or_else(|| self.pending_voice.as_ref().map(|pending| pending.muted))
                    .unwrap_or(false);
                self.pending_voice = Some(PendingVoice {
                    pane,
                    selection,
                    muted,
                });
                // Wait for the old session's finished status before starting new media.
                if let Some(voice) = &self.voice {
                    voice.stop();
                }
                Ok(None)
            }
            Command::Stop => {
                self.pending_voice = None;
                if let Some(voice) = &self.voice {
                    voice.stop();
                }
                Ok(None)
            }
            Command::ToggleMute | Command::Unmute => {
                let current = self
                    .pending_voice
                    .as_ref()
                    .map(|pending| pending.muted)
                    .or_else(|| self.voice.as_ref().map(|voice| voice.is_muted()))
                    .ok_or_else(|| "Start /voice before muting.".to_owned())?;
                let muted = command == Command::ToggleMute && !current;
                if let Some(voice) = &self.voice {
                    voice.mute(muted);
                }
                if let Some(pending) = &mut self.pending_voice {
                    pending.muted = muted;
                }
                Ok(None)
            }
            Command::Help => Ok(Some(crate::nanocodex2::voice::HELP.into())),
            Command::ListProvider(crate::nanocodex2::voice::Provider::Chatgpt) => {
                Ok(Some(format!(
                    "ChatGPT voices: {}. Use /voice chatgpt NAME.",
                    nanocodex_voice_protocol::CHATGPT_REALTIME_VOICES.join(", ")
                )))
            }
            Command::List
            | Command::ListProvider(crate::nanocodex2::voice::Provider::ElevenLabs) => {
                let all = command == Command::List;
                self.voice_tasks.spawn(async move {
                    let result = list_elevenlabs_voices().await;
                    let result = if all {
                        let chatgpt = format!(
                            "ChatGPT voices: {}. Use /voice chatgpt NAME.",
                            nanocodex_voice_protocol::CHATGPT_REALTIME_VOICES.join(", ")
                        );
                        Ok(format!(
                            "{chatgpt}\n{}",
                            result.unwrap_or_else(|error| format!("ElevenLabs: {error}"))
                        ))
                    } else {
                        result
                    };
                    (pane, result)
                });
                Ok(Some("Loading voice catalog…".into()))
            }
            Command::CloneOpen(name) => {
                if self.clone_panel.is_some() {
                    return Err("Cancel the current clone first.".into());
                }
                self.clone_panel = Some(voice_clone::Panel::new(name));
                Ok(None)
            }
            Command::CloneRecord(name) => {
                if let Some(name) = name {
                    if self.clone_panel.is_some() {
                        return Err("Cancel the current clone first.".into());
                    }
                    self.clone_panel = Some(voice_clone::Panel::new(name));
                }
                self.clone_panel
                    .as_mut()
                    .ok_or("Open /voice clone NAME first.")?
                    .record()?;
                self.pending_voice = None;
                if let Some(voice) = &self.voice {
                    voice.stop();
                }
                Ok(None)
            }
            Command::CloneStop => {
                self.clone_panel
                    .as_mut()
                    .ok_or("No clone recording is open.")?
                    .stop()?;
                Ok(None)
            }
            Command::ClonePlay => {
                self.clone_panel
                    .as_mut()
                    .ok_or("No clone recording is open.")?
                    .play()?;
                Ok(None)
            }
            Command::CloneReview => Ok(Some(
                self.clone_panel
                    .as_ref()
                    .ok_or("No clone recording is open.")?
                    .review()?,
            )),
            Command::CloneCancel => {
                self.clone_panel = None;
                Ok(Some("Clone cancelled; local recording discarded.".into()))
            }
            Command::CloneSubmit => {
                if !self
                    .clone_panel
                    .as_ref()
                    .is_some_and(|panel| matches!(panel.state, voice_clone::State::Review(_)))
                {
                    return Err("Stop and review a local recording before submitting.".into());
                }
                // Keep the recording available if local credentials are missing.
                crate::nanocodex2::voice::elevenlabs::Client::from_env()
                    .map_err(|error| error.to_string())?;
                let mut panel = self.clone_panel.take().unwrap();
                let name = panel.name.clone();
                let voice_clone::State::Review(sample) =
                    std::mem::replace(&mut panel.state, voice_clone::State::Busy)
                else {
                    unreachable!()
                };
                self.voice_tasks.spawn(async move {
                    let result = clone_elevenlabs_voice(name, sample.path().to_owned()).await;
                    drop(sample);
                    (pane, result)
                });
                Ok(Some(
                    "Uploading consented recording directly to ElevenLabs…".into(),
                ))
            }
            Command::Clone { name, path } => {
                let path = resolve_voice_sample_path(
                    &self.workspace,
                    path,
                    std::env::var_os("HOME").map(PathBuf::from),
                )?;
                self.voice_tasks
                    .spawn(async move { (pane, clone_elevenlabs_voice(name, path).await) });
                Ok(Some("Cloning voice with ElevenLabs…".into()))
            }
            Command::Status => Ok(Some(self.voice_status().map_or_else(
                || format!("Voice is off · selected {}", self.voice_selection.label()),
                |status| {
                    if status.muted {
                        format!("{} · microphone muted", status.text)
                    } else {
                        status.text
                    }
                },
            ))),
            Command::Toggle => unreachable!("toggle resolved above"),
        }
    }

    fn finish_resume(&mut self, task_id: tokio::task::Id) -> Option<PaneId> {
        let (task, _) = self.pending_resume.as_ref()?;
        if task.id() != task_id {
            return None;
        }
        self.pending_resume.take().map(|(_, pane)| pane)
    }

    fn begin_recovery(
        &mut self,
        app: &mut AppNode,
        scheduler: &mut RenderScheduler,
        automatic: bool,
    ) {
        if self.local.is_some() {
            // A local agent has no durable stream to reattach; its turns
            // finish through their own results.
            let _ = (app, scheduler, automatic);
            return;
        }
        if let Some((_, pane)) = &self.pending_resume {
            // A failed old stream must not race the explicitly selected session.
            // If resume fails, its completion path will recover this connection.
            self.managed_events = None;
            self.managed_events_open = false;
            request_render(
                app.update(AppEvent::NotifyError {
                    pane: *pane,
                    error: "Previous session disconnected · waiting for the selected session"
                        .to_owned(),
                }),
                scheduler,
            );
            return;
        }
        if matches!(
            self.recovery,
            Some(RecoveryPhase::Connecting | RecoveryPhase::Replaying)
        ) {
            return;
        }
        if self.recovery.is_none() {
            request_render(
                app.update(AppEvent::AgentStreamClosed(PaneId::Main)),
                scheduler,
            );
            self.managed_events = None;
            self.managed_events_open = false;
            self.connection_generation = self.connection_generation.wrapping_add(1);
            let mut pending: Vec<_> = self.unacknowledged_inputs.drain().collect();
            pending.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
            for (_, (pane, request_id, prompt)) in pending {
                if !self.confirmed_requests.contains(&request_id) {
                    request_render(
                        app.update(AppEvent::RetainPrompt {
                            pane,
                            request_id,
                            prompt,
                        }),
                        scheduler,
                    );
                }
            }
            self.admissions = JoinSet::new();
            self.completions = JoinSet::new();
            self.steers = JoinSet::new();
            self.withdrawals = JoinSet::new();
            self.pending_withdrawals.clear();
            self.steer_receipts.retain(|key, (generation, target, _)| {
                if !self.unresolved_steers.contains_key(key) {
                    return false;
                }
                *generation = self.connection_generation;
                if let SteerTarget::Local(local) = target {
                    let Some(turn_id) = self.local_managed_turns.get(local) else {
                        return false;
                    };
                    *target = SteerTarget::Managed {
                        agent_id: self.agent_id.clone(),
                        turn_id: turn_id.clone(),
                    };
                }
                true
            });
            self.controls.clear();
            self.admitting.clear();
            self.cancel_after_admission.clear();
            let uncertain = self
                .unconfirmed_steer
                .take()
                .map(|(id, _, target)| (id, target))
                .or_else(|| self.pending_steer_target.take());
            self.unconfirmed_steer = uncertain.and_then(|(id, target)| {
                let turn_id = match target {
                    SteerTarget::Local(local) => self.local_managed_turns.get(&local)?.clone(),
                    SteerTarget::Managed { turn_id, .. } => turn_id,
                };
                Some((
                    id,
                    self.connection_generation,
                    SteerTarget::Managed {
                        agent_id: self.agent_id.clone(),
                        turn_id,
                    },
                ))
            });
            self.local_managed_turns.clear();
            self.local_terminal_turns.clear();
            self.managed_active_turns = ManagedActiveTurns::default();
            self.history_generation = self.history_generation.wrapping_add(1);
            self.history_loads = JoinSet::new();
            self.history_replays = JoinSet::new();
            self.history_prefetch.reset();
            self.history_tree_failed = false;
            for (pane, id) in take_waiting_steer_failures(&mut self.waiting_steers) {
                request_render(app.update(AppEvent::SteerFailed { pane, id }), scheduler);
            }
            if let Some(previous) = self.agent.take() {
                self.connection.spawn(async move {
                    // This driver already failed; detachment must not cancel its durable work.
                    drop(previous.disconnect().await);
                    ConnectionResult::Disconnected(Ok(()))
                });
            }
        }
        if automatic
            && self
                .last_recovery
                .is_some_and(|when| when.elapsed() < std::time::Duration::from_secs(5))
        {
            self.recovery = Some(RecoveryPhase::Disconnected);
            request_render(
                app.update(AppEvent::AgentReconnectFailed {
                    pane: PaneId::Main,
                    error: "The connection stopped again. Press Enter to reconnect.".to_owned(),
                }),
                scheduler,
            );
            return;
        }
        self.recovery = Some(RecoveryPhase::Connecting);
        self.last_recovery = Some(Instant::now());
        let client = self.client.clone();
        let agent_id = self.agent_id.clone();
        let cursor = self.observed_cursor.clone();
        self.connection.spawn(async move {
            ConnectionResult::Recovered(reconnect_agent(client, agent_id, cursor).await)
        });
    }

    fn reconcile_steer_receipt(
        &mut self,
        pane: PaneId,
        id: components::QueueId,
        target: &SteerTarget,
        message_id: String,
        input: nanocodex_managed::PromptInput,
    ) {
        let (agent_id, turn_id) = match target {
            SteerTarget::Local(local) => {
                let Some(turn_id) = self.local_managed_turns.get(local) else {
                    return;
                };
                (self.agent_id.clone(), turn_id.clone())
            }
            SteerTarget::Managed { agent_id, turn_id } => (agent_id.clone(), turn_id.clone()),
        };
        let cancellation = CancellationToken::new();
        if let Some(previous) = self
            .unresolved_steers
            .insert((pane, id), cancellation.clone())
        {
            previous.cancel();
        }
        let client = self.client.clone();
        self.receipt_reconciliations.spawn(async move {
            let mut delay = std::time::Duration::from_secs(1);
            loop {
                // Only a read is repeated. An old server cannot accidentally receive
                // a duplicate instruction while capability support is unknown.
                tokio::select! {
                    () = cancellation.cancelled() => return (pane, id, agent_id, message_id, nanocodex_managed::SteerReceiptState::Unknown),
                    () = tokio::time::sleep(delay) => {}
                }
                let result = tokio::select! {
                    () = cancellation.cancelled() => return (pane, id, agent_id, message_id, nanocodex_managed::SteerReceiptState::Unknown),
                    result = client.steer_receipt(&agent_id, &turn_id, &message_id) => result,
                };
                match result {
                    Ok(receipt) if receipt.terminal || receipt.state != nanocodex_managed::SteerReceiptState::Unknown => {
                        let state = if receipt.matches_input(&input) {
                            receipt.state
                        } else {
                            nanocodex_managed::SteerReceiptState::Unknown
                        };
                        return (pane, id, agent_id, message_id, state);
                    }
                    Err(ManagedError::Http { status, .. }) if matches!(status.as_u16(), 400 | 401 | 403 | 404 | 405) => {
                        return (pane, id, agent_id, message_id, nanocodex_managed::SteerReceiptState::Unknown);
                    }
                    _ => {}
                }
                delay = (delay * 2).min(std::time::Duration::from_secs(30));
            }
        });
    }

    fn resolve_steer(
        &self,
        generation: u64,
        target: &SteerTarget,
        outcome: Result<(), SteerFailure>,
    ) -> SteerResolution {
        if generation != self.connection_generation {
            return SteerResolution::Stale;
        }
        match outcome {
            // An acknowledgement means the input was accepted even if the
            // terminal event won the race with the control response. Requeuing
            // it here would submit the same instruction a second time.
            Ok(()) => SteerResolution::Admitted,
            Err(SteerFailure::Other(error)) => SteerResolution::Unconfirmed {
                error,
                active: self.steer_target_current(target),
            },
            Err(SteerFailure::Inactive) => SteerResolution::Failed,
            Err(SteerFailure::Rejected(error)) => SteerResolution::Rejected(error),
        }
    }

    fn steer_target_current(&self, target: &SteerTarget) -> bool {
        match target {
            SteerTarget::Local(id) => self.controls.contains_key(id),
            SteerTarget::Managed { agent_id, turn_id } => {
                agent_id == &self.agent_id && self.managed_active_turns.ids.contains(turn_id)
            }
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
        if self.local.is_some() {
            return;
        }
        if (self.history_tree_open && self.history_tree_failed)
            || !self.history_loads.is_empty()
            || !self.history_replays.is_empty()
        {
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
        if (self.history_tree_open && self.history_tree_failed) || !self.history_replays.is_empty()
        {
            return;
        }
        let Some((requested_before, page)) = self
            .history_prefetch
            .take_requested(self.history.before.as_deref())
        else {
            return;
        };
        let sequences = self.history_sequences.clone();
        let history_records = self.history_records.clone();
        let live_records = self.live_records.clone();
        let next_sequence = self.sequence;
        // Projection allocates at most one record per older event. Reserve its IDs before
        // yielding so incoming records cannot reuse them while the blocking task runs.
        self.sequence = self.sequence.saturating_add(page.data.len() as u64);
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
                &agent_id,
                &workspace,
                effort,
            );
            (pane, agent_id, generation, requested_before, result)
        });
    }

    fn update_tree_history(&mut self, open: bool) {
        if open != self.history_tree_open {
            self.history_tree_open = open;
            self.history_tree_failed = false;
            if !open {
                // A closing overlay must not keep fetching or install a late projection.
                self.history_generation = self.history_generation.wrapping_add(1);
                self.history_loads = JoinSet::new();
                self.history_replays = JoinSet::new();
                self.history_prefetch.reset();
            }
        }
        if open && !self.history_tree_failed && self.history.has_more {
            // Consume the existing bounded prefetch queue one page at a time. Older
            // child receipts may precede the entire newest transcript page.
            self.history_prefetch.request_replay();
            self.start_requested_history_replay(PaneId::Main);
            self.start_history_prefetch(PaneId::Main);
        }
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
                self.history_tree_failed = self.history_tree_open;
                self.history_prefetch.reset();
                self.start_history_prefetch(pane);
                return Err(error);
            }
        };
        // Only rebuild presentation state: these records have already driven queue/control
        // effects in the live root. Replaying those effects would submit inputs twice.
        prepared.projection.append_records(
            self.live_records[prepared.live_records_len..]
                .iter()
                .cloned(),
        );
        if !self.managed_events_open {
            prepared.projection.close_stream();
        }
        self.history.prepend_window(prepared.older_history);
        self.history_sequences = prepared.sequences;
        self.sequence = self.sequence.max(prepared.next_sequence);
        self.next_turn = self.next_turn.max(self.sequence);
        self.history_records = prepared.history_records;
        self.cache_prompts(prepared.older_prompts.clone());
        self.recent_prompts.append(&mut prepared.older_prompts);
        self.start_history_prefetch(pane);
        Ok(prepared.projection)
    }

    fn local_record(&mut self, event: LocalEvent) -> Result<Arc<TranscriptRecord>, ManagedError> {
        self.feature_turn_event(&event);
        let record =
            TranscriptRecord::from_local(self.sequence, unix_ms(), event).map_err(|error| {
                ManagedError::Configuration(format!("TUI transcript error: {error}"))
            })?;
        self.sequence = self.sequence.saturating_add(1);
        let record = Arc::new(record);
        self.live_records.push(Arc::clone(&record));
        Ok(record)
    }

    /// Resolves feature receipts and busy state from turn records.
    fn feature_turn_event(&mut self, event: &LocalEvent) {
        let Some(local) = self.local.as_mut() else {
            return;
        };
        match event {
            LocalEvent::WorkerTurnAccepted { .. } => local.set_busy(true),
            LocalEvent::WorkerTurnFinished { id, error } => {
                for completion in self.feature_completions.remove(id).into_iter().flatten() {
                    let _ = completion.send(error.is_none());
                }
                self.feature_instructions.remove(id);
                self.feature_turns.remove(id);
                local.set_busy(false);
            }
            LocalEvent::WorkerTurnsInterrupted { .. } => {
                for (_, completions) in self.feature_completions.drain() {
                    for completion in completions {
                        let _ = completion.send(false);
                    }
                }
                local.set_busy(false);
            }
            _ => {}
        }
    }

    fn start_submission(&mut self, pane: PaneId, id: TurnId, prompt: Submission) {
        if self.recovery.is_some() || self.harness_relaunching {
            self.pending_submission = Some((pane, id, prompt));
            return;
        }
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
        // Private instructions and typed-prompt hooks.
        let mut agent_prompt = prompt.agent_prompt();
        let mut rejection = None;
        if let Some(local) = &mut self.local {
            if let Some(instruction) = self.feature_instructions.remove(&id) {
                agent_prompt = Submission::text(instruction).agent_prompt();
            } else if !self.feature_turns.contains(&id) {
                let text = prompt.display_text().to_owned();
                // Typed Claude slash prompts (/loop, user skills) expand at the
                // user submission boundary, as the legacy local TUI did.
                match crate::config::expand_session_user_skill(&agent, &text) {
                    Ok(Some(instruction)) => {
                        agent_prompt = Submission::text(instruction).agent_prompt();
                    }
                    Ok(None) => {}
                    Err(error) => rejection = Some(error),
                }
                match local.with_features(|features, cx| features.user_prompt(&text, cx)) {
                    Ok(completions) if !completions.is_empty() => {
                        self.feature_completions
                            .entry(id)
                            .or_default()
                            .extend(completions);
                    }
                    Ok(_) => {}
                    Err(error) => rejection = Some(error),
                }
            }
        }
        let managed_request_id = uuid::Uuid::now_v7().to_string();
        if let Some(local) = &mut self.local {
            // The event bridge tags the next local run with this request id.
            local.submissions.push(managed_request_id.clone());
            local.prompted = true;
        }
        self.submitted_turns.insert(managed_request_id.clone());
        self.unacknowledged_inputs
            .insert(id, (pane, managed_request_id.clone(), prompt));
        self.local_managed_turns
            .insert(id, managed_request_id.clone());
        self.admitting.insert(id);
        let local_submission = self.local.is_some();
        self.admissions.spawn(async move {
            let turn = match rejection {
                Some(error) => Err(nanocodex::NanocodexError::backend(
                    "loop",
                    std::io::Error::other(error),
                )),
                None => {
                    let request = PromptRequest::new(agent_prompt);
                    let request = if local_submission {
                        request
                    } else {
                        request.request_id(managed_request_id)
                    };
                    agent.prompt(request).await
                }
            };
            (pane, id, turn)
        });
    }

    fn record_submission(
        &mut self,
        id: TurnId,
        prompt: &Submission,
    ) -> Result<Arc<TranscriptRecord>, ManagedError> {
        let text = prompt.display_text().to_owned();
        let record = self.local_record(LocalEvent::UserSubmitted {
            id,
            text: text.clone(),
        })?;
        self.recent_prompts.insert(
            0,
            RecentPrompt {
                text,
                recorded_at_unix_ms: unix_ms(),
                session_id: self.agent_id.clone(),
                workspace: self.workspace.clone(),
            },
        );
        self.recent_prompts.truncate(100);
        self.cache_prompts(vec![self.recent_prompts[0].clone()]);
        Ok(record)
    }

    fn project_managed_event(
        &mut self,
        event: ManagedEvent,
    ) -> Result<Option<history::LiveManagedProjection>, ManagedError> {
        // The local prompt is already visible, including while creation is pending.
        // Retain IDs after completion: the observer can deliver acceptance after
        // the completion future, and replay must not duplicate the prompt either.
        if let ManagedEventData::TurnAccepted { id, .. } = &event.data
            && (self.submitted_turns.contains(id) || self.detached_submissions.contains(id))
        {
            return Ok(None);
        }
        live_managed_projection(event, &self.agent_id, &self.workspace, &mut self.sequence)
    }

    fn refresh_routing(&mut self) {
        if self.local.is_some()
            || self.agent_id.is_empty()
            || !self.routing_updates.is_empty()
            || !self.settings_updates.is_empty()
        {
            return;
        }
        let client = self.client.clone();
        let agent_id = self.agent_id.clone();
        let generation = self.routing_generation;
        self.routing_updates.spawn(async move {
            let result = client.routing_status(&agent_id).await;
            (agent_id, generation, result)
        });
    }

    fn enable_autoroute(&mut self, pane: PaneId) {
        if self.agent.is_none() {
            self.pending_autoroute = Some(pane);
            if self.connection.is_empty()
                && let Some(target) = self.retry_target.take()
            {
                self.spawn_connection(ConnectionPurpose::Startup, target);
            }
            return;
        }
        self.queue_settings(pane, SettingsMutation::AutoRoute);
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
        if self.local.is_some() {
            let agent = self.agent.clone();
            let current = self.settings;
            self.settings_updates.spawn(async move {
                let result = local::apply_settings(agent, current, mutation).await;
                (pane, agent_id, mutation, result)
            });
            return;
        }
        let client = self.client.clone();
        let was_routed = self.routing_enabled;
        let model = self.settings.model;
        // A read started before this mutation must not restore an obsolete route.
        self.routing_generation = self.routing_generation.wrapping_add(1);
        self.routing_updates = JoinSet::new();
        self.settings_updates.spawn(async move {
            let result = match mutation {
                SettingsMutation::AutoRoute => client
                    .enable_auto_routing(&agent_id)
                    .await
                    .map(|receipt| receipt.settings),
                SettingsMutation::Complete(settings) => {
                    if gateway_model(settings.model) || was_routed {
                        match client
                            .set_manual_routing(&agent_id, settings.model, settings.thinking)
                            .await
                        {
                            Ok(receipt) if gateway_model(settings.model) => Ok(receipt.settings),
                            Ok(receipt) if receipt.settings == settings => Ok(receipt.settings),
                            Ok(_) => client.set_settings(&agent_id, settings).await,
                            Err(error) => Err(error),
                        }
                    } else {
                        client.set_settings(&agent_id, settings).await
                    }
                }
                SettingsMutation::Thinking(thinking) => {
                    if gateway_model(model) {
                        client
                            .set_manual_routing(&agent_id, model, thinking)
                            .await
                            .map(|receipt| receipt.settings)
                    } else {
                        client.set_thinking(&agent_id, thinking).await
                    }
                }
                SettingsMutation::FastMode(enabled) => {
                    client.set_fast_mode(&agent_id, enabled).await
                }
            };
            (pane, agent_id, mutation, result)
        });
    }

    /// Starts the queued harness relaunch once no connection is in flight.
    fn start_queued_relaunch(&mut self) -> Option<String> {
        if !self.connection.is_empty() {
            return None;
        }
        let launch = self.queued_relaunch.take()?;
        match self.local_switch(ConnectionPurpose::Startup, *launch) {
            Ok(_) => None,
            Err(error) => {
                self.harness_relaunching = false;
                Some(error)
            }
        }
    }

    fn spawn_connection(&mut self, purpose: ConnectionPurpose, target: RetryTarget) {
        if matches!(purpose, ConnectionPurpose::Startup) {
            self.retry_target = Some(target.clone());
        }
        if let Some(local) = &self.local {
            let connecting = local.connect();
            self.connection.spawn(async move {
                let result = connecting
                    .await
                    .map(local::LocalConnection::into_connected)
                    .map_err(|error| ConnectionFailure {
                        error: ManagedError::Configuration(error),
                        retry: target,
                    });
                ConnectionResult::Agent { purpose, result }
            });
            return;
        }
        let client = self.client.clone();
        let (agent_id, settings) = match target {
            RetryTarget::Default => (None, AgentSettings::default()),
            RetryTarget::Create(settings) => (None, fresh_thread_settings(false, settings)),
            RetryTarget::Agent(agent_id) => (Some(agent_id), AgentSettings::default()),
        };
        self.connection.spawn(async move {
            // The hosted live endpoint and this initial view share AgentSettings
            // defaults. Catalog discovery only populates the optional model picker;
            // it must never delay connecting or submitting the first prompt.
            let result = connect_agent(client, agent_id, settings).await;
            ConnectionResult::Agent { purpose, result }
        });
    }

    fn detach_bug_source(&mut self) {
        self.receipt_reconciliations = JoinSet::new();
        self.unresolved_steers.clear();
        self.admissions = JoinSet::new();
        self.completions = JoinSet::new();
        self.steers = JoinSet::new();
        self.cancellations = JoinSet::new();
        self.settings_updates = JoinSet::new();
        self.settings_queue.clear();
        self.pending_settings = None;
        self.pending_autoroute = None;
        self.routing_enabled = false;
        self.routing_resolved = false;
        self.routing_generation = self.routing_generation.wrapping_add(1);
        self.routing_updates = JoinSet::new();
        self.controls.clear();
        self.admitting.clear();
        self.cancel_after_admission.clear();
        self.local_managed_turns.clear();
        self.local_terminal_turns.clear();
        self.unacknowledged_inputs.clear();
        self.confirmed_requests.clear();
        self.waiting_steers.clear();
        self.pending_steer_target = None;
        self.unconfirmed_steer = None;
        self.pending_submission = None;
        self.pending_voice = None;
        self.clone_panel = None;
        self.recovery = None;
        self.recovery_events.clear();
        // Discard any queued recovery result for the old agent as well.
        self.connection = JoinSet::new();
        self.session_list_cancellations.clear();
        self.shell_cancellation.cancel();
        self.shells = JoinSet::new();
        self.shell_cancellation = CancellationToken::new();
        self.active_shells = 0;
    }

    fn start_new_session(&mut self, settings: AgentSettings) {
        self.voice.take();
        self.clone_panel = None;
        self.pending_voice = None;
        // Stop routing input and events to the previous agent before exposing
        // the new composer. Creation then uses the same pending-input path as launch.
        if let Some(previous) = self.agent.take() {
            self.connection
                .spawn(async move { ConnectionResult::Disconnected(previous.disconnect().await) });
        }
        self.managed_events = None;
        self.managed_events_open = false;
        self.observed_cursor = "0".to_owned();
        self.last_recovery = None;
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.agent_id.clear();
        self.settings = settings;
        self.pending_settings = None;
        self.pending_autoroute = None;
        self.routing_enabled = false;
        self.routing_resolved = false;
        self.routing_generation = self.routing_generation.wrapping_add(1);
        self.routing_updates = JoinSet::new();
        self.managed_active_turns = ManagedActiveTurns::default();
        self.local_managed_turns.clear();
        self.local_terminal_turns.clear();
        self.submitted_turns.clear();
        self.detached_submissions.clear();
        self.unacknowledged_inputs.clear();
        self.confirmed_requests.clear();
        self.steer_receipts.clear();
        self.pending_withdrawals.clear();
        self.withdrawals = JoinSet::new();
        self.cancellation_fences.reset();
        self.cancellation_had_effect = false;
        self.cancellation_failed = false;
        self.history_generation = self.history_generation.wrapping_add(1);
        self.history_loads = JoinSet::new();
        self.history_replays = JoinSet::new();
        self.history_prefetch.reset();
        self.history_tree_open = false;
        self.history_tree_failed = false;
        self.history = HistoryWindow::default();
        self.history_sequences.clear();
        self.history_records.clear();
        self.live_records.clear();
        self.recent_prompts.clear();
        self.shell_context.clear();
        self.sequence = 1;
        self.spawn_connection(ConnectionPurpose::Startup, RetryTarget::Create(settings));
    }

    /// Local mutations must settle before replacing the client; accepted remote
    /// turns need not finish. Local shell commands also retain their terminal.
    fn ready_for_reload(&self) -> bool {
        self.pending_resume.is_none()
            && self.connection.is_empty()
            && self.admissions.is_empty()
            && self.pending_submission.is_none()
            && self.steers.is_empty()
            && self.waiting_steers.is_empty()
            && self.unconfirmed_steer.is_none()
            && self.withdrawals.is_empty()
            && self.cancellations.is_empty()
            && self.settings_updates.is_empty()
            && self.settings_queue.is_empty()
            && self.vault_tasks.is_empty()
            && self.secure_input.is_none()
            && self.secure_input_tasks.is_empty()
            && self.share_tasks.is_empty()
            && self.site_tasks.is_empty()
            && self.done_updates.is_empty()
            && self.voice_tasks.is_empty()
            // Keep local recordings and samples until explicitly submitted or discarded.
            && self.clone_panel.is_none()
            && self.active_shells == 0
    }

    fn idle(&self) -> bool {
        self.pending_resume.is_none()
            && self.recovery.is_none()
            && self.controls.is_empty()
            && self.managed_active_turns.ids.is_empty()
            && self.admissions.is_empty()
            && self.completions.is_empty()
            && self.steers.is_empty()
            && self.withdrawals.is_empty()
            && self.waiting_steers.is_empty()
            && self.unconfirmed_steer.is_none()
            && self.cancellations.is_empty()
            && self.settings_updates.is_empty()
            && self.settings_queue.is_empty()
            && self.share_tasks.is_empty()
            && self.site_tasks.is_empty()
            && self.done_updates.is_empty()
            && self.active_shells == 0
            && self.pending_submission.is_none()
            && self.cancel_after_admission.is_empty()
            && !self.cancellation_fences.has_in_flight()
            && self.connection.len() == self.session_list_cancellations.len()
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
        if self.local.is_some() {
            let control = match &target {
                CancelTarget::Local { id, .. } => self.controls.get(id).cloned(),
                CancelTarget::Managed { .. } => None,
            };
            self.cancellations.spawn(async move {
                let outcome = match control {
                    Some(control) => control
                        .cancel()
                        .await
                        .map(|()| CancelDisposition::Accepted)
                        .map_err(|error| error.to_string()),
                    None => Ok(CancelDisposition::Terminal),
                };
                (pane, target, outcome)
            });
            return;
        }
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

fn connection_failure(error: &NanocodexError) -> bool {
    matches!(
        error,
        NanocodexError::AgentStopped | NanocodexError::TurnStopped
    ) || matches!(error, NanocodexError::Backend { source, .. }
            if matches!(source.downcast_ref::<ManagedError>(), Some(ManagedError::InvalidEvent(_))))
}

fn withdraw_waiting_steer(
    waiting: &mut VecDeque<WaitingSteer>,
    pane: PaneId,
    id: components::QueueId,
) -> bool {
    let Some(index) = waiting
        .iter()
        .position(|(owner, queued, _)| *owner == pane && *queued == id)
    else {
        return false;
    };
    waiting.remove(index);
    true
}

fn take_waiting_steer_failures(
    waiting: &mut VecDeque<WaitingSteer>,
) -> Vec<(PaneId, components::QueueId)> {
    // Each failure keeps its position in the queue, independent of callback order.
    waiting.drain(..).map(|(pane, id, _)| (pane, id)).collect()
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

async fn reconnect_agent(
    client: ManagedClient,
    agent_id: String,
    after: String,
) -> Result<ConnectedAgent, ConnectionFailure> {
    let mut connected = connect_agent(
        client.clone(),
        Some(agent_id.clone()),
        AgentSettings::default(),
    )
    .await?;
    let catch_up = async {
        if let Some(warning) = connected.5.take() {
            return Err(ManagedError::Configuration(warning));
        }
        while connected.4.has_more
            && connected
                .4
                .events
                .first()
                .is_none_or(|event| !cursor_at_or_before(&event.cursor, &after))
        {
            let before = connected
                .4
                .before
                .clone()
                .expect("nonterminal history has a cursor");
            let page = client
                .history(&agent_id, Some(&before), HISTORY_PAGE_SIZE)
                .await?;
            connected.4.prepend(page)?;
        }
        connected
            .4
            .events
            .retain(|event| !cursor_at_or_before(&event.cursor, &after));
        Ok(())
    }
    .await;
    if let Err(error) = catch_up {
        drop(connected.0.disconnect().await);
        return Err(ConnectionFailure {
            error,
            retry: RetryTarget::Agent(agent_id),
        });
    }
    Ok(connected)
}

/// Renderer telemetry and completion notifications shared by every TUI driver.
pub(crate) struct Presentation {
    stream: telemetry::StreamTelemetry,
    view: telemetry::ViewTelemetry,
    pub(crate) notifier: notification::Notifier,
}

impl Presentation {
    pub(crate) fn new() -> Self {
        Self {
            stream: telemetry::StreamTelemetry::default(),
            view: telemetry::ViewTelemetry::default(),
            notifier: notification::Notifier::from_env(),
        }
    }

    /// Record one agent event as it reaches the TUI, before it is applied.
    pub(crate) fn received(&mut self, session_id: &str, event: telemetry::Received) {
        if event.kind() == "turn.failed" {
            self.notifier.turn_failed();
        }
        self.stream.received(session_id, event);
    }

    /// After every presented frame: frame cost, view changes and notifications.
    pub(crate) fn presented(
        &mut self,
        app: &AppNode,
        terminal: &mut TerminalSession,
        session_id: &str,
        render_started: Instant,
        draw: terminal::DrawMetrics,
    ) {
        let main = app.main_pane();
        let fork = app.fork_pane();
        let focused = app.focused_pane();
        let view = telemetry::ViewState {
            split: fork.is_some(),
            focus_main: focused.is_some() && focused == main,
            screen: focused.is_none(),
        };
        self.view.observe(session_id, &view);
        self.stream
            .presented(session_id, &view, render_started, draw);
        let busy = |pane: Option<PaneId>| {
            pane.and_then(|pane| app.root(pane))
                .is_some_and(RootNode::has_active_turns)
        };
        self.notifier.after_frame(terminal, busy(main), busy(fork));
    }
}

pub(crate) async fn run(
    client: &ManagedClient,
    agent_id: Option<String>,
) -> Result<(), ManagedError> {
    run_inner(client, Some(agent_id), None).await
}

pub(crate) async fn run_new(client: &ManagedClient) -> Result<(), ManagedError> {
    run_inner(client, None, None).await
}

/// The local, non-durable TUI (`ncl`, `nanocodex --local`). It runs the same
/// driver as the managed TUI; account-only features are hidden by capability.
pub(crate) async fn run_local(launch: local::agent::LocalLaunch) -> Result<(), ManagedError> {
    // Managed-only effects are hidden in local mode; this unroutable client only
    // backs code paths that capabilities never reach.
    let api_key = nanocodex_managed::ManagedApiKey::parse(format!(
        "ncx_live_{}_{}",
        "0".repeat(12),
        "0".repeat(43)
    ))?;
    let client = ManagedClient::new("http://127.0.0.1:9", api_key)?;
    run_inner(&client, None, Some(launch)).await
}

/// `Some(id)` attaches, `Some(None)` opens the in-TUI picker, and `None` creates.
async fn run_inner(
    client: &ManagedClient,
    attach: Option<Option<String>>,
    local_launch: Option<local::agent::LocalLaunch>,
) -> Result<(), ManagedError> {
    let first_frame = crate::nanocodex2::startup_timing::Stage::new("tui_first_frame");
    let workspace = match &local_launch {
        Some(launch) => launch.args.cwd().to_path_buf(),
        None => HostConfig::load()
            .map_err(|error| ManagedError::Configuration(error.to_string()))?
            .workspace()
            .to_path_buf(),
    };
    // Paint the hosted defaults immediately. New creation uses this same policy;
    // attach hydrates retained settings in connect_agent, where failures already
    // have retry semantics. Optional catalog discovery never gates startup.
    let initial_settings = local_launch
        .as_ref()
        .map(local::settings_from_launch)
        .transpose()
        .map_err(ManagedError::Configuration)?
        .unwrap_or_default();
    let initial_effort = effort_from_thinking(initial_settings.thinking);
    let initial_reasoning_mode = reasoning_mode_from_managed(initial_settings.reasoning_mode);
    let mut root = RootNode::new(&workspace, initial_effort);
    root.set_capabilities(if local_launch.is_some() {
        backend::Capabilities::LOCAL
    } else {
        backend::Capabilities::MANAGED
    });
    root.set_reasoning_modes(initial_reasoning_mode, initial_reasoning_mode);
    root.set_fast_mode(initial_settings.fast_mode);
    root.set_model(initial_settings.model);
    if let Some(launch) = &local_launch {
        root.set_model_catalog(local::model_catalog(launch));
    }

    let mut app = AppNode::new(Theme::default(), workspace.clone(), root);
    let mut reload: Option<crate::nanocodex2::reload::Registration> = None;
    let mut reload_requested = false;
    let _observability = if local_launch.is_none() {
        telemetry::install_observability()
    } else {
        // The local command owns its configured logging/OTLP guard.
        None
    };
    let mut terminal = TerminalSession::enter().await.map_err(terminal_error)?;
    let mut input = EventStream::new();
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut presentation = Presentation::new();
    let (btw_events, mut btw_updates) = mpsc::unbounded_channel();
    let is_local = local_launch.is_some();
    let mut runtime = DriverRuntime {
        control_bridge: None,
        btw: None,
        btw_events,
        screen: screen::Controller::new(None),
        client: client.clone(),
        local: local_launch.map(local::LocalState::new),
        feature_overlay: None,
        feature_instructions: HashMap::new(),
        feature_completions: HashMap::new(),
        feature_turns: HashSet::new(),
        local_voice_status: None,
        harness_relaunching: false,
        queued_relaunch: None,
        pending_voice: None,
        voice_selection: Default::default(),
        voice_tasks: JoinSet::new(),
        clone_panel: None,
        voice: None,
        agent: None,
        startup_attach: matches!(attach, Some(Some(_))),
        pending_resume: None,
        managed_events: None,
        managed_events_open: false,
        recovery: None,
        recovery_events: VecDeque::new(),
        observed_cursor: "0".to_owned(),
        last_recovery: None,
        connection_generation: 0,
        agent_id: String::new(),
        settings: initial_settings,
        pending_settings: None,
        pending_autoroute: None,
        routing_enabled: false,
        routing_resolved: false,
        routing_generation: 0,
        routing_updates: JoinSet::new(),
        workspace: workspace.clone(),
        sequence: 1,
        next_turn: 1,
        next_shell: 1,
        controls: HashMap::new(),
        local_managed_turns: HashMap::new(),
        local_terminal_turns: HashSet::new(),
        submitted_turns: HashSet::new(),
        detached_submissions: HashSet::new(),
        unacknowledged_inputs: HashMap::new(),
        confirmed_requests: HashSet::new(),
        managed_active_turns: ManagedActiveTurns::default(),
        admitting: HashSet::new(),
        cancel_after_admission: HashSet::new(),
        cancellation_fences: CancellationFences::default(),
        cancellation_failed: false,
        cancellation_had_effect: false,
        admissions: JoinSet::new(),
        completions: JoinSet::new(),
        steers: JoinSet::new(),
        receipt_reconciliations: JoinSet::new(),
        unresolved_steers: HashMap::new(),
        vault_tasks: JoinSet::new(),
        secure_input: None,
        secure_input_tasks: JoinSet::new(),
        secure_input_attempted: HashSet::new(),
        share_tasks: JoinSet::new(),
        site_tasks: JoinSet::new(),
        done_updates: JoinSet::new(),
        vault_attempted: HashSet::new(),
        steer_receipts: HashMap::new(),
        pending_withdrawals: HashSet::new(),
        withdrawals: JoinSet::new(),
        pending_steer_target: None,
        waiting_steers: VecDeque::new(),
        unconfirmed_steer: None,
        cancellations: JoinSet::new(),
        settings_updates: JoinSet::new(),
        settings_queue: VecDeque::new(),
        shells: JoinSet::new(),
        links: JoinSet::new(),
        history_loads: JoinSet::new(),
        history_replays: JoinSet::new(),
        history_prefetch: HistoryPrefetch::default(),
        history_tree_open: false,
        history_tree_failed: false,
        history_generation: 0,
        history: HistoryWindow::default(),
        history_sequences: HashMap::new(),
        history_records: Vec::new(),
        live_records: Vec::new(),
        active_shells: 0,
        shell_cancellation: CancellationToken::new(),
        shell_context: Vec::new(),
        pending_submission: None,
        recent_prompts: Vec::new(),
        prompt_cache: PromptCache::for_client(client),
        prompt_cache_writes: JoinSet::new(),
        prompt_cache_pending: Vec::new(),
        prompt_cache_retry_at: None,
        prompt_cache_retries: 1,
        prompt_cache_warned: false,
        recent_prompt_request: 0,
        recent_prompt_loads: HashMap::new(),
        connection: JoinSet::new(),
        session_list_cancellations: HashMap::new(),
        review_branch_loads: JoinSet::new(),
        session_searches: JoinSet::new(),
        session_search_tasks: HashMap::new(),
        retry_target: None,
    };
    // An attached session is not send-ready until retained state and history
    // hydrate. Mark that before the first frame so input cannot race a false
    // idle footer while startup is still connecting.
    if matches!(attach, Some(Some(_))) {
        app.update(AppEvent::AgentConnecting(PaneId::Main));
    }
    // Put the complete interface on screen before any managed request starts.
    terminal
        .draw(|frame| {
            app.render(frame);
            if let Some(flow) = &mut runtime.secure_input {
                flow.render(frame);
            }
        })
        .map_err(terminal_error)?;
    scheduler.presented(Instant::now());
    drop(first_frame);
    let mut catalog_setup = JoinSet::new();
    if !is_local {
        let catalog_client = client.clone();
        catalog_setup.spawn(async move { catalog_client.models().await });
    }
    // An updater can hold reload's coordination lock. Keep registration owned,
    // but wait off the input loop so it becomes available after contention clears.
    // Dropping the JoinSet also drops any uncollected registration and its lease.
    let mut reload_setup = JoinSet::new();
    if !is_local {
        reload_setup.spawn_blocking(crate::nanocodex2::reload::register);
    }
    // Theme and tmux discovery must not delay the first editable frame. These
    // tasks never read stdin; the terminal event stream remains its sole owner.
    let mut presentation_setup = JoinSet::new();
    presentation_setup.spawn(async {
        components::initialize_image_renderer().await;
        None
    });
    presentation_setup.spawn_blocking(detect_system_scheme);
    let mut feature_updates = runtime
        .local
        .as_mut()
        .and_then(|local| local.features.take_updates());
    let initial_prompt = runtime.local.as_mut().and_then(|local| {
        let instruction = local.launch.initial_instruction.take();
        local
            .launch
            .initial_prompt
            .take()
            .map(|text| (text, instruction))
    });
    if let Some((text, instruction)) = initial_prompt {
        // Shown immediately; held as the pending submission until the agent connects.
        let prompt = Submission::text(text);
        let id = TurnId::new(runtime.next_turn);
        runtime.next_turn = runtime.next_turn.saturating_add(1);
        if let Some(instruction) = instruction {
            // A labelled launch prompt is sent like a feature prompt.
            runtime.feature_turns.insert(id);
            runtime.feature_instructions.insert(id, instruction);
        }
        let record = runtime.record_submission(id, &prompt)?;
        request_render(
            app.update(AppEvent::Transcript {
                pane: PaneId::Main,
                record,
            }),
            &mut scheduler,
        );
        runtime.start_submission(PaneId::Main, id, prompt);
    }
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
            request_render(
                app.update(AppEvent::AgentConnecting(PaneId::Main)),
                &mut scheduler,
            );
            runtime.spawn_connection(ConnectionPurpose::Startup, RetryTarget::Agent(agent_id));
        }
        None => {
            runtime.spawn_connection(ConnectionPurpose::Startup, RetryTarget::Default);
        }
    }
    let mut clone_tick = tokio::time::interval(std::time::Duration::from_millis(200));
    clone_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tmux = tmux::Publisher::new();
    let mut tmux_tick = tokio::time::interval(Duration::from_secs(2));
    // Idle tick (Claude scheduler) while local main is idle.
    let mut feature_tick = tokio::time::interval(Duration::from_secs(1));
    feature_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tmux_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stopping = false;
    #[cfg(unix)]
    let mut control_server = if nanocodex_tui_control::Server::enabled() {
        // The local TUI registers as kind "native".
        Some(
            nanocodex_tui_control::Server::start(if runtime.local.is_some() {
                "native"
            } else {
                "managed"
            })
            .map_err(terminal_error)?,
        )
    } else {
        None
    };
    #[cfg(unix)]
    {
        runtime.control_bridge = control_server.as_ref().map(|server| server.bridge.clone());
    }
    let mut control_tasks = JoinSet::new();

    let mut routing_tick = tokio::time::interval(Duration::from_secs(1));
    routing_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !stopping {
        if let Some(error) = runtime.start_queued_relaunch() {
            request_render(
                app.update(AppEvent::NotifyError {
                    pane: PaneId::Main,
                    error,
                }),
                &mut scheduler,
            );
        }
        runtime.update_tree_history(
            app.root(PaneId::Main)
                .is_some_and(RootNode::subagent_overlay_open),
        );
        // Crossterm's reader can prequeue terminal bytes while HTTP is pending.
        // Drain at EVERY private phase boundary before drawing/enabling input.
        if runtime
            .secure_input
            .as_mut()
            .is_some_and(|flow| flow.take_drain())
        {
            drain_private_input(&mut input, &mut runtime).await;
        }
        // Scope changes can come from mouse/control/API/session activity, not
        // just keyboard shortcuts. Wipe before publishing ANY UI snapshot.
        if runtime.secure_input.as_ref().is_some_and(|flow| {
            !flow.scope_matches(
                &runtime.agent_id,
                runtime.connection_generation,
                app.focused_pane(),
            )
        }) {
            runtime.cancel_secure_input();
            scheduler.request_immediate(Instant::now());
        }
        if let Some(bridge) = &runtime.control_bridge {
            control::snapshot(bridge, &app, &runtime, false);
        }
        // Finish thread selection and prompt admission before detaching. The durable
        // managed turn itself continues independently of this terminal.
        if reload_requested && runtime.ready_for_reload() {
            match reload.as_ref().expect("registered reload").preflight() {
                Ok(()) => break,
                Err(error) => {
                    reload_requested = false;
                    request_render(
                        app.update(AppEvent::NotifyError {
                            pane: PaneId::Main,
                            error,
                        }),
                        &mut scheduler,
                    );
                }
            }
        }
        if runtime.recovery == Some(RecoveryPhase::Replaying) && runtime.recovery_events.is_empty()
        {
            runtime.recovery = None;
            // These requests no longer have local workers to reconcile, but their
            // prompts remain in the transcript if admission arrives on the new stream.
            runtime
                .detached_submissions
                .extend(runtime.submitted_turns.drain());
            runtime.confirmed_requests.clear();
            runtime.local_managed_turns.clear();
            runtime.local_terminal_turns.clear();
            request_render(
                app.update(AppEvent::SettingsHydrated {
                    pane: PaneId::Main,
                    effort: effort_from_thinking(runtime.settings.thinking),
                    fast_mode: runtime.settings.fast_mode,
                    model: runtime.settings.model,
                }),
                &mut scheduler,
            );
            let update = app.update(AppEvent::AgentReconnected {
                pane: PaneId::Main,
                active_turns: runtime.managed_active_turns.ids.len(),
                pending_local: runtime.pending_submission.is_some(),
                reasoning_mode: reasoning_mode_from_managed(runtime.settings.reasoning_mode),
            });
            stopping |= apply_update(
                update,
                &mut app,
                &mut runtime,
                &mut terminal,
                &mut scheduler,
            )
            .await?;
            if runtime.active_shells == 0
                && let Some((pane, id, prompt)) = runtime.pending_submission.take()
            {
                runtime.start_submission(pane, id, prompt);
            }
            runtime.refresh_routing();
            runtime.start_history_prefetch(PaneId::Main);
        }
        if runtime.recovery.is_none()
            && runtime
                .unconfirmed_steer
                .as_ref()
                .is_some_and(|(_, generation, target)| {
                    *generation != runtime.connection_generation
                        || !runtime.steer_target_current(target)
                })
        {
            runtime.unconfirmed_steer = None;
        }
        // Admit steering serially. In particular, attached-agent HTTP requests must not
        // overtake one another, and cancellation must still run while an ack is pending.
        while runtime.recovery.is_none()
            && runtime.steers.is_empty()
            && runtime.unconfirmed_steer.is_none()
            && !runtime.waiting_steers.is_empty()
        {
            if !runtime.controls.is_empty() || !runtime.managed_active_turns.ids.is_empty() {
                let (pane, id, prompt) = runtime.waiting_steers.pop_front().unwrap();
                let update = ComponentUpdate {
                    effects: vec![AppEffect::Pane {
                        pane,
                        effect: RootEffect::Steer { id, prompt },
                    }],
                    render: RenderRequest::None,
                };
                stopping |= apply_update(
                    update,
                    &mut app,
                    &mut runtime,
                    &mut terminal,
                    &mut scheduler,
                )
                .await?;
            } else if runtime.admitting.is_empty() && runtime.pending_submission.is_none() {
                for (pane, id) in take_waiting_steer_failures(&mut runtime.waiting_steers) {
                    let update = app.update(AppEvent::SteerFailed { pane, id });
                    stopping |= apply_update(
                        update,
                        &mut app,
                        &mut runtime,
                        &mut terminal,
                        &mut scheduler,
                    )
                    .await?;
                }
            } else {
                break;
            }
        }
        if runtime.voice.is_none()
            && let Some(panel) = &mut runtime.clone_panel
        {
            panel.start_if_ready();
        }
        // A voice request queued during attach must be checked again after
        // retained provider settings arrive, before any media/API session starts.
        if runtime.agent.is_some()
            && runtime.settings.model.oai().is_none()
            && let Some(pending) = runtime.pending_voice.take()
        {
            request_render(
                app.update(AppEvent::NotifyError {
                    pane: pending.pane,
                    error: "Claude currently supports text only; voice input is unavailable".into(),
                }),
                &mut scheduler,
            );
            request_render(
                app.update(AppEvent::VoiceStatus(runtime.voice_status())),
                &mut scheduler,
            );
        }
        if let Some(pending) = runtime.take_ready_voice() {
            match crate::nanocodex2::voice::Session::start_with_settings(
                runtime.client.clone(),
                runtime.agent_id.clone(),
                voice_settings(&pending.selection),
                pending.muted,
            ) {
                Ok(voice) => runtime.voice = Some(voice),
                Err(error) => request_render(
                    app.update(AppEvent::NotifyError {
                        pane: pending.pane,
                        error: error.to_string(),
                    }),
                    &mut scheduler,
                ),
            }
            request_render(
                app.update(AppEvent::VoiceStatus(runtime.voice_status())),
                &mut scheduler,
            );
        }
        if scheduler.is_due(Instant::now()) {
            let render_started = Instant::now();
            let draw = terminal
                .draw(|frame| {
                    app.render(frame);
                    if let Some(overlay) = &mut runtime.feature_overlay {
                        overlay.render(frame, frame.area(), app.theme());
                    }
                    if let Some(flow) = &mut runtime.secure_input {
                        flow.render(frame);
                    }
                })
                .map_err(terminal_error)?;
            presentation.presented(&app, &mut terminal, &runtime.agent_id, render_started, draw);
            runtime.screen.size.send_if_modified(|size| {
                let current = app.screen_size();
                if *size == current {
                    false
                } else {
                    *size = current;
                    true
                }
            });
            scheduler.presented(Instant::now());
        }

        let render_deadline = scheduler.deadline();
        let animation_deadline = app.animation_deadline();
        let prompt_cache_deadline = runtime.prompt_cache_retry_at;
        let (mut voice_status, mut voice_transcripts) =
            runtime.voice.as_mut().map_or((None, None), |voice| {
                (Some(&mut voice.status), Some(&mut voice.transcripts))
            });
        tokio::select! {
            Some(update) = async {
                match feature_updates.as_mut() {
                    Some(updates) => updates.recv().await,
                    None => pending().await,
                }
            } => {
                stopping |= apply_feature_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
            }
            _ = feature_tick.tick(), if runtime.local.is_some() => {
                let idle = runtime.controls.is_empty()
                    && runtime.admitting.is_empty()
                    && runtime.pending_submission.is_none()
                    && runtime.active_shells == 0
                    && runtime.feature_overlay.is_none()
                    && app
                        .root(PaneId::Main)
                        .is_some_and(|root| root.composer().draft().trim().is_empty());
                if idle && let Some(local) = &mut runtime.local {
                    local.with_features(|features, cx| features.idle_tick(cx));
                }
            }
            _ = tmux_tick.tick(), if tmux.is_some() => {
                if let Some(publisher) = &mut tmux && !(runtime.startup_attach && runtime.agent_id.is_empty()) {
                    let status = if runtime.recovery.is_some() { "reconnecting" }
                        else if runtime.agent.is_none() { "connecting" }
                        else if !runtime.managed_events_open { "disconnected" }
                        else if !runtime.controls.is_empty() || !runtime.admitting.is_empty()
                            || !runtime.managed_active_turns.ids.is_empty() { "running" }
                        else { "idle" };
                    let prompt = runtime.recent_prompts.iter()
                        .find(|prompt| prompt.session_id == runtime.agent_id);
                    publisher.publish(&runtime.agent_id, status,
                        prompt.map_or("", |prompt| prompt.text.as_str()),
                        prompt.map_or(0, |prompt| prompt.recorded_at_unix_ms));
                }
            }
            result = reload_setup.join_next(), if !reload_setup.is_empty() => {
                match result {
                    Some(Ok(Ok(registration))) => reload = Some(registration),
                    Some(Ok(Err(error))) => tracing::warn!(%error, "local reload unavailable"),
                    Some(Err(error)) => tracing::warn!(%error, "local reload setup failed"),
                    None => {}
                }
            }
            result = catalog_setup.join_next(), if !catalog_setup.is_empty() => {
                match result {
                    Some(Ok(Ok(catalog))) => {
                        if let Some(root) = app.root_mut(PaneId::Main) {
                            root.set_model_catalog(catalog.data);
                        }
                        scheduler.request_immediate(Instant::now());
                    }
                    Some(Ok(Err(error))) => tracing::warn!(%error, "managed catalog unavailable"),
                    Some(Err(error)) => tracing::warn!(%error, "managed catalog discovery failed"),
                    None => {}
                }
            }
            result = presentation_setup.join_next(), if !presentation_setup.is_empty() => {
                if let Some(Ok(Some(scheme))) = result {
                    request_render(app.update(AppEvent::SystemThemeChanged(scheme)), &mut scheduler);
                } else {
                    scheduler.request_immediate(Instant::now());
                }
            }
            command = async {
                #[cfg(unix)]
                if let Some(server) = &mut control_server {
                    return server.commands.recv().await;
                }
                pending::<Option<nanocodex_tui_control::Command>>().await
            } => {
                if let Some(command) = command {
                    if runtime.local.is_some() {
                        // Local control: rollout history and the in-process agent.
                        let bridge = runtime.control_bridge.clone().unwrap();
                        if let Some(update) = local::control::dispatch(command, &bridge, &mut runtime, &mut app, &mut control_tasks) {
                            stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                    } else {
                        control::dispatch(command, runtime.control_bridge.as_ref().unwrap(), &runtime, &mut control_tasks);
                    }
                }
            }
            Some(result) = control_tasks.join_next(), if !control_tasks.is_empty() => {
                if let Ok((command, result, settings, session)) = result {
                    if session == runtime.agent_id && let Some(settings) = settings {
                        runtime.settings = settings;
                        if command.request.params["settings"]["model"].is_string() && let Some(root) = app.root_mut(PaneId::Main) {
                            let mode = reasoning_mode_from_managed(settings.reasoning_mode);
                            root.set_reasoning_modes(mode, mode);
                        }
                        request_render(app.update(AppEvent::SettingsHydrated {pane:PaneId::Main,
                            effort:effort_from_thinking(settings.thinking),fast_mode:settings.fast_mode,model:settings.model}), &mut scheduler);
                    }
                    // Publish readiness and revisions before a client can act on this acknowledgement.
                    if let Some(bridge) = &runtime.control_bridge {
                        control::snapshot(bridge, &app, &runtime, false);
                    }
                    command.finish(result);
                }
            }
            _ = routing_tick.tick(), if runtime.routing_enabled && !runtime.routing_resolved
                && (!runtime.controls.is_empty() || !runtime.admitting.is_empty() || !runtime.managed_active_turns.ids.is_empty()) => {
                runtime.refresh_routing();
            }
            result = runtime.routing_updates.join_next(), if !runtime.routing_updates.is_empty() => {
                if let Some(Ok((agent_id, generation, Ok(status)))) = result
                    && agent_id == runtime.agent_id && generation == runtime.routing_generation
                {
                    runtime.routing_enabled = status.enabled;
                    runtime.routing_resolved = status.route.is_some();

                    let automatic = status.automatic.unwrap_or(status.enabled);
                    let (provider, model, effort) = status.route.map_or((None, None, None), |route| {
                        runtime.settings.model = route.model;
                        runtime.settings.thinking = route.thinking;
                        (Some(route.backend.label().to_owned()), Some(route.model), Some(effort_from_thinking(route.thinking)))
                    });
                    request_render(app.update(AppEvent::RoutingHydrated { pane: PaneId::Main,
                        enabled: automatic, provider, model, effort }), &mut scheduler);
                }
            }
            _ = clone_tick.tick(), if runtime.clone_panel.as_ref().is_some_and(|panel| matches!(panel.state, voice_clone::State::Recording(_))) => {
                let panel = runtime.clone_panel.as_mut().unwrap();
                let stop_reason = match &mut panel.state {
                    voice_clone::State::Recording(recorder) if recorder.elapsed().as_secs() >= crate::nanocodex2::voice_recording::MAX_SECONDS => Some("Reached the 2-minute recording limit"),
                    voice_clone::State::Recording(recorder) => match recorder.is_finished() {
                        Ok(true) if recorder.elapsed().as_secs() >= crate::nanocodex2::voice_recording::MAX_SECONDS - 1 => Some("Reached the 2-minute recording limit"),
                        Ok(true) => Some("Microphone recorder ended early; R records a new sample"),
                        Err(_) => Some("Microphone recorder stopped unexpectedly; R retries"),
                        Ok(false) => None,
                    },
                    _ => None,
                };
                if let Some(reason) = stop_reason { let _ = panel.stop_with_reason(reason); }
                request_render(app.update(AppEvent::VoiceStatus(runtime.voice_status())), &mut scheduler);
            }
            Some(completion) = async {
                match runtime.clone_panel.as_mut() {
                    Some(panel) if !panel.tasks.is_empty() => panel.tasks.join_next().await,
                    _ => pending().await,
                }
            } => {
                let message = match completion {
                    Ok(Ok(state)) => {
                        let panel = runtime.clone_panel.as_mut().unwrap();
                        panel.state = match state {
                            voice_clone::State::PlaybackFailed(sample, error) => { panel.error = Some(error); voice_clone::State::Review(sample) }
                            other => other,
                        };
                        panel.review().unwrap_or_else(|_| panel.text())
                    }
                    result => {
                        let message = match result { Ok(Err(error)) => error, _ => "Local recording task failed; recording discarded.".into() };
                        if let Some(panel) = &mut runtime.clone_panel {
                            panel.state = voice_clone::State::Ready;
                            panel.error = Some(format!("{message}\nPress R to retry, or Esc to cancel."));
                        }
                        message
                    }
                };
                request_render(app.update(AppEvent::VoiceStatus(runtime.voice_status())), &mut scheduler);
                request_render(app.update(AppEvent::VoiceOutput { pane: PaneId::Main, text: message }), &mut scheduler);
            }
            Some(completion) = runtime.voice_tasks.join_next(), if !runtime.voice_tasks.is_empty() => {
                let event = match completion {
                    Ok((pane, Ok(text))) => AppEvent::VoiceOutput { pane, text },
                    Ok((pane, Err(text))) => AppEvent::VoiceOutput { pane, text },
                    Err(_) => AppEvent::NotifyError { pane: PaneId::Main, error: "Voice operation failed".into() },
                };
                request_render(app.update(event), &mut scheduler);
            }
            result = async { match &mut reload {
                Some(registration) => registration.requested().await,
                None => pending().await,
            } }, if !reload_requested => {
                if let Err(error) = result {
                    reload = None;
                    request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main, error }), &mut scheduler);
                    continue;
                }
                reload_requested = true;
                request_render(app.update(AppEvent::NotifySuccess {
                    pane: PaneId::Main,
                    message: "Reloading after pending local operations finish…".into(),
                }), &mut scheduler);
            }
            Some(completion) = runtime.secure_input_tasks.join_next(), if !runtime.secure_input_tasks.is_empty() => {
                if let Ok((agent_id, generation, request_id, outcome)) = completion {
                    if !vault::scope_matches(&agent_id, generation, &runtime.agent_id, runtime.connection_generation) { continue; }
                    if let secure_input::Outcome::Sudo(sudo_input::Outcome::Status(status)) = &outcome {
                        let update = app.update(AppEvent::SecureInputReceipt { pane: PaneId::Main, request_id: request_id.clone(), status: *status });
                        stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                    }
                    if let secure_input::Outcome::Private(private_input::Outcome::Receipt(receipt,_))=&outcome {
                        let update=app.update(AppEvent::VaultReceipt{pane:PaneId::Main,receipt:receipt.clone()});
                        stopping |= apply_update(update,&mut app,&mut runtime,&mut terminal,&mut scheduler).await?;
                    }
                    if let Some(flow) = &mut runtime.secure_input
                        && flow.request().id() == request_id && flow.generation() == generation {
                        flow.finish(outcome);
                        scheduler.request_immediate(Instant::now());
                    }
                } else if let Some(flow) = &mut runtime.secure_input {
                    flow.cancel_local();
                    scheduler.request_immediate(Instant::now());
                }
            }
            Some(completion) = runtime.vault_tasks.join_next(), if !runtime.vault_tasks.is_empty() => {
                if let Ok((pane, agent_id, generation, result)) = completion {
                    if !vault::scope_matches(&agent_id, generation, &runtime.agent_id, runtime.connection_generation) {
                        request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main, error: "Vault request finished after changing conversations. Check your Vault before continuing.".into() }), &mut scheduler);
                        continue;
                    }
                    let update = match result {
                        Ok(vault::Outcome::Review(review)) => app.update(AppEvent::VaultReview { pane, review }),
                        Ok(vault::Outcome::Saved(receipt)) => app.update(AppEvent::VaultReceipt { pane, receipt }),
                        Err(error) => app.update(AppEvent::NotifyError { pane, error }),
                    };
                    stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                }
            }
            changed = runtime.screen.updates.changed() => {
                if changed.is_ok() {
                    let snapshot = runtime.screen.updates.borrow_and_update().clone();
                    request_render(app.update(AppEvent::Screen(snapshot)), &mut scheduler);
                }
            }
            Some(transcript) = async { match &mut voice_transcripts { Some(receiver) => receiver.recv().await, None => pending().await } } => {
                // A stopped/replaced session may still have queued final captions.
                // Do not present them as speech from the newly selected voice.
                if runtime.voice.as_ref().is_some_and(crate::nanocodex2::voice::Session::accepting_transcripts) {
                    let record = runtime.local_record(LocalEvent::VoiceTranscript(transcript))?;
                    request_render(app.update(AppEvent::Transcript { pane: PaneId::Main, record }), &mut scheduler);
                }
            }
            changed = async { match &mut voice_status { Some(receiver) => receiver.changed().await, None => pending().await } } => {
                let voice = runtime.voice.as_mut().unwrap();
                let status = voice.status.borrow_and_update().clone();
                let finished = changed.is_err() || status.finished;
                if finished {
                    if status.text.contains("cleanup unconfirmed") || changed.is_err() && !status.finished {
                        runtime.clone_panel = None;
                        runtime.pending_voice = None;
                    }
                    let mut voice = runtime.voice.take().unwrap();
                    while let Ok(transcript) = voice.transcripts.try_recv() {
                        if voice.accepting_transcripts() {
                            let record = runtime.local_record(LocalEvent::VoiceTranscript(transcript))?;
                            request_render(app.update(AppEvent::Transcript { pane: PaneId::Main, record }), &mut scheduler);
                        }
                    }
                }
                let update = app.update(AppEvent::VoiceStatus(runtime.voice_status()));
                request_render(update, &mut scheduler);
                if finished {
                    let event = if status.text.starts_with("Voice failed") || status.text.contains("cleanup unconfirmed") { AppEvent::NotifyError {pane: PaneId::Main, error: status.text} } else { AppEvent::NotifySuccess {pane: PaneId::Main, message: status.text} };
                    request_render(app.update(event), &mut scheduler);
                }
            }
            input_event = input.next() => {
                let event = input_event
                    .transpose()
                    .map_err(terminal_error)?
                    .ok_or_else(|| terminal_error(io::Error::new(io::ErrorKind::UnexpectedEof, "terminal input closed")))?;
                presentation.notifier.observe_event(&event);
                // SECURITY: intercept BEFORE ordinary AppEvent, clipboard,
                // screen, composer, shell, debug/control, export or history.
                if let Some(flow) = &mut runtime.secure_input {
                    let action = flow.intercept(event);
                    let drain = flow.take_drain();
                    if drain { drain_private_input(&mut input, &mut runtime).await; }
                    runtime.secure_input_action(action);
                    scheduler.request_immediate(Instant::now());
                    continue;
                }
                // A feature overlay (Claude interaction, branch navigator) is modal.
                if let Some(overlay) = &mut runtime.feature_overlay {
                    match &event {
                        Event::Key(key) if key.kind != KeyEventKind::Release => {
                            if overlay.key(*key) == features::OverlayOutcome::Close {
                                runtime.feature_overlay = None;
                                if let Some(local) = &mut runtime.local {
                                    local.features.overlay_state(false);
                                }
                            }
                            scheduler.request_immediate(Instant::now());
                            continue;
                        }
                        Event::Paste(text) => {
                            overlay.paste(text);
                            scheduler.request_immediate(Instant::now());
                            continue;
                        }
                        _ => {}
                    }
                }
                if let (Some(local), Event::Key(key)) = (&mut runtime.local, &event)
                    && key.kind == KeyEventKind::Press
                    && local.with_features(|features, cx| features.key(key, cx))
                        == features::KeyOutcome::Consumed
                {
                    scheduler.request_immediate(Instant::now());
                    continue;
                }
                // Mute remains global while another pane or a modal has focus.
                if (runtime.voice.is_some() || runtime.pending_voice.is_some())
                    && matches!(&event, Event::Key(key) if key.code == KeyCode::Char('x') && key.modifiers == KeyModifiers::CONTROL)
                {
                    if matches!(&event, Event::Key(key) if key.kind == KeyEventKind::Press) {
                        let _ = runtime.voice_command(PaneId::Main, crate::nanocodex2::voice::Command::ToggleMute);
                        request_render(app.update(AppEvent::VoiceStatus(runtime.voice_status())), &mut scheduler);
                    }
                    continue;
                }
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
                if let Some(event) = runtime.recovery_events.pop_front() { return Some(event); }
                match runtime.managed_events.as_mut() {
                    Some(events) => events.recv().await,
                    None => pending().await,
                }
            }, if runtime.managed_events_open => {
                match event {
                    Some(event) => {
                        presentation.received(&runtime.agent_id, telemetry::Received::managed(&event));
                        if let Some(bridge) = &runtime.control_bridge { let mut value=serde_json::to_value(&event).unwrap_or_default(); value["session_id"]=serde_json::json!(runtime.agent_id); bridge.publish("managed.event",value); }
                        runtime.observed_cursor.clone_from(&event.cursor);
                        if let Some(request_id) = event.data.turn_id() {
                            if runtime.submitted_turns.contains(request_id) {
                                runtime.confirmed_requests.insert(request_id.to_owned());
                            }
                            let update = app.update(AppEvent::PromptConfirmed { pane: PaneId::Main, request_id: request_id.to_owned() });
                            stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                        if runtime.recovery == Some(RecoveryPhase::Replaying) {
                            // The state snapshot already includes these historical transitions.
                            // Replay their transcript without changing its current active-turn set.
                            if let Some((record, prompt)) = runtime.project_managed_event(event)? {
                                runtime.live_records.push(Arc::clone(&record));
                                if let Some(prompt) = prompt { runtime.cache_prompts(vec![prompt.clone()]); runtime.recent_prompts.insert(0, prompt); }
                                let update = app.update(AppEvent::ExternalTranscript { pane: PaneId::Main, record });
                                stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                            }
                            continue;
                        }
                        // The durable envelope is authoritative even when a failed runtime
                        // could not publish its nested run terminal. Keep caller IDs after
                        // completion because the worker result may win this race.
                        let local_terminal = matches!(
                            &event.data,
                            ManagedEventData::TurnCompleted { id, .. }
                                | ManagedEventData::TurnCancelled { id }
                                | ManagedEventData::TurnFailed { id, .. }
                                if runtime.submitted_turns.contains(id)
                        );
                        match &event.data {
                            ManagedEventData::TurnCompleted { id, .. }
                            | ManagedEventData::TurnCancelled { id }
                            | ManagedEventData::TurnFailed { id, .. } => {
                                if runtime.routing_enabled && !runtime.routing_resolved {
                                    runtime.routing_updates = JoinSet::new();
                                    runtime.refresh_routing();
                                }
                                runtime.cancellation_fences.managed_terminal(id);
                                if let Some(local_id) = runtime
                                    .local_managed_turns
                                    .iter()
                                    .find_map(|(local_id, managed_id)| {
                                        (managed_id == id).then_some(*local_id)
                                    })
                                {
                                    runtime.local_terminal_turns.insert(local_id);
                                    runtime.cancellation_fences.local_terminal(local_id);
                                }
                            }
                            _ => {}
                        }
                        let observation = runtime
                            .managed_active_turns
                            .observe(&event, &runtime.local_managed_turns);
                        if observation.active_changed {
                            let update = app.update(AppEvent::ManagedActiveTurns {
                                pane: PaneId::Main,
                                count: runtime.managed_active_turns.ids.len(),
                            });
                            stopping |= apply_update(
                                update,
                                &mut app,
                                &mut runtime,
                                &mut terminal,
                                &mut scheduler,
                            ).await?;
                        }
                        if let Some((record, prompt)) = runtime.project_managed_event(event)? {
                            runtime.live_records.push(Arc::clone(&record));
                            if let Some(prompt) = prompt {
                                runtime.cache_prompts(vec![prompt.clone()]);
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
                        if local_terminal {
                            let update = app.update(AppEvent::ManagedTurnFinished(PaneId::Main));
                            stopping |= apply_update(
                                update,
                                &mut app,
                                &mut runtime,
                                &mut terminal,
                                &mut scheduler,
                            ).await?;
                        }
                    }
                    None => runtime.begin_recovery(&mut app, &mut scheduler, true),
                }
            }
            Some(result) = runtime.prompt_cache_writes.join_next(), if !runtime.prompt_cache_writes.is_empty() => {
                let error = match result {
                    Ok((_, Ok(()))) => {
                        runtime.start_prompt_cache_write();
                        None
                    }
                    Ok((mut batch, Err(error))) => {
                        batch.append(&mut runtime.prompt_cache_pending);
                        runtime.prompt_cache_pending = prompt_cache::bounded(batch);
                        if runtime.prompt_cache_retries > 0 {
                            runtime.prompt_cache_retries -= 1;
                            runtime.prompt_cache_retry_at = Some(Instant::now() + Duration::from_millis(250));
                        }
                        Some(error)
                    }
                    Err(_) => Some("Prompt history task failed".to_owned()),
                };
                if let Some(error) = error && !runtime.prompt_cache_warned {
                    runtime.prompt_cache_warned = true;
                    request_render(app.update(AppEvent::NotifyError {
                        pane: PaneId::Main, error: format!("Could not save recent prompts: {error}"),
                    }), &mut scheduler);
                }
            }
            () = wait_until(prompt_cache_deadline), if prompt_cache_deadline.is_some() => {
                runtime.prompt_cache_retry_at = None;
                runtime.start_prompt_cache_write();
            }
            Some(event) = btw_updates.recv() => {
                let pane = match &event {
                    btw::Event::Ready { pane, .. } | btw::Event::Record { pane, .. } | btw::Event::Finished(pane) | btw::Event::Failed { pane, .. } => *pane,
                };
                if runtime.btw.as_ref().is_none_or(|btw| btw.pane != pane) { continue; }
                let update = match event {
                    btw::Event::Ready { pane, agent_id, settings } => {
                        if let Some(btw) = &mut runtime.btw { btw.agent_id = Some(agent_id); }
                        // Ready first: hydration restores idle activity, and the side pane
                        // must leave its "Opening /btw" state exactly once.
                        let ready = app.update(AppEvent::ForkReady { pane });
                        let update = app.update(AppEvent::SettingsHydrated { pane,
                            effort: effort_from_thinking(settings.thinking), fast_mode: settings.fast_mode,
                            model: settings.model });
                        request_render(update, &mut scheduler);
                        ready
                    }
                    btw::Event::Record { pane, record } => {
                        if record.source() == "tact" && matches!(record.kind(), "user.submitted" | "user.steered") {
                            #[derive(serde::Deserialize)]
                            struct CachedInput { text: String }
                            if let Ok(input) = record.decode_payload::<CachedInput>() {
                                runtime.cache_prompts(vec![RecentPrompt {
                                    text: vault::receipt_summary(&input.text).unwrap_or(input.text),
                                    recorded_at_unix_ms: record.recorded_at_unix_ms(),
                                    session_id: runtime.pane_session_id(pane), workspace: runtime.workspace.clone(),
                                }]);
                            }
                        }
                        app.update(AppEvent::Transcript { pane, record })
                    },
                    btw::Event::Finished(pane) => app.update(AppEvent::WorkerTurnFinished { pane, terminal_expected: false }),
                    btw::Event::Failed { pane, error, opening: true } => {
                        runtime.btw.take();
                        app.update(AppEvent::ForkFailed { pane, error })
                    }
                    btw::Event::Failed { pane, error, opening: false } => app.update(AppEvent::NotifyError { pane, error }),
                };
                stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
            }
            Some(result) = runtime.review_branch_loads.join_next(), if !runtime.review_branch_loads.is_empty() => {
                if let Ok(loaded) = result {
                    request_render(app.update(AppEvent::ReviewBranchesLoaded {
                        pane: loaded.pane, request_id: loaded.request_id, result: loaded.result,
                    }), &mut scheduler);
                }
            }
            Some(result) = runtime.session_searches.join_next(), if !runtime.session_searches.is_empty() => {
                if let Ok(search) = result {
                    request_render(app.update(AppEvent::SessionSearchResults {
                        pane: search.pane, picker_id: search.picker_id, request_id: search.request_id,
                        query: search.query, result: search.result,
                    }), &mut scheduler);
                }
            }
            result = runtime.connection.join_next_with_id(), if !runtime.connection.is_empty() => {
                if let Some(result) = result {
                    let (task_id, result) = match result {
                        Ok(result) => result,
                        Err(error) => {
                            let message =
                                format!("Managed connection task stopped unexpectedly: {error}");
                            if let Some(pane) = runtime.finish_resume(error.id()) {
                                request_render(app.update(AppEvent::SessionLoadFailed { pane, error: message }), &mut scheduler);
                                if !runtime.managed_events_open {
                                    runtime.begin_recovery(&mut app, &mut scheduler, true);
                                }
                                continue;
                            }
                            if error.is_cancelled() {
                                continue;
                            }
                            runtime.pending_voice = None;
                            request_render(app.update(AppEvent::VoiceStatus(runtime.voice_status())), &mut scheduler);
                            if runtime.recovery == Some(RecoveryPhase::Connecting) {
                                runtime.recovery = Some(RecoveryPhase::Disconnected);
                                request_render(app.update(AppEvent::AgentReconnectFailed { pane: PaneId::Main, error: message }), &mut scheduler);
                                continue;
                            }
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
                    if matches!(&result, ConnectionResult::Agent { purpose: ConnectionPurpose::Resume(_) | ConnectionPurpose::Bug(_), .. })
                        && runtime.finish_resume(task_id).is_none()
                    {
                        // Abort cannot retract a result already queued by JoinSet.
                        // Only the still-selected resume task may install its agent.
                        if let ConnectionResult::Agent { result: Ok((agent, ..)), .. } = result {
                            runtime.connection.spawn(async move {
                                ConnectionResult::Disconnected(agent.disconnect().await)
                            });
                        }
                        continue;
                    }
                    match result {
                        ConnectionResult::Recovered(Ok((agent, events, agent_id, workspace, history, _, settings, _, active_turns))) => {
                            runtime.agent = Some(agent);
                            if runtime.agent_id != agent_id {
                                runtime.voice.take();
                                runtime.clone_panel = None;
                                request_render(app.update(AppEvent::VoiceStatus(None)), &mut scheduler);
                            }
                            runtime.agent_id = agent_id;
                            runtime.workspace = workspace;
                            runtime.settings = settings;
                            runtime.managed_events = Some(events);
                            runtime.managed_events_open = true;
                            runtime.managed_active_turns = active_turns;
                            runtime.cancellation_fences.reset();
                            runtime.cancellation_had_effect = false;
                            runtime.cancellation_failed = false;
                            for request_id in &runtime.managed_active_turns.ids {
                                request_render(app.update(AppEvent::PromptConfirmed { pane: PaneId::Main, request_id: request_id.clone() }), &mut scheduler);
                            }
                            runtime.recovery_events = history.events.into();
                            runtime.recovery = Some(RecoveryPhase::Replaying);
                        }
                        ConnectionResult::Recovered(Err(failure)) => {
                            runtime.pending_voice = None;
                            request_render(app.update(AppEvent::VoiceStatus(runtime.voice_status())), &mut scheduler);
                            runtime.recovery = Some(RecoveryPhase::Disconnected);
                            request_render(app.update(AppEvent::AgentReconnectFailed {
                                pane: PaneId::Main,
                                error: format!("Could not reconnect: {}. Press Enter to retry.", failure.error),
                            }), &mut scheduler);
                        }
                        ConnectionResult::RecentPrompts { pane, request_id, session_id, local, result } => {
                            if runtime.recent_prompt_loads.get(&pane) != Some(&request_id)
                                || runtime.pane_session_id(pane) != session_id { continue; }
                            runtime.recent_prompt_loads.remove(&pane);
                            if app.root(pane).is_none_or(|root| !root.recent_prompts_loading()) { continue; }
                            let prompts = match result {
                                Ok(prompts) => prompts,
                                Err(error) => {
                                    request_render(app.update(AppEvent::NotifyError { pane,
                                        error: format!("Saved prompt history unavailable: {error}"),
                                    }), &mut scheduler);
                                    local
                                }
                            };
                            let update = app.update(AppEvent::RecentPromptsLoaded { pane, session_id, prompts });
                            stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                        ConnectionResult::LocalSessions { pane, request_id, result } => {
                            let cancelled = runtime.session_list_cancellations.remove(&(pane, request_id))
                                .is_none_or(|token| token.is_cancelled());
                            if cancelled { continue; }
                            let Some(result) = result else { continue; };
                            let update = match result {
                                Ok(sessions) => app.update(AppEvent::SessionsLoaded { pane, request_id, sessions }),
                                Err(error) => app.update(AppEvent::SessionListFailed {
                                    pane,
                                    request_id,
                                    error: format!("Could not load local sessions: {error}"),
                                }),
                            };
                            stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                        ConnectionResult::Sessions { pane, request_id, result } => {
                            let cancelled = runtime.session_list_cancellations.remove(&(pane, request_id))
                                .is_none_or(|token| token.is_cancelled());
                            if cancelled { continue; }
                            let Some(result) = result else { continue; };
                            let update = match result {
                                Ok(list) => app.update(AppEvent::SessionsLoaded {
                                    pane,
                                    request_id,
                                    sessions: session_summaries(&list, &runtime.workspace),
                                }),
                                Err(error) => app.update(AppEvent::SessionListFailed {
                                    pane,
                                    request_id,
                                    error: format!("Could not load managed sessions: {error}"),
                                }),
                            };
                            stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                        }
                        ConnectionResult::Agent { purpose, result: Ok((agent, managed_events, agent_id, workspace, history, warning, settings, created, active_turns)) } => {
                            if let Some(local) = &mut runtime.local {
                                local.adopt();
                                if let Some(root) = app.root_mut(PaneId::Main) {
                                    root.set_capabilities(local.capabilities());
                                }
                            }
                            if runtime.queued_relaunch.is_none() {
                                runtime.harness_relaunching = false;
                            }
                            if matches!(purpose, ConnectionPurpose::Bug(_)) {
                                runtime.detach_bug_source();
                            }
                            runtime.startup_attach = false;
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
                            runtime.history_tree_open = false;
                            runtime.history_tree_failed = false;
                            if !matches!(purpose, ConnectionPurpose::Startup) {
                                // Unconsumed local output belongs to the previous session.
                                // Preserve it until a resume succeeds, then drop it with
                                // that session's local transcript.
                                runtime.shell_context.clear();
                                runtime.history_sequences.clear();
                                runtime.history_records.clear();
                                runtime.live_records.clear();
                                runtime.submitted_turns.clear();
                                runtime.detached_submissions.clear();
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
                                for prompt in &mut live_prompts {
                                    if prompt.session_id.is_empty() {
                                        prompt.session_id.clone_from(&agent_id);
                                    }
                                }
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
                            runtime.steer_receipts.clear();
                            runtime.receipt_reconciliations = JoinSet::new();
                            runtime.unresolved_steers.clear();
                            runtime.pending_withdrawals.clear();
                            runtime.withdrawals = JoinSet::new();
                            runtime.managed_events = Some(managed_events);
                            runtime.managed_events_open = true;
                            if runtime.agent_id != agent_id {
                                runtime.voice.take();
                                runtime.clone_panel = None;
                                request_render(app.update(AppEvent::VoiceStatus(None)), &mut scheduler);
                            }
                            tracing::info!(
                                pid = std::process::id(),
                                session.id = %agent_id,
                                workspace = %workspace.display(),
                                "TUI session connected"
                            );
                            runtime.agent_id = agent_id;
                            runtime.settings = settings;
                            runtime.workspace = workspace;
                            runtime.managed_active_turns = active_turns;
                            runtime.cancellation_fences.reset();
                            runtime.cancellation_had_effect = false;
                            runtime.cancellation_failed = false;
                            runtime.next_turn = runtime.next_turn.max(runtime.sequence);
                            runtime.cache_prompts(prompts.clone());
                            runtime.recent_prompts = prompts;
                            runtime.observed_cursor = history.events.last().map_or_else(|| "0".to_owned(), |event| event.cursor.clone());
                            runtime.history = history;
                            runtime.history_records = history_records;
                            let pane = match purpose {
                                ConnectionPurpose::Startup => PaneId::Main,
                                ConnectionPurpose::Resume(pane) | ConnectionPurpose::Bug(pane) => pane,
                            };
                            let update = match purpose {
                                ConnectionPurpose::Startup if created => {
                                    // This is the session already shown locally. Hydrate its
                                    // settings without resetting prompts, cancellations, shell
                                    // output, or a draft entered while creation was in flight.
                                    app.update(AppEvent::SettingsHydrated {
                                        pane,
                                        effort: effort_from_thinking(display_settings.thinking),
                                        fast_mode: display_settings.fast_mode,
                                        model: display_settings.model,
                                    })
                                }
                                ConnectionPurpose::Startup | ConnectionPurpose::Resume(_) | ConnectionPurpose::Bug(_) => {
                                    let effort = effort_from_thinking(settings.thinking);
                                    let reasoning_mode =
                                        reasoning_mode_from_managed(settings.reasoning_mode);
                                    // This history belongs to the live stream installed above.
                                    // Closing it would fail active tools and erase retry status.
                                    let projection = RootNode::project_open_session(effort, records);
                                    app.update(AppEvent::SessionRestored {
                                        pane,
                                        draft_reset: match purpose {
                                            ConnectionPurpose::Startup => DraftReset::Preserve,
                                            ConnectionPurpose::Resume(_) | ConnectionPurpose::Bug(_) => DraftReset::Clear,
                                        },
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
                            if matches!(purpose, ConnectionPurpose::Bug(_)) {
                                request_render(app.update(AppEvent::NotifySuccess {
                                    pane,
                                    message: format!("Debugging Nanocodex in cloud agent {}", runtime.agent_id),
                                }), &mut scheduler);
                            }
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
                            if let Some(pane) = runtime.pending_autoroute.take() {
                                runtime.queue_settings(pane, SettingsMutation::AutoRoute);
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
                            // Live creation accepts fixed model settings only;
                            // routing starts disabled. Existing threads still
                            // hydrate their durable routing configuration.
                            if !created {
                                runtime.refresh_routing();
                            }
                            runtime.start_history_prefetch(pane);
                        }
                        ConnectionResult::Agent { purpose, result: Err(failure) } => {
                            // A failed relaunch keeps the running agent; a newer
                            // queued selection still holds its prompts.
                            let relaunch_queued = runtime.queued_relaunch.is_some();
                            if !relaunch_queued {
                                runtime.harness_relaunching = false;
                            }
                            runtime.pending_voice = None;
                            request_render(app.update(AppEvent::VoiceStatus(runtime.voice_status())), &mut scheduler);
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
                                ConnectionPurpose::Startup if runtime.startup_attach => app.update(AppEvent::AgentReconnectFailed {
                                    pane: PaneId::Main,
                                    error: message.clone(),
                                }),
                                ConnectionPurpose::Startup => app.update(AppEvent::NotifyError {
                                    pane: PaneId::Main,
                                    error: message.clone(),
                                }),
                                ConnectionPurpose::Bug(pane) => app.update(AppEvent::NotifyError { pane, error: message.clone() }),
                                ConnectionPurpose::Resume(pane) => app.update(AppEvent::SessionLoadFailed {
                                    pane,
                                    error: message.clone(),
                                }),
                            };
                            request_render(update, &mut scheduler);
                            if matches!(purpose, ConnectionPurpose::Resume(_) | ConnectionPurpose::Bug(_)) && !runtime.managed_events_open {
                                runtime.begin_recovery(&mut app, &mut scheduler, true);
                            }
                            if matches!(purpose, ConnectionPurpose::Bug(_)) {
                                continue;
                            }
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
                                && !relaunch_queued
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
            Some(result) = runtime.done_updates.join_next(), if !runtime.done_updates.is_empty() => {
                match result {
                    Ok((agent_id, _generation, done, result)) if runtime.agent_id == agent_id => {
                        let event = match result {
                            Ok(receipt) if receipt.done == done => AppEvent::NotifySuccess { pane: PaneId::Main, message: if done {
                                "Marked done · hidden from continue. /undone restores it; history and running work are unchanged.".into()
                            } else { "Session restored to continue.".into() } },
                            Ok(_) | Err(_) => AppEvent::NotifyError { pane: PaneId::Main, error: "Session change could not be confirmed. Check the session list before retrying; no automatic retry was made.".into() },
                        };
                        request_render(app.update(event), &mut scheduler);
                    }
                    Ok(_) => {},
                    Err(_) => request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main,
                        error: "Session change stopped unexpectedly. Check the session list before retrying.".into() }), &mut scheduler),
                }
            }
            Some(result) = runtime.share_tasks.join_next(), if !runtime.share_tasks.is_empty() => {
                match result {
                    Ok((pane, agent_id, generation, outcome)) if runtime.agent_id == agent_id
                        && runtime.connection_generation == generation => {
                        match outcome {
                            Ok(share::Outcome::Created(receipt)) => {
                                // Do not put a bearer URL in notification, transcript, or logs.
                                let copied = clipboard::copy_text(&receipt.url).is_ok();
                                request_render(app.update(AppEvent::ShareOutput { pane, text: receipt.url }), &mut scheduler);
                                request_render(app.update(AppEvent::NotifySuccess { pane, message: if copied {
                                    "Share link copied. Keep it private; press c to copy again or Esc to close.".into()
                                } else {
                                    "Share link created. Clipboard unavailable; press c in the link panel to retry.".into()
                                } }), &mut scheduler);
                            }
                            Ok(share::Outcome::Listed(links)) => request_render(
                                app.update(AppEvent::ShareOutput { pane, text: share::list_text(&links) }), &mut scheduler),
                            Ok(share::Outcome::Revoked) => request_render(
                                app.update(AppEvent::NotifySuccess { pane, message: "Share link revoked.".into() }), &mut scheduler),
                            Err(error) => request_render(
                                app.update(AppEvent::NotifyError { pane, error: share::error(&error) }), &mut scheduler),
                        }
                    }
                    Ok(_) => {}, // No bearer URL from a previous thread may appear in the current one.
                    Err(_) => request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main,
                        error: "Share request stopped unexpectedly. Check /share list before retrying a mutation.".into() }), &mut scheduler),
                }
            }
            Some(result) = runtime.site_tasks.join_next(), if !runtime.site_tasks.is_empty() => {
                match result {
                    Ok((pane, agent_id, generation, outcome)) if runtime.agent_id == agent_id
                        && runtime.connection_generation == generation => {
                        match outcome {
                            Ok(sites::Outcome::Listed(list)) => request_render(
                                app.update(AppEvent::SitesOutput { pane, text: sites::list_text(&list) }), &mut scheduler),
                            Ok(sites::Outcome::Published(site)) => request_render(
                                app.update(AppEvent::NotifySuccess { pane, message: sites::published_text(&site) }), &mut scheduler),
                            Ok(sites::Outcome::Opened(view)) => {
                                let client = runtime.client.clone();
                                let agent_id = runtime.agent_id.clone();
                                runtime.links.spawn(async move { (pane, links::open(&client, &agent_id, &view.url).await) });
                                request_render(app.update(AppEvent::NotifySuccess { pane, message: format!(
                                    "Opening {} v{} in your browser. The private preview works for an hour.", view.site_id, view.version) }), &mut scheduler);
                            }
                            Ok(sites::Outcome::Shared(share)) => {
                                let copied = clipboard::copy_text(&share.url).is_ok();
                                request_render(app.update(AppEvent::SitesOutput { pane, text: format!(
                                    "{}\n\nAnyone with this link can open {} v{} until you revoke it:\n/sites revoke {} {}",
                                    share.url, share.site_id, share.version, share.site_id, share.id) }), &mut scheduler);
                                request_render(app.update(AppEvent::NotifySuccess { pane, message: if copied {
                                    "Site link copied. Press c to copy again or Esc to close.".into()
                                } else {
                                    "Site link created. Clipboard unavailable; press c in the panel to retry.".into()
                                } }), &mut scheduler);
                            }
                            Ok(sites::Outcome::Revoked) => request_render(
                                app.update(AppEvent::NotifySuccess { pane, message: "Site link revoked. It stops working immediately.".into() }), &mut scheduler),
                            Err(error) => request_render(
                                app.update(AppEvent::NotifyError { pane, error: sites::error(&error) }), &mut scheduler),
                        }
                    }
                    Ok(_) => {},
                    Err(_) => request_render(app.update(AppEvent::NotifyError { pane: PaneId::Main,
                        error: "Sites request stopped unexpectedly. Run /sites before retrying a change.".into() }), &mut scheduler),
                }
            }
            Some(result) = runtime.links.join_next(), if !runtime.links.is_empty() => {
                let (pane, result) = result.unwrap_or_else(|error| (
                    PaneId::Main, Err(format!("Could not open link: {error}")),
                ));
                if let Err(error) = result {
                    request_render(app.update(AppEvent::NotifyError { pane, error }), &mut scheduler);
                }
            }
            result = runtime.settings_updates.join_next(), if !runtime.settings_updates.is_empty() => {
                if let Some(result) = result {
                    let (pane, agent_id, mutation, outcome) = result.map_err(|error| {
                        ManagedError::Configuration(format!("settings task failed: {error}"))
                    })?;
                    if agent_id == runtime.agent_id {
                        match outcome {
                            Ok(settings) => {
                                runtime.settings = settings;
                                if let Some(local) = &mut runtime.local
                                    && let Ok(model) = settings.model.as_str().parse()
                                {
                                    local.launch.args.select_tui_model(model, settings.thinking, settings.fast_mode);
                                }
                                if matches!(mutation, SettingsMutation::Complete(_)) && let Some(root) = app.root_mut(pane) {
                                    let mode = reasoning_mode_from_managed(settings.reasoning_mode);
                                    root.set_reasoning_modes(mode, mode);
                                }
                                if matches!(mutation, SettingsMutation::Complete(_))
                                    || matches!(mutation, SettingsMutation::Thinking(_)) && gateway_model(settings.model)
                                {
                                    runtime.routing_enabled = gateway_model(settings.model);
                                    runtime.routing_resolved = false;
                                    request_render(app.update(AppEvent::RoutingHydrated { pane, enabled: false,
                                        provider: None, model: None, effort: None }), &mut scheduler);
                                }
                                if matches!(mutation, SettingsMutation::AutoRoute) {
                                    runtime.routing_generation = runtime.routing_generation.wrapping_add(1);
                                    runtime.routing_updates = JoinSet::new();
                                    runtime.routing_enabled = true;
                                    runtime.routing_resolved = false;
                                    request_render(app.update(AppEvent::RoutingHydrated { pane, enabled: true,
                                        provider: None, model: None, effort: None }), &mut scheduler);
                                    request_render(app.update(AppEvent::NotifySuccess {
                                        pane,
                                        message: "Automatic routing enabled. Jev will choose from your first message, then lock this thread’s provider and model.".to_owned(),
                                    }), &mut scheduler);
                                }
                            },
                            Err(error) => {
                                // Switching from routing to native settings can require two
                                // requests. Re-read retained settings if only the first applied.
                                if runtime.local.is_none() && let Ok(state) = runtime.client.state(&agent_id).await {
                                    runtime.settings = state.settings;
                                    if let Some(root) = app.root_mut(pane) {
                                        let mode = reasoning_mode_from_managed(state.settings.reasoning_mode);
                                        root.set_reasoning_modes(mode, mode);
                                    }
                                }
                                runtime.refresh_routing();
                                request_render(app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!("Could not {}: {error}", mutation.failure_subject()),
                                }), &mut scheduler);
                            },
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
                    if runtime.local.is_none() && admission.as_ref().is_err_and(connection_failure) {
                        runtime.begin_recovery(&mut app, &mut scheduler, true);
                        continue;
                    }
                    runtime.unacknowledged_inputs.remove(&id);
                    runtime.admitting.remove(&id);
                    let cancelled_after_admission = runtime.cancel_after_admission.remove(&id);
                    let mut updates = Vec::new();
                    match admission {
                        Ok(turn) => {
                            let control = turn.control();
                            if let Some(managed_turn_id) = turn.request_id() {
                                // Local harnesses (Claude) assign their own turn id;
                                // only the managed service echoes the caller's id.
                                debug_assert!(
                                    runtime.local.is_some()
                                        || runtime.local_managed_turns.get(&id).map(String::as_str)
                                            == Some(managed_turn_id),
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
                            }
                        }
                        Err(error) => {
                            if let Some(request_id) = runtime.local_managed_turns.remove(&id)
                                && let Some(local) = &runtime.local
                            {
                                local.submissions.remove(&request_id);
                                runtime.submitted_turns.remove(&request_id);
                            }
                            runtime.local_terminal_turns.remove(&id);
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
                if runtime.routing_enabled && !runtime.routing_resolved {
                    runtime.routing_updates = JoinSet::new();
                    runtime.refresh_routing();
                }
                if let Some(result) = result {
                    let (pane, id, outcome) = result.map_err(|error| ManagedError::Configuration(format!("turn task failed: {error}")))?;
                    if runtime.local.is_none() && outcome.as_ref().is_err_and(connection_failure) {
                        runtime.begin_recovery(&mut app, &mut scheduler, true);
                        continue;
                    }
                    runtime.controls.remove(&id);
                    runtime.local_managed_turns.remove(&id);
                    runtime.local_terminal_turns.remove(&id);
                    runtime.cancellation_fences.local_terminal(id);
                    let error = outcome.err().map(|error| error.to_string());
                    let record = runtime.local_record(LocalEvent::WorkerTurnFinished { id, error })?;
                    request_render(app.update(AppEvent::Transcript { pane, record }), &mut scheduler);
                    let update = app.update(AppEvent::WorkerTurnFinished { pane, terminal_expected: true });
                    stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                }
            }
            result = runtime.receipt_reconciliations.join_next(), if !runtime.receipt_reconciliations.is_empty() => {
                if let Some(Ok((pane, id, agent_id, message_id, state))) = result {
                    if agent_id != runtime.agent_id
                        || !runtime.unresolved_steers.contains_key(&(pane, id))
                        || !runtime.steer_receipts.get(&(pane, id)).is_some_and(|(_, _, retained)| *retained == message_id) { continue; }
                    if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                    if state == nanocodex_managed::SteerReceiptState::Unknown { continue; }
                    if runtime.pending_steer_target.as_ref().is_some_and(|(pending, _)| *pending == id) {
                        runtime.pending_steer_target = None;
                        runtime.steers = JoinSet::new();
                    }
                    if runtime.unconfirmed_steer.as_ref().is_some_and(|(pending, _, _)| *pending == id) {
                        runtime.unconfirmed_steer = None;
                    }
                    let mut update = if state == nanocodex_managed::SteerReceiptState::Accepted {
                        app.update(AppEvent::SteerAdmitted { pane, id })
                    } else {
                        runtime.steer_receipts.remove(&(pane, id));
                        app.update(AppEvent::SteerWithdrawn { pane, id })
                    };
                    if runtime.pending_withdrawals.remove(&(pane, id)) && state == nanocodex_managed::SteerReceiptState::Accepted {
                        update.effects.push(AppEffect::Pane { pane, effect: RootEffect::WithdrawSteer { id } });
                    }
                    stopping |= apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                }
            }
            result = runtime.steers.join_next(), if !runtime.steers.is_empty() => {
                if let Some(result) = result {
                    let (pane, id, generation, target, outcome) = result.map_err(|error| ManagedError::Configuration(format!("steer task failed: {error}")))?;
                    runtime.pending_steer_target = None;
                    let withdraw = runtime.pending_withdrawals.remove(&(pane, id));
                    let mut update = match runtime.resolve_steer(generation, &target, outcome) {
                        SteerResolution::Admitted => {
                            if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                            runtime.steer_receipts.retain(|key @ (owner, candidate), _| *owner != pane || *candidate == id || runtime.unresolved_steers.contains_key(key));
                            app.update(AppEvent::SteerAdmitted { pane, id })
                        }
                        SteerResolution::Unconfirmed { error, active } => {
                            if active {
                                runtime.unconfirmed_steer = Some((id, generation, target));
                                request_render(app.update(AppEvent::NotifyError { pane, error: format!("Could not confirm steering: {error}. Delivery unknown; it will not be retried automatically.") }), &mut scheduler);
                            }
                            // Shared telemetry cannot correlate this request. Preserve it
                            // for explicit review even after the owning turn finishes.
                            app.update(AppEvent::SteerUnconfirmed { pane, id })
                        }
                        SteerResolution::Rejected(error) => {
                            if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                            runtime.steer_receipts.remove(&(pane, id));
                            request_render(app.update(AppEvent::NotifyError { pane, error: format!("Steering rejected: {error}") }), &mut scheduler);
                            app.update(if withdraw { AppEvent::SteerWithdrawn { pane, id } } else { AppEvent::SteerFailed { pane, id } })
                        }
                        SteerResolution::Failed if withdraw => {
                            if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                            runtime.steer_receipts.remove(&(pane, id));
                            app.update(AppEvent::SteerWithdrawn { pane, id })
                        }
                        SteerResolution::Failed => {
                            if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                            runtime.steer_receipts.remove(&(pane, id));
                            app.update(AppEvent::SteerFailed { pane, id })
                        }
                        SteerResolution::Stale => continue,
                    };
                    if withdraw && runtime.steer_receipts.contains_key(&(pane, id)) {
                        update.effects.push(AppEffect::Pane { pane, effect: RootEffect::WithdrawSteer { id } });
                    }
                    stopping = apply_update(update, &mut app, &mut runtime, &mut terminal, &mut scheduler).await?;
                }
            }
            result = runtime.withdrawals.join_next(), if !runtime.withdrawals.is_empty() => {
                if let Some(result) = result {
                    let (pane, id, generation, outcome) = result.map_err(|error| ManagedError::Configuration(format!("withdrawal task failed: {error}")))?;
                    if generation != runtime.connection_generation { continue; }
                    let update = match outcome {
                        Ok(true) => {
                            runtime.steer_receipts.remove(&(pane, id));
                            if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                            if runtime.unconfirmed_steer.as_ref().is_some_and(|(pending, _, _)| *pending == id) {
                                runtime.unconfirmed_steer = None;
                            }
                            app.update(AppEvent::SteerWithdrawn { pane, id })
                        }
                        Ok(false) => app.update(AppEvent::SteerWithdrawalFailed { pane, id, error: "Message already received by the model or no longer withdrawable.".to_owned() }),
                        Err(error) => app.update(AppEvent::SteerWithdrawalFailed { pane, id, error: format!("Withdrawal unconfirmed: {error}. The message has not been restored.") }),
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
            result = runtime.history_replays.join_next(), if !runtime.history_replays.is_empty() => {
                if let Some(result) = result {
                    match result {
                        Err(error) => {
                            runtime.history_tree_failed = runtime.history_tree_open;
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
                    runtime.start_requested_history_replay(PaneId::Main);
                }
            }
            result = runtime.history_loads.join_next(), if !runtime.history_loads.is_empty() => {
                if let Some(result) = result {
                    match result {
                        Err(error) => {
                            runtime.history_tree_failed = runtime.history_tree_open;
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
                                runtime.history_tree_failed = runtime.history_tree_open;
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
                                    runtime.history_tree_failed = runtime.history_tree_open;
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

    runtime.cancel_secure_input(); // wipe before terminal restoration/shutdown
    drop(terminal);
    // Finish already-scheduled local writes before /reload replaces this process.
    while let Some(result) = runtime.prompt_cache_writes.join_next().await {
        if let Ok((batch, Err(_))) = result {
            runtime.prompt_cache_pending.extend(batch);
        }
    }
    if !runtime.prompt_cache_pending.is_empty()
        && let Ok(cache) = &runtime.prompt_cache
    {
        let pending = prompt_cache::bounded(std::mem::take(&mut runtime.prompt_cache_pending));
        if cache.merge(pending).await.is_err() {
            tracing::warn!("could not finish saving recent prompts");
        }
    }
    if let Some(voice) = runtime.voice.take() {
        voice.finish().await;
    }
    if reload_requested {
        if let Some(agent) = runtime.agent.take() {
            agent.disconnect().await.map_err(super::agent_error)?;
        }
        return reload
            .expect("reload request requires a registration")
            .restart(&runtime.agent_id)
            .map_err(ManagedError::Configuration);
    }
    if let Some(mut local) = runtime.local.take() {
        // A local agent is not durable: stop it and release VM/browser/MPP.
        drop(runtime.agent.take());
        return local
            .shutdown()
            .await
            .map_err(|error| ManagedError::Configuration(format!("{error:#}")));
    }
    let Some(agent) = runtime.agent.take() else {
        return Ok(());
    };
    if runtime.idle() {
        agent.shutdown().await.map_err(super::agent_error)
    } else {
        agent.disconnect().await.map_err(super::agent_error)
    }
}

const fn gateway_model(model: ManagedModel) -> bool {
    matches!(
        model,
        ManagedModel::Oai(Model::Glm53 | Model::Kimi | Model::Mimo)
    )
}

fn fresh_thread_settings(was_routed: bool, settings: AgentSettings) -> AgentSettings {
    // Routed provider choices belong to the old conversation. Gateway settings
    // must never be passed to the fixed-settings agent creation endpoint.
    if was_routed || gateway_model(settings.model) {
        new_agent_settings()
    } else {
        settings
    }
}

fn new_agent_settings() -> AgentSettings {
    AgentSettings::default()
}

/// Applies one request from a feature module (see `features`).
async fn apply_feature_update(
    update: features::FeatureUpdate,
    app: &mut AppNode,
    runtime: &mut DriverRuntime,
    terminal: &mut TerminalSession,
    scheduler: &mut RenderScheduler,
) -> Result<bool, ManagedError> {
    use features::FeatureUpdate;
    let main = |pane: Option<PaneId>| pane.unwrap_or(PaneId::Main);
    let update = match update {
        FeatureUpdate::Notice { pane, message } => app.update(AppEvent::NotifySuccess {
            pane: main(pane),
            message,
        }),
        FeatureUpdate::Error { pane, message } => app.update(AppEvent::NotifyError {
            pane: main(pane),
            error: message,
        }),
        FeatureUpdate::Submit { pane, text } => {
            let pane = main(pane);
            let prompt = Submission::text(text);
            let id = TurnId::new(runtime.next_turn);
            runtime.next_turn = runtime.next_turn.saturating_add(1);
            let record = runtime.record_submission(id, &prompt)?;
            let update = app.update(AppEvent::Transcript { pane, record });
            if runtime.active_shells == 0 {
                runtime.start_submission(pane, id, prompt);
            } else {
                runtime.pending_submission = Some((pane, id, prompt));
            }
            update
        }
        FeatureUpdate::Record { pane, event } => {
            let record = runtime.local_record(event)?;
            app.update(AppEvent::Transcript {
                pane: main(pane),
                record,
            })
        }
        FeatureUpdate::OpenOverlay(overlay) => {
            if overlay.discards_draft()
                && let Some(root) = app.root_mut(PaneId::Main)
            {
                root.discard_feature_draft();
            }
            runtime.feature_overlay = Some(overlay);
            if let Some(local) = &mut runtime.local {
                local.features.overlay_state(true);
            }
            scheduler.request_immediate(Instant::now());
            return Ok(false);
        }
        FeatureUpdate::CloseOverlay => {
            if runtime.feature_overlay.take().is_some()
                && let Some(local) = &mut runtime.local
            {
                local.features.overlay_state(false);
            }
            scheduler.request_immediate(Instant::now());
            return Ok(false);
        }
        FeatureUpdate::SubmitPrompt(prompt) => {
            let pane = main(prompt.pane);
            let submission = Submission::text(prompt.display);
            let id = TurnId::new(runtime.next_turn);
            runtime.next_turn = runtime.next_turn.saturating_add(1);
            runtime.feature_turns.insert(id);
            if let Some(instruction) = prompt.instruction {
                runtime.feature_instructions.insert(id, instruction);
            }
            if let Some(completion) = prompt.completion {
                runtime
                    .feature_completions
                    .entry(id)
                    .or_default()
                    .push(completion);
            }
            let record = runtime.record_submission(id, &submission)?;
            let update = app.update(AppEvent::Transcript { pane, record });
            if runtime.active_shells == 0 {
                runtime.start_submission(pane, id, submission);
            } else {
                runtime.pending_submission = Some((pane, id, submission));
            }
            update
        }
        // Branch switch/edit reopens another session in place.
        FeatureUpdate::Relaunch(launch) if launch.resume.is_some() => {
            match runtime.local_switch(ConnectionPurpose::Resume(PaneId::Main), *launch) {
                Ok(task) => runtime.pending_resume = Some((task, PaneId::Main)),
                Err(error) => {
                    request_render(
                        app.update(AppEvent::NotifyError {
                            pane: PaneId::Main,
                            error,
                        }),
                        scheduler,
                    );
                }
            }
            scheduler.request_immediate(Instant::now());
            return Ok(false);
        }
        FeatureUpdate::Relaunch(launch) => {
            let Some(local) = &mut runtime.local else {
                return Ok(false);
            };
            let _ = local;
            // Prompts typed now belong to the selected harness: hold them until
            // its agent is adopted. Relaunches run one at a time behind any
            // connection in flight (they share the local build slot), and the
            // newest selection replaces an older queued one. The current
            // launch stays until the rebuilt agent connects (adopt), so a
            // failed relaunch keeps the running agent.
            runtime.harness_relaunching = true;
            runtime.queued_relaunch = Some(launch);
            if let Some(error) = runtime.start_queued_relaunch() {
                request_render(
                    app.update(AppEvent::NotifyError {
                        pane: PaneId::Main,
                        error,
                    }),
                    scheduler,
                );
            }
            scheduler.request_immediate(Instant::now());
            return Ok(false);
        }
        FeatureUpdate::Subagent(update) => app.update(AppEvent::Subagent {
            pane: PaneId::Main,
            update,
        }),
        FeatureUpdate::VoiceStatus(status) => {
            runtime.local_voice_status = status;
            app.update(AppEvent::VoiceStatus(runtime.voice_status()))
        }
        // /collapse and /split close the local side pane.
        FeatureUpdate::ClosePane(pane) => app.close_fork(pane),
        // The root queues it and emits its usual RootEffect::Steer.
        FeatureUpdate::Steer {
            pane,
            display,
            instruction,
        } => app.update(AppEvent::FeatureSteer {
            pane: main(pane),
            prompt: match instruction {
                Some(instruction) => Submission::labelled(display, instruction),
                None => Submission::text(display),
            },
        }),
        FeatureUpdate::Capabilities(capabilities) => {
            if let Some(root) = app.root_mut(PaneId::Main) {
                root.set_capabilities(capabilities);
            }
            scheduler.request_immediate(Instant::now());
            return Ok(false);
        }
    };
    apply_update(update, app, runtime, terminal, scheduler).await
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
            AppEffect::Screen(command) => runtime.screen.command(&runtime.client, command),
            AppEffect::Shutdown => {
                runtime.cancel_secure_input();
                stopping = true;
            }
            AppEffect::SetTheme(_) => scheduler.request_immediate(Instant::now()),
            AppEffect::OpenFork { pane, .. } => {
                if runtime.agent_id.is_empty() {
                    absorb(
                        app.update(AppEvent::ForkFailed {
                            pane,
                            error: "Wait for the main agent to connect before opening /btw".into(),
                        }),
                        &mut effects,
                        scheduler,
                    );
                    continue;
                }
                if runtime.local.is_some() {
                    // Local /btw forks the in-process agent.
                    let Some(main) = runtime.agent.clone() else {
                        absorb(
                            app.update(AppEvent::ForkFailed {
                                pane,
                                error: "Wait for the local agent before opening /btw".into(),
                            }),
                            &mut effects,
                            scheduler,
                        );
                        continue;
                    };
                    let (commands, requests) = mpsc::unbounded_channel();
                    let task = tokio::spawn(features::btw_local::run(
                        pane,
                        main,
                        runtime.settings,
                        runtime.sequence.saturating_add(1),
                        requests,
                        runtime.btw_events.clone(),
                    ));
                    runtime.btw = Some(BtwConnection {
                        pane,
                        agent_id: None,
                        commands,
                        task,
                    });
                    continue;
                }
                let (commands, requests) = mpsc::unbounded_channel();
                let task = tokio::spawn(btw::run(
                    pane,
                    runtime.client.clone(),
                    runtime.agent_id.clone(),
                    fresh_thread_settings(
                        runtime.routing_enabled || gateway_model(runtime.settings.model),
                        runtime.settings,
                    ),
                    runtime.workspace.clone(),
                    runtime.sequence.saturating_add(1),
                    requests,
                    runtime.btw_events.clone(),
                ));
                runtime.btw = Some(BtwConnection {
                    pane,
                    agent_id: None,
                    commands,
                    task,
                });
            }
            AppEffect::ClosePane(pane) => {
                if runtime.btw.as_ref().is_some_and(|btw| btw.pane == pane)
                    && let Some(btw) = runtime.btw.take()
                {
                    if runtime.local.is_some() {
                        // Dropping the request channel makes the local
                        // side task cancel its turn and shut its forked agent down.
                        drop(btw.commands);
                    } else {
                        btw.task.abort();
                    }
                }
            }
            AppEffect::Pane { pane, effect } => {
                if runtime.local.is_some()
                    && matches!(
                        &effect,
                        RootEffect::AutoRoute
                            | RootEffect::Connectors(_)
                            | RootEffect::Reload
                            | RootEffect::SetDone(_)
                            | RootEffect::Bug(_)
                            | RootEffect::Vault(_)
                            | RootEffect::SecureInput(_)
                            | RootEffect::Share(_)
                            | RootEffect::Sites(_)
                            | RootEffect::ApproveVault(_)
                            | RootEffect::Handoff
                    )
                {
                    absorb(
                        app.update(AppEvent::NotifyError {
                            pane,
                            error: backend::Capabilities::LOCAL.unavailable("This command"),
                        }),
                        &mut effects,
                        scheduler,
                    );
                    continue;
                }

                if let RootEffect::CopyResponse(text) = effect {
                    let event = match clipboard::copy_text(&text) {
                        Ok(()) => AppEvent::NotifySuccess {
                            pane,
                            message: "Copied response".to_owned(),
                        },
                        Err(error) => AppEvent::NotifyError {
                            pane,
                            error: format!("Clipboard copy failed: {error}"),
                        },
                    };
                    absorb(app.update(event), &mut effects, scheduler);
                    continue;
                }
                if let RootEffect::LoadRecentPrompts(drafts) = effect {
                    runtime.load_prompt_cache(pane, drafts);
                    continue;
                }
                if let RootEffect::LoadReviewBranches {
                    request_id,
                    workspace,
                } = effect
                {
                    runtime.review_branch_loads.spawn(async move {
                        ReviewBranchesCompletion {
                            pane,
                            request_id,
                            result: review::branches(&workspace).await,
                        }
                    });
                    continue;
                }
                if pane != PaneId::Main {
                    match effect {
                        RootEffect::Feature(command) => {
                            // /collapse and /split typed in the side pane.
                            let handled = runtime.local.as_mut().is_some_and(|local| {
                                local.with_features(|features, cx| {
                                    features.command(pane, &command, cx)
                                })
                            });
                            if !handled {
                                absorb(
                                    app.update(AppEvent::NotifyError {
                                        pane,
                                        error: "This command needs a local agent (run ncl)".into(),
                                    }),
                                    &mut effects,
                                    scheduler,
                                );
                            }
                        }
                        RootEffect::Submit(prompt) | RootEffect::ContinueSubagent(prompt) => {
                            if runtime
                                .btw
                                .as_ref()
                                .filter(|btw| btw.pane == pane)
                                .is_none_or(|btw| {
                                    btw.commands.send(btw::Request::Submit(prompt)).is_err()
                                })
                            {
                                absorb(
                                    app.update(AppEvent::NotifyError {
                                        pane,
                                        error: "Side agent is no longer connected".into(),
                                    }),
                                    &mut effects,
                                    scheduler,
                                );
                                absorb(
                                    app.update(AppEvent::WorkerTurnFinished {
                                        pane,
                                        terminal_expected: false,
                                    }),
                                    &mut effects,
                                    scheduler,
                                );
                            }
                        }
                        RootEffect::CancelTurns => {
                            if let Some(btw) = runtime.btw.as_ref().filter(|btw| btw.pane == pane) {
                                let _ = btw.commands.send(btw::Request::Cancel);
                            }
                        }
                        RootEffect::ShowAgentId => {
                            let id = runtime
                                .btw
                                .as_ref()
                                .filter(|btw| btw.pane == pane)
                                .and_then(|btw| btw.agent_id.clone())
                                .unwrap_or_else(|| "connecting".into());
                            absorb(
                                app.update(AppEvent::ShowAgentId { pane, id }),
                                &mut effects,
                                scheduler,
                            );
                        }
                        RootEffect::Copy(text) => {
                            if let Err(error) = clipboard::copy_text(&text) {
                                absorb(
                                    app.update(AppEvent::NotifyError {
                                        pane,
                                        error: format!("Clipboard copy failed: {error}"),
                                    }),
                                    &mut effects,
                                    scheduler,
                                );
                            }
                        }
                        RootEffect::Steer { id, .. } => {
                            absorb(
                                app.update(AppEvent::SteerFailed { pane, id }),
                                &mut effects,
                                scheduler,
                            );
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Queue a follow-up with Tab in /btw; steering is not available".into() }), &mut effects, scheduler);
                        }
                        _ => absorb(
                            app.update(AppEvent::NotifyError {
                                pane,
                                error: "This command is unavailable in /btw".into(),
                            }),
                            &mut effects,
                            scheduler,
                        ),
                    }
                    continue;
                }
                // Keep the hosted effect boundary visually separate from app-level routing.
                match effect {
                    RootEffect::Reload => {
                        let update = match crate::nanocodex2::reload::request_all() {
                            Ok(count) => app.update(AppEvent::NotifySuccess { pane, message: format!("Reload requested for {count} local terminal(s)…") }),
                            Err(error) => app.update(AppEvent::NotifyError { pane, error }),
                        };
                        absorb(update, &mut effects, scheduler);
                    }
                    RootEffect::Screen | RootEffect::Zoom | RootEffect::Btw(_) | RootEffect::CloseBtw => {
                        unreachable!("workspace commands are handled by AppNode")
                    }
                    RootEffect::Feature(command) => {
                        // Feature command dispatch.
                        let handled = runtime.local.as_mut().is_some_and(|local| {
                            local.with_features(|features, cx| features.command(pane, &command, cx))
                        });
                        if !handled {
                            let error = match &command {
                                features::FeatureCommand::RealtimeVoice(arguments) => crate::nanocodex2::voice::Command::parse(arguments)
                                    .err()
                                    .unwrap_or_else(|| "This /voice command needs a local agent (run ncl)".into()),
                                _ => "This command needs a local agent (run ncl)".into(),
                            };
                            absorb(app.update(AppEvent::NotifyError { pane, error }), &mut effects, scheduler);
                        }
                    }
                    RootEffect::Voice(command) if runtime.local.is_some() => {
                        // Local Realtime voice.
                        use crate::nanocodex2::voice::{Command, Provider};
                        let argument = match command {
                            Command::Toggle => Some(String::new()),
                            Command::Start(None) => Some("on".to_owned()),
                            Command::Start(Some(name)) => Some(name.to_owned()),
                            Command::Stop => Some("off".to_owned()),
                            Command::ToggleMute => Some("mute".to_owned()),
                            Command::List | Command::ListProvider(Provider::Chatgpt) => Some("list".to_owned()),
                            _ => None,
                        };
                        let handled = argument.is_some_and(|argument| {
                            runtime.local.as_mut().is_some_and(|local| local.with_features(|features, cx| {
                                features.command(pane, &features::FeatureCommand::RealtimeVoice(argument), cx)
                            }))
                        });
                        if !handled {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "This /voice command needs a Nanocodex account session (run nanocodex)".into() }), &mut effects, scheduler);
                        }
                    }
                    RootEffect::Voice(command) => {
                        if matches!(runtime.retry_target, Some(RetryTarget::Default)) {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Wait for managed model availability before using voice input".into() }), &mut effects, scheduler);
                            continue;
                        }
                        if runtime.settings.model.oai().is_none() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Claude currently supports text only; voice input is unavailable".into() }), &mut effects, scheduler);
                            continue;
                        }
                        let persistent = matches!(command, crate::nanocodex2::voice::Command::Help | crate::nanocodex2::voice::Command::ListProvider(crate::nanocodex2::voice::Provider::Chatgpt));
                        let outcome = runtime.voice_command(pane, command);
                        absorb(
                            app.update(AppEvent::VoiceStatus(runtime.voice_status())),
                            &mut effects,
                            scheduler,
                        );
                        match outcome {
                            Ok(Some(message)) => absorb(
                                app.update(if persistent { AppEvent::VoiceOutput { pane, text: message } } else { AppEvent::NotifySuccess { pane, message } }),
                                &mut effects,
                                scheduler,
                            ),
                            Err(error) => absorb(
                                app.update(AppEvent::NotifyError { pane, error }),
                                &mut effects,
                                scheduler,
                            ),
                            Ok(None) => {}
                        }
                    }
                    RootEffect::Submit(prompt) | RootEffect::ContinueSubagent(prompt) => {
                        if let Some(voice) = &runtime.voice {
                            voice.typed();
                        }
                        let id = TurnId::new(runtime.next_turn);
                        runtime.next_turn = runtime.next_turn.saturating_add(1);
                        let record = runtime.record_submission(id, &prompt)?;
                        absorb(
                            app.update(AppEvent::Transcript { pane, record }),
                            &mut effects,
                            scheduler,
                        );
                        if runtime.active_shells == 0 {
                            runtime.start_submission(pane, id, prompt);
                        } else {
                            runtime.pending_submission = Some((pane, id, prompt));
                        }
                    }
                    RootEffect::SetDone(done) => {
                        if runtime.agent_id.is_empty() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "No managed session yet. Attach a session before marking it done.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        if !runtime.done_updates.is_empty() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "A session change is already being saved.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        let client = runtime.client.clone();
                        let agent_id = runtime.agent_id.clone();
                        let generation = runtime.connection_generation;
                        runtime.done_updates.spawn(async move {
                            let result = client.set_done(&agent_id, done).await;
                            (agent_id, generation, done, result)
                        });
                    }
                    RootEffect::Share(command) => {
                        if command == share::Command::Help {
                            absorb(app.update(AppEvent::ShareOutput { pane, text: share::help() }), &mut effects, scheduler);
                            continue;
                        }
                        if runtime.agent_id.is_empty() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "No managed thread yet. Send a prompt or attach a thread before sharing.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        if runtime.agent.is_none() || runtime.recovery.is_some() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Managed thread is offline. Reconnect before managing share links.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        let client = runtime.client.clone();
                        let agent_id = runtime.agent_id.clone();
                        let generation = runtime.connection_generation;
                        runtime.share_tasks.spawn(async move {
                            let result = match command {
                                share::Command::Create(permission) => client.create_share_link(&agent_id, permission).await.map(share::Outcome::Created),
                                share::Command::List => client.list_share_links(&agent_id).await.map(share::Outcome::Listed),
                                share::Command::Revoke(id) => client.revoke_share_link(&agent_id, &id).await.map(|()| share::Outcome::Revoked),
                                share::Command::Help => unreachable!(),
                            };
                            (pane, agent_id, generation, result)
                        });
                    }
                    RootEffect::Sites(command) => {
                        if command == sites::Command::Help {
                            absorb(app.update(AppEvent::SitesOutput { pane, text: sites::help() }), &mut effects, scheduler);
                            continue;
                        }
                        if runtime.agent_id.is_empty() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "No managed thread yet. Send a prompt or attach a thread before using /sites.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        if runtime.agent.is_none() || runtime.recovery.is_some() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Managed thread is offline. Reconnect before managing sites.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        let client = runtime.client.clone();
                        let agent_id = runtime.agent_id.clone();
                        let generation = runtime.connection_generation;
                        runtime.site_tasks.spawn(async move {
                            let result = match command {
                                sites::Command::List => client.list_sites(&agent_id).await.map(sites::Outcome::Listed),
                                sites::Command::Publish { path, id } => client.publish_site(&agent_id, &sites::publish_request(path, id)).await.map(sites::Outcome::Published),
                                sites::Command::Open { site, version } => client.open_site(&agent_id, &site, version).await.map(sites::Outcome::Opened),
                                sites::Command::Share { site, version } => client.create_site_share(&agent_id, &site, version).await.map(sites::Outcome::Shared),
                                sites::Command::Revoke { site, link } => client.revoke_site_share(&agent_id, &site, &link).await.map(|()| sites::Outcome::Revoked),
                                sites::Command::Help => unreachable!(),
                            };
                            (pane, agent_id, generation, result)
                        });
                    }
                    RootEffect::SecureInput(request) => {
                        runtime.cancel_secure_input();
                        let request=request.map(|mut r|{if let secure_input::Request::Private(p)=&mut r && matches!(p.kind,nanocodex_managed::PrivateInputKind::Vault(_)){p.agent_id=runtime.agent_id.clone();}r});
                        let Some(request) = request.filter(|request| request.agent() == runtime.agent_id && request.is_current() && !runtime.secure_input_attempted.contains(request.id())) else {
                            absorb(app.update(AppEvent::NotifyError { pane, error: secure_input::HELP.into() }), &mut effects, scheduler);
                            continue;
                        };
                        if pane != PaneId::Main || runtime.agent.is_none() || runtime.recovery.is_some() || !runtime.secure_input_tasks.is_empty() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Private secure input is unavailable while disconnected or another approval is pending. No password requested.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        if secure_input::protect_process().is_err() {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Private input protection unavailable. No password requested or submitted.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        let client = runtime.client.clone();
                        let agent = runtime.agent_id.clone();
                        let generation = runtime.connection_generation;
                        runtime.secure_input = Some(secure_input::Flow::loading(request.clone(), generation, pane));
                        runtime.secure_input_tasks.spawn(async move {
                            let outcome=match &request {
                                secure_input::Request::Sudo(r)=>secure_input::Outcome::Sudo(client.describe_native_secure_input(r).await.map(sudo_input::Outcome::Description).unwrap_or(sudo_input::Outcome::Status(secure_input::Status::Unavailable))),
                                secure_input::Request::Private(r)=>secure_input::Outcome::Private(private_input::describe(&client,r).await),
                            };
                            (agent, generation, request.id().to_owned(), outcome)
                        });
                        scheduler.request_immediate(Instant::now());
                    }
                    RootEffect::Connectors(text) => {
                        let args: Vec<_> = text.split_whitespace().collect();
                        let private = match args.as_slice() {
                            [_, "whatsapp-pair", id] if uuid::Uuid::parse_str(id).is_ok() => Some(("whatsapp", (*id).to_owned())),
                            [_, "chatgpt-start"] => Some(("chatgpt", "start".into())),
                            [_, "chatgpt-status"] => Some(("chatgpt", "status".into())),
                            _ => None,
                        };
                        if let Some((kind, name)) = private {
                            let request = nanocodex_managed::PrivateInputRequest { request_id: uuid::Uuid::new_v4().to_string(), agent_id: runtime.agent_id.clone(), origin: String::new(), expires_at: Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64 + 600_000), kind: nanocodex_managed::PrivateInputKind::Connector(kind.into()), allowed_origins: Vec::new(), name };
                            effects.push_back(AppEffect::Pane { pane, effect: RootEffect::SecureInput(Some(secure_input::Request::Private(request))) });
                            continue;
                        }
                        let client = runtime.client.clone();
                        let agent_id = runtime.agent_id.clone();
                        let generation = runtime.connection_generation;
                        runtime.vault_tasks.spawn(async move {
                            let result = match crate::nanocodex2::connectors::parse_local(&text) {
                                Ok(command) => command.execute(&client).await.map(vault::Outcome::Saved).map_err(|e| e.to_string()),
                                Err(error) => Err(error.to_string()),
                            };
                            (pane, agent_id, generation, result)
                        });
                    }
                    RootEffect::Vault(command) => {
                        match command {
                            vault::Command::Open => {
                                let client = runtime.client.clone();
                                let agent_id = runtime.agent_id.clone();
                                let generation = runtime.connection_generation;
                                runtime.vault_tasks.spawn(async move {
                                    let result = client.vault_list().await.map(|v| vault::Outcome::Saved(format!("Vault items: {}\nUse /vault add login|api_key|card|address|phone to open private input.", v))).map_err(|_| "Vault metadata unavailable".to_owned());
                                    (pane, agent_id, generation, result)
                                });
                            }
                            command @ (vault::Command::Add { .. } | vault::Command::SshAdd { .. }) => {
                                let (kind, name) = match command { vault::Command::Add { kind } if kind == "openai" => (nanocodex_managed::PrivateInputKind::Credential(kind), String::new()), vault::Command::Add { kind } => (nanocodex_managed::PrivateInputKind::Vault(kind), String::new()), vault::Command::SshAdd { reference } => (nanocodex_managed::PrivateInputKind::Credential("ssh".into()), reference), _ => unreachable!() };
                                let request = nanocodex_managed::PrivateInputRequest {
                                    request_id: uuid::Uuid::new_v4().to_string(), agent_id: runtime.agent_id.clone(),
                                    origin: String::new(), expires_at: None,
                                    kind, allowed_origins: Vec::new(), name,
                                };
                                effects.push_back(AppEffect::Pane { pane, effect: RootEffect::SecureInput(Some(secure_input::Request::Private(request))) });
                            }
                            command @ (vault::Command::Delete { .. } | vault::Command::SshRemove { .. } | vault::Command::Card { .. } | vault::Command::Store { .. }) => {
                                if !runtime.vault_tasks.is_empty() { continue; }
                                let client = runtime.client.clone();
                                let agent_id = runtime.agent_id.clone();
                                let generation = runtime.connection_generation;
                                runtime.vault_tasks.spawn(async move {
                                    let result = match command {
                                        vault::Command::Delete { kind, id } => client.vault_delete(&kind, &id).await,
                                        vault::Command::SshRemove { reference } => client.vault_ssh_remove(&reference).await,
                                        vault::Command::Card { operation, id, capture, operation_id } => client.vault_provider_card(&operation, &id, capture, operation_id.as_deref()).await,
                                        vault::Command::Store { capture_id, operation_id } => client.vault_provider_store(&capture_id, &operation_id, None, None).await,
                                        _ => unreachable!(),
                                    }.map(|receipt| vault::Outcome::Saved(receipt.to_string())).map_err(|_| "Vault request failed. Check status before retrying; retain the same operation ID.".to_owned());
                                    (pane, agent_id, generation, result)
                                });
                            }
                            vault::Command::Latest | vault::Command::Help => absorb(app.update(AppEvent::NotifyError { pane, error: "Use /vault list; /vault add KIND; /vault delete KIND ID; /vault card status|balance ID; /vault card refresh ID OPERATION_UUID; /vault store CAPTURE_ID OPERATION_UUID. SSH setup: nanocodex2 vault ssh-save --help. Never enter secret values in chat.".into() }), &mut effects, scheduler),
                            vault::Command::Review { id, origin } => {
                                if !runtime.vault_tasks.is_empty() { continue; }
                                let client = runtime.client.clone();
                                let agent_id = runtime.agent_id.clone();
                                let generation = runtime.connection_generation;
                                runtime.vault_tasks.spawn(async move {
                                    let result = client.vault_login(&id).await
                                        .map(|login| vault::Outcome::Review(vault::Review { login, origin, agent_id: agent_id.clone(), generation, visible: false }))
                                        .map_err(|_| "Couldn’t verify this saved login. Use /vault open to check the item and your account.".to_owned());
                                    (pane, agent_id, generation, result)
                                });
                                absorb(app.update(AppEvent::NotifySuccess { pane, message: "Verifying saved login in your Vault…".into() }), &mut effects, scheduler);
                            }
                        }
                    }
                    RootEffect::ApproveVault(review) => {
                        if !vault::scope_matches(&review.agent_id, review.generation, &runtime.agent_id, runtime.connection_generation) || !runtime.vault_tasks.is_empty() { continue; }
                        if !runtime.vault_attempted.insert((review.login.id.clone(), review.origin.clone())) {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "This approval was already attempted. Check /vault open before trying again.".into() }), &mut effects, scheduler);
                            continue;
                        }
                        let client = runtime.client.clone();
                        runtime.vault_tasks.spawn(async move {
                            let result = client.approve_vault_login_origin(&review.login.id, &review.origin).await
                                .map(|login| vault::Outcome::Saved(vault::receipt(&login)))
                                .map_err(|_| "The website approval could not be confirmed. Check /vault open; this request will not be retried automatically.".to_owned());
                            (pane, review.agent_id, review.generation, result)
                        });
                        absorb(app.update(AppEvent::NotifySuccess { pane, message: "Saving website approval to Vault…".into() }), &mut effects, scheduler);
                    }
                    RootEffect::ShowAgentId => {
                        if runtime.agent_id.is_empty() {
                            absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: "No agent ID yet. Send a prompt to start a session."
                                        .to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                        } else {
                            absorb(
                                app.update(AppEvent::ShowAgentId {
                                    pane,
                                    id: runtime.agent_id.clone(),
                                }),
                                &mut effects,
                                scheduler,
                            );
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
                        let cancellation = runtime.shell_cancellation.clone();
                        runtime.shells.spawn(async move {
                            (
                                pane,
                                shell::execute(id, command, workspace, cancellation).await,
                            )
                        });
                    }
                    RootEffect::Steer { id, prompt } => {
                        if let Some(voice) = &runtime.voice {
                            voice.typed();
                        }
                        if !runtime.steers.is_empty() || runtime.unconfirmed_steer.is_some() {
                            runtime.waiting_steers.push_back((pane, id, prompt));
                            continue;
                        }
                        if let Some((turn_id, control)) = runtime
                            .controls
                            .iter()
                            .next()
                            .map(|(turn_id, control)| (*turn_id, control.clone()))
                        {
                            let generation = runtime.connection_generation;
                            let target = SteerTarget::Local(turn_id);
                            let message_id = uuid::Uuid::now_v7().to_string();
                            let local_steer = runtime.local.is_some();
                            if !local_steer {
                                runtime.steer_receipts.insert(
                                    (pane, id),
                                    (generation, target.clone(), message_id.clone()),
                                );
                                runtime.reconcile_steer_receipt(pane, id, &target, message_id.clone(), prompt.managed_prompt());
                            }
                            runtime.pending_steer_target = Some((id, target.clone()));
                            runtime.steers.spawn(async move {
                                let result = if local_steer {
                                    control.steer(prompt.agent_prompt()).await
                                } else {
                                    control.steer_with_id(message_id, prompt.agent_prompt()).await
                                }.map_err(SteerFailure::backend);
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
                                    let message_id = uuid::Uuid::now_v7().to_string();
                                    runtime.steer_receipts.insert(
                                        (pane, id),
                                        (generation, target.clone(), message_id.clone()),
                                    );
                                    runtime.reconcile_steer_receipt(pane, id, &target, message_id.clone(), input.clone());
                                    runtime.pending_steer_target = Some((id, target.clone()));
                                    runtime.steers.spawn(async move {
                                        let result = client
                                            .steer_with_id(&agent_id, &turn_id, &message_id, &input)
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
                            // The owning turn ended before this input could be delivered.
                            // Let the root recover the whole steer lane in its original order.
                            absorb(
                                app.update(AppEvent::SteerFailed { pane, id }),
                                &mut effects,
                                scheduler,
                            );
                        }
                    }
                    RootEffect::ForgetSteerReceipt { id } => {
                        if let Some(cancellation) = runtime.unresolved_steers.remove(&(pane, id)) { cancellation.cancel(); }
                        runtime.steer_receipts.remove(&(pane, id));
                        if runtime.unconfirmed_steer.as_ref().is_some_and(|(pending, _, _)| *pending == id) {
                            runtime.unconfirmed_steer = None;
                        }
                    }
                    RootEffect::WithdrawSteer { id } => {
                        if withdraw_waiting_steer(&mut runtime.waiting_steers, pane, id) {
                            absorb(
                                app.update(AppEvent::SteerWithdrawn { pane, id }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        }
                        if runtime
                            .pending_steer_target
                            .as_ref()
                            .is_some_and(|(pending, _)| *pending == id)
                        {
                            // Wait for admission to settle: withdrawing before the POST could
                            // otherwise race a successful late admission of the same message.
                            runtime.pending_withdrawals.insert((pane, id));
                            continue;
                        }
                        let Some((generation, target, message_id)) =
                            runtime.steer_receipts.get(&(pane, id)).cloned()
                        else {
                            absorb(
                                app.update(AppEvent::SteerWithdrawalFailed {
                                    pane,
                                    id,
                                    error: "Cannot confirm withdrawal of this message.".to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        };
                        if generation != runtime.connection_generation {
                            absorb(
                                app.update(AppEvent::SteerWithdrawalFailed {
                                    pane,
                                    id,
                                    error: "Connection changed; withdrawal was not confirmed."
                                        .to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        }
                        match target {
                            SteerTarget::Local(turn_id) => {
                                if let Some(control) = runtime.controls.get(&turn_id).cloned() {
                                    runtime.withdrawals.spawn(async move {
                                        (
                                            pane,
                                            id,
                                            generation,
                                            control
                                                .withdraw_steer(message_id)
                                                .await
                                                .map_err(|error| error.to_string()),
                                        )
                                    });
                                } else {
                                    absorb(app.update(AppEvent::SteerWithdrawalFailed { pane, id, error: "The turn has ended; this message cannot be withdrawn.".to_owned() }), &mut effects, scheduler);
                                }
                            }
                            SteerTarget::Managed { agent_id, turn_id } => {
                                let client = runtime.client.clone();
                                runtime.withdrawals.spawn(async move {
                                    let result = client
                                        .withdraw_steer(&agent_id, &turn_id, &message_id)
                                        .await
                                        .map_err(|error| error.to_string())
                                        .and_then(|response| {
                                            if response.turn_id == turn_id
                                                && response.message_id == message_id
                                            {
                                                Ok(response.withdrawn)
                                            } else {
                                                Err("Withdrawal acknowledged a different message"
                                                    .to_owned())
                                            }
                                        });
                                    (pane, id, generation, result)
                                });
                            }
                        }
                    }
                    RootEffect::PersistSteerWithdrawal { text } => {
                        let record =
                            runtime.local_record(LocalEvent::UserSteerWithdrawn { text })?;
                        absorb(
                            app.update(AppEvent::Transcript { pane, record }),
                            &mut effects,
                            scheduler,
                        );
                    }
                    RootEffect::Reconnect => {
                        if runtime.startup_attach {
                            if let Some(target) = runtime.retry_target.take() {
                                absorb(
                                    app.update(AppEvent::AgentConnecting(pane)),
                                    &mut effects,
                                    scheduler,
                                );
                                runtime.spawn_connection(ConnectionPurpose::Startup, target);
                            }
                        } else {
                            runtime.begin_recovery(app, scheduler, false);
                        }
                    }
                    RootEffect::PersistSteer { id, text } => {
                        // Only this request's own acknowledgement may release its fence.
                        // Uncorrelated shared telemetry never emits this effect.
                        if runtime
                            .unconfirmed_steer
                            .as_ref()
                            .is_some_and(|(pending, _, _)| *pending == id)
                        {
                            runtime.unconfirmed_steer = None;
                        }
                        let prompt = RecentPrompt { text: text.clone(), recorded_at_unix_ms: unix_ms(),
                            session_id: runtime.agent_id.clone(), workspace: runtime.workspace.clone() };
                        runtime.cache_prompts(vec![prompt.clone()]);
                        runtime.recent_prompts.insert(0, prompt);
                        runtime.recent_prompts.truncate(100);
                        let record = runtime.local_record(LocalEvent::UserSteered { text })?;
                        absorb(
                            app.update(AppEvent::Transcript { pane, record }),
                            &mut effects,
                            scheduler,
                        );
                    }
                    RootEffect::CancelTurns => {
                        runtime.shell_cancellation.cancel();
                        runtime.shell_cancellation = CancellationToken::new();
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
                        let managed_turns =
                            runtime.managed_active_turns.ids.iter().cloned().collect();
                        runtime.cancel_managed_turns(pane, managed_turns);
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
                        if let Err(error) = clipboard::copy_text(&text) {
                            tracing::warn!(%error, "failed to copy the mouse selection");
                            absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!("Clipboard copy failed: {error}"),
                                }),
                                &mut effects,
                                scheduler,
                            );
                        }
                    }
                    RootEffect::CopyResponse(_) => unreachable!("handled before pane routing"),
                    RootEffect::SetTheme(_) => {}
                    RootEffect::LoadReviewBranches { .. } => unreachable!("handled before pane routing"),
                    RootEffect::SearchSessions {
                        picker_id,
                        request_id,
                        query,
                    } => {
                        if let Some(task) = runtime.session_search_tasks.remove(&pane) {
                            task.abort();
                        }
                        if !query.trim().is_empty() && runtime.local.is_some() {
                            let task = runtime.session_searches.spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                let search = query.clone();
                                let result = tokio::task::spawn_blocking(move || {
                                    local::sessions::search(&search, 20)
                                })
                                .await
                                .map_err(|error| error.to_string())
                                .and_then(|result| result.map_err(|error| format!("{error:#}")));
                                SessionSearchCompletion {
                                    pane,
                                    picker_id,
                                    request_id,
                                    query,
                                    result,
                                }
                            });
                            runtime.session_search_tasks.insert(pane, task);
                        } else if !query.trim().is_empty() {
                            let client = runtime.client.clone();
                            let task = runtime.session_searches.spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                let result = client
                                    .find(&nanocodex_managed::FindSessionsRequest {
                                        query: query.clone(),
                                        limit: Some(20),
                                    })
                                    .await
                                    .map(|response| response.results)
                                    .map_err(|error| error.to_string());
                                SessionSearchCompletion {
                                    pane,
                                    picker_id,
                                    request_id,
                                    query,
                                    result,
                                }
                            });
                            runtime.session_search_tasks.insert(pane, task);
                        }
                    }
                    RootEffect::CancelSessionSearch => {
                        if let Some(task) = runtime.session_search_tasks.remove(&pane) {
                            task.abort();
                        }
                    }
                    RootEffect::LoadSessions { request_id, .. } if runtime.local.is_some() => {
                        // Scanning rollouts and journals reads the disk; keep it off the input loop.
                        let workspace = runtime.workspace.clone();
                        let cancellation = CancellationToken::new();
                        runtime
                            .session_list_cancellations
                            .insert((pane, request_id), cancellation.clone());
                        runtime.connection.spawn(async move {
                            let list = tokio::task::spawn_blocking(move || {
                                local::sessions::list(&workspace).map_err(|error| format!("{error:#}"))
                            });
                            ConnectionResult::LocalSessions {
                                pane,
                                request_id,
                                result: tokio::select! {
                                    () = cancellation.cancelled() => None,
                                    result = list => Some(result.map_err(|error| error.to_string()).and_then(|result| result)),
                                },
                            }
                        });
                    }
                    RootEffect::LoadSessions { request_id, .. } => {
                        let client = runtime.client.clone();
                        let cancellation = CancellationToken::new();
                        runtime
                            .session_list_cancellations
                            .insert((pane, request_id), cancellation.clone());
                        runtime.connection.spawn(async move {
                            ConnectionResult::Sessions {
                                pane,
                                request_id,
                                result: tokio::select! {
                                    () = cancellation.cancelled() => None,
                                    result = client.list() => Some(result),
                                },
                            }
                        });
                    }
                    RootEffect::CancelSessionList(request_id) => {
                        if let Some(cancellation) =
                            runtime.session_list_cancellations.get(&(pane, request_id))
                        {
                            cancellation.cancel();
                        }
                    }
                    RootEffect::CancelSessionResume => {
                        if let Some((task, _)) = runtime.pending_resume.take() {
                            task.abort();
                            if !runtime.managed_events_open {
                                runtime.begin_recovery(app, scheduler, true);
                            }
                        }
                    }
                    RootEffect::LoadRecentPrompts(_) => unreachable!("handled before pane routing"),
                    RootEffect::LoadOlderHistory => {
                        runtime.history_prefetch.request_replay();
                        runtime.start_requested_history_replay(pane);
                        runtime.start_history_prefetch(pane);
                    }
                    RootEffect::ResumeSession(agent_id) => {
                        if let Some(task) = runtime.session_search_tasks.remove(&pane) {
                            task.abort();
                        }
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
                        if runtime.local.is_some() {
                            // Local sessions resume from the rollout/journal store.
                            match runtime.local_relaunch(ConnectionPurpose::Resume(pane), Some(&agent_id)) {
                                Ok(resume) => runtime.pending_resume = Some((resume, pane)),
                                Err(error) => absorb(
                                    app.update(AppEvent::SessionLoadFailed { pane, error }),
                                    &mut effects,
                                    scheduler,
                                ),
                            }
                            continue;
                        }
                        let client = runtime.client.clone();
                        let resume = runtime.connection.spawn(async move {
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
                        runtime.pending_resume = Some((resume, pane));
                    }
                    RootEffect::Bug(description) => {
                        if runtime.agent_id.is_empty() || runtime.pending_resume.is_some() {
                            absorb(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: "Wait for the agent connection before starting /bug."
                                        .to_owned(),
                                }),
                                &mut effects,
                                scheduler,
                            );
                            continue;
                        }
                        let prompt = bug::debug_prompt(
                            &runtime.agent_id,
                            &runtime.observed_cursor,
                            &description,
                            &runtime.history_records,
                            &runtime.live_records,
                        );
                        let client = runtime.client.clone();
                        let settings = fresh_thread_settings(runtime.routing_enabled, runtime.settings);
                        let task = runtime.connection.spawn(async move {
                            ConnectionResult::Agent {
                                purpose: ConnectionPurpose::Bug(pane),
                                result: bug::launch(client, settings, prompt).await,
                            }
                        });
                        runtime.pending_resume = Some((task, pane));
                        absorb(
                            app.update(AppEvent::NotifySuccess {
                                pane,
                                message: "Starting a cloud agent to debug Nanocodex…".to_owned(),
                            }),
                            &mut effects,
                            scheduler,
                        );
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
                        let settings = fresh_thread_settings(root.composer().auto_routing(), AgentSettings {
                            model,
                            thinking: thinking_from_effort(root.composer().effort()),
                            reasoning_mode: managed_reasoning_mode(root.preferred_reasoning_mode()),
                            fast_mode: root.composer().fast_mode(),
                        });
                        request_render(app.update(AppEvent::VoiceStatus(None)), scheduler);
                        if let Some(local) = &mut runtime.local {
                            // /clear starts a fresh local agent, not the resumed session.
                            local.launch = local::sessions::fresh(&local.launch);
                        }
                        runtime.start_new_session(settings);
                        absorb(
                            app.update(AppEvent::NewSessionReady {
                                pane,
                                effort: effort_from_thinking(settings.thinking),
                                reasoning_mode: reasoning_mode_from_managed(
                                    settings.reasoning_mode,
                                ),
                                fast_mode: settings.fast_mode,
                                model: settings.model,
                                draft_reset: DraftReset::Clear,
                                skills: Arc::from([]),
                            }),
                            &mut effects,
                            scheduler,
                        );
                    }
                    RootEffect::Reflect(prompt) => {
                        let id = TurnId::new(runtime.next_turn);
                        runtime.next_turn = runtime.next_turn.saturating_add(1);
                        let prompt = prompt.prepend_text(
                        "Reflect on this managed conversation and return a concise, actionable report.".to_owned(),
                    );
                        let record = runtime.record_submission(id, &prompt)?;
                        absorb(
                            app.update(AppEvent::Transcript { pane, record }),
                            &mut effects,
                            scheduler,
                        );
                        runtime.start_submission(pane, id, prompt);
                    }
                    RootEffect::OpenLink(destination) => {
                        let client = runtime.client.clone();
                        let agent_id = runtime.agent_id.clone();
                        runtime.links.spawn(async move {
                            (pane, links::open(&client, &agent_id, &destination).await)
                        });
                    }
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
                        if let Some(bridge) = &runtime.control_bridge {
                            control::snapshot(bridge, app, runtime, true);
                        }
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
                    RootEffect::AutoRoute => runtime.enable_autoroute(pane),
                    RootEffect::SetModel(model) if runtime.local.is_some() => {
                        // Local model/harness switch before the first prompt.
                        let command = features::FeatureCommand::SwitchModel(model.to_string());
                        let handled = runtime.local.as_mut().is_some_and(|local| {
                            local.with_features(|features, cx| features.command(pane, &command, cx))
                        });
                        if !handled {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Model switching is unavailable for this local session".into() }), &mut effects, scheduler);
                        }
                    }
                    RootEffect::SetModel(model) => {
                        let root = app.root(pane).expect("model-selection pane must exist");
                        // Recheck availability at the public account boundary. Normalize
                        // only capabilities unsupported by the newly selected provider.
                        let catalog = match runtime.client.models().await {
                            Ok(catalog) => catalog,
                            Err(error) => {
                                request_render(app.update(AppEvent::SettingsHydrated { pane, effort: effort_from_thinking(runtime.settings.thinking), fast_mode: runtime.settings.fast_mode, model: runtime.settings.model }), scheduler);
                                absorb(app.update(AppEvent::NotifyError { pane, error: error.to_string() }), &mut effects, scheduler);
                                continue;
                            }
                        };
                        let Some(entry) = catalog.data.iter().find(|entry| entry.id == model) else {
                            request_render(app.update(AppEvent::SettingsHydrated { pane, effort: effort_from_thinking(runtime.settings.thinking), fast_mode: runtime.settings.fast_mode, model: runtime.settings.model }), scheduler);
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Model is no longer available to this account".into() }), &mut effects, scheduler);
                            continue;
                        };
                        let thinking = thinking_from_effort(root.composer().effort());
                        let thinking = if entry.thinking.contains(&thinking) { thinking } else {
                            model.default_thinking()
                        };
                        let preferred_mode = managed_reasoning_mode(root.preferred_reasoning_mode());
                        let requested = AgentSettings {
                            model,
                            thinking,
                            reasoning_mode: if entry.reasoning_modes.contains(&preferred_mode) { preferred_mode } else { nanocodex::ReasoningMode::Standard },
                            fast_mode: root.composer().fast_mode() && entry.fast_mode,
                        };
                        if !entry.thinking.contains(&requested.thinking) || !entry.reasoning_modes.contains(&requested.reasoning_mode) {
                            request_render(app.update(AppEvent::SettingsHydrated { pane, effort: effort_from_thinking(runtime.settings.thinking), fast_mode: runtime.settings.fast_mode, model: runtime.settings.model }), scheduler);
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Model catalog has no compatible text settings".into() }), &mut effects, scheduler);
                            continue;
                        }
                        if let Some(root) = app.root_mut(pane) { root.set_model_catalog(catalog.data); }
                        if runtime.agent.is_none() {
                            runtime.settings = requested;
                            runtime.pending_settings = Some(requested);
                            runtime.pending_autoroute = None;
                            if let Some(RetryTarget::Create(settings)) =
                                runtime.retry_target.as_mut()
                            {
                                *settings = fresh_thread_settings(false, requested);
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
                                && !gateway_model(runtime.settings.model)
                            {
                                settings.thinking = thinking;
                            }
                            continue;
                        }
                        runtime.queue_settings(pane, SettingsMutation::Thinking(thinking));
                    }
                    RootEffect::SetFastMode(enabled) => {
                        let supports_fast = if runtime.local.is_some() {
                            runtime.settings.model.as_str().parse::<nanocodex::HarnessModel>()
                                .is_ok_and(nanocodex::HarnessModel::supports_fast_mode)
                        } else { runtime.settings.model.supports_fast_mode() };
                        if enabled && !supports_fast {
                            absorb(app.update(AppEvent::NotifyError { pane, error: "Fast mode is unavailable for this model".into() }), &mut effects, scheduler);
                            continue;
                        }
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
                    RootEffect::SetMaxSubagents(limit)
                        if runtime.local.as_ref().is_some_and(|local| {
                            local.backend.as_ref().is_some_and(|backend| backend.parts.child_agents.is_some())
                        }) =>
                    {
                        // FEATURE-HOOK: local child-agent concurrency limit (legacy ChildAgents).
                        if let Some(child_agents) = runtime.local.as_ref()
                            .and_then(|local| local.backend.as_ref())
                            .and_then(|backend| backend.parts.child_agents.as_ref())
                        {
                            child_agents.set_max_concurrency(limit);
                        }
                        if let Some(root) = app.root_mut(pane) {
                            root.set_max_subagents(limit);
                        }
                        absorb(
                            app.update(AppEvent::NotifySuccess {
                                pane,
                                message: format!("Subagent limit set to {limit}"),
                            }),
                            &mut effects,
                            scheduler,
                        );
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
            let updated_at = summary.updated_at.max(summary.created_at);
            let timestamp = if updated_at < 10_000_000_000.0 {
                updated_at * 1_000.0
            } else {
                updated_at
            };
            Some(SessionSummary {
                session_id: agent_id.clone(),
                updated_at_unix_ms: timestamp.max(0.0) as u64,
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
    // Server controls must stay literal; retain shell output for the next task.
    if context.is_empty()
        || (!prompt.has_images()
            && prompt.display_text().split_whitespace().next() == Some("/goal"))
    {
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

fn terminal_error(error: io::Error) -> ManagedError {
    ManagedError::Configuration(format!("terminal error: {error}"))
}

// Private approval driver: tasks contain only safe metadata/ciphertext.
impl DriverRuntime {
    fn cancel_secure_input(&mut self) {
        let Some(flow) = &mut self.secure_input else {
            return;
        };
        let request = flow.request();
        let generation = flow.generation();
        flow.cancel_local();
        if !self.secure_input_attempted.insert(request.id().into()) {
            return;
        }
        let client = self.client.clone();
        self.secure_input_tasks.spawn(async move {
            let outcome = match &request {
                secure_input::Request::Sudo(r) => {
                    secure_input::Outcome::Sudo(sudo_input::Outcome::Status(
                        client
                            .cancel_native_secure_input(r)
                            .await
                            .map(|_| secure_input::Status::Cancelled)
                            .unwrap_or(secure_input::Status::Unknown),
                    ))
                }
                secure_input::Request::Private(r) => secure_input::Outcome::Private(
                    private_input::run(&client, r, private_input::Operation::Cancel).await,
                ),
            };
            (
                request.agent().into(),
                generation,
                request.id().into(),
                outcome,
            )
        });
    }
    fn secure_input_action(&mut self, action: secure_input::Action) {
        match action {
            secure_input::Action::None => {}
            secure_input::Action::Cancel => self.cancel_secure_input(),
            secure_input::Action::Dismiss => {
                if self.secure_input.as_ref().is_some_and(|f| f.can_dismiss()) {
                    self.secure_input.take();
                }
            }
            secure_input::Action::Browser => {
                if let Some(flow) = &self.secure_input
                    && let secure_input::Request::Private(r) = flow.request()
                    && let Ok(destination) = self.client.private_input_browser_url(&r)
                {
                    let client = self.client.clone();
                    let agent = self.agent_id.clone();
                    self.links.spawn(async move {
                        (
                            PaneId::Main,
                            links::open(&client, &agent, &destination).await,
                        )
                    });
                }
            }
            action => {
                let Some(flow) = &self.secure_input else {
                    return;
                };
                let request = flow.request();
                let generation = flow.generation();
                let terminal = matches!(
                    &action,
                    secure_input::Action::Sudo(_)
                        | secure_input::Action::Private(
                            private_input::Operation::Submit(_) | private_input::Operation::Finish
                        )
                );
                if !flow.is_sending()
                    || request.agent() != self.agent_id
                    || generation != self.connection_generation
                    || !request.is_current()
                    || (terminal && !self.secure_input_attempted.insert(request.id().into()))
                {
                    self.cancel_secure_input();
                    return;
                }
                let client = self.client.clone();
                self.secure_input_tasks.spawn(async move {
                    let outcome = match (&request, action) {
                        (secure_input::Request::Sudo(r), secure_input::Action::Sudo(envelope)) => {
                            let status = match client.submit_native_secure_input(r, envelope).await
                            {
                                Ok(v) => match v.status.as_str() {
                                    "completed" => secure_input::Status::Completed,
                                    "failed" => secure_input::Status::Failed,
                                    _ => secure_input::Status::Unknown,
                                },
                                Err(_) => secure_input::Status::Unknown,
                            };
                            secure_input::Outcome::Sudo(sudo_input::Outcome::Status(status))
                        }
                        (secure_input::Request::Private(r), secure_input::Action::Private(op)) => {
                            secure_input::Outcome::Private(private_input::run(&client, r, op).await)
                        }
                        _ => secure_input::Outcome::Private(private_input::Outcome::Failed),
                    };
                    (
                        request.agent().into(),
                        generation,
                        request.id().into(),
                        outcome,
                    )
                });
            }
        }
    }
}

/// Discard prequeued keys/paste before private UI transitions. FocusLost remains
/// meaningful even in the discarded tail; it must wipe the newly opened field.
async fn drain_private_input(input: &mut EventStream, runtime: &mut DriverRuntime) {
    // A mere now_or_never poll is insufficient: Crossterm's background reader
    // may not yet have parsed bytes already in the PTY kernel queue. Require a
    // quiet interval, while the field remains unrendered and disabled.
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        if Instant::now() >= deadline {
            runtime.cancel_secure_input();
            return;
        }
        let result = match tokio::time::timeout(Duration::from_millis(30), input.next()).await {
            Err(_) => return,
            Ok(Some(Ok(event))) => event,
            _ => {
                runtime.cancel_secure_input();
                return;
            }
        };
        let mut event = result;
        if matches!(event, Event::FocusLost | Event::FocusGained) {
            if let Some(flow) = &mut runtime.secure_input {
                let action = flow.intercept(event);
                runtime.secure_input_action(action);
            }
        } else if let Event::Paste(text) = &mut event {
            use zeroize::Zeroize;
            text.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CancelDisposition, CancelTarget, CancellationFences, CancellationResolution,
        CancellationToken, DriverRuntime, HistoryPrefetch, HistoryWindow, ManagedActiveTurns,
        SteerResolution, SteerTarget, cursor_at_or_before, decimal_successor, history_projection,
        history_projection_with_sequences, history_replay_matches, live_managed_projection,
        new_agent_settings, prepare_history_replay, session_summaries,
    };
    use crate::nanocodex2::config::ReasoningEffort;
    use crate::nanocodex2::tui::{
        components::QueueId, pane::PaneId, prompt::Submission, transcript::TurnId,
    };
    use nanocodex::Model;
    use nanocodex_managed::{
        AgentList, AgentSettings, AgentSummary, EventHistoryPage, ManagedApiKey, ManagedClient,
        ManagedError, ManagedEvent, ManagedEventData, PromptInput, Thinking,
    };
    use serde_json::{json, value::to_raw_value};
    use std::{
        collections::{BTreeMap, HashMap, HashSet, VecDeque},
        path::Path,
    };
    use tokio::task::JoinSet;

    #[test]
    fn goal_commands_preserve_pending_shell_context() {
        let mut context = vec!["Shell output: completed".to_owned()];
        for command in ["/goal", "/goal pause", "/goal resume"] {
            let prompt =
                super::inject_shell_context(&mut context, Submission::text(command.into()));
            assert_eq!(prompt.display_text(), command);
            assert_eq!(context.len(), 1);
        }
        let next = super::inject_shell_context(&mut context, Submission::text("continue".into()));
        assert_eq!(next.display_text(), "Shell output: completed\n\ncontinue");
        assert!(context.is_empty());
    }

    #[tokio::test]
    async fn bug_switch_discards_old_local_work_and_queued_recovery() {
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime.pending_submission = Some((
            PaneId::Main,
            TurnId::new(7),
            Submission::text("old input".into()),
        ));
        runtime.admitting.insert(TurnId::new(7));
        runtime.cancel_after_admission.insert(TurnId::new(7));
        runtime
            .local_managed_turns
            .insert(TurnId::new(7), "old-turn".into());
        runtime.recovery = Some(super::RecoveryPhase::Connecting);
        runtime
            .connection
            .spawn(async { super::ConnectionResult::Disconnected(Ok(())) });
        runtime.active_shells = 1;
        let old_shell_cancellation = runtime.shell_cancellation.clone();
        let source_id = runtime.agent_id.clone();

        runtime.detach_bug_source();

        assert!(runtime.connection.is_empty());
        assert!(runtime.pending_submission.is_none());
        assert!(runtime.admitting.is_empty());
        assert!(runtime.cancel_after_admission.is_empty());
        assert!(runtime.local_managed_turns.is_empty());
        assert!(runtime.recovery.is_none());
        assert!(old_shell_cancellation.is_cancelled());
        assert!(!runtime.shell_cancellation.is_cancelled());
        assert_eq!(runtime.active_shells, 0);
        assert_eq!(runtime.agent_id, source_id);
        assert!(runtime.cancellations.is_empty());
    }

    #[tokio::test]
    async fn reload_waits_for_local_work_but_not_durable_turns() {
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime
            .managed_active_turns
            .ids
            .insert("remote-turn".into());
        assert!(runtime.ready_for_reload());
        runtime.active_shells = 1;
        assert!(!runtime.ready_for_reload());
        runtime.active_shells = 0;
        runtime.connection.spawn(std::future::pending());
        assert!(!runtime.ready_for_reload());
        runtime.connection.abort_all();
        while runtime.connection.join_next().await.is_some() {}
        assert!(runtime.ready_for_reload());
        runtime.voice_tasks.spawn(std::future::pending());
        assert!(!runtime.ready_for_reload());
        runtime.voice_tasks.abort_all();
        while runtime.voice_tasks.join_next().await.is_some() {}
        assert!(runtime.ready_for_reload());
        runtime.clone_panel = Some(super::voice_clone::Panel::new("Synthetic speaker".into()));
        assert!(!runtime.ready_for_reload());
        runtime.clone_panel = None;
        assert!(runtime.ready_for_reload());
    }

    #[test]
    fn new_threads_do_not_inherit_a_routed_provider_model_or_effort() {
        let routed = AgentSettings {
            model: nanocodex::Model::Glm53.into(),
            thinking: nanocodex_managed::Thinking::High,
            ..AgentSettings::default()
        };
        assert_eq!(
            super::fresh_thread_settings(true, routed),
            new_agent_settings()
        );
        let manual = AgentSettings {
            model: nanocodex::Model::Sol.into(),
            ..AgentSettings::default()
        };
        assert_eq!(super::fresh_thread_settings(false, manual), manual);
    }

    #[tokio::test]
    async fn completed_cancelled_resume_cannot_finish_a_newer_switch() {
        let mut runtime = history_runtime(HistoryWindow::default());
        let old = runtime
            .connection
            .spawn(async { super::ConnectionResult::Disconnected(Ok(())) });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !old.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Cancelling after completion leaves an Ok result queued in JoinSet.
        old.abort();
        let (release, ready) = tokio::sync::oneshot::channel::<()>();
        let fresh = runtime.connection.spawn(async move {
            ready.await.unwrap();
            super::ConnectionResult::Disconnected(Ok(()))
        });
        let fresh_id = fresh.id();
        runtime.pending_resume = Some((fresh, PaneId::Main));
        let (old_id, _) = runtime
            .connection
            .join_next_with_id()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old_id, old.id());
        assert!(runtime.finish_resume(old_id).is_none());
        assert_eq!(runtime.pending_resume.as_ref().unwrap().0.id(), fresh_id);
        release.send(()).unwrap();
        let (completed, _) = runtime
            .connection
            .join_next_with_id()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(runtime.finish_resume(completed), Some(PaneId::Main));
        assert!(runtime.pending_resume.is_none());
        assert!(runtime.finish_resume(completed).is_none());
    }

    #[tokio::test]
    async fn voice_requested_during_startup_starts_once_when_connected_with_selected_controls() {
        use crate::nanocodex2::voice::Command;
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime.agent_id.clear();
        assert_eq!(
            runtime.voice_command(PaneId::Main, Command::Toggle),
            Ok(None)
        );
        assert!(runtime.take_ready_voice().is_none());
        assert_eq!(
            runtime.voice_status().unwrap().phase,
            crate::nanocodex2::voice_state::Phase::Connecting
        );
        runtime
            .voice_command(PaneId::Main, Command::Start(Some("ember")))
            .unwrap();
        runtime
            .voice_command(PaneId::Main, Command::ToggleMute)
            .unwrap();
        // Repeated explicit start keeps the selected voice and mute preference.
        runtime
            .voice_command(PaneId::Main, Command::Start(None))
            .unwrap();
        runtime.agent_id = "connected-agent".into();
        assert!(runtime.take_ready_voice().is_none());
        runtime.managed_events_open = true;
        let ready = runtime.take_ready_voice().unwrap();
        assert_eq!(
            ready.selection,
            crate::nanocodex2::voice::Selection::Chatgpt("ember")
        );
        assert!(ready.muted);
        assert!(runtime.take_ready_voice().is_none());
    }

    #[tokio::test]
    async fn clone_panel_never_starts_or_uploads_implicitly() {
        use crate::nanocodex2::voice::Command;
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime
            .voice_command(PaneId::Main, Command::CloneOpen("Synthetic voice".into()))
            .unwrap();
        assert!(runtime.voice_tasks.is_empty());
        assert!(runtime.clone_panel.as_ref().unwrap().tasks.is_empty());
        assert!(
            runtime
                .voice_command(PaneId::Main, Command::CloneSubmit)
                .is_err()
        );
        assert!(
            runtime
                .voice_command(PaneId::Main, Command::Start(None))
                .is_err()
        );
        assert!(runtime.voice_status().unwrap().text.contains("Ready"));
        runtime
            .voice_command(PaneId::Main, Command::CloneRecord(None))
            .unwrap();
        assert!(runtime.pending_voice.is_none());
        assert!(runtime.clone_panel.as_ref().unwrap().tasks.is_empty());
        runtime
            .voice_command(PaneId::Main, Command::CloneCancel)
            .unwrap();
        assert!(runtime.clone_panel.is_none());
        assert!(runtime.voice_tasks.is_empty());
    }

    #[test]
    fn voice_sample_paths_expand_home_without_shell_expansion() {
        use std::path::PathBuf;
        let workspace = Path::new("/workspace");
        let home = Some(PathBuf::from("/home/speaker"));
        assert_eq!(
            super::resolve_voice_sample_path(workspace, "~/audio sample.wav".into(), home.clone())
                .unwrap(),
            PathBuf::from("/home/speaker/audio sample.wav")
        );
        assert_eq!(
            super::resolve_voice_sample_path(workspace, "samples/voice.wav".into(), home.clone())
                .unwrap(),
            PathBuf::from("/workspace/samples/voice.wav")
        );
        assert_eq!(
            super::resolve_voice_sample_path(workspace, "/audio/voice.wav".into(), home.clone())
                .unwrap(),
            PathBuf::from("/audio/voice.wav")
        );
        assert_eq!(
            super::resolve_voice_sample_path(workspace, "~other/$(example).wav".into(), home)
                .unwrap(),
            PathBuf::from("/workspace/~other/$(example).wav")
        );
        assert!(super::resolve_voice_sample_path(workspace, "~/audio.wav".into(), None).is_err());
    }

    #[test]
    fn voice_provider_settings_keep_valid_realtime_input() {
        use crate::nanocodex2::voice::Selection;
        use nanocodex_voice_protocol::VoiceOutputProvider;
        let eleven = super::voice_settings(&Selection::ElevenLabs("sample_voice".into()));
        assert_eq!(eleven.output_provider, VoiceOutputProvider::Elevenlabs);
        assert_eq!(eleven.eleven_labs_voice_id.as_deref(), Some("sample_voice"));
        eleven.validate_chatgpt().unwrap();
        let chatgpt = super::voice_settings(&Selection::Chatgpt("ember"));
        assert_eq!(chatgpt.output_provider, VoiceOutputProvider::Openai);
        assert_eq!(chatgpt.voice, "ember");
        assert!(chatgpt.eleven_labs_voice_id.is_none());
    }

    #[tokio::test]
    async fn voice_provider_selection_survives_stop_and_replaces_pending_start() {
        use crate::nanocodex2::voice::{Command, Selection};
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime
            .voice_command(
                PaneId::Main,
                Command::Select(Selection::ElevenLabs("sample_voice".into())),
            )
            .unwrap();
        runtime.voice_command(PaneId::Main, Command::Stop).unwrap();
        runtime
            .voice_command(PaneId::Main, Command::Start(None))
            .unwrap();
        runtime.managed_events_open = true;
        assert_eq!(
            runtime.take_ready_voice().unwrap().selection,
            Selection::ElevenLabs("sample_voice".into())
        );
        runtime
            .voice_command(
                PaneId::Main,
                Command::Select(Selection::ElevenLabs("second".into())),
            )
            .unwrap();
        runtime
            .voice_command(PaneId::Main, Command::Start(Some("ember")))
            .unwrap();
        assert_eq!(
            runtime.take_ready_voice().unwrap().selection,
            Selection::Chatgpt("ember")
        );
    }

    #[tokio::test]
    async fn queued_voice_can_be_cancelled_and_does_not_leak_to_a_new_session() {
        use crate::nanocodex2::voice::Command;
        let mut runtime = history_runtime(HistoryWindow::default());
        for stop in [Command::Stop, Command::Toggle] {
            runtime
                .voice_command(PaneId::Main, Command::Start(None))
                .unwrap();
            runtime.voice_command(PaneId::Main, stop).unwrap();
            runtime.managed_events_open = true;
            assert!(runtime.take_ready_voice().is_none());
            assert!(runtime.voice_status().is_none());
        }
        runtime
            .voice_command(PaneId::Main, Command::Start(None))
            .unwrap();
        runtime.start_new_session(new_agent_settings());
        assert!(runtime.pending_voice.is_none());
    }

    #[tokio::test]
    async fn queued_voice_waits_for_recovery_and_session_switch() {
        use crate::nanocodex2::voice::Command;
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime
            .voice_command(PaneId::Main, Command::Toggle)
            .unwrap();
        runtime.managed_events_open = true;
        runtime.recovery = Some(super::RecoveryPhase::Replaying);
        assert!(runtime.take_ready_voice().is_none());
        runtime.recovery = None;
        let task = runtime.connection.spawn(std::future::pending());
        runtime.pending_resume = Some((task, PaneId::Main));
        assert!(runtime.take_ready_voice().is_none());
        runtime.pending_resume.take().unwrap().0.abort();
        runtime
            .voice_command(PaneId::Main, Command::ToggleMute)
            .unwrap();
        runtime
            .voice_command(PaneId::Main, Command::Unmute)
            .unwrap();
        assert!(!runtime.take_ready_voice().unwrap().muted);
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
            control_bridge: None,
            btw: None,
            btw_events: tokio::sync::mpsc::unbounded_channel().0,
            screen: crate::nanocodex2::tui::screen::Controller::new(Some(
                ratatui_image::picker::Picker::halfblocks(),
            )),
            pending_voice: None,
            voice_selection: Default::default(),
            voice_tasks: JoinSet::new(),
            clone_panel: None,
            voice: None,
            client: ManagedClient::new("http://127.0.0.1:9", api_key).unwrap(),
            local: None,
            feature_overlay: None,
            feature_instructions: HashMap::new(),
            feature_completions: HashMap::new(),
            feature_turns: HashSet::new(),
            local_voice_status: None,
            harness_relaunching: false,
            queued_relaunch: None,
            agent: None,
            startup_attach: false,
            pending_resume: None,
            managed_events: None,
            managed_events_open: false,
            recovery: None,
            recovery_events: VecDeque::new(),
            observed_cursor: "0".to_owned(),
            last_recovery: None,
            connection_generation: 1,
            agent_id: "agent-1".to_owned(),
            settings: AgentSettings::default(),
            pending_settings: None,
            pending_autoroute: None,
            routing_enabled: false,
            routing_resolved: false,
            routing_generation: 0,
            routing_updates: JoinSet::new(),
            workspace: Path::new("/workspace").to_path_buf(),
            sequence,
            next_turn: sequence,
            next_shell: 1,
            controls: HashMap::new(),
            local_managed_turns: HashMap::new(),
            local_terminal_turns: HashSet::new(),
            submitted_turns: HashSet::new(),
            detached_submissions: HashSet::new(),
            unacknowledged_inputs: HashMap::new(),
            confirmed_requests: HashSet::new(),
            managed_active_turns: ManagedActiveTurns::default(),
            admitting: HashSet::new(),
            cancel_after_admission: HashSet::new(),
            cancellation_fences: CancellationFences::default(),
            cancellation_failed: false,
            cancellation_had_effect: false,
            admissions: JoinSet::new(),
            completions: JoinSet::new(),
            steers: JoinSet::new(),
            receipt_reconciliations: JoinSet::new(),
            unresolved_steers: HashMap::new(),
            vault_tasks: JoinSet::new(),
            secure_input: None,
            secure_input_tasks: JoinSet::new(),
            secure_input_attempted: HashSet::new(),
            share_tasks: JoinSet::new(),
            site_tasks: JoinSet::new(),
            done_updates: JoinSet::new(),
            vault_attempted: HashSet::new(),
            steer_receipts: HashMap::new(),
            pending_withdrawals: HashSet::new(),
            withdrawals: JoinSet::new(),
            pending_steer_target: None,
            waiting_steers: VecDeque::new(),
            unconfirmed_steer: None,
            cancellations: JoinSet::new(),
            settings_updates: JoinSet::new(),
            settings_queue: VecDeque::new(),
            shells: JoinSet::new(),
            links: JoinSet::new(),
            history_loads: JoinSet::new(),
            history_replays: JoinSet::new(),
            history_prefetch: HistoryPrefetch::default(),
            history_tree_open: false,
            history_tree_failed: false,
            history_generation: 1,
            history,
            history_sequences,
            history_records,
            live_records: Vec::new(),
            active_shells: 0,
            shell_cancellation: CancellationToken::new(),
            shell_context: Vec::new(),
            pending_submission: None,
            recent_prompts,
            prompt_cache: Err("disabled in runtime fixture".into()),
            prompt_cache_writes: JoinSet::new(),
            prompt_cache_pending: Vec::new(),
            prompt_cache_retry_at: None,
            prompt_cache_retries: 1,
            prompt_cache_warned: false,
            recent_prompt_request: 0,
            recent_prompt_loads: HashMap::new(),
            connection: JoinSet::new(),
            session_list_cancellations: HashMap::new(),
            review_branch_loads: JoinSet::new(),
            session_searches: JoinSet::new(),
            session_search_tasks: HashMap::new(),
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
            SteerResolution::Failed
        );
        assert_eq!(
            runtime.resolve_steer(
                1,
                &target,
                Err(super::SteerFailure::Other("lost ack".into()))
            ),
            SteerResolution::Unconfirmed {
                error: "lost ack".into(),
                active: true,
            }
        );
        runtime.managed_active_turns.ids.remove("turn-7");
        assert_eq!(
            runtime.resolve_steer(1, &target, Ok(())),
            SteerResolution::Admitted
        );
        assert_eq!(
            runtime.resolve_steer(
                1,
                &target,
                Err(super::SteerFailure::Other("lost ack".into()))
            ),
            SteerResolution::Unconfirmed {
                error: "lost ack".into(),
                active: false,
            }
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
    fn undo_waiting_steer_removes_only_the_exact_message_once() {
        let first = QueueId::new(1);
        let second = QueueId::new(2);
        let mut waiting = VecDeque::from([
            (PaneId::Main, first, Submission::text("first".to_owned())),
            (PaneId::Main, second, Submission::text("second".to_owned())),
        ]);
        assert!(super::withdraw_waiting_steer(
            &mut waiting,
            PaneId::Main,
            second
        ));
        assert!(!super::withdraw_waiting_steer(
            &mut waiting,
            PaneId::Main,
            second
        ));
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].1, first);
        assert_eq!(waiting[0].2.display_text(), "first");
    }

    #[test]
    fn managed_active_turns_reconcile_cursor_order_idempotently() {
        let mut active = ManagedActiveTurns {
            ids: HashSet::from(["attached-1".to_owned()]),
            order: vec!["attached-1".to_owned()],
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
    fn local_submission_survives_delayed_acceptance_and_replay_without_duplicates() {
        let mut runtime = history_runtime(HistoryWindow::default());
        let record = runtime
            .record_submission(TurnId::new(1), &Submission::text("start work".to_owned()))
            .unwrap();
        assert_eq!(record.kind(), "user.submitted");
        assert_eq!(runtime.live_records.len(), 1);
        assert_eq!(runtime.recent_prompts[0].text, "start work");
        assert!(runtime.agent.is_none());

        runtime.submitted_turns.insert("turn-42".to_owned());
        for _ in 0..2 {
            assert!(
                runtime
                    .project_managed_event(managed_turn("42", "start work"))
                    .unwrap()
                    .is_none()
            );
        }
        runtime.submitted_turns.clear();
        runtime.detached_submissions.insert("turn-42".to_owned());
        assert!(
            runtime
                .project_managed_event(managed_turn("42", "start work"))
                .unwrap()
                .is_none()
        );
        // Identical text from another client is still a separate message.
        assert!(
            runtime
                .project_managed_event(managed_turn("43", "start work"))
                .unwrap()
                .is_some()
        );
        assert_eq!(runtime.live_records.len(), 1);
    }

    #[tokio::test]
    async fn autoroute_waits_for_connection_and_does_not_leak_into_new_sessions() {
        let mut runtime = history_runtime(HistoryWindow::default());
        runtime.agent_id.clear();
        runtime.enable_autoroute(PaneId::Main);
        assert_eq!(runtime.pending_autoroute, Some(PaneId::Main));
        assert!(runtime.settings_updates.is_empty());
        assert!(runtime.admissions.is_empty());
        runtime.start_new_session(new_agent_settings());
        assert!(runtime.pending_autoroute.is_none());
    }

    #[tokio::test]
    async fn autoroute_settings_update_holds_the_first_prompt_until_it_settles() {
        let mut runtime = history_runtime(HistoryWindow::default());
        // A pending API receipt fences the first prompt, including non-keyboard input.
        runtime.settings_updates.spawn(std::future::pending());
        runtime.start_submission(
            PaneId::Main,
            TurnId::new(1),
            Submission::text("first task".into()),
        );
        assert!(runtime.admissions.is_empty());
        assert!(runtime.submitted_turns.is_empty());
        assert_eq!(
            runtime
                .pending_submission
                .as_ref()
                .unwrap()
                .2
                .display_text(),
            "first task"
        );
    }

    #[tokio::test]
    async fn new_session_holds_input_for_its_connection_and_discards_previous_projection() {
        let mut runtime = history_runtime(HistoryWindow {
            events: vec![managed_turn("1", "old thread")],
            ..HistoryWindow::default()
        });
        runtime.submitted_turns.insert("old-turn".to_owned());
        runtime
            .detached_submissions
            .insert("old-detached-turn".to_owned());
        runtime.shell_context.push("old shell output".to_owned());
        runtime.managed_events_open = true;
        let old_generation = runtime.history_generation;

        runtime.start_new_session(new_agent_settings());
        assert!(runtime.agent_id.is_empty());
        assert!(!runtime.managed_events_open);
        assert!(runtime.history_records.is_empty());
        assert!(runtime.recent_prompts.is_empty());
        assert!(runtime.submitted_turns.is_empty());
        assert!(runtime.detached_submissions.is_empty());
        assert!(runtime.shell_context.is_empty());
        assert_ne!(runtime.history_generation, old_generation);

        let prompt = Submission::text("new thread".to_owned());
        runtime.record_submission(TurnId::new(2), &prompt).unwrap();
        runtime.start_submission(PaneId::Main, TurnId::new(2), prompt);
        assert!(runtime.admissions.is_empty());
        assert_eq!(runtime.live_records.len(), 1);
        assert_eq!(
            runtime
                .pending_submission
                .as_ref()
                .unwrap()
                .2
                .display_text(),
            "new thread"
        );
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
        let error = match prepare_history_replay(
            EventHistoryPage {
                data: Vec::new(),
                has_more: true,
                latest_cursor: "5".to_owned(),
            },
            sequences.clone(),
            2,
            Vec::new(),
            Vec::new(),
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        ) {
            Ok(_) => panic!("invalid page unexpectedly projected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("empty nonterminal page"));
        assert_eq!(history.before.as_deref(), Some("5"));
        assert!(history.has_more);
        assert_eq!(history.events.len(), 1);
        assert_eq!(history.events[0].cursor, "5");
        assert_eq!(sequences, HashMap::from([("5".to_owned(), 1)]));
    }

    #[tokio::test]
    async fn history_replay_keeps_input_received_while_projection_runs() {
        let mut runtime = history_runtime(HistoryWindow {
            events: vec![managed_turn("9", "retained prompt")],
            before: Some("9".to_owned()),
            has_more: true,
        });
        let before = runtime.history_prefetch.claim(&runtime.history).unwrap();
        runtime
            .history_prefetch
            .store(
                &before,
                EventHistoryPage {
                    data: vec![managed_turn("7", "older prompt")],
                    has_more: false,
                    latest_cursor: "9".to_owned(),
                },
            )
            .unwrap();
        runtime.history_prefetch.request_replay();
        runtime.start_requested_history_replay(PaneId::Main);
        runtime
            .record_submission(
                TurnId::new(50),
                &Submission::text("input during replay".to_owned()),
            )
            .unwrap();
        let (_, _, _, _, result) = runtime.history_replays.join_next().await.unwrap().unwrap();
        let projection = runtime.finish_history_replay(PaneId::Main, result).unwrap();
        let sequences = runtime
            .history_records
            .iter()
            .chain(&runtime.live_records)
            .map(|record| record.sequence())
            .collect::<Vec<_>>();
        assert_eq!(
            sequences.iter().copied().collect::<HashSet<_>>().len(),
            sequences.len(),
            "history and incoming events need distinct IDs"
        );
        let mut root = super::RootNode::new(Path::new("/workspace"), ReasoningEffort::Medium);
        root.install_session_projection(
            Path::new("/workspace"),
            ReasoningEffort::Medium,
            crate::nanocodex2::config::ReasoningMode::Standard,
            crate::nanocodex2::config::ReasoningMode::Standard,
            false,
            *projection,
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| root.render_focused(frame, frame.area(), &super::Theme::default(), true))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("older prompt"));
        assert!(rendered.contains("retained prompt"));
        assert!(rendered.contains("input during replay"));
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
        let first_before = runtime.history_prefetch.claim(&runtime.history).unwrap();
        runtime
            .history_prefetch
            .store(
                &first_before,
                EventHistoryPage {
                    data: vec![managed_turn("7", "older")],
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
        let (requested_before, _) = runtime
            .history_prefetch
            .take_requested(runtime.history.before.as_deref())
            .unwrap();
        assert_eq!(requested_before, "9");
        // A failed projection has consumed its page, while later pages remain
        // buffered. Inject that failure at the completion boundary.
        assert!(
            runtime
                .finish_history_replay(
                    PaneId::Main,
                    Err(ManagedError::Configuration(
                        "history projection task failed".to_owned()
                    ))
                )
                .is_err()
        );
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
    fn tool_result_loaded_before_its_call_is_restored_across_page_boundaries() {
        use crate::nanocodex2::tui::transcript::{EntryKind, ToolState, TranscriptModel};
        let mut sequences = HashMap::new();
        let mut next_sequence = 1;
        let (records, _) = history_projection_with_sequences(
            &[managed_turn("9", "newer")],
            "agent-1",
            Path::new("/workspace"),
            &mut sequences,
            &mut next_sequence,
        )
        .unwrap();
        let nested = |cursor: &str, kind: &str, payload| {
            ManagedEvent {
            cursor: cursor.to_owned(), created_at: None, turn_id: Some("turn-6".to_owned()),
            data: ManagedEventData::Event { event: to_raw_value(&json!({"protocol_version": 1, "request_id": "agent-1", "seq": cursor.parse::<u64>().unwrap(), "type": kind, "payload": payload})).unwrap(), agent_id: None },
        }
        };
        let first = prepare_history_replay(
            EventHistoryPage { data: vec![nested("8", "tool.result", json!({"call_id": "old-call", "tool": "read_file", "status": "completed", "duration_ns": 10, "result": {"text": "PAGE_BOUNDARY_RESULT"}, "structured_result": null, "metadata": null}))], has_more: true, latest_cursor: "9".to_owned() },
            sequences, next_sequence, records, Vec::new(), "agent-1", Path::new("/workspace"), ReasoningEffort::Medium,
        ).unwrap();
        assert_eq!(first.history_records.len(), 2);
        let result_sequence = first.history_records[0].sequence();
        let second = prepare_history_replay(
            EventHistoryPage { data: vec![managed_turn("6", "older"), nested("7", "tool.call", json!({"call_id": "old-call", "tool": "read_file", "arguments": {"path": "old.txt"}}))], has_more: false, latest_cursor: "9".to_owned() },
            first.sequences, first.next_sequence, first.history_records, Vec::new(), "agent-1", Path::new("/workspace"), ReasoningEffort::Medium,
        ).unwrap();
        assert_eq!(
            second
                .history_records
                .iter()
                .find(|record| record.kind() == "tool.result")
                .unwrap()
                .sequence(),
            result_sequence
        );
        let mut model = TranscriptModel::default();
        for record in second.history_records {
            model.apply(&record);
        }
        let tools = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].state, ToolState::Succeeded);
        assert!(
            tools[0]
                .result
                .as_ref()
                .unwrap()
                .to_string()
                .contains("PAGE_BOUNDARY_RESULT")
        );
    }

    #[test]
    fn older_page_without_a_prompt_is_preserved_before_the_retained_tail() {
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
            "agent-1",
            Path::new("/workspace"),
            ReasoningEffort::Medium,
        )
        .unwrap();

        assert_eq!(prepared.history_records.len(), 2);
        assert_eq!(prepared.history_records[0].kind(), "display.error");
        assert!(prepared.older_prompts.is_empty());
        assert_eq!(prepared.sequences.len(), 2);
    }

    #[test]
    fn replay_preserves_records_before_the_first_loaded_prompt() {
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

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind(), "display.error");
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
                    presentation: None,
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
        assert_eq!(sessions[0].updated_at_unix_ms, 1_750_000_100_000);
    }
}
