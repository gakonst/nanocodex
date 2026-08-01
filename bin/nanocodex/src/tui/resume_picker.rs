use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use nanocodex::agent::rollout::DurableSessionSummary;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::terminal::TerminalSession;

pub(crate) async fn select_session(
    sessions: &[DurableSessionSummary],
    show_all: bool,
) -> Result<Option<String>> {
    let choices = sessions.iter().map(ResumeChoice::from).collect::<Vec<_>>();
    let mut picker = ResumePicker::new(choices);
    let mut terminal = TerminalSession::enter().wrap_err("failed to initialize resume picker")?;
    let mut events = EventStream::new();
    loop {
        terminal.draw(|frame| render(frame, &mut picker, show_all))?;
        let event = events
            .next()
            .await
            .transpose()?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        let Event::Key(key) = event else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Ok(None),
                KeyCode::Char('u') => picker.clear_query(),
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Up => picker.move_selection(-1),
            KeyCode::Down => picker.move_selection(1),
            KeyCode::Home => picker.select_first(),
            KeyCode::End => picker.select_last(),
            KeyCode::Backspace => picker.pop_query(),
            KeyCode::Char(character) => picker.push_query(character),
            KeyCode::Enter => return Ok(picker.selected_thread_id().map(str::to_owned)),
            KeyCode::Esc => return Ok(None),
            _ => {}
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ResumeChoice {
    thread_id: String,
    workspace: String,
    updated: String,
    prompt: String,
}

impl From<&DurableSessionSummary> for ResumeChoice {
    fn from(summary: &DurableSessionSummary) -> Self {
        Self {
            thread_id: summary.thread_id().to_owned(),
            workspace: summary.workspace().display().to_string(),
            updated: updated_label(summary.updated_at()),
            prompt: summary
                .first_prompt()
                .map(compact_prompt)
                .unwrap_or_default(),
        }
    }
}

fn updated_label(updated_at: std::time::SystemTime) -> String {
    let elapsed = std::time::SystemTime::now()
        .duration_since(updated_at)
        .unwrap_or_default();
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        "now".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{}h ago", seconds / (60 * 60))
    } else {
        format!("{}d ago", seconds / (24 * 60 * 60))
    }
}

fn compact_prompt(prompt: &str) -> String {
    prompt.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct ResumePicker {
    choices: Vec<ResumeChoice>,
    visible: Vec<usize>,
    query: String,
    state: ListState,
}

impl ResumePicker {
    fn new(choices: Vec<ResumeChoice>) -> Self {
        let visible = (0..choices.len()).collect::<Vec<_>>();
        let mut state = ListState::default();
        if !visible.is_empty() {
            state.select(Some(0));
        }
        Self {
            choices,
            visible,
            query: String::new(),
            state,
        }
    }

    fn move_selection(&mut self, direction: isize) {
        let Some(selected) = self.state.selected() else {
            return;
        };
        let last = self.visible.len().saturating_sub(1);
        self.state
            .select(Some(selected.saturating_add_signed(direction).min(last)));
    }

    fn select_first(&mut self) {
        if !self.visible.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn select_last(&mut self) {
        if !self.visible.is_empty() {
            self.state.select(Some(self.visible.len() - 1));
        }
    }

    fn push_query(&mut self, character: char) {
        self.query.push(character);
        self.rebuild_visible();
    }

    fn pop_query(&mut self) {
        let _ = self.query.pop();
        self.rebuild_visible();
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.rebuild_visible();
    }

    fn rebuild_visible(&mut self) {
        let query = self.query.to_lowercase();
        self.visible = self
            .choices
            .iter()
            .enumerate()
            .filter_map(|(index, choice)| {
                (choice.prompt.to_lowercase().contains(&query)
                    || choice.workspace.to_lowercase().contains(&query)
                    || choice.thread_id.to_lowercase().contains(&query))
                .then_some(index)
            })
            .collect();
        self.state.select((!self.visible.is_empty()).then_some(0));
    }

    fn selected_thread_id(&self) -> Option<&str> {
        self.state
            .selected()
            .and_then(|visible| self.visible.get(visible))
            .and_then(|choice| self.choices.get(*choice))
            .map(|choice| choice.thread_id.as_str())
    }
}

fn render(frame: &mut Frame<'_>, picker: &mut ResumePicker, show_all: bool) {
    let [header, sessions, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    render_header(frame, picker, header, show_all);
    render_sessions(frame, picker, sessions);
    render_footer(frame, picker, footer);
}

fn render_header(frame: &mut Frame<'_>, picker: &ResumePicker, area: Rect, show_all: bool) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Resume a previous session",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let [search, options] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .areas(Rect::new(area.x, area.y.saturating_add(2), area.width, 1));
    let search_line = if picker.query.is_empty() {
        Line::from(Span::styled(
            "Type to search",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(vec![
            Span::styled("Search: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&picker.query),
            Span::styled("▌", Style::default().fg(Color::Cyan)),
        ])
    };
    frame.render_widget(Paragraph::new(search_line), search);
    let filter = if show_all { "All" } else { "Cwd" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Filter: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[{filter}]"),
                Style::default().fg(Color::LightMagenta),
            ),
            Span::styled("   Sort: ", Style::default().fg(Color::DarkGray)),
            Span::styled("[Updated]", Style::default().fg(Color::White)),
        ]))
        .alignment(Alignment::Right),
        options,
    );
}

fn render_sessions(frame: &mut Frame<'_>, picker: &mut ResumePicker, area: Rect) {
    let row_width = usize::from(area.width).saturating_sub(2);
    let items = picker.visible.iter().filter_map(|index| {
        let choice = picker.choices.get(*index)?;
        let age = format!("{:>7}", choice.updated);
        let prompt_width = row_width.saturating_sub(age.width()).saturating_sub(2);
        Some(ListItem::new(Line::from(vec![
            Span::styled(age, Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::raw(truncate_with_ellipsis(&choice.prompt, prompt_width)),
        ])))
    });
    let list = List::new(items).highlight_symbol("› ").highlight_style(
        Style::default()
            .fg(Color::LightYellow)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, area, &mut picker.state);
}

fn render_footer(frame: &mut Frame<'_>, picker: &ResumePicker, area: Rect) {
    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [controls, count] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(16)]).areas(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("enter", Style::default().fg(Color::White)),
            Span::styled(" resume   ", Style::default().fg(Color::DarkGray)),
            Span::styled("esc", Style::default().fg(Color::White)),
            Span::styled(" cancel   ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑/↓", Style::default().fg(Color::White)),
            Span::styled(" browse", Style::default().fg(Color::DarkGray)),
        ])),
        controls,
    );
    let selected = picker.state.selected().map_or(0, |index| index + 1);
    frame.render_widget(
        Paragraph::new(format!("{selected} / {}", picker.visible.len()))
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Right),
        count,
    );
}

fn truncate_with_ellipsis(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let target = width.saturating_sub(1);
    let mut truncated = String::new();
    let mut used = 0_usize;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if used.saturating_add(character_width) > target {
            break;
        }
        truncated.push(character);
        used = used.saturating_add(character_width);
    }
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn choices() -> Vec<ResumeChoice> {
        ["one", "two", "three"]
            .into_iter()
            .map(|thread_id| ResumeChoice {
                thread_id: thread_id.to_owned(),
                workspace: "/workspace/nanocodex".to_owned(),
                updated: "3m ago".to_owned(),
                prompt: format!("{thread_id} prompt"),
            })
            .collect()
    }

    #[test]
    fn selection_is_bounded_and_can_jump_to_each_end() {
        let mut picker = ResumePicker::new(choices());
        assert_eq!(picker.selected_thread_id(), Some("one"));

        picker.move_selection(-1);
        assert_eq!(picker.selected_thread_id(), Some("one"));
        picker.select_last();
        picker.move_selection(1);
        assert_eq!(picker.selected_thread_id(), Some("three"));
        picker.select_first();
        assert_eq!(picker.selected_thread_id(), Some("one"));
    }

    #[test]
    fn search_filters_prompts_and_resets_the_selection() {
        let mut picker = ResumePicker::new(choices());
        for character in "two".chars() {
            picker.push_query(character);
        }
        assert_eq!(picker.visible.len(), 1);
        assert_eq!(picker.selected_thread_id(), Some("two"));

        picker.clear_query();
        assert_eq!(picker.visible.len(), 3);
        assert_eq!(picker.selected_thread_id(), Some("one"));
    }

    #[test]
    fn picker_renders_a_compact_single_line_session_list() {
        let mut picker = ResumePicker::new(choices());
        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, &mut picker, false);
            })
            .unwrap();

        let rendered = terminal.backend().buffer().content().to_owned();
        assert!(rendered.iter().any(|cell| cell.symbol() == "›"));
        let text = rendered
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Resume a previous session"));
        assert!(text.contains("Type to search"));
        assert!(text.contains("Filter:"));
        assert!(text.contains("one prompt"));
        assert!(!text.contains("/workspace/nanocodex"));
    }

    #[test]
    fn prompt_preview_is_single_line_and_width_truncated() {
        assert_eq!(compact_prompt("  fix\n  the\tthing "), "fix the thing");
        assert_eq!(truncate_with_ellipsis("abcdefgh", 5), "abcd…");
        assert_eq!(truncate_with_ellipsis("界界界", 5), "界界…");
    }
}
