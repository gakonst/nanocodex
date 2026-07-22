use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use super::{
    app::{App, Conversation, PaneId},
    composer::ComposerLayout,
    transcript::InlineEdit,
};

pub(super) fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let composer_width = if app.historical_editor_active() {
        area.width.saturating_sub(4).max(1)
    } else {
        area.width.saturating_sub(2).max(1)
    };
    app.set_composer_width(composer_width);
    let composer_layout = ComposerLayout::new(&app.input, composer_width);
    let composer_height = if app.historical_editor_active() || app.branch_navigator_active() {
        3
    } else {
        composer_height(&composer_layout)
    };
    let cursor = composer_layout.cursor_position(&app.input, app.cursor);
    app.settle_composer_viewport(
        cursor.row,
        composer_layout.row_count(),
        usize::from(composer_height.saturating_sub(2)),
    );
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
    let mut selectable_areas = SelectableAreas::default();
    render_transcripts(frame, app, transcript_area, &mut selectable_areas);
    render_pending(frame, app, pending_area);
    selectable_areas.push(render_composer(frame, app, composer_area, &composer_layout));
    render_footer(frame, app, footer_area);
    app.render_mouse_selection(frame.buffer_mut(), selectable_areas.as_slice());
}

#[derive(Default)]
struct SelectableAreas {
    areas: [Rect; 3],
    count: usize,
}

impl SelectableAreas {
    fn push(&mut self, area: Rect) {
        if let Some(slot) = self.areas.get_mut(self.count) {
            *slot = area;
            self.count += 1;
        }
    }

    fn as_slice(&self) -> &[Rect] {
        &self.areas[..self.count]
    }
}

fn render_transcripts(
    frame: &mut Frame<'_>,
    app: &mut App,
    transcript_area: Rect,
    selectable_areas: &mut SelectableAreas,
) {
    let historical_editor_index = app.historical_editor_index();
    let inline_edit = historical_editor_index.map(|index| InlineEdit {
        index,
        input: app.input.as_str(),
        cursor: app.cursor,
    });
    if app.btw.is_some() {
        let [main_area, btw_area] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(transcript_area);
        let preserve_main = app.mouse_selection_intersects(main_area);
        let preserve_btw = app.mouse_selection_intersects(btw_area);
        if let Some(btw) = app.btw.as_mut() {
            selectable_areas.push(render_transcript(
                frame,
                &mut app.main,
                main_area,
                TranscriptRenderOptions {
                    title: " Main ",
                    focused: app.focus == PaneId::Main,
                    inline_edit,
                    empty_message:
                        "Ask Nanocodex to inspect, edit, run, or explain this workspace.",
                    preserve_view: preserve_main,
                },
            ));
            selectable_areas.push(render_transcript(
                frame,
                &mut btw.conversation,
                btw_area,
                TranscriptRenderOptions {
                    title: " BTW · forked context ",
                    focused: app.focus == PaneId::Btw(btw.id),
                    inline_edit: None,
                    empty_message: "Ask a quick side question without interrupting the main thread.",
                    preserve_view: preserve_btw,
                },
            ));
        }
    } else if app.branch_navigator_active() {
        let [main_area, navigator_area] =
            Layout::horizontal([Constraint::Percentage(68), Constraint::Percentage(32)])
                .areas(transcript_area);
        let selected = app.branch_navigator_selected_id().unwrap_or_default();
        let title = format!(" Branch {selected} preview ");
        {
            let conversation = app.branch_navigator_conversation_mut();
            selectable_areas.push(render_transcript(
                frame,
                conversation,
                main_area,
                TranscriptRenderOptions {
                    title: &title,
                    focused: true,
                    inline_edit: None,
                    empty_message:
                        "Ask Nanocodex to inspect, edit, run, or explain this workspace.",
                    preserve_view: false,
                },
            ));
        }
        render_branch_navigator(frame, app, navigator_area);
    } else {
        let preserve_main = app.mouse_selection_intersects(transcript_area);
        selectable_areas.push(render_transcript(
            frame,
            &mut app.main,
            transcript_area,
            TranscriptRenderOptions {
                title: " Main ",
                focused: true,
                inline_edit,
                empty_message: "Ask Nanocodex to inspect, edit, run, or explain this workspace.",
                preserve_view: preserve_main,
            },
        ));
    }
}

fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut spans = vec![
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
    ];
    let graph = app.main_branch_graph();
    if graph != "0*" {
        spans.push(Span::styled(
            format!("  branches {graph} · Ctrl+Alt+B browse · Ctrl+Alt+↑/↓ cycle"),
            Style::default().fg(Color::Yellow),
        ));
    }
    let title = Line::from(spans);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_branch_navigator(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = if app.main.running || app.main.pending_turns > 0 {
        " Branch tree · live preview; switch when idle "
    } else {
        " Branch tree · moving switches "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let previews = app.branch_previews();
    let capacity = (usize::from(inner.height) / 3).max(1);
    let selected = previews
        .iter()
        .position(|preview| preview.selected)
        .unwrap_or(0);
    let start = selected
        .saturating_sub(capacity / 2)
        .min(previews.len().saturating_sub(capacity));
    let mut lines = Vec::new();
    for preview in previews.iter().skip(start).take(capacity) {
        let active = if preview.active { " current" } else { "" };
        let marker = if preview.selected { "›" } else { " " };
        let node = if preview.active { "●" } else { "○" };
        let header_style = if preview.selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if preview.active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::styled(
            format!(
                "{marker} {}{node} branch {}{active}",
                preview.tree_prefix, preview.id
            ),
            header_style,
        ));
        lines.push(Line::styled(
            format!(
                "  {}{}",
                "  ".repeat(preview.depth),
                preview
                    .prompt
                    .map_or("(branch point)".to_owned(), prompt_preview)
            ),
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::raw(""));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[derive(Clone, Copy)]
struct TranscriptRenderOptions<'a> {
    title: &'a str,
    focused: bool,
    inline_edit: Option<InlineEdit<'a>>,
    empty_message: &'static str,
    preserve_view: bool,
}

fn render_transcript(
    frame: &mut Frame<'_>,
    conversation: &mut Conversation,
    area: Rect,
    options: TranscriptRenderOptions<'_>,
) -> Rect {
    let TranscriptRenderOptions {
        title,
        focused,
        inline_edit,
        empty_message,
        preserve_view,
    } = options;
    let title = if conversation.has_unseen_output {
        format!("{title}↓ New output · Ctrl+End ")
    } else {
        title.to_owned()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }));
    let inner = block.inner(area);
    conversation.settle_viewport_with_selection(inner.width, inner.height, preserve_view);
    let scroll_from_bottom = conversation.display_scroll_from_bottom();
    frame.render_widget(block, area);

    frame.render_widget(
        conversation.transcript.widget(
            scroll_from_bottom,
            conversation.selected_user,
            inline_edit,
            empty_message,
        ),
        inner,
    );
    if let Some(edit) = inline_edit
        && let Some(position) = conversation.transcript.inline_edit_cursor(
            inner,
            scroll_from_bottom,
            conversation.selected_user,
            edit,
        )
    {
        frame.set_cursor_position(position);
    }
    inner
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect, layout: &ComposerLayout) -> Rect {
    let conversation = app.active_conversation();
    let target = match app.focus {
        PaneId::Main => "Main",
        PaneId::Btw(_) => "BTW",
    };
    let title = if app.historical_editor_active() {
        " Message composer · editing history inline above ".to_owned()
    } else if app.branch_navigator_active() {
        " Message composer · browsing branches ".to_owned()
    } else if conversation.running {
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
    if app.historical_editor_active() || app.branch_navigator_active() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                " draft preserved ",
                Style::default().fg(Color::DarkGray),
            )),
            inner,
        );
        return inner;
    }
    let cursor = layout.cursor_position(&app.input, app.cursor);
    let vertical_scroll = app.composer_scroll();
    let visible_end = vertical_scroll.saturating_add(usize::from(inner.height));
    let lines = (vertical_scroll..visible_end)
        .filter_map(|row| layout.row(row))
        .map(|range| Line::raw(&app.input[range.clone()]))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);

    if app.transcript_selection_active() || app.branch_navigator_active() {
        return inner;
    }

    let x = inner
        .x
        .saturating_add(saturating_u16(cursor.column).min(inner.width.saturating_sub(1)));
    let y = inner
        .y
        .saturating_add(saturating_u16(cursor.row.saturating_sub(vertical_scroll)));
    frame.set_cursor_position(Position::new(x, y));
    inner
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let conversation = app.active_conversation();
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let state = if app.branch_navigator_active() {
        "Branches — ↑/↓ or j/k switch + preview · Esc close".to_owned()
    } else if app.historical_editor_active() {
        "Editing history — Enter fork/send · Shift+Enter newline · Esc cancel · Ctrl+G $EDITOR"
            .to_owned()
    } else if app.transcript_selection_active() {
        "History — ↑/↓ navigate · e fork-edit · Esc return".to_owned()
    } else if app.cancel_confirmation_active() {
        "Stop Agent Turn — Esc again to confirm".to_owned()
    } else if conversation.running {
        format!("{} Thinking...", spinner[app.frame % spinner.len()])
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
        "  BackTab switch · Ctrl+V image · /close dismiss · Enter send/steer · Tab queue · Esc Esc stop · Ctrl+C quit"
    } else {
        "  /btw <question> side fork · Ctrl+V image · Enter send/steer · Tab queue · Esc Esc stop · Ctrl+C quit"
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

fn composer_height(layout: &ComposerLayout) -> u16 {
    saturating_u16(layout.row_count())
        .clamp(1, 7)
        .saturating_add(2)
}

fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use std::io;

    use ratatui::{
        Terminal,
        backend::{Backend, ClearType, TestBackend, WindowSize},
        buffer::Cell,
        layout::{Position, Rect, Size},
        style::Color,
    };

    use super::render;
    use crate::tui::{app::App, transcript::TranscriptItem};

    #[test]
    fn btw_renders_as_a_side_by_side_focused_pane() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into());
        app.begin_btw();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Main"));
        assert!(rendered.contains("BTW · forked context"));
        assert!(rendered.contains("Message → BTW"));
        assert!(rendered.contains("BackTab switch"));
    }

    #[test]
    fn active_turn_renders_steers_separately_from_queued_follow_ups() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into());
        app.main.running = true;
        let steer_id = app
            .queue_steer(
                crate::tui::app::PaneId::Main,
                "use the database implementation".to_owned(),
            )
            .unwrap();
        app.steer_admitted(crate::tui::app::PaneId::Main, steer_id);
        assert!(
            app.queue_prompt(
                crate::tui::app::PaneId::Main,
                "write a final benchmark summary".to_owned()
            )
            .is_some()
        );

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Enter steers · Tab queues"));
        assert!(rendered.contains("Pending input"));
        assert!(rendered.contains("↳ steer"));
        assert!(rendered.contains("use the database implementation"));
        assert!(rendered.contains("⏳ queued"));
        assert!(rendered.contains("write a final benchmark summary"));
    }

    #[test]
    fn running_footer_uses_one_fixed_thinking_label() {
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        let mut app = App::new("/workspace".into());
        app.main.running = true;
        app.main.status = "Running exec_command".to_owned();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Thinking..."));
        assert!(!rendered.contains("Running exec_command"));
    }

    #[test]
    fn mouse_selection_copies_composer_and_transcript_text() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let mut app = App::new("/workspace".into());
        app.input = "copy composer".to_owned();
        app.cursor = app.input.len();
        app.main
            .transcript
            .push(TranscriptItem::User("transcript copy".to_owned()));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert!(app.begin_mouse_selection((1, 9).into()));
        assert!(app.finish_mouse_selection((13, 9).into()));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.take_pending_copy().as_deref(), Some("copy composer"));
        assert_eq!(
            terminal.backend().buffer().cell((1, 9)).unwrap().bg,
            Color::LightBlue
        );

        let _ = app.clear_mouse_selection();
        assert!(app.begin_mouse_selection((3, 3).into()));
        assert!(app.finish_mouse_selection((17, 3).into()));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.take_pending_copy().as_deref(), Some("transcript copy"));
    }

    #[test]
    fn selecting_history_during_a_running_response_keeps_transcript_context_visible() {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("active prompt".to_owned(), 1);
        app.main.push_assistant_delta(
            "streaming answer\nline two\nline three\nline four\nline five\nline six",
        );
        app.main.running = true;
        app.move_up();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("active prompt"));
        assert!(rendered.contains("streaming answer"));
        assert!(rendered.contains("line six"));
    }

    #[test]
    fn branch_navigator_renders_prompt_previews_beside_the_transcript() {
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let mut app = App::new("/workspace".into());
        app.main
            .transcript
            .push_editable_user("root branch prompt".to_owned(), 1);
        app.move_up();
        assert!(app.start_historical_edit());
        app.replace_input("revised branch prompt".to_owned());
        let request = app.commit_historical_edit().unwrap();
        let _ = app.main_branch_opened(
            request.new_branch,
            request.source_branch,
            request.prompt,
            std::sync::Arc::from("branch-session"),
        );
        app.main
            .transcript
            .push_editable_user("revised branch prompt".to_owned(), 2);
        assert!(app.toggle_branch_navigator());
        app.move_branch_navigator(-1);

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Branch tree · moving switches"));
        assert!(rendered.contains("Branch 0 preview"));
        assert!(rendered.contains("root branch prompt"));
        assert!(rendered.contains("revised branch prompt"));
        assert!(rendered.contains("› ○ branch 0"));
        assert!(rendered.contains("└─● branch 1 current"));
    }

    #[test]
    fn btw_focus_keeps_main_steers_visible_in_their_own_pending_pane() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into());
        app.main.running = true;
        let btw_id = app.begin_btw();
        let steer_id = app
            .queue_steer(
                crate::tui::app::PaneId::Main,
                "main correction remains visible".to_owned(),
            )
            .unwrap();
        app.steer_admitted(crate::tui::app::PaneId::Main, steer_id);
        assert!(
            app.queue_prompt(
                crate::tui::app::PaneId::Btw(btw_id),
                "queued BTW follow-up".to_owned(),
            )
            .is_some()
        );

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Main pending input"));
        assert!(rendered.contains("BTW pending input"));
        assert!(rendered.contains("↳ steer"));
        assert!(rendered.contains("main correction remains visible"));
        assert!(rendered.contains("⏳ queued"));
        assert!(rendered.contains("queued BTW follow-up"));
    }

    #[test]
    fn unseen_output_is_indicated_only_on_its_conversation() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut app = App::new("/workspace".into());
        let btw_id = app.begin_btw();
        app.main.has_unseen_output = true;

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Main ↓ New output · Ctrl+End"));
        assert!(!rendered.contains("BTW · forked context ↓ New output"));
        assert_eq!(app.focus, crate::tui::app::PaneId::Btw(btw_id));
    }

    #[test]
    fn empty_main_layout_snapshot() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let mut app = App::new("/workspace".into());

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

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
                "\" Ready  /btw <question> side fork · Ctrl+V image\"\n",
            )
        );
    }

    #[test]
    fn cursor_tracks_multiline_unicode_input_exactly() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let mut app = App::new("/workspace".into());
        app.input = "ab\n界c".to_owned();
        app.cursor = app.input.len();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 9));
    }

    #[test]
    fn cursor_at_an_exact_wrap_boundary_uses_the_next_visual_row() {
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
        let mut app = App::new("/workspace".into());
        app.input = "123456789012345678".to_owned();
        app.cursor = app.input.len();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(1, 7));
    }

    #[test]
    fn multiline_cursor_moves_before_the_viewport_scrolls() {
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        let mut app = App::new("/workspace".into());
        app.input = (0..10)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        app.cursor = app.input.len();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let bottom = terminal.get_cursor_position().unwrap();
        app.move_up();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert_eq!(terminal.get_cursor_position().unwrap().y, bottom.y - 1);
        assert_eq!(app.composer_scroll(), 3);

        for _ in 0..5 {
            app.move_up();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
        }
        let top = terminal.get_cursor_position().unwrap().y;
        assert_eq!(app.composer_scroll(), 3);

        app.move_up();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap().y, top);
        assert_eq!(app.composer_scroll(), 2);
    }

    #[test]
    fn resize_reflows_layout_and_repositions_cursor() {
        let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
        let mut app = App::new("/workspace".into());
        app.input = "abc".to_owned();
        app.cursor = app.input.len();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        terminal.backend_mut().resize(32, 10);
        terminal.autoresize().unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert_eq!(terminal.backend().buffer().area, Rect::new(0, 0, 32, 10));
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 7));
    }

    #[test]
    fn ratatui_draws_only_changed_cells_after_the_first_frame() {
        let backend = CountingBackend::new(48, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new("/workspace".into());

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(terminal.backend().draw_counts[0] > 0);

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(terminal.backend().draw_counts[1], 0);

        app.input.push('x');
        app.cursor = app.input.len();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
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
