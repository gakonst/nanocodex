// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Root layout and component event routing.

use super::{
    actions::{Action, ActionAvailability, ActionsEffect, ActionsEvent, ActionsMenu},
    composer::{
        Composer, ComposerChromeTarget, ComposerDraft, ComposerEffect, ComposerEvent,
        SettingsCommand,
    },
    context_diagnostics::{
        ContextDiagnosticsEffect, ContextDiagnosticsEvent, ContextDiagnosticsPanel,
    },
    effort::{EffortEffect, EffortEvent, EffortSelector},
    file_finder::{FileFinder, FileFinderEffect, FileFinderEvent},
    floating::Floating,
    keybindings::{KeybindingsEffect, KeybindingsEvent, KeybindingsHelp},
    model_selector::{ModelSelector, ModelSelectorEffect, ModelSelectorEvent},
    node::{Component, ComponentUpdate, Node, RenderRequest},
    queue::{MessageQueue, QueueEffect, QueueEvent, QueueId},
    recent_prompt_picker::{RecentPromptPicker, RecentPromptPickerEffect, RecentPromptPickerEvent},
    review_confirmation::{
        ReviewConfirmationEffect, ReviewConfirmationEvent, ReviewDownloadConfirmation,
    },
    selection::{Selection, Surface, TextSpan},
    session_picker::{SessionPicker, SessionPickerEffect, SessionPickerEvent, SessionPickerMode},
    skill_picker::{SkillPicker, SkillPickerEffect, SkillPickerEvent},
    subagents::{SubagentEffect, SubagentOverlay, SubagentTree},
    theme_selector::{ThemeSelector, ThemeSelectorEffect, ThemeSelectorEvent},
    transcript::{ScrollCommand, Transcript, TranscriptEvent},
};
use crate::{
    config::{ReasoningEffort, ReasoningMode},
    skill::Skill,
    tui::{
        context::ContextDiagnostics,
        prompt::Submission,
        session::{RecentPrompt, SessionSummary},
        theme::{Theme, ThemeMode},
        transcript::TranscriptRecord,
    },
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use nanocodex::Model;
use nanocodex_subagents::{AgentId, AgentStatus, AgentUpdate, MessageSender};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use semver::Version;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const KEY_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2);
const SELECTION_SCROLL_INTERVAL: Duration = Duration::from_millis(60);
const BREADCRUMB_DURATION: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Eq, PartialEq)]
enum ConfirmationAction {
    Interrupt,
    CancelReview,
    Exit,
}

impl ConfirmationAction {
    const fn title_key(self) -> &'static str {
        match self {
            Self::Interrupt => "Esc",
            Self::CancelReview => "Esc",
            Self::Exit => "Ctrl+C",
        }
    }

    const fn action_label(self) -> &'static str {
        match self {
            Self::Interrupt => "Interrupt",
            Self::CancelReview => "Cancel review",
            Self::Exit => "Quit",
        }
    }

    const fn effect(self) -> RootEffect {
        match self {
            Self::Interrupt => RootEffect::CancelTurns,
            Self::CancelReview => RootEffect::CancelReview,
            Self::Exit => RootEffect::Shutdown,
        }
    }
}

struct KeyConfirmation {
    action: ConfirmationAction,
    deadline: Instant,
}

struct Notification {
    message: Line<'static>,
    color: Color,
    deadline: Instant,
}

struct SelectionAutoScroll {
    direction: isize,
    position: Position,
    deadline: Instant,
}

impl Notification {
    fn plain(message: String, color: Color) -> Self {
        Self {
            message: Line::styled(
                message,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            color,
            deadline: Instant::now() + BREADCRUMB_DURATION,
        }
    }

    fn update_available(version: Version) -> Self {
        let green = Style::default().fg(Color::Green);
        Self {
            message: Line::from(vec![
                Span::styled("Update available · ", green),
                Span::styled(format!("v{version}"), green.add_modifier(Modifier::BOLD)),
                Span::styled(" · update Nanocodex2 to apply", green),
            ]),
            color: Color::Green,
            deadline: Instant::now() + BREADCRUMB_DURATION,
        }
    }
}

pub(crate) enum RootEvent {
    VoiceStatus(Option<crate::voice_state::Status>),
    ShowAgentId(String),
    VaultReview(crate::tui::vault::Review),
    VaultReceipt(String),
    Terminal(Event),
    PasteImage(String),
    #[cfg(test)]
    ContextTokens(u64),
    Transcript(Arc<TranscriptRecord>),
    ExternalTranscript(Arc<TranscriptRecord>),
    AgentStreamClosed,
    AgentConnecting,
    AgentReconnected {
        active_turns: usize,
        pending_local: bool,
        reasoning_mode: ReasoningMode,
    },
    AgentReconnectFailed(String),
    Subagent(AgentUpdate),
    ReplaceDraft(String),
    HandoffFinished(String),
    HandoffCancelled,
    HandoffFailed(String),
    ReviewStarted,
    ReviewReady(String),
    ReviewCancelled,
    ReviewFinished(String),
    ReviewFailed(String),
    WorkerTurnFinished {
        terminal_expected: bool,
    },
    ManagedTurnFinished,
    ManagedActiveTurns(usize),
    ShellFinished,
    TurnsCancelled,
    ForkReady,
    NewSessionFailed(String),
    SessionSearchResults {
        picker_id: u64,
        request_id: u64,
        query: String,
        result: Result<Vec<nanocodex_managed::SessionSearchHit>, String>,
    },
    SessionsLoaded {
        request_id: u64,
        sessions: Vec<SessionSummary>,
    },
    SessionListFailed {
        request_id: u64,
        error: String,
    },
    RecentPromptsLoaded {
        session_id: String,
        prompts: Vec<RecentPrompt>,
    },
    RecentPromptLoadFailed(String),
    SessionLoadFailed(String),
    SessionRestored {
        draft_reset: DraftReset,
        projection: Box<RestoredSessionProjection>,
        effort: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        fast_mode: bool,
        model: Model,
        skills: Arc<[Skill]>,
    },
    SettingsHydrated {
        effort: ReasoningEffort,
        fast_mode: bool,
        model: Model,
    },
    HistoryReplayed {
        projection: Box<RestoredSessionProjection>,
    },
    NotifyError(String),
    NotifySuccess(String),
    ConfirmReviewDownload,
    UpdateAvailable(Version),
    SteerAdmitted(QueueId),
    SteerWithdrawn(QueueId),
    SteerWithdrawalFailed {
        id: QueueId,
        error: String,
    },
    SteerUnconfirmed(QueueId),
    RetainPrompt {
        request_id: String,
        prompt: Submission,
    },
    PromptConfirmed(String),
    SteerFailed {
        id: QueueId,
    },
    AnimationFrame(Instant),
}

pub(crate) struct RestoredSessionProjection {
    transcript: Transcript,
    context_diagnostics: ContextDiagnostics,
    context_tokens: Option<u64>,
    recent_prompts: Vec<RecentPromptDraft>,
    seen_vault_requests: std::collections::HashSet<String>,
}

impl RestoredSessionProjection {
    pub(crate) fn append_records(
        &mut self,
        records: impl IntoIterator<Item = Arc<TranscriptRecord>>,
    ) {
        for record in records {
            if self.transcript.ignores_finished_run_event(&record) {
                continue;
            }
            if let Some(prompt) = recent_prompt(&record) {
                self.recent_prompts.push(prompt);
            }
            let observation = self.context_diagnostics.observe(&record);
            if observation.completed_tokens.is_some() {
                self.context_tokens = observation.completed_tokens;
            }
            if let Some((key, _)) = crate::tui::vault::request(&record) {
                self.seen_vault_requests.insert(key);
            }
            let _ = self.transcript.update(TranscriptEvent::Record(record));
        }
    }

    pub(crate) fn close_stream(&mut self) {
        let _ = self.transcript.update(TranscriptEvent::AgentStreamClosed);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecentPromptDraft {
    pub(crate) text: String,
    pub(crate) recorded_at_unix_ms: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum SessionListKind {
    Resume,
    Mention,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RootEffect {
    Reload,
    Bug(String),
    Screen,
    Zoom,
    Voice(crate::voice::Command),
    ShowAgentId,
    Vault(crate::tui::vault::Command),
    ApproveVault(crate::tui::vault::Review),
    Submit(Submission),
    Reflect(Submission),
    RunShell(String),
    ContinueSubagent(Submission),
    OpenDraftEditor,
    OpenConfigEditor,
    OpenLink(String),
    ReloadConfig,
    NewSession(Model),
    SearchSessions {
        picker_id: u64,
        request_id: u64,
        query: String,
    },
    CancelSessionSearch,
    LoadSessions {
        request_id: u64,
        kind: SessionListKind,
    },
    CancelSessionList(u64),
    CancelSessionResume,
    LoadRecentPrompts(Vec<RecentPromptDraft>),
    LoadOlderHistory,
    ResumeSession(String),
    Steer {
        id: QueueId,
        prompt: Submission,
    },
    WithdrawSteer {
        id: QueueId,
    },
    PersistSteerWithdrawal {
        text: String,
    },
    Reconnect,
    PersistSteer {
        id: QueueId,
        text: String,
    },
    Copy(String),
    Handoff,
    Review {
        download_assets: bool,
    },
    SetEffort {
        effort: ReasoningEffort,
        reasoning_mode: ReasoningMode,
    },
    SetModel(Model),
    SetFastMode(bool),
    SetMaxSubagents(usize),
    SetTheme(ThemeMode),
    Fork,
    CancelTurns,
    CancelReview,
    CancelHandoff,
    Shutdown,
}

enum Overlay {
    VaultReview(crate::tui::vault::Review),
    AgentId(String),
    Actions(Node<ActionsMenu>),
    ContextDiagnostics(Node<ContextDiagnosticsPanel>),
    Effort(Node<EffortSelector>),
    Model(Node<ModelSelector>),
    Theme(Node<ThemeSelector>),
    FileFinder(FileMention),
    Skills(SkillMention),
    Keybindings(Node<KeybindingsHelp>),
    RecentPrompts(Node<RecentPromptPicker>),
    Sessions(Node<SessionPicker>),
    ReviewDownload(Node<ReviewDownloadConfirmation>),
    Subagents(SubagentOverlay),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingTask {
    Handoff,
    Review,
}

struct FileMention {
    finder: Node<FileFinder>,
    start: usize,
}

struct SkillMention {
    picker: Node<SkillPicker>,
    start: usize,
}

struct QueueEdit {
    id: QueueId,
    original_draft: Option<ComposerDraft>,
    original_input_mode: Option<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ThreadState {
    New,
    Started,
}

#[derive(Clone, Copy)]
pub(crate) enum DraftReset {
    Clear,
    Preserve,
}

/// Owns layout and routing so future screen components do not widen the event loop.
pub(crate) struct RootNode {
    transcript: Node<Transcript>,
    composer: Node<Composer>,
    queue: Node<MessageQueue>,
    workspace: PathBuf,
    overlay: Option<Overlay>,
    thread: ThreadState,
    key_confirmation: Option<KeyConfirmation>,
    notification: Option<Notification>,
    voice_status: Option<crate::voice_state::Status>,
    discarded_draft: Option<ComposerDraft>,
    last_admitted_steer: Option<(QueueId, Submission)>,
    withdrawn_draft: Option<ComposerDraft>,
    withdrawing_steer: Option<QueueId>,
    withdrawing_prompt: Option<Submission>,
    withdrawing_remote: bool,
    queue_edit: Option<QueueEdit>,
    selection: Selection,
    selection_auto_scroll: Option<SelectionAutoScroll>,
    transcript_area: Rect,
    composer_area: Rect,
    composer_content_area: Rect,
    queue_area: Rect,
    in_flight_turns: usize,
    managed_active_turns: usize,
    unmatched_worker_turns: usize,
    unmatched_agent_turns: usize,
    in_flight_shells: usize,
    blocking_task: Option<BlockingTask>,
    review_url: Option<String>,
    fork_available: bool,
    skills: Arc<[Skill]>,
    interactive: bool,
    resuming_session: bool,
    reconnecting: Option<bool>,
    unconfirmed_prompts: HashMap<String, QueueId>,
    confirmed_queue_edit: Option<QueueId>,
    theme_mode: ThemeMode,
    preferred_reasoning_mode: ReasoningMode,
    subagents: SubagentTree,
    context_diagnostics: ContextDiagnostics,
    recent_prompts: Vec<RecentPromptDraft>,
    seen_vault_requests: std::collections::HashSet<String>,
    pending_session_mention: Option<usize>,
    pending_session_list: Option<u64>,
    next_session_list: u64,
    reflection_input: bool,
}

impl RootNode {
    pub(crate) fn new(workspace: &Path, thinking: ReasoningEffort) -> Self {
        let mut transcript = Transcript::with_effort(thinking);
        transcript.set_workspace(workspace);
        let mut subagents = SubagentTree::new(thinking);
        subagents.set_workspace(workspace);
        Self {
            transcript: Node::new(transcript),
            composer: Node::new(Composer::new(workspace, thinking)),
            queue: Node::new(MessageQueue::default()),
            workspace: workspace.to_path_buf(),
            overlay: None,
            thread: ThreadState::New,
            key_confirmation: None,
            notification: None,
            voice_status: None,
            discarded_draft: None,
            last_admitted_steer: None,
            withdrawn_draft: None,
            withdrawing_steer: None,
            withdrawing_prompt: None,
            withdrawing_remote: false,
            queue_edit: None,
            selection: Selection::default(),
            selection_auto_scroll: None,
            transcript_area: Rect::default(),
            composer_area: Rect::default(),
            composer_content_area: Rect::default(),
            queue_area: Rect::default(),
            in_flight_turns: 0,
            managed_active_turns: 0,
            unmatched_worker_turns: 0,
            unmatched_agent_turns: 0,
            in_flight_shells: 0,
            blocking_task: None,
            review_url: None,
            fork_available: true,
            skills: Arc::from([]),
            interactive: true,
            resuming_session: false,
            reconnecting: None,
            unconfirmed_prompts: HashMap::new(),
            confirmed_queue_edit: None,
            theme_mode: ThemeMode::Auto,
            preferred_reasoning_mode: ReasoningMode::Standard,
            subagents,
            context_diagnostics: ContextDiagnostics::default(),
            recent_prompts: Vec::new(),
            seen_vault_requests: Default::default(),
            pending_session_mention: None,
            pending_session_list: None,
            next_session_list: 0,
            reflection_input: false,
        }
    }

    pub(crate) fn fork(&self, workspace: &Path, thinking: ReasoningEffort) -> Self {
        let mut root = Self::new(workspace, thinking);
        root.transcript = Node::new(self.transcript.component().fork_snapshot());
        root.composer
            .component_mut()
            .update(ComposerEvent::ContextTokens(
                self.composer.component().context_tokens(),
            ));
        root.set_fast_mode(self.composer.component().fast_mode());
        root.set_model(self.composer.component().model());
        root.set_reasoning_modes(
            self.composer.component().reasoning_mode(),
            self.preferred_reasoning_mode,
        );
        root.set_max_subagents(self.subagents.max_subagents());
        root.thread = ThreadState::Started;
        root.fork_available = false;
        root.set_skills(Arc::clone(&self.skills));
        root.theme_mode = self.theme_mode;
        root.context_diagnostics = self.context_diagnostics.clone();
        root.interactive = false;
        root.composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Forking session…".to_owned()),
                now: Instant::now(),
            });
        root
    }

    pub(crate) fn set_fork_available(&mut self, available: bool) {
        self.fork_available = available;
        self.refresh_actions();
    }

    pub(crate) fn set_skills(&mut self, skills: Arc<[Skill]>) {
        self.skills = skills;
        if self.skills.is_empty() && matches!(&self.overlay, Some(Overlay::Skills(_))) {
            self.overlay = None;
        }
    }

    pub(crate) fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.theme_mode = mode;
    }

    pub(crate) fn set_fast_mode(&mut self, enabled: bool) {
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetFastMode(enabled));
    }

    pub(crate) fn set_model(&mut self, model: Model) {
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetModel(model));
    }

    pub(crate) fn set_reasoning_modes(&mut self, actual: ReasoningMode, preferred: ReasoningMode) {
        self.preferred_reasoning_mode = preferred;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetReasoningMode(actual));
    }

    pub(crate) const fn set_preferred_reasoning_mode(&mut self, mode: ReasoningMode) {
        self.preferred_reasoning_mode = mode;
    }

    pub(crate) const fn preferred_reasoning_mode(&self) -> ReasoningMode {
        self.preferred_reasoning_mode
    }

    pub(crate) fn set_max_subagents(&mut self, limit: usize) {
        self.subagents.set_max_subagents(limit);
    }

    pub(crate) fn reset_session(
        &mut self,
        workspace: &Path,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        draft_reset: DraftReset,
    ) {
        let current_draft = self.composer.component_mut().take_draft();
        let previous_discarded_draft = self.discarded_draft.take();
        let withdrawn_draft = self.withdrawn_draft.take();
        let replaced_draft = current_draft.is_some() && matches!(draft_reset, DraftReset::Clear);
        let (preserved_draft, discarded_draft) = match draft_reset {
            DraftReset::Clear => (None, current_draft.or(previous_discarded_draft)),
            DraftReset::Preserve => (current_draft, previous_discarded_draft),
        };
        let fork_available = self.fork_available;
        let theme_mode = self.theme_mode;
        let max_subagents = self.subagents.max_subagents();
        let next_session_list = self.next_session_list;
        *self = Self::new(workspace, thinking);
        self.next_session_list = next_session_list;
        self.set_reasoning_modes(reasoning_mode, preferred_reasoning_mode);
        self.discarded_draft = discarded_draft;
        self.withdrawn_draft = withdrawn_draft;
        self.fork_available = fork_available;
        self.theme_mode = theme_mode;
        self.set_max_subagents(max_subagents);
        if let Some(draft) = preserved_draft {
            self.composer.component_mut().restore_draft(draft);
        }
        if replaced_draft {
            self.show_draft_saved();
        }
    }

    #[allow(dead_code, reason = "used by restoration benchmarks and focused tests")]
    pub(crate) fn restore_session(
        &mut self,
        workspace: &Path,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        fast_mode: bool,
        records: Vec<Arc<TranscriptRecord>>,
    ) {
        let projection = Self::project_session(thinking, records);
        self.install_session_projection(
            workspace,
            thinking,
            reasoning_mode,
            preferred_reasoning_mode,
            fast_mode,
            projection,
        );
    }

    pub(crate) fn project_session(
        thinking: ReasoningEffort,
        records: Vec<Arc<TranscriptRecord>>,
    ) -> RestoredSessionProjection {
        Self::project_session_with_stream_state(thinking, records, true)
    }

    pub(crate) fn project_open_session(
        thinking: ReasoningEffort,
        records: Vec<Arc<TranscriptRecord>>,
    ) -> RestoredSessionProjection {
        Self::project_session_with_stream_state(thinking, records, false)
    }

    fn project_session_with_stream_state(
        thinking: ReasoningEffort,
        records: Vec<Arc<TranscriptRecord>>,
        stream_closed: bool,
    ) -> RestoredSessionProjection {
        let mut projection = RestoredSessionProjection {
            transcript: Transcript::with_effort(thinking),
            context_diagnostics: ContextDiagnostics::default(),
            context_tokens: None,
            recent_prompts: Vec::new(),
            seen_vault_requests: Default::default(),
        };
        projection.append_records(records);
        if stream_closed {
            projection.close_stream();
        }
        projection
    }

    fn replay_history(&mut self, mut projection: RestoredSessionProjection) {
        projection
            .transcript
            .preserve_viewport_from(self.transcript.component());
        projection.transcript.set_workspace(&self.workspace);
        projection
            .transcript
            .set_effort(self.composer.component().effort());
        self.seen_vault_requests
            .extend(projection.seen_vault_requests);
        self.transcript = Node::new(projection.transcript);
        self.context_diagnostics = projection.context_diagnostics;
        self.recent_prompts = projection.recent_prompts;
        if let Some(tokens) = projection.context_tokens {
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::ContextTokens(tokens));
        }
    }

    pub(crate) fn install_session_projection(
        &mut self,
        workspace: &Path,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        fast_mode: bool,
        mut projection: RestoredSessionProjection,
    ) {
        let preserve_active_submission = !self.resuming_session
            && (self.has_active_turns()
                || !self.queue.component().is_empty()
                || self.queue.component().has_pending_steer());
        let started = preserve_active_submission || !projection.recent_prompts.is_empty();
        if preserve_active_submission {
            self.workspace = workspace.to_path_buf();
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::SetEffort(thinking));
            self.set_reasoning_modes(reasoning_mode, preferred_reasoning_mode);
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active: true,
                    status: Some("Thinking…".to_owned()),
                    now: Instant::now(),
                });
        } else {
            self.reset_session(
                workspace,
                thinking,
                reasoning_mode,
                preferred_reasoning_mode,
                DraftReset::Clear,
            );
        }
        self.set_fast_mode(fast_mode);
        projection.transcript.set_workspace(workspace);
        self.seen_vault_requests
            .extend(projection.seen_vault_requests);
        self.transcript = Node::new(projection.transcript);
        self.context_diagnostics = projection.context_diagnostics;
        self.recent_prompts = projection.recent_prompts;
        if let Some(tokens) = projection.context_tokens {
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::ContextTokens(tokens));
        }
        self.thread = if started {
            ThreadState::Started
        } else {
            ThreadState::New
        };
    }

    pub(crate) fn allows_pane_switch(&self) -> bool {
        self.overlay.is_none() && !self.composer.component().draft().starts_with('/')
    }

    pub(crate) const fn composer(&self) -> &Composer {
        self.composer.component()
    }

    pub(crate) fn render_focused(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: bool,
    ) {
        self.render_root(frame, area, theme, focused);
    }

    pub(crate) fn animation_deadline(&self) -> Option<Instant> {
        let selector = match &self.overlay {
            Some(Overlay::Effort(selector)) => selector.component().animation_deadline(),
            Some(Overlay::Model(selector)) => selector.component().animation_deadline(),
            _ => None,
        };
        [
            selector,
            self.transcript.component().animation_deadline(),
            self.composer.component().animation_deadline(),
            self.queue.component().animation_deadline(),
            self.key_confirmation
                .as_ref()
                .map(|confirmation| confirmation.deadline),
            self.notification.as_ref().map(|notice| notice.deadline),
            self.selection_auto_scroll
                .as_ref()
                .map(|scroll| scroll.deadline),
            self.subagents.animation_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn render_root(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, focused: bool) {
        self.refresh_actions();
        let height = self
            .composer
            .component_mut()
            .desired_height(area.width)
            .min(area.height);
        let composer_area = Rect {
            y: area.bottom().saturating_sub(height),
            height,
            ..area
        };
        self.composer_area = composer_area;
        let voice_height = if self.voice_status.is_some() {
            1.min(area.height.saturating_sub(height))
        } else {
            0
        };
        let voice_area = Rect {
            y: composer_area.y.saturating_sub(voice_height),
            height: voice_height,
            ..area
        };
        let queue_height = self.queue.component().desired_height().min(
            area.height
                .saturating_sub(height)
                .saturating_sub(voice_height),
        );
        let queue_width = area.width.saturating_mul(95) / 100;
        let queue_area = Rect {
            x: area.x + area.width.saturating_sub(queue_width) / 2,
            y: voice_area.y.saturating_sub(queue_height),
            width: queue_width,
            height: queue_height,
        };
        self.queue_area = queue_area;
        let transcript_area = Rect {
            height: area
                .height
                .saturating_sub(height)
                .saturating_sub(queue_height)
                .saturating_sub(voice_height),
            ..area
        };
        self.transcript_area = transcript_area;
        self.composer_content_area = if composer_area.width >= 2 && composer_area.height >= 3 {
            Rect::new(
                composer_area.x + 1,
                composer_area.y + 1,
                composer_area.width - 2,
                composer_area.height - 2,
            )
        } else {
            Rect {
                height: composer_area.height.min(1),
                ..composer_area
            }
        };
        self.transcript.render(frame, transcript_area, theme);
        self.queue.render(frame, queue_area, theme);
        if let Some(status) = &self.voice_status {
            super::voice::render(frame, status, voice_area);
        }
        let composer_selection = (self.selection.surface() == Some(Surface::Composer))
            .then(|| self.selection.range())
            .flatten();
        self.composer.component_mut().render_focused_with_selection(
            frame,
            composer_area,
            theme,
            focused
                && self.blocking_task.is_none()
                && !self.transcript.component().expandables_focused()
                && (!self.queue.component().focused() || self.queue_edit.is_some()),
            composer_selection,
        );
        if self.selection.surface() == Some(Surface::Transcript)
            && let Some(range) = self.selection.range()
        {
            self.transcript
                .component()
                .render_selection(frame.buffer_mut(), range);
        }
        self.transcript
            .component_mut()
            .render_chrome(frame, transcript_area, theme);
        if let Some(overlay) = &mut self.overlay {
            match overlay {
                Overlay::VaultReview(review) => {
                    let layout = Floating::new(
                        "Approve Vault website",
                        86,
                        24,
                        &[("ctrl+enter", "approve"), ("esc", "cancel")],
                    )
                    .render(frame, area, theme);
                    let lines =
                        crate::tui::vault::review_lines(&review.description(), layout.body.width);
                    review.visible = lines.len() <= usize::from(layout.body.height);
                    if review.visible {
                        frame.render_widget(Paragraph::new(lines.join("\n")), layout.body);
                    } else {
                        frame.render_widget(Paragraph::new("Enlarge the terminal to review the complete website approval. Approval is disabled until all details fit. Esc cancels.").wrap(Wrap { trim: false }), layout.body);
                    }
                }
                Overlay::AgentId(id) => {
                    let layout =
                        Floating::new("Agent ID", 58, 7, &[("enter", "copy"), ("esc", "close")])
                            .render(frame, area, theme);
                    frame.render_widget(
                        Paragraph::new(id.as_str())
                            .style(Style::default().fg(theme.accent()))
                            .wrap(Wrap { trim: false }),
                        layout.body,
                    );
                }
                Overlay::Actions(actions) => actions.render(frame, area, theme),
                Overlay::ContextDiagnostics(panel) => panel.render(frame, area, theme),
                Overlay::Effort(selector) => selector.render(frame, area, theme),
                Overlay::Model(selector) => selector.render(frame, area, theme),
                Overlay::Theme(selector) => selector.render(frame, area, theme),
                Overlay::FileFinder(mention) => mention.finder.render(frame, area, theme),
                Overlay::Skills(mention) => mention.picker.render(frame, area, theme),
                Overlay::Keybindings(help) => help.render(frame, area, theme),
                Overlay::RecentPrompts(picker) => picker.render(frame, area, theme),
                Overlay::Sessions(picker) => picker.render(frame, area, theme),
                Overlay::ReviewDownload(confirmation) => {
                    confirmation.render(frame, area, theme);
                }
                Overlay::Subagents(SubagentOverlay::Tree) => {
                    self.subagents.render_tree(frame, area, theme);
                }
                Overlay::Subagents(SubagentOverlay::Transcript(id)) => {
                    self.subagents.render_transcript(*id, frame, area, theme);
                }
            }
        }
        if let Some(notification) = &self.notification {
            render_notification(
                frame,
                area,
                theme,
                &notification.message,
                notification.color,
            );
        }
        if let Some(confirmation) = &self.key_confirmation {
            render_key_confirmation(frame, area, composer_area, theme, confirmation.action);
        }
    }

    fn update_terminal(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if self.voice_status.is_some() && is_control_key(&event, 'x') {
            if matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Press) {
                return ComponentUpdate::none();
            }
            return self
                .apply_settings_command(SettingsCommand::Voice(crate::voice::Command::ToggleMute));
        }
        if matches!(event, Event::Resize(_, _)) {
            self.selection.clear();
            self.selection_auto_scroll = None;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if is_confirmation_key_repeat(&event) {
            return ComponentUpdate::none();
        }
        if self.pending_session_list.is_some() && (is_escape(&event) || is_control_c(&event)) {
            return self.cancel_session_list();
        }
        if self.resuming_session && (is_escape(&event) || is_control_c(&event)) {
            self.resuming_session = false;
            self.key_confirmation = None;
            let mut update = self.restore_session_activity();
            self.notification = Some(Notification::plain(
                "Session switch cancelled.".to_owned(),
                Color::Yellow,
            ));
            update.effects.push(RootEffect::CancelSessionResume);
            return update;
        }
        if self.reflection_input && is_escape(&event) {
            return self.cancel_reflection();
        }
        if self.blocking_task.is_some() && is_control_c(&event) {
            return self.update_key_confirmation(ConfirmationAction::Exit, Instant::now());
        }
        match self.blocking_task {
            Some(BlockingTask::Review) => return self.update_review_input(event),
            Some(BlockingTask::Handoff) => return self.update_handoff_input(event),
            None => {}
        }
        if is_control_c(&event) {
            if self.overlay.is_none()
                && !self.queue.component().focused()
                && !self.transcript.component().expandables_focused()
                && !self.composer.component().draft().is_empty()
            {
                self.key_confirmation = None;
                return self.discard_draft();
            }
            return self.update_key_confirmation(ConfirmationAction::Exit, Instant::now());
        }
        if is_escape(&event)
            && self
                .key_confirmation
                .as_ref()
                .is_some_and(|confirmation| confirmation.action == ConfirmationAction::Exit)
        {
            self.key_confirmation = None;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let confirmation_cleared =
            !is_escape(&event) && !is_key_release(&event) && self.key_confirmation.take().is_some();
        let mut update = self.update_terminal_without_confirmation(event);
        if confirmation_cleared {
            update.render = update.render.max(RenderRequest::Immediate);
        }
        update
    }

    pub(crate) fn refresh_terminal_images(&mut self) {
        self.transcript.component_mut().refresh_terminal_images();
        self.subagents.refresh_terminal_images();
    }

    fn update_terminal_without_confirmation(
        &mut self,
        mut event: Event,
    ) -> ComponentUpdate<RootEffect> {
        if self.resuming_session {
            return ComponentUpdate::none();
        }
        if let Some(connecting) = self.reconnecting {
            // Local editing and shell controls remain available while Enter
            // is reserved for reconnecting to the managed agent.
            if self.queue_edit.is_some() && is_escape(&event) {
                return self.finish_queue_edit(false);
            }
            if is_escape(&event) {
                if self.selection.clear() {
                    self.selection_auto_scroll = None;
                    self.key_confirmation = None;
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                if self.overlay.is_none() {
                    if self.queue.component().focused() {
                        self.key_confirmation = None;
                        return self.update_queue(event);
                    }
                    if self.transcript.component().expandables_focused() {
                        self.key_confirmation = None;
                        return self.update_transcript(TranscriptEvent::BlurExpandables);
                    }
                    if self.in_flight_shells > 0 {
                        return self.update_key_confirmation(
                            ConfirmationAction::Interrupt,
                            Instant::now(),
                        );
                    }
                }
            }
            if is_submit_enter(&event) {
                // Voice is a local control: accept it while ordinary prompts
                // remain fenced behind the managed connection.
                if self.queue_edit.is_none()
                    && !self.queue.component().focused()
                    && !self.composer.component().has_images()
                    && matches!(
                        self.composer.component().draft().split_whitespace().next(),
                        Some("/voice" | "/screen" | "/zoom" | "/reload")
                    )
                {
                    let mut update = self
                        .update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
                    if !connecting && update.effects.iter().any(|effect| matches!(effect,
                        RootEffect::Voice(crate::voice::Command::Start(_))
                        | RootEffect::Voice(crate::voice::Command::Toggle) if self.voice_status.is_none()))
                    {
                        self.reconnecting = Some(true);
                        update.render = update.render.max(self.reconnection_status("Reconnecting…").render);
                        update.effects.push(RootEffect::Reconnect);
                    }
                    return update;
                }
                if connecting {
                    return ComponentUpdate::none();
                }
                self.reconnecting = Some(true);
                let mut update = self.reconnection_status("Reconnecting…");
                update.effects.push(RootEffect::Reconnect);
                return update;
            }
            if is_focus_toggle(&event) {
                return ComponentUpdate::none();
            }
            // Draft editing stays available while submission is paused.
            if matches!(&event, Event::Paste(_))
                || matches!(&event, Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat))
            {
                return self.edit_composer(ComposerEvent::Terminal(event));
            }
            return self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
        }
        if !self.interactive || self.pending_session_list.is_some() {
            return ComponentUpdate::none();
        }
        if self.queue_edit.is_some() {
            return self.update_queue_editor(event);
        }
        if self.reflection_input && is_submit_enter(&event) {
            return self.submit_reflection();
        }
        if let Some(Overlay::Subagents(SubagentOverlay::Transcript(id))) = self.overlay
            && is_control_key(&event, 'o')
        {
            let render = if self.subagents.toggle_expand_all(id) {
                RenderRequest::Immediate
            } else {
                RenderRequest::None
            };
            return ComponentUpdate::render(render);
        }
        if self.overlay.is_some() {
            return self.update_overlay(event, Instant::now());
        }
        if matches!(&event, Event::Key(key)
            if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::ALT
                && key.kind == KeyEventKind::Press)
        {
            return self.undo_latest_message();
        }
        if is_control_key(&event, 'z')
            && !self.queue.component().focused()
            && !self.transcript.component().expandables_focused()
        {
            return self.restore_discarded_draft();
        }
        if is_control_key(&event, 'o') {
            return self.update_transcript(TranscriptEvent::ToggleExpandAll);
        }
        if is_control_key(&event, 's') {
            return self.open_effort();
        }
        if is_control_key(&event, 'd') {
            return self.open_model();
        }
        if is_control_key(&event, 'r') {
            return self.load_recent_prompts();
        }
        if is_control_key(&event, 't') {
            return self.open_fork();
        }
        if is_escape(&event) {
            if self.selection.clear() {
                self.selection_auto_scroll = None;
                self.key_confirmation = None;
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            if self.queue.component().focused() {
                self.key_confirmation = None;
                return self.update_queue(event);
            }
            if self.transcript.component().expandables_focused() {
                self.key_confirmation = None;
                return self.update_transcript(TranscriptEvent::BlurExpandables);
            }
            if self.has_active_turns() || self.in_flight_shells > 0 {
                return self.update_key_confirmation(ConfirmationAction::Interrupt, Instant::now());
            }
            let cleared = self
                .key_confirmation
                .as_ref()
                .is_some_and(|confirmation| confirmation.action == ConfirmationAction::Interrupt);
            self.key_confirmation = None;
            return ComponentUpdate::render(if cleared {
                RenderRequest::Immediate
            } else {
                RenderRequest::None
            });
        }
        if self.transcript.component().updates_banner_clicked(&event) {
            return self.update_transcript(TranscriptEvent::FollowTail);
        }
        if let Some(update) = self.update_selection_mouse(&mut event) {
            return update;
        }
        if let Some(destination) = self.transcript.component().link_destination(&event) {
            self.focus_composer();
            return ComponentUpdate {
                effects: vec![RootEffect::OpenLink(destination.to_string())],
                render: RenderRequest::Immediate,
            };
        }
        if let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        {
            let position = Position::new(mouse.column, mouse.row);
            match self.composer.component().chrome_target(position) {
                Some(ComposerChromeTarget::Effort) => return self.open_effort(),
                Some(ComposerChromeTarget::Model) => return self.open_model(),
                Some(ComposerChromeTarget::Subagents) => {
                    self.subagents.open_tree();
                    self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                None => {}
            }
        }
        if is_queue_shortcut(&event)
            && self.has_active_turns()
            && !self.queue.component().focused()
            && !self.transcript.component().expandables_focused()
            && !self.composer.component().draft().trim().is_empty()
        {
            return self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
        }
        if is_focus_toggle(&event) {
            return self.update_focus();
        }
        if is_left_click_in(&event, self.queue_area) {
            let Event::Mouse(mouse) = &event else {
                unreachable!("left click helper only accepts mouse events");
            };
            let _ = self
                .queue
                .component_mut()
                .focus_row(mouse.row, self.queue_area);
            let _ = self
                .transcript
                .component_mut()
                .update(TranscriptEvent::BlurExpandables);
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if is_left_click_in(&event, self.composer_area) {
            self.focus_composer();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if let Some(command) = self.transcript.component().expandable_command(&event) {
            self.queue.component_mut().set_focused(false);
            return self.update_transcript(TranscriptEvent::Expandable(command));
        }
        if is_left_click(&event) {
            self.focus_composer();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if self.queue.component().focused() {
            return self.update_queue(event);
        }
        if self.has_active_turns()
            && self.composer.component().draft().is_empty()
            && !self.queue.component().is_empty()
            && !self.queue.component().has_pending_steer()
            && is_plain_enter(&event)
        {
            return self.update_queue(event);
        }
        if !self.skills.is_empty()
            && !self.composer.component().draft().starts_with('!')
            && is_skill_picker_trigger(&event)
            && self.composer.component().cursor_is_at_token_boundary()
        {
            let start = self.composer.component().cursor();
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            self.overlay = Some(Overlay::Skills(SkillMention {
                picker: Node::new(SkillPicker::new(Arc::clone(&self.skills))),
                start,
            }));
            return update;
        }
        if is_file_finder_trigger(&event) && self.composer.component().cursor_is_at_token_boundary()
        {
            let start = self.composer.component().cursor();
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            self.overlay = Some(Overlay::FileFinder(FileMention {
                finder: Node::new(FileFinder::new(&self.workspace)),
                start,
            }));
            return update;
        }
        if !self.reflection_input
            && self.composer.component().draft().is_empty()
            && is_actions_trigger(&event)
        {
            self.overlay = Some(Overlay::Actions(Node::new(ActionsMenu::new(
                self.action_availability(),
            ))));
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if let Some(command) = self.transcript.component().scroll_command(&event) {
            let load_older = self.transcript.component().should_load_older_after(command);
            let transcript = self.transcript.update(TranscriptEvent::Scroll(command));
            return ComponentUpdate {
                effects: load_older
                    .then_some(RootEffect::LoadOlderHistory)
                    .into_iter()
                    .collect(),
                render: transcript.render,
            };
        }
        if self.transcript.component().expandables_focused() {
            return ComponentUpdate::none();
        }
        self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate)
    }

    fn update_review_input(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if is_control_key(&event, 't') {
            self.key_confirmation = None;
            return self.open_fork();
        }
        if is_plain_key(&event, 'o')
            && let Some(url) = &self.review_url
        {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![RootEffect::OpenLink(url.clone())],
                render: RenderRequest::None,
            };
        }
        if is_plain_key(&event, 'c')
            && let Some(url) = &self.review_url
        {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![RootEffect::Copy(url.clone())],
                render: RenderRequest::None,
            };
        }
        if is_escape(&event) {
            return self.update_key_confirmation(ConfirmationAction::CancelReview, Instant::now());
        }
        if is_key_release(&event) {
            return ComponentUpdate::none();
        }
        let confirmation_cleared = self.key_confirmation.take().is_some();
        ComponentUpdate::render(if confirmation_cleared {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn update_handoff_input(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if is_escape(&event) {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![RootEffect::CancelHandoff],
                render: RenderRequest::Immediate,
            };
        }
        if is_key_release(&event) {
            return ComponentUpdate::none();
        }
        let confirmation_cleared = self.key_confirmation.take().is_some();
        ComponentUpdate::render(if confirmation_cleared {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn update_selection_mouse(&mut self, event: &mut Event) -> Option<ComponentUpdate<RootEffect>> {
        let Event::Mouse(mouse) = event else {
            return None;
        };
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (surface, span) = self.selection_span_at(position)?;
                self.selection.begin(surface, span);
                self.selection_auto_scroll = None;
                Some(ComponentUpdate::render(RenderRequest::Immediate))
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let surface = self.selection.surface()?;
                let span = self.selection_span_on(surface, position)?;
                self.selection.drag(span);
                self.begin_selection_auto_scroll(surface, position);
                Some(ComponentUpdate::render(RenderRequest::Immediate))
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.selection.is_active() || self.selection.is_pending() =>
            {
                let rows = if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                let render = match self.selection.surface()? {
                    Surface::Transcript => {
                        self.transcript
                            .update(TranscriptEvent::Scroll(ScrollCommand::Rows(rows)));
                        RenderRequest::Immediate
                    }
                    Surface::Composer => {
                        let changed = self
                            .composer
                            .component_mut()
                            .scroll_selection(rows as isize, self.composer_content_area);
                        if changed {
                            RenderRequest::Immediate
                        } else {
                            RenderRequest::None
                        }
                    }
                };
                Some(ComponentUpdate::render(render))
            }
            MouseEventKind::Up(MouseButton::Left)
                if self.selection.is_active() || self.selection.is_pending() =>
            {
                let surface = self.selection.surface()?;
                self.selection_auto_scroll = None;
                let Some(span) = self.selection_span_on(surface, position) else {
                    self.selection.clear();
                    return Some(ComponentUpdate::render(RenderRequest::Immediate));
                };
                if !self.selection.finish(span) {
                    mouse.kind = MouseEventKind::Down(MouseButton::Left);
                    return None;
                }
                let range = self.selection.take_range()?;
                let text = match surface {
                    Surface::Transcript => self.transcript.component().selection_text(range),
                    Surface::Composer => self.composer.component().selection_text(range),
                };
                Some(ComponentUpdate {
                    effects: text.map(RootEffect::Copy).into_iter().collect(),
                    render: RenderRequest::Immediate,
                })
            }
            _ => None,
        }
    }

    fn selection_span_at(&mut self, position: Position) -> Option<(Surface, TextSpan)> {
        if self.composer_content_area.contains(position) {
            let span = self
                .composer
                .component_mut()
                .selection_span(position, self.composer_content_area)?;
            return Some((Surface::Composer, span));
        }
        if !self.transcript_area.contains(position) {
            return None;
        }
        let span = self.transcript.component().selection_span(position)?;
        Some((Surface::Transcript, span))
    }

    fn selection_span_on(&mut self, surface: Surface, position: Position) -> Option<TextSpan> {
        match surface {
            Surface::Transcript => {
                let position = clamp_to(position, self.transcript_area);
                self.transcript.component().selection_span_nearest(position)
            }
            Surface::Composer => {
                let position = clamp_to(position, self.composer_content_area);
                self.composer
                    .component_mut()
                    .selection_span(position, self.composer_content_area)
            }
        }
    }

    fn begin_selection_auto_scroll(&mut self, surface: Surface, position: Position) {
        let area = match surface {
            Surface::Transcript => self.transcript_area,
            Surface::Composer => self.composer_content_area,
        };
        let direction = if position.y <= area.y {
            -1
        } else if position.y >= area.bottom().saturating_sub(1) {
            1
        } else {
            self.selection_auto_scroll = None;
            return;
        };
        if let Some(scroll) = &mut self.selection_auto_scroll
            && scroll.direction == direction
        {
            scroll.position = position;
            return;
        }
        self.selection_auto_scroll = Some(SelectionAutoScroll {
            direction,
            position,
            deadline: Instant::now() + SELECTION_SCROLL_INTERVAL,
        });
    }

    fn scroll_selected_surface(&mut self, surface: Surface, rows: isize) -> bool {
        match surface {
            Surface::Transcript => {
                self.transcript
                    .update(TranscriptEvent::Scroll(ScrollCommand::Rows(rows as i32)));
                true
            }
            Surface::Composer => self
                .composer
                .component_mut()
                .scroll_selection(rows, self.composer_content_area),
        }
    }

    fn update_key_confirmation(
        &mut self,
        action: ConfirmationAction,
        now: Instant,
    ) -> ComponentUpdate<RootEffect> {
        let confirmed = self.key_confirmation.as_ref().is_some_and(|confirmation| {
            confirmation.action == action && now <= confirmation.deadline
        });
        if confirmed {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![action.effect()],
                render: RenderRequest::Immediate,
            };
        }
        self.key_confirmation = Some(KeyConfirmation {
            action,
            deadline: now + KEY_CONFIRMATION_TIMEOUT,
        });
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_overlay(&mut self, event: Event, now: Instant) -> ComponentUpdate<RootEffect> {
        match &self.overlay {
            Some(Overlay::VaultReview(_)) => self.update_vault_review(event),
            Some(Overlay::AgentId(_)) => self.update_agent_id(event),
            Some(Overlay::Actions(_)) => self.update_actions(event),
            Some(Overlay::ContextDiagnostics(_)) => self.update_context_diagnostics(event),
            Some(Overlay::Effort(_)) => self.update_effort(EffortEvent::Terminal { event, now }),
            Some(Overlay::Model(_)) => {
                self.update_model(ModelSelectorEvent::Terminal { event, now })
            }
            Some(Overlay::Theme(_)) => {
                self.update_theme_selector(ThemeSelectorEvent::Terminal(event))
            }
            Some(Overlay::FileFinder(_)) => self.update_file_finder(event),
            Some(Overlay::Skills(_)) => self.update_skill_picker(event),
            Some(Overlay::Keybindings(_)) => self.update_keybindings(event),
            Some(Overlay::RecentPrompts(_)) => self.update_recent_prompt_picker(event),
            Some(Overlay::Sessions(_)) => self.update_session_picker(event),
            Some(Overlay::ReviewDownload(_)) => self.update_review_confirmation(event),
            Some(Overlay::Subagents(SubagentOverlay::Tree)) => {
                let effect = self.subagents.update_tree(event);
                self.apply_subagent_effect(effect)
            }
            Some(Overlay::Subagents(SubagentOverlay::Transcript(id))) => {
                let effect = self.subagents.update_transcript(*id, event);
                self.apply_subagent_effect(effect)
            }
            None => ComponentUpdate::none(),
        }
    }

    fn apply_subagent_effect(
        &mut self,
        effect: Option<SubagentEffect>,
    ) -> ComponentUpdate<RootEffect> {
        match effect {
            Some(SubagentEffect::Dismiss) => {
                self.subagents.finish_camera_animation();
                self.overlay = None;
            }
            Some(SubagentEffect::Inspect(id)) => {
                self.subagents.finish_camera_animation();
                self.overlay = Some(Overlay::Subagents(SubagentOverlay::Transcript(id)));
            }
            Some(SubagentEffect::Back) => {
                self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
            }
            Some(SubagentEffect::OpenLink(destination)) => {
                return ComponentUpdate {
                    effects: vec![RootEffect::OpenLink(destination)],
                    render: RenderRequest::None,
                };
            }
            Some(SubagentEffect::SetMaxSubagents(limit)) => {
                return ComponentUpdate {
                    effects: vec![RootEffect::SetMaxSubagents(limit)],
                    render: RenderRequest::Immediate,
                };
            }
            None => {}
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_file_finder(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::FileFinder(mention)) = &self.overlay else {
            return ComponentUpdate::none();
        };
        let start = mention.start;

        if is_key_release(&event) {
            return ComponentUpdate::none();
        }

        let starts_session_mention = is_file_finder_trigger(&event)
            && self
                .mention_query(start, '@')
                .is_some_and(|query| query.is_empty());
        if starts_session_mention {
            let composer =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            let mut sessions = self.load_session_mentions(start);
            sessions.render = sessions.render.max(composer.render);
            return sessions;
        }

        if is_mention_edit(&event) {
            let keep_open = mention_edit_continues_query(&event, is_file_query_character);
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            let query = if keep_open {
                self.mention_query(start, '@')
            } else {
                None
            };
            let Some(query) = query else {
                self.overlay = None;
                return update;
            };
            if let Some(Overlay::FileFinder(mention)) = &mut self.overlay {
                let _ = mention.finder.update(FileFinderEvent::Query(query));
            }
            return update;
        }

        if !is_picker_navigation(&event) {
            self.overlay = None;
            if is_escape(&event) {
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            let mut update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            update.render = update.render.max(RenderRequest::Immediate);
            return update;
        }

        let Some(Overlay::FileFinder(mention)) = &mut self.overlay else {
            unreachable!("file mention was checked above");
        };
        let update = mention.finder.update(FileFinderEvent::Terminal(event));
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            FileFinderEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            FileFinderEffect::Insert(path) => self.update_composer(
                ComposerEvent::ReplaceRange {
                    range: start..self.composer.component().cursor(),
                    text: format!("@{path} "),
                },
                RenderRequest::Immediate,
            ),
        }
    }

    fn update_skill_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Skills(mention)) = &self.overlay else {
            return ComponentUpdate::none();
        };
        let start = mention.start;

        if is_key_release(&event) {
            return ComponentUpdate::none();
        }

        if is_mention_edit(&event) {
            let keep_open = mention_edit_continues_query(&event, is_skill_query_character);
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            let query = if keep_open {
                self.mention_query(start, '$')
            } else {
                None
            };
            let Some(query) = query else {
                self.overlay = None;
                return update;
            };
            if let Some(Overlay::Skills(mention)) = &mut self.overlay {
                let _ = mention.picker.update(SkillPickerEvent::Query(query));
            }
            return update;
        }

        if !is_picker_navigation(&event) {
            self.overlay = None;
            if is_escape(&event) {
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            let mut update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            update.render = update.render.max(RenderRequest::Immediate);
            return update;
        }

        let Some(Overlay::Skills(mention)) = &mut self.overlay else {
            unreachable!("skill picker was checked above");
        };
        let update = mention.picker.update(SkillPickerEvent::Terminal(event));
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            SkillPickerEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            SkillPickerEffect::Insert(name) => self.update_composer(
                ComposerEvent::ReplaceRange {
                    range: start..self.composer.component().cursor(),
                    text: format!("${name} "),
                },
                RenderRequest::Immediate,
            ),
        }
    }

    fn mention_query(&self, start: usize, prefix: char) -> Option<String> {
        let composer = self.composer.component();
        composer
            .draft()
            .get(start..composer.cursor())?
            .strip_prefix(prefix)
            .map(str::to_owned)
    }

    fn action_availability(&self) -> ActionAvailability {
        ActionAvailability {
            new_session: !self.has_active_turns()
                && self.in_flight_shells == 0
                && self.blocking_task.is_none()
                && self.queue.component().is_empty(),
            fork: self.can_fork(),
            fast_mode: self.composer.component().fast_mode(),
            model: self.thread == ThreadState::New,
        }
    }

    fn refresh_actions(&mut self) {
        let availability = self.action_availability();
        if let Some(Overlay::Actions(actions)) = &mut self.overlay {
            actions.component_mut().set_availability(availability);
        }
    }

    fn submit_action_command(&mut self, command: String) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        self.composer.component_mut().replace_draft(command);
        self.update_composer(
            ComposerEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            RenderRequest::Immediate,
        )
    }

    fn update_actions(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        self.refresh_actions();
        let Some(Overlay::Actions(actions)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = actions.update(ActionsEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(ActionsEffect::Dismiss) => self.overlay = None,
            Some(ActionsEffect::Submit(command)) => return self.submit_action_command(command),
            Some(ActionsEffect::Trigger(Action::Goal)) => {
                return self.submit_action_command("/goal".to_owned());
            }
            Some(ActionsEffect::Trigger(Action::Bug)) => {
                self.overlay = None;
                return self.apply_settings_command(SettingsCommand::Bug(String::new()));
            }
            Some(ActionsEffect::Trigger(Action::Reload)) => {
                self.overlay = None;
                return self.apply_settings_command(SettingsCommand::Reload);
            }
            Some(ActionsEffect::Trigger(Action::Screen)) => {
                self.overlay = None;
                return self.apply_settings_command(SettingsCommand::Screen);
            }
            Some(ActionsEffect::Trigger(Action::Zoom)) => {
                self.overlay = None;
                return self.apply_settings_command(SettingsCommand::Zoom);
            }
            Some(ActionsEffect::Trigger(Action::Voice)) => {
                self.overlay = None;
                return self
                    .apply_settings_command(SettingsCommand::Voice(crate::voice::Command::Toggle));
            }
            Some(ActionsEffect::Trigger(Action::AgentId)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::ShowAgentId],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Settings(command)) => {
                self.overlay = None;
                return self.apply_settings_command(command);
            }
            Some(ActionsEffect::Trigger(Action::Effort)) => {
                return self.open_effort();
            }
            Some(ActionsEffect::Trigger(Action::Model)) => {
                return self.open_model();
            }
            Some(ActionsEffect::Trigger(Action::FastMode)) => {
                self.overlay = None;
                let enabled = !self.composer.component().fast_mode();
                self.set_fast_mode(enabled);
                return ComponentUpdate {
                    effects: vec![RootEffect::SetFastMode(enabled)],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::Theme)) => {
                self.overlay = Some(Overlay::Theme(Node::new(ThemeSelector::new(
                    self.theme_mode,
                ))));
            }
            Some(ActionsEffect::Trigger(Action::NewSession)) => {
                return self.open_new_session();
            }
            Some(ActionsEffect::Trigger(Action::ResumeSession)) => {
                return self.load_sessions();
            }
            Some(ActionsEffect::Trigger(Action::Fork)) => return self.open_fork(),
            Some(ActionsEffect::Trigger(Action::Keybindings)) => {
                self.overlay = Some(Overlay::Keybindings(Node::new(KeybindingsHelp::default())));
            }
            Some(ActionsEffect::Trigger(Action::ReloadConfig)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::ReloadConfig],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::EditConfig)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::OpenConfigEditor],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::DebugContext)) => {
                self.overlay = Some(Overlay::ContextDiagnostics(Node::new(
                    ContextDiagnosticsPanel::new(self.context_diagnostics.clone()),
                )));
            }
            Some(ActionsEffect::Trigger(Action::Reflection)) => {
                self.overlay = None;
                self.reflection_input = true;
                return self.update_composer(
                    ComposerEvent::InputMode(Some(
                        "Reflection instructions · enter start · esc cancel".to_owned(),
                    )),
                    RenderRequest::Immediate,
                );
            }
            Some(ActionsEffect::Trigger(Action::Review)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::Review {
                        download_assets: false,
                    }],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::Handoff)) => {
                self.overlay = None;
                self.blocking_task = Some(BlockingTask::Handoff);
                let waiting = self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: true,
                        status: Some("Preparing handoff…".to_owned()),
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                );
                return ComponentUpdate {
                    effects: vec![RootEffect::Handoff],
                    render: waiting.render.max(RenderRequest::Immediate),
                };
            }
            None => {}
        }
        ComponentUpdate {
            effects: Vec::new(),
            render: update.render,
        }
    }

    fn update_context_diagnostics(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = panel.update(ContextDiagnosticsEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(ContextDiagnosticsEffect::Dismiss) => self.overlay = None,
            Some(ContextDiagnosticsEffect::Refresh) => {
                if let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay {
                    panel
                        .component_mut()
                        .replace(self.context_diagnostics.clone());
                }
            }
            None => {}
        }
        ComponentUpdate {
            effects: Vec::new(),
            render: update.render,
        }
    }

    fn update_review_confirmation(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::ReviewDownload(confirmation)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = confirmation.update(ReviewConfirmationEvent::Terminal(event));
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };
        self.overlay = None;
        match effect {
            ReviewConfirmationEffect::Confirm => ComponentUpdate {
                effects: vec![RootEffect::Review {
                    download_assets: true,
                }],
                render: RenderRequest::Immediate,
            },
            ReviewConfirmationEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
        }
    }

    fn open_effort(&mut self) -> ComponentUpdate<RootEffect> {
        self.overlay = Some(Overlay::Effort(Node::new(EffortSelector::new(
            self.composer.component().effort(),
            self.preferred_reasoning_mode == ReasoningMode::Pro,
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn open_model(&mut self) -> ComponentUpdate<RootEffect> {
        if self.thread != ThreadState::New {
            self.notification = Some(Notification::plain(
                "The model can only be changed before the first prompt".to_owned(),
                Color::Red,
            ));
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.overlay = Some(Overlay::Model(Node::new(ModelSelector::new(
            self.composer.component().model(),
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_theme_selector(&mut self, event: ThemeSelectorEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Theme(selector)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = selector.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };
        self.overlay = None;
        match effect {
            ThemeSelectorEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            ThemeSelectorEffect::Apply(mode) => ComponentUpdate {
                effects: vec![RootEffect::SetTheme(mode)],
                render: RenderRequest::Immediate,
            },
        }
    }

    fn open_fork(&mut self) -> ComponentUpdate<RootEffect> {
        if !self.can_fork() {
            return ComponentUpdate::none();
        }
        self.overlay = None;
        ComponentUpdate {
            effects: vec![RootEffect::Fork],
            render: RenderRequest::Immediate,
        }
    }

    fn can_fork(&self) -> bool {
        self.fork_available
    }

    fn open_new_session(&mut self) -> ComponentUpdate<RootEffect> {
        if self.has_active_turns()
            || self.in_flight_shells > 0
            || !self.queue.component().is_empty()
        {
            return ComponentUpdate::none();
        }
        self.overlay = None;
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Starting new session…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::NewSession(self.composer.component().model())],
            render: RenderRequest::Immediate,
        }
    }

    pub(super) fn load_sessions(&mut self) -> ComponentUpdate<RootEffect> {
        self.pending_session_mention = None;
        self.start_session_list(SessionListKind::Resume)
    }

    fn load_session_mentions(&mut self, start: usize) -> ComponentUpdate<RootEffect> {
        self.pending_session_mention = Some(start);
        self.start_session_list(SessionListKind::Mention)
    }

    fn start_session_list(&mut self, kind: SessionListKind) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        let request_id = self.next_session_list;
        self.next_session_list = self.next_session_list.wrapping_add(1);
        let previous = self.pending_session_list.replace(request_id);
        let _ = self.session_lookup_status();
        ComponentUpdate {
            effects: previous
                .into_iter()
                .map(RootEffect::CancelSessionList)
                .chain([RootEffect::LoadSessions { request_id, kind }])
                .collect(),
            render: RenderRequest::Immediate,
        }
    }

    fn session_lookup_status(&mut self) -> RenderRequest {
        self.session_loading_status("Loading sessions… · Esc cancel")
    }

    fn session_resume_status(&mut self) -> RenderRequest {
        self.session_loading_status("Resuming session… · Esc cancel")
    }

    fn session_loading_status(&mut self, status: &str) -> RenderRequest {
        self.interactive = false;
        let update = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some(status.to_owned()),
                now: Instant::now(),
            });
        if update.changed {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        }
    }

    fn cancel_session_list(&mut self) -> ComponentUpdate<RootEffect> {
        let Some(request_id) = self.pending_session_list.take() else {
            return ComponentUpdate::none();
        };
        self.pending_session_mention = None;
        self.key_confirmation = None;
        let mut update = self.resume_after_session_lookup();
        update
            .effects
            .push(RootEffect::CancelSessionList(request_id));
        update
    }

    fn resume_after_session_lookup(&mut self) -> ComponentUpdate<RootEffect> {
        let mut update = self.restore_session_activity();
        // A turn can finish while lookup input is paused. Releasing that pause
        // must also release ready follow-ups, without consuming the draft.
        update.effects.extend(self.submit_next_queued().effects);
        update
    }

    fn load_recent_prompts(&mut self) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Loading recent prompts…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::LoadRecentPrompts(self.recent_prompts.clone())],
            render: RenderRequest::Immediate,
        }
    }

    fn recent_prompts_loaded(
        &mut self,
        session_id: String,
        prompts: Vec<RecentPrompt>,
    ) -> ComponentUpdate<RootEffect> {
        self.restore_session_activity();
        self.overlay = Some(Overlay::RecentPrompts(Node::new(RecentPromptPicker::new(
            prompts, session_id,
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_recent_prompt_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::RecentPrompts(picker)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = picker.update(RecentPromptPickerEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(RecentPromptPickerEffect::Dismiss) => {
                self.overlay = None;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            Some(RecentPromptPickerEffect::Insert(prompt)) => {
                self.overlay = None;
                self.update_composer(
                    ComposerEvent::ReplaceDraft(prompt),
                    RenderRequest::Immediate,
                )
            }
            None => ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            },
        }
    }

    fn recent_prompt_load_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.notification = Some(Notification::plain(message, Color::Red));
        self.restore_session_activity()
    }

    fn sessions_loaded(
        &mut self,
        request_id: u64,
        sessions: Vec<SessionSummary>,
    ) -> ComponentUpdate<RootEffect> {
        if self.pending_session_list != Some(request_id) {
            return ComponentUpdate::none();
        }
        self.pending_session_list = None;
        let update = self.resume_after_session_lookup();
        let mode = if self.pending_session_mention.is_some() {
            SessionPickerMode::Mention
        } else {
            SessionPickerMode::Resume
        };
        self.overlay = Some(Overlay::Sessions(Node::new(SessionPicker::new_with_id(
            sessions, mode, request_id,
        ))));
        update
    }

    fn update_session_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Sessions(picker)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = picker.update(SessionPickerEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(SessionPickerEffect::Search { request_id, query }) => ComponentUpdate {
                effects: vec![RootEffect::SearchSessions {
                    picker_id: picker.component().id(),
                    request_id,
                    query,
                }],
                render: update.render,
            },
            Some(SessionPickerEffect::Dismiss) => {
                self.overlay = None;
                self.pending_session_mention = None;
                ComponentUpdate {
                    effects: vec![RootEffect::CancelSessionSearch],
                    render: RenderRequest::Immediate,
                }
            }
            Some(SessionPickerEffect::Resume(session_id)) => {
                self.overlay = None;
                self.resuming_session = true;
                self.session_resume_status();
                ComponentUpdate {
                    effects: vec![RootEffect::ResumeSession(session_id)],
                    render: RenderRequest::Immediate,
                }
            }
            Some(SessionPickerEffect::Mention(session_id)) => {
                self.overlay = None;
                let Some(start) = self.pending_session_mention.take() else {
                    return ComponentUpdate::none();
                };
                let cursor = self.composer.component().cursor();
                if !self
                    .composer
                    .component()
                    .draft()
                    .get(start..cursor)
                    .is_some_and(|query| query.starts_with("@@"))
                {
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                self.update_composer(
                    ComposerEvent::ReplaceRange {
                        range: start..cursor,
                        text: format!("@@{session_id} "),
                    },
                    RenderRequest::Immediate,
                )
            }
            None => ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            },
        }
    }

    fn session_load_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.resuming_session = false;
        self.pending_session_mention = None;
        self.restore_session_activity();
        self.notification = Some(Notification::plain(message, Color::Red));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn new_session_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.restore_session_activity();
        self.notification = Some(Notification::plain(
            format!("Could not start a new session: {message}"),
            Color::Red,
        ));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn fork_ready(&mut self) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let update = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        debug_assert!(update.changed);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_vault_review(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let approve = matches!(&event, Event::Key(key) if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::CONTROL && key.kind == crossterm::event::KeyEventKind::Press);
        if !approve && !is_escape(&event) {
            return ComponentUpdate::none();
        }
        if approve && !matches!(&self.overlay, Some(Overlay::VaultReview(review)) if review.visible)
        {
            return ComponentUpdate::none();
        }
        let Some(Overlay::VaultReview(review)) = self.overlay.take() else {
            return ComponentUpdate::none();
        };
        ComponentUpdate {
            effects: if approve {
                vec![RootEffect::ApproveVault(review)]
            } else {
                Vec::new()
            },
            render: RenderRequest::Immediate,
        }
    }

    fn update_agent_id(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::AgentId(id)) = &self.overlay else {
            return ComponentUpdate::none();
        };
        let effects = if is_submit_enter(&event) {
            vec![RootEffect::Copy(id.clone())]
        } else if is_escape(&event) {
            Vec::new()
        } else {
            return ComponentUpdate::none();
        };
        self.overlay = None;
        ComponentUpdate {
            effects,
            render: RenderRequest::Immediate,
        }
    }

    fn update_keybindings(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Keybindings(help)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = help.update(KeybindingsEvent::Terminal(event));
        if matches!(update.effects.as_slice(), [KeybindingsEffect::Dismiss]) {
            self.overlay = None;
        }
        ComponentUpdate::render(update.render)
    }

    fn update_effort(&mut self, event: EffortEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Effort(selector)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = selector.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            EffortEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            EffortEffect::Apply(effort, pro) => self.apply_effort(effort, pro),
        }
    }

    fn apply_effort(&mut self, effort: ReasoningEffort, pro: bool) -> ComponentUpdate<RootEffect> {
        let reasoning_mode = if pro {
            ReasoningMode::Pro
        } else {
            ReasoningMode::Standard
        };
        let previous_reasoning_mode = self.preferred_reasoning_mode;
        self.preferred_reasoning_mode = reasoning_mode;
        if reasoning_mode != previous_reasoning_mode {
            let state = if pro { "enabled" } else { "disabled" };
            let suffix = if self.composer.component().reasoning_mode() != reasoning_mode {
                " · start a new session to apply."
            } else {
                "."
            };
            let message = format!("Pro {state} for new sessions{suffix}");
            self.notification = Some(Notification::plain(message, Color::Green));
        }
        self.transcript.component_mut().set_effort(effort);
        self.subagents.set_effort(effort);
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetEffort(effort));
        ComponentUpdate {
            effects: vec![RootEffect::SetEffort {
                effort,
                reasoning_mode,
            }],
            render: RenderRequest::Immediate,
        }
    }

    fn update_model(&mut self, event: ModelSelectorEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Model(selector)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = selector.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        if self.thread != ThreadState::New {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        match effect {
            ModelSelectorEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            ModelSelectorEffect::Apply(model) => self.apply_model(model),
        }
    }

    fn apply_model(&mut self, model: Model) -> ComponentUpdate<RootEffect> {
        if self.thread != ThreadState::New {
            self.notification = Some(Notification::plain(
                "The model can only be changed before the first prompt".to_owned(),
                Color::Red,
            ));
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if model == self.composer.component().model() {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some(format!("Starting {} session…", model_name(model))),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::SetModel(model)],
            render: RenderRequest::Immediate,
        }
    }

    fn update_focus(&mut self) -> ComponentUpdate<RootEffect> {
        let focus_queue = !self.queue.component().focused() && !self.queue.component().is_empty();
        self.queue.component_mut().set_focused(focus_queue);
        let transcript = self.transcript.update(TranscriptEvent::BlurExpandables);
        ComponentUpdate::render(if focus_queue || transcript.render != RenderRequest::None {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn focus_composer(&mut self) {
        self.queue.component_mut().set_focused(false);
        let _ = self
            .transcript
            .component_mut()
            .update(TranscriptEvent::BlurExpandables);
    }

    fn update_queue(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let update = self.queue.update(QueueEvent::Terminal(event));
        let mut effects = Vec::new();
        let mut render = update.render;
        for effect in update.effects {
            match effect {
                QueueEffect::Blur => {}
                QueueEffect::Edit { id, prompt } => {
                    let edit = self.begin_queue_edit(id, prompt);
                    effects.extend(edit.effects);
                    render = render.max(edit.render);
                }
                QueueEffect::Steer { id, prompt } => {
                    effects.push(RootEffect::Steer { id, prompt });
                }
            }
        }
        ComponentUpdate { effects, render }
    }

    fn begin_queue_edit(&mut self, id: QueueId, prompt: Submission) -> ComponentUpdate<RootEffect> {
        let original_input_mode = self
            .composer
            .component()
            .input_mode()
            .map(ToOwned::to_owned);
        let original_draft = self.composer.component_mut().take_draft();
        self.composer.component_mut().restore_draft(prompt.into());
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::InputMode(Some(
                "editing queued message · enter save · esc cancel".to_owned(),
            )));
        self.queue_edit = Some(QueueEdit {
            id,
            original_draft,
            original_input_mode,
        });
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_queue_editor(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if is_escape(&event) {
            return self.finish_queue_edit(false);
        }
        if is_submit_enter(&event) {
            return self.finish_queue_edit(true);
        }
        self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate)
    }

    fn finish_queue_edit(&mut self, save: bool) -> ComponentUpdate<RootEffect> {
        let Some(edit) = self.queue_edit.take() else {
            return ComponentUpdate::none();
        };
        if save {
            // A saved revision is a new instruction. A late receipt for the
            // original request must not remove it from the queue.
            self.unconfirmed_prompts.retain(|_, id| *id != edit.id);
        }
        let prompt = save.then(|| {
            self.composer
                .component_mut()
                .take_draft()
                .map(ComposerDraft::into_submission)
                .unwrap_or_else(|| Submission::text(String::new()))
        });
        self.composer.component_mut().replace_draft(String::new());
        if let Some(draft) = edit.original_draft {
            self.composer.component_mut().restore_draft(draft);
        }
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::InputMode(edit.original_input_mode));

        let confirmed = self.confirmed_queue_edit.take() == Some(edit.id);
        let restored = if confirmed && !save {
            self.queue.component_mut().steer_admitted(edit.id).is_some()
        } else {
            match prompt {
                Some(prompt) => self.queue.component_mut().finish_edit(edit.id, prompt),
                None => self.queue.component_mut().cancel_edit(edit.id),
            }
        };
        if !restored {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.submit_next_queued()
    }

    fn edit_composer(&mut self, event: ComposerEvent) -> ComponentUpdate<RootEffect> {
        // Offline editing and image paste act on the draft rather than a picker.
        // Drop any saved mention position before that edit changes its bounds.
        let mut update = self.cancel_session_list();
        self.overlay = None;
        self.pending_session_mention = None;
        let composer = if matches!(&event, ComposerEvent::Terminal(event) if is_control_key(event, 'z'))
        {
            self.restore_discarded_draft()
        } else {
            self.update_composer(event, RenderRequest::Immediate)
        };
        update.effects.extend(composer.effects);
        update.render = update.render.max(composer.render);
        update
    }

    fn update_composer(
        &mut self,
        event: ComposerEvent,
        priority: RenderRequest,
    ) -> ComponentUpdate<RootEffect> {
        let update = self.composer.component_mut().update(event);
        if let Some(ComposerEffect::Settings(command)) = &update.effect {
            return self.apply_settings_command(command.clone());
        }
        let delivered = matches!(
            &update.effect,
            Some(ComposerEffect::Submit(_) | ComposerEffect::Queue(_))
        );
        if delivered {
            self.thread = ThreadState::Started;
        }
        let mut render = if update.changed {
            priority
        } else {
            RenderRequest::None
        };
        if delivered {
            render = render.max(self.update_transcript(TranscriptEvent::FollowTail).render);
        }
        let effects = match update.effect {
            Some(ComposerEffect::Vault(command)) => {
                let command = if command == crate::tui::vault::Command::Latest {
                    self.transcript
                        .component()
                        .latest_vault_command()
                        .unwrap_or(crate::tui::vault::Command::Help)
                } else {
                    command
                };
                vec![RootEffect::Vault(command)]
            }
            Some(ComposerEffect::ShowAgentId) => vec![RootEffect::ShowAgentId],
            // Goal controls are intercepted by the managed server and must not
            // wait behind active model work or an unacknowledged steer.
            Some(ComposerEffect::Submit(prompt))
                if prompt.display_text().split_whitespace().next() == Some("/goal") =>
            {
                self.in_flight_turns = self.in_flight_turns.saturating_add(1);
                vec![RootEffect::Submit(prompt)]
            }
            Some(ComposerEffect::Submit(prompt)) if self.has_active_turns() => {
                let (id, prompt) = self.queue.component_mut().begin_steer(prompt);
                vec![RootEffect::Steer { id, prompt }]
            }
            Some(ComposerEffect::Submit(prompt)) if self.queue.component().has_pending_steer() => {
                self.queue.component_mut().push(prompt);
                Vec::new()
            }
            Some(ComposerEffect::Submit(prompt)) => {
                self.in_flight_turns = self.in_flight_turns.saturating_add(1);
                vec![RootEffect::Submit(prompt)]
            }
            Some(ComposerEffect::Queue(prompt)) => {
                self.queue.component_mut().push(prompt);
                let queued = self.submit_next_queued();
                render = render.max(queued.render);
                queued.effects
            }
            Some(ComposerEffect::RunShell(command)) => {
                self.in_flight_shells = self.in_flight_shells.saturating_add(1);
                vec![RootEffect::RunShell(command)]
            }
            Some(ComposerEffect::OpenDraftEditor) => vec![RootEffect::OpenDraftEditor],
            Some(ComposerEffect::Settings(_)) => {
                unreachable!("settings commands return before prompt delivery")
            }
            None => Vec::new(),
        };

        if delivered && self.has_active_turns() {
            let activity = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active: true,
                    status: Some(
                        self.transcript
                            .component()
                            .activity()
                            .status
                            .unwrap_or_else(|| "Thinking…".to_owned()),
                    ),
                    now: Instant::now(),
                });
            if activity.changed {
                render = render.max(RenderRequest::Immediate);
            }
        }
        let controls = self.sync_live_controls();
        render = render.max(controls);

        ComponentUpdate { effects, render }
    }

    fn apply_settings_command(&mut self, command: SettingsCommand) -> ComponentUpdate<RootEffect> {
        match command {
            SettingsCommand::Bug(description) => ComponentUpdate {
                effects: vec![RootEffect::Bug(description)],
                render: RenderRequest::Immediate,
            },
            SettingsCommand::Attach => {
                if !self.action_availability().new_session {
                    self.notification = Some(Notification::plain(
                        "Finish the current work before attaching to another thread".into(),
                        Color::Red,
                    ));
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                self.load_sessions()
            }
            SettingsCommand::Screen | SettingsCommand::Zoom => ComponentUpdate {
                effects: vec![if command == SettingsCommand::Screen {
                    RootEffect::Screen
                } else {
                    RootEffect::Zoom
                }],
                render: RenderRequest::Immediate,
            },
            SettingsCommand::Reload => ComponentUpdate {
                effects: vec![RootEffect::Reload],
                render: RenderRequest::Immediate,
            },
            SettingsCommand::Voice(command) => ComponentUpdate {
                effects: vec![RootEffect::Voice(command)],
                render: RenderRequest::Immediate,
            },
            SettingsCommand::OpenEffort => self.open_effort(),
            SettingsCommand::SetEffort(effort) => {
                self.apply_effort(effort, self.preferred_reasoning_mode == ReasoningMode::Pro)
            }
            SettingsCommand::OpenModel => self.open_model(),
            SettingsCommand::SetModel(model) => self.apply_model(model),
            SettingsCommand::Invalid(message) => {
                self.notification = Some(Notification::plain(message, Color::Red));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
        }
    }

    fn submit_reflection(&mut self) -> ComponentUpdate<RootEffect> {
        let instructions = self
            .composer
            .component_mut()
            .take_submission()
            .unwrap_or_else(|| Submission::text(String::new()));
        self.reflection_input = false;
        let mode = self.update_composer(ComposerEvent::InputMode(None), RenderRequest::Immediate);
        self.thread = ThreadState::Started;
        self.in_flight_turns = self.in_flight_turns.saturating_add(1);
        let transcript = self.update_transcript(TranscriptEvent::FollowTail);
        ComponentUpdate {
            effects: vec![RootEffect::Reflect(instructions)],
            render: mode.render.max(transcript.render),
        }
    }

    fn cancel_reflection(&mut self) -> ComponentUpdate<RootEffect> {
        self.reflection_input = false;
        self.composer.component_mut().replace_draft(String::new());
        self.update_composer(ComposerEvent::InputMode(None), RenderRequest::Immediate)
    }

    fn discard_draft(&mut self) -> ComponentUpdate<RootEffect> {
        let Some(draft) = self.composer.component_mut().take_draft() else {
            return ComponentUpdate::none();
        };
        self.discarded_draft = Some(draft);
        self.show_draft_saved();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn restore_discarded_draft(&mut self) -> ComponentUpdate<RootEffect> {
        if !self.composer.component().draft().is_empty() || self.composer.component().has_images() {
            return ComponentUpdate::none();
        }
        let Some(draft) = self
            .withdrawn_draft
            .take()
            .or_else(|| self.discarded_draft.take())
        else {
            return ComponentUpdate::none();
        };
        self.composer.component_mut().restore_draft(draft);
        self.notification = Some(Notification::plain(
            "Draft restored.".to_owned(),
            Color::Green,
        ));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn show_draft_saved(&mut self) {
        self.notification = Some(Notification::plain(
            "Draft cleared · Ctrl+Z to restore".to_owned(),
            Color::Yellow,
        ));
    }

    fn turn_finished(&mut self) -> ComponentUpdate<RootEffect> {
        self.in_flight_turns = self.in_flight_turns.saturating_sub(1);
        let mut update = self.submit_next_queued();
        if !self.has_active_turns() {
            let activity = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active: false,
                    status: None,
                    now: Instant::now(),
                });
            if activity.changed {
                update.render = update.render.max(RenderRequest::Immediate);
            }
            let timers = self
                .composer
                .component_mut()
                .update(ComposerEvent::TurnsCleared);
            if timers.changed {
                update.render = update.render.max(RenderRequest::Immediate);
            }
        }
        update.render = update.render.max(self.sync_live_controls());
        update
    }

    fn worker_turn_finished(&mut self, terminal_expected: bool) -> ComponentUpdate<RootEffect> {
        if !terminal_expected {
            return self.turn_finished();
        }
        if self.unmatched_agent_turns > 0 {
            self.unmatched_agent_turns -= 1;
            return self.turn_finished();
        }
        self.unmatched_worker_turns = self.unmatched_worker_turns.saturating_add(1);
        ComponentUpdate::none()
    }

    fn agent_turn_finished(&mut self) -> ComponentUpdate<RootEffect> {
        if self.unmatched_worker_turns > 0 {
            self.unmatched_worker_turns -= 1;
            return self.turn_finished();
        }
        self.unmatched_agent_turns = self.unmatched_agent_turns.saturating_add(1);
        ComponentUpdate::none()
    }

    fn turns_cancelled(&mut self) -> ComponentUpdate<RootEffect> {
        // Cancellation admission is not a terminal event or a steering acknowledgement.
        // Keep applied-before-ack evidence until both have resolved.
        self.submit_next_queued()
    }

    fn managed_active_turns(&mut self, count: usize) -> ComponentUpdate<RootEffect> {
        self.managed_active_turns = count;
        let mut update = if self.has_active_turns() {
            ComponentUpdate::render(RenderRequest::Immediate)
        } else {
            self.submit_next_queued()
        };
        let active = self.has_active_turns();
        let activity = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active,
                status: active.then(|| {
                    self.transcript
                        .component()
                        .activity()
                        .status
                        .unwrap_or_else(|| "Thinking…".to_owned())
                }),
                now: Instant::now(),
            });
        if activity.changed {
            update.render = update.render.max(RenderRequest::Immediate);
        }
        if !active {
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::TurnsCleared);
        }
        update.render = update.render.max(self.sync_live_controls());
        update
    }

    fn restore_session_activity(&mut self) -> ComponentUpdate<RootEffect> {
        if self.resuming_session {
            self.session_resume_status();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if let Some(connecting) = self.reconnecting {
            return self.reconnection_status(if connecting {
                "Reconnecting…"
            } else {
                "Connection lost · Enter to reconnect"
            });
        }
        if self.pending_session_list.is_some() {
            let _ = self.session_lookup_status();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.interactive = true;
        let active = self.has_active_turns();
        let status = active.then(|| {
            self.transcript
                .component()
                .activity()
                .status
                .unwrap_or_else(|| "Thinking…".to_owned())
        });
        let mut update = self.update_composer(
            ComposerEvent::Activity {
                active,
                status,
                now: Instant::now(),
            },
            RenderRequest::Immediate,
        );
        update.render = RenderRequest::Immediate;
        update
    }

    fn reconnection_status(&mut self, status: &str) -> ComponentUpdate<RootEffect> {
        self.composer
            .component_mut()
            .update(ComposerEvent::SubmissionPaused(true));
        self.update_composer(
            ComposerEvent::Activity {
                active: true,
                status: Some(status.to_owned()),
                now: Instant::now(),
            },
            RenderRequest::Immediate,
        )
    }

    fn agent_stream_closed(&mut self) -> ComponentUpdate<RootEffect> {
        self.managed_active_turns = 0;
        self.interactive = false;
        self.reconnecting = Some(true);
        self.key_confirmation = None;
        self.withdrawing_steer = None;
        self.withdrawing_prompt = None;
        self.queue.component_mut().connection_lost();
        let mut update = self.update_transcript(TranscriptEvent::AgentStreamClosed);
        let status = self.reconnection_status("Reconnecting…");
        update.effects.extend(status.effects);
        update.render = RenderRequest::Immediate;
        update
    }

    fn agent_reconnected(
        &mut self,
        active_turns: usize,
        pending_local: bool,
        reasoning_mode: ReasoningMode,
    ) -> ComponentUpdate<RootEffect> {
        self.set_reasoning_modes(reasoning_mode, reasoning_mode);
        self.reconnecting = None;
        self.interactive = self.pending_session_list.is_none() && !self.resuming_session;
        self.composer
            .component_mut()
            .update(ComposerEvent::SubmissionPaused(false));
        self.in_flight_turns = usize::from(pending_local);
        self.unmatched_worker_turns = 0;
        self.unmatched_agent_turns = 0;
        self.notification = Some(Notification::plain("Reconnected".to_owned(), Color::Green));
        self.managed_active_turns(active_turns)
    }

    fn retain_prompt(
        &mut self,
        request_id: String,
        prompt: Submission,
    ) -> ComponentUpdate<RootEffect> {
        if !self.unconfirmed_prompts.contains_key(&request_id) {
            let (id, _) = self.queue.component_mut().begin_steer(prompt);
            self.queue.component_mut().steer_unconfirmed(id);
            self.unconfirmed_prompts.insert(request_id, id);
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn confirm_prompt(&mut self, request_id: &str) -> ComponentUpdate<RootEffect> {
        let Some(id) = self.unconfirmed_prompts.remove(request_id) else {
            return ComponentUpdate::none();
        };
        if self.queue_edit.as_ref().is_some_and(|edit| edit.id == id) {
            self.confirmed_queue_edit = Some(id);
            self.notification = Some(Notification::plain(
                "Original prompt delivered. Save to send your edit, or Esc to dismiss it."
                    .to_owned(),
                Color::Green,
            ));
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        // Its original submission is already in the transcript. A durable receipt
        // confirms this exact request ID without creating a second user message.
        self.queue.component_mut().steer_admitted(id);
        self.submit_next_queued()
    }

    fn undo_latest_message(&mut self) -> ComponentUpdate<RootEffect> {
        if self.withdrawing_steer.is_some() {
            return ComponentUpdate::none();
        }
        if self.withdrawn_draft.is_some() {
            self.notification = Some(Notification::plain(
                "Restore the withdrawn draft with Ctrl+Z first.".to_owned(),
                Color::Yellow,
            ));
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let queued = self.queue.component().latest();
        let admitted = self
            .last_admitted_steer
            .as_ref()
            .map(|(id, _)| (*id, false));
        let Some((id, local)) = queued.into_iter().chain(admitted).max_by_key(|(id, _)| *id) else {
            self.notification = Some(Notification::plain(
                "No queued message to undo.".to_owned(),
                Color::Yellow,
            ));
            return ComponentUpdate::render(RenderRequest::Immediate);
        };
        self.withdrawing_prompt = self.queue.component().prompt(id).or_else(|| {
            self.last_admitted_steer
                .as_ref()
                .filter(|(candidate, _)| *candidate == id)
                .map(|(_, prompt)| prompt.clone())
        });
        self.withdrawing_steer = Some(id);
        self.withdrawing_remote = !local;
        if local {
            return self.steer_withdrawn(id);
        }
        self.notification = Some(Notification::plain(
            "Withdrawing message…".to_owned(),
            Color::Yellow,
        ));
        ComponentUpdate {
            effects: vec![RootEffect::WithdrawSteer { id }],
            render: RenderRequest::Immediate,
        }
    }

    fn steer_withdrawn(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        if self.withdrawing_steer != Some(id) {
            return ComponentUpdate::none();
        }
        self.withdrawing_steer = None;
        let prompt = self.queue.component_mut().withdraw(id).or_else(|| {
            if self
                .last_admitted_steer
                .as_ref()
                .is_some_and(|(candidate, _)| *candidate == id)
            {
                self.last_admitted_steer.take().map(|(_, prompt)| prompt)
            } else {
                None
            }
        });
        let Some(prompt) = prompt.or_else(|| self.withdrawing_prompt.take()) else {
            return ComponentUpdate::none();
        };
        self.withdrawing_prompt = None;
        let effects = if self.withdrawing_remote {
            vec![RootEffect::PersistSteerWithdrawal {
                text: prompt.display_text().to_owned(),
            }]
        } else {
            Vec::new()
        };
        self.withdrawing_remote = false;
        self.queue.component_mut().set_focused(false);
        let message = if self.composer.component().draft().is_empty()
            && !self.composer.component().has_images()
            && self.queue_edit.is_none()
            && !self.reflection_input
            && self.overlay.is_none()
        {
            self.composer.component_mut().restore_draft(prompt.into());
            "Message withdrawn · edit and send again."
        } else {
            // Never overwrite text typed while the server was deciding withdrawal.
            self.withdrawn_draft = Some(prompt.into());
            "Message withdrawn · clear the composer, then Ctrl+Z to restore."
        };
        self.notification = Some(Notification::plain(message.to_owned(), Color::Green));
        ComponentUpdate {
            effects,
            render: RenderRequest::Immediate,
        }
    }

    fn steer_admitted(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        let accepted = self.queue.component_mut().steer_admitted(id);
        if let Some(accepted) = &accepted {
            self.last_admitted_steer = Some(accepted.clone());
        }
        self.finish_accepted_steer(accepted)
    }

    fn steer_unconfirmed(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        self.queue.component_mut().steer_unconfirmed(id);
        self.submit_next_queued()
    }

    fn steer_failed(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        self.queue.component_mut().steer_failed(id);
        self.submit_next_queued()
    }

    fn finish_accepted_steer(
        &mut self,
        accepted: Option<(QueueId, Submission)>,
    ) -> ComponentUpdate<RootEffect> {
        let mut update = self.submit_next_queued();
        if let Some((id, prompt)) = accepted {
            update.effects.insert(
                0,
                RootEffect::PersistSteer {
                    id,
                    text: prompt.display_text().to_owned(),
                },
            );
        }
        update
    }

    fn submit_next_queued(&mut self) -> ComponentUpdate<RootEffect> {
        if !self.interactive || self.has_active_turns() {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if self.withdrawing_steer.is_some() || self.queue.component().has_pending_steer() {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let prompts = self.queue.component_mut().drain_ready();
        if prompts.is_empty() {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.in_flight_turns = 1;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Thinking…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::Submit(Submission::join(prompts))],
            render: RenderRequest::Immediate,
        }
    }

    fn sync_live_controls(&mut self) -> RenderRequest {
        let active = self.has_active_turns();
        let update = self
            .composer
            .component_mut()
            .update(ComposerEvent::LiveControls(active));
        let render = if update.changed {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        };
        if self.resuming_session {
            render.max(self.session_resume_status())
        } else if self.pending_session_list.is_some() && self.reconnecting.is_none() {
            render.max(self.session_lookup_status())
        } else {
            render
        }
    }

    const fn has_active_turns(&self) -> bool {
        self.in_flight_turns > 0 || self.managed_active_turns > 0
    }

    fn update_transcript(&mut self, event: TranscriptEvent) -> ComponentUpdate<RootEffect> {
        let update = self.transcript.update(event);
        let mut render = update.render;
        for effect in update.effects {
            // Background activity must not replace the foreground status while
            // reconnection or a session lookup owns the input controls.
            if self.reconnecting.is_some()
                || self.pending_session_list.is_some()
                || self.resuming_session
            {
                continue;
            }
            // Local submission is rendered before the server starts the run.
            // An idle transcript must not clear that pending turn's activity.
            let active = effect.active || self.has_active_turns();
            let status = effect
                .status
                .or_else(|| active.then(|| "Thinking…".to_owned()));
            let composer = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active,
                    status,
                    now: Instant::now(),
                });
            if composer.changed {
                render = render.max(RenderRequest::Streaming);
            }
        }
        ComponentUpdate {
            effects: Vec::new(),
            render,
        }
    }

    fn update_animation(&mut self, now: Instant) -> ComponentUpdate<RootEffect> {
        let confirmation = if self
            .key_confirmation
            .as_ref()
            .is_some_and(|confirmation| now >= confirmation.deadline)
        {
            self.key_confirmation = None;
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        };
        let effort = self.update_effort(EffortEvent::AnimationFrame(now));
        let model = self.update_model(ModelSelectorEvent::AnimationFrame(now));
        let transcript = self.update_transcript(TranscriptEvent::AnimationFrame(now));
        let composer =
            self.update_composer(ComposerEvent::AnimationFrame(now), RenderRequest::Streaming);
        let queue = self.queue.update(QueueEvent::AnimationFrame(now));
        debug_assert!(queue.effects.is_empty());
        let subagents = if self.subagents.advance(now) {
            RenderRequest::Streaming
        } else {
            RenderRequest::None
        };
        let selection = self.update_selection_auto_scroll(now);
        let notification = if self
            .notification
            .as_ref()
            .is_some_and(|notice| now >= notice.deadline)
        {
            self.notification = None;
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        };
        ComponentUpdate {
            effects: effort
                .effects
                .into_iter()
                .chain(model.effects)
                .chain(composer.effects)
                .collect(),
            render: effort
                .render
                .max(model.render)
                .max(transcript.render)
                .max(composer.render)
                .max(queue.render)
                .max(subagents)
                .max(selection)
                .max(confirmation)
                .max(notification),
        }
    }

    fn update_selection_auto_scroll(&mut self, now: Instant) -> RenderRequest {
        let Some(mut scroll) = self.selection_auto_scroll.take() else {
            return RenderRequest::None;
        };
        if now < scroll.deadline {
            self.selection_auto_scroll = Some(scroll);
            return RenderRequest::None;
        }
        let Some(surface) = self.selection.surface() else {
            return RenderRequest::None;
        };
        let Some(span) = self.selection_span_on(surface, scroll.position) else {
            return RenderRequest::None;
        };
        self.selection.drag(span);
        if !self.scroll_selected_surface(surface, scroll.direction) {
            return RenderRequest::None;
        }
        scroll.deadline = now + SELECTION_SCROLL_INTERVAL;
        self.selection_auto_scroll = Some(scroll);
        RenderRequest::Immediate
    }

    fn apply_subagent_update(&mut self, update: AgentUpdate) -> ComponentUpdate<RootEffect> {
        let previous_active = self.subagents.active_count();
        let completion = match &update {
            AgentUpdate::Status {
                id,
                status: AgentStatus::Completed { .. },
            } => Some(*id),
            _ => None,
        };
        let root_message = match &update {
            AgentUpdate::Message(update)
                if update.thread.messages.iter().any(|message| {
                    message.id == update.message_id
                        && matches!(message.from, MessageSender::Agent { .. })
                }) =>
            {
                Some(update.clone())
            }
            _ => None,
        };
        let subagents_changed = self.subagents.apply(update);
        let mut result = root_message.map_or_else(ComponentUpdate::none, |update| {
            self.update_transcript(TranscriptEvent::DirectedMessage {
                perspective: MessageSender::Root,
                update,
            })
        });
        if !subagents_changed && result.render == RenderRequest::None {
            return result;
        }
        if let Some(Overlay::Subagents(SubagentOverlay::Transcript(id))) = self.overlay
            && !self.subagents.contains(id)
        {
            self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
        }
        let active = self.subagents.active_count();
        if active != previous_active {
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::ActiveSubagents {
                    count: active,
                    now: Instant::now(),
                });
        }
        if subagents_changed {
            result.render = result.render.max(RenderRequest::Immediate);
        }
        if let Some(id) = completion
            && subagents_changed
            && self.subagents.is_direct_child(id)
            && !self.has_active_turns()
            && self.blocking_task.is_none()
            && self.interactive
        {
            self.thread = ThreadState::Started;
            self.in_flight_turns = 1;
            result
                .effects
                .push(RootEffect::ContinueSubagent(subagent_completion_prompt(id)));
        }
        result
    }

    fn transcript_record(&mut self, record: Arc<TranscriptRecord>) -> ComponentUpdate<RootEffect> {
        if self
            .transcript
            .component()
            .ignores_finished_run_event(&record)
        {
            return ComponentUpdate::none();
        }
        if let Some(prompt) = recent_prompt(&record) {
            self.recent_prompts.push(prompt);
        }
        let turn_timer = turn_timer_event(&record);
        let observation = self.context_diagnostics.observe(&record);
        if let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay {
            panel
                .component_mut()
                .replace(self.context_diagnostics.clone());
        }
        let vault = crate::tui::vault::request(&record);
        let mut update = self.update_transcript(TranscriptEvent::Record(record));
        if let Some((key, command)) = vault
            && self.seen_vault_requests.insert(key)
        {
            update.effects.push(RootEffect::Vault(command));
            update.render = RenderRequest::Immediate;
        }
        if let Some(event) = turn_timer {
            let timer = self.update_composer(event, RenderRequest::Streaming);
            update.effects.extend(timer.effects);
            update.render = update.render.max(timer.render);
        }
        if let Some(tokens) = observation.completed_tokens {
            let context = self.update_composer(
                ComposerEvent::ContextTokens(tokens),
                RenderRequest::Streaming,
            );
            update.effects.extend(context.effects);
            update.render = update.render.max(context.render);
        }
        update
    }
}

fn subagent_completion_prompt(id: AgentId) -> Submission {
    Submission::text(format!(
        "A subagent completed after the previous turn ended. Continue the current task by \
         inspecting its structured result. In code mode, include completed agents when calling \
         list_agents, find agent {id}, and expose only the result fields needed for the next step. \
         Integrate or verify them as appropriate, perform any remaining work, and then respond to \
         the user. Do not merely repeat the raw result.\n\n\
         <subagent_completion agent_id=\"{id}\" />"
    ))
}

impl Component for RootNode {
    type Event = RootEvent;
    type Effect = RootEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            RootEvent::Terminal(event) => self.update_terminal(event),
            RootEvent::PasteImage(data_url) => {
                if self.blocking_task.is_some()
                    || self.overlay.is_some()
                    || (self.queue.component().focused() && self.queue_edit.is_none())
                {
                    ComponentUpdate::none()
                } else {
                    self.edit_composer(ComposerEvent::PasteImage(data_url))
                }
            }
            #[cfg(test)]
            RootEvent::ContextTokens(tokens) => self.update_composer(
                ComposerEvent::ContextTokens(tokens),
                RenderRequest::Streaming,
            ),
            RootEvent::Transcript(record) | RootEvent::ExternalTranscript(record) => {
                self.transcript_record(record)
            }
            RootEvent::AgentStreamClosed => self.agent_stream_closed(),
            RootEvent::AgentConnecting => {
                self.interactive = false;
                self.reconnecting = Some(true);
                self.reconnection_status("Connecting…")
            }
            RootEvent::AgentReconnected {
                active_turns,
                pending_local,
                reasoning_mode,
            } => self.agent_reconnected(active_turns, pending_local, reasoning_mode),
            RootEvent::AgentReconnectFailed(error) => {
                self.reconnecting = Some(false);
                self.notification = Some(Notification::plain(error, Color::Red));
                self.reconnection_status("Connection lost · Enter to reconnect")
            }
            RootEvent::Subagent(update) => self.apply_subagent_update(update),
            RootEvent::ReplaceDraft(draft) => {
                self.update_composer(ComposerEvent::ReplaceDraft(draft), RenderRequest::Immediate)
            }
            RootEvent::HandoffFinished(prompt) => {
                self.blocking_task = None;
                let waiting = self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                );
                let mut draft = self.update_composer(
                    ComposerEvent::ReplaceDraft(prompt),
                    RenderRequest::Immediate,
                );
                draft.effects.extend(waiting.effects);
                draft.render = draft.render.max(waiting.render);
                draft
            }
            RootEvent::HandoffCancelled => {
                self.blocking_task = None;
                self.notification = Some(Notification::plain(
                    "Handoff cancelled.".to_owned(),
                    Color::Yellow,
                ));
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::HandoffFailed(message) => {
                self.blocking_task = None;
                self.notification = Some(Notification::plain(message, Color::Red));
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewStarted => {
                self.blocking_task = Some(BlockingTask::Review);
                self.review_url = None;
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: true,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewReady(url) => {
                self.review_url = Some(url);
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: true,
                        status: Some("Review ready · O reopen · C copy link".to_owned()),
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewFinished(markdown) => {
                self.blocking_task = None;
                self.review_url = None;
                let waiting = self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                );
                let cursor = self.composer.component().cursor();
                let draft = self.composer.component().draft();
                let before = if draft[..cursor].is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                let after = if draft[cursor..].is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                let mut update = self.update_composer(
                    ComposerEvent::ReplaceRange {
                        range: cursor..cursor,
                        text: format!("{before}{markdown}{after}"),
                    },
                    RenderRequest::Immediate,
                );
                update.effects.extend(waiting.effects);
                update.render = update.render.max(waiting.render);
                update
            }
            RootEvent::ReviewCancelled => {
                self.blocking_task = None;
                self.review_url = None;
                self.notification = Some(Notification::plain(
                    "Review cancelled.".to_owned(),
                    Color::Yellow,
                ));
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewFailed(message) => {
                self.blocking_task = None;
                self.review_url = None;
                self.notification = Some(Notification::plain(message, Color::Red));
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::WorkerTurnFinished { terminal_expected } => {
                self.worker_turn_finished(terminal_expected)
            }
            RootEvent::ManagedTurnFinished => self.agent_turn_finished(),
            RootEvent::ManagedActiveTurns(count) => self.managed_active_turns(count),
            RootEvent::ShellFinished => {
                self.in_flight_shells = self.in_flight_shells.saturating_sub(1);
                ComponentUpdate::none()
            }
            RootEvent::TurnsCancelled => self.turns_cancelled(),
            RootEvent::ForkReady => self.fork_ready(),
            RootEvent::NewSessionFailed(message) => self.new_session_failed(message),
            RootEvent::SessionSearchResults {
                picker_id,
                request_id,
                query,
                result,
            } => {
                let Some(Overlay::Sessions(picker)) = &mut self.overlay else {
                    return ComponentUpdate::none();
                };
                if picker.component().id() != picker_id {
                    return ComponentUpdate::none();
                }
                let update = picker.update(SessionPickerEvent::SearchResults {
                    request_id,
                    query,
                    result,
                });
                ComponentUpdate {
                    effects: Vec::new(),
                    render: update.render,
                }
            }
            RootEvent::SessionsLoaded {
                request_id,
                sessions,
            } => self.sessions_loaded(request_id, sessions),
            RootEvent::SessionListFailed { request_id, error } => {
                if self.pending_session_list != Some(request_id) {
                    return ComponentUpdate::none();
                }
                self.pending_session_list = None;
                self.pending_session_mention = None;
                self.notification = Some(Notification::plain(error, Color::Red));
                self.resume_after_session_lookup()
            }
            RootEvent::RecentPromptsLoaded {
                session_id,
                prompts,
            } => self.recent_prompts_loaded(session_id, prompts),
            RootEvent::RecentPromptLoadFailed(message) => self.recent_prompt_load_failed(message),
            RootEvent::SessionLoadFailed(message) => self.session_load_failed(message),
            RootEvent::SessionRestored {
                draft_reset,
                projection,
                effort,
                reasoning_mode,
                preferred_reasoning_mode,
                fast_mode,
                model,
                skills,
            } => {
                // Startup restoration installs history for the session already
                // being edited. Preserve the complete draft, not just its text.
                self.reconnecting = None;
                self.interactive = true;
                self.composer
                    .component_mut()
                    .update(ComposerEvent::SubmissionPaused(false));
                let draft = match draft_reset {
                    DraftReset::Preserve => self.composer.component_mut().take_draft(),
                    DraftReset::Clear => None,
                };
                let workspace = self.workspace.clone();
                self.install_session_projection(
                    &workspace,
                    effort,
                    reasoning_mode,
                    preferred_reasoning_mode,
                    fast_mode,
                    *projection,
                );
                if let Some(draft) = draft {
                    self.composer.component_mut().restore_draft(draft);
                }
                self.set_model(model);
                self.set_skills(skills);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::SettingsHydrated {
                effort,
                fast_mode,
                model,
            } => {
                self.transcript.component_mut().set_effort(effort);
                self.subagents.set_effort(effort);
                self.composer
                    .component_mut()
                    .update(ComposerEvent::SetEffort(effort));
                self.set_fast_mode(fast_mode);
                self.set_model(model);
                self.restore_session_activity()
            }
            RootEvent::HistoryReplayed { projection } => {
                self.replay_history(*projection);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::VaultReceipt(receipt) => {
                self.thread = ThreadState::Started;
                self.queue.component_mut().push(Submission::text(receipt));
                self.submit_next_queued()
            }
            RootEvent::VaultReview(review) => {
                self.overlay = Some(Overlay::VaultReview(review));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::ShowAgentId(id) => {
                self.overlay = Some(Overlay::AgentId(id));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::VoiceStatus(status) => {
                self.voice_status = status;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::NotifyError(message) => {
                self.notification = Some(Notification::plain(message, Color::Red));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::NotifySuccess(message) => {
                self.notification = Some(Notification::plain(message, Color::Green));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::ConfirmReviewDownload => {
                self.overlay = Some(Overlay::ReviewDownload(Node::new(
                    ReviewDownloadConfirmation,
                )));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::UpdateAvailable(version) => {
                self.notification = Some(Notification::update_available(version));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::SteerAdmitted(id) => self.steer_admitted(id),
            RootEvent::SteerWithdrawn(id) => self.steer_withdrawn(id),
            RootEvent::SteerWithdrawalFailed { id, error } => {
                if self.withdrawing_steer == Some(id) {
                    self.withdrawing_steer = None;
                    self.withdrawing_prompt = None;
                    self.notification = Some(Notification::plain(error, Color::Yellow));
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::SteerUnconfirmed(id) => self.steer_unconfirmed(id),
            RootEvent::RetainPrompt { request_id, prompt } => {
                self.retain_prompt(request_id, prompt)
            }
            RootEvent::PromptConfirmed(request_id) => self.confirm_prompt(&request_id),
            RootEvent::SteerFailed { id } => self.steer_failed(id),
            RootEvent::AnimationFrame(now) => self.update_animation(now),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.render_root(frame, area, theme, true);
    }
}

fn turn_timer_event(record: &TranscriptRecord) -> Option<ComposerEvent> {
    if record.source() != "agent" {
        return None;
    }
    if record.kind() == "run.started" {
        let now = Instant::now();
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let elapsed_ms = u64::try_from(now_unix_ms)
            .unwrap_or(u64::MAX)
            .saturating_sub(record.recorded_at_unix_ms());
        return Some(ComposerEvent::TurnStarted {
            elapsed: Duration::from_millis(elapsed_ms),
            now,
        });
    }
    matches!(record.kind(), "run.completed" | "run.failed").then_some(ComposerEvent::TurnFinished)
}

fn recent_prompt(record: &TranscriptRecord) -> Option<RecentPromptDraft> {
    #[derive(serde::Deserialize)]
    struct UserPrompt {
        text: String,
    }

    if record.source() != "tact" || !matches!(record.kind(), "user.submitted" | "user.steered") {
        return None;
    }
    let prompt = record.decode_payload::<UserPrompt>().ok()?;
    Some(RecentPromptDraft {
        text: crate::tui::vault::receipt_summary(&prompt.text).unwrap_or(prompt.text),
        recorded_at_unix_ms: record.recorded_at_unix_ms(),
    })
}

fn render_notification(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    message: &Line<'_>,
    color: Color,
) {
    if area.is_empty() {
        return;
    }
    let text_width = message.width();
    let width = u16::try_from(text_width.saturating_add(4)).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(message.clone())
        .centered()
        .wrap(Wrap { trim: true });
    let body_width = width.min(area.width).saturating_sub(2).max(1);
    let body_height = u16::try_from(text_width.div_ceil(usize::from(body_width)))
        .unwrap_or(u16::MAX)
        .max(1);
    let popup = Floating::new("", width, body_height.saturating_add(2), &[])
        .at_top()
        .colors(color, color)
        .render(frame, area, theme);
    frame.render_widget(paragraph, popup.body);
}

fn render_key_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    composer_area: Rect,
    theme: &Theme,
    action: ConfirmationAction,
) {
    const HEIGHT: u16 = 4;
    const WIDTH: u16 = 28;

    let available_height = composer_area.y.saturating_sub(area.y);
    if available_height < HEIGHT {
        return;
    }

    let width = WIDTH.min(composer_area.width).min(area.width);
    let gap = u16::from(available_height > HEIGHT);
    let popup = Rect {
        x: composer_area.right().saturating_sub(width).max(area.x),
        y: composer_area.y.saturating_sub(HEIGHT + gap),
        width,
        height: HEIGHT,
    };
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", action.title_key()),
            Style::reset().add_modifier(Modifier::BOLD),
        ),
        Span::styled("then ", Style::default().fg(theme.muted())),
    ]);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border()))
        .title(title);
    let body = block.inner(popup);

    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            confirmation_line(action.title_key(), action.action_label(), theme),
            confirmation_line(
                if action == ConfirmationAction::Exit {
                    "Esc"
                } else {
                    "Any other key"
                },
                "cancel",
                theme,
            ),
        ]),
        body,
    );
}

fn confirmation_line(key: &'static str, label: &'static str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(key, Style::reset().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {label}"), Style::default().fg(theme.muted())),
    ])
}

fn clamp_to(position: Position, area: Rect) -> Position {
    Position::new(
        position.x.clamp(area.x, area.right().saturating_sub(1)),
        position.y.clamp(area.y, area.bottom().saturating_sub(1)),
    )
}

fn is_actions_trigger(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('/')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_file_finder_trigger(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('@')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_skill_picker_trigger(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('$')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_picker_navigation(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Tab | KeyCode::Up | KeyCode::Down | KeyCode::Esc
        )
}

fn is_mention_edit(event: &Event) -> bool {
    match event {
        Event::Key(key) => {
            matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && (key.code == KeyCode::Backspace
                    || matches!(key.code, KeyCode::Char(_))
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT))
        }
        Event::Paste(_) => true,
        _ => false,
    }
}

fn mention_edit_continues_query(event: &Event, valid: fn(char) -> bool) -> bool {
    match event {
        Event::Key(key) if key.code == KeyCode::Backspace => true,
        Event::Key(key) => {
            matches!(key.code, KeyCode::Char(character) if valid(character))
        }
        Event::Paste(text) => text.chars().all(valid),
        _ => false,
    }
}

fn is_file_query_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/')
}

fn model_name(model: Model) -> &'static str {
    match model {
        Model::Luna => "Luna",
        Model::Terra => "Terra",
        Model::Sol => "Sol",
        Model::Astra => "Astra",
        _ => model.as_str(),
    }
}

fn is_skill_query_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-'
}

fn is_focus_toggle(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
}

fn is_queue_shortcut(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Tab
        && key.modifiers.is_empty()
}

fn is_left_click_in(event: &Event, area: Rect) -> bool {
    if !is_left_click(event) {
        return false;
    }
    let Event::Mouse(mouse) = event else {
        unreachable!("left click helper only accepts mouse events");
    };
    area.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
}

fn is_left_click(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left)
    )
}

fn is_control_c(event: &Event) -> bool {
    is_control_key(event, 'c')
}

fn is_confirmation_key_repeat(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind != KeyEventKind::Repeat {
        return false;
    }
    is_control_c(event) || is_escape(event)
}

fn is_key_release(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release)
}

fn is_control_key(event: &Event, character: char) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char(character)
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_escape(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Esc
        && key.modifiers.is_empty()
}

fn is_submit_enter(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Enter
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
}

fn is_plain_enter(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Enter
        && key.modifiers.is_empty()
}

fn is_plain_key(event: &Event, character: char) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char(character)
        && key.modifiers.is_empty()
}

#[cfg(test)]
mod history_tests {
    use super::{Component, RenderRequest, RootEffect, RootEvent, RootNode};
    use crate::config::ReasoningEffort;
    use crate::tui::{
        theme::Theme,
        transcript::{LocalEvent, TranscriptRecord, TurnId},
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::Arc;

    #[test]
    fn live_voice_is_inline_and_mute_preserves_the_draft() {
        use crate::voice_state::{Phase, Status};
        let mut root = RootNode::new(std::path::Path::new("/workspace"), ReasoningEffort::Medium);
        root.composer
            .component_mut()
            .replace_draft("keep my draft".into());
        root.update(RootEvent::VoiceStatus(Some(Status {
            phase: Phase::Active,
            ..Status::default()
        })));
        for (sequence, speaker, text) in [
            (1, "user", "Check Omarchy"),
            (2, "assistant", "Checking now"),
        ] {
            let record = TranscriptRecord::from_local(
                sequence,
                0,
                LocalEvent::VoiceTranscript(crate::voice_state::Transcript {
                    session: "call".into(),
                    speaker: speaker.into(),
                    id: 0,
                    text: text.into(),
                    is_partial: false,
                }),
            )
            .unwrap();
            root.update(RootEvent::Transcript(Arc::new(record)));
        }
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal
            .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..20)
            .map(|y| (0..90).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        let user = rows
            .iter()
            .position(|row| row.contains("Check Omarchy"))
            .unwrap();
        let assistant = rows
            .iter()
            .position(|row| row.contains("Checking now"))
            .unwrap();
        let draft = rows
            .iter()
            .position(|row| row.contains("keep my draft"))
            .unwrap();
        assert!(user < assistant && assistant < draft);
        assert!(root.notification.is_none());
        assert!(root.transcript_area.bottom() <= root.queue_area.y);
        let key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        let update = root.update(RootEvent::Terminal(Event::Key(key)));
        assert!(matches!(
            update.effects.as_slice(),
            [RootEffect::Voice(crate::voice::Command::ToggleMute)]
        ));
        let mut repeated = key;
        repeated.kind = crossterm::event::KeyEventKind::Repeat;
        assert!(
            root.update(RootEvent::Terminal(Event::Key(repeated)))
                .effects
                .is_empty()
        );
        assert_eq!(root.composer.component().draft(), "keep my draft");
        root.update(RootEvent::VoiceStatus(None));
        terminal
            .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
            .unwrap();
        assert_eq!(root.transcript_area.bottom(), root.composer_area.y);
    }

    #[test]
    fn upward_at_top_and_home_request_older_history() {
        let mut root = RootNode::new(std::path::Path::new("/workspace"), ReasoningEffort::Medium);

        let page_up = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageUp,
            KeyModifiers::NONE,
        ))));
        assert!(matches!(
            page_up.effects.as_slice(),
            [RootEffect::LoadOlderHistory]
        ));

        let home = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Home,
            KeyModifiers::CONTROL,
        ))));
        assert!(matches!(
            home.effects.as_slice(),
            [RootEffect::LoadOlderHistory]
        ));
    }

    #[test]
    fn downward_scroll_does_not_request_older_history() {
        let mut root = RootNode::new(std::path::Path::new("/workspace"), ReasoningEffort::Medium);
        let update = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));

        assert!(update.effects.is_empty());
    }

    #[test]
    fn upward_scroll_away_from_the_loaded_top_does_not_request_history() {
        let mut root = RootNode::new(std::path::Path::new("/workspace"), ReasoningEffort::Medium);
        for sequence in 1..=30 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            let _ = root.update(RootEvent::Transcript(Arc::new(record)));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
            .unwrap();

        let update = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageUp,
            KeyModifiers::NONE,
        ))));

        assert!(update.effects.is_empty());
    }

    #[test]
    fn upward_scroll_prefetches_before_entering_the_cached_near_top_window() {
        let mut root = RootNode::new(std::path::Path::new("/workspace"), ReasoningEffort::Medium);
        for sequence in 1..=40 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            let _ = root.update(RootEvent::Transcript(Arc::new(record)));
        }
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let page_up = || {
            RootEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::NONE,
            )))
        };

        for _ in 0..20 {
            terminal
                .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
                .unwrap();
            let was_near_top = root.transcript.component().near_top();
            let update = root.update(page_up());
            if matches!(update.effects.as_slice(), [RootEffect::LoadOlderHistory]) {
                assert!(!was_near_top);
                assert!(!root.transcript.component().at_top());
                return;
            }
            assert!(!was_near_top);
        }

        panic!("expected to prefetch before entering the cached near-top window");
    }

    #[test]
    fn background_history_replay_does_not_restore_the_original_prompt_into_the_composer() {
        let mut root = RootNode::new(std::path::Path::new("/workspace"), ReasoningEffort::Medium);
        root.composer
            .component_mut()
            .replace_draft("new follow-up draft".to_owned());
        let original = Arc::new(
            TranscriptRecord::from_local(
                1,
                1,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(1),
                    text: "original prompt".to_owned(),
                },
            )
            .unwrap(),
        );
        let projection = RootNode::project_open_session(ReasoningEffort::Medium, vec![original]);

        let update = root.update(RootEvent::HistoryReplayed {
            projection: Box::new(projection),
        });

        assert_eq!(update.render, RenderRequest::Immediate);
        assert_eq!(root.composer.component().draft(), "new follow-up draft");
    }
}

#[cfg(test)]
mod live_control_tests {
    use super::{Component, RootEffect, RootEvent, RootNode};
    use crate::config::{ReasoningEffort, ReasoningMode};
    use crate::tui::transcript::{LocalEvent, TranscriptRecord, TurnId};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use nanocodex::{
        Model,
        agent::events::{AgentEvent, AgentEventKind},
    };
    use serde_json::{json, value::to_raw_value};
    use std::{path::Path, sync::Arc};

    fn key(code: KeyCode) -> RootEvent {
        RootEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn root_with_draft(draft: &str) -> RootNode {
        let mut root = RootNode::new(Path::new("/workspace"), ReasoningEffort::Medium);
        root.composer
            .component_mut()
            .replace_draft(draft.to_owned());
        root
    }

    #[test]
    fn finished_run_telemetry_does_not_replace_live_or_restored_context_usage() {
        let completed = |total| {
            json!({
                "call_index": 1, "model": "gpt-6-astra", "attempt": 1,
                "connection_generation": 1, "status": "completed", "duration_ns": 1,
                "time_to_first_event_ns": 1, "tool_calls": 0,
                "usage": {"total_tokens": total},
            })
        };
        let agent = |sequence, turn, kind, payload| {
            Arc::new(
                TranscriptRecord::from_agent(
                    sequence,
                    sequence * 10,
                    AgentEvent {
                        protocol_version: 1,
                        request_id: Arc::from("request"),
                        seq: sequence,
                        kind,
                        payload: to_raw_value(&payload).unwrap().into(),
                    },
                )
                .with_managed_turn_id(Some(turn)),
            )
        };
        let records = vec![
            agent(1, "old", AgentEventKind::RunStarted, json!({})),
            Arc::new(
                TranscriptRecord::from_local(
                    2,
                    20,
                    LocalEvent::ManagedFinalMessage {
                        turn_id: "old".to_owned(),
                        text: "finished".to_owned(),
                    },
                )
                .unwrap(),
            ),
            agent(3, "current", AgentEventKind::RunStarted, json!({})),
            agent(
                4,
                "current",
                AgentEventKind::ModelCallCompleted,
                completed(42),
            ),
            agent(
                5,
                "old",
                AgentEventKind::ApiEvent,
                json!({"phase": "generation", "direction": "inbound", "event": {"type": "response.completed", "response": {"usage": {"total_tokens": 9000}}}}),
            ),
            agent(6, "old", AgentEventKind::RunStarted, json!({})),
            agent(
                7,
                "old",
                AgentEventKind::ModelCallCompleted,
                completed(90000),
            ),
        ];
        let mut root = root_with_draft("preserve this draft");
        for record in &records[..4] {
            root.update(RootEvent::ExternalTranscript(Arc::clone(record)));
        }
        assert_eq!(root.context_diagnostics.usage.unwrap().total, 42);
        for record in &records[4..] {
            root.update(RootEvent::ExternalTranscript(Arc::clone(record)));
        }
        assert_eq!(root.context_diagnostics.usage.unwrap().total, 42);
        assert_eq!(root.composer.component().draft(), "preserve this draft");
        let restored = RootNode::project_open_session(ReasoningEffort::Medium, records);
        assert_eq!(restored.context_tokens, Some(42));
        assert_eq!(restored.context_diagnostics.usage.unwrap().total, 42);
    }

    #[test]
    fn restored_session_applies_the_draft_policy_without_losing_images_or_cursor() {
        use nanocodex::agent::input::{PromptInput, UserInput};
        for preserve in [false, true] {
            let mut root = root_with_draft("inspect ");
            root.composer
                .component_mut()
                .update(super::ComposerEvent::PasteImage(
                    "data:image/png;base64,attached".to_owned(),
                ));
            root.update(key(KeyCode::Home));
            root.update(key(KeyCode::Right));
            root.update(key(KeyCode::Right));
            let text = root.composer.component().draft().to_owned();
            let cursor = root.composer.component().cursor();
            if preserve {
                root.update(RootEvent::AgentConnecting);
            }
            root.update(RootEvent::SessionRestored {
                draft_reset: if preserve {
                    super::DraftReset::Preserve
                } else {
                    super::DraftReset::Clear
                },
                projection: Box::new(RootNode::project_open_session(
                    ReasoningEffort::Medium,
                    Vec::new(),
                )),
                effort: ReasoningEffort::Medium,
                reasoning_mode: ReasoningMode::Standard,
                preferred_reasoning_mode: ReasoningMode::Standard,
                fast_mode: false,
                model: Model::Sol,
                skills: Arc::from([]),
            });
            assert!(root.interactive);
            assert!(root.reconnecting.is_none());
            if preserve {
                assert!(root.discarded_draft.is_none());
            } else {
                assert!(root.composer.component().draft().is_empty());
                root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::Char('z'),
                    KeyModifiers::CONTROL,
                ))));
            }
            assert_eq!(root.composer.component().draft(), text);
            assert_eq!(root.composer.component().cursor(), cursor);
            let submission = root.composer.component_mut().take_submission().unwrap();
            let PromptInput::Content(content) = submission.agent_prompt().instruction else {
                panic!("restoring a draft must retain its attachment");
            };
            assert!(content.iter().any(|item| matches!(item, UserInput::Image { image_url, .. } if image_url.ends_with("attached"))));
        }
    }

    fn vault_review() -> crate::tui::vault::Review {
        crate::tui::vault::Review {
            login: nanocodex_managed::VaultLogin {
                id: "abcdefghijklmnopqrstuv".into(),
                name: "Verified login".into(),
                browser_origin: Some("https://old.example.com".into()),
            },
            origin: "https://example.com".into(),
            agent_id: "agent".into(),
            generation: 1,
            visible: false,
        }
    }

    #[test]
    fn vault_review_requires_visible_explicit_approval_and_cancels() {
        let mut root = root_with_draft("keep this draft");
        root.update(RootEvent::VaultReview(vault_review()));
        assert!(
            root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL,
            ))))
            .effects
            .is_empty()
        );
        assert!(root.update(key(KeyCode::Char('a'))).effects.is_empty());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| {
                root.render_focused(
                    frame,
                    frame.area(),
                    &crate::tui::theme::Theme::default(),
                    true,
                )
            })
            .unwrap();
        assert!(root.update(key(KeyCode::Enter)).effects.is_empty());
        assert!(root.update(key(KeyCode::Char('a'))).effects.is_empty());
        let approved = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::CONTROL,
        ))));
        assert!(
            matches!(approved.effects.as_slice(), [RootEffect::ApproveVault(review)] if review.login.name == "Verified login" && review.origin == "https://example.com")
        );
        assert_eq!(root.composer.component().draft(), "keep this draft");
        root.update(RootEvent::VaultReview(vault_review()));
        assert!(root.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(root.overlay.is_none());
    }

    #[test]
    fn vault_live_requests_deduplicate_echoes_and_replayed_history() {
        use nanocodex::agent::events::{AgentEvent, AgentEventKind};
        use serde_json::value::to_raw_value;
        let record = |sequence, tool: &str, result: serde_json::Value| {
            let (structured_result, result) = if tool == "request_vault_intake" {
                (result, serde_json::Value::Null)
            } else {
                (serde_json::Value::Null, result)
            };
            Arc::new(TranscriptRecord::from_agent(
                sequence,
                sequence,
                AgentEvent {
                    protocol_version: 1,
                    request_id: Arc::from("vault-turn"),
                    seq: sequence,
                    kind: AgentEventKind::ToolResult,
                    payload: to_raw_value(
                        &json!({"call_id": format!("call-{sequence}"), "tool": tool,
                    "status": "completed",
                    "structured_result": structured_result,
                    "result": result}),
                    )
                    .unwrap()
                    .into(),
                },
            ))
        };
        let request = json!({"type":"vault_intake","status":"input_required","operation":"authorize_origin",
            "kind":"login","vault_id":"abcdefghijklmnopqrstuv","origin":"https://example.com"});
        let direct = record(1, "request_vault_intake", request.clone());
        let echo = record(
            2,
            "exec",
            json!({"content":[{"type":"text","text":request.to_string()}]}),
        );
        let mut root = root_with_draft("preserved draft");
        assert!(matches!(
            root.update(RootEvent::Transcript(direct.clone()))
                .effects
                .as_slice(),
            [RootEffect::Vault(crate::tui::vault::Command::Review { .. })]
        ));
        assert!(
            root.update(RootEvent::Transcript(echo.clone()))
                .effects
                .is_empty()
        );
        assert_eq!(root.composer.component().draft(), "preserved draft");
        let mut restored = root_with_draft("restored draft");
        restored.replay_history(RootNode::project_open_session(
            ReasoningEffort::default(),
            vec![direct],
        ));
        assert!(restored.overlay.is_none());
        assert!(
            restored
                .update(RootEvent::Transcript(echo))
                .effects
                .is_empty()
        );
        assert_eq!(restored.composer.component().draft(), "restored draft");
        let create = record(
            3,
            "request_vault_intake",
            json!({"type":"vault_intake", "status":"input_required", "kind":"login"}),
        );
        assert!(matches!(
            root.update(RootEvent::Transcript(create))
                .effects
                .as_slice(),
            [RootEffect::Vault(crate::tui::vault::Command::Open)]
        ));
        let invalid = record(
            4,
            "request_vault_intake",
            json!({"type":"vault_intake", "status":"input_required", "kind":"login", "origin":"http://example.com"}),
        );
        assert!(
            root.update(RootEvent::Transcript(invalid))
                .effects
                .is_empty()
        );
    }

    #[test]
    fn vault_recent_receipt_is_readable() {
        let text = json!({"type":"vault_intake_receipt","status":"saved","operation":"authorize_origin","id":"abcdefghijklmnopqrstuv","kind":"login","name":"Example","browser_origin":"https://example.com"}).to_string();
        let record = TranscriptRecord::from_local(
            1,
            1,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text,
            },
        )
        .unwrap();
        let prompt = super::recent_prompt(&record).unwrap();
        assert!(prompt.text.contains("Website approved for Example"));
        assert!(!prompt.text.contains("vault_intake_receipt"));
    }

    #[test]
    fn vault_command_is_local_and_receipt_is_submitted() {
        let mut root = root_with_draft("/vault review abcdefghijklmnopqrstuv https://example.com");
        assert!(matches!(
            root.update(key(KeyCode::Enter)).effects.as_slice(),
            [RootEffect::Vault(crate::tui::vault::Command::Review { .. })]
        ));
        let receipt = crate::tui::vault::receipt(&vault_review().login);
        let update = root.update(RootEvent::VaultReceipt(receipt.clone()));
        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == receipt)
        );
    }

    #[test]
    fn idle_enter_submits_once() {
        let mut root = root_with_draft("start work");

        let update = root.update(key(KeyCode::Enter));

        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == "start work")
        );
        assert_eq!(root.in_flight_turns, 1);
        assert!(root.queue.component().is_empty());
    }

    #[test]
    fn durable_completion_releases_queue_in_either_callback_order() {
        for worker_first in [false, true] {
            let mut root = root_with_draft("start work");
            root.update(key(KeyCode::Enter));
            root.queue.component_mut().push("followup".to_owned());
            let worker = RootEvent::WorkerTurnFinished {
                terminal_expected: true,
            };
            let managed = RootEvent::ManagedTurnFinished;
            let (first, last) = if worker_first {
                (worker, managed)
            } else {
                (managed, worker)
            };
            assert!(root.update(first).effects.is_empty());
            let update = root.update(last);
            assert!(
                matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == "followup")
            );
            assert_eq!(root.in_flight_turns, 1);
            assert_eq!(root.unmatched_worker_turns, 0);
            assert_eq!(root.unmatched_agent_turns, 0);
        }
    }

    #[test]
    fn local_prompt_keeps_thinking_visible_until_a_pending_submission_fails() {
        let mut root = root_with_draft("start work");
        let _ = root.update(key(KeyCode::Enter));
        let record = TranscriptRecord::from_local(
            1,
            1,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text: "start work".to_owned(),
            },
        )
        .unwrap();
        let _ = root.update(RootEvent::Transcript(Arc::new(record)));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let mut rendered = |root: &mut RootNode| {
            terminal
                .draw(|frame| {
                    root.render_focused(
                        frame,
                        frame.area(),
                        &crate::tui::theme::Theme::default(),
                        true,
                    )
                })
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let pending = rendered(&mut root);
        assert!(pending.contains("start work"));
        assert!(pending.contains("Thinking…"));

        let _ = root.update(RootEvent::WorkerTurnFinished {
            terminal_expected: false,
        });
        assert!(!rendered(&mut root).contains("Thinking…"));
    }

    #[test]
    fn goal_menu_selection_submits_the_bare_command() {
        let mut root = root_with_draft("");
        for character in "/Goal".chars() {
            root.update(key(KeyCode::Char(character)));
        }
        let update = root.update(key(KeyCode::Enter));
        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == "/goal")
        );
        assert!(root.overlay.is_none());
    }

    #[test]
    fn slash_goal_bypasses_active_work_and_pending_steers() {
        for typed in [false, true] {
            for active in [false, true] {
                for pending_steer in [false, true] {
                    for command in ["/goal status", "/goal pause", "/goal resume", "/goal clear"] {
                        let mut root = root_with_draft(if typed { "" } else { command });
                        root.managed_active_turns = usize::from(active);
                        if pending_steer {
                            root.queue
                                .component_mut()
                                .begin_steer("existing steer".to_owned().into());
                        }
                        let _ = root.sync_live_controls();
                        if typed {
                            for character in command.chars() {
                                root.update(key(KeyCode::Char(character)));
                            }
                        }
                        let update = root.update(key(KeyCode::Enter));
                        assert!(
                            matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == command)
                        );
                        assert_eq!(root.queue.component().has_pending_steer(), pending_steer);
                    }
                }
            }
        }
    }

    #[test]
    fn slash_goal_routes_literal_commands_from_draft_and_actions() {
        for typed in [false, true] {
            for command in [
                "/goal",
                "/goal status",
                "/goal pause",
                "/goal resume",
                "/goal clear",
                "/goal build  a better TUI",
            ] {
                let mut root = root_with_draft(if typed { "" } else { command });
                if typed {
                    for character in command.chars() {
                        root.update(key(KeyCode::Char(character)));
                    }
                }
                let update = root.update(key(KeyCode::Enter));
                assert!(
                    matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == command)
                );
                assert!(root.composer.component().draft().is_empty());
                assert!(root.overlay.is_none());
            }
        }
    }

    #[test]
    fn slash_bug_routes_from_draft_and_actions_even_while_active() {
        for typed in [false, true] {
            for activity in 0..3 {
                for description in ["", "rendering breaks on resize"] {
                    let command = format!("/bug {description}");
                    let mut root = root_with_draft(if typed { "" } else { &command });
                    root.in_flight_turns = usize::from(activity == 1);
                    root.managed_active_turns = usize::from(activity == 2);
                    let _ = root.sync_live_controls();
                    if typed {
                        for character in command.chars() {
                            root.update(key(KeyCode::Char(character)));
                        }
                    }
                    let update = root.update(key(KeyCode::Enter));
                    assert!(matches!(
                        update.effects.as_slice(),
                        [RootEffect::Bug(actual)] if actual == description
                    ));
                    assert!(root.composer.component().draft().is_empty());
                    assert!(root.overlay.is_none());
                    assert_eq!(root.in_flight_turns, usize::from(activity == 1));
                    assert_eq!(root.managed_active_turns, usize::from(activity == 2));
                }
            }
        }
    }

    #[test]
    fn slash_attach_opens_thread_picker_from_draft_and_actions() {
        for typed in [false, true] {
            let mut root = root_with_draft(if typed { "" } else { "/attach" });
            if typed {
                for character in "/attach".chars() {
                    root.update(key(KeyCode::Char(character)));
                }
            }
            let update = root.update(key(KeyCode::Enter));
            assert!(matches!(
                update.effects.as_slice(),
                [RootEffect::LoadSessions {
                    kind: super::SessionListKind::Resume,
                    ..
                }]
            ));
            assert!(root.composer.component().draft().is_empty());
            assert_eq!(root.in_flight_turns, 0);
        }
    }

    #[test]
    fn slash_attach_rejects_arguments_and_active_work_without_submitting() {
        let mut root = root_with_draft("/attach unexpected");
        assert!(root.update(key(KeyCode::Enter)).effects.is_empty());
        assert!(root.pending_session_list.is_none());
        let mut root = root_with_draft("/attach");
        root.in_flight_turns = 1;
        assert!(root.update(key(KeyCode::Enter)).effects.is_empty());
        assert!(root.pending_session_list.is_none());
    }

    #[test]
    fn slash_model_command_routes_as_a_hosted_setting_instead_of_a_prompt() {
        let mut root = root_with_draft("/model sol");

        let update = root.update(key(KeyCode::Enter));

        assert!(matches!(
            update.effects.as_slice(),
            [RootEffect::SetModel(Model::Sol)]
        ));
        assert!(matches!(root.thread, super::ThreadState::New));
        assert!(root.composer.component().draft().is_empty());
        assert_eq!(root.in_flight_turns, 0);
    }

    #[test]
    fn slash_action_overlay_routes_direct_model_command() {
        let mut root = root_with_draft("");
        let _ = root.update(key(KeyCode::Char('/')));
        for character in "model sol".chars() {
            let _ = root.update(key(KeyCode::Char(character)));
        }

        let update = root.update(key(KeyCode::Enter));

        assert!(matches!(
            update.effects.as_slice(),
            [RootEffect::SetModel(Model::Sol)]
        ));
        assert!(matches!(root.thread, super::ThreadState::New));
        assert_eq!(root.in_flight_turns, 0);
    }

    #[test]
    fn empty_attached_agent_keeps_model_selection_unlocked() {
        let mut root = root_with_draft("");
        let projection = RootNode::project_open_session(ReasoningEffort::Medium, Vec::new());
        root.install_session_projection(
            Path::new("/workspace"),
            ReasoningEffort::Medium,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            false,
            projection,
        );
        let _ = root.update(key(KeyCode::Char('/')));
        for character in "model sol".chars() {
            let _ = root.update(key(KeyCode::Char(character)));
        }

        let update = root.update(key(KeyCode::Enter));

        assert!(matches!(
            update.effects.as_slice(),
            [RootEffect::SetModel(Model::Sol)]
        ));
    }

    #[test]
    fn slash_thinking_alias_routes_as_a_durable_effort_setting() {
        let mut root = root_with_draft("/reasoning high");

        let update = root.update(key(KeyCode::Enter));

        assert!(matches!(
            update.effects.as_slice(),
            [RootEffect::SetEffort {
                effort: ReasoningEffort::High,
                ..
            }]
        ));
        assert_eq!(root.composer.component().effort(), ReasoningEffort::High);
        assert_eq!(root.in_flight_turns, 0);
    }

    fn undo_message() -> RootEvent {
        RootEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::ALT,
        )))
    }

    #[test]
    fn undo_latest_queued_message_uses_submission_order() {
        let mut root = root_with_draft("");
        root.in_flight_turns = 1;
        root.queue.component_mut().push("older followup".to_owned());
        let (id, _) = root
            .queue
            .component_mut()
            .begin_steer("newer steer".to_owned().into());
        root.queue.component_mut().steer_failed(id);
        let update = root.update(undo_message());
        assert!(update.effects.is_empty());
        assert_eq!(root.composer.component().draft(), "newer steer");
        assert_eq!(root.queue.component().len(), 1);
    }

    #[test]
    fn undo_queued_image_message_restores_the_original_image_payload() {
        use nanocodex::agent::input::{PromptInput, UserInput};
        let mut root = root_with_draft("inspect ");
        root.update(RootEvent::PasteImage(
            "data:image/png;base64,original".to_owned(),
        ));
        let draft = root.composer.component_mut().take_draft().unwrap();
        root.in_flight_turns = 1;
        root.queue.component_mut().push(draft.into_submission());
        root.update(undo_message());
        assert!(root.composer.component().has_images());
        let draft = root.composer.component_mut().take_draft().unwrap();
        let PromptInput::Content(content) = draft.into_submission().agent_prompt().instruction
        else {
            panic!("expected content")
        };
        assert!(content.iter().any(|item| matches!(item, UserInput::Image { image_url, .. } if image_url == "data:image/png;base64,original")));
    }

    #[test]
    fn undo_steer_waits_for_confirmed_withdrawal_and_ignores_duplicate_shortcut() {
        let mut root = root_with_draft("change direction");
        root.update(RootEvent::ManagedActiveTurns(1));
        let sent = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = sent.effects.as_slice() else {
            panic!("expected steer")
        };
        let id = *id;
        let update = root.update(undo_message());
        assert_eq!(update.effects, [RootEffect::WithdrawSteer { id }]);
        assert!(root.composer.component().draft().is_empty());
        assert_eq!(root.queue.component().len(), 1);
        assert!(root.update(undo_message()).effects.is_empty());
        root.update(RootEvent::SteerAdmitted(id));
        assert!(root.composer.component().draft().is_empty());
        root.update(RootEvent::SteerWithdrawn(id));
        assert_eq!(root.composer.component().draft(), "change direction");
        assert!(root.queue.component().is_empty());
        assert!(root.last_admitted_steer.is_none());
    }

    #[test]
    fn undo_failure_never_restores_or_removes_the_steer() {
        let mut root = root_with_draft("already received");
        root.update(RootEvent::ManagedActiveTurns(1));
        let sent = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = sent.effects.as_slice() else {
            panic!("expected steer")
        };
        let id = *id;
        root.update(undo_message());
        root.update(RootEvent::SteerWithdrawalFailed {
            id,
            error: "Already received by the model".to_owned(),
        });
        assert!(root.composer.component().draft().is_empty());
        assert_eq!(root.queue.component().len(), 1);
        assert!(root.withdrawing_steer.is_none());
    }

    #[test]
    fn undo_preserves_its_prompt_when_a_newer_steer_is_admitted() {
        let mut root = root_with_draft("withdraw first");
        root.update(RootEvent::ManagedActiveTurns(1));
        let sent = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = sent.effects.as_slice() else {
            panic!("expected steer")
        };
        let id = *id;
        root.update(RootEvent::SteerAdmitted(id));
        root.update(undo_message());
        root.composer
            .component_mut()
            .replace_draft("new steer".to_owned());
        let sent = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id: newer, .. }] = sent.effects.as_slice() else {
            panic!("expected steer")
        };
        let newer = *newer;
        root.update(RootEvent::SteerAdmitted(newer));
        root.update(RootEvent::SteerWithdrawn(id));
        assert_eq!(root.composer.component().draft(), "withdraw first");
        assert_eq!(root.last_admitted_steer.as_ref().unwrap().0, newer);
    }

    #[test]
    fn confirmed_undo_preserves_an_image_only_composer() {
        let mut root = root_with_draft("withdraw me");
        root.update(RootEvent::ManagedActiveTurns(1));
        let sent = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = sent.effects.as_slice() else {
            panic!("expected steer")
        };
        let id = *id;
        root.update(RootEvent::SteerAdmitted(id));
        root.update(undo_message());
        root.update(RootEvent::PasteImage(
            "data:image/png;base64,new-image".to_owned(),
        ));
        let image_draft = root.composer.component().draft().to_owned();
        root.update(RootEvent::SteerWithdrawn(id));
        root.restore_discarded_draft();
        assert_eq!(root.composer.component().draft(), image_draft);
        assert!(root.composer.component().has_images());
        assert!(root.withdrawn_draft.is_some());
        let draft = root.composer.component_mut().take_draft().unwrap();
        let nanocodex::agent::input::PromptInput::Content(content) =
            draft.into_submission().agent_prompt().instruction
        else {
            panic!("expected image content")
        };
        assert!(content.iter().any(|item| matches!(item,
            nanocodex::agent::input::UserInput::Image { image_url, .. }
                if image_url == "data:image/png;base64,new-image")));
        root.restore_discarded_draft();
        assert_eq!(root.composer.component().draft(), "withdraw me");
        assert!(!root.composer.component().has_images());
        assert!(root.withdrawn_draft.is_none());
    }

    #[test]
    fn confirmed_undo_preserves_a_draft_typed_during_withdrawal() {
        let mut root = root_with_draft("withdraw me");
        root.update(RootEvent::ManagedActiveTurns(1));
        let sent = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = sent.effects.as_slice() else {
            panic!("expected steer")
        };
        let id = *id;
        root.update(RootEvent::SteerAdmitted(id));
        root.update(undo_message());
        root.composer
            .component_mut()
            .replace_draft("new draft".to_owned());
        root.update(RootEvent::SteerWithdrawn(id));
        assert_eq!(root.composer.component().draft(), "new draft");
        root.discard_draft();
        root.restore_discarded_draft();
        assert_eq!(root.composer.component().draft(), "withdraw me");
        assert!(root.discarded_draft.is_some());
    }

    #[test]
    fn active_enter_steers_once_without_first_queuing() {
        let mut root = root_with_draft("change direction");
        root.in_flight_turns = 1;
        let _ = root.sync_live_controls();

        let update = root.update(key(KeyCode::Enter));

        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Steer { prompt, .. }] if prompt.display_text() == "change direction")
        );
        assert_eq!(root.in_flight_turns, 1);
        assert_eq!(root.queue.component().len(), 1);
        assert!(root.queue.component().has_pending_steer());
    }

    #[test]
    fn attached_active_enter_steers_without_starting_a_local_turn() {
        let mut root = root_with_draft("change attached direction");
        let _ = root.update(RootEvent::ManagedActiveTurns(1));

        let update = root.update(key(KeyCode::Enter));

        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Steer { prompt, .. }] if prompt.display_text() == "change attached direction")
        );
        assert_eq!(root.in_flight_turns, 0);
        assert_eq!(root.managed_active_turns, 1);
        assert!(root.queue.component().has_pending_steer());
    }

    #[test]
    fn foreign_steering_never_consumes_local_input_and_unknown_delivery_survives_completion() {
        let mut root = root_with_draft("my instruction");
        root.update(RootEvent::ManagedActiveTurns(1));
        let update = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = update.effects.as_slice() else {
            panic!("expected local steering");
        };
        let id = *id;
        let foreign = |seq| {
            Arc::new(TranscriptRecord::from_agent(
                seq,
                seq,
                AgentEvent {
                    protocol_version: 1,
                    request_id: Arc::from("shared-agent"),
                    seq,
                    kind: AgentEventKind::RunSteered,
                    payload: to_raw_value(&json!({"steer_index": seq, "instruction_bytes": 14}))
                        .unwrap()
                        .into(),
                },
            ))
        };
        assert!(
            root.update(RootEvent::ExternalTranscript(foreign(1)))
                .effects
                .is_empty()
        );
        assert_eq!(root.queue.component().len(), 1);
        assert!(root.queue.component().has_pending_steer());
        root.update(RootEvent::SteerUnconfirmed(id));
        assert!(
            root.update(RootEvent::ExternalTranscript(foreign(2)))
                .effects
                .is_empty()
        );
        assert_eq!(root.queue.component().len(), 1);
        root.queue.component_mut().push("known unsent".to_owned());
        let completion = root.update(RootEvent::ManagedActiveTurns(0));
        assert!(
            matches!(completion.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == "known unsent")
        );
        assert_eq!(
            root.queue.component().len(),
            1,
            "uncertain input must remain visible after completion"
        );
        assert!(root.queue.component_mut().drain_ready().is_empty());
    }

    #[test]
    fn terminal_then_late_steer_recovery_drains_the_queue() {
        let mut root = root_with_draft("change attached direction");
        let _ = root.update(RootEvent::ManagedActiveTurns(1));
        let steer = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { id, .. }] = steer.effects.as_slice() else {
            panic!("active submission should start a steer");
        };
        let id = *id;
        root.queue.component_mut().push("follow up".to_owned());

        let terminal = root.update(RootEvent::ManagedActiveTurns(0));
        assert!(terminal.effects.is_empty());
        assert!(root.queue.component().has_pending_steer());

        let recovered = root.update(RootEvent::SteerFailed { id });
        assert!(
            matches!(recovered.effects.as_slice(), [RootEffect::Submit(prompt)]
                if prompt.display_text().contains("change attached direction")
                    && prompt.display_text().contains("follow up"))
        );
        assert!(root.queue.component().is_empty());
        assert!(!root.queue.component().has_pending_steer());
    }

    #[test]
    fn nonempty_tab_queues_during_an_active_turn() {
        let mut root = root_with_draft("follow up");
        root.in_flight_turns = 1;
        let _ = root.sync_live_controls();

        let update = root.update(key(KeyCode::Tab));

        assert!(update.effects.is_empty());
        assert_eq!(root.queue.component().len(), 1);
        assert!(!root.queue.component().has_pending_steer());
        assert!(root.composer.component().draft().is_empty());
    }

    #[test]
    fn idle_nonempty_tab_keeps_the_draft_and_focus_traversal() {
        let mut root = root_with_draft("not yet");
        root.queue.component_mut().push("already queued".to_owned());

        let update = root.update(key(KeyCode::Tab));

        assert!(update.effects.is_empty());
        assert_eq!(root.composer.component().draft(), "not yet");
        assert_eq!(root.queue.component().len(), 1);
        assert!(root.queue.component().focused());
    }

    #[test]
    fn queue_focused_tab_returns_to_the_composer_without_consuming_its_draft() {
        let mut root = root_with_draft("first follow up");
        root.in_flight_turns = 1;
        let _ = root.sync_live_controls();
        let _ = root.update(key(KeyCode::Tab));
        root.composer
            .component_mut()
            .replace_draft("keep this draft".to_owned());
        root.queue.component_mut().set_focused(true);

        let update = root.update(key(KeyCode::Tab));

        assert!(update.effects.is_empty());
        assert_eq!(root.composer.component().draft(), "keep this draft");
        assert_eq!(root.queue.component().len(), 1);
        assert!(!root.queue.component().focused());
    }

    #[test]
    fn escape_offers_stop_for_a_locally_active_turn() {
        let mut idle = root_with_draft("");
        assert!(idle.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(idle.key_confirmation.is_none());

        let mut active = root_with_draft("");
        active.in_flight_turns = 1;
        let _ = active.sync_live_controls();
        assert!(active.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(active.key_confirmation.is_some());
        assert!(matches!(
            active.update(key(KeyCode::Esc)).effects.as_slice(),
            [RootEffect::CancelTurns]
        ));
    }

    #[test]
    fn escape_offers_stop_for_an_attached_active_turn() {
        let mut root = root_with_draft("");
        let _ = root.update(RootEvent::ManagedActiveTurns(1));

        assert!(root.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(root.key_confirmation.is_some());
        assert!(matches!(
            root.update(key(KeyCode::Esc)).effects.as_slice(),
            [RootEffect::CancelTurns]
        ));
    }

    #[test]
    fn external_run_failed_does_not_match_a_local_worker_completion() {
        let mut root = root_with_draft("");
        root.in_flight_turns = 1;
        root.unmatched_worker_turns = 1;
        let record = TranscriptRecord::from_agent(
            1,
            1,
            AgentEvent {
                protocol_version: 1,
                request_id: Arc::from("attached-1"),
                seq: 1,
                kind: AgentEventKind::RunFailed,
                payload: to_raw_value(&json!({"error": "attached failure"}))
                    .unwrap()
                    .into(),
            },
        );

        let update = root.update(RootEvent::ExternalTranscript(Arc::new(record)));

        assert!(update.effects.is_empty());
        assert_eq!(root.in_flight_turns, 1);
        assert_eq!(root.unmatched_worker_turns, 1);
        assert_eq!(root.unmatched_agent_turns, 0);
    }

    #[test]
    fn unknown_image_steering_supports_explicit_edit_retry_and_cancellation() {
        use crate::tui::Submission;
        use nanocodex::agent::input::{PromptInput, UserInput};
        for save in [false, true] {
            let mut root = root_with_draft("preserved draft ");
            root.update(RootEvent::PasteImage(
                "data:image/png;base64,preserved".to_owned(),
            ));
            root.update(RootEvent::ManagedActiveTurns(1));
            let original = Submission::multimodal(
                "inspect [Image #3]".to_owned(),
                [(8..18, "data:image/png;base64,original".to_owned())],
            );
            let (id, _) = root.queue.component_mut().begin_steer(original);
            root.update(RootEvent::SteerUnconfirmed(id));
            root.queue.component_mut().set_focused(true);
            root.update(key(KeyCode::Char('e')));
            assert!(
                root.queue_edit.is_some(),
                "unknown image input needs an explicit retry path"
            );
            assert!(root.composer.component().has_images());
            root.update(key(KeyCode::Home));
            root.update(RootEvent::Terminal(Event::Paste("updated ".to_owned())));
            root.update(key(KeyCode::End));
            root.update(RootEvent::PasteImage(
                "data:image/png;base64,added".to_owned(),
            ));
            assert_eq!(
                root.composer.component().draft(),
                "updated inspect [Image #3][Image #4]"
            );
            assert!(
                root.update(key(if save { KeyCode::Enter } else { KeyCode::Esc }))
                    .effects
                    .is_empty()
            );
            assert_eq!(
                root.composer.component().draft(),
                "preserved draft [Image #1]"
            );
            assert!(root.composer.component().has_images());
            let mut update = root.update(RootEvent::ManagedActiveTurns(0));
            if !save {
                assert!(
                    update.effects.is_empty(),
                    "cancelled unknown input must not be retried automatically"
                );
                root.update(key(KeyCode::Char('e')));
                update = root.update(key(KeyCode::Enter));
            }
            let [RootEffect::Submit(prompt)] = update.effects.as_slice() else {
                panic!("explicitly saving the image input should submit it once");
            };
            assert_eq!(
                prompt.display_text(),
                if save {
                    "updated inspect [Image #3][Image #4]"
                } else {
                    "inspect [Image #3]"
                }
            );
            let PromptInput::Content(content) = prompt.agent_prompt().instruction else {
                panic!("retry must contain image content");
            };
            let images = content
                .iter()
                .filter_map(|item| match item {
                    UserInput::Image { image_url, .. } => Some(image_url.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                images,
                if save {
                    vec![
                        "data:image/png;base64,original",
                        "data:image/png;base64,added",
                    ]
                } else {
                    vec!["data:image/png;base64,original"]
                }
            );
            assert!(root.queue.component().is_empty());
        }
    }

    #[test]
    fn saving_a_queue_edit_preserves_images_recalled_from_prompt_history() {
        use nanocodex::agent::input::{PromptInput, UserInput};
        let mut root = root_with_draft("inspect ");
        root.update(RootEvent::PasteImage(
            "data:image/png;base64,recalled".to_owned(),
        ));
        root.update(key(KeyCode::Enter));
        root.update(RootEvent::WorkerTurnFinished {
            terminal_expected: false,
        });
        root.update(RootEvent::ManagedActiveTurns(1));
        root.queue
            .component_mut()
            .push("replace this instruction".to_owned());
        root.queue.component_mut().set_focused(true);
        root.update(key(KeyCode::Char('e')));
        root.update(key(KeyCode::Up));
        assert!(root.composer.component().has_images());
        root.update(key(KeyCode::Home));
        root.update(RootEvent::Terminal(Event::Paste("  ".to_owned())));
        root.update(key(KeyCode::End));
        root.update(RootEvent::Terminal(Event::Paste("  ".to_owned())));
        assert!(root.update(key(KeyCode::Enter)).effects.is_empty());
        let update = root.update(RootEvent::ManagedActiveTurns(0));
        let [RootEffect::Submit(prompt)] = update.effects.as_slice() else {
            panic!("the saved queue revision should submit after the current turn");
        };
        let PromptInput::Content(content) = prompt.agent_prompt().instruction else {
            panic!("the saved revision must remain multimodal");
        };
        assert!(
            matches!(content.as_slice(), [UserInput::Text { text }, UserInput::Image { image_url, .. }, UserInput::Text { text: trailing }]
            if text == "  inspect " && image_url == "data:image/png;base64,recalled" && trailing == "  ")
        );
    }

    #[test]
    fn history_navigation_keeps_images_in_a_queued_followup() {
        use nanocodex::agent::input::{PromptInput, UserInput};
        let mut root = root_with_draft("earlier prompt");
        assert!(matches!(
            root.update(key(KeyCode::Enter)).effects.as_slice(),
            [RootEffect::Submit(_)]
        ));
        root.update(RootEvent::WorkerTurnFinished {
            terminal_expected: false,
        });
        root.update(RootEvent::ManagedActiveTurns(1));
        root.update(RootEvent::ReplaceDraft("inspect ".to_owned()));
        root.update(RootEvent::PasteImage(
            "data:image/png;base64,queued-image".to_owned(),
        ));
        root.update(key(KeyCode::Up));
        assert_eq!(root.composer.component().draft(), "earlier prompt");
        root.update(key(KeyCode::Down));
        assert!(root.update(key(KeyCode::Tab)).effects.is_empty());
        let update = root.update(RootEvent::ManagedActiveTurns(0));
        let [RootEffect::Submit(prompt)] = update.effects.as_slice() else {
            panic!("the followup should submit after the active turn ends");
        };
        let PromptInput::Content(content) = prompt.agent_prompt().instruction else {
            panic!("queued followup must retain image content");
        };
        assert!(
            matches!(content.as_slice(), [UserInput::Text { text }, UserInput::Image { image_url, .. }]
            if text == "inspect " && image_url == "data:image/png;base64,queued-image")
        );
    }

    #[test]
    fn reflection_submit_shortcuts_keep_the_reflection_action_and_close_its_editor() {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SUPER] {
            let mut root = root_with_draft("");
            root.update(key(KeyCode::Char('/')));
            root.update(RootEvent::Terminal(Event::Paste("reflection".to_owned())));
            root.update(key(KeyCode::Enter));
            assert!(root.reflection_input);
            root.update(RootEvent::ReplaceDraft(
                "review the failed attempts".to_owned(),
            ));

            let update = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                modifiers,
            ))));
            assert!(
                matches!(update.effects.as_slice(), [RootEffect::Reflect(prompt)]
                if prompt.display_text() == "review the failed attempts")
            );
            assert!(!root.reflection_input);
            assert!(root.composer.component().input_mode().is_none());
            assert!(root.composer.component().draft().is_empty());
        }
    }

    #[test]
    fn disconnected_queue_editor_can_cancel_without_sending_or_reconnecting() {
        for reconnect_failed in [false, true] {
            let mut root = root_with_draft("preserved composer draft");
            root.update(RootEvent::ManagedActiveTurns(1));
            root.queue
                .component_mut()
                .push("original instruction".to_owned());
            root.queue.component_mut().set_focused(true);
            root.update(key(KeyCode::Char('e')));
            root.composer
                .component_mut()
                .replace_draft("unsaved revision".to_owned());
            root.update(RootEvent::AgentStreamClosed);
            if reconnect_failed {
                root.update(RootEvent::AgentReconnectFailed("offline".to_owned()));
            }

            assert!(root.update(key(KeyCode::Esc)).effects.is_empty());
            assert!(
                root.queue_edit.is_none(),
                "local cancellation must work while disconnected"
            );
            assert_eq!(
                root.composer.component().draft(),
                "preserved composer draft"
            );
            assert_eq!(root.reconnecting, Some(!reconnect_failed));
            assert!(!root.interactive);

            let update = root.update(RootEvent::AgentReconnected {
                active_turns: 0,
                pending_local: false,
                reasoning_mode: ReasoningMode::Standard,
            });
            assert!(
                matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)]
                if prompt.display_text() == "original instruction")
            );
            assert!(root.queue.component().is_empty());
        }
    }

    #[test]
    fn tab_in_queue_editor_keeps_the_revision_unsent_until_explicit_save() {
        for save in [false, true] {
            let mut root = root_with_draft("preserved draft");
            root.update(RootEvent::ManagedActiveTurns(1));
            root.queue
                .component_mut()
                .push("original instruction".to_owned());
            root.queue.component_mut().set_focused(true);
            root.update(key(KeyCode::Char('e')));
            assert!(root.queue_edit.is_some());
            root.composer
                .component_mut()
                .replace_draft("unfinished revision".to_owned());

            assert!(root.update(key(KeyCode::Tab)).effects.is_empty());
            assert_eq!(root.composer.component().draft(), "unfinished revision");
            assert_eq!(root.queue.component().len(), 1);
            assert!(
                root.update(key(if save { KeyCode::Enter } else { KeyCode::Esc }))
                    .effects
                    .is_empty()
            );
            assert_eq!(root.composer.component().draft(), "preserved draft");

            let update = root.update(RootEvent::ManagedActiveTurns(0));
            let [RootEffect::Submit(prompt)] = update.effects.as_slice() else {
                panic!("exactly one queued instruction should be submitted");
            };
            assert_eq!(
                prompt.display_text(),
                if save {
                    "unfinished revision"
                } else {
                    "original instruction"
                }
            );
            assert!(root.queue.component().is_empty());
        }
    }

    #[test]
    fn prompt_confirmation_during_edit_preserves_the_draft_and_requires_explicit_resubmission() {
        for save in [false, true] {
            let mut root = root_with_draft("preserved draft");
            root.retain_prompt("request-1".to_owned(), "original".to_owned().into());
            root.queue.component_mut().set_focused(true);
            root.update(key(KeyCode::Char('e')));
            root.composer
                .component_mut()
                .replace_draft("revised input".to_owned());
            root.confirm_prompt("request-1");
            let update = root.update(key(if save { KeyCode::Enter } else { KeyCode::Esc }));
            assert_eq!(root.composer.component().draft(), "preserved draft");
            assert!(root.queue.component().is_empty());
            if save {
                assert!(
                    matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == "revised input")
                );
            } else {
                assert!(
                    update.effects.is_empty(),
                    "cancelling an edit must not repeat confirmed delivery"
                );
            }
        }
    }

    #[test]
    fn late_original_receipt_does_not_discard_an_explicitly_saved_revision() {
        let mut root = root_with_draft("preserved draft");
        root.update(RootEvent::ManagedActiveTurns(1));
        root.retain_prompt("request-1".to_owned(), "original".to_owned().into());
        root.queue.component_mut().set_focused(true);
        root.update(key(KeyCode::Char('e')));
        root.composer
            .component_mut()
            .replace_draft("revised input".to_owned());
        assert!(
            root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::SUPER
            ))))
            .effects
            .is_empty()
        );
        assert!(root.queue_edit.is_none());
        root.confirm_prompt("request-1");
        let update = root.update(RootEvent::ManagedActiveTurns(0));
        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Submit(prompt)] if prompt.display_text() == "revised input")
        );
        assert_eq!(root.composer.component().draft(), "preserved draft");
    }

    #[test]
    fn connecting_blocks_modified_submit_keys_but_keeps_multiline_editing() {
        for connecting in [RootEvent::AgentConnecting, RootEvent::AgentStreamClosed] {
            let mut root = root_with_draft("preserved");
            root.update(connecting);
            let modified_enter = |modifiers| {
                RootEvent::Terminal(Event::Key(KeyEvent::new(KeyCode::Enter, modifiers)))
            };
            let update = root.update(modified_enter(KeyModifiers::SUPER));
            assert!(update.effects.is_empty());
            assert!(root.update(key(KeyCode::Tab)).effects.is_empty());
            assert_eq!(root.composer.component().draft(), "preserved");
            root.update(modified_enter(KeyModifiers::SHIFT));
            root.update(modified_enter(KeyModifiers::ALT));
            assert_eq!(root.composer.component().draft(), "preserved\n\n");
            root.update(RootEvent::AgentReconnectFailed("offline".to_owned()));
            let update = root.update(modified_enter(KeyModifiers::SUPER));
            assert!(matches!(update.effects.as_slice(), [RootEffect::Reconnect]));
            assert_eq!(root.composer.component().draft(), "preserved\n\n");
        }
    }

    #[test]
    fn offline_draft_restore_preserves_images_and_cursor() {
        use nanocodex::agent::input::{PromptInput, UserInput};
        for connecting in [true, false] {
            let mut root = root_with_draft("inspect 短 ");
            root.update(RootEvent::PasteImage(
                "data:image/png;base64,attached".to_owned(),
            ));
            root.update(key(KeyCode::Home));
            root.update(key(KeyCode::Right));
            let text = root.composer.component().draft().to_owned();
            let cursor = root.composer.component().cursor();
            root.update(RootEvent::AgentStreamClosed);
            if !connecting {
                root.update(RootEvent::AgentReconnectFailed("offline".to_owned()));
            }
            let control = |letter| {
                RootEvent::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::Char(letter),
                    KeyModifiers::CONTROL,
                )))
            };
            assert!(root.update(control('c')).effects.is_empty());
            assert!(root.composer.component().draft().is_empty());
            assert!(root.update(control('z')).effects.is_empty());
            assert_eq!(root.composer.component().draft(), text);
            assert_eq!(root.composer.component().cursor(), cursor);
            assert_eq!(root.reconnecting, Some(connecting));
            let submission = root.composer.component_mut().take_submission().unwrap();
            let PromptInput::Content(content) = submission.agent_prompt().instruction else {
                panic!("offline draft restoration must retain its attachment");
            };
            assert!(content.iter().any(|item| matches!(item, UserInput::Image { image_url, .. } if image_url.ends_with("attached"))));
        }
    }

    #[test]
    fn open_actions_menu_tracks_current_activity() {
        use crate::tui::theme::Theme;
        use ratatui::{Terminal, backend::TestBackend};
        for starts_active in [false, true] {
            let mut root = root_with_draft("");
            root.update(RootEvent::ManagedActiveTurns(usize::from(starts_active)));
            root.update(key(KeyCode::Char('/')));
            root.update(RootEvent::Terminal(Event::Paste("restore".to_owned())));
            root.update(RootEvent::ManagedActiveTurns(usize::from(!starts_active)));
            let mut terminal = Terminal::new(TestBackend::new(160, 32)).unwrap();
            terminal
                .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
                .unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert_eq!(
                screen.contains("Resume session · finish"),
                !starts_active,
                "{screen}"
            );
            let update = root.update(key(KeyCode::Enter));
            if starts_active {
                assert!(matches!(
                    update.effects.as_slice(),
                    [RootEffect::LoadSessions { .. }]
                ));
            } else {
                assert!(update.effects.is_empty());
                assert!(matches!(root.overlay, Some(super::Overlay::Actions(_))));
            }
        }
    }

    #[test]
    fn content_results_are_scoped_to_the_picker_that_requested_them() {
        use super::RenderRequest;
        let mut root = root_with_draft("");
        root.load_sessions();
        let picker_id = root.pending_session_list.unwrap();
        root.sessions_loaded(picker_id, Vec::new());
        let update = root.update(key(KeyCode::Char('x')));
        assert!(
            matches!(update.effects.as_slice(), [RootEffect::SearchSessions { picker_id: id, request_id: 1, query }] if *id == picker_id && query == "x")
        );
        let update = root.update(RootEvent::SessionSearchResults {
            picker_id: picker_id + 1,
            request_id: 1,
            query: "x".into(),
            result: Ok(Vec::new()),
        });
        assert!(matches!(update.render, RenderRequest::None));
        let update = root.update(RootEvent::SessionSearchResults {
            picker_id,
            request_id: 1,
            query: "x".into(),
            result: Ok(Vec::new()),
        });
        assert!(matches!(update.render, RenderRequest::Immediate));
        root.update(key(KeyCode::Esc));
        let update = root.update(RootEvent::SessionSearchResults {
            picker_id,
            request_id: 1,
            query: "x".into(),
            result: Ok(Vec::new()),
        });
        assert!(matches!(update.render, RenderRequest::None));
    }

    #[test]
    fn resuming_a_session_keeps_input_paused_during_background_updates() {
        use crate::tui::{session::SessionSummary, theme::Theme};
        use ratatui::{Terminal, backend::TestBackend};
        for outcome in ["failure", "success", "escape", "control-c"] {
            let mut root = root_with_draft("preserve the old draft");
            root.load_sessions();
            root.sessions_loaded(
                root.pending_session_list.unwrap(),
                vec![SessionSummary {
                    session_id: "selected-agent".to_owned(),
                    updated_at_unix_ms: 0,
                    model: "gpt-6-astra".to_owned(),
                    effort: ReasoningEffort::Low,
                    reasoning_mode: ReasoningMode::Standard,
                    workspace: root.workspace.clone(),
                    preview: "selected session".to_owned(),
                }],
            );
            assert!(
                matches!(root.update(key(KeyCode::Enter)).effects.as_slice(),
            [RootEffect::ResumeSession(id)] if id == "selected-agent")
            );
            for event in [
                RootEvent::SettingsHydrated {
                    effort: ReasoningEffort::High,
                    fast_mode: false,
                    model: Model::Astra,
                },
                RootEvent::ManagedActiveTurns(0),
                RootEvent::ManagedActiveTurns(1),
            ] {
                assert!(root.update(event).effects.is_empty());
                assert!(!root.interactive);
                assert!(root.update(key(KeyCode::Char('!'))).effects.is_empty());
                assert!(root.update(key(KeyCode::Enter)).effects.is_empty());
                assert_eq!(root.composer.component().draft(), "preserve the old draft");
                let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
                terminal
                    .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
                    .unwrap();
                let screen: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(screen.contains("Resuming session"), "{screen}");
            }
            if outcome == "success" {
                root.update(RootEvent::SessionRestored {
                    draft_reset: super::DraftReset::Clear,
                    projection: Box::new(RootNode::project_open_session(
                        ReasoningEffort::Low,
                        Vec::new(),
                    )),
                    effort: ReasoningEffort::Low,
                    reasoning_mode: ReasoningMode::Standard,
                    preferred_reasoning_mode: ReasoningMode::Standard,
                    fast_mode: false,
                    model: Model::Astra,
                    skills: Arc::from([]),
                });
                assert!(!root.resuming_session);
                assert!(
                    !root.has_active_turns(),
                    "old-session activity must not survive a successful resume"
                );
                assert!(root.composer.component().draft().is_empty());
                root.composer
                    .component_mut()
                    .replace_draft("preserve the old draft".to_owned());
            } else {
                if outcome == "failure" {
                    root.update(RootEvent::SessionLoadFailed("resume failed".to_owned()));
                } else {
                    let cancel = if outcome == "escape" {
                        key(KeyCode::Esc)
                    } else {
                        RootEvent::Terminal(Event::Key(KeyEvent::new(
                            KeyCode::Char('c'),
                            KeyModifiers::CONTROL,
                        )))
                    };
                    assert!(matches!(
                        root.update(cancel).effects.as_slice(),
                        [RootEffect::CancelSessionResume]
                    ));
                    assert!(!root.resuming_session);
                    assert!(root.key_confirmation.is_none());
                    assert_eq!(root.composer.component().draft(), "preserve the old draft");
                }
                assert!(root.has_active_turns());
                root.update(RootEvent::ManagedActiveTurns(0));
            }
            assert!(root.interactive);
            assert!(
                matches!(root.update(key(KeyCode::Enter)).effects.as_slice(),
            [RootEffect::Submit(prompt)] if prompt.display_text() == "preserve the old draft")
            );
        }
    }

    #[test]
    fn local_shell_interruption_stays_available_while_disconnected() {
        let mut root = root_with_draft("!sleep 30");
        assert!(matches!(
            root.update(key(KeyCode::Enter)).effects.as_slice(),
            [RootEffect::RunShell(_)]
        ));
        root.update(RootEvent::AgentStreamClosed);
        root.update(RootEvent::AgentReconnectFailed("offline".to_owned()));
        root.transcript.component_mut().focus_expandables();
        assert!(root.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(!root.transcript.component().expandables_focused());
        assert!(root.key_confirmation.is_none());
        assert!(root.update(key(KeyCode::Esc)).effects.is_empty());
        assert!(
            matches!(
                root.update(key(KeyCode::Esc)).effects.as_slice(),
                [RootEffect::CancelTurns]
            ),
            "disconnecting the managed service must not disable local shell cancellation"
        );
    }

    #[test]
    fn dragging_below_the_draft_finishes_copy_and_releases_the_composer() {
        use crate::tui::theme::Theme;
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{Terminal, backend::TestBackend};
        for (beyond_area, clear) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut root = root_with_draft("copy 短 this");
            root.update(RootEvent::ManagedActiveTurns(1));
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
                .unwrap();
            let area = root.composer_content_area;
            assert!(area.height > 1);
            let mouse = |kind, row| {
                RootEvent::Terminal(Event::Mouse(MouseEvent {
                    kind,
                    column: area.x,
                    row,
                    modifiers: KeyModifiers::NONE,
                }))
            };
            root.update(mouse(MouseEventKind::Down(MouseButton::Left), area.y));
            let row = if beyond_area {
                area.bottom() + 5
            } else {
                area.y + 1
            };
            root.update(mouse(MouseEventKind::Drag(MouseButton::Left), row));
            if clear {
                root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                ))));
                assert!(root.composer.component().draft().is_empty());
            }
            let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), row));
            if clear {
                assert!(
                    update.effects.is_empty(),
                    "a cleared draft must not copy stale text"
                );
                root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::Char('z'),
                    KeyModifiers::CONTROL,
                ))));
            } else {
                assert!(
                    matches!(update.effects.as_slice(), [RootEffect::Copy(text)] if text == "copy 短 this"),
                    "releasing below the last text line must finish the copy"
                );
            }
            assert_eq!(root.composer.component().draft(), "copy 短 this");
            assert!(root.selection.surface().is_none());
            assert!(root.selection_auto_scroll.is_none());
            root.update(key(KeyCode::Char('!')));
            let update = root.update(key(KeyCode::Enter));
            assert!(
                matches!(update.effects.as_slice(), [RootEffect::Steer { prompt, .. }] if prompt.display_text() == "copy 短 this!")
            );
        }
    }

    #[test]
    fn offline_edits_dismiss_pending_and_open_session_pickers() {
        for loaded in [false, true] {
            let mut root = root_with_draft("original long draft @@");
            root.update(RootEvent::ManagedActiveTurns(1));
            root.load_session_mentions("original long draft ".len());
            let request_id = root.pending_session_list.unwrap();
            if loaded {
                root.update(RootEvent::SessionsLoaded {
                    request_id,
                    sessions: Vec::new(),
                });
                assert!(root.overlay.is_some());
            }
            root.update(RootEvent::AgentStreamClosed);
            let update = root.update(RootEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Char('u'),
                KeyModifiers::CONTROL,
            ))));
            assert_eq!(
                update
                    .effects
                    .iter()
                    .filter(|effect| matches!(effect, RootEffect::CancelSessionList(_)))
                    .count(),
                usize::from(!loaded)
            );
            root.update(RootEvent::Terminal(Event::Paste("短".to_owned())));
            assert!(root.pending_session_list.is_none());
            assert!(root.pending_session_mention.is_none());
            assert!(root.overlay.is_none());
            root.update(RootEvent::SessionsLoaded {
                request_id,
                sessions: Vec::new(),
            });
            assert!(
                root.overlay.is_none(),
                "late lookup must not cover the edited draft"
            );
            root.update(RootEvent::AgentReconnected {
                active_turns: 1,
                pending_local: false,
                reasoning_mode: ReasoningMode::Standard,
            });
            let update = root.update(key(KeyCode::Enter));
            assert!(
                matches!(update.effects.as_slice(), [RootEffect::Steer { prompt, .. }] if prompt.display_text() == "短")
            );
        }
    }

    #[test]
    fn pasting_an_image_cancels_a_pending_session_mention_without_losing_the_image() {
        use nanocodex::agent::input::{PromptInput, UserInput};
        let mut root = root_with_draft("inspect @@");
        root.update(RootEvent::ManagedActiveTurns(1));
        root.load_session_mentions("inspect ".len());
        let request_id = root.pending_session_list.unwrap();
        let update = root.update(RootEvent::PasteImage(
            "data:image/png;base64,kept".to_owned(),
        ));
        assert!(
            matches!(update.effects.as_slice(), [RootEffect::CancelSessionList(id)] if *id == request_id)
        );
        root.update(RootEvent::SessionsLoaded {
            request_id,
            sessions: Vec::new(),
        });
        assert!(root.overlay.is_none());
        let update = root.update(key(KeyCode::Enter));
        let [RootEffect::Steer { prompt, .. }] = update.effects.as_slice() else {
            panic!("edited prompt should steer");
        };
        let PromptInput::Content(content) = prompt.agent_prompt().instruction else {
            panic!("expected image content");
        };
        assert!(
            matches!(content.as_slice(), [UserInput::Text { text }, UserInput::Image { image_url, .. }]
            if text == "inspect @@" && image_url == "data:image/png;base64,kept")
        );
    }

    #[test]
    fn pending_session_lookup_keeps_its_input_and_status_during_live_updates() {
        use crate::tui::theme::Theme;
        use ratatui::{Terminal, backend::TestBackend};
        let mut root = root_with_draft("preserved @@");
        root.update(RootEvent::ManagedActiveTurns(1));
        root.load_session_mentions("preserved ".len());
        let request_id = root.pending_session_list.unwrap();
        root.update(RootEvent::SettingsHydrated {
            effort: ReasoningEffort::High,
            fast_mode: false,
            model: Model::Astra,
        });
        assert!(
            !root.interactive,
            "settings must not release the pending lookup's input pause"
        );
        root.update(key(KeyCode::Char('u')));
        assert_eq!(root.composer.component().draft(), "preserved @@");
        root.update(RootEvent::ManagedActiveTurns(0));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("Loading sessions") && screen.contains("Esc cancel"),
            "{screen}"
        );
        root.update(RootEvent::SessionsLoaded {
            request_id,
            sessions: Vec::new(),
        });
        assert!(root.interactive);
        root.update(key(KeyCode::Esc));
        root.update(key(KeyCode::Char('x')));
        assert_eq!(root.composer.component().draft(), "preserved @@x");
    }

    #[test]
    fn session_lookup_can_be_cancelled_while_a_turn_is_running() {
        let mut root = root_with_draft("steering draft");
        root.update(RootEvent::ManagedActiveTurns(1));
        root.load_sessions();
        root.update(key(KeyCode::Esc));
        assert!(root.interactive, "Esc must release a slow session lookup");
        assert_eq!(root.composer.component().draft(), "steering draft");
        let update = root.update(key(KeyCode::Enter));
        assert!(
            matches!(update.effects.as_slice(), [RootEffect::Steer { prompt, .. }] if prompt.display_text() == "steering draft")
        );
    }

    #[test]
    fn session_lookup_resolution_releases_ready_followups_without_sending_the_draft() {
        for resolution in 0..3 {
            let mut root = root_with_draft("unfinished steering draft");
            root.update(RootEvent::ManagedActiveTurns(1));
            root.queue
                .component_mut()
                .push("queued after lookup".to_owned());
            root.load_session_mentions(0);
            let request_id = root.pending_session_list.unwrap();
            assert!(
                root.update(RootEvent::ManagedActiveTurns(0))
                    .effects
                    .is_empty()
            );
            let event = match resolution {
                0 => key(KeyCode::Esc),
                1 => RootEvent::SessionsLoaded {
                    request_id,
                    sessions: Vec::new(),
                },
                _ => RootEvent::SessionListFailed {
                    request_id,
                    error: "lookup failed".to_owned(),
                },
            };
            let update = root.update(event);
            let submitted: Vec<_> = update
                .effects
                .iter()
                .filter_map(|effect| match effect {
                    RootEffect::Submit(prompt) => Some(prompt.display_text()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                submitted,
                ["queued after lookup"],
                "ready followup stuck after lookup resolution {resolution}"
            );
            assert_eq!(
                root.composer.component().draft(),
                "unfinished steering draft"
            );
            assert!(root.queue.component().is_empty());
        }
    }

    #[test]
    fn cancelled_session_results_cannot_replace_a_newer_lookup() {
        let mut root = root_with_draft("preserved draft");
        root.load_session_mentions(0);
        let first = root.pending_session_list.unwrap();
        root.update(key(KeyCode::Esc));
        root.reset_session(
            Path::new("/workspace"),
            ReasoningEffort::Medium,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            super::DraftReset::Preserve,
        );
        root.load_sessions();
        let second = root.pending_session_list.unwrap();
        assert_ne!(first, second, "session reset must not reuse lookup IDs");
        for event in [
            RootEvent::SessionsLoaded {
                request_id: first,
                sessions: Vec::new(),
            },
            RootEvent::SessionListFailed {
                request_id: first,
                error: "late lookup failure".to_owned(),
            },
        ] {
            assert!(root.update(event).effects.is_empty());
            assert_eq!(root.pending_session_list, Some(second));
            assert!(!root.interactive);
            assert!(root.overlay.is_none());
        }
        root.update(RootEvent::SessionsLoaded {
            request_id: second,
            sessions: Vec::new(),
        });
        assert!(root.interactive);
        assert!(root.overlay.is_some());
        assert_eq!(root.composer.component().draft(), "preserved draft");
    }

    #[test]
    fn loading_callbacks_restore_current_activity_without_losing_drafts() {
        use crate::tui::theme::Theme;
        use ratatui::{Terminal, backend::TestBackend};

        let callbacks: [fn() -> RootEvent; 6] = [
            || RootEvent::RecentPromptsLoaded {
                session_id: "session".to_owned(),
                prompts: Vec::new(),
            },
            || RootEvent::RecentPromptLoadFailed("lookup failed".to_owned()),
            || RootEvent::SessionsLoaded {
                request_id: 0,
                sessions: Vec::new(),
            },
            || RootEvent::SessionLoadFailed("lookup failed".to_owned()),
            || RootEvent::NewSessionFailed("still active".to_owned()),
            || RootEvent::SettingsHydrated {
                effort: ReasoningEffort::High,
                fast_mode: false,
                model: Model::Astra,
            },
        ];
        for state in 0..4 {
            for callback in callbacks {
                let mut root = root_with_draft("preserved input");
                root.update(RootEvent::ManagedActiveTurns(usize::from(state != 0)));
                let record = TranscriptRecord::from_agent(
                    1,
                    1,
                    AgentEvent {
                        protocol_version: 1,
                        request_id: Arc::from("agent"),
                        seq: 1,
                        kind: AgentEventKind::ModelWarmupStarted,
                        payload: to_raw_value(&json!({})).unwrap().into(),
                    },
                );
                root.update(RootEvent::ExternalTranscript(Arc::new(record)));
                if state >= 2 {
                    root.update(RootEvent::AgentStreamClosed);
                }
                if state == 3 {
                    root.update(RootEvent::AgentReconnectFailed("offline".to_owned()));
                }
                let event = callback();
                if matches!(event, RootEvent::SessionsLoaded { .. }) {
                    root.pending_session_list = Some(0);
                }
                assert!(root.update(event).effects.is_empty());
                root.overlay = None;
                assert_eq!(root.composer.component().draft(), "preserved input");
                assert_eq!(root.interactive, state < 2);
                let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
                terminal
                    .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
                    .unwrap();
                let screen: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                match state {
                    0 => assert!(
                        !screen.contains("Warming model") && !screen.contains("Thinking"),
                        "{screen}"
                    ),
                    1 => assert!(screen.contains("Warming model"), "{screen}"),
                    2 => assert!(screen.contains("Reconnecting"), "{screen}"),
                    _ => assert!(screen.contains("Connection lost"), "{screen}"),
                }
            }
        }
    }

    #[test]
    fn history_and_settings_updates_preserve_the_connection_status() {
        use crate::tui::theme::Theme;
        use ratatui::{Terminal, backend::TestBackend};

        for failed in [false, true] {
            let mut root = root_with_draft("pending");
            root.update(key(KeyCode::Enter));
            root.update(RootEvent::AgentStreamClosed);
            if failed {
                root.update(RootEvent::AgentReconnectFailed("offline".to_owned()));
            }
            root.update(RootEvent::SettingsHydrated {
                effort: ReasoningEffort::High,
                fast_mode: false,
                model: Model::Astra,
            });
            let record = TranscriptRecord::from_agent(
                1,
                1,
                AgentEvent {
                    protocol_version: 1,
                    request_id: Arc::from("managed-agent"),
                    seq: 1,
                    kind: AgentEventKind::RunFailed,
                    payload: to_raw_value(&json!({"error": "missed failure"}))
                        .unwrap()
                        .into(),
                },
            );
            root.update(RootEvent::ExternalTranscript(Arc::new(record)));
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| root.render_focused(frame, frame.area(), &Theme::default(), true))
                .unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                screen.contains(if failed {
                    "Connection lost"
                } else {
                    "Reconnecting"
                }),
                "{screen}"
            );
            assert!(!root.interactive);
        }
    }

    #[test]
    fn closed_managed_stream_blocks_submission_without_promoting_the_queue() {
        let mut root = root_with_draft("do not submit");
        root.queue.component_mut().push("still queued".to_owned());
        let _ = root.update(RootEvent::ManagedActiveTurns(1));

        let disconnected = root.update(RootEvent::AgentStreamClosed);
        let enter = root.update(key(KeyCode::Enter));

        assert!(disconnected.effects.is_empty());
        assert!(enter.effects.is_empty());
        assert!(!root.interactive);
        assert_eq!(root.managed_active_turns, 0);
        assert_eq!(root.queue.component().len(), 1);
        assert_eq!(root.composer.component().draft(), "do not submit");
    }

    #[test]
    fn closed_managed_stream_preserves_local_activity_state() {
        let mut root = root_with_draft("");
        root.in_flight_turns = 1;
        let _ = root.update(RootEvent::ManagedActiveTurns(1));

        let update = root.update(RootEvent::AgentStreamClosed);

        assert!(update.effects.is_empty());
        assert!(!root.interactive);
        assert_eq!(root.managed_active_turns, 0);
        assert!(root.has_active_turns());
        assert_eq!(root.in_flight_turns, 1);
    }
}
