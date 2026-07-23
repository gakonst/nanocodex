use std::{
    borrow::Cow,
    collections::VecDeque,
    mem,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::app::PlanStepStatus;
use super::composer::ComposerLayout;
use super::diff::{PatchPresentation, present_apply_patch};
use super::markdown::{
    LogicalMarkdown, heal_streaming_markdown, render_agent_markdown,
    restore_markdown_links_from_sources,
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
    Plan {
        explanation: Option<String>,
        steps: Vec<(String, PlanStepStatus)>,
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

pub(super) struct Transcript {
    entries: Vec<Arc<TranscriptEntry>>,
    editable_users: Vec<usize>,
    cached_total_height: AtomicU64,
    tool_details_expanded: Arc<AtomicBool>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            editable_users: Vec::new(),
            cached_total_height: AtomicU64::new(0),
            tool_details_expanded: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Clone for Transcript {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            editable_users: self.editable_users.clone(),
            cached_total_height: AtomicU64::new(self.cached_total_height.load(Ordering::Relaxed)),
            tool_details_expanded: Arc::clone(&self.tool_details_expanded),
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
        if matches!(&item, TranscriptItem::Reasoning(_))
            && let Some(entry) = self.entries.last_mut()
        {
            Arc::make_mut(entry).remove_trailing_user_separator();
        }
        self.entries.push(Arc::new(TranscriptEntry::new(
            item,
            Arc::clone(&self.tool_details_expanded),
        )));
        self.invalidate_total_height();
    }

    pub(super) fn set_tool_details_expanded(&mut self, expanded: bool) {
        if self.tool_details_expanded.swap(expanded, Ordering::Relaxed) == expanded {
            return;
        }
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
        let mut entry = TranscriptEntry::new(
            TranscriptItem::User(message),
            Arc::clone(&self.tool_details_expanded),
        );
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

    pub(super) fn semanticize_copy(&self, selected: String) -> String {
        let mut local_matches = self
            .entries
            .iter()
            .filter_map(|entry| {
                let EntryContent::Markdown(markdown) = &entry.content else {
                    return None;
                };
                markdown.semanticize_copy(&selected)
            })
            .take(2);
        if let Some(copied) = local_matches.next()
            && local_matches.next().is_none()
        {
            return copied;
        }
        restore_markdown_links_from_sources(
            selected,
            self.entries.iter().filter_map(|entry| {
                let EntryContent::Markdown(markdown) = &entry.content else {
                    return None;
                };
                Some(markdown.source.as_str())
            }),
        )
    }

    pub(super) fn prefix_before(&self, index: usize) -> Self {
        let end = index.min(self.entries.len());
        Self {
            entries: self.entries[..end].to_vec(),
            editable_users: self.editable_users
                [..self.editable_users.partition_point(|i| *i < end)]
                .to_vec(),
            cached_total_height: AtomicU64::new(0),
            tool_details_expanded: Arc::clone(&self.tool_details_expanded),
        }
    }

    pub(super) fn set_tool_result(
        &mut self,
        call_id: &str,
        status: ToolStatus,
        duration_ns: Option<u64>,
        result: Option<String>,
    ) -> bool {
        self.set_tool_result_timing(call_id, status, None, duration_ns, result)
    }

    pub(super) fn set_tool_result_timing(
        &mut self,
        call_id: &str,
        status: ToolStatus,
        started_after_ns: Option<u64>,
        duration_ns: Option<u64>,
        result: Option<String>,
    ) -> bool {
        let parent_id = call_id
            .split_once("/code-")
            .map_or(call_id, |(parent, _)| parent);
        if let Some(entry) = self.entries.iter_mut().rev().find(
            |entry| matches!(&entry.kind, EntryKind::Tool { call_id } if call_id == parent_id),
        ) {
            Arc::make_mut(entry).set_tool_result(
                call_id,
                status,
                started_after_ns,
                duration_ns,
                result,
            );
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
    Plan,
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
    Markdown(MarkdownContent),
    Tool(ToolActivity),
}

struct ToolActivity {
    call_id: String,
    name: String,
    arguments: String,
    status: ToolStatus,
    started_after_ns: Option<u64>,
    duration_ns: Option<u64>,
    result: Option<String>,
    children: Vec<ToolActivity>,
    patch: Option<PatchPresentation>,
    plain_detail: Option<StreamingText>,
    cached_layout: Mutex<Option<Box<CachedToolLayout>>>,
    details_expanded: Arc<AtomicBool>,
}

struct MarkdownContent {
    source: String,
    streaming: bool,
    plain_streaming: bool,
    base_style: Style,
    show_header: bool,
    cached: Mutex<Option<RenderedText>>,
    logical_copy: Mutex<Option<LogicalMarkdown>>,
}

#[derive(Clone)]
struct RenderedText {
    width: u16,
    text: Text<'static>,
    line_heights: Vec<usize>,
    prewrapped_lines: Vec<Option<Vec<Line<'static>>>>,
    height: usize,
}

#[derive(Clone)]
struct CachedToolLayout {
    expanded: bool,
    rendered: RenderedText,
}

struct StreamingText {
    lines: Vec<StreamingLine>,
    first_line_prefix_width: u16,
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
            first_line_prefix_width: self.first_line_prefix_width,
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

impl Clone for ToolActivity {
    fn clone(&self) -> Self {
        Self {
            call_id: self.call_id.clone(),
            name: self.name.clone(),
            arguments: self.arguments.clone(),
            status: self.status,
            started_after_ns: self.started_after_ns,
            duration_ns: self.duration_ns,
            result: self.result.clone(),
            children: self.children.clone(),
            patch: self.patch.clone(),
            plain_detail: self.plain_detail.clone(),
            cached_layout: Mutex::new(
                self.cached_layout
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            details_expanded: Arc::clone(&self.details_expanded),
        }
    }
}

impl Clone for MarkdownContent {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            streaming: self.streaming,
            plain_streaming: self.plain_streaming,
            base_style: self.base_style,
            show_header: self.show_header,
            cached: Mutex::new(
                self.cached
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            logical_copy: Mutex::new(
                self.logical_copy
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
    fn new(item: TranscriptItem, tool_details_expanded: Arc<AtomicBool>) -> Self {
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
                EntryContent::Markdown(MarkdownContent::reasoning(&message)),
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
                EntryContent::Tool(ToolActivity::new(
                    call_id,
                    name,
                    arguments,
                    status,
                    tool_details_expanded,
                )),
            ),
            TranscriptItem::Plan { explanation, steps } => (
                EntryKind::Plan,
                None,
                EntryContent::Static(plan_text(explanation.as_deref(), &steps)),
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
            EntryContent::Markdown(markdown) => markdown.materialized_text(80),
            EntryContent::Tool(tool) => tool.text(80),
        })
        .wrap(Wrap { trim: false })
    }

    fn user_message(&self) -> Option<&str> {
        self.user_message.as_deref()
    }

    fn remove_trailing_user_separator(&mut self) {
        if !matches!(self.kind, EntryKind::User) {
            return;
        }
        let EntryContent::Static(text) = &mut self.content else {
            return;
        };
        if text
            .lines
            .last()
            .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
        {
            let _ = text.lines.pop();
            self.cached_height.store(0, Ordering::Relaxed);
        }
    }

    fn height(&self, width: u16) -> usize {
        const HEIGHT_MASK: u64 = (1_u64 << 47) - 1;
        const TOOL_EXPANDED: u64 = 1_u64 << 47;

        let cached = self.cached_height.load(Ordering::Relaxed);
        let tool_expanded = match &self.content {
            EntryContent::Tool(tool) => Some(tool.details_expanded.load(Ordering::Relaxed)),
            _ => None,
        };
        let cache_entry_height = !matches!(self.content, EntryContent::Markdown(_));
        let fold_state_matches =
            tool_expanded.is_none_or(|expanded| (cached & TOOL_EXPANDED != 0) == expanded);
        if cache_entry_height
            && cached != 0
            && cached >> 48 == u64::from(width)
            && fold_state_matches
        {
            return usize::try_from(cached & HEIGHT_MASK).unwrap_or(usize::MAX);
        }
        let height = match &self.content {
            EntryContent::Static(text) => Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .line_count(width),
            EntryContent::Markdown(markdown) => markdown.height(width),
            EntryContent::Tool(tool) => tool.height(width),
        };
        let encoded = (u64::from(width) << 48)
            | tool_expanded.map_or(0, |expanded| u64::from(expanded) * TOOL_EXPANDED)
            | u64::try_from(height)
                .unwrap_or(HEIGHT_MASK)
                .min(HEIGHT_MASK);
        if cache_entry_height {
            self.cached_height.store(encoded, Ordering::Relaxed);
        }
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
                    paragraph = paragraph.style(if matches!(self.kind, EntryKind::User) {
                        Style::default().bg(Color::Indexed(8))
                    } else {
                        Style::default().add_modifier(Modifier::REVERSED)
                    });
                }
                paragraph
                    .scroll((saturating_u16(scroll), 0))
                    .render(area, buffer);
                if selected && matches!(self.kind, EntryKind::User) {
                    render_selected_user_affordance(area, buffer, scroll);
                }
            }
            EntryContent::Markdown(markdown) => {
                markdown.render(area, buffer, scroll, total_height, selected);
            }
            EntryContent::Tool(tool) => {
                tool.render(area, buffer, scroll, total_height, selected);
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

        let EntryContent::Markdown(markdown) = &mut self.content else {
            return false;
        };
        markdown.append_reasoning(delta);
        self.cached_height.store(0, Ordering::Relaxed);
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
        tool.plain_detail = None;
        *tool
            .cached_layout
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        tool.children.push(ToolActivity::new(
            call_id,
            name,
            arguments,
            status,
            Arc::clone(&tool.details_expanded),
        ));
        self.cached_height.store(0, Ordering::Relaxed);
    }

    fn set_tool_result(
        &mut self,
        call_id: &str,
        status: ToolStatus,
        started_after_ns: Option<u64>,
        duration_ns: Option<u64>,
        result: Option<String>,
    ) {
        let EntryKind::Tool { .. } = self.kind else {
            return;
        };
        let EntryContent::Tool(tool) = &mut self.content else {
            return;
        };
        *tool
            .cached_layout
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
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
        target.started_after_ns = started_after_ns;
        target.duration_ns = duration_ns;
        target.result = result;
        target.refresh_plain_detail();
        *target
            .cached_layout
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.cached_height.store(0, Ordering::Relaxed);
    }
}

impl ToolActivity {
    fn new(
        call_id: String,
        name: String,
        arguments: String,
        status: ToolStatus,
        details_expanded: Arc<AtomicBool>,
    ) -> Self {
        let patch = (name == "apply_patch")
            .then(|| present_apply_patch(&arguments))
            .flatten();
        let plain_detail = (name != "exec" && patch.is_none() && !arguments.is_empty())
            .then(|| StreamingText::tool_detail(&arguments, None));
        Self {
            call_id,
            name,
            arguments,
            status,
            started_after_ns: None,
            duration_ns: None,
            result: None,
            children: Vec::new(),
            patch,
            plain_detail,
            cached_layout: Mutex::new(None),
            details_expanded,
        }
    }

    fn refresh_plain_detail(&mut self) {
        self.plain_detail = (self.children.is_empty()
            && self.name != "exec"
            && self.patch.is_none()
            && (!self.arguments.is_empty()
                || self
                    .result
                    .as_deref()
                    .is_some_and(|result| !result.is_empty())))
        .then(|| StreamingText::tool_detail(&self.arguments, self.result.as_deref()));
    }

    fn uses_plain_detail(&self) -> bool {
        self.details_expanded.load(Ordering::Relaxed) && self.plain_detail.is_some()
    }

    fn height(&self, width: u16) -> usize {
        if self.uses_plain_detail()
            && let Some(detail) = &self.plain_detail
        {
            let header_height = Paragraph::new(self.plain_header_line())
                .wrap(Wrap { trim: false })
                .line_count(width)
                .max(1);
            return header_height.saturating_add(detail.height(width));
        }
        self.with_rendered(width, |rendered| rendered.height)
    }

    fn render(
        &self,
        area: Rect,
        buffer: &mut Buffer,
        scroll: usize,
        total_height: usize,
        selected: bool,
    ) {
        if !self.uses_plain_detail() {
            self.with_rendered(area.width, |rendered| {
                rendered.render(area, buffer, scroll, total_height, selected);
            });
            return;
        }
        let Some(detail) = &self.plain_detail else {
            return;
        };
        let header = self.plain_header_line();
        let header_height = Paragraph::new(header.clone())
            .wrap(Wrap { trim: false })
            .line_count(area.width)
            .max(1);
        let mut detail_y = 0_u16;
        if scroll < header_height && !area.is_empty() {
            let visible_header = header_height
                .saturating_sub(scroll)
                .min(usize::from(area.height));
            let mut header = Paragraph::new(header).wrap(Wrap { trim: false });
            if selected {
                header = header.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            header.scroll((saturating_u16(scroll), 0)).render(
                Rect::new(area.x, area.y, area.width, saturating_u16(visible_header)),
                buffer,
            );
            detail_y = saturating_u16(visible_header);
        }
        if detail_y >= area.height {
            return;
        }
        let detail_scroll = scroll.saturating_sub(header_height);
        detail.render(
            Rect::new(
                area.x,
                area.y.saturating_add(detail_y),
                area.width,
                area.height.saturating_sub(detail_y),
            ),
            buffer,
            detail_scroll,
            total_height.saturating_sub(header_height),
            selected,
        );
    }

    fn with_rendered<R>(&self, width: u16, read: impl FnOnce(&RenderedText) -> R) -> R {
        let expanded = self.details_expanded.load(Ordering::Relaxed);
        let mut cached = self
            .cached_layout
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if cached
            .as_ref()
            .is_some_and(|layout| layout.expanded != expanded || layout.rendered.width != width)
        {
            *cached = None;
        }
        let layout = cached.get_or_insert_with(|| {
            Box::new(CachedToolLayout {
                expanded,
                rendered: RenderedText::new(self.text(width), width),
            })
        });
        read(&layout.rendered)
    }

    fn plain_header_line(&self) -> Line<'static> {
        let (icon, color) = tool_style(self.status);
        let display_name = if self.name == "exec" {
            if self.children.is_empty() {
                "Working"
            } else {
                "Tools"
            }
        } else {
            self.name.as_str()
        };
        tool_header_line(icon, color, display_name, &self.summary_details())
    }

    fn summary_details(&self) -> Vec<String> {
        let mut details = Vec::new();
        if let Some(duration_ns) = self.duration_ns {
            details.push(format_duration(duration_ns));
        }
        details
    }

    fn text(&self, width: u16) -> Text<'static> {
        let details_expanded = self.details_expanded.load(Ordering::Relaxed);
        if details_expanded
            && self.children.is_empty()
            && let Some(patch) = &self.patch
        {
            let mut lines = patch.lines(width);
            lines.push(Line::raw(""));
            return Text::from(lines);
        }
        let (icon, color) = tool_style(self.status);
        let display_name = if self.name == "exec" {
            if self.children.is_empty() {
                "Working"
            } else {
                "Tools"
            }
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
        if let Some(duration_ns) = self.duration_ns {
            details.push(format_duration(duration_ns));
        }
        if !self.children.is_empty()
            && let Some(result) = self.result.as_deref().filter(|result| !result.is_empty())
        {
            details.push(result.to_owned());
        }

        if !details_expanded {
            return self.collapsed_text(icon, color, display_name, details);
        }

        let mut lines = vec![tool_header_line(icon, color, display_name, &details)];

        if self.children.is_empty() && self.name != "exec" {
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
            let groups = child_activity_groups(&self.children);
            for (group_index, group) in groups.iter().enumerate() {
                let group_is_last = group_index + 1 == groups.len();
                let group_len = group.end - group.start;
                for (child_index, child) in self.children[group.clone()].iter().enumerate() {
                    let child_is_last = child_index + 1 == group_len;
                    let (connector, continuation) = activity_connector(
                        group_is_last,
                        group_len > 1,
                        child_index,
                        child_is_last,
                    );
                    lines.extend(child_lines(child, connector, continuation, width));
                }
            }
        }
        lines.push(Line::raw(""));
        Text::from(lines)
    }

    fn collapsed_text(
        &self,
        icon: &str,
        color: Color,
        display_name: &str,
        mut details: Vec<String>,
    ) -> Text<'static> {
        if self.children.is_empty() && self.name != "exec" {
            let preview = self.patch.as_ref().map_or_else(
                || compact_activity_preview(&self.arguments),
                |patch| patch.summary.clone(),
            );
            if !preview.is_empty() {
                details.push(preview);
            }
        }
        Text::from(vec![
            tool_header_line(icon, color, display_name, &details),
            Line::raw(""),
        ])
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

fn child_lines(
    child: &ToolActivity,
    connector: &'static str,
    continuation: &'static str,
    width: u16,
) -> Vec<Line<'static>> {
    let (icon, color) = tool_style(child.status);
    let argument_lines = child.arguments.lines().collect::<Vec<_>>();
    let detail_style = Style::default().fg(Color::DarkGray);
    let mut detail = Vec::new();
    if let Some(patch) = &child.patch {
        push_styled_detail(&mut detail, patch.summary.clone(), detail_style);
    } else if argument_lines.len() <= 1 {
        push_styled_detail(&mut detail, child.arguments.clone(), detail_style);
    }
    if let Some(duration_ns) = child.duration_ns {
        push_styled_detail(&mut detail, format_duration(duration_ns), detail_style);
    }
    if let Some(result) = child.result.as_deref().filter(|result| !result.is_empty()) {
        push_styled_detail(&mut detail, result.to_owned(), detail_style);
    }
    let child_name = match (child.name.as_str(), child.status) {
        ("exec_command", ToolStatus::Running) => "Running",
        ("exec_command", _) => "Ran",
        _ => child.name.as_str(),
    };
    let mut header = vec![
        Span::styled(connector, Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" {icon} {child_name}"), Style::default().fg(color)),
    ];
    if !detail.is_empty() {
        header.push(Span::styled("  ", detail_style));
        header.extend(detail);
    }
    let mut lines = vec![Line::from(header)];
    if let Some(patch) = &child.patch {
        let prefix_width = u16::try_from(continuation.len()).unwrap_or(u16::MAX);
        let patch_lines = patch.lines(width.saturating_sub(prefix_width));
        lines.extend(prefixed_patch_lines(&patch_lines[1..], continuation));
    } else if argument_lines.len() > 1 {
        for argument in argument_lines {
            lines.push(Line::from(vec![
                Span::styled(continuation, Style::default().fg(Color::DarkGray)),
                Span::styled(format!("  {argument}"), Style::default().fg(Color::Gray)),
            ]));
        }
    }
    lines
}

fn child_activity_groups(children: &[ToolActivity]) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut group_start = 0;
    let mut group_end_ns = None::<u64>;
    for (index, child) in children.iter().enumerate() {
        let interval = child
            .started_after_ns
            .zip(child.duration_ns)
            .map(|(start, duration)| (start, start.saturating_add(duration)));
        let overlaps = interval
            .zip(group_end_ns)
            .is_some_and(|((start, _), end)| start < end);
        if index > group_start && !overlaps {
            groups.push(group_start..index);
            group_start = index;
            group_end_ns = None;
        }
        if let Some((_, end)) = interval {
            group_end_ns = Some(group_end_ns.map_or(end, |current| current.max(end)));
        }
    }
    if group_start < children.len() {
        groups.push(group_start..children.len());
    }
    groups
}

fn activity_connector(
    group_is_last: bool,
    parallel: bool,
    child_index: usize,
    child_is_last: bool,
) -> (&'static str, &'static str) {
    if !parallel {
        return if group_is_last {
            ("  └──", "      ")
        } else {
            ("  ├──", "  │   ")
        };
    }
    match (child_index, child_is_last, group_is_last) {
        (0, _, true) => ("  └─┬", "    │ "),
        (0, _, false) => ("  ├─┬", "  │ │ "),
        (_, false, true) => ("    ├", "    │ "),
        (_, true, true) => ("    └", "      "),
        (_, false, false) => ("  │ ├", "  │ │ "),
        (_, true, false) => ("  │ └", "  │   "),
    }
}

fn push_styled_detail(target: &mut Vec<Span<'static>>, detail: String, style: Style) {
    if detail.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(Span::styled(" · ", style));
    }
    target.push(Span::styled(detail, style));
}

fn plan_text(explanation: Option<&str>, steps: &[(String, PlanStepStatus)]) -> Text<'static> {
    let explanation = explanation.map(str::trim).filter(|text| !text.is_empty());
    let mut lines = vec![Line::from(vec![
        Span::styled("● ", Style::default().fg(Color::Green)),
        Span::styled(
            "Plan",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    if let Some(explanation) = explanation {
        lines.push(Line::styled(
            format!("  {explanation}"),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    for (step, status) in steps {
        let (marker, style) = match status {
            PlanStepStatus::Completed => ("✓ ", Style::default().fg(Color::DarkGray)),
            PlanStepStatus::InProgress => (
                "→ ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            PlanStepStatus::Pending => ("· ", Style::default().fg(Color::DarkGray)),
        };
        lines.push(Line::styled(format!("  {marker}{step}"), style));
    }
    lines.push(Line::raw(""));
    Text::from(lines)
}

fn prefixed_patch_lines(lines: &[Line<'static>], prefix: &'static str) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            let mut spans = vec![Span::styled(prefix, Style::default().fg(Color::DarkGray))];
            spans.extend(line.spans.clone());
            Line::from(spans).style(line.style)
        })
        .collect()
}

fn push_detail(target: &mut String, detail: &str) {
    if !target.is_empty() {
        target.push_str(" · ");
    }
    target.push_str(detail);
}

fn compact_activity_preview(detail: &str) -> String {
    const MAX_CHARS: usize = 96;
    let mut preview = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.chars().count() > MAX_CHARS {
        preview = preview.chars().take(MAX_CHARS - 1).collect();
        preview.push('…');
    }
    preview
}

fn is_plain_streaming_markdown(source: &str) -> bool {
    !source.bytes().any(|byte| {
        matches!(
            byte,
            b'\r'
                | b'\\'
                | b'`'
                | b'*'
                | b'_'
                | b'['
                | b']'
                | b'<'
                | b'>'
                | b'#'
                | b'&'
                | b'!'
                | b'|'
                | b'~'
                | b'='
                | b'{'
                | b'}'
        )
    }) && source.lines().all(plain_markdown_line)
}

fn plain_markdown_extension_is_safe(source: &str, delta: &str) -> bool {
    if !is_plain_streaming_markdown(delta) {
        return false;
    }
    if delta.contains("\n\n") {
        let one_boundary = source.ends_with('\n')
            && !source.ends_with("\n\n")
            && delta.starts_with('\n')
            && !delta[1..].contains("\n\n");
        if !one_boundary {
            return false;
        }
    }
    if source.ends_with("\n\n") && delta.starts_with('\n') {
        return false;
    }
    let previous = source.rsplit('\n').next().unwrap_or_default();
    let next = delta.split('\n').next().unwrap_or_default();
    let trimmed = previous.trim_start_matches(' ');
    let next_bytes = next.as_bytes();
    if trimmed.bytes().all(|byte| byte.is_ascii_digit())
        && !trimmed.is_empty()
        && (next_bytes.starts_with(b". ") || next_bytes.starts_with(b") "))
    {
        return false;
    }
    if matches!(trimmed, "-" | "+") && next.starts_with(' ') {
        return false;
    }
    if trimmed.bytes().all(|byte| byte == b'-')
        && trimmed
            .len()
            .saturating_add(next.bytes().take_while(|byte| *byte == b'-').count())
            >= 3
    {
        return false;
    }
    if previous.bytes().all(|byte| byte == b' ')
        && previous
            .len()
            .saturating_add(next.bytes().take_while(|byte| *byte == b' ').count())
            >= 4
    {
        return false;
    }
    true
}

fn plain_markdown_line(line: &str) -> bool {
    if !line.is_empty() && line.trim().is_empty() {
        return false;
    }
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent >= 4 {
        return false;
    }
    let line = &line[indent..];
    if line.starts_with("- ") || line.starts_with("+ ") || line.starts_with("---") {
        return false;
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits == 0 || !line[digits..].starts_with(". ") && !line[digits..].starts_with(") ")
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
            plain_streaming: false,
            base_style: Style::default(),
            show_header: true,
            cached: Mutex::new(None),
            logical_copy: Mutex::new(None),
        }
    }

    fn streaming(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            streaming: true,
            plain_streaming: is_plain_streaming_markdown(source),
            base_style: Style::default(),
            show_header: true,
            cached: Mutex::new(None),
            logical_copy: Mutex::new(None),
        }
    }

    fn reasoning(source: &str) -> Self {
        Self {
            source: format!("• {}", indent_reasoning(source)),
            streaming: true,
            plain_streaming: false,
            base_style: Style::default().add_modifier(Modifier::DIM),
            show_header: false,
            cached: Mutex::new(None),
            logical_copy: Mutex::new(None),
        }
    }

    fn append(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let incremental = self.plain_streaming
            && plain_markdown_extension_is_safe(&self.source, delta)
            && self.show_header;
        let starts_new_paragraph = self.source.ends_with("\n\n")
            || (self.source.ends_with('\n') && delta.starts_with('\n'));
        let paragraph_delta = if self.source.ends_with('\n') && delta.starts_with('\n') {
            &delta[1..]
        } else {
            delta
        };
        let tail_start = self
            .source
            .rfind('\n')
            .map_or(0, |index| index.saturating_add(1));
        let replace_tail = !self.source.is_empty() && !self.source.ends_with('\n');
        self.source.push_str(delta);
        *self
            .logical_copy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.plain_streaming = incremental;
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if incremental && let Some(rendered) = cached.as_mut() {
            if starts_new_paragraph {
                rendered.append_plain_paragraph(paragraph_delta, self.base_style);
            } else {
                rendered.replace_plain_tail(
                    replace_tail,
                    &self.source[tail_start..],
                    self.base_style,
                );
            }
        } else {
            *cached = None;
        }
    }

    fn append_reasoning(&mut self, delta: &str) {
        if self.source.ends_with("**") && delta.starts_with("**") {
            self.source.push_str("\n• ");
        }
        self.source.push_str(&indent_reasoning(delta));
        *self
            .logical_copy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn finalize(&mut self, source: &str) {
        source.clone_into(&mut self.source);
        self.streaming = false;
        self.plain_streaming = false;
        *self
            .logical_copy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn semanticize_copy(&self, selected: &str) -> Option<String> {
        let mut logical = self
            .logical_copy
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        logical
            .get_or_insert_with(|| LogicalMarkdown::from_sources([self.source.as_str()]))
            .copy_range(selected)
    }

    #[cfg(test)]
    fn materialized_text(&self, width: u16) -> Text<'static> {
        self.with_rendered(width, |rendered| rendered.text.clone())
    }

    fn height(&self, width: u16) -> usize {
        self.with_rendered(width, |rendered| rendered.height)
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
        self.with_rendered(area.width, |rendered| {
            rendered.render(area, buffer, scroll, total_height, selected);
        });
    }

    fn with_rendered<R>(&self, width: u16, read: impl FnOnce(&RenderedText) -> R) -> R {
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
            let mut text = render_agent_markdown(&source, width);
            if !self.show_header && !text.lines.is_empty() {
                let _ = text.lines.remove(0);
                for (index, line) in text.lines.iter_mut().enumerate() {
                    if line
                        .spans
                        .first()
                        .is_some_and(|span| span.content.as_ref() == "  ")
                    {
                        let _ = line.spans.remove(0);
                    }
                    if index > 0 && !line.spans.is_empty() {
                        line.spans.insert(0, Span::raw("  "));
                    }
                }
            }
            text.style = self.base_style;
            RenderedText::new(text, width)
        });
        read(rendered)
    }
}

impl RenderedText {
    fn new(text: Text<'static>, width: u16) -> Self {
        let mut line_heights = Vec::with_capacity(text.lines.len());
        let mut prewrapped_lines = Vec::with_capacity(text.lines.len());
        for line in &text.lines {
            let (height, rows) = rendered_line_layout(line, &text, width);
            line_heights.push(height);
            prewrapped_lines.push(rows);
        }
        let height = line_heights.iter().copied().sum();
        Self {
            width,
            text,
            line_heights,
            prewrapped_lines,
            height,
        }
    }

    fn replace_plain_tail(&mut self, replace_tail: bool, tail: &str, base_style: Style) {
        self.pop_line();
        if replace_tail {
            self.pop_line();
        }
        for line in tail.split_terminator('\n') {
            self.push_line(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_owned(), Style::default().fg(Color::White)),
            ]));
        }
        self.text.style = base_style;
        self.push_line(Line::raw(""));
    }

    fn append_plain_paragraph(&mut self, paragraph: &str, base_style: Style) {
        self.pop_line();
        self.push_line(Line::raw(""));
        for line in paragraph.split_terminator('\n') {
            self.push_line(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_owned(), Style::default().fg(Color::White)),
            ]));
        }
        self.text.style = base_style;
        self.push_line(Line::raw(""));
    }

    fn pop_line(&mut self) {
        let _ = self.text.lines.pop();
        if let Some(height) = self.line_heights.pop() {
            self.height = self.height.saturating_sub(height);
        }
        let _ = self.prewrapped_lines.pop();
    }

    fn push_line(&mut self, line: Line<'static>) {
        let (height, rows) = rendered_line_layout(&line, &self.text, self.width);
        self.height = self.height.saturating_add(height);
        self.text.lines.push(line);
        self.line_heights.push(height);
        self.prewrapped_lines.push(rows);
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
        let Some((first, mut line_top)) = self.line_at_visual_row(scroll, total_height) else {
            return;
        };
        let viewport_bottom = scroll.saturating_add(usize::from(area.height));
        let mut index = first;
        while index < self.text.lines.len() {
            let line_height = self.line_height(index);
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
                if let Some(rows) = self.prewrapped_lines[index].as_deref() {
                    for (screen_y, row) in rows
                        .iter()
                        .skip(visible_top.saturating_sub(line_top))
                        .take(usize::from(line_area.height))
                        .enumerate()
                    {
                        row.clone().render(
                            Rect::new(
                                line_area.x,
                                line_area.y.saturating_add(saturating_u16(screen_y)),
                                line_area.width,
                                1,
                            ),
                            buffer,
                        );
                    }
                } else {
                    let mut line = self.text.lines[index].clone();
                    line.alignment = line.alignment.or(self.text.alignment);
                    Paragraph::new(line)
                        .style(self.text.style)
                        .wrap(Wrap { trim: false })
                        .scroll((saturating_u16(visible_top.saturating_sub(line_top)), 0))
                        .render(line_area, buffer);
                }
            }
            line_top = line_bottom;
            index = index.saturating_add(1);
        }
        if selected {
            buffer.set_style(area, Style::default().add_modifier(Modifier::REVERSED));
        }
    }

    fn line_height(&self, index: usize) -> usize {
        self.line_heights[index]
    }

    fn line_at_visual_row(&self, row: usize, total_height: usize) -> Option<(usize, usize)> {
        if row >= total_height {
            return None;
        }
        if row <= total_height / 2 {
            let mut top = 0_usize;
            for index in 0..self.line_heights.len() {
                let height = self.line_height(index);
                let bottom = top.saturating_add(height);
                if row < bottom {
                    return Some((index, top));
                }
                top = bottom;
            }
            return None;
        }

        let mut bottom = total_height;
        for index in (0..self.line_heights.len()).rev() {
            let height = self.line_height(index);
            let top = bottom.saturating_sub(height);
            if row >= top {
                return Some((index, top));
            }
            bottom = top;
        }
        None
    }
}

fn rendered_line_layout(
    line: &Line<'static>,
    text: &Text<'static>,
    width: u16,
) -> (usize, Option<Vec<Line<'static>>>) {
    let cache_rows = line
        .spans
        .iter()
        .map(|span| span.content.len())
        .sum::<usize>()
        > 4_096;
    if cache_rows {
        let rows = wrap_styled_line(line, text.style, text.alignment, width);
        (rows.len(), Some(rows))
    } else {
        (
            Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width)
                .max(1),
            None,
        )
    }
}

impl StreamingText {
    fn tool_detail(arguments: &str, result: Option<&str>) -> Self {
        let body_style = Style::default().fg(Color::Gray);
        let mut detail = arguments.to_owned();
        if let Some(result) = result.filter(|result| !result.is_empty()) {
            push_detail(&mut detail, result);
        }
        let mut parts = detail.split('\n');
        let mut lines = Vec::with_capacity(detail.lines().count().saturating_add(1));
        lines.push(StreamingLine::new(
            format!("  └─ {}", parts.next().unwrap_or_default()),
            body_style,
        ));
        lines.extend(parts.map(|line| StreamingLine::new(format!("     {line}"), body_style)));
        Self {
            lines,
            first_line_prefix_width: 5,
        }
    }

    fn height(&self, width: u16) -> usize {
        self.lines.iter().map(|line| line.height(width)).sum()
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
        for (offset, line) in self.lines.iter().skip(first).enumerate() {
            let index = first.saturating_add(offset);
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
                if index == 0 && visible_top == line_top {
                    let prefix_width = if line_area.width <= self.first_line_prefix_width {
                        self.first_line_prefix_width.saturating_sub(1)
                    } else {
                        self.first_line_prefix_width
                    };
                    buffer.set_style(
                        Rect::new(
                            line_area.x,
                            line_area.y,
                            prefix_width.min(line_area.width),
                            1,
                        ),
                        Style::default().fg(Color::DarkGray),
                    );
                }
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
#[derive(Clone, Copy)]
struct StyledPart<'a> {
    symbol: &'a str,
    style: Style,
}

fn wrap_parts<'a>(
    parts: impl IntoIterator<Item = StyledPart<'a>>,
    width: u16,
) -> Vec<Vec<StyledPart<'a>>> {
    if width == 0 {
        return Vec::new();
    }
    let max_width = usize::from(width);
    let mut wrapped = Vec::<Vec<StyledPart<'a>>>::new();
    let mut pending_line = Vec::<StyledPart<'a>>::new();
    let mut pending_word = Vec::<StyledPart<'a>>::new();
    let mut pending_whitespace = VecDeque::<StyledPart<'a>>::new();
    let mut line_width = 0_usize;
    let mut word_width = 0_usize;
    let mut whitespace_width = 0_usize;
    let mut non_whitespace_previous = false;

    for part in parts {
        let is_whitespace = part.symbol == "\u{200b}"
            || (part.symbol.chars().all(char::is_whitespace) && part.symbol != "\u{00a0}");
        let symbol_width = UnicodeWidthStr::width(part.symbol);
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
                let whitespace_symbol_width = UnicodeWidthStr::width(whitespace.symbol);
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
            pending_whitespace.push_back(part);
        } else {
            word_width = word_width.saturating_add(symbol_width);
            pending_word.push(part);
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
}

fn wrap_line(content: &str, width: u16) -> Vec<String> {
    wrap_parts(
        UnicodeSegmentation::graphemes(content, true).map(|symbol| StyledPart {
            symbol,
            style: Style::default(),
        }),
        width,
    )
    .into_iter()
    .map(|line| line.into_iter().map(|part| part.symbol).collect())
    .collect()
}

fn wrap_styled_line(
    line: &Line<'static>,
    base_style: Style,
    default_alignment: Option<ratatui::layout::Alignment>,
    width: u16,
) -> Vec<Line<'static>> {
    let alignment = line.alignment.or(default_alignment);
    wrap_parts(
        line.styled_graphemes(base_style)
            .map(|grapheme| StyledPart {
                symbol: grapheme.symbol,
                style: grapheme.style,
            }),
        width,
    )
    .into_iter()
    .map(|parts| {
        let mut spans = Vec::<Span<'static>>::new();
        let mut content = String::new();
        let mut style = None;
        for part in parts {
            if style.is_some_and(|current| current != part.style) {
                spans.push(Span::styled(
                    mem::take(&mut content),
                    style.unwrap_or_default(),
                ));
            }
            style = Some(part.style);
            content.push_str(part.symbol);
        }
        if !content.is_empty() {
            spans.push(Span::styled(content, style.unwrap_or_default()));
        }
        let mut row = Line::from(spans);
        row.alignment = alignment;
        row
    })
    .collect()
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

fn render_selected_user_affordance(area: Rect, buffer: &mut Buffer, scroll: usize) {
    const HINT: &str = "e to edit";
    if scroll == 0 && area.width > saturating_u16(HINT.len() + 4) {
        let hint_width = saturating_u16(HINT.len());
        let hint_area = Rect::new(
            area.right().saturating_sub(hint_width + 1),
            area.y,
            hint_width,
            1,
        );
        Paragraph::new(HINT)
            .alignment(Alignment::Right)
            .style(Style::default().fg(Color::Yellow).bg(Color::Indexed(8)))
            .render(hint_area, buffer);
    }
}

fn indent_reasoning(source: &str) -> String {
    source.replace('\n', "\n  ")
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
    use std::sync::{Arc, atomic::AtomicBool};

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
        EntryContent, InlineEdit, MarkdownContent, StreamingLine, ToolActivity, ToolStatus,
        Transcript, TranscriptItem, child_lines, render_agent_markdown, saturating_u16, tool_style,
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
    fn markdown_viewport_render_matches_full_paragraph_scrolling() {
        let markdown = MarkdownContent::new(
            "# Heading\n\nA **styled** paragraph with `code`, unicode λ, and a long line that wraps across several visual rows without losing its formatting.\n\n- first\n  - nested\n\n| A | B |\n| - | - |\n| one | two |",
        );
        for width in [20, 43] {
            let height = markdown.height(width);
            for scroll in [0, height / 2, height.saturating_sub(5)] {
                let area = Rect::new(0, 0, width, 5);
                let mut expected = Buffer::empty(area);
                Paragraph::new(render_agent_markdown(&markdown.source, width))
                    .wrap(Wrap { trim: false })
                    .scroll((saturating_u16(scroll), 0))
                    .render(area, &mut expected);

                let mut actual = Buffer::empty(area);
                markdown.render(area, &mut actual, scroll, height, false);
                assert_eq!(actual, expected, "width={width} scroll={scroll}");
            }
        }
    }

    #[test]
    fn long_styled_markdown_line_uses_parity_checked_cached_rows() {
        let source = format!("**{}**", "styled λ text ".repeat(600));
        let markdown = MarkdownContent::new(&source);
        let width = 20;
        let height = markdown.height(width);
        assert!(height > 256);
        for scroll in [0, height / 2, height.saturating_sub(5)] {
            let area = Rect::new(0, 0, width, 5);
            let mut expected = Buffer::empty(area);
            Paragraph::new(render_agent_markdown(&markdown.source, width))
                .wrap(Wrap { trim: false })
                .scroll((saturating_u16(scroll), 0))
                .render(area, &mut expected);

            let mut actual = Buffer::empty(area);
            markdown.render(area, &mut actual, scroll, height, false);
            assert_eq!(actual, expected, "scroll={scroll}");
        }
    }

    #[test]
    fn streaming_plain_markdown_append_matches_a_fresh_parse() {
        for (source, delta) in [
            ("plain words", " added words"),
            ("first line\nsecond line", "\nthird λ line"),
            ("first paragraph\n", "\nsecond paragraph"),
            ("first paragraph\n\nsecond", " paragraph"),
            ("first paragraph\n\n", "second paragraph"),
            ("1", " item"),
            ("1", ". ordered item"),
            ("-", " list item"),
            ("plain", "\n\n# heading"),
        ] {
            for width in [20, 80] {
                let mut incremental = MarkdownContent::streaming(source);
                let _ = incremental.height(width);
                incremental.append(delta);

                let fresh = MarkdownContent::streaming(&format!("{source}{delta}"));
                assert_eq!(
                    incremental.materialized_text(width),
                    fresh.materialized_text(width),
                    "source={source:?} delta={delta:?} width={width}",
                );
                assert_eq!(incremental.height(width), fresh.height(width));
            }
        }
    }

    #[test]
    fn long_nested_tool_result_cache_matches_full_paragraph_scrolling() {
        let details_expanded = Arc::new(AtomicBool::new(true));
        let mut tool = ToolActivity::new(
            "code-mode-1".to_owned(),
            "exec".to_owned(),
            "text(await tools.exec_command({ cmd: 'render report' }));".to_owned(),
            ToolStatus::Completed,
            Arc::clone(&details_expanded),
        );
        let mut child = ToolActivity::new(
            "code-mode-1/code-1".to_owned(),
            "exec_command".to_owned(),
            "render report".to_owned(),
            ToolStatus::Completed,
            details_expanded,
        );
        child.duration_ns = Some(1_000_000);
        child.result = Some("styled λ output ".repeat(20_000));
        child.refresh_plain_detail();
        tool.children.push(child);

        for width in [40, 120] {
            let height = tool.height(width);
            for scroll in [0, height / 2, height.saturating_sub(8)] {
                let area = Rect::new(0, 0, width, 8);
                let mut expected = Buffer::empty(area);
                Paragraph::new(tool.text(width))
                    .wrap(Wrap { trim: false })
                    .scroll((saturating_u16(scroll), 0))
                    .render(area, &mut expected);

                let mut actual = Buffer::empty(area);
                tool.render(area, &mut actual, scroll, height, false);
                assert_eq!(actual, expected, "width={width} scroll={scroll}");
            }
        }
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
            arguments: "const parts = await Promise.all(files.map(scan));\nawait reduce(parts);"
                .to_owned(),
            status: ToolStatus::Running,
        });
        assert!(transcript.push_tool_child(
            "call-1/code-1".to_owned(),
            "exec_command".to_owned(),
            "cargo test \\\n  --workspace".to_owned(),
            ToolStatus::Running,
        ));
        assert!(transcript.set_tool_result_timing(
            "call-1/code-1",
            ToolStatus::Completed,
            Some(1_000_000),
            Some(90_000_000),
            Some("exit 0".to_owned()),
        ));
        assert!(transcript.push_tool_child(
            "call-1/code-2".to_owned(),
            "apply_patch".to_owned(),
            "src/main.rs".to_owned(),
            ToolStatus::Running,
        ));
        assert!(transcript.set_tool_result_timing(
            "call-1/code-2",
            ToolStatus::Completed,
            Some(2_000_000),
            Some(80_000_000),
            Some("applied".to_owned()),
        ));
        assert!(transcript.push_tool_child(
            "call-1/code-3".to_owned(),
            "exec_command".to_owned(),
            "jq -s add /tmp/parts/*.json · in /repo".to_owned(),
            ToolStatus::Running,
        ));
        assert!(transcript.set_tool_result_timing(
            "call-1/code-3",
            ToolStatus::Completed,
            Some(100_000_000),
            Some(20_000_000),
            Some("exit 0".to_owned()),
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
        assert!(rendered.contains("✓ Tools  3 calls · 120ms"));
        assert!(!rendered.contains("overlapping"));
        assert!(!rendered.contains("sequence"));
        assert!(!rendered.contains("javascript"));
        assert!(!rendered.contains("const tasks"));
        assert!(rendered.contains("├─┬ ✓ Ran"));
        assert!(rendered.contains("cargo test \\"));
        assert!(rendered.contains("--workspace"));
        assert!(rendered.contains("│ └ ✓ apply_patch  src/main.rs · 80ms · applied"));
        assert!(rendered.contains("└── ✓ Ran  jq -s add /tmp/parts/*.json"));
    }

    #[test]
    fn parallel_tool_rows_update_independently_as_promises_resolve() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "call-1".to_owned(),
            name: "exec".to_owned(),
            arguments: "await Promise.all(tasks);".to_owned(),
            status: ToolStatus::Running,
        });
        for (call_id, command) in [
            ("call-1/code-1", "sleep 0.08"),
            ("call-1/code-2", "sleep 0.01"),
        ] {
            assert!(transcript.push_tool_child(
                call_id.to_owned(),
                "exec_command".to_owned(),
                command.to_owned(),
                ToolStatus::Running,
            ));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        let running = terminal.backend().to_string();
        assert!(running.contains("◌ Running  sleep 0.08"));
        assert!(running.contains("◌ Running  sleep 0.01"));

        assert!(transcript.set_tool_result_timing(
            "call-1/code-2",
            ToolStatus::Completed,
            Some(1_000_000),
            Some(10_000_000),
            Some("exit 0".to_owned()),
        ));
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("◌ Running  sleep 0.08"));
        assert!(rendered.contains("✓ Ran  sleep 0.01 · 10ms · exit 0"));
    }

    #[test]
    fn code_mode_frame_never_falls_back_to_the_raw_javascript() {
        let source = "await tools.update_plan({ plan: tasks }); // secretFlashMarker";
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "call-1".to_owned(),
            name: "exec".to_owned(),
            arguments: source.to_owned(),
            status: ToolStatus::Running,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        assert!(terminal.backend().to_string().contains("◌ Working"));
        assert!(!terminal.backend().to_string().contains(source));

        assert!(transcript.push_tool_child(
            "call-1/code-1".to_owned(),
            "exec_command".to_owned(),
            "cargo test --workspace".to_owned(),
            ToolStatus::Running,
        ));
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("◌ Tools  1 call"));
        assert!(rendered.contains("cargo test --workspace"));
        assert!(!rendered.contains(source));
    }

    #[test]
    fn code_mode_patch_body_uses_compact_child_indentation() {
        let child = ToolActivity::new(
            "call-1/code-1".to_owned(),
            "apply_patch".to_owned(),
            "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old();\n+new();\n*** End Patch"
                .to_owned(),
            ToolStatus::Completed,
            Arc::new(AtomicBool::new(true)),
        );

        let lines = child_lines(&child, "  └──", "      ", 80);
        let body = lines
            .iter()
            .skip(1)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(body.iter().any(|line| line.starts_with("        1 -old")));
        assert!(body.iter().all(|line| !line.starts_with("          ")));
    }

    #[test]
    fn tool_details_can_be_folded_without_losing_the_activity_summary() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "call-fold".to_owned(),
            name: "exec_command".to_owned(),
            arguments: "cargo test --workspace\nsecond detail line".to_owned(),
            status: ToolStatus::Completed,
        });

        let mut expanded = Terminal::new(TestBackend::new(80, 8)).unwrap();
        expanded
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        assert!(
            expanded
                .backend()
                .to_string()
                .contains("second detail line")
        );

        transcript.set_tool_details_expanded(false);
        let mut folded = Terminal::new(TestBackend::new(80, 8)).unwrap();
        folded
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, None, None, "empty"), frame.area());
            })
            .unwrap();
        let rendered = folded.backend().to_string();
        assert!(rendered.contains("✓ exec_command"));
        assert!(rendered.contains("cargo test --workspace second detail line"));
        assert!(!rendered.contains("└─"));
    }

    #[test]
    fn folding_does_not_copy_branch_shared_tool_entries() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Tool {
            call_id: "call-fold".to_owned(),
            name: "exec_command".to_owned(),
            arguments: "x".repeat(100_000),
            status: ToolStatus::Completed,
        });
        let shared = transcript.clone();

        transcript.set_tool_details_expanded(false);

        assert!(Arc::ptr_eq(&transcript.entries[0], &shared.entries[0]));
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
        assert!(rendered.contains("• Edited src/main.rs (+1 -1)"));
        assert!(rendered.contains("1 -old();"));
        assert!(rendered.contains("1 +new();"));
    }

    #[test]
    fn reasoning_deltas_stream_as_compact_dim_markdown() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::User("Please inspect this.".to_owned()));
        transcript.push(TranscriptItem::Reasoning("**Inspecting**".to_owned()));
        assert!(transcript.append_reasoning_delta(" the *request*\nand tools"));

        let area = Rect::new(0, 0, 30, 6);
        let mut buffer = Buffer::empty(area);
        transcript
            .widget(0, None, None, "empty")
            .render(area, &mut buffer);

        let bullet = (0..area.height)
            .flat_map(|row| (0..area.width).map(move |column| (column, row)))
            .find(|position| buffer.cell(*position).unwrap().symbol() == "•")
            .unwrap();
        assert_eq!(bullet, (0, 2));
        assert_eq!(buffer.cell((2, 3)).unwrap().symbol(), "a");
        assert!(
            buffer
                .cell((0, 2))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::DIM)
        );
        assert!(
            buffer
                .cell((2, 2))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        let rendered = (0..area.width)
            .map(|column| buffer.cell((column, 2)).unwrap().symbol())
            .collect::<String>();
        assert_eq!(rendered.matches('*').count(), 0);
        let request_column = u16::try_from(rendered.find("request").unwrap()).unwrap();
        assert!(
            buffer
                .cell((request_column, 2))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::ITALIC)
        );
    }

    #[test]
    fn adjacent_bold_reasoning_summaries_render_as_separate_bullets() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Reasoning(
            "**Planning parallel commands**".to_owned(),
        ));
        assert!(transcript.append_reasoning_delta("**Designing varied-duration command demos**"));

        let EntryContent::Markdown(markdown) = &transcript.entries[0].content else {
            panic!("reasoning should render as Markdown");
        };
        let rendered = markdown
            .materialized_text(80)
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Planning parallel commands"));
        assert!(rendered.contains("• Designing varied-duration command demos"));
        assert!(!rendered.contains("****"));
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
        assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Indexed(8));
        assert_eq!(buffer.cell((0, 2)).unwrap().bg, Color::Indexed(8));
        assert_eq!(buffer.cell((0, 3)).unwrap().bg, Color::Reset);
        let header = buffer.content[..20]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(header.contains("e to edit"));
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
        assert_eq!(buffer.cell((0, 3)).unwrap().bg, Color::Indexed(8));
        assert_eq!(buffer.cell((0, 5)).unwrap().bg, Color::Indexed(8));
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
