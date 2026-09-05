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
const BINDINGS: [(&str, &str); 27] = [
    ("ctrl+s", "change reasoning effort"),
    ("ctrl+d", "select model · before first prompt"),
    ("ctrl+g", "edit prompt in $EDITOR"),
    ("ctrl+r", "recent prompts"),
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
    ("tab", "focus queue · when present"),
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

#[cfg(test)]
mod tests {
    use super::{BINDINGS, Component, KeybindingsEffect, KeybindingsEvent, KeybindingsHelp};
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    #[test]
    fn popup_right_aligns_muted_descriptions() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();

        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let row = rendered
            .iter()
            .position(|line| line.contains("ctrl+g"))
            .expect("editor shortcut should render");
        for description in ["edit prompt in $EDITOR", "paste clipboard image"] {
            let line = rendered
                .iter()
                .find(|line| line.contains(description))
                .expect("description should render");
            let start = line.find(description).unwrap();
            let end = unicode_width::UnicodeWidthStr::width(&line[..start])
                + unicode_width::UnicodeWidthStr::width(description);
            assert_eq!(end, 75);
        }
        assert_eq!(
            buffer[(5, u16::try_from(row).unwrap())].fg,
            Theme::default().accent()
        );
        assert_eq!(
            buffer[(74, u16::try_from(row).unwrap())].fg,
            Color::DarkGray
        );
    }

    #[test]
    fn popup_documents_context_sensitive_composer_shortcuts() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 31)).unwrap();

        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        for expected in [
            "ctrl+s",
            "change reasoning effort",
            "ctrl+d",
            "select model · before first prompt",
            "ctrl/cmd+v",
            "ctrl+r",
            "ctrl+z",
            "ctrl+c ctrl+c",
            "clear input · when composer is focused and nonempty",
            "split closes pane · else exit",
            "enter + enter",
            "submit prompt and steer",
            "shift/alt+enter · ctrl+j",
            "ctrl+a/e",
            "move to line start · end",
            "ctrl+b/f",
            "move to previous · next character",
            "ctrl/alt+←/→ · alt+b/f",
            "move to previous · next word",
            "ctrl+w · alt/option+backspace",
            "delete previous word",
            "ctrl+u/k",
            "delete to line start · end",
            "ctrl+h/d",
            "delete previous · next character",
            "ctrl+p/n",
            "prompt history at edge",
            "focus queue · when present",
            "open actions · empty prompt only",
            "insert workspace file",
            "local shell command · prompt start",
            "mouse click/drag",
            "pgup/pgdn · wheel",
            "scroll transcript",
            "ctrl+home/end",
            "jump to start · follow latest",
            "↑↓ scroll",
        ] {
            assert!(rendered.iter().any(|line| line.contains(expected)));
        }
    }

    #[test]
    fn compact_popup_scrolls_to_late_shortcuts() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        for _ in &BINDINGS {
            help.update(KeybindingsEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            ))));
            terminal
                .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
                .unwrap();
        }

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        for expected in [
            "local shell command · prompt start",
            "mouse click/drag",
            "scroll transcript",
            "jump to start · follow latest",
        ] {
            assert!(rendered.iter().any(|line| line.contains(expected)));
        }
    }

    #[test]
    fn narrow_terminals_do_not_overflow_the_popup() {
        let mut help = KeybindingsHelp::default();
        let mut terminal = Terminal::new(TestBackend::new(8, 4)).unwrap();

        terminal
            .draw(|frame| help.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(terminal.backend().buffer().area.width, 8);
    }

    #[test]
    fn escape_dismisses_the_popup() {
        let mut help = KeybindingsHelp::default();

        let update = help.update(KeybindingsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))));

        assert_eq!(update.effects, [KeybindingsEffect::Dismiss]);
    }
}
