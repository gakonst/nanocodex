// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Account-authorized managed models, never a local provider roster.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::nanocodex2::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use nanocodex_managed::{AvailableModel, ManagedModel};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Instant;

pub(super) enum ModelSelectorEvent {
    Terminal { event: Event, now: Instant },
    AnimationFrame(Instant),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ModelSelectorEffect {
    Apply(ManagedModel),
    Dismiss,
}

pub(super) struct ModelSelector {
    models: Vec<AvailableModel>,
    selected: usize,
}

impl ModelSelector {
    pub(super) fn new(initial: ManagedModel, models: Vec<AvailableModel>) -> Self {
        let selected = models
            .iter()
            .position(|entry| entry.id == initial)
            .unwrap_or(0);
        Self { models, selected }
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        None
    }
}

impl Component for ModelSelector {
    type Event = ModelSelectorEvent;
    type Effect = ModelSelectorEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        let ModelSelectorEvent::Terminal {
            event: Event::Key(key),
            now: _,
        } = event
        else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::Left | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Right | KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.models.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(entry) = self.models.get(self.selected) {
                    return ComponentUpdate {
                        effects: vec![ModelSelectorEffect::Apply(entry.id)],
                        render: RenderRequest::Immediate,
                    };
                }
            }
            KeyCode::Esc | KeyCode::Backspace => {
                return ComponentUpdate {
                    effects: vec![ModelSelectorEffect::Dismiss],
                    render: RenderRequest::Immediate,
                };
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let bindings = [("↑/↓", "model"), ("enter", "apply"), ("esc", "cancel")];
        let height = u16::try_from(self.models.len()).unwrap_or(12).min(12) + 4;
        let layout =
            Floating::new("Select model", 64, height, &bindings).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let visible = usize::from(layout.body.height.max(1));
        let start = self.selected.saturating_sub(visible - 1);
        let mut lines = Vec::new();
        if self.models.is_empty() {
            lines.push(Line::from("No authorized models available"));
        }
        for (index, entry) in self.models.iter().enumerate().skip(start).take(visible) {
            let selected = index == self.selected;
            lines.push(Line::from(Span::styled(
                format!(
                    "{}{} · {}",
                    if selected { "› " } else { "  " },
                    entry.name,
                    entry.provider
                ),
                Style::default()
                    .fg(if selected {
                        theme.accent()
                    } else {
                        theme.text()
                    })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )));
        }
        frame.render_widget(Paragraph::new(lines), layout.body);
    }
}
