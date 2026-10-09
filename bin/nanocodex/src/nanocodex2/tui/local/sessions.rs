//! Local session source for the unified TUI: the `ncl resume` picker, resuming
//! saved sessions of either harness (`ncl resume [ID] [--from ROLLOUT --at N]`), the
//! in-TUI /attach picker and the replay of a resumed session's history.
//!
//! Every lookup goes through the family-neutral durable catalog
//! ([`crate::sessions`]), so lists, searches, switches and branches show and
//! continue Codex and Claude sessions alike.

use std::{
    borrow::Cow,
    future::Future,
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use eyre::{Result, WrapErr as _, eyre};
use nanocodex::{HarnessFamily, agent::session::TranscriptItem};
use nanocodex_managed::{ManagedEvent, ManagedEventData, PromptInput};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use serde_json::{Value, json};

use super::agent::LocalLaunch;
use crate::config::ConfiguredAgent;
use crate::nanocodex2::{
    config::{ReasoningEffort, ReasoningMode},
    tui::{history::HistoryWindow, session::SessionSummary, terminal::TerminalSession},
};

/// A saved session to reopen when the local agent is (re)built. A session
/// resumed at startup is carried by `AgentArgs::resume` instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Resume {
    /// A saved session of either harness (branch switch); the connection task
    /// resolves it with [`resolve`] so lookups stay off the input loop.
    Session(String),
    /// A branch started by editing an earlier prompt: reopen `thread`, or branch
    /// `fork` into a new session first (None for both: a fresh session), and
    /// submit `prompt` once it connects.
    Branch {
        thread: Option<String>,
        fork: Option<Fork>,
        prompt: String,
    },
}

/// Where a branch copies its history from: session `session` (rollout `source`)
/// through `turns` completed turns, rooted at `workspace`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fork {
    pub(crate) session: String,
    pub(crate) source: PathBuf,
    pub(crate) turns: usize,
    pub(crate) workspace: PathBuf,
}

/// The harness that owns a saved session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Harness {
    Codex,
    Claude,
}

impl Harness {
    const fn label(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

impl From<HarnessFamily> for Harness {
    fn from(family: HarnessFamily) -> Self {
        match family {
            HarnessFamily::Claude => Self::Claude,
            _ => Self::Codex,
        }
    }
}

/// One resumable local session, Codex or Claude.
#[derive(Clone, Debug)]
pub(crate) struct LocalSession {
    pub(crate) id: String,
    pub(crate) harness: Harness,
    pub(crate) workspace: Option<String>,
    pub(crate) preview: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) updated: SystemTime,
    pub(crate) archived: bool,
}

impl From<&crate::sessions::SessionSummary> for LocalSession {
    fn from(session: &crate::sessions::SessionSummary) -> Self {
        Self {
            id: session.id().to_owned(),
            harness: session.family().into(),
            workspace: session.workspace().map(str::to_owned),
            // Claude previews are raw first prompts; catalog previews of Codex
            // threads are shown as recorded.
            preview: session.preview().map(|preview| match session.family() {
                HarnessFamily::Claude => single_line(preview),
                _ => preview.to_owned(),
            }),
            model: session.model().map(|model| model.to_string()),
            updated: session.modified_at(),
            archived: session.is_archived(),
        }
    }
}

/// Runs a catalog operation from the blocking pool, where every caller of
/// these synchronous helpers already runs.
fn blocking<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Handle::current().block_on(future)
}

/// Every resumable local session under `home`, newest activity first.
pub(crate) async fn discover(home: &Path) -> Result<Vec<LocalSession>> {
    Ok(crate::sessions::list(home)
        .await?
        .iter()
        .map(LocalSession::from)
        .collect())
}

/// Recent local Codex and Claude sessions for the in-TUI picker, newest first.
pub(crate) fn list(workspace: &Path) -> Result<Vec<SessionSummary>> {
    let home = crate::config::default_codex_home()?;
    Ok(blocking(discover(&home))?
        .into_iter()
        .map(|session| summary(&session, workspace))
        .collect())
}

fn summary(session: &LocalSession, fallback: &Path) -> SessionSummary {
    SessionSummary {
        session_id: session.id.clone(),
        updated_at_unix_ms: session
            .updated
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            }),
        model: session
            .model
            .clone()
            .unwrap_or_else(|| session.harness.label().to_owned()),
        effort: ReasoningEffort::Medium,
        reasoning_mode: ReasoningMode::Standard,
        workspace: session
            .workspace
            .as_ref()
            .map_or_else(|| fallback.to_path_buf(), PathBuf::from),
        preview: session.preview.clone().unwrap_or_default(),
    }
}

/// Content search over local session transcripts for the /attach picker.
pub(crate) fn search(
    query: &str,
    limit: usize,
) -> Result<Vec<nanocodex_managed::SessionSearchHit>> {
    let home = crate::config::default_codex_home()?;
    let terms = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    for session in blocking(discover(&home))? {
        let Ok(loaded) = blocking(crate::sessions::load(&home, &session.id)) else {
            continue;
        };
        let found = loaded.transcript().iter().enumerate().find_map(|(index, item)| {
            let text = match item {
                TranscriptItem::User(text)
                | TranscriptItem::Assistant(text)
                | TranscriptItem::Reasoning(text) => text.as_str(),
                TranscriptItem::Tool { arguments, .. } => arguments.as_str(),
            };
            let lower = text.to_lowercase();
            terms
                .iter()
                .all(|term| lower.contains(term))
                .then(|| (index, excerpt(text, &terms[0])))
        });
        if let Some((index, snippet)) = found {
            hits.push(nanocodex_managed::SessionSearchHit {
                session_id: session.id.clone(),
                title: session.preview.clone().unwrap_or_default(),
                turn_id: String::new(),
                cursor: format!("resume-{}", index + 1),
                score: 1.0,
                snippet,
            });
            if hits.len() >= limit {
                break;
            }
        }
    }
    Ok(hits)
}

fn excerpt(text: &str, term: &str) -> String {
    let lower = text.to_lowercase();
    let start = lower.find(term).unwrap_or(0);
    let mut begin = start.saturating_sub(60);
    while !text.is_char_boundary(begin) {
        begin -= 1;
    }
    text[begin..]
        .chars()
        .take(200)
        .collect::<String>()
        .replace('\n', " ")
}

fn single_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

/// User prompts of a saved session of either harness, oldest first.
pub(crate) fn prompts(id: &str) -> Vec<String> {
    let Ok(home) = crate::config::default_codex_home() else {
        return Vec::new();
    };
    blocking(crate::sessions::load(&home, id))
        .map(|session| {
            session
                .transcript()
                .iter()
                .filter_map(|item| match item {
                    TranscriptItem::User(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Returns `base` relaunched against the saved session `id` of either harness,
/// keeping VM and other launch flags. The session continues in the harness
/// that recorded it.
pub(crate) fn relaunch(base: &LocalLaunch, id: &str) -> Result<LocalLaunch> {
    let home = crate::config::default_codex_home()?;
    // Validate before replacing the running agent.
    let session = blocking(crate::sessions::load(&home, id))?;
    Ok(LocalLaunch {
        args: base.args.clone().for_session_switch().resume(session)?,
        vm: base.vm.clone(),
        replaceable: false,
        initial_prompt: None,
        initial_instruction: None,
        resume: None,
    })
}

/// `base` relaunched against a saved session or branch without touching the
/// disk; the connection task loads (and validates) it.
pub(crate) fn session_launch(base: &LocalLaunch, resume: Resume) -> LocalLaunch {
    LocalLaunch {
        args: base.args.clone().for_session_switch(),
        vm: base.vm.clone(),
        replaceable: false,
        initial_prompt: None,
        initial_instruction: None,
        resume: Some(resume),
    }
}

/// Returns `base` relaunched as a fresh session (/clear).
pub(crate) fn fresh(base: &LocalLaunch) -> LocalLaunch {
    LocalLaunch {
        args: base.args.clone().fresh_session(),
        vm: base.vm.clone(),
        replaceable: true,
        initial_prompt: None,
        initial_instruction: None,
        resume: None,
    }
}

/// Resolves a saved-session or branch launch to the harness that saved it, on
/// the blocking pool; other launches are returned unchanged. Branch copies
/// happen here: a durable session branches in its own family through the
/// catalog; a rollout-only Codex thread is copied with `rollout_fork`.
pub(crate) async fn resolve(launch: LocalLaunch) -> Result<LocalLaunch> {
    if !matches!(
        &launch.resume,
        Some(
            Resume::Session(_)
                | Resume::Branch {
                    thread: Some(_),
                    ..
                }
                | Resume::Branch { fork: Some(_), .. }
        )
    ) {
        return Ok(launch);
    }
    tokio::task::spawn_blocking(move || resolve_blocking(launch))
        .await
        .wrap_err("session lookup task failed")?
}

fn resolve_blocking(mut launch: LocalLaunch) -> Result<LocalLaunch> {
    let home = crate::config::default_codex_home()?;
    let id = match launch.resume.clone() {
        Some(Resume::Session(id)) => return relaunch(&launch, &id),
        Some(Resume::Branch {
            thread: Some(thread),
            ..
        }) => thread,
        Some(Resume::Branch {
            thread: None,
            fork: Some(fork),
            prompt,
        }) => {
            let thread = branch(&home, &fork).wrap_err("could not start a branch")?;
            launch.resume = Some(Resume::Branch {
                thread: Some(thread.clone()),
                fork: None,
                prompt,
            });
            thread
        }
        _ => return Ok(launch),
    };
    let session = blocking(crate::sessions::load(&home, &id))?;
    launch.args = launch.args.resume(session)?;
    Ok(launch)
}

/// Starts a new session holding `fork.turns` completed turns of its source.
fn branch(home: &Path, fork: &Fork) -> Result<String> {
    if let Ok((store, turns)) = blocking(crate::sessions::turns(home, &fork.session)) {
        let at = match turns.get(fork.turns) {
            Some(turn) => nanocodex_durability::BranchPoint::Before(turn.id.clone()),
            None => nanocodex_durability::BranchPoint::Latest,
        };
        let branched = blocking(crate::sessions::branch(
            &store,
            &fork.session,
            at,
            Some(fork.workspace.clone()),
        ))?;
        return Ok(branched.id().to_owned());
    }
    crate::rollout_fork::fork(
        &fork.source,
        &crate::rollout_fork::Point::Count(fork.turns),
        home,
        &fork.workspace,
    )
}

/// What building a (possibly resumed) local agent produced.
pub(crate) struct Built {
    pub(crate) agent: ConfiguredAgent,
    pub(crate) workspace: PathBuf,
    /// Visible history of a resumed session, oldest first.
    pub(crate) transcript: Vec<TranscriptItem>,
}

/// Builds the local agent for `launch`, continuing its resumed session if any.
/// A saved-session or branch launch must be [`resolve`]d first.
pub(crate) async fn build(launch: &LocalLaunch) -> Result<Built> {
    if let Some(Resume::Session(id)) = &launch.resume {
        return Err(eyre!("saved session {id} was not resolved before building"));
    }
    let resumed = launch.args.resumed();
    let workspace = resumed
        .and_then(crate::sessions::ResumedSession::workspace)
        .map_or_else(|| launch.args.cwd().to_path_buf(), Path::to_path_buf);
    let transcript = resumed
        .map(|session| session.transcript().to_vec())
        .unwrap_or_default();
    let agent = launch
        .args
        .clone()
        .build_tui(launch.vm.clone())
        .await
        .wrap_err(if resumed.is_some() {
            "could not resume the local agent"
        } else {
            "could not start the local agent"
        })?;
    Ok(Built {
        agent,
        workspace,
        transcript,
    })
}

/// The visible history of a resumed session as managed history events, so the
/// driver projects it exactly like a managed session's durable history.
pub(in crate::nanocodex2::tui) fn history_window(
    transcript: &[TranscriptItem],
    request_id: &str,
) -> HistoryWindow {
    let mut replay = Replay {
        request_id,
        events: Vec::new(),
        turn: None,
        turns: 0,
        call: 0,
        final_message: String::new(),
    };
    for item in transcript {
        match item {
            TranscriptItem::User(text) => {
                replay.finish_turn();
                replay.turns += 1;
                let id = format!("resume-turn-{}", replay.turns);
                replay.turn = Some(id.clone());
                replay.push(ManagedEventData::TurnAccepted {
                    id,
                    input: PromptInput::Text(text.clone()),
                    replayed: true,
                });
            }
            TranscriptItem::Reasoning(text) => {
                replay.ensure_turn();
                replay.agent(
                    "reasoning.summary.delta",
                    json!({"model_call_index": replay.call, "text": text}),
                );
            }
            TranscriptItem::Assistant(text) => {
                replay.ensure_turn();
                let item = format!("resume-message-{}", replay.events.len());
                replay.agent(
                    "assistant.message",
                    json!({"model_call_index": replay.call, "item_id": item, "phase": null, "text": text}),
                );
                replay.final_message.clone_from(text);
                replay.call += 1;
            }
            TranscriptItem::Tool {
                call_id,
                name,
                arguments,
            } => {
                replay.ensure_turn();
                let arguments = serde_json::from_str::<Value>(arguments)
                    .unwrap_or_else(|_| Value::String(arguments.clone()));
                replay.agent(
                    "tool.call",
                    json!({"call_id": call_id, "tool": name, "arguments": arguments, "model_call_index": replay.call}),
                );
                replay.agent(
                    "tool.result",
                    json!({"call_id": call_id, "tool": name, "status": "completed", "duration_ns": 0, "result": null}),
                );
            }
        }
    }
    replay.finish_turn();
    HistoryWindow {
        events: replay.events,
        before: None,
        has_more: false,
    }
}

struct Replay<'a> {
    request_id: &'a str,
    events: Vec<ManagedEvent>,
    turn: Option<String>,
    turns: usize,
    call: u32,
    final_message: String,
}

impl Replay<'_> {
    fn push(&mut self, data: ManagedEventData) {
        let cursor = format!("resume-{}", self.events.len() + 1);
        self.events.push(ManagedEvent {
            cursor,
            created_at: None,
            turn_id: self.turn.clone(),
            data,
        });
    }

    fn agent(&mut self, kind: &str, payload: Value) {
        let event = json!({
            "protocol_version": 1,
            "request_id": self.request_id,
            "seq": self.events.len() + 1,
            "type": kind,
            "payload": payload,
        });
        if let Ok(event) = serde_json::value::to_raw_value(&event) {
            self.push(ManagedEventData::Event {
                event,
                agent_id: None,
            });
        }
    }

    fn ensure_turn(&mut self) {
        if self.turn.is_none() {
            self.turns += 1;
            self.turn = Some(format!("resume-turn-{}", self.turns));
        }
    }

    fn finish_turn(&mut self) {
        if let Some(id) = self.turn.clone() {
            let final_message = std::mem::take(&mut self.final_message);
            self.push(ManagedEventData::TurnCompleted {
                id,
                final_message,
                usage: None,
                citations: Vec::new(),
                usage_error: None,
            });
            self.turn = None;
            self.call = 0;
        }
    }
}

/// Full-screen picker for `ncl resume` without an id (legacy resume picker).
/// Returns the selected session, or None when cancelled.
pub(crate) async fn select(sessions: &[LocalSession]) -> io::Result<Option<LocalSession>> {
    let mut terminal = TerminalSession::enter().await?;
    let mut picker = Picker::new(sessions.len());
    loop {
        terminal.draw(|frame| render(frame, sessions, &mut picker, SystemTime::now()))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        match picker.handle_key(key) {
            PickerAction::Continue => {}
            PickerAction::Select => return Ok(sessions.get(picker.selected).cloned()),
            PickerAction::Cancel => return Ok(None),
        }
    }
}

struct Picker {
    selected: usize,
    session_count: usize,
    page_size: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PickerAction {
    Continue,
    Select,
    Cancel,
}

impl Picker {
    const fn new(session_count: usize) -> Self {
        Self {
            selected: 0,
            session_count,
            page_size: 1,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return PickerAction::Cancel;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-saturating_isize(self.page_size)),
            KeyCode::PageDown => self.move_by(saturating_isize(self.page_size)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.session_count.saturating_sub(1),
            KeyCode::Enter => return PickerAction::Select,
            KeyCode::Esc | KeyCode::Char('q') => return PickerAction::Cancel,
            _ => {}
        }
        PickerAction::Continue
    }

    fn move_by(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.session_count.saturating_sub(1));
    }

    fn visible_range(&mut self, page_size: usize) -> std::ops::Range<usize> {
        self.page_size = page_size.max(1);
        let max_start = self.session_count.saturating_sub(self.page_size);
        let start = self
            .selected
            .saturating_sub(self.page_size.saturating_sub(1))
            .min(max_start);
        start..(start + self.page_size).min(self.session_count)
    }
}

fn render(frame: &mut Frame<'_>, sessions: &[LocalSession], picker: &mut Picker, now: SystemTime) {
    let area = frame.area();
    let block = Block::default()
        .title(" Resume a thread ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [summary, list, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!("  {} resumable threads", sessions.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                "  Newest activity first",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        summary,
    );
    let page_size = usize::from(list.height / 2).max(1);
    let visible = picker.visible_range(page_size);
    let items = visible
        .map(|index| session_item(&sessions[index], index == picker.selected, now))
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items), list);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ↑/↓", Style::default().fg(Color::Cyan)),
            Span::raw(" select · "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(" resume · "),
            Span::styled("esc", Style::default().fg(Color::Cyan)),
            Span::raw(" cancel"),
        ])),
        footer,
    );
}

fn session_item(session: &LocalSession, selected: bool, now: SystemTime) -> ListItem<'static> {
    let marker = if selected { "›" } else { " " };
    let style = if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let workspace = sanitized(
        session
            .workspace
            .as_deref()
            .unwrap_or("(workspace unavailable)"),
    )
    .into_owned();
    let location = if session.archived {
        "archived"
    } else {
        "active"
    };
    let preview =
        sanitized(session.preview.as_deref().unwrap_or("(prompt unavailable)")).into_owned();
    ListItem::new(vec![
        Line::styled(
            format!("{marker} {} · {preview}", format_age(session.updated, now)),
            style,
        ),
        Line::styled(
            format!(
                "  {workspace} · {} · {location} · {}",
                session.harness.label(),
                session.id
            ),
            if selected {
                style
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ])
}

fn sanitized(value: &str) -> Cow<'_, str> {
    if value.chars().any(char::is_control) {
        Cow::Owned(
            value
                .chars()
                .filter(|character| !character.is_control())
                .collect(),
        )
    } else {
        Cow::Borrowed(value)
    }
}

fn format_age(modified_at: SystemTime, now: SystemTime) -> String {
    let Ok(elapsed) = now.duration_since(modified_at) else {
        return "now".to_owned();
    };
    match elapsed.as_secs() {
        0..60 => "now".to_owned(),
        seconds @ 60..3_600 => format!("{}m ago", seconds / 60),
        seconds @ 3_600..86_400 => format!("{}h ago", seconds / 3_600),
        seconds => format!("{}d ago", seconds / 86_400),
    }
}

fn saturating_isize(value: usize) -> isize {
    isize::try_from(value).unwrap_or(isize::MAX)
}

/// No saved sessions: the error `ncl resume` reports.
pub(crate) fn none_found(home: &Path) -> eyre::Report {
    eyre!(
        "no resumable Codex or Claude sessions found under {}",
        home.display()
    )
}
