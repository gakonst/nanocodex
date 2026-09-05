// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Collapsible presentation for one directed-message thread.

use super::markdown::{sanitize, wrap_plain, wrap_spans};
use crate::tui::{theme::Theme, transcript::DirectedMessageEntry};
use nanocodex_subagents::{
    AgentId, AgentMessage, MessageDeliveryState, MessageDisposition, MessagePriority,
    MessagePurpose, MessageSender,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(super) fn render(
    entry: &DirectedMessageEntry,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let mut lines = render_summary(entry, width, theme, expanded);
    if !expanded {
        return lines;
    }

    append_messages(&mut lines, entry, width, theme);
    lines
}

fn render_summary(
    entry: &DirectedMessageEntry,
    width: u16,
    theme: &Theme,
    expanded: bool,
) -> Vec<Line<'static>> {
    let indicator = if expanded { "▼ " } else { "▶ " };
    let Some(latest) = entry.thread.messages.last() else {
        return wrap_summary(
            vec![
                Span::raw("  "),
                Span::styled(indicator, Style::default().fg(theme.border())),
                Span::raw("  "),
            ],
            vec![Span::styled(
                format!("Message thread #{}", entry.thread.id),
                Style::default()
                    .fg(theme.text())
                    .add_modifier(Modifier::BOLD),
            )],
            width,
        );
    };

    let direction = direction(latest, entry.perspective);
    let route = route(latest, entry.perspective);
    let (delivery, delivery_style) = entry.delivery(latest.id).map_or_else(
        || ("pending".to_owned(), Style::default().fg(theme.muted())),
        |state| delivery_label(state, theme),
    );
    let count = entry.thread.messages.len();
    let count = (count > 1).then(|| format!(" · {count} messages"));
    let body = summary_body(&latest.body);
    let prefix = vec![
        Span::raw("  "),
        Span::styled(indicator, Style::default().fg(theme.border())),
        Span::styled(
            format!("{direction} "),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let content = vec![
        Span::styled(
            "Message",
            Style::default()
                .fg(theme.text())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {route}"), Style::default().fg(theme.text())),
        Span::styled(
            format!(" · {}", purpose_label(latest.purpose)),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(format!(" · {delivery}"), delivery_style),
        Span::styled(
            count.unwrap_or_default(),
            Style::default().fg(theme.muted()),
        ),
        Span::styled(format!(" · {body}"), Style::default().fg(theme.muted())),
    ];
    wrap_summary(prefix, content, width)
}

fn wrap_summary(
    prefix: Vec<Span<'static>>,
    content: Vec<Span<'static>>,
    width: u16,
) -> Vec<Line<'static>> {
    const PREFIX_WIDTH: u16 = 6;
    if width <= PREFIX_WIDTH {
        return wrap_spans(
            &prefix.into_iter().chain(content).collect::<Vec<_>>(),
            width,
            true,
        );
    }

    let mut lines = wrap_spans(&content, width - PREFIX_WIDTH, true);
    for (index, line) in lines.iter_mut().enumerate() {
        let line_prefix = if index == 0 {
            prefix.clone()
        } else {
            vec![Span::raw("      ")]
        };
        line.spans.splice(0..0, line_prefix);
    }
    lines
}

fn append_messages(
    lines: &mut Vec<Line<'static>>,
    entry: &DirectedMessageEntry,
    width: u16,
    theme: &Theme,
) {
    let detail_width = width.saturating_sub(6).max(1);
    for message in &entry.thread.messages {
        let (delivery, delivery_style) = entry.delivery(message.id).map_or_else(
            || ("pending".to_owned(), Style::default().fg(theme.muted())),
            |state| delivery_label(state, theme),
        );
        let header = vec![
            Span::styled(
                route(message, entry.perspective),
                Style::default()
                    .fg(theme.text())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    " · {} · {} · ",
                    purpose_label(message.purpose),
                    priority_label(message.priority),
                ),
                Style::default().fg(theme.muted()),
            ),
            Span::styled(delivery, delivery_style),
        ];
        append_railed(lines, wrap_spans(&header, detail_width, true), width, theme);
        append_railed(
            lines,
            wrap_plain(
                &message.body,
                detail_width,
                Style::default().fg(theme.text()),
            ),
            width,
            theme,
        );
    }

    let footer = format!(
        "thread #{} · {} {}",
        entry.thread.id,
        entry.thread.messages.len(),
        if entry.thread.messages.len() == 1 {
            "message"
        } else {
            "messages"
        }
    );
    append_footer(lines, &footer, width, theme);
}

fn append_railed(
    destination: &mut Vec<Line<'static>>,
    source: Vec<Line<'static>>,
    width: u16,
    theme: &Theme,
) {
    if width < 7 {
        destination.extend(source);
        return;
    }
    destination.extend(source.into_iter().map(|line| {
        Line::from(
            std::iter::once(Span::styled("    │ ", Style::default().fg(theme.border())))
                .chain(line.spans)
                .collect::<Vec<_>>(),
        )
    }));
}

fn append_footer(lines: &mut Vec<Line<'static>>, footer: &str, width: u16, theme: &Theme) {
    if width < 7 {
        lines.extend(wrap_plain(
            footer,
            width,
            Style::default().fg(theme.muted()),
        ));
        return;
    }

    let footer = wrap_plain(footer, width - 6, Style::default().fg(theme.muted()));
    lines.extend(footer.into_iter().enumerate().map(|(index, line)| {
        let prefix = if index == 0 { "    └ " } else { "      " };
        Line::from(
            std::iter::once(Span::styled(prefix, Style::default().fg(theme.border())))
                .chain(line.spans)
                .collect::<Vec<_>>(),
        )
    }));
}

fn direction(message: &AgentMessage, perspective: MessageSender) -> &'static str {
    if message.from == perspective {
        "→"
    } else {
        "←"
    }
}

fn route(message: &AgentMessage, perspective: MessageSender) -> String {
    format!(
        "{} → {}",
        sender_label(message.from, perspective),
        agent_label(message.to, perspective)
    )
}

fn sender_label(sender: MessageSender, perspective: MessageSender) -> String {
    if sender == perspective {
        return "you".to_owned();
    }
    match sender {
        MessageSender::Root => "root".to_owned(),
        MessageSender::Agent { agent_id } => agent_label(agent_id, perspective),
    }
}

fn agent_label(agent_id: AgentId, perspective: MessageSender) -> String {
    if perspective == (MessageSender::Agent { agent_id }) {
        return "you".to_owned();
    }
    format!("#{agent_id}")
}

fn delivery_label(state: &MessageDeliveryState, theme: &Theme) -> (String, Style) {
    match state {
        MessageDeliveryState::Admitted { disposition } => (
            format!("admitted · {}", disposition_label(*disposition)),
            Style::default().fg(theme.accent()),
        ),
        MessageDeliveryState::Delivered { disposition } => (
            format!("delivered · {}", disposition_label(*disposition)),
            Style::default().fg(ratatui::style::Color::Green),
        ),
        MessageDeliveryState::Failed { error } => (
            format!("failed · {}", first_line(error)),
            Style::default().fg(theme.thinking_xhigh()),
        ),
    }
}

const fn disposition_label(disposition: MessageDisposition) -> &'static str {
    match disposition {
        MessageDisposition::Started => "started",
        MessageDisposition::Queued => "queued",
        MessageDisposition::Steered => "steered",
    }
}

const fn purpose_label(purpose: MessagePurpose) -> &'static str {
    match purpose {
        MessagePurpose::Delegate => "delegate",
        MessagePurpose::Coordinate => "coordinate",
        MessagePurpose::Finding => "finding",
        MessagePurpose::Question => "question",
        MessagePurpose::Reply => "reply",
    }
}

const fn priority_label(priority: MessagePriority) -> &'static str {
    match priority {
        MessagePriority::Deferred => "deferred",
        MessagePriority::Urgent => "urgent",
    }
}

fn summary_body(body: &str) -> String {
    const LIMIT: usize = 72;
    let body = first_line(body);
    let mut characters = body.chars();
    let summary = characters.by_ref().take(LIMIT).collect::<String>();
    if characters.next().is_some() {
        return format!("{summary}…");
    }
    summary
}

fn first_line(text: &str) -> String {
    sanitize(
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default(),
    )
}
