use std::{borrow::Cow, io, time::SystemTime};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nanocodex::agent::rollout::RolloutSessionInfo;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use super::terminal::TerminalSession;

pub(crate) fn select_resume_session(sessions: &[RolloutSessionInfo]) -> io::Result<Option<String>> {
    let mut terminal = TerminalSession::enter()?;
    let mut picker = ResumePicker::new(sessions.len());
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
            PickerAction::Select => {
                return Ok(sessions
                    .get(picker.selected)
                    .map(|session| session.thread_id().to_owned()));
            }
            PickerAction::Cancel => return Ok(None),
        }
    }
}

struct ResumePicker {
    selected: usize,
    session_count: usize,
    page_size: usize,
}

impl ResumePicker {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PickerAction {
    Continue,
    Select,
    Cancel,
}

fn render(
    frame: &mut Frame<'_>,
    sessions: &[RolloutSessionInfo],
    picker: &mut ResumePicker,
    now: SystemTime,
) {
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

fn session_item(
    session: &RolloutSessionInfo,
    selected: bool,
    now: SystemTime,
) -> ListItem<'static> {
    let marker = if selected { "›" } else { " " };
    let style = if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let workspace =
        sanitized_terminal_text(session.workspace().unwrap_or("(workspace unavailable)"));
    let location = if session.is_archived() {
        "archived"
    } else {
        "active"
    };
    let preview = sanitized_terminal_text(session.preview().unwrap_or("(prompt unavailable)"));
    ListItem::new(vec![
        Line::styled(
            format!(
                "{marker} {} · {preview}",
                format_age(session.modified_at(), now)
            ),
            style,
        ),
        Line::styled(
            format!("  {workspace} · {location} · {}", session.thread_id()),
            if selected {
                style
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ])
}

fn sanitized_terminal_text(value: &str) -> Cow<'_, str> {
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::KeyEvent;

    use super::*;

    #[test]
    fn picker_navigation_is_bounded_and_pages_by_the_visible_count() {
        let mut picker = ResumePicker::new(10);
        assert_eq!(picker.visible_range(3), 0..3);

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            PickerAction::Continue
        );
        assert_eq!(picker.selected, 3);
        assert_eq!(picker.visible_range(3), 1..4);

        picker.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(picker.selected, 9);
        assert_eq!(picker.visible_range(3), 7..10);

        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(picker.selected, 9);
        picker.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn picker_confirms_and_cancels_with_standard_keys() {
        let mut picker = ResumePicker::new(1);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PickerAction::Select
        );
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PickerAction::Cancel
        );
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            PickerAction::Cancel
        );
    }

    #[test]
    fn age_labels_are_stable_at_display_boundaries() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
        assert_eq!(format_age(now - Duration::from_secs(59), now), "now");
        assert_eq!(format_age(now - Duration::from_secs(60), now), "1m ago");
        assert_eq!(format_age(now - Duration::from_secs(3_600), now), "1h ago");
        assert_eq!(format_age(now - Duration::from_secs(86_400), now), "1d ago");
    }

    #[test]
    fn terminal_text_strips_control_sequences_without_allocating_normal_text() {
        assert!(matches!(
            sanitized_terminal_text("normal text"),
            Cow::Borrowed("normal text")
        ));
        assert_eq!(
            sanitized_terminal_text("/tmp/\u{1b}]52;c;payload\u{7}\nworkspace"),
            "/tmp/]52;c;payloadworkspace"
        );
    }
}
