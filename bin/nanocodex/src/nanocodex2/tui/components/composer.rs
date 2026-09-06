// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Multiline prompt editing and Pi-style composer rendering.

mod history;
mod layout;

use super::{
    node::{Component, ComponentUpdate, RenderRequest},
    selection::{TextRange, TextSpan},
    waved_text::WavedText,
};
use crate::{
    config::{ReasoningEffort, ReasoningMode},
    tui::{
        context::MODEL_WINDOW_TOKENS,
        format::{
            format_turn_duration, normalize_line_endings, sanitize_terminal_text, shorten_home,
            terminal_text_width,
        },
        prompt::Submission,
        theme::Theme,
    },
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use history::PromptHistory;
use layout::{VisualLayout, byte_at_column, grapheme_at_column};
use nanocodex::Model;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::{
    collections::VecDeque,
    mem,
    ops::Range,
    path::Path,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MIN_CONTENT_ROWS: usize = 3;
const MAX_CONTENT_ROWS: usize = 6;
const DEVELOPMENT_BADGE: &str = " ◉ dev ";

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ComposerEffect {
    Submit(Submission),
    Queue(Submission),
    RunShell(String),
    OpenDraftEditor,
    Settings(SettingsCommand),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingsCommand {
    OpenEffort,
    SetEffort(ReasoningEffort),
    OpenModel,
    SetModel(Model),
    Invalid(String),
}

impl SettingsCommand {
    pub(super) fn parse(input: &str) -> Option<Self> {
        let mut parts = input.split_whitespace();
        let command = parts.next()?;
        match command {
            "/model" => {
                let Some(argument) = parts.next() else {
                    return Some(Self::OpenModel);
                };
                if parts.next().is_some() {
                    return Some(Self::Invalid(
                        "Usage: /model [sol|terra|luna|astra]".to_owned(),
                    ));
                }
                Some(match argument.parse() {
                    Ok(model) => Self::SetModel(model),
                    Err(error) => Self::Invalid(error),
                })
            }
            "/effort" | "/reasoning" | "/thinking" => {
                let Some(argument) = parts.next() else {
                    return Some(Self::OpenEffort);
                };
                if parts.next().is_some() {
                    return Some(Self::Invalid(
                        "Usage: /thinking [low|medium|high|xhigh|max]".to_owned(),
                    ));
                }
                Some(match argument.parse() {
                    Ok(effort) => Self::SetEffort(effort),
                    Err(error) => Self::Invalid(error),
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComposerChromeTarget {
    Effort,
    Model,
    Subagents,
}

pub(crate) enum ComposerEvent {
    Terminal(Event),
    PasteImage(String),
    ContextTokens(u64),
    ReplaceRange {
        range: Range<usize>,
        text: String,
    },
    ReplaceDraft(String),
    SetEffort(ReasoningEffort),
    SetModel(Model),
    SetReasoningMode(ReasoningMode),
    SetFastMode(bool),
    InputMode(Option<String>),
    LiveControls(bool),
    Activity {
        active: bool,
        status: Option<String>,
        now: Instant,
    },
    ReviewWaiting {
        waiting: bool,
        status: Option<String>,
        now: Instant,
    },
    ActiveSubagents {
        count: usize,
        now: Instant,
    },
    TurnStarted {
        elapsed: Duration,
        now: Instant,
    },
    TurnFinished,
    TurnsCleared,
    AnimationFrame(Instant),
}

pub(crate) struct Composer {
    draft: String,
    images: Vec<PastedImage>,
    next_image: u64,
    cursor: usize,
    preferred_column: Option<usize>,
    scroll: usize,
    last_width: usize,
    context_tokens: u64,
    workspace: String,
    thinking: ReasoningEffort,
    model: Model,
    reasoning_mode: ReasoningMode,
    fast_mode: bool,
    input_mode: Option<String>,
    live_controls: bool,
    activity_active: bool,
    activity_wave: Option<WavedText>,
    activity_status: Option<String>,
    review_wave: Option<WavedText>,
    review_status: Option<String>,
    active_subagents: usize,
    subagent_wave: Option<WavedText>,
    turn_timers: VecDeque<TurnTimer>,
    effort_hit_area: Option<Rect>,
    model_hit_area: Option<Rect>,
    subagent_hit_area: Option<Rect>,
    layout: Option<CachedLayout>,
    history: PromptHistory,
}

pub(crate) struct ComposerDraft {
    text: String,
    images: Vec<PastedImage>,
    next_image: u64,
    cursor: usize,
}

struct PastedImage {
    range: Range<usize>,
    data_url: String,
}

struct CachedLayout {
    width: usize,
    cursor: usize,
    value: VisualLayout,
}

struct TurnTimer {
    observed_at: Instant,
    elapsed_at_observation: Duration,
    displayed_seconds: u64,
}

impl TurnTimer {
    fn new(elapsed: Duration, now: Instant) -> Self {
        Self {
            observed_at: now,
            elapsed_at_observation: elapsed,
            displayed_seconds: elapsed.as_secs(),
        }
    }

    fn advance(&mut self, now: Instant) -> bool {
        let displayed_seconds = self.elapsed(now).as_secs();
        if self.displayed_seconds == displayed_seconds {
            return false;
        }
        self.displayed_seconds = displayed_seconds;
        true
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.elapsed_at_observation
            .saturating_add(now.saturating_duration_since(self.observed_at))
    }

    fn deadline(&self) -> Instant {
        let elapsed_subsecond = self.elapsed_at_observation.subsec_nanos();
        let until_next_second = Duration::from_nanos(1_000_000_000 - u64::from(elapsed_subsecond));
        self.observed_at
            + until_next_second
            + Duration::from_secs(
                self.displayed_seconds
                    .saturating_sub(self.elapsed_at_observation.as_secs()),
            )
    }

    fn label(&self) -> String {
        format_turn_duration(self.displayed_seconds.saturating_mul(1_000_000_000))
    }
}

pub(crate) struct ComposerUpdate {
    pub(crate) effect: Option<ComposerEffect>,
    pub(crate) changed: bool,
}

impl Composer {
    pub(crate) fn new(workspace: &Path, thinking: ReasoningEffort) -> Self {
        Self {
            draft: String::new(),
            images: Vec::new(),
            next_image: 1,
            cursor: 0,
            preferred_column: None,
            scroll: 0,
            last_width: 78,
            context_tokens: 0,
            workspace: shorten_home(workspace),
            thinking,
            model: Model::default(),
            reasoning_mode: ReasoningMode::Standard,
            fast_mode: false,
            input_mode: None,
            live_controls: false,
            activity_active: false,
            activity_wave: None,
            activity_status: None,
            review_wave: None,
            review_status: None,
            active_subagents: 0,
            subagent_wave: None,
            turn_timers: VecDeque::new(),
            effort_hit_area: None,
            model_hit_area: None,
            subagent_hit_area: None,
            layout: None,
            history: PromptHistory::default(),
        }
    }

    pub(crate) const fn context_tokens(&self) -> u64 {
        self.context_tokens
    }

    pub(crate) fn update(&mut self, event: ComposerEvent) -> ComposerUpdate {
        match event {
            ComposerEvent::Terminal(Event::Key(key)) => self.handle_key(key),
            ComposerEvent::Terminal(Event::Paste(text)) => {
                self.history.detach();
                self.insert(&text);
                ComposerUpdate::changed()
            }
            ComposerEvent::Terminal(_) => ComposerUpdate::unchanged(),
            ComposerEvent::PasteImage(data_url) => {
                self.history.detach();
                self.insert_image(data_url);
                ComposerUpdate::changed()
            }
            ComposerEvent::ContextTokens(tokens) => {
                if self.context_tokens == tokens {
                    return ComposerUpdate::unchanged();
                }
                self.context_tokens = tokens;
                ComposerUpdate::changed()
            }
            ComposerEvent::ReplaceRange { range, text } => {
                self.history.detach();
                self.remove_range(range);
                self.insert(&text);
                ComposerUpdate::changed()
            }
            ComposerEvent::ReplaceDraft(draft) => {
                self.history.detach();
                self.replace_draft(draft);
                ComposerUpdate::changed()
            }
            ComposerEvent::SetEffort(effort) => {
                if self.thinking == effort {
                    return ComposerUpdate::unchanged();
                }
                self.thinking = effort;
                ComposerUpdate::changed()
            }
            ComposerEvent::SetModel(model) => {
                if self.model == model {
                    return ComposerUpdate::unchanged();
                }
                self.model = model;
                ComposerUpdate::changed()
            }
            ComposerEvent::SetReasoningMode(mode) => {
                if self.reasoning_mode == mode {
                    return ComposerUpdate::unchanged();
                }
                self.reasoning_mode = mode;
                ComposerUpdate::changed()
            }
            ComposerEvent::SetFastMode(enabled) => {
                if self.fast_mode == enabled {
                    return ComposerUpdate::unchanged();
                }
                self.fast_mode = enabled;
                ComposerUpdate::changed()
            }
            ComposerEvent::InputMode(mode) => {
                if self.input_mode == mode {
                    return ComposerUpdate::unchanged();
                }
                self.input_mode = mode;
                ComposerUpdate::changed()
            }
            ComposerEvent::LiveControls(active) => {
                if self.live_controls == active {
                    return ComposerUpdate::unchanged();
                }
                self.live_controls = active;
                ComposerUpdate::changed()
            }
            ComposerEvent::Activity {
                active,
                status,
                now,
            } => {
                let status = if active { status } else { None };
                if self.activity_active == active && self.activity_status == status {
                    return ComposerUpdate::unchanged();
                }
                self.activity_active = active;
                self.activity_wave = status.as_ref().map(|status| {
                    let mut wave = WavedText::new(status, Color::Cyan);
                    wave.set_active(true, now);
                    wave
                });
                self.activity_status = status;
                ComposerUpdate::changed()
            }
            ComposerEvent::ReviewWaiting {
                waiting,
                status,
                now,
            } => {
                let status =
                    waiting.then(|| status.unwrap_or_else(|| "Waiting for review…".to_owned()));
                if self.review_status == status {
                    return ComposerUpdate::unchanged();
                }
                self.review_wave = status.as_ref().map(|status| {
                    let mut wave = WavedText::new(status, Color::Green);
                    wave.set_active(true, now);
                    wave
                });
                self.review_status = status;
                ComposerUpdate::changed()
            }
            ComposerEvent::ActiveSubagents { count, now } => {
                if self.active_subagents == count {
                    return ComposerUpdate::unchanged();
                }
                self.active_subagents = count;
                self.subagent_wave = (count > 0).then(|| {
                    let mut wave = WavedText::new(format!("{count} subagents"), Color::Yellow);
                    wave.set_active(true, now);
                    wave
                });
                ComposerUpdate::changed()
            }
            ComposerEvent::TurnStarted { elapsed, now } => {
                self.turn_timers.push_back(TurnTimer::new(elapsed, now));
                ComposerUpdate::changed()
            }
            ComposerEvent::TurnFinished => {
                if self.turn_timers.pop_front().is_none() {
                    return ComposerUpdate::unchanged();
                }
                ComposerUpdate::changed()
            }
            ComposerEvent::TurnsCleared => {
                if self.turn_timers.is_empty() {
                    return ComposerUpdate::unchanged();
                }
                self.turn_timers.clear();
                ComposerUpdate::changed()
            }
            ComposerEvent::AnimationFrame(now) => {
                let activity_changed = self
                    .activity_wave
                    .as_mut()
                    .is_some_and(|wave| wave.advance(now));
                let review_changed = self
                    .review_wave
                    .as_mut()
                    .is_some_and(|wave| wave.advance(now));
                let subagent_changed = self
                    .subagent_wave
                    .as_mut()
                    .is_some_and(|wave| wave.advance(now));
                let mut timer_changed = false;
                for timer in &mut self.turn_timers {
                    timer_changed |= timer.advance(now);
                }
                ComposerUpdate::from_change(
                    activity_changed || review_changed || subagent_changed || timer_changed,
                )
            }
        }
    }

    pub(super) fn chrome_target(&self, position: Position) -> Option<ComposerChromeTarget> {
        if self
            .subagent_hit_area
            .is_some_and(|area| area.contains(position))
        {
            return Some(ComposerChromeTarget::Subagents);
        }
        if self
            .model_hit_area
            .is_some_and(|area| area.contains(position))
        {
            return Some(ComposerChromeTarget::Model);
        }
        self.effort_hit_area
            .is_some_and(|area| area.contains(position))
            .then_some(ComposerChromeTarget::Effort)
    }

    pub(crate) fn animation_deadline(&self) -> Option<Instant> {
        self.activity_wave
            .as_ref()
            .and_then(WavedText::animation_deadline)
            .into_iter()
            .chain(
                self.review_wave
                    .as_ref()
                    .and_then(WavedText::animation_deadline),
            )
            .chain(
                self.subagent_wave
                    .as_ref()
                    .and_then(WavedText::animation_deadline),
            )
            .chain(self.turn_timers.iter().map(TurnTimer::deadline))
            .min()
    }

    pub(crate) fn desired_height(&mut self, width: u16) -> u16 {
        if width < 2 {
            return 1;
        }

        let content_width = usize::from(width.saturating_sub(2)).max(1);
        let rows = self
            .visual_layout(content_width)
            .lines
            .len()
            .clamp(MIN_CONTENT_ROWS, MAX_CONTENT_ROWS);
        u16::try_from(rows + 2).unwrap_or(u16::MAX)
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.render_focused(frame, area, theme, true);
    }

    pub(crate) fn render_focused(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: bool,
    ) {
        self.render_focused_with_selection(frame, area, theme, focused, None);
    }

    pub(super) fn render_focused_with_selection(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: bool,
        selection: Option<TextRange>,
    ) {
        if area.is_empty() {
            return;
        }
        if area.width < 2 || area.height < 3 {
            self.render_narrow(frame, area, theme, focused, selection);
            return;
        }

        let content_width = usize::from(area.width - 2).max(1);
        self.last_width = content_width;
        let (cursor_row, cursor_column, line_count) = {
            let layout = self.visual_layout(content_width);
            (layout.cursor_row, layout.cursor_column, layout.lines.len())
        };
        let visible_rows = usize::from(area.height - 2);
        if selection.is_none() {
            self.keep_cursor_visible(cursor_row, visible_rows, line_count);
        } else {
            self.clamp_scroll(visible_rows, line_count);
        }

        let buffer = frame.buffer_mut();
        buffer.set_style(area, Style::default().fg(theme.text()));
        self.render_chrome(buffer, area, theme);
        let border = self.border_style(theme);

        for row in 0..visible_rows {
            let y = area.y + 1 + u16::try_from(row).unwrap_or(u16::MAX);
            draw_symbol(buffer, area.x, y, "│", border);
            draw_symbol(buffer, area.right() - 1, y, "│", border);

            let Some(line) = self
                .layout
                .as_ref()
                .and_then(|cached| cached.value.lines.get(self.scroll + row))
            else {
                continue;
            };
            render_draft_line(
                buffer,
                Position::new(area.x + 1, y),
                &self.draft,
                &self.images,
                line.start..line.end,
                content_width,
                theme,
            );
            if let Some(selection) = selection {
                render_selection(
                    buffer,
                    Position::new(area.x + 1, y),
                    &self.draft,
                    line.start..line.end,
                    selection,
                    content_width,
                );
            }
        }

        let cursor_row = cursor_row.saturating_sub(self.scroll);
        let cursor_x = area.x + 1 + u16::try_from(cursor_column).unwrap_or(u16::MAX);
        let cursor_y = area.y + 1 + u16::try_from(cursor_row).unwrap_or(u16::MAX);
        let max_cursor_x = area.right().saturating_sub(2);
        if focused && selection.is_none() {
            frame.set_cursor_position(Position::new(cursor_x.min(max_cursor_x), cursor_y));
        }
    }

    pub(super) fn selection_span(&mut self, position: Position, area: Rect) -> Option<TextSpan> {
        if area.is_empty() {
            return None;
        }

        let width = usize::from(area.width).max(1);
        self.last_width = width;
        let position = Position::new(
            position.x.clamp(area.x, area.right().saturating_sub(1)),
            position.y.clamp(area.y, area.bottom().saturating_sub(1)),
        );
        let row = self.scroll + usize::from(position.y - area.y);
        let column = usize::from(position.x - area.x);
        let line = self.visual_layout(width).lines.get(row)?.clone();
        let range = grapheme_at_column(&self.draft, &line, column);
        Some(TextSpan::new(0, range.start, range.end))
    }

    pub(super) fn selection_text(&self, selection: TextRange) -> Option<String> {
        let range = selection.source_range(0, self.draft.len())?;
        self.draft.get(range).map(ToOwned::to_owned)
    }

    pub(super) fn scroll_selection(&mut self, rows: isize, area: Rect) -> bool {
        if area.is_empty() {
            return false;
        }

        let width = usize::from(area.width).max(1);
        self.last_width = width;
        let line_count = self.visual_layout(width).lines.len();
        let visible_rows = usize::from(area.height);
        let maximum = line_count.saturating_sub(visible_rows);
        let scroll = self.scroll.saturating_add_signed(rows).min(maximum);
        if scroll == self.scroll {
            return false;
        }
        self.scroll = scroll;
        true
    }

    pub(crate) fn draft(&self) -> &str {
        &self.draft
    }

    pub(crate) fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    pub(crate) fn input_mode(&self) -> Option<&str> {
        self.input_mode.as_deref()
    }

    pub(crate) const fn effort(&self) -> ReasoningEffort {
        self.thinking
    }

    pub(crate) const fn model(&self) -> Model {
        self.model
    }

    pub(crate) const fn fast_mode(&self) -> bool {
        self.fast_mode
    }

    pub(crate) const fn reasoning_mode(&self) -> ReasoningMode {
        self.reasoning_mode
    }

    pub(crate) const fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn cursor_is_at_token_boundary(&self) -> bool {
        self.draft[..self.cursor]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
    }

    pub(crate) fn replace_draft(&mut self, draft: String) {
        self.draft = if draft.contains('\r') {
            normalize_line_endings(&draft).into_owned()
        } else {
            draft
        };
        self.images.clear();
        self.next_image = 1;
        self.cursor = self.draft.len();
        self.preferred_column = None;
        self.scroll = 0;
        self.layout = None;
    }

    pub(crate) fn take_submission(&mut self) -> Option<Submission> {
        let trimmed = self.draft.trim();
        if trimmed.is_empty() {
            return None;
        }

        let start = self.draft.len() - self.draft.trim_start().len();
        let end = start + trimmed.len();
        let text = trimmed.to_owned();
        let images = self
            .images
            .iter()
            .filter(|image| image.range.start >= start && image.range.end <= end)
            .map(|image| {
                (
                    image.range.start - start..image.range.end - start,
                    image.data_url.clone(),
                )
            });
        let prompt = Submission::multimodal(text, images);
        self.replace_draft(String::new());
        Some(prompt)
    }

    pub(crate) fn take_draft(&mut self) -> Option<ComposerDraft> {
        if self.draft.is_empty() {
            return None;
        }

        let draft = ComposerDraft {
            text: mem::take(&mut self.draft),
            images: mem::take(&mut self.images),
            next_image: mem::replace(&mut self.next_image, 1),
            cursor: mem::take(&mut self.cursor),
        };
        self.history.detach();
        self.preferred_column = None;
        self.scroll = 0;
        self.layout = None;
        Some(draft)
    }

    pub(crate) fn restore_draft(&mut self, draft: ComposerDraft) {
        self.draft = draft.text;
        self.images = draft.images;
        self.next_image = draft.next_image;
        self.cursor = draft.cursor;
        self.history.detach();
        self.preferred_column = None;
        self.scroll = 0;
        self.layout = None;
    }

    fn handle_key(&mut self, key: KeyEvent) -> ComposerUpdate {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComposerUpdate::unchanged();
        }

        if key.modifiers == KeyModifiers::CONTROL {
            if matches!(
                key.code,
                KeyCode::Char('a' | 'b' | 'd' | 'e' | 'f' | 'h' | 'j' | 'k' | 'u' | 'w')
                    | KeyCode::Left
                    | KeyCode::Right
            ) {
                self.history.detach();
            }
            return match key.code {
                KeyCode::Char('a') => ComposerUpdate::from_change(self.move_to_logical_edge(false)),
                KeyCode::Char('b') => ComposerUpdate::from_change(self.move_left()),
                KeyCode::Char('d') => ComposerUpdate::from_change(self.delete()),
                KeyCode::Char('e') => ComposerUpdate::from_change(self.move_to_logical_edge(true)),
                KeyCode::Char('f') => ComposerUpdate::from_change(self.move_right()),
                KeyCode::Char('g') => {
                    ComposerUpdate::effect(ComposerEffect::OpenDraftEditor, false)
                }
                KeyCode::Char('j') => {
                    self.history.detach();
                    self.insert("\n");
                    ComposerUpdate::changed()
                }
                KeyCode::Char('h') => ComposerUpdate::from_change(self.backspace()),
                KeyCode::Char('k') => {
                    ComposerUpdate::from_change(self.delete_to_logical_edge(true))
                }
                KeyCode::Char('n') => ComposerUpdate::from_change(self.move_down()),
                KeyCode::Char('p') => ComposerUpdate::from_change(self.move_up()),
                KeyCode::Char('u') => {
                    ComposerUpdate::from_change(self.delete_to_logical_edge(false))
                }
                KeyCode::Char('w') => ComposerUpdate::from_change(self.delete_word_before_cursor()),
                KeyCode::Left => ComposerUpdate::from_change(self.move_by_word(false)),
                KeyCode::Right => ComposerUpdate::from_change(self.move_by_word(true)),
                _ => ComposerUpdate::unchanged(),
            };
        }
        // Prevent unsupported Ctrl chords from falling through as text input.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return ComposerUpdate::unchanged();
        }

        let detaches_history = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Backspace
                | KeyCode::Delete
        ) || key.code == KeyCode::Enter
            && key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        if detaches_history {
            self.history.detach();
        }

        match key.code {
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert("\n");
                ComposerUpdate::changed()
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Tab if key.modifiers.is_empty() => self.queue(),
            KeyCode::Char('b') if key.modifiers == KeyModifiers::ALT => {
                ComposerUpdate::from_change(self.move_by_word(false))
            }
            KeyCode::Char('f') if key.modifiers == KeyModifiers::ALT => {
                ComposerUpdate::from_change(self.move_by_word(true))
            }
            KeyCode::Left if key.modifiers == KeyModifiers::ALT => {
                ComposerUpdate::from_change(self.move_by_word(false))
            }
            KeyCode::Right if key.modifiers == KeyModifiers::ALT => {
                ComposerUpdate::from_change(self.move_by_word(true))
            }
            KeyCode::Char(character) => {
                self.insert(&character.to_string());
                ComposerUpdate::changed()
            }
            KeyCode::Left => ComposerUpdate::from_change(self.move_left()),
            KeyCode::Right => ComposerUpdate::from_change(self.move_right()),
            KeyCode::Up => ComposerUpdate::from_change(self.move_up()),
            KeyCode::Down => ComposerUpdate::from_change(self.move_down()),
            KeyCode::Home => ComposerUpdate::from_change(self.move_to_visual_edge(false)),
            KeyCode::End => ComposerUpdate::from_change(self.move_to_visual_edge(true)),
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
                ComposerUpdate::from_change(self.delete_word_before_cursor())
            }
            KeyCode::Backspace => ComposerUpdate::from_change(self.backspace()),
            KeyCode::Delete => ComposerUpdate::from_change(self.delete()),
            _ => ComposerUpdate::unchanged(),
        }
    }

    fn submit(&mut self) -> ComposerUpdate {
        if self.draft.trim().is_empty() {
            return ComposerUpdate::unchanged();
        }

        if let Some(command) = self.take_settings_command() {
            return command;
        }

        let trimmed = self.draft.trim();
        if self.images.is_empty() && self.draft.starts_with('!') {
            let command = trimmed.trim_start_matches('!').trim().to_owned();
            if command.is_empty() {
                return ComposerUpdate::unchanged();
            }
            self.history.record(format!("!{command}"));
            self.replace_draft(String::new());
            return ComposerUpdate::effect(ComposerEffect::RunShell(command), true);
        }

        let prompt = self
            .take_submission()
            .expect("non-empty composer draft must produce a submission");
        self.history.record(prompt.display_text().to_owned());
        ComposerUpdate::effect(ComposerEffect::Submit(prompt), true)
    }

    fn queue(&mut self) -> ComposerUpdate {
        if let Some(command) = self.take_settings_command() {
            return command;
        }
        let Some(prompt) = self.take_submission() else {
            return ComposerUpdate::unchanged();
        };
        self.history.record(prompt.display_text().to_owned());
        ComposerUpdate::effect(ComposerEffect::Queue(prompt), true)
    }

    fn take_settings_command(&mut self) -> Option<ComposerUpdate> {
        if !self.images.is_empty() {
            return None;
        }
        let command = SettingsCommand::parse(self.draft.trim())?;
        self.history.record(self.draft.trim().to_owned());
        self.replace_draft(String::new());
        Some(ComposerUpdate::effect(
            ComposerEffect::Settings(command),
            true,
        ))
    }

    fn move_up(&mut self) -> bool {
        if !self.history.is_browsing() && self.move_vertical(-1) {
            return true;
        }

        let Some(prompt) = self.history.previous(&self.draft) else {
            return false;
        };
        self.replace_draft(prompt);
        true
    }

    fn move_down(&mut self) -> bool {
        if !self.history.is_browsing() {
            return self.move_vertical(1);
        }

        let Some(prompt) = self.history.next() else {
            return false;
        };
        self.replace_draft(prompt);
        true
    }

    fn insert(&mut self, text: &str) {
        let text = normalize_line_endings(text);
        self.move_cursor_out_of_image();
        for image in &mut self.images {
            if image.range.start >= self.cursor {
                image.range.start += text.len();
                image.range.end += text.len();
            }
        }
        self.draft.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.preferred_column = None;
        self.layout = None;
    }

    fn move_left(&mut self) -> bool {
        let Some(previous) = self.draft[..self.cursor].grapheme_indices(true).next_back() else {
            return false;
        };
        self.cursor = self
            .images
            .iter()
            .find(|image| image.range.contains(&previous.0))
            .map_or(previous.0, |image| image.range.start);
        self.preferred_column = None;
        true
    }

    fn move_right(&mut self) -> bool {
        let Some(next) = self.draft[self.cursor..].graphemes(true).next() else {
            return false;
        };
        let target = self.cursor + next.len();
        self.cursor = self
            .images
            .iter()
            .find(|image| image.range.start < target && target < image.range.end)
            .map_or(target, |image| image.range.end);
        self.preferred_column = None;
        true
    }

    fn backspace(&mut self) -> bool {
        if let Some(index) = self
            .images
            .iter()
            .position(|image| image.range.start < self.cursor && self.cursor <= image.range.end)
        {
            let range = self.images.remove(index).range;
            self.remove_range(range);
            return true;
        }
        let Some(previous) = self.draft[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map(|(index, _)| index)
        else {
            return false;
        };
        self.remove_range(previous..self.cursor);
        true
    }

    fn delete_word_before_cursor(&mut self) -> bool {
        let mut start = self.cursor;
        while let Some((index, character)) = self.draft[..start].char_indices().next_back() {
            if !character.is_whitespace() {
                break;
            }
            start = index;
        }
        while let Some((index, character)) = self.draft[..start].char_indices().next_back() {
            if character.is_whitespace() {
                break;
            }
            start = index;
        }
        let end = self.cursor;
        if let Some(image_start) = self
            .images
            .iter()
            .filter(|image| image.range.start < end && start < image.range.end)
            .map(|image| image.range.start)
            .min()
        {
            start = image_start;
        }
        if start == end {
            return false;
        }

        self.images
            .retain(|image| image.range.end <= start || image.range.start >= end);
        self.remove_range(start..end);
        true
    }

    fn delete_to_logical_edge(&mut self, end: bool) -> bool {
        let target = if end {
            let line_end = self.draft[self.cursor..]
                .find('\n')
                .map_or(self.draft.len(), |offset| self.cursor + offset);
            if line_end == self.cursor && line_end < self.draft.len() {
                line_end + 1
            } else {
                line_end
            }
        } else {
            let line_start = self.draft[..self.cursor]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            if line_start == self.cursor && line_start > 0 {
                line_start - 1
            } else {
                line_start
            }
        };
        let range = if end {
            self.cursor..target
        } else {
            target..self.cursor
        };
        if range.is_empty() {
            return false;
        }
        self.images
            .retain(|image| image.range.end <= range.start || image.range.start >= range.end);
        self.remove_range(range);
        true
    }

    fn move_by_word(&mut self, forward: bool) -> bool {
        let is_word = |grapheme: &str| grapheme.chars().any(char::is_alphanumeric);
        let target = if forward {
            let mut found_word = false;
            let mut target = self.draft.len();
            for (offset, grapheme) in self.draft[self.cursor..].grapheme_indices(true) {
                if is_word(grapheme) {
                    found_word = true;
                    target = self.cursor + offset + grapheme.len();
                } else if found_word {
                    break;
                }
            }
            target
        } else {
            let mut found_word = false;
            let mut target = 0;
            for (offset, grapheme) in self.draft[..self.cursor].grapheme_indices(true).rev() {
                if is_word(grapheme) {
                    found_word = true;
                    target = offset;
                } else if found_word {
                    break;
                }
            }
            target
        };

        let adjust_position_out_of_image = |position: usize, prefer_start: bool| {
            let Some(image) = self
                .images
                .iter()
                .find(|image| image.range.start < position && position < image.range.end)
            else {
                return position;
            };
            if prefer_start {
                image.range.start
            } else {
                image.range.end
            }
        };
        let target = adjust_position_out_of_image(target, !forward);
        if target == self.cursor {
            return false;
        }
        self.cursor = target;
        self.preferred_column = None;
        true
    }

    fn delete(&mut self) -> bool {
        if let Some(index) = self
            .images
            .iter()
            .position(|image| image.range.start <= self.cursor && self.cursor < image.range.end)
        {
            let range = self.images.remove(index).range;
            self.remove_range(range);
            return true;
        }
        let Some(next) = self.draft[self.cursor..].graphemes(true).next() else {
            return false;
        };
        self.remove_range(self.cursor..self.cursor + next.len());
        true
    }

    fn insert_image(&mut self, data_url: String) {
        self.move_cursor_out_of_image();
        let marker = format!("[Image #{}]", self.next_image);
        let start = self.cursor;
        self.insert(&marker);
        self.images.push(PastedImage {
            range: start..self.cursor,
            data_url,
        });
        self.images.sort_by_key(|image| image.range.start);
        self.next_image = self.next_image.saturating_add(1);
    }

    fn move_cursor_out_of_image(&mut self) {
        if let Some(image) = self
            .images
            .iter()
            .find(|image| image.range.start < self.cursor && self.cursor < image.range.end)
        {
            self.cursor = image.range.end;
        }
    }

    fn remove_range(&mut self, range: Range<usize>) {
        let removed = range.len();
        self.draft.drain(range.clone());
        for image in &mut self.images {
            if image.range.start >= range.end {
                image.range.start -= removed;
                image.range.end -= removed;
            }
        }
        self.cursor = range.start;
        self.preferred_column = None;
        self.layout = None;
    }

    fn move_vertical(&mut self, direction: isize) -> bool {
        let layout = VisualLayout::new(&self.draft, self.cursor, self.last_width.max(1));
        let target_row = layout.cursor_row.saturating_add_signed(direction);
        if target_row == layout.cursor_row || target_row >= layout.lines.len() {
            return false;
        }

        let desired = *self.preferred_column.get_or_insert(layout.cursor_column);
        let target = byte_at_column(&self.draft, &layout.lines[target_row], desired);
        self.cursor = self
            .images
            .iter()
            .find(|image| image.range.start < target && target < image.range.end)
            .map_or(target, |image| {
                if direction.is_negative() {
                    image.range.start
                } else {
                    image.range.end
                }
            });
        true
    }

    fn move_to_visual_edge(&mut self, end: bool) -> bool {
        let layout = VisualLayout::new(&self.draft, self.cursor, self.last_width.max(1));
        let line = &layout.lines[layout.cursor_row];
        let target = if end { line.end } else { line.start };
        if target == self.cursor {
            return false;
        }
        self.cursor = target;
        self.preferred_column = None;
        true
    }

    fn move_to_logical_edge(&mut self, end: bool) -> bool {
        let target = if end {
            self.draft[self.cursor..]
                .find('\n')
                .map_or(self.draft.len(), |offset| self.cursor + offset)
        } else {
            self.draft[..self.cursor]
                .rfind('\n')
                .map_or(0, |offset| offset + '\n'.len_utf8())
        };
        if target == self.cursor {
            return false;
        }
        self.cursor = target;
        self.preferred_column = None;
        true
    }

    fn keep_cursor_visible(&mut self, cursor_row: usize, visible: usize, line_count: usize) {
        if visible == 0 {
            self.scroll = 0;
            return;
        }
        if cursor_row < self.scroll {
            self.scroll = cursor_row;
        } else if cursor_row >= self.scroll + visible {
            self.scroll = cursor_row + 1 - visible;
        }

        self.clamp_scroll(visible, line_count);
    }

    fn clamp_scroll(&mut self, visible: usize, line_count: usize) {
        self.scroll = self.scroll.min(line_count.saturating_sub(visible));
    }

    fn visual_layout(&mut self, width: usize) -> &VisualLayout {
        let stale = self
            .layout
            .as_ref()
            .is_some_and(|cached| cached.width != width || cached.cursor != self.cursor);
        if stale {
            self.layout = None;
        }

        let cursor = self.cursor;
        let draft = &self.draft;
        let cached = self.layout.get_or_insert_with(|| CachedLayout {
            width,
            cursor,
            value: VisualLayout::new(draft, cursor, width),
        });
        &cached.value
    }

    fn render_narrow(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: bool,
        selection: Option<TextRange>,
    ) {
        let width = usize::from(area.width).max(1);
        let (cursor_row, cursor_column, line_count) = {
            let layout = self.visual_layout(width);
            (layout.cursor_row, layout.cursor_column, layout.lines.len())
        };
        if selection.is_none() {
            self.scroll = 0;
        } else {
            self.clamp_scroll(1, line_count);
        }
        let scroll = self.scroll;
        let line = self.visual_layout(width).lines[scroll].clone();

        let buffer = frame.buffer_mut();
        buffer.set_style(area, Style::default().fg(theme.text()));
        render_draft_line(
            buffer,
            Position::new(area.x, area.y),
            &self.draft,
            &self.images,
            line.start..line.end,
            width,
            theme,
        );
        if let Some(selection) = selection {
            render_selection(
                buffer,
                Position::new(area.x, area.y),
                &self.draft,
                line.start..line.end,
                selection,
                width,
            );
        }
        if focused && selection.is_none() {
            let cursor_column = if cursor_row == scroll {
                u16::try_from(cursor_column).unwrap_or(u16::MAX)
            } else {
                0
            };
            let cursor_x = area
                .x
                .saturating_add(cursor_column.min(area.width.saturating_sub(1)));
            frame.set_cursor_position(Position::new(cursor_x, area.y));
        }
    }

    fn render_chrome(&mut self, buffer: &mut Buffer, area: Rect, theme: &Theme) {
        self.effort_hit_area = None;
        self.model_hit_area = None;
        self.subagent_hit_area = None;
        let shell_mode = self.draft.starts_with('!');
        let border = self.border_style(theme);
        let top = area.y;
        let bottom = area.bottom() - 1;

        for x in area.x..area.right() {
            draw_symbol(buffer, x, top, "─", border);
            draw_symbol(buffer, x, bottom, "─", border);
        }
        draw_symbol(buffer, area.x, top, "╭", border);
        draw_symbol(buffer, area.right() - 1, top, "╮", border);
        draw_symbol(buffer, area.x, bottom, "╰", border);
        draw_symbol(buffer, area.right() - 1, bottom, "╯", border);

        if area.width < 4 {
            return;
        }

        let content_start = area.x + 2;
        let content_width = usize::from(area.width - 4);
        let content_end = content_start + u16::try_from(content_width).unwrap_or(u16::MAX);
        let usage_prefix = format!(" {}%/272k ", context_percent(self.context_tokens));
        let input_mode_segment = self
            .input_mode
            .as_ref()
            .map(|mode| format!("{mode} "))
            .unwrap_or_default();
        let review_segment = self
            .review_status
            .as_ref()
            .map(|status| format!("{status} "))
            .unwrap_or_default();
        let status_segment = self.activity_status.clone().unwrap_or_default();
        let subagent_segment = if self.active_subagents > 0 {
            format!(" {} subagents", self.active_subagents)
        } else {
            String::new()
        };
        let usage_before_activity = format!("{usage_prefix}{input_mode_segment}{review_segment}");
        let usage_before_subagents = self.activity_wave.as_ref().map_or_else(
            || usage_before_activity.clone(),
            |_| format!("{usage_before_activity}{status_segment} "),
        );
        let usage = if subagent_segment.is_empty() {
            usage_before_subagents.clone()
        } else {
            format!("{usage_before_subagents}{} ", subagent_segment.trim_start())
        };
        let model = format!(" {} ", self.model);
        let timer = self
            .turn_timers
            .front()
            .map(|timer| format!(" {} ", timer.label()))
            .unwrap_or_default();
        let effort = format!(" {} ", self.thinking.as_str());
        let fast_mode = self.fast_mode.then_some("⚡ ");
        let pro_mode = (self.reasoning_mode == ReasoningMode::Pro).then_some("pro ");
        let right_width = timer.width()
            + model.width()
            + effort.width()
            + fast_mode.map_or(0, UnicodeWidthStr::width)
            + pro_mode.map_or(0, UnicodeWidthStr::width);
        let right_start = content_start
            + u16::try_from(content_width.saturating_sub(right_width)).unwrap_or(u16::MAX);

        let usage_space = usize::from(right_start.saturating_sub(content_start)).saturating_sub(1);
        buffer.set_stringn(
            content_start,
            top,
            usage,
            usage_space,
            Style::default().fg(theme.muted()),
        );
        if let Some(wave) = &self.review_wave {
            let mut x = content_start + u16::try_from(usage_prefix.width()).unwrap_or(u16::MAX);
            for span in wave.spans() {
                if x >= right_start {
                    break;
                }
                let width = u16::try_from(span.width()).unwrap_or(u16::MAX);
                buffer.set_span(x, top, &span, right_start.saturating_sub(x));
                x = x.saturating_add(width);
            }
        }
        if let Some(wave) = &self.activity_wave {
            let mut x =
                content_start + u16::try_from(usage_before_activity.width()).unwrap_or(u16::MAX);
            for span in wave.spans() {
                if x >= right_start {
                    break;
                }
                let width = u16::try_from(span.width()).unwrap_or(u16::MAX);
                buffer.set_span(x, top, &span, right_start.saturating_sub(x));
                x = x.saturating_add(width);
            }
        }
        if let Some(wave) = &self.subagent_wave {
            let wave_x =
                content_start + u16::try_from(usage_before_subagents.width()).unwrap_or(u16::MAX);
            let wave_width =
                u16::try_from(wave.spans().iter().map(|span| span.width()).sum::<usize>())
                    .unwrap_or(u16::MAX)
                    .min(right_start.saturating_sub(wave_x));
            if wave_width > 0 {
                self.subagent_hit_area = Some(Rect::new(wave_x, top, wave_width, 1));
            }
            let mut x = wave_x;
            for span in wave.spans() {
                if x >= right_start {
                    break;
                }
                let width = u16::try_from(span.width()).unwrap_or(u16::MAX);
                buffer.set_span(x, top, &span, right_start.saturating_sub(x));
                x = x.saturating_add(width);
            }
        }
        buffer.set_stringn(
            right_start,
            top,
            &timer,
            usize::from(content_end.saturating_sub(right_start)),
            Style::default().fg(theme.muted()),
        );
        let model_start = right_start + u16::try_from(timer.width()).unwrap_or(u16::MAX);
        if model_start < content_end {
            self.model_hit_area = Some(Rect::new(
                model_start,
                top,
                u16::try_from(model.width())
                    .unwrap_or(u16::MAX)
                    .min(content_end.saturating_sub(model_start)),
                1,
            ));
        }
        buffer.set_stringn(
            model_start,
            top,
            &model,
            usize::from(content_end.saturating_sub(model_start)),
            Style::default().fg(theme.model(self.model)),
        );
        let effort_start = model_start + u16::try_from(model.width()).unwrap_or(u16::MAX);
        if effort_start < content_end {
            self.effort_hit_area = Some(Rect::new(
                effort_start,
                top,
                u16::try_from(effort.width())
                    .unwrap_or(u16::MAX)
                    .min(content_end.saturating_sub(effort_start)),
                1,
            ));
            buffer.set_stringn(
                effort_start,
                top,
                &effort,
                usize::from(content_end - effort_start),
                Style::default()
                    .fg(theme.effort(self.thinking))
                    .add_modifier(Modifier::BOLD),
            );
        }
        let fast_mode_start = effort_start + u16::try_from(effort.width()).unwrap_or(u16::MAX);
        let fast_mode_width =
            u16::try_from(fast_mode.map_or(0, UnicodeWidthStr::width)).unwrap_or(u16::MAX);
        let natural_pro_mode_start = fast_mode_start + fast_mode_width;
        let pro_mode_width =
            u16::try_from(pro_mode.map_or(0, UnicodeWidthStr::width)).unwrap_or(u16::MAX);
        let pro_mode_start = if pro_mode.is_some() {
            natural_pro_mode_start.min(
                content_end
                    .saturating_sub(pro_mode_width)
                    .max(content_start),
            )
        } else {
            natural_pro_mode_start
        };
        if let Some(fast_mode) = fast_mode
            && fast_mode_start < content_end
            && fast_mode_start.saturating_add(fast_mode_width) <= pro_mode_start
        {
            buffer.set_stringn(
                fast_mode_start,
                top,
                fast_mode,
                usize::from(content_end - fast_mode_start),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        }
        if let Some(pro_mode) = pro_mode
            && pro_mode_start < content_end
        {
            buffer.set_stringn(
                pro_mode_start,
                top,
                pro_mode,
                usize::from(content_end - pro_mode_start),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            );
        }
        let directory = format!(" {} ", self.workspace);
        let directory_width = directory.width().min(content_width);
        let directory_start =
            content_end.saturating_sub(u16::try_from(directory_width).unwrap_or(u16::MAX));
        let development_width = if crate::installation::current().is_development()
            && DEVELOPMENT_BADGE.width()
                <= usize::from(directory_start.saturating_sub(content_start))
        {
            DEVELOPMENT_BADGE.width()
        } else {
            0
        };
        let development_start =
            directory_start.saturating_sub(u16::try_from(development_width).unwrap_or(u16::MAX));
        let hint_space = usize::from(development_start.saturating_sub(content_start));
        let entry_hint = entry_hint(
            theme,
            self.draft.is_empty(),
            self.activity_active,
            self.live_controls,
            hint_space,
        );
        if entry_hint.width() <= hint_space {
            buffer.set_line(
                content_start,
                bottom,
                &entry_hint,
                u16::try_from(hint_space).unwrap_or(u16::MAX),
            );
        }
        if shell_mode {
            buffer.set_stringn(
                content_start,
                bottom,
                " shell ",
                hint_space,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        }
        if development_width > 0 {
            buffer.set_stringn(
                development_start,
                bottom,
                DEVELOPMENT_BADGE,
                development_width,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            );
        }
        buffer.set_stringn(
            directory_start,
            bottom,
            directory,
            directory_width,
            Style::default().fg(theme.muted()),
        );
    }

    fn border_style(&self, theme: &Theme) -> Style {
        Style::default().fg(if self.review_wave.is_some() {
            Color::Green
        } else if self.draft.starts_with('!') {
            Color::Yellow
        } else {
            theme.border()
        })
    }
}

fn entry_hint(
    theme: &Theme,
    include_actions: bool,
    activity_active: bool,
    live_controls: bool,
    max_width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    if live_controls {
        spans.extend([
            Span::styled("Enter", Style::reset()),
            Span::styled(" steer · ", Style::default().fg(theme.muted())),
            Span::styled("Tab", Style::reset()),
            Span::styled(" queue · ", Style::default().fg(theme.muted())),
            Span::styled("Esc Esc", Style::reset()),
            Span::styled(" stop ", Style::default().fg(theme.muted())),
        ]);
        return Line::from(spans);
    }
    if activity_active {
        spans.push(Span::styled(
            "remote · read only ",
            Style::default().fg(theme.muted()),
        ));
        return Line::from(spans);
    }
    let send_start = spans.len();
    spans.extend([
        Span::styled("Enter", Style::reset()),
        Span::styled(" send · ", Style::default().fg(theme.muted())),
    ]);
    if include_actions {
        spans.extend([
            Span::styled("/", Style::reset()),
            Span::styled(" actions · ", Style::default().fg(theme.muted())),
        ]);
    }
    spans.extend([
        Span::styled("@", Style::reset()),
        Span::styled(" paths · ", Style::default().fg(theme.muted())),
        Span::styled("@@", Style::reset()),
        Span::styled(" sessions ", Style::default().fg(theme.muted())),
    ]);
    let full = Line::from(spans.clone());
    if full.width() <= max_width {
        return full;
    }
    spans.drain(send_start..send_start + 2);
    Line::from(spans)
}

impl Component for Composer {
    type Event = ComposerEvent;
    type Effect = ComposerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        let update = Self::update(self, event);
        ComponentUpdate {
            effects: update.effect.into_iter().collect(),
            render: if update.changed {
                RenderRequest::Immediate
            } else {
                RenderRequest::None
            },
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        Self::render(self, frame, area, theme);
    }
}

impl ComposerUpdate {
    fn unchanged() -> Self {
        Self {
            effect: None,
            changed: false,
        }
    }

    fn changed() -> Self {
        Self {
            effect: None,
            changed: true,
        }
    }

    fn effect(effect: ComposerEffect, changed: bool) -> Self {
        Self {
            effect: Some(effect),
            changed,
        }
    }

    fn from_change(changed: bool) -> Self {
        Self {
            effect: None,
            changed,
        }
    }
}

fn context_percent(tokens: u64) -> u64 {
    tokens
        .saturating_mul(100)
        .saturating_add(MODEL_WINDOW_TOKENS / 2)
        / MODEL_WINDOW_TOKENS
}

fn draw_symbol(buffer: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    buffer[(x, y)].set_symbol(symbol).set_style(style);
}

fn render_draft_line(
    buffer: &mut Buffer,
    position: Position,
    draft: &str,
    images: &[PastedImage],
    range: Range<usize>,
    width: usize,
    theme: &Theme,
) {
    let rendered = sanitize_terminal_text(&draft[range.clone()]);
    buffer.set_stringn(
        position.x,
        position.y,
        rendered,
        width,
        Style::default().fg(theme.text()),
    );
    for image in images {
        let start = image.range.start.max(range.start);
        let end = image.range.end.min(range.end);
        if start >= end {
            continue;
        }
        let offset = terminal_text_width(&draft[range.start..start]);
        buffer.set_stringn(
            position
                .x
                .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX)),
            position.y,
            &draft[start..end],
            width.saturating_sub(offset),
            Style::default().fg(Color::Blue),
        );
    }
}

fn render_selection(
    buffer: &mut Buffer,
    position: Position,
    draft: &str,
    line: Range<usize>,
    selection: TextRange,
    width: usize,
) {
    let Some(selected) = selection.source_range(0, draft.len()) else {
        return;
    };
    let start = selected.start.max(line.start);
    let end = selected.end.min(line.end);
    if start >= end {
        return;
    }
    let Some(prefix) = draft.get(line.start..start) else {
        return;
    };
    let Some(text) = draft.get(start..end) else {
        return;
    };
    let offset = terminal_text_width(prefix);
    let selected_width = terminal_text_width(text).min(width.saturating_sub(offset));
    if selected_width == 0 {
        return;
    }
    let x = position
        .x
        .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
    let width = u16::try_from(selected_width).unwrap_or(u16::MAX);
    buffer.set_style(
        Rect::new(x, position.y, width, 1),
        Style::reset().fg(Color::Black).bg(Color::Yellow),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        super::selection::{Selection, Surface, TextRange},
        Composer, ComposerEffect, ComposerEvent, SettingsCommand, context_percent,
    };
    use crate::{
        config::{ReasoningEffort, ReasoningMode},
        tui::theme::Theme,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use nanocodex::{
        Model,
        agent::input::{PromptInput, UserInput},
    };
    use ratatui::{
        Terminal,
        backend::TestBackend,
        layout::{Position, Rect},
        style::Color,
    };
    use std::{
        path::Path,
        time::{Duration, Instant},
    };
    use unicode_width::UnicodeWidthStr;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> ComposerEvent {
        ComposerEvent::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }

    fn render(composer: &mut Composer, width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| composer.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
    }

    fn render_with_selection(
        composer: &mut Composer,
        width: u16,
        height: u16,
        selection: TextRange,
    ) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render_focused_with_selection(
                    frame,
                    frame.area(),
                    &Theme::default(),
                    true,
                    Some(selection),
                );
            })
            .unwrap();
        terminal
    }

    fn rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(usize::from(buffer.area.width))
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    #[test]
    fn empty_composer_matches_the_pi_chrome() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        let terminal = render(&mut composer, 60, 5);
        let footer = if crate::installation::current().is_development() {
            "╰─ / actions · @ paths · @@ sessions ─────── ◉ dev  /work ─╯"
        } else {
            "╰─ / actions · @ paths · @@ sessions ───────────────── /work ─╯"
        };

        assert_eq!(
            rows(&terminal),
            [
                "╭─ 0%/272k ────────────────────────── gpt-5.6-sol  medium ─╮",
                "│                                                          │",
                "│                                                          │",
                "│                                                          │",
                footer,
            ]
        );

        let buffer = terminal.backend().buffer();
        let footer = &rows(&terminal)[4];
        let action_key =
            u16::try_from(footer[..footer.find("/ actions").unwrap()].width()).unwrap();
        let action_help = action_key + 2;
        assert_eq!(buffer[(action_key, 4)].fg, Color::Reset);
        assert_eq!(buffer[(action_help, 4)].fg, Theme::default().muted());
    }

    #[test]
    fn composer_chrome_uses_the_model_palette() {
        for (model, color) in [
            (Model::Luna, Color::White),
            (Model::Terra, Color::Green),
            (Model::Sol, Color::Yellow),
            (Model::Astra, Color::LightMagenta),
        ] {
            let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
            composer.update(ComposerEvent::SetModel(model));
            let terminal = render(&mut composer, 60, 5);
            let label = model.to_string().chars().collect::<Vec<_>>();
            let line = rows(&terminal)[0].chars().collect::<Vec<_>>();
            let start = line
                .windows(label.len())
                .position(|window| window == label)
                .unwrap();

            assert_eq!(
                terminal.backend().buffer()[(u16::try_from(start).unwrap(), 0)].fg,
                color
            );
        }
    }

    #[test]
    fn review_waiting_uses_green_chrome_next_to_context() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::ReviewWaiting {
            waiting: true,
            status: None,
            now: Instant::now(),
        });

        let terminal = render(&mut composer, 80, 5);

        assert!(rows(&terminal)[0].contains("0%/272k Waiting for review"));
        assert_eq!(terminal.backend().buffer()[(0, 0)].fg, Color::Green);
    }

    #[test]
    fn turn_timer_is_rendered_immediately_before_the_model() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        let started_at = Instant::now();
        composer.update(ComposerEvent::TurnStarted {
            elapsed: Duration::from_secs(65),
            now: started_at,
        });

        let terminal = render(&mut composer, 72, 5);
        assert!(rows(&terminal)[0].contains(" 1m 5s  gpt-5.6-sol "));

        let update = composer.update(ComposerEvent::AnimationFrame(
            started_at + Duration::from_secs(2),
        ));
        assert!(update.changed);
        let terminal = render(&mut composer, 72, 5);
        assert!(rows(&terminal)[0].contains(" 1m 7s  gpt-5.6-sol "));

        composer.update(ComposerEvent::TurnFinished);
        let terminal = render(&mut composer, 72, 5);
        assert!(!rows(&terminal)[0].contains("1m 7s"));
    }

    #[test]
    fn completing_one_run_keeps_the_next_active_run_timed() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        let now = Instant::now();
        composer.update(ComposerEvent::TurnStarted {
            elapsed: Duration::from_secs(65),
            now,
        });
        composer.update(ComposerEvent::TurnStarted {
            elapsed: Duration::from_secs(5),
            now,
        });
        composer.update(ComposerEvent::AnimationFrame(now + Duration::from_secs(2)));

        composer.update(ComposerEvent::TurnFinished);

        let terminal = render(&mut composer, 72, 5);
        assert!(rows(&terminal)[0].contains(" 7s  gpt-5.6-sol "));
        assert!(composer.animation_deadline().is_some());
    }

    #[test]
    fn fast_mode_places_a_yellow_bolt_after_effort() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::SetFastMode(true));

        let terminal = render(&mut composer, 60, 5);
        let top = &terminal.backend().buffer().content[..60];
        let rendered = top.iter().map(|cell| cell.symbol()).collect::<String>();
        let bolt = top
            .iter()
            .position(|cell| cell.symbol() == "⚡")
            .expect("fast mode should render its indicator");

        assert!(rendered.contains("medium ⚡"));
        assert_eq!(top[bolt].fg, Color::Yellow);
        assert!(top[bolt].modifier.contains(ratatui::style::Modifier::BOLD));
    }

    #[test]
    fn pro_mode_places_a_green_badge_after_the_fast_mode_bolt() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::SetFastMode(true));
        composer.update(ComposerEvent::SetReasoningMode(ReasoningMode::Pro));

        let terminal = render(&mut composer, 60, 5);
        let top = &terminal.backend().buffer().content[..60];
        let rendered = top.iter().map(|cell| cell.symbol()).collect::<String>();
        let pro = top
            .windows(3)
            .position(|cells| {
                cells[0].symbol() == "p" && cells[1].symbol() == "r" && cells[2].symbol() == "o"
            })
            .expect("Pro mode should render its badge");

        assert!(rendered.contains("medium ⚡  pro"));
        for cell in &top[pro..pro + 3] {
            assert_eq!(cell.fg, Color::Green);
            assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
        }
    }

    #[test]
    fn narrow_composer_prioritizes_the_complete_pro_badge() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::SetFastMode(true));
        composer.update(ComposerEvent::SetReasoningMode(ReasoningMode::Pro));

        let terminal = render(&mut composer, 30, 5);
        let top = &terminal.backend().buffer().content[..30];
        let rendered = top.iter().map(|cell| cell.symbol()).collect::<String>();
        let pro = top
            .windows(3)
            .position(|cells| {
                cells[0].symbol() == "p" && cells[1].symbol() == "r" && cells[2].symbol() == "o"
            })
            .expect("Pro mode should retain its complete badge");

        assert!(!rendered.contains('⚡'));
        assert!((pro..pro + 3).all(|index| top[index].fg == Color::Green));

        let terminal = render(&mut composer, 6, 5);
        let top = &terminal.backend().buffer().content[..6];
        assert_eq!(top[0].symbol(), "╭");
        assert_eq!(top[5].symbol(), "╮");
    }

    #[test]
    fn standard_mode_does_not_render_the_pro_badge() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);

        let terminal = render(&mut composer, 60, 5);
        let rendered = terminal.backend().buffer().content[..60]
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(!rendered.contains("pro"));
    }

    #[test]
    fn development_badge_matches_the_installation_kind() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        let terminal = render(&mut composer, 60, 5);
        let row = &terminal.backend().buffer().content[4 * 60..5 * 60];
        let rendered = row.iter().map(|cell| cell.symbol()).collect::<String>();
        let badge_start = row.iter().position(|cell| cell.symbol() == "◉");

        if !crate::installation::current().is_development() {
            assert!(badge_start.is_none());
            assert!(!rendered.contains("dev"));
            return;
        }

        let badge_start = badge_start.unwrap();
        assert!(rendered.contains("◉ dev  /work"));
        for cell in &row[badge_start..badge_start + 5] {
            assert_eq!(cell.fg, Color::Red);
            assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
        }
    }

    #[test]
    fn entry_hint_keeps_file_and_session_shortcuts_visible_while_typing() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        assert!(rows(&render(&mut composer, 60, 5))[4].contains("@@ sessions"));

        composer.replace_draft("hello".to_owned());
        let footer = &rows(&render(&mut composer, 60, 5))[4];
        assert!(!footer.contains("/ actions"));
        assert!(footer.contains("@ paths · @@ sessions"));

        composer.replace_draft(String::new());
        assert!(!rows(&render(&mut composer, 20, 5))[4].contains("/ actions"));
    }

    #[test]
    fn active_turn_waves_the_transient_status_after_context_usage() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Activity {
            active: true,
            status: Some("Running exec command…".to_owned()),
            now: Instant::now(),
        });

        let terminal = render(&mut composer, 60, 5);

        assert!(rows(&terminal)[0].contains("0%/272k Running exec command…"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .filter(|cell| "Runningexeccommand…".contains(cell.symbol()))
                .any(|cell| cell.fg == Color::Cyan)
        );
        assert!(composer.animation_deadline().is_some());
    }

    #[test]
    fn active_subagents_wave_after_the_transient_status() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        let now = Instant::now();
        composer.update(ComposerEvent::Activity {
            active: true,
            status: Some("Thinking…".to_owned()),
            now,
        });
        composer.update(ComposerEvent::ActiveSubagents { count: 2, now });

        let terminal = render(&mut composer, 72, 5);
        let top = rows(&terminal)[0].clone();

        assert!(top.contains("Thinking… 2 subagents"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .filter(|cell| "2subagents".contains(cell.symbol()))
                .any(|cell| cell.fg == Color::Yellow)
        );
        assert!(composer.animation_deadline().is_some());
    }

    #[test]
    fn composer_grows_from_three_through_six_rows() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        assert_eq!(composer.desired_height(20), 5);

        composer.replace_draft("1\n2\n3\n4\n5\n6".to_owned());
        assert_eq!(composer.desired_height(20), 8);

        composer.replace_draft("1\n2\n3\n4\n5\n6\n7".to_owned());
        assert_eq!(composer.desired_height(20), 8);
    }

    #[test]
    fn overflow_scrolls_to_keep_the_cursor_visible() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("one\ntwo\nthree\nfour\nfive\nsix\nseven".to_owned());
        let terminal = render(&mut composer, 30, 8);
        let rows = rows(&terminal);

        assert!(rows[1].contains("two"));
        assert!(rows[6].contains("seven"));
        assert!(!rows.iter().any(|row| row.contains("one")));
    }

    #[test]
    fn resize_reflows_wrapped_text() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("alpha beta gamma delta".to_owned());

        render(&mut composer, 14, 5);
        assert_eq!(composer.desired_height(14), 5);
        assert_eq!(composer.last_width, 12);

        render(&mut composer, 8, 6);
        assert_eq!(composer.desired_height(8), 6);
        assert_eq!(composer.last_width, 6);
    }

    #[test]
    fn cursor_movement_respects_graphemes_and_display_width() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("a界e\u{301}".to_owned());
        composer.update(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(composer.cursor(), 4);
        composer.update(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(composer.cursor(), 1);

        let terminal = render(&mut composer, 20, 5);
        assert_eq!(terminal.backend().cursor_position(), Position::new(2, 1));
    }

    #[test]
    fn paste_and_editor_replacement_preserve_multiline_text() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste("one\ntwo".to_owned())));
        assert_eq!(composer.draft(), "one\ntwo");

        composer.update(ComposerEvent::ReplaceDraft("edited\ndraft".to_owned()));
        assert_eq!(composer.draft(), "edited\ndraft");
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn paste_and_editor_replacement_normalize_carriage_returns() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste(
            "one\r\ntwo\rthree".to_owned(),
        )));
        assert_eq!(composer.draft(), "one\ntwo\nthree");

        composer.update(ComposerEvent::ReplaceDraft("edited\r\ndraft".to_owned()));
        assert_eq!(composer.draft(), "edited\ndraft");
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn pasted_controls_are_visible_without_changing_the_submission() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        let pasted = "one\ttwo\u{1b}three";
        composer.update(ComposerEvent::Terminal(Event::Paste(pasted.to_owned())));

        let terminal = render(&mut composer, 40, 5);
        assert!(rows(&terminal)[1].contains("one    two�three"));
        assert_eq!(terminal.backend().cursor_position(), Position::new(17, 1));

        let submission = composer.take_submission().unwrap();
        assert_eq!(submission.display_text(), pasted);
    }

    #[test]
    fn pasted_images_render_as_numbered_blue_tokens_and_submit_as_images() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste("inspect ".to_owned())));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,first".to_owned(),
        ));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,second".to_owned(),
        ));

        assert_eq!(composer.draft(), "inspect [Image #1][Image #2]");
        let terminal = render(&mut composer, 50, 5);
        let buffer = terminal.backend().buffer();
        for x in 9..29 {
            assert_eq!(buffer[(x, 1)].fg, Color::Blue);
        }

        let update = composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let Some(ComposerEffect::Submit(submission)) = update.effect else {
            panic!("image prompt should submit");
        };
        assert_eq!(submission.display_text(), "inspect [Image #1][Image #2]");
        let PromptInput::Content(content) = submission.agent_prompt().instruction else {
            panic!("image prompt should use multimodal content");
        };
        assert!(matches!(&content[0], UserInput::Text { text } if text == "inspect "));
        assert!(
            matches!(&content[1], UserInput::Image { image_url, .. } if image_url.ends_with("first"))
        );
        assert!(
            matches!(&content[2], UserInput::Image { image_url, .. } if image_url.ends_with("second"))
        );
    }

    #[test]
    fn deleting_an_image_token_removes_its_attachment_atomically() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,removed".to_owned(),
        ));

        composer.update(key(KeyCode::Backspace, KeyModifiers::NONE));

        assert!(composer.draft().is_empty());
        assert!(composer.images.is_empty());
    }

    #[test]
    fn option_backspace_deletes_the_previous_word() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("one two  ".to_owned());

        let update = composer.update(key(KeyCode::Backspace, KeyModifiers::ALT));

        assert!(update.changed);
        assert_eq!(composer.draft(), "one ");
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn readline_deletion_shortcuts_match_the_primary_composer() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("one two  ".to_owned());

        composer.update(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one ");
        composer.update(key(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one");
        composer.cursor = 0;
        composer.update(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "ne");

        composer.replace_draft("one\ntwo\nthree".to_owned());
        composer.cursor = "one\ntw".len();
        composer.update(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one\no\nthree");
        assert_eq!(composer.cursor(), "one\n".len());
        composer.update(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one\n\nthree");
        composer.update(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one\nthree");
    }

    #[test]
    fn readline_line_deletion_removes_intersecting_image_attachments() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste(
            "one\ninspect ".to_owned(),
        )));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,removed-by-u".to_owned(),
        ));
        composer.update(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one\n");
        assert!(composer.images.is_empty());

        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,removed-by-k".to_owned(),
        ));
        composer.update(ComposerEvent::Terminal(Event::Paste("\nthree".to_owned())));
        composer.cursor = "one\n".len();
        composer.update(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "one\n\nthree");
        assert!(composer.images.is_empty());
    }

    #[test]
    fn control_and_option_arrows_move_by_word() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("one...two".to_owned());

        composer.update(key(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "one...".len());
        composer.update(key(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(composer.cursor(), 0);
        composer.update(key(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "one".len());
        composer.update(key(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn option_backspace_removes_an_image_attachment_with_its_token() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste("inspect ".to_owned())));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,removed".to_owned(),
        ));

        composer.update(key(KeyCode::Backspace, KeyModifiers::ALT));

        assert_eq!(composer.draft(), "inspect ");
        assert!(composer.images.is_empty());
    }

    #[test]
    fn readline_shortcuts_move_by_character_and_stay_on_the_logical_line() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("one\ntwo\nthree".to_owned());
        composer.cursor = "one\nt".len();

        composer.update(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "one\n".len());
        let update = composer.update(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(!update.changed);
        assert_eq!(composer.cursor(), "one\n".len());

        composer.update(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "one\ntwo".len());
        let update = composer.update(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(!update.changed);
        assert_eq!(composer.cursor(), "one\ntwo".len());

        composer.update(key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "one\ntw".len());
        composer.update(key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "one\ntwo".len());
    }

    #[test]
    fn readline_shortcuts_require_exact_modifiers() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("abcd".to_owned());
        composer.cursor = 2;

        let update = composer.update(key(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert!(!update.changed);
        assert_eq!(composer.draft(), "abcd");
        assert_eq!(composer.cursor(), 2);

        composer.update(key(
            KeyCode::Char('b'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ));
        assert_eq!(composer.draft(), "abbcd");
        assert_eq!(composer.cursor(), 3);
    }

    #[test]
    fn readline_word_movement_skips_delimiters_between_alphanumeric_words() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("foo...bar".to_owned());

        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), "foo...".len());
        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), 0);

        composer.update(key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), "foo".len());
        composer.update(key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), composer.draft().len());

        composer.replace_draft("can't".to_owned());
        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), "can'".len());
        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), 0);

        composer.replace_draft("alpha   beta".to_owned());
        composer.cursor = "alpha ".len();
        composer.update(key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), composer.draft().len());
        composer.cursor = "alpha  ".len();
        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), 0);

        composer.replace_draft("你好".to_owned());
        composer.cursor = 0;
        composer.update(key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), composer.draft().len());
        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), 0);
    }

    #[test]
    fn readline_word_movement_treats_images_as_atomic() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste("inspect ".to_owned())));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,attached".to_owned(),
        ));

        composer.update(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), "inspect ".len());
        composer.update(key(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn readline_vertical_movement_treats_images_as_atomic() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,attached".to_owned(),
        ));
        render(&mut composer, 5, 5);

        composer.update(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        composer.update(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), composer.draft().len());
        composer.update(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), 0);

        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste("a".to_owned())));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,attached".to_owned(),
        ));
        composer.update(ComposerEvent::Terminal(Event::Paste("\n12345".to_owned())));

        composer.update(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), "a".len());
        composer.update(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), composer.draft().len());

        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(ComposerEvent::Terminal(Event::Paste(
            "123456789\na".to_owned(),
        )));
        composer.update(ComposerEvent::PasteImage(
            "data:image/png;base64,attached".to_owned(),
        ));
        composer.update(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        composer.update(key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        composer.update(key(KeyCode::Char('f'), KeyModifiers::CONTROL));

        composer.update(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn readline_vertical_shortcuts_restore_the_unsent_draft_after_history() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("older".to_owned());
        composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
        composer.replace_draft("newer".to_owned());
        composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
        composer.replace_draft("top\nbottom".to_owned());

        composer.update(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "top\nbottom");
        assert_eq!(composer.cursor(), "top".len());
        composer.update(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "newer");
        composer.update(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "older");
        composer.update(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "newer");
        composer.update(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "top\nbottom");
        assert_eq!(composer.cursor(), composer.draft().len());
    }

    #[test]
    fn readline_horizontal_shortcuts_detach_recalled_history() {
        for (code, modifiers) in [
            (KeyCode::Char('a'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::ALT),
        ] {
            let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
            composer.replace_draft("previous".to_owned());
            composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
            composer.update(key(KeyCode::Up, KeyModifiers::NONE));

            composer.update(key(code, modifiers));
            composer.update(key(KeyCode::Char('n'), KeyModifiers::CONTROL));

            assert_eq!(composer.draft(), "previous");
        }
    }

    #[test]
    fn submission_trims_nonempty_prompts_and_preserves_empty_drafts() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("  inspect this  \n".to_owned());
        let update = composer.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            update.effect,
            Some(ComposerEffect::Submit("inspect this".to_owned().into()))
        );
        assert!(composer.draft().is_empty());

        composer.replace_draft("   \n".to_owned());
        let update = composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(update.effect, None);
        assert_eq!(composer.draft(), "   \n");
    }

    #[test]
    fn tab_queues_nonempty_input_and_preserves_empty_input() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("  follow up  ".to_owned());

        let update = composer.update(key(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(
            update.effect,
            Some(ComposerEffect::Queue("follow up".to_owned().into()))
        );
        assert!(composer.draft().is_empty());

        composer.replace_draft("   ".to_owned());
        let update = composer.update(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(update.effect, None);
        assert_eq!(composer.draft(), "   ");
    }

    #[test]
    fn composer_discloses_idle_local_and_remote_control_modes() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        assert!(rows(&render(&mut composer, 90, 5))[4].contains("Enter send"));

        composer.update(ComposerEvent::Activity {
            active: true,
            status: Some("Thinking…".to_owned()),
            now: Instant::now(),
        });
        let footer = &rows(&render(&mut composer, 90, 5))[4];
        assert!(footer.contains("remote · read only"));
        assert!(!footer.contains("Enter steer"));

        composer.update(ComposerEvent::LiveControls(true));
        let footer = &rows(&render(&mut composer, 90, 5))[4];
        assert!(footer.contains("Enter steer · Tab queue · Esc Esc stop"));
        assert!(!footer.contains("read only"));
    }

    #[test]
    fn leading_bang_uses_yellow_shell_chrome_and_submits_only_the_command() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("!  printf hello  ".to_owned());

        let terminal = render(&mut composer, 80, 5);
        let buffer = terminal.backend().buffer();
        for position in [(0, 0), (79, 0), (0, 2), (79, 2), (0, 4), (79, 4)] {
            assert_eq!(buffer[position].fg, Color::Yellow);
        }
        assert!(!rows(&terminal)[0].contains("shell"));
        assert!(rows(&terminal)[4].starts_with("╰─ shell "));

        let update = composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            update.effect,
            Some(ComposerEffect::RunShell("printf hello".to_owned()))
        );
        assert!(composer.draft().is_empty());
    }

    #[test]
    fn bang_without_a_command_is_not_submitted() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("!   ".to_owned());

        let update = composer.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(update.effect, None);
        assert_eq!(composer.draft(), "!   ");
    }

    #[test]
    fn arrows_cycle_submitted_prompts_and_restore_the_unsent_draft() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        for prompt in ["first", "second"] {
            composer.replace_draft(prompt.to_owned());
            composer.update(key(KeyCode::Enter, KeyModifiers::NONE));
        }
        composer.replace_draft("unfinished\nline".to_owned());

        composer.update(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "unfinished\nline");
        composer.update(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "second");
        composer.update(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "first");
        composer.update(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "second");
        composer.update(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "unfinished\nline");
    }

    #[test]
    fn editing_a_recalled_prompt_detaches_it_from_history() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("previous".to_owned());
        composer.update(key(KeyCode::Enter, KeyModifiers::NONE));

        composer.update(key(KeyCode::Up, KeyModifiers::NONE));
        composer.update(key(KeyCode::Char('!'), KeyModifiers::NONE));
        composer.update(key(KeyCode::Down, KeyModifiers::NONE));

        assert_eq!(composer.draft(), "previous!");
    }

    #[test]
    fn multiline_and_control_effect_keys_are_distinct() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.update(key(KeyCode::Enter, KeyModifiers::SHIFT));
        composer.update(key(KeyCode::Enter, KeyModifiers::ALT));
        composer.update(key(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(composer.draft(), "\n\n\n");

        assert_eq!(
            composer
                .update(key(KeyCode::Char('g'), KeyModifiers::CONTROL))
                .effect,
            Some(ComposerEffect::OpenDraftEditor)
        );
    }

    #[test]
    fn editing_keys_follow_visual_lines_and_grapheme_boundaries() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("abc\ndef".to_owned());
        render(&mut composer, 20, 5);

        composer.update(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(composer.cursor(), 3);
        composer.update(key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(composer.cursor(), 0);
        composer.update(key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "bc\ndef");
        composer.update(key(KeyCode::End, KeyModifiers::NONE));
        composer.update(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.draft(), "b\ndef");
    }

    #[test]
    fn wrapping_prefers_words_and_hard_wraps_long_words() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("alpha betaabcdefgh".to_owned());
        let terminal = render(&mut composer, 8, 6);
        let rows = rows(&terminal);

        assert!(rows[1].contains("alpha"));
        assert!(rows[2].contains("betaab"));
        assert!(rows[3].contains("cdefgh"));
    }

    #[test]
    fn semantic_selection_preserves_source_across_soft_and_hard_wraps() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("abcdef\ngh".to_owned());
        let area = Rect::new(10, 5, 3, 3);
        let anchor = composer.selection_span(Position::new(11, 5), area).unwrap();
        let head = composer.selection_span(Position::new(11, 7), area).unwrap();
        let mut selection = Selection::default();
        selection.begin(Surface::Composer, anchor);
        selection.drag(head);

        assert_eq!(
            composer
                .selection_text(selection.range().unwrap())
                .as_deref(),
            Some("bcdef\ngh")
        );
    }

    #[test]
    fn selection_scrolling_keeps_the_semantic_range_visible_without_cursor_follow() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("line 1\nline 2\nline 3\nline 4".to_owned());
        render(&mut composer, 12, 4);
        assert_eq!(composer.scroll, 2);

        let content = Rect::new(1, 1, 10, 2);
        assert!(composer.scroll_selection(-1, content));
        let anchor = composer
            .selection_span(Position::new(1, 1), content)
            .unwrap();
        let head = composer
            .selection_span(Position::new(6, 2), content)
            .unwrap();
        let mut selection = Selection::default();
        selection.begin(Surface::Composer, anchor);
        selection.drag(head);
        let range = selection.range().unwrap();

        let terminal = render_with_selection(&mut composer, 12, 4, range);

        assert_eq!(composer.scroll, 1);
        assert_eq!(
            composer.selection_text(range).as_deref(),
            Some("line 2\nline 3")
        );
        assert_eq!(terminal.backend().buffer()[(1, 1)].bg, Color::Yellow);
        assert_eq!(terminal.backend().buffer()[(6, 2)].bg, Color::Yellow);
        assert_eq!(terminal.backend().cursor_position(), Position::new(0, 0));
    }

    #[test]
    fn narrow_selection_matches_the_visible_draft_text() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("first\nsecond\nthird".to_owned());
        render(&mut composer, 12, 4);

        let terminal = render(&mut composer, 2, 2);
        let visible = terminal.backend().buffer()[(0, 0)].symbol().to_owned();
        let area = Rect::new(0, 0, 2, 1);
        let span = composer.selection_span(Position::new(0, 0), area).unwrap();
        let mut selection = Selection::default();
        selection.begin(Surface::Composer, span);
        selection.drag(span);

        assert_eq!(
            composer
                .selection_text(selection.range().unwrap())
                .as_deref(),
            Some(visible.as_str())
        );
    }

    #[test]
    fn context_percentage_is_rounded() {
        assert_eq!(context_percent(0), 0);
        assert_eq!(context_percent(136_000), 50);
        assert_eq!(context_percent(1_400), 1);
    }

    #[test]
    fn slash_settings_commands_open_pickers_and_accept_direct_values() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);

        composer.replace_draft("/model".to_owned());
        assert_eq!(
            composer.submit().effect,
            Some(ComposerEffect::Settings(SettingsCommand::OpenModel))
        );
        composer.replace_draft("/model astra".to_owned());
        assert_eq!(
            composer.submit().effect,
            Some(ComposerEffect::Settings(SettingsCommand::SetModel(
                Model::Astra
            )))
        );

        for alias in ["/effort", "/reasoning", "/thinking"] {
            composer.replace_draft(alias.to_owned());
            assert_eq!(
                composer.submit().effect,
                Some(ComposerEffect::Settings(SettingsCommand::OpenEffort))
            );
            composer.replace_draft(format!("{alias} high"));
            assert_eq!(
                composer.submit().effect,
                Some(ComposerEffect::Settings(SettingsCommand::SetEffort(
                    ReasoningEffort::High
                )))
            );
        }
    }

    #[test]
    fn similarly_prefixed_prompts_are_not_treated_as_settings_commands() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("/modeling the system".to_owned());

        assert!(matches!(
            composer.submit().effect,
            Some(ComposerEffect::Submit(prompt))
                if prompt.display_text() == "/modeling the system"
        ));
    }

    #[test]
    fn narrow_rendering_truncates_without_panicking() {
        let mut composer = Composer::new(Path::new("/work"), ReasoningEffort::Medium);
        composer.replace_draft("abcdef".to_owned());

        let terminal = render(&mut composer, 3, 2);

        assert_eq!(rows(&terminal)[0], "abc");
    }
}
