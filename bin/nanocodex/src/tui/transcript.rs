use std::{
    borrow::Cow,
    collections::VecDeque,
    mem,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::composer::ComposerLayout;
use super::diff::{PatchPresentation, present_apply_patch};
use super::markdown::{
    code_line_count, heal_streaming_markdown, highlighted_code_lines, render_agent_markdown,
};

#[derive(Clone, Copy)]
pub(super) struct InlineEdit<'a> {
    pub(super) index: usize,
    pub(super) input: &'a str,
    pub(super) cursor: usize,
}

pub(super) enum TranscriptItem {
    User(String),
    Reasoning(String),
    Assistant(String),
    Tool {
        call_id: String,
        name: String,
        arguments: String,
        status: ToolStatus,
    },
    Error(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Default)]
pub(super) struct Transcript {
    entries: Vec<Arc<TranscriptEntry>>,
    editable_users: Vec<usize>,
    cached_total_height: AtomicU64,
}

impl Clone for Transcript {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            editable_users: self.editable_users.clone(),
            cached_total_height: AtomicU64::new(self.cached_total_height.load(Ordering::Relaxed)),
        }
    }
}

impl Transcript {
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn push(&mut self, item: TranscriptItem) {
        self.entries.push(Arc::new(TranscriptEntry::new(item)));
        self.invalidate_total_height();
    }

    pub(super) fn has_tool_parent(&self, call_id: &str) -> bool {
        let Some((parent, _)) = call_id.split_once("/code-") else {
            return false;
        };
        self.entries
            .iter()
            .rev()
            .any(|entry| matches!(&entry.kind, EntryKind::Tool { call_id } if call_id == parent))
    }

    pub(super) fn push_tool_child(
        &mut self,
        call_id: String,
        name: String,
        arguments: String,
        status: ToolStatus,
    ) -> bool {
        let Some((parent, _)) = call_id.split_once("/code-") else {
            return false;
        };
        let Some(entry) =
            self.entries.iter_mut().rev().find(
                |entry| matches!(&entry.kind, EntryKind::Tool { call_id } if call_id == parent),
            )
        else {
            return false;
        };
        Arc::make_mut(entry).push_tool_child(call_id, name, arguments, status);
        self.invalidate_total_height();
        true
    }

    pub(super) fn push_editable_user(&mut self, message: String, prompt_id: u64) {
        let mut entry = TranscriptEntry::new(TranscriptItem::User(message));
        entry.prompt_id = Some(prompt_id);
        self.editable_users.push(self.entries.len());
        self.entries.push(Arc::new(entry));
        self.invalidate_total_height();
    }

    pub(super) fn append_assistant_delta(&mut self, delta: &str) -> bool {
        let appended = self
            .entries
            .last_mut()
            .is_some_and(|entry| Arc::make_mut(entry).append_assistant_delta(delta));
        if appended {
            self.invalidate_total_height();
        }
        appended
    }

    pub(super) fn append_reasoning_delta(&mut self, delta: &str) -> bool {
        let appended = self
            .entries
            .last_mut()
            .is_some_and(|entry| Arc::make_mut(entry).append_reasoning_delta(delta));
        if appended {
            self.invalidate_total_height();
        }
        appended
    }

    pub(super) fn finalize_assistant(&mut self, message: &str) -> bool {
        let finalized = self
            .entries
            .last_mut()
            .is_some_and(|entry| Arc::make_mut(entry).finalize_assistant(message));
        if finalized {
            self.invalidate_total_height();
        }
        finalized
    }

    pub(super) fn tail_is_assistant(&self) -> bool {
        self.entries
            .last()
            .is_some_and(|entry| matches!(entry.kind, EntryKind::Assistant))
    }

    pub(super) fn tail_is_reasoning(&self) -> bool {
        self.entries
            .last()
            .is_some_and(|entry| matches!(entry.kind, EntryKind::Reasoning))
    }

    pub(super) fn tail_height(&self, width: u16) -> Option<usize> {
        self.entries.last().map(|entry| entry.height(width))
    }

    pub(super) fn height_at(&self, index: usize, width: u16) -> Option<usize> {
        self.entries.get(index).map(|entry| entry.height(width))
    }

    pub(super) fn height_from(&self, first: usize, width: u16) -> usize {
        if first == 0 {
            return self.total_height(width);
        }
        self.entries[first..]
            .iter()
            .map(|entry| entry.height(width))
            .sum()
    }

    #[cfg(test)]
    pub(super) fn max_scroll_from_bottom(&self, width: u16, viewport_height: u16) -> usize {
        self.total_height(width)
            .saturating_sub(usize::from(viewport_height))
    }

    pub(super) fn clamp_scroll_from_bottom(
        &self,
        scroll_from_bottom: usize,
        width: u16,
        viewport_height: u16,
    ) -> usize {
        let viewport_height = usize::from(viewport_height);
        let needed_height = scroll_from_bottom.saturating_add(viewport_height);
        let available_height = self.height_up_to(width, needed_height);
        if available_height >= needed_height {
            scroll_from_bottom
        } else {
            scroll_from_bottom.min(available_height.saturating_sub(viewport_height))
        }
    }

    pub(super) fn height_up_to(&self, width: u16, limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        let mut height = 0_usize;
        for entry in self.entries.iter().rev() {
            height = height.saturating_add(entry.height(width));
            if height >= limit {
                return limit;
            }
        }
        height
    }

    pub(super) fn previous_user(&self, before: Option<usize>) -> Option<usize> {
        let before = before.unwrap_or(self.entries.len()).min(self.entries.len());
        let position = self.editable_users.partition_point(|index| *index < before);
        position
            .checked_sub(1)
            .and_then(|position| self.editable_users.get(position).copied())
    }

    pub(super) fn next_user(&self, after: usize) -> Option<usize> {
        let position = self.editable_users.partition_point(|index| *index <= after);
        self.editable_users.get(position).copied()
    }

    #[cfg(test)]
    pub(super) fn user_message(&self, index: usize) -> Option<&str> {
        self.entries.get(index)?.user_message()
    }

    pub(super) fn user_edit_target(&self, index: usize) -> Option<(u64, &str)> {
        let entry = self.entries.get(index)?;
        Some((entry.prompt_id?, entry.user_message()?))
    }

    pub(super) fn latest_user_message(&self) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| entry.user_message())
    }

    pub(super) fn prefix_before(&self, index: usize) -> Self {
        let end = index.min(self.entries.len());
        Self {
            entries: self.entries[..end].to_vec(),
            editable_users: self.editable_users
                [..self.editable_users.partition_point(|i| *i < end)]
                .to_vec(),
            cached_total_height: AtomicU64::new(0),
        }
    }

    pub(super) fn set_tool_result(
        &mut self,
        call_id: &str,
        status: ToolStatus,
        duration_ns: Option<u64>,
        result: Option<String>,
    ) -> bool {
        let parent_id = call_id
            .split_once("/code-")
            .map_or(call_id, |(parent, _)| parent);
        if let Some(entry) = self.entries.iter_mut().rev().find(
            |entry| matches!(&entry.kind, EntryKind::Tool { call_id } if call_id == parent_id),
        ) {
            Arc::make_mut(entry).set_tool_result(call_id, status, duration_ns, result);
            self.invalidate_total_height();
            return true;
        }
        false
    }

    fn total_height(&self, width: u16) -> usize {
        let cached = self.cached_total_height.load(Ordering::Relaxed);
        if cached != 0 && cached >> 48 == u64::from(width) {
            return usize::try_from(cached & ((1_u64 << 48) - 1)).unwrap_or(usize::MAX);
        }
        let height = self
            .entries
            .iter()
            .map(|entry| entry.height(width))
            .sum::<usize>();
        let encoded = (u64::from(width) << 48)
            | u64::try_from(height)
                .unwrap_or((1_u64 << 48) - 1)
                .min((1_u64 << 48) - 1);
        self.cached_total_height.store(encoded, Ordering::Relaxed);
        height
    }

    fn invalidate_total_height(&self) {
        self.cached_total_height.store(0, Ordering::Relaxed);
    }

    pub(super) fn widget<'a>(
        &'a self,
        scroll_from_bottom: usize,
        selected: Option<usize>,
        inline_edit: Option<InlineEdit<'a>>,
        empty_message: &'static str,
    ) -> TranscriptWidget<'a> {
        TranscriptWidget {
            transcript: self,
            scroll_from_bottom,
            selected,
            inline_edit,
            empty_message,
        }
    }

    pub(super) fn inline_edit_cursor(
        &self,
        area: Rect,
        scroll_from_bottom: usize,
        selected: Option<usize>,
        edit: InlineEdit<'_>,
    ) -> Option<Position> {
        if area.is_empty() || selected != Some(edit.index) || edit.index >= self.entries.len() {
            return None;
        }
        let viewport = selection_viewport(
            &self.entries,
            area,
            scroll_from_bottom,
            edit.index,
            Some(edit),
        );
        let mut screen_y = viewport.screen_y;
        let mut local_scroll = viewport.local_scroll;
        for (index, entry) in self.entries.iter().enumerate().skip(viewport.first) {
            let entry_height = rendered_height(entry, index, area.width, Some(edit));
            if index == edit.index {
                let layout = ComposerLayout::new(edit.input, area.width.saturating_sub(2).max(1));
                let cursor = layout.cursor_position(edit.input, edit.cursor);
                let local_y = cursor.row.saturating_add(1);
                if local_y < local_scroll {
                    return None;
                }
                let y = screen_y.saturating_add(local_y.saturating_sub(local_scroll));
                if y >= usize::from(area.height) {
                    return None;
                }
                return Some(Position::new(
                    area.x.saturating_add(
                        saturating_u16(cursor.column.saturating_add(1))
                            .min(area.width.saturating_sub(1)),
                    ),
                    area.y.saturating_add(saturating_u16(y)),
                ));
            }
            screen_y = screen_y.saturating_add(entry_height.saturating_sub(local_scroll));
            if screen_y >= usize::from(area.height) {
                return None;
            }
            local_scroll = 0;
        }
        None
    }
}

pub(super) struct TranscriptWidget<'a> {
    transcript: &'a Transcript,
    scroll_from_bottom: usize,
    selected: Option<usize>,
    inline_edit: Option<InlineEdit<'a>>,
    empty_message: &'static str,
}

impl Widget for TranscriptWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        if self.transcript.entries.is_empty() {
            Paragraph::new(Text::from(vec![
                Line::raw(""),
                Line::styled(
                    format!("  {}", self.empty_message),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
            .wrap(Wrap { trim: false })
            .render(area, buffer);
            return;
        }

        if let Some(selected) = self
            .selected
            .filter(|index| *index < self.transcript.entries.len())
        {
            let viewport = selection_viewport(
                &self.transcript.entries,
                area,
                self.scroll_from_bottom,
                selected,
                self.inline_edit,
            );
            render_entries(
                &self.transcript.entries,
                area,
                buffer,
                viewport,
                Some(selected),
                self.inline_edit,
            );
            return;
        }

        let width = area.width;
        let viewport_height = usize::from(area.height);
        let viewport_top_from_bottom = self.scroll_from_bottom.saturating_add(viewport_height);
        let mut height_below = 0_usize;
        let mut first_visible = None;
        let mut first_visible_height_below = 0_usize;

        for (index, entry) in self.transcript.entries.iter().enumerate().rev() {
            let entry_height = entry.height(width);
            let entry_top_from_bottom = height_below.saturating_add(entry_height);
            if entry_top_from_bottom > self.scroll_from_bottom
                && height_below < viewport_top_from_bottom
            {
                first_visible = Some(index);
                first_visible_height_below = height_below;
            }
            height_below = entry_top_from_bottom;
            if height_below >= viewport_top_from_bottom {
                break;
            }
        }

        if height_below < viewport_top_from_bottom {
            // The requested offset is above the available content. Match the
            // clamped scroll behavior by rendering from the first entry.
            render_entries(
                &self.transcript.entries,
                area,
                buffer,
                Viewport {
                    first: 0,
                    local_scroll: 0,
                    screen_y: 0,
                },
                self.selected,
                self.inline_edit,
            );
            return;
        }

        let Some(first_visible) = first_visible else {
            return;
        };
        let first_height = self.transcript.entries[first_visible].height(width);
        let first_top_from_bottom = first_visible_height_below.saturating_add(first_height);
        let local_scroll = first_top_from_bottom.saturating_sub(viewport_top_from_bottom);
        let screen_y = viewport_top_from_bottom.saturating_sub(first_top_from_bottom);
        render_entries(
            &self.transcript.entries,
            area,
            buffer,
            Viewport {
                first: first_visible,
                local_scroll,
                screen_y,
            },
            self.selected,
            self.inline_edit,
        );
    }
}

#[derive(Clone, Copy)]
struct Viewport {
    first: usize,
    local_scroll: usize,
    screen_y: usize,
}

fn selection_viewport(
    entries: &[Arc<TranscriptEntry>],
    area: Rect,
    scroll_from_bottom: usize,
    selected: usize,
    inline_edit: Option<InlineEdit<'_>>,
) -> Viewport {
    const REVEAL_PADDING: usize = 1;

    let (current, selected_is_visible) =
        bottom_viewport(entries, area, scroll_from_bottom, selected, inline_edit);
    if selected_is_visible {
        return current;
    }

    let mut first = selected;
    let mut rows_above = 0_usize;
    let mut local_scroll = 0_usize;

    for index in (0..selected).rev() {
        let height = rendered_height(&entries[index], index, area.width, inline_edit);
        first = index;
        if rows_above.saturating_add(height) >= REVEAL_PADDING {
            local_scroll = rows_above
                .saturating_add(height)
                .saturating_sub(REVEAL_PADDING);
            break;
        }
        rows_above = rows_above.saturating_add(height);
    }

    Viewport {
        first,
        local_scroll,
        screen_y: 0,
    }
}

fn bottom_viewport(
    entries: &[Arc<TranscriptEntry>],
    area: Rect,
    scroll_from_bottom: usize,
    selected: usize,
    inline_edit: Option<InlineEdit<'_>>,
) -> (Viewport, bool) {
    const PADDING: usize = 1;

    let viewport_height = usize::from(area.height);
    let viewport_top_from_bottom = scroll_from_bottom.saturating_add(viewport_height);
    let mut height_below = 0_usize;
    let mut first_visible = None;
    let mut first_visible_height_below = 0_usize;
    let mut selected_bounds = None;
    for (index, entry) in entries.iter().enumerate().rev() {
        let height = rendered_height(entry, index, area.width, inline_edit);
        let top_from_bottom = height_below.saturating_add(height);
        if index == selected {
            selected_bounds = Some((height_below, top_from_bottom));
        }
        if top_from_bottom > scroll_from_bottom && height_below < viewport_top_from_bottom {
            first_visible = Some(index);
            first_visible_height_below = height_below;
        }
        height_below = top_from_bottom;
        if height_below >= viewport_top_from_bottom {
            break;
        }
    }
    if height_below < viewport_top_from_bottom {
        let visible = selected_bounds.is_some_and(|(bottom, top)| {
            let screen_top = height_below.saturating_sub(top);
            let screen_bottom = height_below.saturating_sub(bottom);
            screen_top >= PADDING.min(viewport_height)
                && screen_bottom <= viewport_height.saturating_sub(PADDING.min(viewport_height))
        });
        return (
            Viewport {
                first: 0,
                local_scroll: 0,
                screen_y: 0,
            },
            visible,
        );
    }
    let first = first_visible.unwrap_or(0);
    let first_height = rendered_height(&entries[first], first, area.width, inline_edit);
    let first_top_from_bottom = first_visible_height_below.saturating_add(first_height);
    let visible = selected_bounds.is_some_and(|(bottom, top)| {
        top <= viewport_top_from_bottom.saturating_sub(PADDING.min(viewport_height))
            && bottom >= scroll_from_bottom.saturating_add(PADDING.min(viewport_height))
    });
    (
        Viewport {
            first,
            local_scroll: first_top_from_bottom.saturating_sub(viewport_top_from_bottom),
            screen_y: viewport_top_from_bottom.saturating_sub(first_top_from_bottom),
        },
        visible,
    )
}

fn render_entries(
    entries: &[Arc<TranscriptEntry>],
    area: Rect,
    buffer: &mut Buffer,
    viewport: Viewport,
    selected: Option<usize>,
    inline_edit: Option<InlineEdit<'_>>,
) {
    let mut local_scroll = viewport.local_scroll;
    let mut screen_y = viewport.screen_y;
    let viewport_height = usize::from(area.height);
    for (index, entry) in entries.iter().enumerate().skip(viewport.first) {
        if screen_y >= viewport_height {
            break;
        }
        let entry_height = rendered_height(entry, index, area.width, inline_edit);
        let visible_height = entry_height
            .saturating_sub(local_scroll)
            .min(viewport_height.saturating_sub(screen_y));
        if visible_height > 0 {
            let entry_area = Rect::new(
                area.x,
                area.y.saturating_add(saturating_u16(screen_y)),
                area.width,
                saturating_u16(visible_height),
            );
            if let Some(edit) = inline_edit.filter(|edit| edit.index == index) {
                rendered_edit_paragraph(edit, area.width)
                    .scroll((saturating_u16(local_scroll), 0))
                    .render(entry_area, buffer);
            } else {
                entry.render(
                    entry_area,
                    buffer,
                    local_scroll,
                    entry_height,
                    selected == Some(index),
                );
            }
            screen_y = screen_y.saturating_add(visible_height);
        }
        local_scroll = 0;
    }
}

fn rendered_height(
    entry: &TranscriptEntry,
    index: usize,
    width: u16,
    inline_edit: Option<InlineEdit<'_>>,
) -> usize {
    inline_edit.filter(|edit| edit.index == index).map_or_else(
        || entry.height(width),
        |edit| {
            inline_edit_layout(edit.input, width)
                .row_count()
                .saturating_add(2)
        },
    )
}

fn rendered_edit_paragraph(edit: InlineEdit<'_>, width: u16) -> Paragraph<'static> {
    Paragraph::new(inline_edit_text(edit.input, width))
        .block(
            Block::default()
                .title(" Edit message · Esc cancel ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false })
}

fn inline_edit_text(input: &str, width: u16) -> Text<'static> {
    let layout = inline_edit_layout(input, width);
    let mut lines = Vec::with_capacity(layout.row_count());
    lines.extend(
        (0..layout.row_count())
            .filter_map(|row| layout.row(row))
            .map(|range| Line::styled(input[range.clone()].to_owned(), Color::White)),
    );
    Text::from(lines)
}

fn inline_edit_layout(input: &str, width: u16) -> ComposerLayout {
    ComposerLayout::new(input, width.saturating_sub(2).max(1))
}

#[derive(Clone)]
enum EntryKind {
    User,
    Reasoning,
    Assistant,
    Tool { call_id: String },
    Error,
}

struct TranscriptEntry {
    kind: EntryKind,
    user_message: Option<String>,
    prompt_id: Option<u64>,
    content: EntryContent,
    cached_height: AtomicU64,
}

#[derive(Clone)]
enum EntryContent {
    Static(Text<'static>),
    Streaming(StreamingText),
    Markdown(MarkdownContent),
    Tool(ToolActivity),
}

#[derive(Clone)]
struct ToolActivity {
    call_id: String,
    name: String,
    arguments: String,
    status: ToolStatus,
    duration_ns: Option<u64>,
    result: Option<String>,
    children: Vec<ToolActivity>,
    highlighted_source: Option<Vec<Line<'static>>>,
    patch: Option<PatchPresentation>,
}

struct MarkdownContent {
    source: String,
    streaming: bool,
    cached: Mutex<Option<RenderedMarkdown>>,
}

#[derive(Clone)]
struct RenderedMarkdown {
    width: u16,
    text: Text<'static>,
    height: usize,
}

struct StreamingText {
    lines: Vec<StreamingLine>,
    body_style: Style,
    continuation_prefix: &'static str,
}

struct StreamingLine {
    content: String,
    style: Style,
    cached_layout: Mutex<Option<StreamingLineLayout>>,
}

#[derive(Clone)]
struct StreamingLineLayout {
    width: u16,
    rows: Vec<String>,
}

impl Clone for StreamingText {
    fn clone(&self) -> Self {
        Self {
            lines: self.lines.clone(),
            body_style: self.body_style,
            continuation_prefix: self.continuation_prefix,
        }
    }
}

impl Clone for StreamingLine {
    fn clone(&self) -> Self {
        Self {
            content: self.content.clone(),
            style: self.style,
            cached_layout: Mutex::new(
                self.cached_layout
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
        }
    }
}

impl Clone for MarkdownContent {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            streaming: self.streaming,
            cached: Mutex::new(
                self.cached
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
        }
    }
}

impl Clone for TranscriptEntry {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            user_message: self.user_message.clone(),
            prompt_id: self.prompt_id,
            content: self.content.clone(),
            cached_height: AtomicU64::new(self.cached_height.load(Ordering::Relaxed)),
        }
    }
}

impl TranscriptEntry {
    fn new(item: TranscriptItem) -> Self {
        let (kind, user_message, content) = match item {
            TranscriptItem::User(message) => {
                let text = message_text(
                    "› You",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    &message,
                );
                (EntryKind::User, Some(message), EntryContent::Static(text))
            }
            TranscriptItem::Assistant(message) => (
                EntryKind::Assistant,
                None,
                EntryContent::Markdown(MarkdownContent::streaming(&message)),
            ),
            TranscriptItem::Reasoning(message) => (
                EntryKind::Reasoning,
                None,
                EntryContent::Streaming(StreamingText::reasoning(&message)),
            ),
            TranscriptItem::Tool {
                call_id,
                name,
                arguments,
                status,
            } => (
                EntryKind::Tool {
                    call_id: call_id.clone(),
                },
                None,
                EntryContent::Tool(ToolActivity::new(call_id, name, arguments, status)),
            ),
            TranscriptItem::Error(message) => (
                EntryKind::Error,
                None,
                EntryContent::Static(Text::from(vec![
                    Line::styled(format!("✗ {message}"), Style::default().fg(Color::Red)),
                    Line::raw(""),
                ])),
            ),
        };
        Self {
            kind,
            user_message,
            prompt_id: None,
            content,
            cached_height: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    fn paragraph(&self) -> Paragraph<'static> {
        Paragraph::new(match &self.content {
            EntryContent::Static(text) => text.clone(),
            EntryContent::Streaming(streaming) => streaming.materialized_text(),
            EntryContent::Markdown(markdown) => markdown.text(80),
            EntryContent::Tool(tool) => tool.text(),
        })
        .wrap(Wrap { trim: false })
    }

    fn user_message(&self) -> Option<&str> {
        self.user_message.as_deref()
    }

    fn height(&self, width: u16) -> usize {
        let cached = self.cached_height.load(Ordering::Relaxed);
        if cached != 0 && cached >> 48 == u64::from(width) {
            return usize::try_from(cached & ((1_u64 << 48) - 1)).unwrap_or(usize::MAX);
        }
        let height = match &self.content {
            EntryContent::Static(text) => Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .line_count(width),
            EntryContent::Streaming(streaming) => streaming.height(width),
            EntryContent::Markdown(markdown) => markdown.height(width),
            EntryContent::Tool(tool) => Paragraph::new(tool.text())
                .wrap(Wrap { trim: false })
                .line_count(width),
        };
        let encoded = (u64::from(width) << 48)
            | u64::try_from(height)
                .unwrap_or((1_u64 << 48) - 1)
                .min((1_u64 << 48) - 1);
        self.cached_height.store(encoded, Ordering::Relaxed);
        height
    }

    fn render(
        &self,
        area: Rect,
        buffer: &mut Buffer,
        scroll: usize,
        total_height: usize,
        selected: bool,
    ) {
        match &self.content {
            EntryContent::Static(text) => {
                let mut paragraph = Paragraph::new(text.clone()).wrap(Wrap { trim: false });
                if selected {
                    paragraph = paragraph.style(Style::default().add_modifier(Modifier::REVERSED));
                }
                paragraph
                    .scroll((saturating_u16(scroll), 0))
                    .render(area, buffer);
            }
            EntryContent::Streaming(streaming) => {
                streaming.render(area, buffer, scroll, total_height, selected);
            }
            EntryContent::Markdown(markdown) => {
                let mut paragraph =
                    Paragraph::new(markdown.text(area.width)).wrap(Wrap { trim: false });
                if selected {
                    paragraph = paragraph.style(Style::default().add_modifier(Modifier::REVERSED));
                }
                paragraph
                    .scroll((saturating_u16(scroll), 0))
                    .render(area, buffer);
            }
            EntryContent::Tool(tool) => {
                let mut paragraph = Paragraph::new(tool.text()).wrap(Wrap { trim: false });
                if selected {
                    paragraph = paragraph.style(Style::default().add_modifier(Modifier::REVERSED));
                }
                paragraph
                    .scroll((saturating_u16(scroll), 0))
                    .render(area, buffer);
            }
        }
    }

    fn append_assistant_delta(&mut self, delta: &str) -> bool {
        if !matches!(self.kind, EntryKind::Assistant) {
            return false;
        }

        let EntryContent::Markdown(markdown) = &mut self.content else {
            return false;
        };
        markdown.append(delta);
        self.cached_height.store(0, Ordering::Relaxed);
        true
    }

    fn append_reasoning_delta(&mut self, delta: &str) -> bool {
        if !matches!(self.kind, EntryKind::Reasoning) {
            return false;
        }

        let EntryContent::Streaming(streaming) = &mut self.content else {
            return false;
        };
        let cached = self.cached_height.load(Ordering::Relaxed);
        let cached_width = (cached != 0).then_some((cached >> 48) as u16);
        let replacement = streaming.append(delta, cached_width);
        self.update_streaming_height(cached, cached_width, replacement);
        true
    }

    fn finalize_assistant(&mut self, message: &str) -> bool {
        if !matches!(self.kind, EntryKind::Assistant) {
            return false;
        }
        match &mut self.content {
            EntryContent::Markdown(markdown) => markdown.finalize(message),
            _ => self.content = EntryContent::Markdown(MarkdownContent::new(message)),
        }
        self.cached_height.store(0, Ordering::Relaxed);
        true
    }

    fn update_streaming_height(
        &self,
        cached: u64,
        cached_width: Option<u16>,
        replacement: Option<(usize, usize)>,
    ) {
        if let Some((old_height, new_height)) = replacement {
            let total = usize::try_from(cached & ((1_u64 << 48) - 1)).unwrap_or(usize::MAX);
            self.cached_height.store(
                encode_height(
                    cached_width.unwrap_or_default(),
                    total.saturating_sub(old_height).saturating_add(new_height),
                ),
                Ordering::Relaxed,
            );
        } else {
            self.cached_height.store(0, Ordering::Relaxed);
        }
    }

    fn push_tool_child(
        &mut self,
        call_id: String,
        name: String,
        arguments: String,
        status: ToolStatus,
    ) {
        let EntryKind::Tool { .. } = self.kind else {
            return;
        };
        let EntryContent::Tool(tool) = &mut self.content else {
            return;
        };
        tool.children
            .push(ToolActivity::new(call_id, name, arguments, status));
        self.cached_height.store(0, Ordering::Relaxed);
    }

    fn set_tool_result(
        &mut self,
        call_id: &str,
        status: ToolStatus,
        duration_ns: Option<u64>,
        result: Option<String>,
    ) {
        let EntryKind::Tool { .. } = self.kind else {
            return;
        };
        let EntryContent::Tool(tool) = &mut self.content else {
            return;
        };
        let target = if tool.call_id == call_id {
            Some(tool)
        } else {
            tool.children
                .iter_mut()
                .find(|child| child.call_id == call_id)
        };
        let Some(target) = target else {
            return;
        };
        target.status = status;
        target.duration_ns = duration_ns;
        target.result = result;
        self.cached_height.store(0, Ordering::Relaxed);
    }
}

impl ToolActivity {
    fn new(call_id: String, name: String, arguments: String, status: ToolStatus) -> Self {
        let highlighted_source = (name == "exec" && !arguments.is_empty())
            .then(|| highlighted_code_lines(Some("javascript"), &arguments));
        let patch = (name == "apply_patch")
            .then(|| present_apply_patch(&arguments))
            .flatten();
        Self {
            call_id,
            name,
            arguments,
            status,
            duration_ns: None,
            result: None,
            children: Vec::new(),
            highlighted_source,
            patch,
        }
    }

    fn text(&self) -> Text<'static> {
        let (icon, color) = tool_style(self.status);
        let display_name = if self.name == "exec" {
            "Code Mode"
        } else {
            self.name.as_str()
        };
        let mut details = Vec::new();
        if !self.children.is_empty() {
            details.push(format!(
                "{} call{}",
                self.children.len(),
                if self.children.len() == 1 { "" } else { "s" }
            ));
        }
        if self.children.is_empty()
            && let Some(patch) = &self.patch
        {
            details.push(patch.summary.clone());
        }
        if let Some(duration_ns) = self.duration_ns {
            details.push(format_duration(duration_ns));
        }
        if self.children.len() >= 2
            && let Some(parent_duration) = self.duration_ns
        {
            let child_duration = self
                .children
                .iter()
                .filter_map(|child| child.duration_ns)
                .fold(0_u64, u64::saturating_add);
            details.push(
                if child_duration > parent_duration.saturating_mul(6) / 5 {
                    "overlapping"
                } else {
                    "sequence"
                }
                .to_owned(),
            );
        }
        if !self.children.is_empty()
            && let Some(result) = self.result.as_deref().filter(|result| !result.is_empty())
        {
            details.push(result.to_owned());
        }

        let mut lines = vec![tool_header_line(icon, color, display_name, &details)];

        if let Some(highlighted_source) = &self.highlighted_source {
            lines.push(Line::styled(
                format!("  ┌─ javascript · {} LOC", code_line_count(&self.arguments)),
                Style::default().fg(Color::DarkGray),
            ));
            for line in highlighted_source {
                let mut spans = vec![Span::styled("  │ ", Style::default().fg(Color::DarkGray))];
                spans.extend(line.spans.clone());
                lines.push(Line::from(spans));
            }
            lines.push(Line::styled("  └─", Style::default().fg(Color::DarkGray)));
            if self.children.is_empty()
                && let Some(result) = self.result.as_deref().filter(|result| !result.is_empty())
            {
                lines.push(Line::from(vec![
                    Span::styled("  └─ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(result.to_owned(), Style::default().fg(Color::Gray)),
                ]));
            }
        } else if self.children.is_empty()
            && let Some(patch) = &self.patch
        {
            lines.extend(prefixed_patch_lines(&patch.lines, "  "));
        } else if self.children.is_empty() {
            let mut activity_detail = self.arguments.clone();
            if let Some(result) = self.result.as_deref().filter(|result| !result.is_empty()) {
                push_detail(&mut activity_detail, result);
            }
            if !activity_detail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  └─ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(activity_detail, Style::default().fg(Color::Gray)),
                ]));
            }
        }
        if !self.children.is_empty() {
            for (index, child) in self.children.iter().enumerate() {
                let last = index + 1 == self.children.len();
                lines.extend(child_lines(child, last));
            }
        }
        lines.push(Line::raw(""));
        Text::from(lines)
    }
}

fn tool_header_line(
    icon: &str,
    color: Color,
    display_name: &str,
    details: &[String],
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{icon} {display_name}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if details.is_empty() {
                String::new()
            } else {
                format!("  {}", details.join(" · "))
            },
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn child_lines(child: &ToolActivity, last: bool) -> Vec<Line<'static>> {
    let (icon, color) = tool_style(child.status);
    let connector = if last { "  └─" } else { "  ├─" };
    let argument_lines = child.arguments.lines().collect::<Vec<_>>();
    let mut detail = if let Some(patch) = &child.patch {
        patch.summary.clone()
    } else if argument_lines.len() <= 1 {
        child.arguments.clone()
    } else {
        String::new()
    };
    if let Some(duration_ns) = child.duration_ns {
        push_detail(&mut detail, &format_duration(duration_ns));
    }
    if let Some(result) = child.result.as_deref().filter(|result| !result.is_empty()) {
        push_detail(&mut detail, result);
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(connector, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {icon} {}", child.name),
            Style::default().fg(color),
        ),
        Span::styled(
            if detail.is_empty() {
                String::new()
            } else {
                format!("  {detail}")
            },
            Style::default().fg(Color::DarkGray),
        ),
    ])];
    if let Some(patch) = &child.patch {
        let continuation = if last { "       " } else { "  │    " };
        lines.extend(prefixed_patch_lines(&patch.lines, continuation));
    } else if argument_lines.len() > 1 {
        let continuation = if last { "     " } else { "  │  " };
        for argument in argument_lines {
            lines.push(Line::from(vec![
                Span::styled(continuation, Style::default().fg(Color::DarkGray)),
                Span::styled(format!("  {argument}"), Style::default().fg(Color::Gray)),
            ]));
        }
    }
    lines
}

fn prefixed_patch_lines(lines: &[Line<'static>], prefix: &'static str) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            let mut spans = vec![Span::styled(prefix, Style::default().fg(Color::DarkGray))];
            spans.extend(line.spans.clone());
            Line::from(spans)
        })
        .collect()
}

fn push_detail(target: &mut String, detail: &str) {
    if !target.is_empty() {
        target.push_str(" · ");
    }
    target.push_str(detail);
}

fn format_duration(duration_ns: u64) -> String {
    if duration_ns < 1_000_000 {
        format!("{}µs", duration_ns / 1_000)
    } else if duration_ns < 1_000_000_000 {
        format!("{}ms", duration_ns / 1_000_000)
    } else {
        let tenths = duration_ns / 100_000_000;
        format!("{}.{:01}s", tenths / 10, tenths % 10)
    }
}

impl MarkdownContent {
    fn new(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            streaming: false,
            cached: Mutex::new(None),
        }
    }

    fn streaming(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            streaming: true,
            cached: Mutex::new(None),
        }
    }

    fn append(&mut self, delta: &str) {
        self.source.push_str(delta);
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn finalize(&mut self, source: &str) {
        source.clone_into(&mut self.source);
        self.streaming = false;
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn text(&self, width: u16) -> Text<'static> {
        self.with_rendered(width, |rendered| rendered.text.clone())
    }

    fn height(&self, width: u16) -> usize {
        self.with_rendered(width, |rendered| rendered.height)
    }

    fn with_rendered<R>(&self, width: u16, read: impl FnOnce(&RenderedMarkdown) -> R) -> R {
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if cached
            .as_ref()
            .is_some_and(|rendered| rendered.width != width)
        {
            *cached = None;
        }
        let rendered = cached.get_or_insert_with(|| {
            let source = if self.streaming {
                heal_streaming_markdown(&self.source)
            } else {
                Cow::Borrowed(self.source.as_str())
            };
            let text = render_agent_markdown(&source, width);
            let height = Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .line_count(width);
            RenderedMarkdown {
                width,
                text,
                height,
            }
        });
        read(rendered)
    }
}

impl StreamingText {
    fn reasoning(message: &str) -> Self {
        let body_style = Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC);
        let mut parts = message.split('\n');
        let mut lines = Vec::with_capacity(message.lines().count().saturating_add(1));
        lines.push(StreamingLine::new(
            format!("• {}", parts.next().unwrap_or_default()),
            body_style,
        ));
        lines.extend(parts.map(|line| StreamingLine::new(format!("  {line}"), body_style)));
        lines.push(StreamingLine::new(String::new(), Style::default()));
        Self {
            lines,
            body_style,
            continuation_prefix: "  ",
        }
    }

    fn height(&self, width: u16) -> usize {
        self.lines.iter().map(|line| line.height(width)).sum()
    }

    fn append(&mut self, delta: &str, cached_width: Option<u16>) -> Option<(usize, usize)> {
        let old_height = cached_width.map(|width| {
            self.lines
                .iter()
                .rev()
                .take(2)
                .map(|line| line.height(width))
                .sum()
        });

        drop(self.lines.pop());
        let mut parts = delta.split('\n');
        if let Some(first) = parts.next()
            && !first.is_empty()
            && let Some(body) = self.lines.last_mut()
        {
            body.content.push_str(first);
            *body
                .cached_layout
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
        let body_style = self.body_style;
        let continuation_prefix = self.continuation_prefix;
        self.lines
            .extend(parts.map(|part| {
                StreamingLine::new(format!("{continuation_prefix}{part}"), body_style)
            }));
        self.lines
            .push(StreamingLine::new(String::new(), Style::default()));

        let width = cached_width?;
        let added_lines = delta.bytes().filter(|byte| *byte == b'\n').count();
        let new_height = self
            .lines
            .iter()
            .rev()
            .take(added_lines.saturating_add(2))
            .map(|line| line.height(width))
            .sum();
        Some((old_height.unwrap_or_default(), new_height))
    }

    fn render(
        &self,
        area: Rect,
        buffer: &mut Buffer,
        scroll: usize,
        total_height: usize,
        selected: bool,
    ) {
        if area.is_empty() {
            return;
        }
        let width = area.width;
        let Some((first, mut line_top)) = self.line_at_visual_row(width, scroll, total_height)
        else {
            return;
        };
        let viewport_bottom = scroll.saturating_add(usize::from(area.height));
        for line in self.lines.iter().skip(first) {
            let line_height = line.height(width);
            let line_bottom = line_top.saturating_add(line_height);
            if line_top >= viewport_bottom {
                break;
            }
            let visible_top = line_top.max(scroll);
            let visible_bottom = line_bottom.min(viewport_bottom);
            if visible_top < visible_bottom {
                let line_area = Rect::new(
                    area.x,
                    area.y.saturating_add(saturating_u16(visible_top - scroll)),
                    area.width,
                    saturating_u16(visible_bottom - visible_top),
                );
                line.render_rows(
                    line_area,
                    buffer,
                    visible_top.saturating_sub(line_top),
                    selected,
                );
            }
            line_top = line_bottom;
        }
    }

    fn line_at_visual_row(
        &self,
        width: u16,
        row: usize,
        total_height: usize,
    ) -> Option<(usize, usize)> {
        if row >= total_height {
            return None;
        }
        if row <= total_height / 2 {
            let mut top = 0_usize;
            for (index, line) in self.lines.iter().enumerate() {
                let bottom = top.saturating_add(line.height(width));
                if row < bottom {
                    return Some((index, top));
                }
                top = bottom;
            }
            return None;
        }

        let mut bottom = total_height;
        for (index, line) in self.lines.iter().enumerate().rev() {
            let top = bottom.saturating_sub(line.height(width));
            if row >= top {
                return Some((index, top));
            }
            bottom = top;
        }
        None
    }

    #[cfg(test)]
    fn materialized_text(&self) -> Text<'static> {
        Text::from(
            self.lines
                .iter()
                .map(|line| Line::styled(line.content.clone(), line.style))
                .collect::<Vec<_>>(),
        )
    }
}

impl StreamingLine {
    fn new(content: String, style: Style) -> Self {
        Self {
            content,
            style,
            cached_layout: Mutex::new(None),
        }
    }

    fn height(&self, width: u16) -> usize {
        self.with_layout(width, |layout| layout.rows.len())
    }

    fn render_rows(&self, area: Rect, buffer: &mut Buffer, first_row: usize, selected: bool) {
        let style = if selected {
            self.style.add_modifier(Modifier::REVERSED)
        } else {
            self.style
        };
        self.with_layout(area.width, |layout| {
            for (screen_row, row) in layout
                .rows
                .iter()
                .skip(first_row)
                .take(usize::from(area.height))
                .enumerate()
            {
                let row_area = Rect::new(
                    area.x,
                    area.y.saturating_add(saturating_u16(screen_row)),
                    area.width,
                    1,
                );
                for column in 0..area.width {
                    buffer[(area.x.saturating_add(column), row_area.y)].reset();
                }
                let mut column = 0_u16;
                for grapheme in UnicodeSegmentation::graphemes(row.as_str(), true) {
                    if column >= area.width {
                        break;
                    }
                    let symbol_width = saturating_u16(UnicodeWidthStr::width(grapheme));
                    if symbol_width == 0 {
                        continue;
                    }
                    buffer[(area.x.saturating_add(column), row_area.y)]
                        .set_symbol(grapheme)
                        .set_style(style);
                    column = column.saturating_add(symbol_width);
                }
            }
        });
    }

    fn with_layout<R>(&self, width: u16, read: impl FnOnce(&StreamingLineLayout) -> R) -> R {
        let mut cached = self
            .cached_layout
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cached.as_ref().is_none_or(|layout| layout.width != width) {
            *cached = None;
        }
        let layout = cached.get_or_insert_with(|| StreamingLineLayout {
            width,
            rows: wrap_line(&self.content, width),
        });
        read(layout)
    }
}

/// Cacheable single-style equivalent of Ratatui 0.29's `WordWrapper` with `trim: false`.
///
/// Ratatui's wrapper is private and `Paragraph::scroll` replays the full logical line before
/// reaching a tail viewport. Keeping the wrapped rows lets streaming frames touch only visible
/// rows; the parity test below protects whitespace, grapheme, and wide-symbol behavior.
fn wrap_line(content: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let max_width = usize::from(width);
    let mut wrapped = Vec::<Vec<&str>>::new();
    let mut pending_line = Vec::<&str>::new();
    let mut pending_word = Vec::<&str>::new();
    let mut pending_whitespace = VecDeque::<&str>::new();
    let mut line_width = 0_usize;
    let mut word_width = 0_usize;
    let mut whitespace_width = 0_usize;
    let mut non_whitespace_previous = false;

    for grapheme in UnicodeSegmentation::graphemes(content, true) {
        let is_whitespace = grapheme == "\u{200b}"
            || (grapheme.chars().all(char::is_whitespace) && grapheme != "\u{00a0}");
        let symbol_width = UnicodeWidthStr::width(grapheme);
        if symbol_width > max_width {
            continue;
        }

        let word_found = non_whitespace_previous && is_whitespace;
        let untrimmed_overflow = pending_line.is_empty()
            && word_width
                .saturating_add(whitespace_width)
                .saturating_add(symbol_width)
                > max_width;
        if word_found || untrimmed_overflow {
            pending_line.extend(pending_whitespace.drain(..));
            line_width = line_width.saturating_add(whitespace_width);
            pending_line.append(&mut pending_word);
            line_width = line_width.saturating_add(word_width);
            whitespace_width = 0;
            word_width = 0;
        }

        let line_full = line_width >= max_width;
        let pending_word_overflow = symbol_width > 0
            && line_width
                .saturating_add(whitespace_width)
                .saturating_add(word_width)
                >= max_width;
        if line_full || pending_word_overflow {
            let mut remaining_width = max_width.saturating_sub(line_width);
            wrapped.push(mem::take(&mut pending_line));
            line_width = 0;

            while let Some(whitespace) = pending_whitespace.front() {
                let whitespace_symbol_width = UnicodeWidthStr::width(*whitespace);
                if whitespace_symbol_width > remaining_width {
                    break;
                }
                whitespace_width = whitespace_width.saturating_sub(whitespace_symbol_width);
                remaining_width = remaining_width.saturating_sub(whitespace_symbol_width);
                let _ = pending_whitespace.pop_front();
            }
            if is_whitespace && pending_whitespace.is_empty() {
                continue;
            }
        }

        if is_whitespace {
            whitespace_width = whitespace_width.saturating_add(symbol_width);
            pending_whitespace.push_back(grapheme);
        } else {
            word_width = word_width.saturating_add(symbol_width);
            pending_word.push(grapheme);
        }
        non_whitespace_previous = !is_whitespace;
    }

    if pending_line.is_empty() && pending_word.is_empty() && !pending_whitespace.is_empty() {
        wrapped.push(Vec::new());
    }
    pending_line.extend(pending_whitespace.drain(..));
    pending_line.append(&mut pending_word);
    if !pending_line.is_empty() {
        wrapped.push(pending_line);
    }
    if wrapped.is_empty() {
        wrapped.push(Vec::new());
    }
    wrapped
        .into_iter()
        .map(|line| line.concat())
        .collect::<Vec<_>>()
}

fn encode_height(width: u16, height: usize) -> u64 {
    (u64::from(width) << 48)
        | u64::try_from(height)
            .unwrap_or((1_u64 << 48) - 1)
            .min((1_u64 << 48) - 1)
}

fn message_text(title: &'static str, title_style: Style, message: &str) -> Text<'static> {
    let mut lines = Vec::with_capacity(message.lines().count().saturating_add(2));
    lines.push(Line::styled(title, title_style));
    for line in message.split('\n') {
        lines.push(Line::styled(
            format!("  {line}"),
            Style::default().fg(Color::White),
        ));
    }
    lines.push(Line::raw(""));
    Text::from(lines)
}

fn tool_style(status: ToolStatus) -> (&'static str, Color) {
    match status {
        ToolStatus::Running => ("◌", Color::Yellow),
        ToolStatus::Completed => ("✓", Color::Green),
        ToolStatus::Cancelled => ("■", Color::Yellow),
        ToolStatus::Failed => ("✗", Color::Red),
    }
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use ratatui::{
        Terminal,
        backend::TestBackend,
        buffer::Buffer,
        layout::Rect,
        style::{Color, Modifier, Style},
        text::Line,
        widgets::{Paragraph, Widget, Wrap},
    };

    use super::{
        EntryContent, InlineEdit, StreamingLine, ToolStatus, Transcript, TranscriptItem,
        saturating_u16, tool_style,
    };

    #[test]
    fn cancelled_tools_have_a_distinct_neutral_terminal_style() {
        assert_eq!(tool_style(ToolStatus::Cancelled), ("■", Color::Yellow));
    }

    #[test]
    fn assistant_deltas_update_only_the_tail_entry() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Assistant("first".to_owned()));
        assert!(transcript.append_assistant_delta(" line\nsecond"));

        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();

        assert_eq!(
            terminal.backend().to_string(),
            concat!(
                "\"● Nanocodex         \"\n",
                "\"  first line        \"\n",
                "\"  second            \"\n",
                "\"                    \"\n",
                "\"                    \"\n",
            )
        );
    }

    #[test]
    fn assistant_markdown_is_healed_and_rendered_while_streaming() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Assistant("Streaming **bold".to_owned()));

        let area = Rect::new(0, 0, 30, 4);
        let mut buffer = Buffer::empty(area);
        transcript
            .widget(0, None, None, "empty")
            .render(area, &mut buffer);
        let rendered = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Streaming bold"));
        assert!(!rendered.contains("**"));
        let bold_cell = buffer.cell((12, 1)).unwrap();
        assert!(bold_cell.modifier.contains(Modifier::BOLD));

        assert!(transcript.append_assistant_delta("**"));
        assert!(transcript.finalize_assistant("Streaming **bold**"));
        let EntryContent::Markdown(markdown) = &transcript.entries[0].content else {
            panic!("assistant entry should retain Markdown source");
        };
        assert_eq!(markdown.source, "Streaming **bold**");
        assert!(!markdown.streaming);
    }

    #[test]
    fn finalized_assistant_renders_markdown_and_reflows_tables_by_width() {
        let source = "## Result\n\n| Name | Status |\n| --- | --- |\n| build | passed |";
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Assistant(source.to_owned()));
        assert!(transcript.finalize_assistant(source));

        let mut wide = Terminal::new(TestBackend::new(48, 8)).unwrap();
        wide.draw(|frame| {
            frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
        })
        .unwrap();
        assert!(wide.backend().to_string().contains("Name  │ Status"));

        let mut narrow = Terminal::new(TestBackend::new(14, 12)).unwrap();
        narrow
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        assert!(narrow.backend().to_string().contains("┌─ row 1"));
    }

    #[test]
    fn code_mode_children_form_one_timed_activity_tree() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "call-1".to_owned(),
            name: "exec".to_owned(),
            arguments: "const tasks = [\"test\", \"patch\"];\nawait Promise.all(tasks);".to_owned(),
            status: ToolStatus::Running,
        });
        assert!(transcript.push_tool_child(
            "call-1/code-1".to_owned(),
            "exec_command".to_owned(),
            "cargo test \\\n  --workspace".to_owned(),
            ToolStatus::Running,
        ));
        assert!(transcript.set_tool_result(
            "call-1/code-1",
            ToolStatus::Completed,
            Some(90_000_000),
            Some("exit 0".to_owned()),
        ));
        assert!(transcript.push_tool_child(
            "call-1/code-2".to_owned(),
            "apply_patch".to_owned(),
            "src/main.rs".to_owned(),
            ToolStatus::Running,
        ));
        assert!(transcript.set_tool_result(
            "call-1/code-2",
            ToolStatus::Completed,
            Some(80_000_000),
            Some("applied".to_owned()),
        ));
        assert!(transcript.set_tool_result(
            "call-1",
            ToolStatus::Completed,
            Some(120_000_000),
            None,
        ));

        assert_eq!(transcript.len(), 1);
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("✓ Code Mode  2 calls · 120ms · overlapping"));
        assert!(rendered.contains("┌─ javascript · 2 LOC"));
        assert!(rendered.contains("const tasks = [\"test\", \"patch\"]"));
        assert!(rendered.contains("├─ ✓ exec_command  90ms · exit 0"));
        assert!(rendered.contains("cargo test \\"));
        assert!(rendered.contains("--workspace"));
        assert!(rendered.contains("└─ ✓ apply_patch  src/main.rs · 80ms · applied"));
    }

    #[test]
    fn apply_patch_activity_renders_paths_counts_and_hunks() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "patch-1".to_owned(),
            name: "apply_patch".to_owned(),
            arguments:
                "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old();\n+new();\n*** End Patch"
                    .to_owned(),
            status: ToolStatus::Completed,
        });

        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Edited src/main.rs (+1 -1)"));
        assert!(rendered.contains("- old();"));
        assert!(rendered.contains("+ new();"));
    }

    #[test]
    fn reasoning_deltas_stream_as_a_dim_inline_block() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Reasoning("Inspecting".to_owned()));
        assert!(transcript.append_reasoning_delta(" the request\nand tools"));

        let area = Rect::new(0, 0, 30, 4);
        let mut buffer = Buffer::empty(area);
        transcript
            .widget(0, None, None, "empty")
            .render(area, &mut buffer);

        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "•");
        assert_eq!(buffer.cell((0, 1)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((2, 1)).unwrap().symbol(), "a");
        assert!(
            buffer
                .cell((0, 0))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
        assert!(
            buffer
                .cell((2, 0))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::ITALIC)
        );
    }

    #[test]
    fn cached_assistant_line_wrapping_matches_ratatui() {
        let cases = [
            "",
            "a long unbroken_identifier_that_wraps",
            "words   with mixed\twhitespace",
            "界🦀 unicode graphemes e\u{301}",
            "zero\u{200b}width and non\u{00a0}breaking",
        ];
        for content in cases {
            for width in [1, 3, 8, 20] {
                let line =
                    StreamingLine::new(content.to_owned(), Style::default().fg(Color::White));
                let height = saturating_u16(line.height(width));
                let area = Rect::new(0, 0, width, height);
                let mut actual = Buffer::empty(area);
                line.render_rows(area, &mut actual, 0, false);
                let mut expected = Buffer::empty(area);
                Paragraph::new(Line::styled(content, Style::default().fg(Color::White)))
                    .wrap(Wrap { trim: false })
                    .render(area, &mut expected);
                assert_eq!(
                    actual, expected,
                    "wrapping differs for {content:?} at {width}"
                );
            }
        }
    }

    #[test]
    fn viewport_skips_entries_above_the_visible_window() {
        let mut transcript = Transcript::default();
        for index in 0..100 {
            transcript.push(TranscriptItem::User(format!("message {index}")));
        }
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 20, 4));

        transcript
            .widget(0, None, None, "empty")
            .render(buffer.area, &mut buffer);

        let rendered = buffer
            .content
            .chunks(20)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered[0].trim(), "");
        assert_eq!(rendered[1].trim(), "› You");
        assert_eq!(rendered[2].trim(), "message 99");
        assert_eq!(rendered[3].trim(), "");
    }

    #[test]
    fn selected_user_is_highlighted_and_rendered_directly_by_entry() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::User("selected prompt".to_owned()));
        transcript.push(TranscriptItem::Assistant("following answer".to_owned()));
        let area = Rect::new(0, 0, 20, 6);
        let mut buffer = Buffer::empty(area);

        transcript
            .widget(usize::MAX, Some(0), None, "empty")
            .render(area, &mut buffer);

        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "›");
        assert!(
            buffer
                .cell((0, 0))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            !buffer
                .cell((0, 3))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }

    #[test]
    fn selected_user_stays_inline_with_surrounding_transcript_context() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Assistant("older answer".to_owned()));
        transcript.push(TranscriptItem::User("selected prompt".to_owned()));
        transcript.push(TranscriptItem::Assistant("following answer".to_owned()));
        let area = Rect::new(0, 0, 40, 10);
        let mut buffer = Buffer::empty(area);

        transcript
            .widget(0, Some(1), None, "empty")
            .render(area, &mut buffer);

        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "●");
        assert_eq!(buffer.cell((0, 3)).unwrap().symbol(), "›");
        assert!(
            buffer
                .cell((0, 3))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert_eq!(buffer.cell((0, 6)).unwrap().symbol(), "●");
    }

    #[test]
    fn inline_editor_replaces_only_the_selected_message_row() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Assistant("older answer".to_owned()));
        transcript.push(TranscriptItem::User("original prompt".to_owned()));
        transcript.push(TranscriptItem::Assistant("following answer".to_owned()));
        let area = Rect::new(0, 0, 40, 10);
        let edit = InlineEdit {
            index: 1,
            input: "revised prompt",
            cursor: "revised prompt".len(),
        };
        let mut buffer = Buffer::empty(area);

        transcript
            .widget(0, Some(1), Some(edit), "empty")
            .render(area, &mut buffer);

        let rendered = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("older answer"));
        assert!(rendered.contains("Edit message · Esc cancel"));
        assert!(rendered.contains("revised prompt"));
        assert!(!rendered.contains("original prompt"));
        assert!(rendered.contains("following answer"));
        assert!(
            transcript
                .inline_edit_cursor(area, 0, Some(1), edit)
                .is_some()
        );
    }

    #[test]
    fn viewport_scrolls_from_the_bottom_without_walking_from_the_oldest_entry() {
        let mut transcript = Transcript::default();
        for index in 0..100 {
            transcript.push(TranscriptItem::User(format!("message {index}")));
        }
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 20, 4));

        transcript
            .widget(3, None, None, "empty")
            .render(buffer.area, &mut buffer);

        let rendered = buffer
            .content
            .chunks(20)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered[0].trim(), "");
        assert_eq!(rendered[1].trim(), "› You");
        assert_eq!(rendered[2].trim(), "message 98");
        assert_eq!(rendered[3].trim(), "");
    }

    #[test]
    fn viewport_clamps_an_oversized_bottom_offset_to_the_oldest_entry() {
        let mut transcript = Transcript::default();
        for index in 0..100 {
            transcript.push(TranscriptItem::User(format!("message {index}")));
        }
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 20, 4));

        transcript
            .widget(usize::MAX, None, None, "empty")
            .render(buffer.area, &mut buffer);

        let rendered = buffer
            .content
            .chunks(20)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered[0].trim(), "› You");
        assert_eq!(rendered[1].trim(), "message 0");
        assert_eq!(rendered[2].trim(), "");
        assert_eq!(rendered[3].trim(), "› You");
    }

    #[test]
    fn bottom_up_viewport_matches_the_complete_height_reference() {
        let mut transcript = Transcript::default();
        for index in 0..30 {
            transcript.push(TranscriptItem::User(format!(
                "message {index} with wrapping words"
            )));
            transcript.push(TranscriptItem::Assistant(format!(
                "answer {index}\nwith a second line"
            )));
            transcript.push(TranscriptItem::Tool {
                call_id: format!("call-{index}"),
                name: "exec_command".to_owned(),
                arguments: format!("argument-{index}"),
                status: ToolStatus::Completed,
            });
        }

        for width in [5, 20, 40] {
            for height in [1, 4, 10] {
                let area = Rect::new(0, 0, width, height);
                for scroll_from_bottom in [0, 1, 3, 15, 250, usize::MAX] {
                    let mut actual = Buffer::empty(area);
                    transcript
                        .widget(scroll_from_bottom, None, None, "empty")
                        .render(area, &mut actual);
                    let expected = reference_buffer(&transcript, scroll_from_bottom, area);
                    assert_eq!(
                        actual, expected,
                        "viewport differs at {width}x{height} with offset {scroll_from_bottom}"
                    );
                }
            }
        }
    }

    fn reference_buffer(transcript: &Transcript, scroll_from_bottom: usize, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        let viewport_height = usize::from(area.height);
        let content_height = transcript
            .entries
            .iter()
            .map(|entry| entry.height(area.width))
            .sum::<usize>();
        let max_scroll = content_height.saturating_sub(viewport_height);
        let scroll = max_scroll.saturating_sub(scroll_from_bottom.min(max_scroll));
        let viewport_end = scroll.saturating_add(viewport_height);
        let mut entry_top = 0_usize;

        for entry in &transcript.entries {
            let entry_height = entry.height(area.width);
            let entry_bottom = entry_top.saturating_add(entry_height);
            if entry_bottom <= scroll {
                entry_top = entry_bottom;
                continue;
            }
            if entry_top >= viewport_end {
                break;
            }

            let visible_top = entry_top.max(scroll);
            let visible_bottom = entry_bottom.min(viewport_end);
            let entry_area = Rect::new(
                area.x,
                area.y
                    .saturating_add(saturating_u16(visible_top.saturating_sub(scroll))),
                area.width,
                saturating_u16(visible_bottom.saturating_sub(visible_top)),
            );
            entry
                .paragraph()
                .scroll((saturating_u16(visible_top.saturating_sub(entry_top)), 0))
                .render(entry_area, &mut buffer);
            entry_top = entry_bottom;
        }
        buffer
    }
}
