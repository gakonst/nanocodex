use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, Conversation, PaneId};

pub(super) fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let composer_height = composer_height(&app.input, area.width.saturating_sub(4));
    let pending_height = pending_height(app);
    let [
        header_area,
        transcript_area,
        pending_area,
        composer_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(pending_height),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, app, header_area);
    if let Some(btw) = &app.btw {
        let [main_area, btw_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(transcript_area);
        render_transcript(
            frame,
            &app.main,
            main_area,
            " Main ",
            app.focus == PaneId::Main,
            "Ask Nanocodex to inspect, edit, run, or explain this workspace.",
        );
        render_transcript(
            frame,
            &btw.conversation,
            btw_area,
            " BTW · forked context ",
            app.focus == PaneId::Btw(btw.id),
            "Ask a quick side question without interrupting the main thread.",
        );
    } else {
        render_transcript(
            frame,
            &app.main,
            transcript_area,
            " Main ",
            true,
            "Ask Nanocodex to inspect, edit, run, or explain this workspace.",
        );
    }
    render_pending(frame, app, pending_area);
    render_composer(frame, app, composer_area);
    render_footer(frame, app, footer_area);
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " nanocodex ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            app.cwd.display().to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_transcript(
    frame: &mut Frame<'_>,
    conversation: &Conversation,
    area: Rect,
    title: &'static str,
    focused: bool,
    empty_message: &'static str,
) {
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    frame.render_widget(
        conversation
            .transcript
            .widget(conversation.scroll_from_bottom, empty_message),
        inner,
    );
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let conversation = app.active_conversation();
    let target = match app.focus {
        PaneId::Main => "Main",
        PaneId::Btw(_) => "BTW",
    };
    let title = if conversation.running {
        format!(" Message → {target} (Enter steers · Tab queues) ")
    } else {
        format!(" Message → {target} ")
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if conversation.running {
            Color::Yellow
        } else {
            Color::Cyan
        }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (cursor_row, cursor_column) = composer_cursor(app, inner.width.max(1));
    let vertical_scroll = cursor_row.saturating_sub(inner.height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(app.input.as_str())
            .wrap(Wrap { trim: false })
            .scroll((vertical_scroll, 0)),
        inner,
    );

    let x = inner
        .x
        .saturating_add(cursor_column.min(inner.width.saturating_sub(1)));
    let y = inner
        .y
        .saturating_add(cursor_row.saturating_sub(vertical_scroll));
    frame.set_cursor_position(Position::new(x, y));
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let conversation = app.active_conversation();
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let state = if app.cancel_confirmation_active() {
        "Stop Agent Turn — Esc again to confirm".to_owned()
    } else if conversation.running {
        format!(
            "{} {}",
            spinner[app.frame % spinner.len()],
            conversation.status
        )
    } else {
        conversation.status.clone()
    };
    let queued = conversation
        .pending_turns
        .saturating_sub(usize::from(conversation.running));
    let steers = conversation.pending_steers.len();
    let queue = if queued == 0 && steers == 0 {
        String::new()
    } else {
        match (steers, queued) {
            (0, queued) => format!(" · {queued} queued"),
            (1, 0) => " · 1 steer".to_owned(),
            (steers, 0) => format!(" · {steers} steers"),
            (1, queued) => format!(" · 1 steer · {queued} queued"),
            (steers, queued) => format!(" · {steers} steers · {queued} queued"),
        }
    };
    let help = if app.btw.is_some() {
        format!(
            "  Thinking: {} · BackTab switch · /trace inspect · /close dismiss · Enter send/steer · Tab queue · Esc×2 stop · Ctrl+C quit",
            app.thinking
        )
    } else {
        format!(
            "  Thinking: {} · /btw <question> side fork · /trace inspect · Enter send/steer · Tab queue · Esc×2 stop · Ctrl+C quit",
            app.thinking
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {state}{queue}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(help, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn conversation_pending_count(conversation: &Conversation) -> usize {
    conversation.pending_steers.len() + conversation.queued_prompts.len()
}

fn pending_height(app: &App) -> u16 {
    let main_count = conversation_pending_count(&app.main);
    let count = app.btw.as_ref().map_or(main_count, |btw| {
        main_count.max(conversation_pending_count(&btw.conversation))
    });
    if count == 0 {
        0
    } else {
        saturating_u16(count.min(3) + 2)
    }
}

fn render_pending(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }

    if let Some(btw) = &app.btw {
        let [main_area, btw_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(area);
        render_conversation_pending(
            frame,
            &app.main,
            main_area,
            " Main pending input ",
            app.focus == PaneId::Main,
        );
        render_conversation_pending(
            frame,
            &btw.conversation,
            btw_area,
            " BTW pending input ",
            app.focus == PaneId::Btw(btw.id),
        );
    } else {
        render_conversation_pending(frame, &app.main, area, " Pending input ", true);
    }
}

fn render_conversation_pending(
    frame: &mut Frame<'_>,
    conversation: &Conversation,
    area: Rect,
    title: &'static str,
    focused: bool,
) {
    let mut lines = Vec::new();
    for steer in &conversation.pending_steers {
        let (label, color) = if steer.is_admitted() {
            ("↳ steer   ", Color::Yellow)
        } else {
            ("… steer   ", Color::DarkGray)
        };
        lines.push(Line::from(vec![
            Span::styled(label, Style::default().fg(color)),
            Span::raw(prompt_preview(steer.prompt())),
        ]));
    }
    for prompt in &conversation.queued_prompts {
        lines.push(Line::from(vec![
            Span::styled("⏳ queued ", Style::default().fg(Color::DarkGray)),
            Span::raw(prompt_preview(prompt)),
        ]));
    }
    lines.truncate(3);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn prompt_preview(prompt: &str) -> String {
    const MAX_CHARS: usize = 96;
    let mut preview = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.chars().count() > MAX_CHARS {
        preview = preview.chars().take(MAX_CHARS - 1).collect();
        preview.push('…');
    }
    preview
}

fn composer_height(input: &str, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let rows = input
        .split('\n')
        .map(|line| UnicodeWidthStr::width(line).div_ceil(width).max(1))
        .sum::<usize>();
    saturating_u16(rows).clamp(1, 7).saturating_add(2)
}

fn composer_cursor(app: &App, width: u16) -> (u16, u16) {
    let width = usize::from(width.max(1));
    let before = &app.input[..app.cursor];
    let mut row = 0_usize;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let columns = UnicodeWidthStr::width(line);
        if lines.peek().is_some() {
            row = row.saturating_add(columns / width + 1);
        } else {
            row = row.saturating_add(columns / width);
            return (saturating_u16(row), saturating_u16(columns % width));
        }
    }
    (0, 0)
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use std::io;

    use nanocodex::Thinking;

    use ratatui::{
        Terminal,
        backend::{Backend, ClearType, TestBackend, WindowSize},
        buffer::Cell,
        layout::{Position, Rect, Size},
    };

    use super::render;
    use crate::tui::app::App;

    #[test]
    fn btw_renders_as_a_side_by_side_focused_pane() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.begin_btw();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Main"));
        assert!(rendered.contains("BTW · forked context"));
        assert!(rendered.contains("Message → BTW"));
        assert!(rendered.contains("BackTab switch"));
    }

    #[test]
    fn active_turn_renders_steers_separately_from_queued_follow_ups() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.main.running = true;
        let steer_id = app
            .queue_steer(
                crate::tui::app::PaneId::Main,
                "use the database implementation".to_owned(),
            )
            .unwrap();
        app.steer_admitted(crate::tui::app::PaneId::Main, steer_id);
        assert!(app.queue_prompt(
            crate::tui::app::PaneId::Main,
            "write a final benchmark summary".to_owned()
        ));

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Enter steers · Tab queues"));
        assert!(rendered.contains("Pending input"));
        assert!(rendered.contains("↳ steer"));
        assert!(rendered.contains("use the database implementation"));
        assert!(rendered.contains("⏳ queued"));
        assert!(rendered.contains("write a final benchmark summary"));
    }

    #[test]
    fn btw_focus_keeps_main_steers_visible_in_their_own_pending_pane() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.main.running = true;
        let btw_id = app.begin_btw();
        let steer_id = app
            .queue_steer(
                crate::tui::app::PaneId::Main,
                "main correction remains visible".to_owned(),
            )
            .unwrap();
        app.steer_admitted(crate::tui::app::PaneId::Main, steer_id);
        assert!(app.queue_prompt(
            crate::tui::app::PaneId::Btw(btw_id),
            "queued BTW follow-up".to_owned(),
        ));

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Main pending input"));
        assert!(rendered.contains("BTW pending input"));
        assert!(rendered.contains("↳ steer"));
        assert!(rendered.contains("main correction remains visible"));
        assert!(rendered.contains("⏳ queued"));
        assert!(rendered.contains("queued BTW follow-up"));
    }

    #[test]
    fn empty_main_layout_snapshot() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let app = App::new("/workspace".into(), Thinking::Medium);

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().to_string(),
            concat!(
                "\" nanocodex   /workspace                         \"\n",
                "\"┌ Main ────────────────────────────────────────┐\"\n",
                "\"│                                              │\"\n",
                "\"│  Ask Nanocodex to inspect, edit, run, or     │\"\n",
                "\"│explain this workspace.                       │\"\n",
                "\"│                                              │\"\n",
                "\"│                                              │\"\n",
                "\"└──────────────────────────────────────────────┘\"\n",
                "\"┌ Message → Main ──────────────────────────────┐\"\n",
                "\"│                                              │\"\n",
                "\"└──────────────────────────────────────────────┘\"\n",
                "\" Ready  Thinking: medium · /btw <question> side \"\n",
            )
        );
    }

    #[test]
    fn footer_renders_the_active_thinking_level_with_shortcuts() {
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        let app = App::new("/workspace".into(), Thinking::Xhigh);

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Thinking: xhigh · /btw <question> side fork"));
        assert!(rendered.contains("Enter send/steer · Tab queue"));
    }

    #[test]
    fn cursor_tracks_multiline_unicode_input_exactly() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.input = "ab\n界c".to_owned();
        app.cursor = app.input.len();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 9));
    }

    #[test]
    fn resize_reflows_layout_and_repositions_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let mut app = App::new("/workspace".into(), Thinking::Medium);
        app.input = "abc".to_owned();
        app.cursor = app.input.len();
        terminal.draw(|frame| render(frame, &app)).unwrap();

        terminal.backend_mut().resize(32, 10);
        terminal.autoresize().unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(terminal.backend().buffer().area, Rect::new(0, 0, 32, 10));
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 7));
    }

    #[test]
    fn ratatui_draws_only_changed_cells_after_the_first_frame() {
        let backend = CountingBackend::new(48, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("/workspace".into(), Thinking::Medium);

        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(terminal.backend().draw_counts[0] > 0);

        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(terminal.backend().draw_counts[1], 0);

        app.input.push('x');
        app.cursor = app.input.len();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(terminal.backend().draw_counts[2], 1);
    }

    struct CountingBackend {
        inner: TestBackend,
        draw_counts: Vec<usize>,
    }

    impl CountingBackend {
        fn new(width: u16, height: u16) -> Self {
            Self {
                inner: TestBackend::new(width, height),
                draw_counts: Vec::new(),
            }
        }
    }

    impl Backend for CountingBackend {
        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            let content = content.collect::<Vec<_>>();
            self.draw_counts.push(content.len());
            self.inner.draw(content.into_iter())
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.inner.hide_cursor()
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.inner.show_cursor()
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            self.inner.get_cursor_position()
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            self.inner.set_cursor_position(position)
        }

        fn clear(&mut self) -> io::Result<()> {
            self.inner.clear()
        }

        fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
            self.inner.clear_region(clear_type)
        }

        fn size(&self) -> io::Result<Size> {
            self.inner.size()
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            self.inner.window_size()
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }
}
