// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Styled global keyboard shortcut reference.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

const FOOTER: [(&str, &str); 2] = [("↑↓", "scroll"), ("esc", "close")];
const BINDINGS: [(&str, &str); 33] = [
    ("ctrl+x", "mute · unmute microphone while voice is active"),
    ("ctrl+s", "change reasoning effort"),
    ("ctrl+d", "select model · before first prompt"),
    ("ctrl+g", "edit prompt in $EDITOR"),
    ("ctrl+r", "recent prompts"),
    (
        "alt+u",
        "undo latest queued/steer message · before model receives it",
    ),
    ("ctrl+z", "restore the last cleared draft"),
    ("ctrl/cmd+v", "paste clipboard image"),
    ("ctrl+o", "expand · collapse all tool calls"),
    (
        "ctrl+c",
        "clear input · when composer is focused and nonempty",
    ),
    ("ctrl+c ctrl+c", "split closes pane · else exit"),
    ("esc esc", "interrupt the active response"),
    ("enter", "submit prompt"),
    ("enter + enter", "submit prompt and steer"),
    ("shift/alt+enter · ctrl+j", "insert newline"),
    ("ctrl+a/e", "move to line start · end"),
    ("ctrl+b/f", "move to previous · next character"),
    ("ctrl/alt+←/→ · alt+b/f", "move to previous · next word"),
    ("ctrl+w · alt/option+backspace", "delete previous word"),
    ("ctrl+u/k", "delete to line start · end"),
    ("ctrl+h/d", "delete previous · next character"),
    ("↑/↓ · ctrl+p/n", "move lines · prompt history at edge"),
    ("tab / shift+tab", "cycle panes · otherwise focus queue"),
    (
        "/reload",
        "restart local terminals in their current threads",
    ),
    ("/autoroute", "enable auto routing · before first prompt"),
    ("/screen", "select a Hand and watch its live screen"),
    ("/zoom", "expand focused pane · restore split layout"),
    ("/", "open actions · empty prompt only"),
    ("@", "insert workspace file"),
    ("!", "local shell command · prompt start"),
    ("mouse click/drag", "open links/tools · copy text"),
    ("pgup/pgdn · wheel", "scroll transcript"),
    ("ctrl+home/end", "jump to start · follow latest"),
];

pub(super) enum KeybindingsEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum KeybindingsEffect {
    Dismiss,
}

#[derive(Default)]
pub(super) struct KeybindingsHelp {
    scroll: u16,
}

impl Component for KeybindingsHelp {
    type Event = KeybindingsEvent;
    type Effect = KeybindingsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            KeybindingsEvent::Terminal(Event::Key(key))
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                match key.code {
                    KeyCode::Esc => {
                        return ComponentUpdate {
                            effects: vec![KeybindingsEffect::Dismiss],
                            render: RenderRequest::Immediate,
                        };
                    }
                    KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                    _ => return ComponentUpdate::none(),
                }
            }
            KeybindingsEvent::Terminal(_) => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let height = u16::try_from(BINDINGS.len())
            .unwrap_or(u16::MAX)
            .saturating_add(3);
        let layout =
            Floating::new("Keyboard shortcuts", 72, height, &FOOTER).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let max_scroll = BINDINGS
            .len()
            .saturating_sub(usize::from(layout.body.height));
        self.scroll = self
            .scroll
            .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
        let lines = BINDINGS
            .iter()
            .map(|&(key, description)| binding_line(key, description, layout.body.width, theme))
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), layout.body);
    }
}

fn binding_line(
    key: &'static str,
    description: &'static str,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let occupied = 1 + key.width() + description.width();
    let gap = usize::from(width).saturating_sub(occupied).max(1);
    Line::from(vec![
        Span::styled(
            format!(" {key}"),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(description, Style::default().fg(theme.muted())),
    ])
}
