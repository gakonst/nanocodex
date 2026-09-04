// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Root layout and component event routing.

use super::{
    actions::{Action, ActionAvailability, ActionsEffect, ActionsEvent, ActionsMenu},
    composer::{Composer, ComposerChromeTarget, ComposerDraft, ComposerEffect, ComposerEvent},
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
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
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
    Terminal(Event),
    PasteImage(String),
    #[cfg(test)]
    ContextTokens(u64),
    Transcript(Arc<TranscriptRecord>),
    ExternalTranscript(Arc<TranscriptRecord>),
    AgentStreamClosed,
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
    ManagedActiveTurns(usize),
    ShellFinished,
    TurnsCancelled,
    ForkReady,
    NewSessionFailed(String),
    SessionsLoaded(Vec<SessionSummary>),
    RecentPromptsLoaded {
        session_id: String,
        prompts: Vec<RecentPrompt>,
    },
    RecentPromptLoadFailed(String),
    SessionLoadFailed(String),
    SessionRestored {
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
    SteerPromoted(QueueId),
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
    Submit(Submission),
    Reflect(Submission),
    RunShell(String),
    ContinueSubagent(Submission),
    OpenDraftEditor,
    OpenConfigEditor,
    OpenLink(String),
    ReloadConfig,
    NewSession(Model),
    LoadSessions(SessionListKind),
    LoadRecentPrompts(Vec<RecentPromptDraft>),
    LoadOlderHistory,
    ResumeSession(String),
    Steer {
        id: QueueId,
        prompt: Submission,
    },
    PersistSteer(String),
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
    discarded_draft: Option<ComposerDraft>,
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
    theme_mode: ThemeMode,
    preferred_reasoning_mode: ReasoningMode,
    subagents: SubagentTree,
    context_diagnostics: ContextDiagnostics,
    recent_prompts: Vec<RecentPromptDraft>,
    pending_session_mention: Option<usize>,
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
            discarded_draft: None,
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
            theme_mode: ThemeMode::Auto,
            preferred_reasoning_mode: ReasoningMode::Standard,
            subagents,
            context_diagnostics: ContextDiagnostics::default(),
            recent_prompts: Vec::new(),
            pending_session_mention: None,
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
        let can_fork = self.can_fork();
        if let Some(Overlay::Actions(actions)) = &mut self.overlay {
            actions.component_mut().set_fork_available(can_fork);
        }
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
        let replaced_draft = current_draft.is_some() && matches!(draft_reset, DraftReset::Clear);
        let (preserved_draft, discarded_draft) = match draft_reset {
            DraftReset::Clear => (None, current_draft.or(previous_discarded_draft)),
            DraftReset::Preserve => (current_draft, previous_discarded_draft),
        };
        let fork_available = self.fork_available;
        let theme_mode = self.theme_mode;
        let max_subagents = self.subagents.max_subagents();
        *self = Self::new(workspace, thinking);
        self.set_reasoning_modes(reasoning_mode, preferred_reasoning_mode);
        self.discarded_draft = discarded_draft;
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
        let mut transcript = Transcript::with_effort(thinking);
        let mut context_diagnostics = ContextDiagnostics::default();
        let mut context_tokens = None;
        let mut recent_prompts = Vec::new();
        for record in records {
            if let Some(prompt) = recent_prompt(&record) {
                recent_prompts.push(prompt);
            }
            let observation = context_diagnostics.observe(&record);
            if observation.completed_tokens.is_some() {
                context_tokens = observation.completed_tokens;
            }
            let _ = transcript.update(TranscriptEvent::Record(record));
        }
        if stream_closed {
            let _ = transcript.update(TranscriptEvent::AgentStreamClosed);
        }
        RestoredSessionProjection {
            transcript,
            context_diagnostics,
            context_tokens,
            recent_prompts,
        }
    }

    fn replay_history(&mut self, mut projection: RestoredSessionProjection) {
        projection
            .transcript
            .preserve_viewport_from(self.transcript.component());
        projection.transcript.set_workspace(&self.workspace);
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
        let preserve_active_submission = self.has_active_turns()
            || !self.queue.component().is_empty()
            || self.queue.component().has_pending_steer();
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
        self.transcript = Node::new(projection.transcript);
        self.context_diagnostics = projection.context_diagnostics;
        self.recent_prompts = projection.recent_prompts;
        if let Some(tokens) = projection.context_tokens {
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::ContextTokens(tokens));
        }
        self.thread = ThreadState::Started;
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
        let queue_height = self
            .queue
            .component()
            .desired_height()
            .min(area.height.saturating_sub(height));
        let queue_width = area.width.saturating_mul(95) / 100;
        let queue_area = Rect {
            x: area.x + area.width.saturating_sub(queue_width) / 2,
            y: composer_area.y.saturating_sub(queue_height),
            width: queue_width,
            height: queue_height,
        };
        self.queue_area = queue_area;
        let transcript_area = Rect {
            height: area
                .height
                .saturating_sub(height)
                .saturating_sub(queue_height),
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
        if matches!(event, Event::Resize(_, _)) {
            self.selection.clear();
            self.selection_auto_scroll = None;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if is_confirmation_key_repeat(&event) {
            return ComponentUpdate::none();
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
        if !self.interactive {
            return ComponentUpdate::none();
        }
        if self.queue_edit.is_some() {
            return self.update_queue_editor(event);
        }
        if self.reflection_input && is_plain_enter(&event) {
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
            if self.has_active_turns() {
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
            let new_session_enabled = !self.has_active_turns()
                && self.in_flight_shells == 0
                && self.blocking_task.is_none()
                && self.queue.component().is_empty();
            self.overlay = Some(Overlay::Actions(Node::new(ActionsMenu::new(
                ActionAvailability {
                    new_session: new_session_enabled,
                    fork: self.can_fork(),
                    fast_mode: self.composer.component().fast_mode(),
                    model: self.thread == ThreadState::New,
                },
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
                let span = self.selection_span_on(surface, position)?;
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

    fn update_actions(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Actions(actions)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = actions.update(ActionsEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(ActionsEffect::Dismiss) => self.overlay = None,
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
            return ComponentUpdate::none();
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
        self.overlay = None;
        self.pending_session_mention = None;
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Loading sessions…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::LoadSessions(SessionListKind::Resume)],
            render: RenderRequest::Immediate,
        }
    }

    fn load_session_mentions(&mut self, start: usize) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        self.pending_session_mention = Some(start);
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Loading sessions…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::LoadSessions(SessionListKind::Mention)],
            render: RenderRequest::Immediate,
        }
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
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
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
        self.interactive = true;
        self.notification = Some(Notification::plain(message, Color::Red));
        self.update_composer(
            ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            },
            RenderRequest::Immediate,
        )
    }

    fn sessions_loaded(&mut self, sessions: Vec<SessionSummary>) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        let mode = if self.pending_session_mention.is_some() {
            SessionPickerMode::Mention
        } else {
            SessionPickerMode::Resume
        };
        self.overlay = Some(Overlay::Sessions(Node::new(SessionPicker::new(
            sessions, mode,
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_session_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Sessions(picker)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = picker.update(SessionPickerEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(SessionPickerEffect::Dismiss) => {
                self.overlay = None;
                self.pending_session_mention = None;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            Some(SessionPickerEffect::Resume(session_id)) => {
                self.overlay = None;
                self.interactive = false;
                let _ = self
                    .composer
                    .component_mut()
                    .update(ComposerEvent::Activity {
                        active: true,
                        status: Some("Resuming session…".to_owned()),
                        now: Instant::now(),
                    });
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
                self.update_composer(
                    ComposerEvent::ReplaceRange {
                        range: start..self.composer.component().cursor(),
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
        self.pending_session_mention = None;
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        self.notification = Some(Notification::plain(message, Color::Red));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn new_session_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
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
            EffortEffect::Apply(effort, pro) => {
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
            ModelSelectorEffect::Apply(model) if model == self.composer.component().model() => {
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            ModelSelectorEffect::Apply(model) => {
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
                QueueEffect::Edit { id, text } => {
                    let edit = self.begin_queue_edit(id, text);
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

    fn begin_queue_edit(&mut self, id: QueueId, text: String) -> ComponentUpdate<RootEffect> {
        let original_input_mode = self
            .composer
            .component()
            .input_mode()
            .map(ToOwned::to_owned);
        let original_draft = self.composer.component_mut().take_draft();
        self.composer.component_mut().replace_draft(text);
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
        if is_plain_enter(&event) {
            return self.finish_queue_edit(true);
        }
        self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate)
    }

    fn finish_queue_edit(&mut self, save: bool) -> ComponentUpdate<RootEffect> {
        let Some(edit) = self.queue_edit.take() else {
            return ComponentUpdate::none();
        };
        let text = save.then(|| self.composer.component().draft().to_owned());
        self.composer.component_mut().replace_draft(String::new());
        if let Some(draft) = edit.original_draft {
            self.composer.component_mut().restore_draft(draft);
        }
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::InputMode(edit.original_input_mode));

        let restored = match text {
            Some(text) => self.queue.component_mut().finish_edit(edit.id, text),
            None => self.queue.component_mut().cancel_edit(edit.id),
        };
        if !restored {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        self.submit_next_queued()
    }

    fn update_composer(
        &mut self,
        event: ComposerEvent,
        priority: RenderRequest,
    ) -> ComponentUpdate<RootEffect> {
        let update = self.composer.component_mut().update(event);
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
            None => Vec::new(),
        };

        if delivered && self.has_active_turns() {
            let activity = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active: true,
                    status: Some("Thinking…".to_owned()),
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
        if !self.composer.component().draft().is_empty() {
            return ComponentUpdate::none();
        }
        let Some(draft) = self.discarded_draft.take() else {
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
        self.queue.component_mut().cancel_steers();
        self.submit_next_queued()
    }

    fn managed_active_turns(&mut self, count: usize) -> ComponentUpdate<RootEffect> {
        self.managed_active_turns = count;
        let active = self.has_active_turns();
        let mut update = if active {
            ComponentUpdate::render(RenderRequest::Immediate)
        } else {
            self.submit_next_queued()
        };
        let activity = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active,
                status: active.then(|| "Thinking…".to_owned()),
                now: Instant::now(),
            });
        if activity.changed {
            update.render = update.render.max(RenderRequest::Immediate);
        }
        update.render = update.render.max(self.sync_live_controls());
        update
    }

    fn agent_stream_closed(&mut self) -> ComponentUpdate<RootEffect> {
        self.managed_active_turns = 0;
        self.interactive = false;
        self.key_confirmation = None;
        let active = self.has_active_turns();
        let mut update = self.update_transcript(TranscriptEvent::AgentStreamClosed);
        let activity = self.update_composer(
            ComposerEvent::Activity {
                active,
                status: active.then(|| "Thinking…".to_owned()),
                now: Instant::now(),
            },
            RenderRequest::Immediate,
        );
        update.effects.extend(activity.effects);
        update.render = update.render.max(activity.render);
        if !active {
            let timers =
                self.update_composer(ComposerEvent::TurnsCleared, RenderRequest::Immediate);
            update.effects.extend(timers.effects);
            update.render = update.render.max(timers.render);
        }
        update.render = update.render.max(self.sync_live_controls());
        update
    }

    fn steer_admitted(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        let applied = self.queue.component_mut().steer_admitted(id);
        self.finish_applied_steer(applied)
    }

    fn steer_promoted(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        let _ = self.queue.component_mut().steer_promoted(id);
        self.in_flight_turns = self.in_flight_turns.saturating_add(1);
        let controls = self.sync_live_controls();
        ComponentUpdate::render(RenderRequest::Immediate.max(controls))
    }

    fn steer_failed(&mut self, id: QueueId) -> ComponentUpdate<RootEffect> {
        self.queue.component_mut().steer_failed(id);
        self.submit_next_queued()
    }

    fn steer_applied(&mut self) -> ComponentUpdate<RootEffect> {
        let applied = self.queue.component_mut().steer_applied();
        self.finish_applied_steer(applied)
    }

    fn finish_applied_steer(&mut self, applied: Option<Submission>) -> ComponentUpdate<RootEffect> {
        let mut update = self.submit_next_queued();
        if let Some(prompt) = applied {
            update.effects.insert(
                0,
                RootEffect::PersistSteer(prompt.display_text().to_owned()),
            );
        }
        update
    }

    fn submit_next_queued(&mut self) -> ComponentUpdate<RootEffect> {
        if !self.interactive
            || self.has_active_turns()
            || self.queue.component().has_pending_steer()
        {
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
        if update.changed {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        }
    }

    const fn has_active_turns(&self) -> bool {
        self.in_flight_turns > 0 || self.managed_active_turns > 0
    }

    fn update_transcript(&mut self, event: TranscriptEvent) -> ComponentUpdate<RootEffect> {
        let update = self.transcript.update(event);
        let mut render = update.render;
        for effect in update.effects {
            let composer = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active: effect.active,
                    status: effect.status,
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

    fn transcript_record(
        &mut self,
        record: Arc<TranscriptRecord>,
        track_local_turn: bool,
    ) -> ComponentUpdate<RootEffect> {
        if let Some(prompt) = recent_prompt(&record) {
            self.recent_prompts.push(prompt);
        }
        let steer_applied = record.kind() == "run.steered";
        let turn_finished =
            track_local_turn && matches!(record.kind(), "run.completed" | "run.failed");
        let turn_timer = turn_timer_event(&record);
        let observation = self.context_diagnostics.observe(&record);
        if let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay {
            panel
                .component_mut()
                .replace(self.context_diagnostics.clone());
        }
        let mut update = self.update_transcript(TranscriptEvent::Record(record));
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
        if steer_applied {
            let applied = self.steer_applied();
            update.effects.extend(applied.effects);
            update.render = update.render.max(applied.render);
        }
        if turn_finished {
            let finished = self.agent_turn_finished();
            update.effects.extend(finished.effects);
            update.render = update.render.max(finished.render);
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
                    || self.queue.component().focused()
                {
                    ComponentUpdate::none()
                } else {
                    self.update_composer(
                        ComposerEvent::PasteImage(data_url),
                        RenderRequest::Immediate,
                    )
                }
            }
            #[cfg(test)]
            RootEvent::ContextTokens(tokens) => self.update_composer(
                ComposerEvent::ContextTokens(tokens),
                RenderRequest::Streaming,
            ),
            RootEvent::Transcript(record) => self.transcript_record(record, true),
            RootEvent::ExternalTranscript(record) => self.transcript_record(record, false),
            RootEvent::AgentStreamClosed => self.agent_stream_closed(),
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
            RootEvent::ManagedActiveTurns(count) => self.managed_active_turns(count),
            RootEvent::ShellFinished => {
                self.in_flight_shells = self.in_flight_shells.saturating_sub(1);
                ComponentUpdate::none()
            }
            RootEvent::TurnsCancelled => self.turns_cancelled(),
            RootEvent::ForkReady => self.fork_ready(),
            RootEvent::NewSessionFailed(message) => self.new_session_failed(message),
            RootEvent::SessionsLoaded(sessions) => self.sessions_loaded(sessions),
            RootEvent::RecentPromptsLoaded {
                session_id,
                prompts,
            } => self.recent_prompts_loaded(session_id, prompts),
            RootEvent::RecentPromptLoadFailed(message) => self.recent_prompt_load_failed(message),
            RootEvent::SessionLoadFailed(message) => self.session_load_failed(message),
            RootEvent::SessionRestored {
                projection,
                effort,
                reasoning_mode,
                preferred_reasoning_mode,
                fast_mode,
                model,
                skills,
            } => {
                let workspace = self.workspace.clone();
                self.install_session_projection(
                    &workspace,
                    effort,
                    reasoning_mode,
                    preferred_reasoning_mode,
                    fast_mode,
                    *projection,
                );
                self.set_model(model);
                self.set_skills(skills);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::SettingsHydrated {
                effort,
                fast_mode,
                model,
            } => {
                self.interactive = true;
                self.transcript.component_mut().set_effort(effort);
                self.subagents.set_effort(effort);
                let _ = self
                    .composer
                    .component_mut()
                    .update(ComposerEvent::SetEffort(effort));
                let _ = self
                    .composer
                    .component_mut()
                    .update(ComposerEvent::Activity {
                        active: false,
                        status: None,
                        now: Instant::now(),
                    });
                self.set_fast_mode(fast_mode);
                self.set_model(model);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::HistoryReplayed { projection } => {
                self.replay_history(*projection);
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
            RootEvent::SteerPromoted(id) => self.steer_promoted(id),
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
        text: prompt.text,
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
    use crate::config::ReasoningEffort;
    use crate::tui::transcript::TranscriptRecord;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use nanocodex::agent::events::{AgentEvent, AgentEventKind};
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
