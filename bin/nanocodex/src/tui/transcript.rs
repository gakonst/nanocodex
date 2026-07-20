use std::cell::Cell;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Widget, Wrap},
};

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

#[derive(Clone, Copy)]
pub(super) enum ToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Default)]
pub(super) struct Transcript {
    entries: Vec<TranscriptEntry>,
}

impl Transcript {
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn push(&mut self, item: TranscriptItem) {
        self.entries.push(TranscriptEntry::new(item));
    }

    pub(super) fn append_assistant_delta(&mut self, delta: &str) -> bool {
        self.entries
            .last_mut()
            .is_some_and(|entry| entry.append_assistant_delta(delta))
    }

    pub(super) fn set_tool_status(&mut self, call_id: &str, status: ToolStatus) {
        if let Some(entry) =
            self.entries.iter_mut().rev().find(
                |entry| matches!(&entry.kind, EntryKind::Tool { call_id: id } if id == call_id),
            )
        {
            entry.set_tool_status(status);
        }
    }

    pub(super) fn widget(
        &self,
        scroll_from_bottom: usize,
        empty_message: &'static str,
    ) -> TranscriptWidget<'_> {
        TranscriptWidget {
            transcript: self,
            scroll_from_bottom,
            empty_message,
        }
    }
}

pub(super) struct TranscriptWidget<'a> {
    transcript: &'a Transcript,
    scroll_from_bottom: usize,
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

        let width = area.width;
        let viewport_height = usize::from(area.height);
        let content_height = self
            .transcript
            .entries
            .iter()
            .map(|entry| entry.height(width))
            .sum::<usize>();
        let max_scroll = content_height.saturating_sub(viewport_height);
        let scroll = max_scroll.saturating_sub(self.scroll_from_bottom.min(max_scroll));
        let viewport_end = scroll.saturating_add(viewport_height);
        let mut entry_top = 0_usize;

        for entry in &self.transcript.entries {
            let entry_height = entry.height(width);
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
            let screen_y = area
                .y
                .saturating_add(saturating_u16(visible_top.saturating_sub(scroll)));
            let visible_height = saturating_u16(visible_bottom.saturating_sub(visible_top));
            let local_scroll = saturating_u16(visible_top.saturating_sub(entry_top));
            let entry_area = Rect::new(area.x, screen_y, area.width, visible_height);
            entry
                .paragraph()
                .scroll((local_scroll, 0))
                .render(entry_area, buffer);
            entry_top = entry_bottom;
        }
    }
}

enum EntryKind {
    User,
    Assistant,
    Tool { call_id: String },
    Error,
}

struct TranscriptEntry {
    kind: EntryKind,
    text: Text<'static>,
    cached_height: Cell<Option<(u16, usize)>>,
}

impl TranscriptEntry {
    fn new(item: TranscriptItem) -> Self {
        let (kind, text) = match item {
            TranscriptItem::User(message) => (
                EntryKind::User,
                message_text(
                    "› You",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    &message,
                ),
            ),
            TranscriptItem::Assistant(message) => (
                EntryKind::Assistant,
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
                Text::from(vec![
                    Line::styled(format!("✗ {message}"), Style::default().fg(Color::Red)),
                    Line::raw(""),
                ]),
            ),
        };
        Self {
            kind,
            text,
            cached_height: Cell::new(None),
        }
    }

    fn paragraph(&self) -> Paragraph<'static> {
        Paragraph::new(self.text.clone()).wrap(Wrap { trim: false })
    }

    fn height(&self, width: u16) -> usize {
        if let Some((cached_width, height)) = self.cached_height.get()
            && cached_width == width
        {
            return height;
        }
        let height = self.paragraph().line_count(width);
        self.cached_height.set(Some((width, height)));
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
        self.cached_height.set(None);
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
        self.cached_height.set(None);
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
        ToolStatus::Failed => ("✗", Color::Red),
    }
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, widgets::Widget};

    use super::{Transcript, TranscriptItem};

    #[test]
    fn assistant_deltas_update_only_the_tail_entry() {
        let mut transcript = Transcript::default();
        transcript.push(TranscriptItem::Assistant("first".to_owned()));
        assert!(transcript.append_assistant_delta(" line\nsecond"));

        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(transcript.widget(0, "empty"), frame.area());
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
            .widget(0, "empty")
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
}
