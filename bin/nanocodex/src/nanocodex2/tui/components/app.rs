// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Application-level ownership for the primary and optional forked panes.

use super::{
    node::{ComponentUpdate, Node, RenderRequest},
    queue::QueueId,
    root::{DraftReset, RestoredSessionProjection, RootEffect, RootEvent, RootNode},
};
use crate::{
    config::{ReasoningEffort, ReasoningMode},
    skill::Skill,
    tui::{
        pane::PaneId,
        session::{RecentPrompt, SessionSummary},
        theme::{ColorScheme, Theme, ThemeMode},
        transcript::TranscriptRecord,
    },
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use nanocodex::Model;
use nanocodex_subagents::AgentUpdate;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders},
};
use semver::Version;
use std::{path::PathBuf, sync::Arc, time::Instant};
use unicode_width::UnicodeWidthStr;

const SPLIT_HINT: &str = " mouse: focus · Ctrl+C: clear · Ctrl+C×2: close ";
const MIN_SPLIT_HINT_WIDTH: u16 = 60;

pub(crate) enum AppEvent {
    Screen(crate::tui::screen::Snapshot),
    Terminal(Event),
    PasteImage(String),
    Transcript {
        pane: PaneId,
        record: Arc<TranscriptRecord>,
    },
    ExternalTranscript {
        pane: PaneId,
        record: Arc<TranscriptRecord>,
    },
    AgentStreamClosed(PaneId),
    AgentConnecting(PaneId),
    RetainPrompt {
        pane: PaneId,
        request_id: String,
        prompt: crate::tui::prompt::Submission,
    },
    PromptConfirmed {
        pane: PaneId,
        request_id: String,
    },
    AgentReconnected {
        pane: PaneId,
        active_turns: usize,
        pending_local: bool,
        reasoning_mode: ReasoningMode,
    },
    AgentReconnectFailed {
        pane: PaneId,
        error: String,
    },
    Subagent {
        pane: PaneId,
        update: AgentUpdate,
    },
    EditorDraft {
        pane: PaneId,
        draft: String,
    },
    ReviewFinished {
        pane: PaneId,
        markdown: String,
    },
    ReviewStarted(PaneId),
    ReviewReady {
        pane: PaneId,
        url: String,
    },
    ReviewCancelled(PaneId),
    ReviewFailed {
        pane: PaneId,
        error: String,
    },
    HandoffReady {
        pane: PaneId,
        prompt: String,
        effort: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        fast_mode: bool,
        model: Model,
        skills: Arc<[Skill]>,
    },
    HandoffCancelled(PaneId),
    HandoffFailed {
        pane: PaneId,
        error: String,
    },
    WorkerTurnFinished {
        pane: PaneId,
        terminal_expected: bool,
    },
    ManagedTurnFinished(PaneId),
    ManagedActiveTurns {
        pane: PaneId,
        count: usize,
    },
    ShellFinished(PaneId),
    TurnsCancelled(PaneId),
    SteerAdmitted {
        pane: PaneId,
        id: QueueId,
    },
    SteerWithdrawn {
        pane: PaneId,
        id: QueueId,
    },
    SteerWithdrawalFailed {
        pane: PaneId,
        id: QueueId,
        error: String,
    },
    SteerUnconfirmed {
        pane: PaneId,
        id: QueueId,
    },
    SteerFailed {
        pane: PaneId,
        id: QueueId,
    },
    ForkReady {
        pane: PaneId,
    },
    ForkFailed {
        pane: PaneId,
        error: String,
    },
    NewSessionReady {
        pane: PaneId,
        effort: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        fast_mode: bool,
        model: Model,
        draft_reset: DraftReset,
        skills: Arc<[Skill]>,
    },
    NewSessionFailed {
        pane: PaneId,
        error: String,
    },
    SessionSearchResults {
        pane: PaneId,
        picker_id: u64,
        request_id: u64,
        query: String,
        result: Result<Vec<nanocodex_managed::SessionSearchHit>, String>,
    },
    SessionsLoaded {
        pane: PaneId,
        request_id: u64,
        sessions: Vec<SessionSummary>,
    },
    SessionListFailed {
        pane: PaneId,
        request_id: u64,
        error: String,
    },
    RecentPromptsLoaded {
        pane: PaneId,
        session_id: String,
        prompts: Vec<RecentPrompt>,
    },
    RecentPromptLoadFailed {
        pane: PaneId,
        error: String,
    },
    SessionLoadFailed {
        pane: PaneId,
        error: String,
    },
    SessionRestored {
        pane: PaneId,
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
        pane: PaneId,
        effort: ReasoningEffort,
        fast_mode: bool,
        model: Model,
    },
    HistoryReplayed {
        pane: PaneId,
        projection: Box<RestoredSessionProjection>,
    },
    VaultReceipt {
        pane: PaneId,
        receipt: String,
    },
    VaultReview {
        pane: PaneId,
        review: crate::tui::vault::Review,
    },
    ShowAgentId {
        pane: PaneId,
        id: String,
    },
    VoiceStatus(Option<crate::voice_state::Status>),
    NotifyError {
        pane: PaneId,
        error: String,
    },
    NotifySuccess {
        pane: PaneId,
        message: String,
    },
    ConfirmReviewDownload {
        pane: PaneId,
    },
    UpdateAvailable(Version),
    ConfigReloaded {
        pane: PaneId,
        theme: Theme,
        preferred_reasoning_mode: ReasoningMode,
        message: String,
    },
    ConfigReloadFailed {
        pane: PaneId,
        error: String,
    },
    SystemThemeChanged(ColorScheme),
    AnimationFrame(Instant),
}

pub(crate) enum AppEffect {
    Screen(crate::tui::screen::Command),
    Pane { pane: PaneId, effect: RootEffect },
    OpenFork { pane: PaneId, parent: PaneId },
    ClosePane(PaneId),
    SetTheme(ThemeMode),
    Shutdown,
}

pub(crate) struct AppNode {
    screen: Option<super::screen::ScreenPane>,
    screen_focused: bool,
    screen_area: Rect,
    zoomed: bool,
    theme: Theme,
    workspace: PathBuf,
    main: Option<(PaneId, Node<RootNode>)>,
    fork: Option<(PaneId, Node<RootNode>)>,
    focus: PaneId,
    main_area: Rect,
    fork_area: Rect,
    next_fork: u64,
}

impl AppNode {
    pub(crate) fn new(theme: Theme, workspace: PathBuf, mut root: RootNode) -> Self {
        root.set_theme_mode(theme.mode());
        Self {
            screen: None,
            screen_focused: false,
            screen_area: Rect::default(),
            zoomed: false,
            theme,
            workspace,
            main: Some((PaneId::Main, Node::new(root))),
            fork: None,
            focus: PaneId::Main,
            main_area: Rect::default(),
            fork_area: Rect::default(),
            next_fork: 1,
        }
    }

    pub(crate) fn open_resume_selector(&mut self) -> ComponentUpdate<AppEffect> {
        let Some((pane, root)) = self.main.as_mut() else {
            return ComponentUpdate::none();
        };
        let pane = *pane;
        let update = root.component_mut().load_sessions();
        self.map_root_update(pane, update)
    }

    pub(crate) fn update(&mut self, event: AppEvent) -> ComponentUpdate<AppEffect> {
        match event {
            AppEvent::Screen(snapshot) => {
                if let Some(screen) = &mut self.screen {
                    screen.snapshot = snapshot;
                }
                ComponentUpdate::render(RenderRequest::Streaming)
            }
            AppEvent::Terminal(event) => self.update_terminal(event),
            AppEvent::PasteImage(data_url) => {
                if self.screen_focused {
                    ComponentUpdate::none()
                } else {
                    self.update_root(self.focus, RootEvent::PasteImage(data_url))
                }
            }
            AppEvent::Transcript { pane, record } => {
                self.update_root(pane, RootEvent::Transcript(record))
            }
            AppEvent::ExternalTranscript { pane, record } => {
                self.update_root(pane, RootEvent::ExternalTranscript(record))
            }
            AppEvent::RetainPrompt {
                pane,
                request_id,
                prompt,
            } => self.update_root(pane, RootEvent::RetainPrompt { request_id, prompt }),
            AppEvent::PromptConfirmed { pane, request_id } => {
                self.update_root(pane, RootEvent::PromptConfirmed(request_id))
            }
            AppEvent::AgentStreamClosed(pane) => {
                self.update_root(pane, RootEvent::AgentStreamClosed)
            }
            AppEvent::AgentConnecting(pane) => self.update_root(pane, RootEvent::AgentConnecting),
            AppEvent::AgentReconnected {
                pane,
                active_turns,
                pending_local,
                reasoning_mode,
            } => self.update_root(
                pane,
                RootEvent::AgentReconnected {
                    active_turns,
                    pending_local,
                    reasoning_mode,
                },
            ),
            AppEvent::AgentReconnectFailed { pane, error } => {
                self.update_root(pane, RootEvent::AgentReconnectFailed(error))
            }
            AppEvent::Subagent { pane, update } => {
                self.update_root(pane, RootEvent::Subagent(update))
            }
            AppEvent::EditorDraft { pane, draft } => {
                self.update_root(pane, RootEvent::ReplaceDraft(draft))
            }
            AppEvent::ReviewFinished { pane, markdown } => {
                self.update_root(pane, RootEvent::ReviewFinished(markdown))
            }
            AppEvent::ReviewStarted(pane) => self.update_root(pane, RootEvent::ReviewStarted),
            AppEvent::ReviewReady { pane, url } => {
                self.update_root(pane, RootEvent::ReviewReady(url))
            }
            AppEvent::ReviewCancelled(pane) => self.update_root(pane, RootEvent::ReviewCancelled),
            AppEvent::ReviewFailed { pane, error } => {
                self.update_root(pane, RootEvent::ReviewFailed(error))
            }
            AppEvent::HandoffReady {
                pane,
                prompt,
                effort,
                reasoning_mode,
                fast_mode,
                model,
                skills,
            } => {
                let workspace = self.workspace.clone();
                {
                    let Some(root) = self.pane_mut(pane) else {
                        return ComponentUpdate::none();
                    };
                    root.component_mut().reset_session(
                        &workspace,
                        effort,
                        reasoning_mode,
                        reasoning_mode,
                        DraftReset::Clear,
                    );
                    root.component_mut().set_fast_mode(fast_mode);
                    root.component_mut().set_model(model);
                    root.component_mut().set_skills(skills);
                }
                self.update_root(pane, RootEvent::HandoffFinished(prompt))
            }
            AppEvent::HandoffCancelled(pane) => self.update_root(pane, RootEvent::HandoffCancelled),
            AppEvent::HandoffFailed { pane, error } => {
                self.update_root(pane, RootEvent::HandoffFailed(error))
            }
            AppEvent::WorkerTurnFinished {
                pane,
                terminal_expected,
            } => self.update_root(pane, RootEvent::WorkerTurnFinished { terminal_expected }),
            AppEvent::ManagedTurnFinished(pane) => {
                self.update_root(pane, RootEvent::ManagedTurnFinished)
            }
            AppEvent::ManagedActiveTurns { pane, count } => {
                self.update_root(pane, RootEvent::ManagedActiveTurns(count))
            }
            AppEvent::ShellFinished(pane) => self.update_root(pane, RootEvent::ShellFinished),
            AppEvent::TurnsCancelled(pane) => self.update_root(pane, RootEvent::TurnsCancelled),
            AppEvent::SteerAdmitted { pane, id } => {
                self.update_root(pane, RootEvent::SteerAdmitted(id))
            }
            AppEvent::SteerWithdrawn { pane, id } => {
                self.update_root(pane, RootEvent::SteerWithdrawn(id))
            }
            AppEvent::SteerWithdrawalFailed { pane, id, error } => {
                self.update_root(pane, RootEvent::SteerWithdrawalFailed { id, error })
            }
            AppEvent::SteerUnconfirmed { pane, id } => {
                self.update_root(pane, RootEvent::SteerUnconfirmed(id))
            }
            AppEvent::SteerFailed { pane, id } => {
                self.update_root(pane, RootEvent::SteerFailed { id })
            }
            AppEvent::ForkReady { pane } => self.update_root(pane, RootEvent::ForkReady),
            AppEvent::ForkFailed { pane, error } => {
                self.remove_pane(pane);
                let Some(target) = self.main_pane() else {
                    return ComponentUpdate {
                        effects: vec![AppEffect::Shutdown],
                        render: RenderRequest::Immediate,
                    };
                };
                self.update_root(
                    target,
                    RootEvent::NotifyError(format!("Could not fork session: {error}")),
                )
            }
            AppEvent::NewSessionReady {
                pane,
                effort,
                reasoning_mode,
                fast_mode,
                model,
                draft_reset,
                skills,
            } => {
                let workspace = self.workspace.clone();
                let Some(root) = self.pane_mut(pane) else {
                    return ComponentUpdate::none();
                };
                root.component_mut().reset_session(
                    &workspace,
                    effort,
                    reasoning_mode,
                    reasoning_mode,
                    draft_reset,
                );
                root.component_mut().set_fast_mode(fast_mode);
                root.component_mut().set_model(model);
                root.component_mut().set_skills(skills);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            AppEvent::NewSessionFailed { pane, error } => {
                self.update_root(pane, RootEvent::NewSessionFailed(error))
            }
            AppEvent::SessionSearchResults {
                pane,
                picker_id,
                request_id,
                query,
                result,
            } => self.update_root(
                pane,
                RootEvent::SessionSearchResults {
                    picker_id,
                    request_id,
                    query,
                    result,
                },
            ),
            AppEvent::SessionsLoaded {
                pane,
                request_id,
                sessions,
            } => self.update_root(
                pane,
                RootEvent::SessionsLoaded {
                    request_id,
                    sessions,
                },
            ),
            AppEvent::SessionListFailed {
                pane,
                request_id,
                error,
            } => self.update_root(pane, RootEvent::SessionListFailed { request_id, error }),
            AppEvent::RecentPromptsLoaded {
                pane,
                session_id,
                prompts,
            } => self.update_root(
                pane,
                RootEvent::RecentPromptsLoaded {
                    session_id,
                    prompts,
                },
            ),
            AppEvent::RecentPromptLoadFailed { pane, error } => {
                self.update_root(pane, RootEvent::RecentPromptLoadFailed(error))
            }
            AppEvent::SessionLoadFailed { pane, error } => {
                self.update_root(pane, RootEvent::SessionLoadFailed(error))
            }
            AppEvent::SessionRestored {
                pane,
                draft_reset,
                projection,
                effort,
                reasoning_mode,
                preferred_reasoning_mode,
                fast_mode,
                model,
                skills,
            } => self.update_root(
                pane,
                RootEvent::SessionRestored {
                    draft_reset,
                    projection,
                    effort,
                    reasoning_mode,
                    preferred_reasoning_mode,
                    fast_mode,
                    model,
                    skills,
                },
            ),
            AppEvent::SettingsHydrated {
                pane,
                effort,
                fast_mode,
                model,
            } => self.update_root(
                pane,
                RootEvent::SettingsHydrated {
                    effort,
                    fast_mode,
                    model,
                },
            ),
            AppEvent::HistoryReplayed { pane, projection } => {
                self.update_root(pane, RootEvent::HistoryReplayed { projection })
            }
            AppEvent::VaultReceipt { pane, receipt } => {
                self.update_root(pane, RootEvent::VaultReceipt(receipt))
            }
            AppEvent::VaultReview { pane, review } => {
                self.update_root(pane, RootEvent::VaultReview(review))
            }
            AppEvent::ShowAgentId { pane, id } => {
                self.update_root(pane, RootEvent::ShowAgentId(id))
            }
            AppEvent::VoiceStatus(status) => {
                self.update_root(PaneId::Main, RootEvent::VoiceStatus(status))
            }
            AppEvent::NotifyError { pane, error } => {
                self.update_root(pane, RootEvent::NotifyError(error))
            }
            AppEvent::NotifySuccess { pane, message } => {
                self.update_root(pane, RootEvent::NotifySuccess(message))
            }
            AppEvent::ConfirmReviewDownload { pane } => {
                self.update_root(pane, RootEvent::ConfirmReviewDownload)
            }
            AppEvent::UpdateAvailable(version) => {
                let pane = self.main_pane().unwrap_or(self.focus);
                self.update_root(pane, RootEvent::UpdateAvailable(version))
            }
            AppEvent::ConfigReloaded {
                pane,
                theme,
                preferred_reasoning_mode,
                message,
            } => {
                self.theme.replace_from_config(theme);
                let mode = self.theme.mode();
                if let Some((_, main)) = &mut self.main {
                    main.component_mut().set_theme_mode(mode);
                }
                if let Some((_, fork)) = &mut self.fork {
                    fork.component_mut().set_theme_mode(mode);
                }
                self.set_preferred_reasoning_mode(preferred_reasoning_mode);
                self.update_root(pane, RootEvent::NotifySuccess(message))
            }
            AppEvent::ConfigReloadFailed { pane, error } => {
                self.update_root(pane, RootEvent::NotifyError(error))
            }
            AppEvent::SystemThemeChanged(scheme) => {
                if self.theme.set_system_scheme(scheme) {
                    ComponentUpdate::render(RenderRequest::Immediate)
                } else {
                    ComponentUpdate::none()
                }
            }
            AppEvent::AnimationFrame(now) => self.update_all(RootEvent::AnimationFrame(now)),
        }
    }

    pub(crate) fn screen_size(&self) -> ratatui::layout::Size {
        self.screen
            .as_ref()
            .map_or(ratatui::layout::Size::new(0, 0), |screen| {
                screen.image_area.as_size()
            })
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>) {
        let mut area = frame.area();
        self.main_area = Rect::default();
        self.fork_area = Rect::default();
        self.screen_area = Rect::default();
        if let Some(screen) = &mut self.screen {
            screen.image_area = Rect::default();
        }
        if self.screen.is_some() || self.fork.is_some() {
            let tabs = format!(
                "{} Chat  {}{}  · Tab: pane · /zoom{}",
                if !self.screen_focused && self.main_pane() == Some(self.focus) {
                    "›"
                } else {
                    " "
                },
                if self.fork.is_some() {
                    if !self.screen_focused && self.main_pane() != Some(self.focus) {
                        "› BTW  "
                    } else {
                        "  BTW  "
                    }
                } else {
                    ""
                },
                if self.screen.is_some() {
                    if self.screen_focused {
                        "› Screen"
                    } else {
                        "  Screen"
                    }
                } else {
                    ""
                },
                if self.zoomed { ": restore" } else { "" }
            );
            frame.render_widget(
                ratatui::widgets::Paragraph::new(tabs)
                    .style(Style::default().fg(self.theme.accent())),
                Rect {
                    height: 1.min(area.height),
                    ..area
                },
            );
            area.y = area.y.saturating_add(1);
            area.height = area.height.saturating_sub(1);
        }
        if self.zoomed && !self.screen_focused {
            let theme = self.theme.clone();
            if self.main_pane() == Some(self.focus) {
                self.main_area = area;
            } else {
                self.fork_area = area;
            }
            if let Some(root) = self.pane_mut(self.focus) {
                root.component_mut()
                    .render_focused(frame, area, &theme, true);
            }
            return;
        }
        if self.screen.is_some() {
            let screen_area = if self.zoomed {
                area
            } else {
                Rect {
                    x: area.x + area.width / 2,
                    width: area.width - area.width / 2,
                    ..area
                }
            };
            if !self.zoomed {
                self.render_chat(
                    frame,
                    Rect {
                        width: area.width / 2,
                        ..area
                    },
                );
            }
            self.screen_area = screen_area;
            if let Some(screen) = &mut self.screen {
                screen.render(frame, screen_area, &self.theme);
            }
        } else {
            self.render_chat(frame, area);
        }
    }

    fn render_chat(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            self.main_area = Rect::default();
            self.fork_area = Rect::default();
            return;
        }
        let Some((fork_pane, fork)) = &mut self.fork else {
            self.main_area = area;
            self.fork_area = Rect::default();
            if let Some((main_pane, main)) = &mut self.main {
                main.component_mut().render_focused(
                    frame,
                    area,
                    &self.theme,
                    !self.screen_focused && self.focus == *main_pane,
                );
            }
            return;
        };

        let divider_x = area.x + area.width.saturating_sub(1) / 2;
        self.main_area = Rect::new(
            area.x,
            area.y,
            divider_x.saturating_sub(area.x),
            area.height,
        );
        self.fork_area = Rect::new(
            divider_x.saturating_add(1),
            area.y,
            area.right().saturating_sub(divider_x.saturating_add(1)),
            area.height,
        );
        let hint_height = u16::from(Self::split_hint_visible(area));
        let main_content = Rect {
            y: self.main_area.y.saturating_add(hint_height),
            height: self.main_area.height.saturating_sub(hint_height),
            ..self.main_area
        };
        let fork_content = Rect {
            y: self.fork_area.y.saturating_add(hint_height),
            height: self.fork_area.height.saturating_sub(hint_height),
            ..self.fork_area
        };
        if let Some((main_pane, main)) = &mut self.main {
            main.component_mut().render_focused(
                frame,
                main_content,
                &self.theme,
                !self.screen_focused && self.focus == *main_pane,
            );
        }
        fork.component_mut().render_focused(
            frame,
            fork_content,
            &self.theme,
            !self.screen_focused && self.focus == *fork_pane,
        );
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(self.theme.border())),
            Rect::new(divider_x, area.y, 1, area.height),
        );
        self.render_split_hint(frame, area, divider_x);
    }

    pub(crate) fn root(&self, pane: PaneId) -> Option<&RootNode> {
        self.pane(pane).map(Node::component)
    }

    pub(crate) fn animation_deadline(&self) -> Option<Instant> {
        [
            self.main
                .as_ref()
                .and_then(|(_, root)| root.component().animation_deadline()),
            self.fork
                .as_ref()
                .and_then(|(_, root)| root.component().animation_deadline()),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn update_terminal(&mut self, event: Event) -> ComponentUpdate<AppEffect> {
        if self.screen_focused
            && let Event::Paste(text) = &event
        {
            if let Some(screen) = &mut self.screen {
                screen.paste(text);
            }
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if let Event::Key(key) = &event {
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                let switch = matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
                    && (self.screen.is_some() || self.fork.is_some())
                    && (self.screen_focused
                        || self
                            .root(self.focus)
                            .is_some_and(RootNode::allows_pane_switch));
                if switch {
                    self.cycle_focus(key.code == KeyCode::BackTab);
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                if self.screen_focused {
                    let effect = self.screen.as_mut().and_then(|screen| screen.key(*key));
                    let mut effects = Vec::new();
                    match effect {
                        Some(super::screen::Effect::Zoom) => self.zoomed = !self.zoomed,
                        Some(super::screen::Effect::Command(command)) => {
                            effects.push(AppEffect::Screen(command))
                        }
                        Some(super::screen::Effect::Close) => {
                            self.screen = None;
                            self.screen_focused = false;
                            self.zoomed = false;
                            effects.push(AppEffect::Screen(crate::tui::screen::Command::Close));
                        }
                        None => {}
                    }
                    return ComponentUpdate {
                        effects,
                        render: RenderRequest::Immediate,
                    };
                }
            } else if self.screen_focused {
                return ComponentUpdate::none();
            }
        }
        if let Event::Mouse(mouse) = &event
            && matches!(mouse.kind, MouseEventKind::Down(_))
        {
            let position = Position::new(mouse.column, mouse.row);
            if self.screen_area.contains(position) {
                self.screen_focused = true;
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            if self.main_area.contains(position) || self.fork_area.contains(position) {
                self.screen_focused = false;
            }
        }
        if self.screen_focused
            && !matches!(
                event,
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost
            )
        {
            return ComponentUpdate::none();
        }

        if matches!(event, Event::FocusGained) {
            self.refresh_terminal_images();
        }
        if matches!(event, Event::Resize(_, _)) {
            let mut update = self.update_all(RootEvent::Terminal(event));
            update.render = RenderRequest::Immediate;
            return update;
        }
        if self.fork.is_some() && is_control_c(&event) {
            let pane = self.focus;
            let update = self.update_root(pane, RootEvent::Terminal(event));
            if !matches!(update.effects.as_slice(), [AppEffect::Shutdown]) {
                return update;
            }
            self.remove_pane(pane);
            return ComponentUpdate {
                effects: vec![AppEffect::ClosePane(pane)],
                render: RenderRequest::Immediate,
            };
        }
        if let Event::Mouse(mouse) = &event
            && matches!(mouse.kind, MouseEventKind::Down(_))
        {
            let position = Position::new(mouse.column, mouse.row);
            if self.fork_area.contains(position) {
                self.focus = self.fork.as_ref().map_or(PaneId::Main, |(pane, _)| *pane);
            } else if self.main_area.contains(position)
                && let Some(main_pane) = self.main_pane()
            {
                self.focus = main_pane;
            }
        }
        self.update_root(self.focus, RootEvent::Terminal(event))
    }

    pub(crate) fn refresh_terminal_images(&mut self) {
        for root in [&mut self.main, &mut self.fork]
            .into_iter()
            .filter_map(Option::as_mut)
            .map(|(_, root)| root)
        {
            root.component_mut().refresh_terminal_images();
        }
    }

    fn render_split_hint(&self, frame: &mut Frame<'_>, area: Rect, divider_x: u16) {
        let width = u16::try_from(SPLIT_HINT.width()).unwrap_or(u16::MAX);
        if !Self::split_hint_visible(area) {
            return;
        }

        let x = divider_x.saturating_sub(width / 2).max(area.x);
        frame.buffer_mut().set_string(
            x,
            area.y,
            SPLIT_HINT,
            Style::default()
                .fg(self.theme.muted())
                .add_modifier(Modifier::DIM),
        );
    }

    fn split_hint_visible(area: Rect) -> bool {
        area.width >= MIN_SPLIT_HINT_WIDTH
            && area.height >= 2
            && SPLIT_HINT.width() <= usize::from(area.width)
    }

    fn update_all(&mut self, event: RootEvent) -> ComponentUpdate<AppEffect> {
        let main = self.main.take().map(|(pane, mut root)| {
            let update = root.update(event_for_other_pane(&event));
            self.main = Some((pane, root));
            self.map_root_update(pane, update)
        });
        let fork = self.fork.take().map(|(pane, mut root)| {
            let update = root.update(event);
            self.fork = Some((pane, root));
            self.map_root_update(pane, update)
        });
        merge_updates(main, fork)
    }

    fn update_root(&mut self, pane: PaneId, event: RootEvent) -> ComponentUpdate<AppEffect> {
        let Some(root) = self.pane_mut(pane) else {
            return ComponentUpdate::none();
        };
        let update = root.update(event);
        self.map_root_update(pane, update)
    }

    fn map_root_update(
        &mut self,
        pane: PaneId,
        update: ComponentUpdate<RootEffect>,
    ) -> ComponentUpdate<AppEffect> {
        let mut effects = Vec::with_capacity(update.effects.len());
        for effect in update.effects {
            match effect {
                RootEffect::Screen => {
                    self.screen = Some(super::screen::ScreenPane::new());
                    self.screen_focused = true;
                    effects.push(AppEffect::Screen(crate::tui::screen::Command::List));
                }
                RootEffect::Zoom => {
                    self.zoomed = !self.zoomed;
                }
                RootEffect::Fork => {
                    if self.fork.is_none() && self.main.is_some() {
                        let (pane, parent) = self.begin_fork();
                        effects.push(AppEffect::OpenFork { pane, parent });
                    }
                }
                RootEffect::Shutdown => {
                    effects.push(AppEffect::Shutdown);
                }
                RootEffect::SetTheme(mode) => {
                    self.set_theme_mode(mode);
                    effects.push(AppEffect::SetTheme(mode));
                }
                effect => effects.push(AppEffect::Pane { pane, effect }),
            }
        }
        ComponentUpdate {
            effects,
            render: update.render,
        }
    }

    fn cycle_focus(&mut self, backwards: bool) {
        let mut panes = vec![self.main_pane()];
        if let Some((id, _)) = &self.fork {
            panes.push(Some(*id));
        }
        if self.screen.is_some() {
            panes.push(None);
        }
        let current = if self.screen_focused {
            None
        } else {
            Some(self.focus)
        };
        let index = panes.iter().position(|pane| *pane == current).unwrap_or(0);
        let next = if backwards {
            (index + panes.len() - 1) % panes.len()
        } else {
            (index + 1) % panes.len()
        };
        self.screen_focused = panes[next].is_none();
        if let Some(pane) = panes[next] {
            self.focus = pane;
        }
    }

    fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.theme.set_mode(mode);
        if let Some((_, main)) = &mut self.main {
            main.component_mut().set_theme_mode(mode);
        }
        if let Some((_, fork)) = &mut self.fork {
            fork.component_mut().set_theme_mode(mode);
        }
    }

    pub(crate) fn set_max_subagents(&mut self, limit: usize) {
        if let Some((_, main)) = &mut self.main {
            main.component_mut().set_max_subagents(limit);
        }
        if let Some((_, fork)) = &mut self.fork {
            fork.component_mut().set_max_subagents(limit);
        }
    }

    pub(crate) fn set_preferred_reasoning_mode(&mut self, mode: ReasoningMode) {
        if let Some((_, main)) = &mut self.main {
            main.component_mut().set_preferred_reasoning_mode(mode);
        }
        if let Some((_, fork)) = &mut self.fork {
            fork.component_mut().set_preferred_reasoning_mode(mode);
        }
    }

    fn begin_fork(&mut self) -> (PaneId, PaneId) {
        if let Some((_, main)) = &mut self.main {
            main.component_mut().set_fork_available(false);
        }
        let (parent, main) = self
            .main
            .as_ref()
            .expect("forking requires the primary pane");
        let thinking = main.component().composer().effort();
        let fork = main.component().fork(&self.workspace, thinking);
        let pane = PaneId::Fork(self.next_fork);
        self.next_fork = self.next_fork.saturating_add(1);
        self.fork = Some((pane, Node::new(fork)));
        self.focus = pane;
        (pane, *parent)
    }

    fn remove_pane(&mut self, pane: PaneId) {
        if self.main.as_ref().is_some_and(|(id, _)| *id == pane) {
            self.main = self.fork.take();
        } else if self.fork.as_ref().is_some_and(|(id, _)| *id == pane) {
            self.fork = None;
        } else {
            return;
        }
        if let Some((main_pane, main)) = &mut self.main {
            self.focus = *main_pane;
            if self.fork.is_none() {
                main.component_mut().set_fork_available(true);
            }
        }
    }

    pub(crate) fn main_pane(&self) -> Option<PaneId> {
        self.main.as_ref().map(|(pane, _)| *pane)
    }

    fn pane(&self, pane: PaneId) -> Option<&Node<RootNode>> {
        self.main
            .as_ref()
            .filter(|(main_pane, _)| *main_pane == pane)
            .or_else(|| {
                self.fork
                    .as_ref()
                    .filter(|(fork_pane, _)| *fork_pane == pane)
            })
            .map(|(_, root)| root)
    }

    fn pane_mut(&mut self, pane: PaneId) -> Option<&mut Node<RootNode>> {
        if self
            .main
            .as_ref()
            .is_some_and(|(main_pane, _)| *main_pane == pane)
        {
            return self.main.as_mut().map(|(_, root)| root);
        }
        self.fork
            .as_mut()
            .filter(|(fork_pane, _)| *fork_pane == pane)
            .map(|(_, root)| root)
    }
}

fn event_for_other_pane(event: &RootEvent) -> RootEvent {
    match event {
        RootEvent::Terminal(Event::Resize(width, height)) => {
            RootEvent::Terminal(Event::Resize(*width, *height))
        }
        RootEvent::AnimationFrame(now) => RootEvent::AnimationFrame(*now),
        _ => unreachable!("only broadcast events are cloned"),
    }
}

fn merge_updates(
    first: Option<ComponentUpdate<AppEffect>>,
    second: Option<ComponentUpdate<AppEffect>>,
) -> ComponentUpdate<AppEffect> {
    let mut merged = ComponentUpdate::none();
    for mut update in first.into_iter().chain(second) {
        merged.effects.append(&mut update.effects);
        merged.render = merged.render.max(update.render);
    }
    merged
}

fn is_control_c(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod screen_tests {
    use super::*;
    use crate::config::ReasoningEffort;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    fn app() -> AppNode {
        AppNode::new(
            Theme::default(),
            PathBuf::from("/test"),
            RootNode::new(std::path::Path::new("/test"), ReasoningEffort::Medium),
        )
    }
    fn key(app: &mut AppNode, code: KeyCode) -> ComponentUpdate<AppEffect> {
        app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        ))))
    }
    #[test]
    fn screen_and_zoom_are_local_and_tab_cycles_without_changing_sessions() {
        let mut app = app();
        let update = app.map_root_update(
            PaneId::Main,
            ComponentUpdate {
                effects: vec![RootEffect::Screen],
                render: RenderRequest::Immediate,
            },
        );
        assert!(matches!(
            update.effects.as_slice(),
            [AppEffect::Screen(crate::tui::screen::Command::List)]
        ));
        assert!(app.screen_focused);
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(app.screen_area.width, 80);
        for c in "/zoom".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(app.screen_area.width, 160);
        key(&mut app, KeyCode::Tab);
        assert!(!app.screen_focused);
        assert!(app.zoomed);
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(app.main_area.width, 160);
        assert!(app.screen_area.is_empty());
        key(&mut app, KeyCode::BackTab);
        let close = key(&mut app, KeyCode::Esc);
        assert!(matches!(
            close.effects.as_slice(),
            [AppEffect::Screen(crate::tui::screen::Command::Close)]
        ));
        assert!(app.screen.is_none());
        assert!(!app.zoomed);
        assert_eq!(app.main_pane(), Some(PaneId::Main));
    }
    #[test]
    fn typed_screen_command_opens_picker_without_submitting_a_prompt() {
        let mut app = app();
        for character in "/screen".chars() {
            key(&mut app, KeyCode::Char(character));
        }
        let update = key(&mut app, KeyCode::Enter);
        assert!(matches!(
            update.effects.as_slice(),
            [AppEffect::Screen(crate::tui::screen::Command::List)]
        ));
        assert!(app.screen_focused);
        assert!(
            app.root(PaneId::Main)
                .unwrap()
                .composer()
                .draft()
                .is_empty()
        );
    }
    #[test]
    fn zoom_and_tab_include_btw_pane() {
        let mut app = app();
        let (fork, _) = app.begin_fork();
        app.map_root_update(
            fork,
            ComponentUpdate {
                effects: vec![RootEffect::Zoom],
                render: RenderRequest::Immediate,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(app.fork_area.width, 120);
        assert!(app.main_area.is_empty());
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, PaneId::Main);
        assert!(app.zoomed);
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(app.main_area.width, 120);
    }
}
