//! Local session source for the unified TUI: the `ncl resume` picker, resuming
//! saved Codex and Claude sessions (`ncl resume [ID] [--from ROLLOUT --at N]`), the
//! in-TUI /attach picker and the replay of a resumed session's history.
//!
//! The unified driver uses this to show and continue local sessions.

use std::{
    borrow::Cow,
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use eyre::{Result, WrapErr as _, eyre};
use nanocodex::agent::rollout::{RolloutConfig, RolloutTranscriptItem};
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

/// A saved Codex thread to reopen when the local agent is (re)built. Claude
/// sessions resume through `AgentArgs::resume_claude` instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Resume {
    Codex(String),
    /// A saved session of either harness (branch switch); the connection task
    /// resolves it with [`resolve`] so lookups stay off the input loop.
    Session(String),
    /// A branch started by editing an earlier prompt: reopen `thread`, or copy
    /// `fork` into a new thread first (None for both: a fresh session), and submit
    /// `prompt` once it connects.
    Branch {
        thread: Option<String>,
        fork: Option<Fork>,
        prompt: String,
    },
}

/// Where a branch copies its history from: the source rollout through
/// `turns` completed turns, rooted at `workspace`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fork {
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

/// Every resumable local session under `home`, newest activity first.
pub(crate) fn discover(home: &Path) -> Result<Vec<LocalSession>> {
    let mut sessions = Vec::new();
    let codex = RolloutConfig::new(home)
        .list_sessions()
        .wrap_err_with(|| format!("failed to discover Codex threads under {}", home.display()))?;
    sessions.extend(codex.into_iter().map(|session| LocalSession {
        id: session.thread_id().to_owned(),
        harness: Harness::Codex,
        workspace: session.workspace().map(str::to_owned),
        preview: session.preview().map(str::to_owned),
        model: None,
        updated: session.modified_at(),
        archived: session.is_archived(),
    }));
    if crate::native_sessions::store_path(home).is_file() {
        for session in crate::native_sessions::discover(home)? {
            let preview = session.transcript.iter().find_map(|item| match item {
                RolloutTranscriptItem::User(text) => Some(single_line(text)),
                _ => None,
            });
            sessions.push(LocalSession {
                updated: UNIX_EPOCH + Duration::from_secs(session.updated()),
                workspace: session
                    .workspace
                    .as_ref()
                    .map(|path| path.display().to_string()),
                model: session.model.map(|model| model.to_string()),
                id: session.id,
                harness: Harness::Claude,
                preview,
                archived: false,
            });
        }
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.updated));
    Ok(sessions)
}

/// Recent local Codex and Claude sessions for the in-TUI picker, newest first.
pub(crate) fn list(workspace: &Path) -> Result<Vec<SessionSummary>> {
    let home = crate::config::default_codex_home()?;
    Ok(discover(&home)?
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
    for session in discover(&home)? {
        let transcript = match session.harness {
            Harness::Codex => match RolloutConfig::new(&home).load_session(&session.id) {
                Ok(loaded) => loaded.transcript().to_vec(),
                Err(_) => continue,
            },
            Harness::Claude => match crate::native_sessions::load(&home, &session.id) {
                Ok(loaded) => loaded.transcript,
                Err(_) => continue,
            },
        };
        let found = transcript.iter().enumerate().find_map(|(index, item)| {
            let text = match item {
                RolloutTranscriptItem::User(text)
                | RolloutTranscriptItem::Assistant(text)
                | RolloutTranscriptItem::Reasoning(text) => text.as_str(),
                RolloutTranscriptItem::Tool { arguments, .. } => arguments.as_str(),
                RolloutTranscriptItem::ToolResult { output, .. } => output.as_str(),
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

/// Returns `base` relaunched against the saved session `id` (Codex thread UUID
/// or Claude session id), keeping VM and other launch flags.
pub(crate) fn relaunch(base: &LocalLaunch, id: &str) -> Result<LocalLaunch> {
    let home = crate::config::default_codex_home()?;
    let mut args = base.args.clone().for_session_switch();
    let resume = if crate::native_sessions::store_path(&home).is_file()
        && let Ok(session) = crate::native_sessions::load(&home, id)
    {
        args.resume_with_harness(nanocodex::HarnessFamily::Claude);
        args = args.resume_claude(session)?;
        None
    } else {
        // Validate before replacing the running agent.
        RolloutConfig::new(&home)
            .load_session(id)
            .wrap_err_with(|| format!("failed to load Codex thread {id}"))?;
        args.resume_with_harness(nanocodex::HarnessFamily::Codex);
        Some(Resume::Codex(id.to_owned()))
    };
    Ok(LocalLaunch {
        args,
        vm: base.vm.clone(),
        replaceable: false,
        initial_prompt: None,
        initial_instruction: None,
        resume,
    })
}

/// `base` relaunched against a known Codex thread or branch without touching the
/// disk; the connection task loads (and validates) it.
pub(crate) fn codex_launch(base: &LocalLaunch, resume: Resume) -> LocalLaunch {
    let mut args = base.args.clone().for_session_switch();
    args.resume_with_harness(nanocodex::HarnessFamily::Codex);
    LocalLaunch {
        args,
        vm: base.vm.clone(),
        replaceable: false,
        initial_prompt: None,
        initial_instruction: None,
        resume: Some(resume),
    }
}

/// Returns `base` relaunched as a fresh session (/clear).
pub(crate) fn fresh(base: &LocalLaunch) -> LocalLaunch {
    let mut args = base.args.clone();
    args.claude_resume = None;
    LocalLaunch {
        args,
        vm: base.vm.clone(),
        replaceable: true,
        initial_prompt: None,
        initial_instruction: None,
        resume: None,
    }
}

/// Resolves a [`Resume::Session`] launch to the harness that saved it, on the
/// blocking pool; other launches are returned unchanged.
pub(crate) async fn resolve(launch: LocalLaunch) -> Result<LocalLaunch> {
    let id = match &launch.resume {
        Some(Resume::Session(id)) => id.clone(),
        _ => return Ok(launch),
    };
    tokio::task::spawn_blocking(move || relaunch(&launch, &id))
        .await
        .wrap_err("session lookup task failed")?
}

/// What building a (possibly resumed) local agent produced.
pub(crate) struct Built {
    pub(crate) agent: ConfiguredAgent,
    pub(crate) workspace: PathBuf,
    /// Visible history of a resumed session, oldest first.
    pub(crate) transcript: Vec<RolloutTranscriptItem>,
}

/// Builds the local agent for `launch`, reopening its saved session if any. File work
/// (branch copies, rollout materialization) runs on the blocking pool. A
/// [`Resume::Session`] launch must be [`resolve`]d first.
pub(crate) async fn build(launch: &LocalLaunch) -> Result<Built> {
    if let Some(Resume::Session(id)) = &launch.resume {
        return Err(eyre!("saved session {id} was not resolved before building"));
    }
    let thread = match &launch.resume {
        Some(
            Resume::Codex(thread)
            | Resume::Branch {
                thread: Some(thread),
                ..
            },
        ) => Some(thread.clone()),
        Some(Resume::Branch {
            thread: None,
            fork: Some(fork),
            ..
        }) => {
            let fork = fork.clone();
            let thread = tokio::task::spawn_blocking(move || -> Result<String> {
                let home = crate::config::default_codex_home()?;
                crate::rollout_fork::fork(
                    &fork.source,
                    &crate::rollout_fork::Point::Count(fork.turns),
                    &home,
                    &fork.workspace,
                )
            })
            .await
            .wrap_err("branch copy task failed")?
            .wrap_err("could not start a branch")?;
            Some(thread)
        }
        _ => None,
    };
    if let Some(thread_id) = thread {
        let session = tokio::task::spawn_blocking(move || -> Result<_> {
            let home = crate::config::default_codex_home()?;
            RolloutConfig::new(&home)
                .load_session(&thread_id)
                .wrap_err_with(|| format!("failed to load Codex thread {thread_id}"))
        })
        .await
        .wrap_err("thread load task failed")??;
        let workspace = PathBuf::from(session.workspace());
        let transcript = session.transcript().to_vec();
        let model = nanocodex::HarnessModel::from(session.model());
        let mut agent = launch
            .args
            .clone()
            .build_resumed_tui(session, launch.vm.clone())
            .await
            .wrap_err("could not resume the local agent")?;
        // The resumed thread keeps its model; the footer and picker show it.
        agent.model = model;
        return Ok(Built {
            agent,
            workspace,
            transcript,
        });
    }
    let workspace = launch.args.cwd().to_path_buf();
    let transcript = launch
        .args
        .claude_resume
        .as_ref()
        .map(|session| session.transcript.clone())
        .unwrap_or_default();
    let agent = launch
        .args
        .clone()
        .build_tui(launch.vm.clone())
        .await
        .wrap_err("could not start the local agent")?;
    Ok(Built {
        agent,
        workspace,
        transcript,
    })
}

/// The visible history of a resumed session as managed history events, so the
/// driver projects it exactly like a managed session's durable history.
pub(in crate::nanocodex2::tui) fn history_window(
    transcript: &[RolloutTranscriptItem],
    request_id: &str,
) -> HistoryWindow {
    let mut replay = Replay {
        request_id,
        events: Vec::new(),
        turn: None,
        turns: 0,
        call: 0,
        final_message: String::new(),
        open_tools: Vec::new(),
    };
    for item in transcript {
        match item {
            RolloutTranscriptItem::User(text) => {
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
            RolloutTranscriptItem::Reasoning(text) => {
                replay.ensure_turn();
                replay.agent(
                    "reasoning.summary.delta",
                    json!({"model_call_index": replay.call, "text": text}),
                );
            }
            RolloutTranscriptItem::Assistant(text) => {
                replay.ensure_turn();
                let item = format!("resume-message-{}", replay.events.len());
                replay.agent(
                    "assistant.message",
                    json!({"model_call_index": replay.call, "item_id": item, "phase": null, "text": text}),
                );
                replay.final_message.clone_from(text);
                replay.call += 1;
            }
            RolloutTranscriptItem::Tool {
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
                replay.open_tools.push((call_id.clone(), name.clone()));
            }
            RolloutTranscriptItem::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                let Some(position) = replay.open_tools.iter().position(|(id, _)| id == call_id)
                else {
                    continue;
                };
                let (call_id, name) = replay.open_tools.remove(position);
                let result = replayed_result(&name, output, *is_error);
                let status = if *is_error { "failed" } else { "completed" };
                replay.agent(
                    "tool.result",
                    json!({"call_id": call_id, "tool": name, "status": status, "duration_ns": 0, "result": result}),
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

/// A replayed outcome in the shape live tools publish. Shell outcomes carry the
/// exit status their text reports; a receipt without one is the tool's own
/// success or failure.
fn replayed_result(name: &str, output: &str, is_error: bool) -> Value {
    if crate::nanocodex2::tui::transcript::ToolEntry::tool_family(name) != "exec_command" {
        return json!({ "text": output });
    }
    let reported = |prefix: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix)?.trim().parse::<i64>().ok())
    };
    let mut result = json!({ "output": output });
    if let Some(session_id) = reported("Process running with session ID ") {
        result["session_id"] = json!(session_id);
    } else {
        result["exit_code"] =
            json!(reported("Process exited with code ").unwrap_or(i64::from(is_error)));
    }
    result
}

struct Replay<'a> {
    request_id: &'a str,
    events: Vec<ManagedEvent>,
    turn: Option<String>,
    turns: usize,
    call: u32,
    final_message: String,
    /// Calls awaiting their replayed outcome, in call order.
    open_tools: Vec<(String, String)>,
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
        // Rollout events such as MCP and web search record only completed calls.
        for (call_id, name) in std::mem::take(&mut self.open_tools) {
            self.agent(
                "tool.result",
                json!({"call_id": call_id, "tool": name, "status": "completed", "duration_ns": 0, "result": null}),
            );
        }
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
