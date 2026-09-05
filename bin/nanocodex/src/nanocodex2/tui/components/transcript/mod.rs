// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Scrollable rendering of the persisted agent session.

mod diff;
mod empty;
mod highlight;
pub(crate) mod image;
mod markdown;
mod message;
mod tool;

use super::{
    node::{Component, ComponentUpdate, RenderRequest},
    selection::{TextRange, TextSpan},
};
use crate::{
    config::ReasoningEffort,
    tui::{
        format::{
            duration_display_tick, format_duration, format_turn_duration, normalize_line_endings,
        },
        spinner::Spinner,
        theme::Theme,
        transcript::{
            EntryId, EntryKind, TranscriptEntry, TranscriptModel, TranscriptRecord, TransientStatus,
        },
    },
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use empty::EmptyLogo;
use nanocodex_subagents::{AgentMessageUpdate, MessageSender};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use std::{
    collections::{HashMap, hash_map::Entry},
    ops::Range,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const EXPANDABLE_FOCUS_HINTS: [&str; 2] =
    ["↑↓ item · Enter toggle · Esc back", "↑↓ item · Enter · Esc"];
const HISTORY_PREFETCH_VIEWPORTS: u16 = 3;
const NESTED_TOOL_INDENT: u16 = 4;
const RETRY_COUNTDOWN_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) enum TranscriptEvent {
    Record(Arc<TranscriptRecord>),
    DirectedMessage {
        perspective: MessageSender,
        update: AgentMessageUpdate,
    },
    AgentStreamClosed,
    Scroll(ScrollCommand),
    FollowTail,
    BlurExpandables,
    Expandable(ExpandableCommand),
    ToggleExpandAll,
    AnimationFrame(Instant),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranscriptEffect {
    pub(crate) active: bool,
    pub(crate) status: Option<String>,
}

pub(crate) struct Transcript {
    model: TranscriptModel,
    cache: LayoutCache,
    scroll: ScrollState,
    pending_scroll: ScrollCommand,
    last_top: Option<Anchor>,
    viewport_height: u16,
    new_updates: u64,
    tool_spinner: Option<Spinner>,
    running_tool_timers: HashMap<EntryId, RunningToolTimer>,
    retry_timer: Option<RetryTimer>,
    expandables_focused: bool,
    selected_expandable: Option<EntryId>,
    expandable_hits: Vec<ExpandableHitRegion>,
    link_hits: Vec<LinkHitRegion>,
    selection_rows: Vec<(u16, Anchor)>,
    transcript_y: u16,
    transcript_x: u16,
    pending_expandable_anchor: Option<PendingExpandableAnchor>,
    empty_logo: EmptyLogo,
    effort: ReasoningEffort,
    updates_banner_area: Option<Rect>,
    at_top: bool,
    rows_before_top: Option<u16>,
}

struct CachedEntry {
    revision: u64,
    width: u16,
    expanded: bool,
    live_duration_ns: Option<u64>,
    tool_summary_lines: usize,
    lines: Vec<Line<'static>>,
    images: Vec<markdown::ImagePlacement>,
    links: Vec<Vec<markdown::LinkSpan>>,
    selections: Vec<Vec<markdown::SourceSpan>>,
    envelopes: Vec<markdown::SourceEnvelope>,
    selection_source: Option<String>,
    image_state: markdown::ImageState,
}

struct LayoutCache {
    entries: HashMap<EntryId, CachedEntry>,
    live_tool_durations: HashMap<EntryId, u64>,
    expansion_overrides: HashMap<EntryId, bool>,
    expand_all: Option<bool>,
    workspace: std::path::PathBuf,
    images: image::Cache,
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            live_tool_durations: HashMap::new(),
            expansion_overrides: HashMap::new(),
            expand_all: None,
            workspace: std::env::current_dir().unwrap_or_default(),
            images: image::Cache::default(),
        }
    }
}

#[derive(Default)]
struct RenderPlan {
    top_padding: u16,
    anchors: Vec<Anchor>,
}

#[derive(Clone, Copy)]
enum ScrollState {
    Follow,
    Detached(Anchor),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Anchor {
    entry: EntryId,
    line: usize,
}

#[derive(Clone, Copy)]
struct ExpandableHitRegion {
    entry: EntryId,
    row: u16,
}

struct LinkHitRegion {
    destination: Arc<str>,
    row: u16,
    start: u16,
    end: u16,
}

#[derive(Clone, Copy)]
enum PendingExpandableAnchor {
    Reveal(EntryId),
    Preserve { entry: EntryId, row: u16 },
}

#[derive(Clone, Copy)]
struct RunningToolTimer {
    observed_at: Instant,
    elapsed_at_observation: Duration,
}

#[derive(Clone, Copy)]
struct RetryTimer {
    deadline: Instant,
    remaining_ns: u64,
    next_frame: Option<Instant>,
}

impl RunningToolTimer {
    fn new(started_at_unix_ms: u64, observed_at: Instant, observed_at_unix_ms: u64) -> Self {
        Self {
            observed_at,
            elapsed_at_observation: Duration::from_millis(
                observed_at_unix_ms.saturating_sub(started_at_unix_ms),
            ),
        }
    }

    fn elapsed(self, now: Instant) -> Duration {
        self.elapsed_at_observation
            .saturating_add(now.saturating_duration_since(self.observed_at))
    }
}

impl RetryTimer {
    fn new(now: Instant, delay_ns: u64) -> Self {
        let deadline = now + Duration::from_nanos(delay_ns);
        Self {
            deadline,
            remaining_ns: delay_ns,
            next_frame: Some((now + RETRY_COUNTDOWN_INTERVAL).min(deadline)),
        }
    }

    fn refresh(&mut self, now: Instant) -> bool {
        let previous_tick = duration_display_tick(self.remaining_ns);
        self.remaining_ns = u64::try_from(self.deadline.saturating_duration_since(now).as_nanos())
            .unwrap_or(u64::MAX);
        self.next_frame =
            (self.remaining_ns > 0).then(|| (now + RETRY_COUNTDOWN_INTERVAL).min(self.deadline));
        duration_display_tick(self.remaining_ns) != previous_tick
    }
}

#[derive(Clone, Copy)]
pub(super) enum ExpandableCommand {
    Previous,
    Next,
    Toggle,
    Click { row: u16 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ScrollCommand {
    #[default]
    None,
    Rows(i32),
    Home,
    End,
}

impl Transcript {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_effort(ReasoningEffort::default())
    }

    pub(crate) fn with_effort(effort: ReasoningEffort) -> Self {
        Self {
            model: TranscriptModel::default(),
            cache: LayoutCache::default(),
            scroll: ScrollState::Follow,
            pending_scroll: ScrollCommand::None,
            last_top: None,
            viewport_height: 0,
            new_updates: 0,
            tool_spinner: None,
            running_tool_timers: HashMap::new(),
            retry_timer: None,
            expandables_focused: false,
            selected_expandable: None,
            expandable_hits: Vec::new(),
            link_hits: Vec::new(),
            selection_rows: Vec::new(),
            transcript_y: 0,
            transcript_x: 0,
            pending_expandable_anchor: None,
            empty_logo: EmptyLogo::new(Instant::now()),
            effort,
            updates_banner_area: None,
            at_top: true,
            rows_before_top: Some(0),
        }
    }

    pub(crate) fn fork_snapshot(&self) -> Self {
        let mut snapshot = Self::with_effort(self.effort);
        snapshot.model = self.model.fork_snapshot();
        snapshot.cache.workspace.clone_from(&self.cache.workspace);
        snapshot
    }

    pub(crate) const fn at_top(&self) -> bool {
        self.at_top
    }

    pub(crate) fn near_top(&self) -> bool {
        self.rows_before_top
            .is_some_and(|rows| rows <= self.history_prefetch_rows())
    }

    pub(super) fn should_load_older_after(&self, command: ScrollCommand) -> bool {
        match command {
            ScrollCommand::Home => true,
            ScrollCommand::Rows(rows) if rows < 0 => {
                let scroll_rows = u16::try_from(rows.unsigned_abs()).unwrap_or(u16::MAX);
                self.rows_before_top.is_some_and(|distance| {
                    distance.saturating_sub(scroll_rows) <= self.history_prefetch_rows()
                })
            }
            ScrollCommand::None | ScrollCommand::Rows(_) | ScrollCommand::End => false,
        }
    }

    pub(crate) fn preserve_viewport_from(&mut self, previous: &Self) {
        let ScrollState::Detached(previous_anchor) = previous.scroll else {
            return;
        };
        let Some(previous_index) = previous.model.index_of(previous_anchor.entry) else {
            return;
        };
        let distance_from_end = previous
            .model
            .entries()
            .len()
            .saturating_sub(previous_index);
        let Some(index) = self.model.entries().len().checked_sub(distance_from_end) else {
            return;
        };
        let Some(entry) = self.model.entries().get(index) else {
            return;
        };
        let anchor = Anchor {
            entry: entry.id,
            line: previous_anchor.line,
        };
        self.scroll = ScrollState::Detached(anchor);
        self.last_top = Some(anchor);
        self.new_updates = previous.new_updates;
        self.at_top = false;
        self.rows_before_top = previous.rows_before_top;
    }

    pub(crate) fn set_workspace(&mut self, workspace: &Path) {
        self.cache.workspace = workspace.to_path_buf();
        self.cache.entries.clear();
        self.cache.images.clear();
    }

    pub(crate) fn refresh_terminal_images(&mut self) {
        self.cache.refresh_terminal_images();
    }

    pub(crate) const fn set_effort(&mut self, effort: ReasoningEffort) {
        self.effort = effort;
    }

    pub(super) fn render_chrome(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.updates_banner_area = None;
        if self.expandables_focused {
            let _ = render_top_right_hint(frame, area, &EXPANDABLE_FOCUS_HINTS, theme.accent());
            return;
        }

        if !matches!(self.scroll, ScrollState::Detached(_)) || self.new_updates == 0 {
            return;
        }

        let noun = if self.new_updates == 1 {
            "update"
        } else {
            "updates"
        };
        let label = format!("↓ {} {noun} · Ctrl+End to follow", self.new_updates);
        let compact_label = format!("↓ {} {noun} · Ctrl+End", self.new_updates);
        self.updates_banner_area =
            render_top_right_hint(frame, area, &[&label, &compact_label], theme.border());
    }

    pub(crate) fn animation_deadline(&self) -> Option<Instant> {
        let empty = self.is_empty().then(|| self.empty_logo.deadline());
        self.tool_spinner
            .map(Spinner::deadline)
            .into_iter()
            .chain(empty)
            .chain(self.retry_timer.and_then(|timer| timer.next_frame))
            .chain(self.cache.images.animation_deadline())
            .min()
    }

    fn update_record(
        &mut self,
        record: Arc<TranscriptRecord>,
    ) -> ComponentUpdate<TranscriptEffect> {
        let previous_activity = self.activity();
        let change = self.model.apply(&record);
        let activity = self.activity();
        let now = Instant::now();
        if record.kind() == "model.attempt.retrying" {
            if let Some(TransientStatus::Retrying(delay_ns)) = self.model.transient() {
                self.retry_timer = Some(RetryTimer::new(now, *delay_ns));
            }
        } else if !matches!(self.model.transient(), Some(TransientStatus::Retrying(_))) {
            self.retry_timer = None;
        }
        self.sync_running_tool_timers(now);
        let tool_active = self.model.has_running_tools();
        if tool_active && self.tool_spinner.is_none() {
            self.tool_spinner = Some(Spinner::new(now));
        } else if !tool_active {
            self.tool_spinner = None;
        }
        if change.changed && matches!(self.scroll, ScrollState::Detached(_)) {
            self.new_updates = self.new_updates.saturating_add(1);
        }
        let effects = (previous_activity != activity)
            .then_some(activity)
            .into_iter()
            .collect();
        let render = if !change.changed {
            RenderRequest::None
        } else if record.source() == "tact" {
            RenderRequest::Immediate
        } else {
            RenderRequest::Streaming
        };
        ComponentUpdate { effects, render }
    }

    fn update_message(
        &mut self,
        perspective: MessageSender,
        update: AgentMessageUpdate,
    ) -> ComponentUpdate<TranscriptEffect> {
        let change = self.model.apply_message(perspective, update);
        if let Some(id) = change.removed {
            self.forget_entry(id);
        }
        if !change.changed {
            return ComponentUpdate::none();
        }
        if matches!(self.scroll, ScrollState::Detached(_)) {
            self.new_updates = self.new_updates.saturating_add(1);
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn forget_entry(&mut self, id: EntryId) {
        self.cache.forget(id);
        self.running_tool_timers.remove(&id);
        self.expandable_hits.retain(|hit| hit.entry != id);
        if self.selected_expandable == Some(id) {
            self.selected_expandable = None;
        }
        if self.last_top.is_some_and(|anchor| anchor.entry == id) {
            self.last_top = None;
        }
        if matches!(self.scroll, ScrollState::Detached(anchor) if anchor.entry == id) {
            self.scroll = ScrollState::Follow;
        }
        if matches!(
            self.pending_expandable_anchor,
            Some(PendingExpandableAnchor::Reveal(entry)) if entry == id
        ) || matches!(
            self.pending_expandable_anchor,
            Some(PendingExpandableAnchor::Preserve { entry, .. }) if entry == id
        ) {
            self.pending_expandable_anchor = None;
        }
    }

    fn agent_stream_closed(&mut self) -> ComponentUpdate<TranscriptEffect> {
        let previous_activity = self.activity();
        if !self.model.agent_stream_closed() {
            return ComponentUpdate::none();
        }
        let now = Instant::now();
        self.sync_running_tool_timers(now);
        self.tool_spinner = self.model.has_running_tools().then(|| Spinner::new(now));
        let activity = self.activity();
        ComponentUpdate {
            effects: (previous_activity != activity)
                .then_some(activity)
                .into_iter()
                .collect(),
            render: RenderRequest::Immediate,
        }
    }

    fn activity(&self) -> TranscriptEffect {
        TranscriptEffect {
            active: self.model.is_active(),
            status: self.model.transient().map(|status| match status {
                TransientStatus::Retrying(delay_ns) => {
                    let remaining_ns = self
                        .retry_timer
                        .map_or(*delay_ns, |timer| timer.remaining_ns);
                    format!("Retrying in {}…", format_duration(remaining_ns))
                }
                status => transient_label(status),
            }),
        }
    }

    fn update_animation(&mut self, now: Instant) -> ComponentUpdate<TranscriptEffect> {
        let previous_activity = self.activity();
        let retry_changed = self
            .retry_timer
            .as_mut()
            .is_some_and(|timer| timer.refresh(now));
        let timer_changed = self.refresh_running_tool_durations(now);
        let tool_changed = self
            .tool_spinner
            .as_mut()
            .is_some_and(|spinner| spinner.advance(now));
        let logo_changed = self.is_empty() && self.empty_logo.advance(now);
        let images_changed = self.cache.poll_images(now);
        let activity = self.activity();
        ComponentUpdate {
            effects: (previous_activity != activity)
                .then_some(activity)
                .into_iter()
                .collect(),
            render: if retry_changed
                || timer_changed
                || tool_changed
                || logo_changed
                || images_changed
            {
                RenderRequest::Streaming
            } else {
                RenderRequest::None
            },
        }
    }

    fn sync_running_tool_timers(&mut self, now: Instant) {
        self.running_tool_timers
            .retain(|id, _| self.model.entry(*id).is_some_and(is_running_tool));
        let observed_at_unix_ms = unix_milliseconds();
        for id in self.model.running_tool_ids() {
            let Some(started_at_unix_ms) =
                self.model.entry(id).and_then(|entry| match &entry.kind {
                    EntryKind::Tool(tool) => Some(tool.started_at_unix_ms),
                    _ => None,
                })
            else {
                continue;
            };
            self.running_tool_timers.entry(id).or_insert_with(|| {
                RunningToolTimer::new(started_at_unix_ms, now, observed_at_unix_ms)
            });
        }
        self.refresh_running_tool_durations(now);
    }

    fn refresh_running_tool_durations(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for (&id, &timer) in &self.running_tool_timers {
            let elapsed = timer.elapsed(now);
            let duration_ns = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
            changed |= self.cache.set_live_tool_duration(id, duration_ns);
        }
        self.cache
            .retain_live_tool_durations(|id| self.running_tool_timers.contains_key(&id));
        changed
    }

    fn is_empty(&self) -> bool {
        self.model.entries().iter().all(|entry| entry.hidden)
    }

    pub(super) fn scroll_command(&self, event: &Event) -> Option<ScrollCommand> {
        let command = match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match (key.code, key.modifiers) {
                    (KeyCode::PageUp, _) => ScrollCommand::Rows(-self.page_size()),
                    (KeyCode::PageDown, _) => ScrollCommand::Rows(self.page_size()),
                    (KeyCode::Home, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                        ScrollCommand::Home
                    }
                    (KeyCode::End, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                        ScrollCommand::End
                    }
                    _ => return None,
                }
            }
            Event::Mouse(mouse) if !mouse.modifiers.contains(KeyModifiers::SHIFT) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => ScrollCommand::Rows(-self.wheel_size()),
                    MouseEventKind::ScrollDown => ScrollCommand::Rows(self.wheel_size()),
                    _ => return None,
                }
            }
            _ => return None,
        };
        Some(command)
    }

    pub(super) fn updates_banner_clicked(&self, event: &Event) -> bool {
        let Event::Mouse(mouse) = event else {
            return false;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        self.updates_banner_area
            .is_some_and(|area| area.contains(Position::new(mouse.column, mouse.row)))
    }

    pub(super) fn expandable_command(&self, event: &Event) -> Option<ExpandableCommand> {
        match event {
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => self
                .expandable_hits
                .iter()
                .any(|hit| hit.row == mouse.row)
                .then_some(ExpandableCommand::Click { row: mouse.row }),
            Event::Key(key)
                if self.expandables_focused
                    && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                match key.code {
                    KeyCode::Up => Some(ExpandableCommand::Previous),
                    KeyCode::Down => Some(ExpandableCommand::Next),
                    KeyCode::Enter => Some(ExpandableCommand::Toggle),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(super) fn link_destination(&self, event: &Event) -> Option<Arc<str>> {
        let Event::Mouse(mouse) = event else {
            return None;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        self.link_hits
            .iter()
            .find(|hit| hit.row == mouse.row && (hit.start..hit.end).contains(&mouse.column))
            .map(|hit| Arc::clone(&hit.destination))
    }

    pub(super) fn selection_span(&self, position: Position) -> Option<TextSpan> {
        self.selection_span_with_fallback(position, false)
    }

    pub(super) fn selection_span_nearest(&self, position: Position) -> Option<TextSpan> {
        self.selection_span_with_fallback(position, true)
    }

    fn selection_span_with_fallback(
        &self,
        position: Position,
        across_entries: bool,
    ) -> Option<TextSpan> {
        let exact = self
            .selection_rows
            .iter()
            .find(|(row, _)| *row == position.y)
            .map(|(_, anchor)| *anchor);
        let exact = match exact {
            Some(exact) => exact,
            None if across_entries => self
                .selection_rows
                .iter()
                .min_by_key(|(row, _)| row.abs_diff(position.y))
                .map(|(_, anchor)| *anchor)?,
            None => return None,
        };
        let anchor = if self.cache.selections(exact).is_empty() {
            if !across_entries {
                let entry = self.model.entry(exact.entry)?;
                if matches!(entry.kind, EntryKind::Tool(_)) {
                    return None;
                }
                self.cache.selection_source(entry)?;
            }
            self.selection_rows
                .iter()
                .filter(|(_, anchor)| {
                    (across_entries || anchor.entry == exact.entry)
                        && !self.cache.selections(*anchor).is_empty()
                })
                .min_by_key(|(row, _)| row.abs_diff(position.y))
                .map(|(_, anchor)| *anchor)?
        } else {
            exact
        };
        let spans = self.cache.selections(anchor);
        let column = position.x.saturating_sub(self.transcript_x);
        let source = spans
            .iter()
            .find(|span| span.columns.contains(&column))
            .or_else(|| {
                spans.iter().min_by_key(|span| {
                    if column < span.columns.start {
                        span.columns.start - column
                    } else {
                        column.saturating_sub(span.columns.end.saturating_sub(1))
                    }
                })
            })?;
        Some(TextSpan::new(
            anchor.entry.index(),
            source.source.start,
            source.source.end,
        ))
    }

    pub(super) fn selection_text(&self, range: TextRange) -> Option<String> {
        let (start, end) = range.bounds();
        let mut fragments = Vec::new();
        for entry in self.model.entries() {
            let block = entry.id.index();
            if block < start.block || block > end.block {
                continue;
            }
            let Some(source) = self.cache.selection_source(entry) else {
                continue;
            };
            let Some(selected) = range.source_range(block, source.len()) else {
                continue;
            };
            let selected = self.cache.expand_selection(entry.id, selected);
            if let Some(fragment) = source.get(selected)
                && !fragment.is_empty()
            {
                fragments.push(fragment);
            }
        }
        (!fragments.is_empty()).then(|| fragments.join("\n\n"))
    }

    pub(super) fn render_selection(&self, buffer: &mut Buffer, range: TextRange) {
        let selected = Style::reset().fg(Color::Black).bg(Color::Yellow);
        for (row, anchor) in &self.selection_rows {
            for span in self.cache.selections(*anchor) {
                if !range.includes(anchor.entry.index(), &span.source) {
                    continue;
                }
                for column in span.columns.clone() {
                    let column = self.transcript_x.saturating_add(column);
                    if let Some(cell) = buffer.cell_mut(Position::new(column, *row)) {
                        cell.set_style(selected);
                    }
                }
            }
        }
    }

    pub(super) const fn expandables_focused(&self) -> bool {
        self.expandables_focused
    }

    fn update_scroll(&mut self, command: ScrollCommand) -> ComponentUpdate<TranscriptEffect> {
        self.pending_scroll = command;
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn follow_tail(&mut self) -> ComponentUpdate<TranscriptEffect> {
        let was_detached = matches!(self.scroll, ScrollState::Detached(_));
        self.scroll = ScrollState::Follow;
        self.pending_scroll = ScrollCommand::None;
        self.new_updates = 0;

        if was_detached {
            ComponentUpdate::render(RenderRequest::Immediate)
        } else {
            ComponentUpdate::none()
        }
    }

    fn blur_expandables(&mut self) -> ComponentUpdate<TranscriptEffect> {
        if !self.expandables_focused {
            return ComponentUpdate::none();
        }
        self.expandables_focused = false;
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    #[cfg(test)]
    pub(super) fn focus_expandables(&mut self) -> ComponentUpdate<TranscriptEffect> {
        self.expandables_focused = true;
        if self.selected_expandable.is_none() {
            self.selected_expandable = self.expandable_hits.last().map(|hit| hit.entry);
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_expandable(
        &mut self,
        command: ExpandableCommand,
    ) -> ComponentUpdate<TranscriptEffect> {
        match command {
            ExpandableCommand::Previous => self.select_expandable(-1),
            ExpandableCommand::Next => self.select_expandable(1),
            ExpandableCommand::Toggle => self.toggle_selected_expandable(),
            ExpandableCommand::Click { row } => {
                let Some(entry) = self
                    .expandable_hits
                    .iter()
                    .find(|hit| hit.row == row)
                    .map(|hit| hit.entry)
                else {
                    return ComponentUpdate::none();
                };
                self.expandables_focused = true;
                self.selected_expandable = Some(entry);
                self.toggle_selected_expandable()
            }
        }
    }

    fn select_expandable(&mut self, direction: i32) -> ComponentUpdate<TranscriptEffect> {
        let entries = self.model.entries();
        let selected = self
            .selected_expandable
            .and_then(|selected| self.model.index_of(selected));
        let next = if direction < 0 {
            let end = selected.unwrap_or(entries.len());
            entries[..end]
                .iter()
                .rev()
                .find(|entry| !entry.hidden && is_expandable(entry))
        } else if let Some(selected) = selected {
            entries[selected.saturating_add(1)..]
                .iter()
                .find(|entry| !entry.hidden && is_expandable(entry))
        } else {
            entries
                .iter()
                .rev()
                .find(|entry| !entry.hidden && is_expandable(entry))
        };
        let Some(selected) = next.map(|entry| entry.id) else {
            return ComponentUpdate::none();
        };
        self.selected_expandable = Some(selected);
        if !self.expandable_hits.iter().any(|hit| hit.entry == selected) {
            self.pending_expandable_anchor = Some(PendingExpandableAnchor::Reveal(selected));
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn toggle_selected_expandable(&mut self) -> ComponentUpdate<TranscriptEffect> {
        let Some(entry_id) = self.selected_expandable else {
            return ComponentUpdate::none();
        };
        let Some(entry_index) = self.model.index_of(entry_id) else {
            return ComponentUpdate::none();
        };
        let row = self
            .expandable_hits
            .iter()
            .find(|hit| hit.entry == entry_id)
            .map_or(0, |hit| hit.row.saturating_sub(self.transcript_y));
        self.cache.toggle(&self.model.entries()[entry_index]);
        self.pending_expandable_anchor = Some(PendingExpandableAnchor::Preserve {
            entry: entry_id,
            row,
        });
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn page_size(&self) -> i32 {
        i32::from(self.viewport_height.saturating_sub(2).max(1))
    }

    fn wheel_size(&self) -> i32 {
        (self.page_size() + 2) / 3
    }

    fn render_plan(&mut self, width: u16, height: u16, theme: &Theme) -> RenderPlan {
        if width == 0 || height == 0 {
            return RenderPlan::default();
        }
        self.viewport_height = height;
        self.apply_pending_expandable_anchor(width, theme);
        self.apply_pending_scroll(width, height, theme);
        let top = match self.scroll {
            ScrollState::Follow => self.tail_top(width, height, theme),
            ScrollState::Detached(anchor) => {
                let top = self
                    .resolve_anchor(anchor, width, theme)
                    .map(|top| self.fill_viewport_from(top, height, width, theme));
                if let Some(top) = top {
                    self.scroll = ScrollState::Detached(top);
                }
                top
            }
        };
        self.last_top = top;
        self.at_top = match top {
            Some(top) => self.first_anchor(width, theme) == Some(top),
            None => true,
        };
        self.rows_before_top = self.rows_before_loaded_top(
            top,
            self.history_prefetch_rows().saturating_add(height.max(1)),
            width,
            theme,
        );
        let anchors = top.map_or_else(Vec::new, |anchor| {
            self.collect_forward_anchors(anchor, usize::from(height), width, theme)
        });
        if matches!(self.scroll, ScrollState::Detached(_))
            && anchors.last().copied() == self.last_anchor(width, theme)
        {
            self.scroll = ScrollState::Follow;
            self.new_updates = 0;
        }
        let occupied = anchors.len();
        let top_padding = height.saturating_sub(u16::try_from(occupied).unwrap_or(u16::MAX));
        self.warm_overscan(top, height, width, theme);
        RenderPlan {
            top_padding,
            anchors,
        }
    }

    fn history_prefetch_rows(&self) -> u16 {
        self.viewport_height
            .max(1)
            .saturating_mul(HISTORY_PREFETCH_VIEWPORTS)
    }

    fn rows_before_loaded_top(
        &mut self,
        top: Option<Anchor>,
        limit: u16,
        width: u16,
        theme: &Theme,
    ) -> Option<u16> {
        let Some(mut anchor) = top else {
            return Some(0);
        };
        let mut rows = 0_u16;
        loop {
            let Some(previous) = self.previous(anchor, width, theme) else {
                return Some(rows);
            };
            rows = rows.saturating_add(1);
            if rows > limit {
                return None;
            }
            anchor = previous;
        }
    }

    fn apply_pending_expandable_anchor(&mut self, width: u16, theme: &Theme) {
        let Some(request) = self.pending_expandable_anchor.take() else {
            return;
        };
        let (entry, row) = match request {
            PendingExpandableAnchor::Reveal(entry) => (entry, 0),
            PendingExpandableAnchor::Preserve { entry, row } => (entry, row),
        };
        let anchor = Anchor { entry, line: 0 };
        let (top, _) = self.move_anchor(anchor, -i32::from(row), width, theme);
        self.scroll = ScrollState::Detached(top);
    }

    fn apply_pending_scroll(&mut self, width: u16, height: u16, theme: &Theme) {
        let command = std::mem::take(&mut self.pending_scroll);
        match command {
            ScrollCommand::None => {}
            ScrollCommand::End => {
                self.scroll = ScrollState::Follow;
                self.new_updates = 0;
            }
            ScrollCommand::Home => {
                if let Some(anchor) = self.first_anchor(width, theme) {
                    self.scroll = ScrollState::Detached(anchor);
                }
            }
            ScrollCommand::Rows(rows) if rows < 0 => {
                let start = match self.scroll {
                    ScrollState::Follow => self
                        .last_top
                        .or_else(|| self.tail_top(width, height, theme)),
                    ScrollState::Detached(anchor) => Some(anchor),
                };
                if let Some(start) = start {
                    let anchor = self.move_anchor(start, rows, width, theme).0;
                    self.scroll = ScrollState::Detached(anchor);
                }
            }
            ScrollCommand::Rows(rows) => {
                let ScrollState::Detached(start) = self.scroll else {
                    return;
                };
                let (anchor, reached_end) = self.move_anchor(start, rows, width, theme);
                if reached_end {
                    self.scroll = ScrollState::Follow;
                    self.new_updates = 0;
                } else {
                    self.scroll = ScrollState::Detached(anchor);
                }
            }
        }
    }

    fn tail_top(&mut self, width: u16, height: u16, theme: &Theme) -> Option<Anchor> {
        let mut anchor = self.last_anchor(width, theme)?;
        for _ in 1..height {
            let Some(previous) = self.previous(anchor, width, theme) else {
                break;
            };
            anchor = previous;
        }
        Some(anchor)
    }

    fn fill_viewport_from(
        &mut self,
        anchor: Anchor,
        height: u16,
        width: u16,
        theme: &Theme,
    ) -> Anchor {
        let mut last = anchor;
        let mut available = 1_u16;
        while available < height {
            let Some(next) = self.next(last, width, theme) else {
                break;
            };
            last = next;
            available = available.saturating_add(1);
        }

        let mut top = anchor;
        for _ in available..height {
            let Some(previous) = self.previous(top, width, theme) else {
                break;
            };
            top = previous;
        }
        top
    }

    fn first_anchor(&mut self, width: u16, theme: &Theme) -> Option<Anchor> {
        for index in 0..self.model.entries().len() {
            let entry = &self.model.entries()[index];
            if entry.hidden || self.cache.layout(entry, width, theme).is_empty() {
                continue;
            }
            return Some(Anchor {
                entry: entry.id,
                line: 0,
            });
        }
        None
    }

    fn last_anchor(&mut self, width: u16, theme: &Theme) -> Option<Anchor> {
        for index in (0..self.model.entries().len()).rev() {
            let entry = &self.model.entries()[index];
            if entry.hidden {
                continue;
            }
            let len = self.cache.layout(entry, width, theme).len();
            if len == 0 {
                continue;
            }
            return Some(Anchor {
                entry: entry.id,
                line: len - 1,
            });
        }
        None
    }

    fn resolve_anchor(&mut self, anchor: Anchor, width: u16, theme: &Theme) -> Option<Anchor> {
        let entry = self.model.entry(anchor.entry)?;
        if entry.hidden {
            return self.next_visible_entry(anchor.entry, width, theme);
        }
        let len = self.cache.layout(entry, width, theme).len();
        (len > 0).then_some(Anchor {
            entry: anchor.entry,
            line: anchor.line.min(len - 1),
        })
    }

    fn move_anchor(
        &mut self,
        mut anchor: Anchor,
        rows: i32,
        width: u16,
        theme: &Theme,
    ) -> (Anchor, bool) {
        if rows < 0 {
            for _ in 0..rows.unsigned_abs() {
                let Some(previous) = self.previous(anchor, width, theme) else {
                    return (anchor, false);
                };
                anchor = previous;
            }
            return (anchor, false);
        }
        for _ in 0..u32::try_from(rows).unwrap_or_default() {
            let Some(next) = self.next(anchor, width, theme) else {
                return (anchor, true);
            };
            anchor = next;
        }
        (anchor, false)
    }

    fn previous(&mut self, anchor: Anchor, width: u16, theme: &Theme) -> Option<Anchor> {
        if anchor.line > 0 {
            return Some(Anchor {
                line: anchor.line - 1,
                ..anchor
            });
        }
        let index = self.model.index_of(anchor.entry)?;
        for previous in (0..index).rev() {
            let entry = &self.model.entries()[previous];
            if entry.hidden {
                continue;
            }
            let len = self.cache.layout(entry, width, theme).len();
            if len > 0 {
                return Some(Anchor {
                    entry: entry.id,
                    line: len - 1,
                });
            }
        }
        None
    }

    fn next(&mut self, anchor: Anchor, width: u16, theme: &Theme) -> Option<Anchor> {
        let entry = self.model.entry(anchor.entry)?;
        let len = self.cache.layout(entry, width, theme).len();
        if anchor.line + 1 < len {
            return Some(Anchor {
                line: anchor.line + 1,
                ..anchor
            });
        }
        self.next_visible_entry(anchor.entry, width, theme)
    }

    fn next_visible_entry(
        &mut self,
        entry_id: EntryId,
        width: u16,
        theme: &Theme,
    ) -> Option<Anchor> {
        let index = self.model.index_of(entry_id)?;
        for next in index + 1..self.model.entries().len() {
            let entry = &self.model.entries()[next];
            if entry.hidden || self.cache.layout(entry, width, theme).is_empty() {
                continue;
            }
            return Some(Anchor {
                entry: entry.id,
                line: 0,
            });
        }
        None
    }

    fn collect_forward_anchors(
        &mut self,
        mut anchor: Anchor,
        height: usize,
        width: u16,
        theme: &Theme,
    ) -> Vec<Anchor> {
        let mut anchors = Vec::with_capacity(height);
        while anchors.len() < height {
            let Some(entry) = self.model.entry(anchor.entry) else {
                break;
            };
            let layout = self.cache.layout(entry, width, theme);
            if layout.get(anchor.line).is_some() {
                anchors.push(anchor);
            }
            let Some(next) = self.next(anchor, width, theme) else {
                break;
            };
            anchor = next;
        }
        anchors
    }

    fn warm_overscan(&mut self, top: Option<Anchor>, height: u16, width: u16, theme: &Theme) {
        let Some(top) = top else {
            return;
        };
        let _ = self.move_anchor(top, -i32::from(height), width, theme);
        let _ = self.move_anchor(top, i32::from(height.saturating_mul(2)), width, theme);
    }
}

fn unix_milliseconds() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn transient_label(status: &TransientStatus) -> String {
    match status {
        TransientStatus::Thinking => "Thinking…".to_owned(),
        TransientStatus::Responding => "Responding…".to_owned(),
        TransientStatus::Warming => "Warming model…".to_owned(),
        TransientStatus::WaitingForBackgroundWork => "Waiting for background work…".to_owned(),
        TransientStatus::Tool(tool) => format!("Running {tool}…"),
        TransientStatus::Compacting => "Compacting context…".to_owned(),
        TransientStatus::Retrying(delay_ns) => {
            format!("Retrying in {}…", format_duration(*delay_ns))
        }
        TransientStatus::Connecting => "Connecting…".to_owned(),
        TransientStatus::Reconnecting => "Reconnecting…".to_owned(),
        TransientStatus::Error(error) => error.clone(),
    }
}

fn is_running_tool(entry: &TranscriptEntry) -> bool {
    matches!(
        &entry.kind,
        EntryKind::Tool(tool) if tool.state == crate::tui::transcript::ToolState::Running
    )
}

fn is_expandable(entry: &TranscriptEntry) -> bool {
    matches!(
        entry.kind,
        EntryKind::Tool(_) | EntryKind::DirectedMessage(_)
    )
}

impl LayoutCache {
    fn refresh_terminal_images(&mut self) {
        self.images.advance_terminal_generation();
        self.entries
            .retain(|_, entry| entry.image_state != markdown::ImageState::Pending);
        for entry in self.entries.values_mut() {
            for image in &mut entry.images {
                image.retransmit = true;
            }
        }
    }

    fn poll_images(&mut self, now: Instant) -> bool {
        let result = self.images.poll(now);
        match result.layout_change {
            image::LayoutChange::Ready => {
                self.entries
                    .retain(|_, entry| entry.image_state == markdown::ImageState::None);
            }
            image::LayoutChange::Pending => {
                self.entries
                    .retain(|_, entry| entry.image_state != markdown::ImageState::Pending);
            }
            image::LayoutChange::None => {}
        }
        result.render_changed
    }

    fn forget(&mut self, id: EntryId) {
        self.entries.remove(&id);
        self.live_tool_durations.remove(&id);
        self.expansion_overrides.remove(&id);
    }

    fn layout(&mut self, entry: &TranscriptEntry, width: u16, theme: &Theme) -> &[Line<'static>] {
        let workspace = &self.workspace;
        let images = &mut self.images;
        let expanded = self
            .expansion_overrides
            .get(&entry.id)
            .copied()
            .or(self.expand_all)
            .unwrap_or_else(|| Self::expanded_by_default(entry));
        let live_duration_ns = self.live_tool_durations.get(&entry.id).copied();
        let cached = match self.entries.entry(entry.id) {
            Entry::Occupied(mut occupied) => {
                let cached = occupied.get();
                if cached.revision != entry.revision
                    || cached.width != width
                    || cached.expanded != expanded
                {
                    occupied.insert(CachedEntry::new(
                        entry,
                        live_duration_ns,
                        width,
                        theme,
                        expanded,
                        workspace,
                        images,
                    ));
                } else if cached.live_duration_ns != live_duration_ns {
                    occupied
                        .get_mut()
                        .update_live_duration(entry, live_duration_ns, theme);
                }
                occupied.into_mut()
            }
            Entry::Vacant(vacant) => vacant.insert(CachedEntry::new(
                entry,
                live_duration_ns,
                width,
                theme,
                expanded,
                workspace,
                images,
            )),
        };
        &cached.lines
    }

    fn set_live_tool_duration(&mut self, id: EntryId, duration_ns: u64) -> bool {
        let display_changed = self.live_tool_durations.get(&id).is_none_or(|previous| {
            duration_display_tick(*previous) != duration_display_tick(duration_ns)
        });
        if display_changed {
            self.live_tool_durations.insert(id, duration_ns);
        }
        display_changed
    }

    fn retain_live_tool_durations(&mut self, mut retain: impl FnMut(EntryId) -> bool) {
        self.live_tool_durations.retain(|id, _| retain(*id));
    }

    fn toggle(&mut self, entry: &TranscriptEntry) {
        let expanded = self
            .expansion_overrides
            .get(&entry.id)
            .copied()
            .or(self.expand_all)
            .unwrap_or_else(|| Self::expanded_by_default(entry));
        self.expansion_overrides.insert(entry.id, !expanded);
        self.entries.remove(&entry.id);
    }

    fn toggle_all(&mut self) {
        self.expand_all = Some(!matches!(self.expand_all, Some(true)));
        self.expansion_overrides.clear();
        self.entries.clear();
    }

    fn expanded_by_default(entry: &TranscriptEntry) -> bool {
        matches!(&entry.kind, EntryKind::Tool(tool) if tool.name == "update_plan")
    }

    fn line(&self, anchor: Anchor) -> Option<&Line<'static>> {
        self.entries
            .get(&anchor.entry)
            .and_then(|cached| cached.lines.get(anchor.line))
    }

    fn links(&self, anchor: Anchor) -> &[markdown::LinkSpan] {
        self.entries
            .get(&anchor.entry)
            .and_then(|cached| cached.links.get(anchor.line))
            .map_or(&[], Vec::as_slice)
    }

    fn selections(&self, anchor: Anchor) -> &[markdown::SourceSpan] {
        self.entries
            .get(&anchor.entry)
            .and_then(|cached| cached.selections.get(anchor.line))
            .map_or(&[], Vec::as_slice)
    }

    fn image(
        &mut self,
        anchor: Anchor,
    ) -> Option<(usize, Arc<ratatui_image::sliced::SlicedProtocol>)> {
        let workspace = &self.workspace;
        let images = &mut self.images;
        self.entries.get_mut(&anchor.entry).and_then(|cached| {
            let index = cached
                .images
                .partition_point(|image| image.line <= anchor.line)
                .checked_sub(1)?;
            let image = cached.images.get_mut(index)?;
            let end = image
                .line
                .saturating_add(usize::from(image.protocol.size().height));
            if anchor.line >= end {
                return None;
            }
            if image.retransmit {
                let size = image.protocol.size();
                match images.retransmit(&image.destination, workspace, size) {
                    image::LoadResult::Loaded(protocol) => {
                        image.protocol = protocol;
                        image.retransmit = false;
                    }
                    image::LoadResult::Failed | image::LoadResult::Unsupported => {
                        image.retransmit = false;
                    }
                    image::LoadResult::Deferred => {}
                }
            }
            Some((image.line, Arc::clone(&image.protocol)))
        })
    }

    fn selection_source<'a>(&'a self, entry: &'a TranscriptEntry) -> Option<&'a str> {
        self.entries
            .get(&entry.id)
            .and_then(|cached| cached.selection_source.as_deref())
            .or_else(|| entry_selection_source(entry))
    }

    fn expand_selection(&self, entry: EntryId, mut selected: Range<usize>) -> Range<usize> {
        let Some(cached) = self.entries.get(&entry) else {
            return selected;
        };
        loop {
            let previous = selected.clone();
            for envelope in &cached.envelopes {
                if selected.start <= envelope.content.start && selected.end >= envelope.content.end
                {
                    selected.start = selected.start.min(envelope.source.start);
                    selected.end = selected.end.max(envelope.source.end);
                }
            }
            if selected == previous {
                return selected;
            }
        }
    }
}

impl CachedEntry {
    fn new(
        entry: &TranscriptEntry,
        live_duration_ns: Option<u64>,
        width: u16,
        theme: &Theme,
        expanded: bool,
        workspace: &Path,
        images: &mut image::Cache,
    ) -> Self {
        let layout = render_entry(
            entry,
            live_duration_ns,
            width,
            theme,
            expanded,
            workspace,
            images,
        );
        let tool_summary_lines = match (&entry.kind, live_duration_ns) {
            (EntryKind::Tool(tool), Some(duration_ns)) => {
                render_live_tool_summary(entry, tool, duration_ns, width, theme, expanded).len()
            }
            _ => 0,
        };
        Self {
            revision: entry.revision,
            width,
            expanded,
            live_duration_ns,
            tool_summary_lines,
            lines: layout.lines,
            images: layout.images,
            links: layout.links,
            selections: layout.selections,
            envelopes: layout.envelopes,
            selection_source: layout.selection_source,
            image_state: layout.image_state,
        }
    }

    fn update_live_duration(
        &mut self,
        entry: &TranscriptEntry,
        live_duration_ns: Option<u64>,
        theme: &Theme,
    ) {
        let (EntryKind::Tool(tool), Some(duration_ns)) = (&entry.kind, live_duration_ns) else {
            return;
        };
        let summary =
            render_live_tool_summary(entry, tool, duration_ns, self.width, theme, self.expanded);
        let summary_len = summary.len();
        self.lines.splice(0..self.tool_summary_lines, summary);
        self.links.splice(
            0..self.tool_summary_lines,
            std::iter::repeat_with(Vec::new).take(summary_len),
        );
        self.selections.splice(
            0..self.tool_summary_lines,
            std::iter::repeat_with(Vec::new).take(summary_len),
        );
        self.live_duration_ns = live_duration_ns;
        self.tool_summary_lines = summary_len;
    }
}

impl Component for Transcript {
    type Event = TranscriptEvent;
    type Effect = TranscriptEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            TranscriptEvent::Record(record) => self.update_record(record),
            TranscriptEvent::DirectedMessage {
                perspective,
                update,
            } => self.update_message(perspective, update),
            TranscriptEvent::AgentStreamClosed => self.agent_stream_closed(),
            TranscriptEvent::Scroll(command) => self.update_scroll(command),
            TranscriptEvent::FollowTail => self.follow_tail(),
            TranscriptEvent::BlurExpandables => self.blur_expandables(),
            TranscriptEvent::Expandable(command) => self.update_expandable(command),
            TranscriptEvent::ToggleExpandAll => {
                self.cache.toggle_all();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            TranscriptEvent::AnimationFrame(now) => self.update_animation(now),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.viewport_height = area.height;
        self.transcript_y = area.y;
        self.transcript_x = area.x;
        self.expandable_hits.clear();
        self.link_hits.clear();
        self.selection_rows.clear();
        Clear.render(area, frame.buffer_mut());
        if self.is_empty() {
            self.empty_logo.render(frame, area, theme, self.effort);
            return;
        }
        let plan = self.render_plan(area.width, area.height, theme);
        let transcript_area = area;
        let RenderPlan {
            top_padding,
            anchors,
        } = plan;
        let mut y = transcript_area.y.saturating_add(top_padding);
        let mut visible_images: Vec<(
            EntryId,
            usize,
            Arc<ratatui_image::sliced::SlicedProtocol>,
            SignedPosition,
        )> = Vec::new();
        for anchor in anchors {
            if let Some(line) = self.cache.line(anchor) {
                frame
                    .buffer_mut()
                    .set_line(transcript_area.x, y, line, transcript_area.width);
            }
            self.link_hits
                .extend(self.cache.links(anchor).iter().map(|link| {
                    LinkHitRegion {
                        destination: Arc::clone(&link.destination),
                        row: y,
                        start: area.x.saturating_add(link.start),
                        end: transcript_area
                            .x
                            .saturating_add(link.end)
                            .min(transcript_area.right()),
                    }
                }));
            self.selection_rows.push((y, anchor));
            let covered_by_last_image =
                visible_images
                    .last()
                    .is_some_and(|(entry, start, protocol, _)| {
                        *entry == anchor.entry
                            && anchor.line >= *start
                            && anchor.line
                                < start.saturating_add(usize::from(protocol.size().height))
                    });
            if !covered_by_last_image && let Some((line, protocol)) = self.cache.image(anchor) {
                let offset = i32::try_from(anchor.line.saturating_sub(line)).unwrap_or(i32::MAX);
                let position = i32::from(y)
                    .saturating_sub(i32::from(transcript_area.y))
                    .saturating_sub(offset)
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                visible_images.push((
                    anchor.entry,
                    line,
                    protocol,
                    SignedPosition::from((0, position as i16)),
                ));
            }
            if anchor.line == 0
                && let Some(entry) = self.model.entry(anchor.entry)
            {
                if !is_expandable(entry) {
                    y = y.saturating_add(1);
                    continue;
                }
                self.expandable_hits.push(ExpandableHitRegion {
                    entry: anchor.entry,
                    row: y,
                });
                if matches!(
                    &entry.kind,
                    EntryKind::Tool(tool)
                        if tool.state == crate::tui::transcript::ToolState::Running
                ) && let Some(spinner) = self.tool_spinner
                {
                    let spinner_x = transcript_area
                        .x
                        .saturating_add(4)
                        .saturating_add(nested_tool_indent(entry, transcript_area.width));
                    if spinner_x < transcript_area.right() {
                        frame.buffer_mut().set_string(
                            spinner_x,
                            y,
                            spinner.symbol(),
                            Style::default()
                                .fg(theme.accent())
                                .add_modifier(Modifier::BOLD),
                        );
                    }
                }
                if self.expandables_focused && self.selected_expandable == Some(anchor.entry) {
                    frame.buffer_mut().set_string(
                        transcript_area.x,
                        y,
                        "›",
                        Style::default()
                            .fg(theme.accent())
                            .add_modifier(Modifier::BOLD),
                    );
                }
            }
            y = y.saturating_add(1);
        }
        for (_, _, protocol, position) in visible_images {
            frame.render_widget(SlicedImage::new(&protocol, position), transcript_area);
        }
    }
}

fn render_top_right_hint(
    frame: &mut Frame<'_>,
    area: Rect,
    labels: &[&str],
    color: Color,
) -> Option<Rect> {
    let label = labels
        .iter()
        .copied()
        .find(|label| line_width(label) <= usize::from(area.width))?;
    let width = u16::try_from(line_width(label)).unwrap_or(u16::MAX);
    let x = area.right().saturating_sub(width);
    frame.buffer_mut().set_line(
        x,
        area.y,
        &Line::from(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        area.right().saturating_sub(x),
    );
    Some(Rect::new(x, area.y, width, 1))
}

fn render_entry(
    entry: &TranscriptEntry,
    live_duration_ns: Option<u64>,
    width: u16,
    theme: &Theme,
    expanded: bool,
    workspace: &Path,
    images: &mut image::Cache,
) -> markdown::Layout {
    let mut layout = match &entry.kind {
        EntryKind::User { text, .. } => render_user(text, width, theme),
        EntryKind::Assistant { text, .. } => {
            markdown::render_cached(text, width, theme, workspace, images)
        }
        EntryKind::Reasoning { text } => {
            let mut layout =
                markdown::render_cached(text, width.saturating_sub(2), theme, workspace, images);
            for line in &mut layout.lines {
                for span in &mut line.spans {
                    span.style = span.style.patch(
                        Style::default()
                            .fg(theme.muted())
                            .add_modifier(Modifier::ITALIC),
                    );
                }
            }
            layout
        }
        EntryKind::Tool(tool) => {
            let indent = nested_tool_indent(entry, width);
            let tool_width = width.saturating_sub(indent);
            let mut layout =
                tool::render_layout(tool, live_duration_ns, tool_width, theme, expanded);
            indent_nested_tool(
                indent,
                &mut layout.lines,
                theme,
                expanded,
                entry.trailing_spacer,
            );
            for spans in &mut layout.selections {
                for span in spans {
                    span.columns.start = span.columns.start.saturating_add(indent);
                    span.columns.end = span.columns.end.saturating_add(indent);
                }
            }
            layout
        }
        EntryKind::DirectedMessage(thread) => {
            layout_without_links(message::render(thread, width, theme, expanded))
        }
        EntryKind::ForkedFrom { session_id } => {
            layout_without_links(vec![Line::from(Span::styled(
                format!("◇ Forked from @@{session_id}"),
                Style::default().fg(theme.muted()),
            ))])
        }
        EntryKind::EffortChanged { to } => layout_without_links(vec![Line::from(vec![
            Span::styled("◇ Effort changed to ", Style::default().fg(theme.muted())),
            Span::styled(
                to.as_str(),
                Style::default()
                    .fg(theme.effort(*to))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " · takes effect on the next turn",
                Style::default().fg(theme.muted()),
            ),
        ])]),
        EntryKind::FastModeChanged { enabled } => {
            let status = if *enabled { "enabled" } else { "disabled" };
            layout_without_links(vec![Line::from(vec![
                Span::styled(
                    "⚡ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("Fast mode {status}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " · takes effect on the next turn",
                    Style::default().fg(theme.muted()),
                ),
            ])])
        }
        EntryKind::ReflectionStarted => layout_without_links(vec![Line::from(Span::styled(
            "◇ Reflection started",
            Style::default().fg(theme.muted()),
        ))]),
        EntryKind::Interrupted { count } => {
            let label = if *count == 1 {
                "◇ Interrupted response".to_owned()
            } else {
                format!("◇ Interrupted {count} responses")
            };
            layout_without_links(vec![Line::from(Span::styled(
                label,
                Style::default().fg(theme.border()),
            ))])
        }
        EntryKind::ContextCompacted { duration_ns } => {
            layout_without_links(vec![Line::from(Span::styled(
                format!("◇ Context compacted · {}", format_duration(*duration_ns)),
                Style::default().fg(theme.muted()),
            ))])
        }
        EntryKind::TurnCompleted { duration_ns } => {
            layout_without_links(vec![Line::from(Span::styled(
                format!("◇ Turn completed · {}", format_turn_duration(*duration_ns)),
                Style::default().fg(theme.muted()),
            ))])
        }
        EntryKind::ContextCompactionFailed { message } => {
            layout_without_links(vec![Line::from(Span::styled(
                format!("◇ Context compaction failed · continuing · {message}"),
                Style::default().fg(theme.thinking_high()),
            ))])
        }
        EntryKind::Error { message } => layout_without_links(markdown::wrap_plain(
            &format!("× {message}"),
            width,
            Style::default().fg(theme.thinking_xhigh()),
        )),
    };
    if entry.trailing_spacer {
        layout.lines.push(Line::default());
        layout.links.push(Vec::new());
        layout.selections.push(Vec::new());
    }
    layout
}

fn render_live_tool_summary(
    entry: &TranscriptEntry,
    tool: &crate::tui::transcript::ToolEntry,
    duration_ns: u64,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Vec<Line<'static>> {
    let indent = nested_tool_indent(entry, width);
    let tool_width = width.saturating_sub(indent);
    let mut lines = tool::render_live_summary(tool, duration_ns, tool_width, theme, expanded);
    indent_nested_tool(indent, &mut lines, theme, expanded, entry.trailing_spacer);
    lines
}

const fn nested_tool_indent(entry: &TranscriptEntry, width: u16) -> u16 {
    if entry.parent.is_some() {
        let available = width.saturating_sub(1);
        if available < NESTED_TOOL_INDENT {
            available
        } else {
            NESTED_TOOL_INDENT
        }
    } else {
        0
    }
}

fn indent_nested_tool(
    indent: u16,
    lines: &mut [Line<'static>],
    theme: &Theme,
    expanded: bool,
    terminal: bool,
) {
    let (terminal_marker, continuing_marker, continuation) = match indent {
        0 => return,
        1 => ("└", "├", "│"),
        2 => ("└─", "├─", "│ "),
        3 => (" └─", " ├─", " │ "),
        _ => ("  └─", "  ├─", "  │ "),
    };
    if lines.is_empty() {
        return;
    }
    let line_count = lines.len();
    for (index, line) in lines.iter_mut().enumerate() {
        let marker = if index == 0 {
            if expanded || !terminal || line_count > 1 {
                continuing_marker
            } else {
                terminal_marker
            }
        } else if !expanded && terminal && index + 1 == line_count {
            terminal_marker
        } else {
            continuation
        };
        line.spans
            .insert(0, Span::styled(marker, Style::default().fg(theme.border())));
    }
}

fn entry_selection_source(entry: &TranscriptEntry) -> Option<&str> {
    match &entry.kind {
        EntryKind::User { text }
        | EntryKind::Assistant { text, .. }
        | EntryKind::Reasoning { text } => Some(text),
        _ => None,
    }
}

fn layout_without_links(lines: Vec<Line<'static>>) -> markdown::Layout {
    let links = vec![Vec::new(); lines.len()];
    let selections = vec![Vec::new(); lines.len()];
    markdown::Layout {
        lines,
        images: Vec::new(),
        links,
        selections,
        envelopes: Vec::new(),
        selection_source: None,
        image_state: markdown::ImageState::None,
    }
}

fn render_user(text: &str, width: u16, theme: &Theme) -> markdown::Layout {
    let text = normalize_line_endings(text);
    let color = theme.thinking_medium();
    let content_width = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut selections = Vec::new();
    let mut source_offset = 0;
    for logical in text.split('\n') {
        let wrapped = markdown::wrap_plain_preserving_whitespace(
            logical,
            content_width,
            Style::default().fg(color),
        );
        let wrapped_selections = markdown::plain_selection_spans(logical, &wrapped);
        for (line, mut line_selections) in wrapped.into_iter().zip(wrapped_selections) {
            for selection in &mut line_selections {
                selection.columns.start = selection.columns.start.saturating_add(2);
                selection.columns.end = selection.columns.end.saturating_add(2);
                selection.source.start = selection.source.start.saturating_add(source_offset);
                selection.source.end = selection.source.end.saturating_add(source_offset);
            }
            lines.push(Line::from(
                std::iter::once(Span::styled("┃ ", Style::default().fg(color)))
                    .chain(line.spans)
                    .collect::<Vec<_>>(),
            ));
            selections.push(line_selections);
        }
        source_offset = source_offset
            .saturating_add(logical.len())
            .saturating_add(1);
    }
    markdown::Layout {
        links: vec![Vec::new(); lines.len()],
        lines,
        images: Vec::new(),
        selections,
        envelopes: Vec::new(),
        selection_source: match text {
            std::borrow::Cow::Borrowed(_) => None,
            std::borrow::Cow::Owned(text) => Some(text),
        },
        image_state: markdown::ImageState::None,
    }
}

fn line_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

#[cfg(test)]
mod history_tests {
    use super::{Anchor, Component, ScrollCommand, ScrollState, Transcript, TranscriptEvent};
    use crate::tui::{
        theme::Theme,
        transcript::{LocalEvent, TranscriptRecord, TurnId},
    };
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use std::sync::Arc;

    #[test]
    fn replayed_prefix_preserves_the_detached_viewport_anchor() {
        let mut previous = Transcript::new();
        for (sequence, text) in [(1, "recent one"), (2, "recent two")] {
            let _ = previous.update(TranscriptEvent::Record(user(sequence, text)));
        }
        let previous_anchor = Anchor {
            entry: previous.model.entries()[0].id,
            line: 0,
        };
        previous.scroll = ScrollState::Detached(previous_anchor);
        previous.last_top = Some(previous_anchor);
        previous.new_updates = 3;

        let mut replayed = Transcript::new();
        for (sequence, text) in [(1, "older"), (2, "recent one"), (3, "recent two")] {
            let _ = replayed.update(TranscriptEvent::Record(user(sequence, text)));
        }
        replayed.preserve_viewport_from(&previous);

        let ScrollState::Detached(anchor) = replayed.scroll else {
            panic!("history replay must remain detached");
        };
        assert_eq!(replayed.model.index_of(anchor.entry), Some(1));
        assert_eq!(anchor.line, 0);
        assert_eq!(replayed.new_updates, 3);
    }

    #[test]
    fn wheel_scroll_uses_one_third_of_the_viewport() {
        let mut transcript = Transcript::new();
        transcript.viewport_height = 32;

        assert_eq!(
            transcript.scroll_command(&wheel(MouseEventKind::ScrollUp)),
            Some(ScrollCommand::Rows(-10))
        );
        assert_eq!(
            transcript.scroll_command(&wheel(MouseEventKind::ScrollDown)),
            Some(ScrollCommand::Rows(10))
        );
    }

    #[test]
    fn near_top_covers_three_viewports_before_the_visible_top() {
        let mut transcript = Transcript::new();
        for sequence in 1..=20 {
            let _ = transcript.update(TranscriptEvent::Record(user(sequence, "message")));
        }

        transcript.scroll = ScrollState::Detached(Anchor {
            entry: transcript.model.entries()[2].id,
            line: 0,
        });
        let _ = transcript.render_plan(80, 5, &Theme::default());
        assert!(transcript.near_top());

        transcript.scroll = ScrollState::Detached(Anchor {
            entry: transcript.model.entries()[6].id,
            line: 0,
        });
        let _ = transcript.render_plan(80, 5, &Theme::default());
        assert!(transcript.near_top());

        transcript.scroll = ScrollState::Detached(Anchor {
            entry: transcript.model.entries()[10].id,
            line: 0,
        });
        let _ = transcript.render_plan(80, 5, &Theme::default());
        assert!(!transcript.near_top());
    }

    fn wheel(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn user(sequence: u64, text: &str) -> Arc<TranscriptRecord> {
        Arc::new(
            TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: text.to_owned(),
                },
            )
            .unwrap(),
        )
    }
}
