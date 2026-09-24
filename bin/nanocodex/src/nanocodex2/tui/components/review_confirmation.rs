// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Paragraph, Wrap},
};

const KEY_BINDINGS: [(&str, &str); 2] = [("enter/y", "download"), ("esc/n", "cancel")];

pub(super) enum ReviewConfirmationEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ReviewConfirmationEffect {
    Confirm,
    Dismiss,
}

pub(super) struct ReviewDownloadConfirmation;

impl Component for ReviewDownloadConfirmation {
    type Event = ReviewConfirmationEvent;
    type Effect = ReviewConfirmationEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        let ReviewConfirmationEvent::Terminal(Event::Key(key)) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') => ComponentUpdate {
                effects: vec![ReviewConfirmationEffect::Confirm],
                render: RenderRequest::Immediate,
            },
            KeyCode::Esc | KeyCode::Char('n' | 'N') => ComponentUpdate {
                effects: vec![ReviewConfirmationEffect::Dismiss],
                render: RenderRequest::Immediate,
            },
            _ => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let layout = Floating::new("Install review interface", 64, 9, &KEY_BINDINGS)
            .render(frame, area, theme);
        let lines = vec![
            Line::from("The browser review interface is not installed."),
            Line::from(""),
            Line::styled(
                "Download the matching, checksummed bundle from this Tact release?",
                Style::default().fg(theme.muted()),
            ),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            layout.body,
        );
    }
}
