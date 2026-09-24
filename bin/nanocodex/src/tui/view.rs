use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::time::Instant;

use super::{
    app::{App, Conversation, MODEL_OPTIONS, PaneId, ReasoningPicker, STANDARD_THINKING_OPTIONS},
    composer::ComposerLayout,
    transcript::InlineEdit,
};

pub(super) fn render(frame: &mut Frame<'_>, app: &mut App) {
    let layout = view_layout(frame.area(), app);

    render_header(frame, app, layout.header);
    let mut selectable_areas = SelectableAreas::default();
    render_transcripts(frame, app, layout.transcript, &mut selectable_areas);
    render_pending(frame, app, layout.pending);
    selectable_areas.push(render_composer(
        frame,
        app,
        layout.composer,
        &layout.composer_layout,
    ));
    super::voice::render(frame, &app.voice, layout.voice);
    render_footer(frame, app, layout.footer);
    app.render_mouse_selection(frame.buffer_mut(), selectable_areas.as_slice());
    render_model_picker(frame, app);
    render_reasoning_picker(frame, app);
}

pub(super) fn render_animation(frame: &mut Frame<'_>, app: &mut App) {
    let layout = view_layout(frame.area(), app);
    render_composer(frame, app, layout.composer, &layout.composer_layout);
    super::voice::render(frame, &app.voice, layout.voice);
    render_footer(frame, app, layout.footer);
    render_model_picker(frame, app);
    render_reasoning_picker(frame, app);
}

fn render_model_picker(frame: &mut Frame<'_>, app: &App) {
    let Some(selected) = app.model_picker() else {
        return;
    };
    let area = frame.area();
    let popup_height = 9.min(area.height);
    let popup_width = area.width.min(64);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height),
        popup_width,
        popup_height,
    );
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::styled(
            "  Select Model",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::default(),
    ];
    for (index, (model, label)) in MODEL_OPTIONS.iter().enumerate() {
        let current = if *model == app.model() {
            " (current)"
        } else {
            ""
        };
        lines.push(reasoning_option_line(
            index == selected,
            index + 1,
            &format!("{label}{current}"),
            model.as_str(),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "  Press enter to confirm or esc to cancel",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(lines), popup);
}

struct ViewLayout {
    header: Rect,
    transcript: Rect,
    pending: Rect,
    composer: Rect,
    voice: Rect,
    footer: Rect,
    composer_layout: ComposerLayout,
}

fn view_layout(area: Rect, app: &mut App) -> ViewLayout {
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
        voice_area,
        composer_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(pending_height),
        Constraint::Length(if app.voice.visible() { 3 } else { 0 }),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area);

    ViewLayout {
        header: header_area,
        transcript: transcript_area,
        pending: pending_area,
        voice: voice_area,
        composer: composer_area,
        footer: footer_area,
        composer_layout,
    }
}

fn render_reasoning_picker(frame: &mut Frame<'_>, app: &App) {
    let Some(picker) = app.reasoning_picker() else {
        return;
    };
    let area = frame.area();
    let popup_height = match picker {
        ReasoningPicker::Standard { .. } => 9,
        ReasoningPicker::Advanced => 7,
    }
    .min(area.height);
    let popup_width = area.width.min(80);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(popup_width) / 2,
        area.y + area.height.saturating_sub(popup_height),
        popup_width,
        popup_height,
    );
    frame.render_widget(Clear, popup);

    let mut lines = Vec::new();
    match picker {
        ReasoningPicker::Standard { selected } => {
            lines.push(Line::styled(
                format!("  Select Reasoning Level for {}", app.model()),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::default());
            for (index, (thinking, label, description)) in
                STANDARD_THINKING_OPTIONS.iter().enumerate()
            {
                let mut label = (*label).to_owned();
                if *thinking == app.model().default_thinking() {
                    label.push_str(" (default)");
                }
                if *thinking == app.thinking() {
                    label.push_str(" (current)");
                }
                lines.push(reasoning_option_line(
                    index == selected,
                    index + 1,
                    &label,
                    description,
                ));
            }
            lines.push(reasoning_option_line(
                selected == STANDARD_THINKING_OPTIONS.len(),
                STANDARD_THINKING_OPTIONS.len() + 1,
                "More reasoning…",
                "Max consumes usage limits faster",
            ));
        }
        ReasoningPicker::Advanced => {
            lines.push(Line::styled(
                "  Advanced Reasoning",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                "  ⚠ Consumes usage limits faster",
                Style::default().fg(Color::Cyan),
            ));
            lines.push(Line::default());
            let label = if app.thinking() == nanocodex::Thinking::Max {
                "Max (current)"
            } else {
                "Max"
            };
            lines.push(reasoning_option_line(
                true,
                1,
                label,
                "For difficult problems when quality matters more than speed · higher usage",
            ));
        }
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "  Press enter to confirm or esc to go back",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(lines), popup);
}

fn reasoning_option_line(
    selected: bool,
    number: usize,
    label: &str,
    description: &str,
) -> Line<'static> {
    let marker = if selected { "›" } else { " " };
    let style = if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    Line::styled(
        format!("{marker} {number}. {label:<19} {description}"),
        style,
    )
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
        conversation
            .transcript
            .widget(
                scroll_from_bottom,
                conversation.selected_user,
                inline_edit,
                empty_message,
            )
            .math_fallback(preserve_view),
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
        format!(
            " Draft parked · editing branch {} message above ",
            app.historical_editor_source_branch().unwrap_or_default()
        )
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
        let branch = app.historical_editor_source_branch().unwrap_or_default();
        if app.main.running || app.main.pending_turns > 0 {
            format!(
                "Editing branch {branch} — Enter stops live turn + forks · Shift+Enter newline · Esc cancel"
            )
        } else {
            format!(
                "Editing branch {branch} — Enter forks here · Shift+Enter newline · Esc cancel · Ctrl+G $EDITOR"
            )
        }
    } else if app.transcript_selection_active() {
        "History — ↑/↓ select · e edit/fork · Esc return".to_owned()
    } else if app.cancel_confirmation_active() {
        "Stop Agent Turn — Esc again to confirm".to_owned()
    } else if conversation.running {
        format!(
            "{} Working ({})",
            spinner[app.frame % spinner.len()],
            format_elapsed(conversation.run_elapsed(Instant::now()))
        )
    } else {
        conversation.status.clone()
    };
    let queued = conversation
        .pending_turns
        .saturating_sub(usize::from(conversation.running));
    let steers = conversation.pending_steers.len();
    let queue = footer_queue(steers, queued);
    let cost = conversation
        .last_cost_usd
        .as_deref()
        .map_or_else(String::new, |usd| format!(" · ${usd}"));
    let escape_help = if steers == 0 {
        "Esc Esc stop"
    } else {
        "Esc interrupt/send"
    };
    let tool_help = if app.tool_details_expanded() {
        "Ctrl+O fold tools"
    } else {
        "Ctrl+O expand tools"
    };
    let help = if app.btw.is_some() {
        format!(
            "  BackTab switch · /collapse merge · /split detach · /close dismiss · {tool_help} · Ctrl+V image · Enter send/steer · Tab queue · {escape_help} · Ctrl+C quit"
        )
    } else {
        format!(
            "  /simplify [focus] cleanup · /btw <question> side fork · /voice [voice] · {tool_help} · Ctrl+V image · Enter send/steer · Tab queue · {escape_help} · Ctrl+C quit"
        )
    };
    let model_width = app.model().as_str().len() + 3 + "default".len() + 7 + 1;
    let model_width = saturating_u16(model_width).min(area.width);
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(model_width)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {state}{cost}{queue}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(help, Style::default().fg(Color::DarkGray)),
        ])),
        left,
    );
    let mut model = vec![Span::styled(
        app.model().as_str(),
        Style::default().fg(Color::Cyan),
    )];
    let thinking = if app.thinking() == nanocodex::Thinking::None {
        "default"
    } else {
        app.thinking().as_str()
    };
    model.push(Span::styled(
        format!(" · {thinking}"),
        Style::default().fg(Color::DarkGray),
    ));
    if app.fast_mode() {
        model.push(Span::styled(
            " · fast",
            Style::default().fg(Color::LightYellow),
        ));
    }
    model.push(Span::raw(" "));
    frame.render_widget(
        Paragraph::new(Line::from(model)).alignment(Alignment::Right),
        right,
    );
}

fn footer_queue(steers: usize, queued: usize) -> String {
    match (steers, queued) {
        (0, 0) => String::new(),
        (0, queued) => format!(" · {queued} queued"),
        (1, 0) => " · 1 steer".to_owned(),
        (steers, 0) => format!(" · {steers} steers"),
        (1, queued) => format!(" · 1 steer · {queued} queued"),
        (steers, queued) => format!(" · {steers} steers · {queued} queued"),
    }
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        return format!("{}m {:02}s", seconds / 60, seconds % 60);
    }
    format!(
        "{}h {:02}m {:02}s",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
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
