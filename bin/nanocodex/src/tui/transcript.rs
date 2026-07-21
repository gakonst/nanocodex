use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use super::composer::ComposerLayout;

#[derive(Clone, Copy)]
pub(super) struct InlineEdit<'a> {
    pub(super) index: usize,
    pub(super) input: &'a str,
    pub(super) cursor: usize,
}

pub(super) enum TranscriptItem {
    User(String),
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

#[derive(Clone, Default)]
pub(super) struct Transcript {
    entries: Vec<Arc<TranscriptEntry>>,
    editable_users: Vec<usize>,
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
    }

    pub(super) fn push_editable_user(&mut self, message: String, prompt_id: u64) {
        let mut entry = TranscriptEntry::new(TranscriptItem::User(message));
        entry.prompt_id = Some(prompt_id);
        self.editable_users.push(self.entries.len());
        self.entries.push(Arc::new(entry));
    }

    pub(super) fn append_assistant_delta(&mut self, delta: &str) -> bool {
        self.entries
            .last_mut()
            .is_some_and(|entry| Arc::make_mut(entry).append_assistant_delta(delta))
    }

    pub(super) fn tail_is_assistant(&self) -> bool {
        self.entries
            .last()
            .is_some_and(|entry| matches!(entry.kind, EntryKind::Assistant))
    }

    pub(super) fn tail_height(&self, width: u16) -> Option<usize> {
        self.entries.last().map(|entry| entry.height(width))
    }

    pub(super) fn height_at(&self, index: usize, width: u16) -> Option<usize> {
        self.entries.get(index).map(|entry| entry.height(width))
    }

    pub(super) fn height_from(&self, first: usize, width: u16) -> usize {
        self.entries[first..]
            .iter()
            .map(|entry| entry.height(width))
            .sum()
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
        }
    }

    pub(super) fn set_tool_status(&mut self, call_id: &str, status: ToolStatus) {
        if let Some(entry) =
            self.entries.iter_mut().rev().find(
                |entry| matches!(&entry.kind, EntryKind::Tool { call_id: id } if id == call_id),
            )
        {
            Arc::make_mut(entry).set_tool_status(status);
        }
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
            let mut paragraph = rendered_paragraph(entry, index, area.width, inline_edit);
            if selected == Some(index) && inline_edit.is_none_or(|edit| edit.index != index) {
                paragraph = paragraph.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            paragraph
                .scroll((saturating_u16(local_scroll), 0))
                .render(entry_area, buffer);
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

fn rendered_paragraph(
    entry: &TranscriptEntry,
    index: usize,
    width: u16,
    inline_edit: Option<InlineEdit<'_>>,
) -> Paragraph<'static> {
    inline_edit.filter(|edit| edit.index == index).map_or_else(
        || entry.paragraph(),
        |edit| {
            Paragraph::new(inline_edit_text(edit.input, width))
                .block(
                    Block::default()
                        .title(" Edit message · Esc cancel ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                )
                .wrap(Wrap { trim: false })
        },
    )
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
    Assistant,
    Tool { call_id: String },
    Error,
}

struct TranscriptEntry {
    kind: EntryKind,
    user_message: Option<String>,
    prompt_id: Option<u64>,
    text: Text<'static>,
    cached_height: AtomicU64,
}

impl Clone for TranscriptEntry {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            user_message: self.user_message.clone(),
            prompt_id: self.prompt_id,
            text: self.text.clone(),
            cached_height: AtomicU64::new(self.cached_height.load(Ordering::Relaxed)),
        }
    }
}

impl TranscriptEntry {
    fn new(item: TranscriptItem) -> Self {
        let (kind, user_message, text) = match item {
            TranscriptItem::User(message) => {
                let text = message_text(
                    "› You",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    &message,
                );
                (EntryKind::User, Some(message), text)
            }
            TranscriptItem::Assistant(message) => (
                EntryKind::Assistant,
                None,
                message_text(
                    "● Nanocodex",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    &message,
                ),
            ),
            TranscriptItem::Tool {
                call_id,
                name,
                arguments,
                status,
            } => {
                let (icon, color) = tool_style(status);
                (
                    EntryKind::Tool { call_id },
                    None,
                    Text::from(vec![
                        Line::from(vec![
                            Span::styled(format!("{icon} {name}"), Style::default().fg(color)),
                            Span::styled(
                                format!("  {arguments}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]),
                        Line::raw(""),
                    ]),
                )
            }
            TranscriptItem::Error(message) => (
                EntryKind::Error,
                None,
                Text::from(vec![
                    Line::styled(format!("✗ {message}"), Style::default().fg(Color::Red)),
                    Line::raw(""),
                ]),
            ),
        };
        Self {
            kind,
            user_message,
            prompt_id: None,
            text,
            cached_height: AtomicU64::new(0),
        }
    }

    fn paragraph(&self) -> Paragraph<'static> {
        Paragraph::new(self.text.clone()).wrap(Wrap { trim: false })
    }

    fn user_message(&self) -> Option<&str> {
        self.user_message.as_deref()
    }

    fn height(&self, width: u16) -> usize {
        let cached = self.cached_height.load(Ordering::Relaxed);
        if cached != 0 && cached >> 48 == u64::from(width) {
            return usize::try_from(cached & ((1_u64 << 48) - 1)).unwrap_or(usize::MAX);
        }
        let height = self.paragraph().line_count(width);
        let encoded = (u64::from(width) << 48)
            | u64::try_from(height)
                .unwrap_or((1_u64 << 48) - 1)
                .min((1_u64 << 48) - 1);
        self.cached_height.store(encoded, Ordering::Relaxed);
        height
    }

    fn append_assistant_delta(&mut self, delta: &str) -> bool {
        if !matches!(self.kind, EntryKind::Assistant) {
            return false;
        }

        drop(self.text.lines.pop());
        let mut parts = delta.split('\n');
        if let Some(first) = parts.next()
            && let Some(body) = self.text.lines.last_mut()
            && let Some(span) = body.spans.last_mut()
        {
            span.content.to_mut().push_str(first);
        }
        for part in parts {
            self.text.lines.push(Line::styled(
                format!("  {part}"),
                Style::default().fg(Color::White),
            ));
        }
        self.text.lines.push(Line::raw(""));
        self.cached_height.store(0, Ordering::Relaxed);
        true
    }

    fn set_tool_status(&mut self, status: ToolStatus) {
        let EntryKind::Tool { .. } = self.kind else {
            return;
        };
        let (icon, color) = tool_style(status);
        if let Some(span) = self
            .text
            .lines
            .first_mut()
            .and_then(|line| line.spans.first_mut())
        {
            let content = span.content.to_mut();
            let old_icon_len = content.chars().next().map_or(0, char::len_utf8);
            content.replace_range(..old_icon_len, icon);
            span.style = Style::default().fg(color);
        }
        self.cached_height.store(0, Ordering::Relaxed);
    }
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
        Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Color, widgets::Widget,
    };

    use super::{InlineEdit, ToolStatus, Transcript, TranscriptItem, saturating_u16, tool_style};

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
