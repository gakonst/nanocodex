// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Shared chrome and layout for centered modal components.

use crate::tui::theme::Theme;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const KEY_BINDING_SEPARATOR: &str = " · ";

pub(super) type KeyBinding = (&'static str, &'static str);

pub(super) struct Floating<'a> {
    title: &'a str,
    width: u16,
    height: u16,
    key_bindings: &'a [KeyBinding],
    placement: Placement,
    border_color: Option<Color>,
    title_color: Option<Color>,
}

#[derive(Clone, Copy, Default)]
enum Placement {
    #[default]
    Center,
    Top,
}

pub(super) struct FloatingLayout {
    pub(super) body: Rect,
}

impl<'a> Floating<'a> {
    pub(super) const fn new(
        title: &'a str,
        width: u16,
        height: u16,
        key_bindings: &'a [KeyBinding],
    ) -> Self {
        Self {
            title,
            width,
            height,
            key_bindings,
            placement: Placement::Center,
            border_color: None,
            title_color: None,
        }
    }

    pub(super) const fn at_top(mut self) -> Self {
        self.placement = Placement::Top;
        self
    }

    pub(super) const fn colors(mut self, border: Color, title: Color) -> Self {
        self.border_color = Some(border);
        self.title_color = Some(title);
        self
    }

    pub(super) fn render(self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> FloatingLayout {
        let popup = match self.placement {
            Placement::Center => centered(area, self.width, self.height),
            Placement::Top => top_centered(area, self.width, self.height),
        };
        let border_color = self.border_color.unwrap_or_else(|| theme.border());
        let title_color = self.title_color.unwrap_or_else(|| theme.accent());
        let mut block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color));
        if !self.title.is_empty() {
            block = block
                .title(format!(" {} ", self.title))
                .title_alignment(Alignment::Center)
                .title_style(
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                );
        }
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);

        let key_bindings = self.key_binding_lines(inner.width, theme);
        let footer_height = u16::try_from(key_bindings.len()).unwrap_or(u16::MAX);
        let (body, footer) = split_footer(inner, footer_height);
        if !footer.is_empty() {
            frame.render_widget(
                Paragraph::new(key_bindings).alignment(Alignment::Center),
                footer,
            );
        }
        FloatingLayout { body }
    }

    fn key_binding_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'a>> {
        if width == 0 {
            return Vec::new();
        }

        let width = usize::from(width);
        let separator_width = KEY_BINDING_SEPARATOR.width();
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        let mut line_width = 0;
        for &(key, help) in self.key_bindings {
            let key_binding_width =
                key.width() + usize::from(!key.is_empty() && !help.is_empty()) + help.width();
            if !spans.is_empty() && line_width + separator_width + key_binding_width > width {
                lines.push(Line::from(std::mem::take(&mut spans)));
                line_width = 0;
            }
            if !spans.is_empty() {
                spans.push(Span::styled(
                    KEY_BINDING_SEPARATOR,
                    Style::default().fg(theme.muted()),
                ));
                line_width += separator_width;
            }
            if !key.is_empty() {
                spans.push(Span::styled(key, Style::reset()));
            }
            if !help.is_empty() {
                spans.push(Span::styled(
                    if key.is_empty() {
                        help.to_owned()
                    } else {
                        format!(" {help}")
                    },
                    Style::default().fg(theme.muted()),
                ));
            }
            line_width += key_binding_width;
        }
        if !spans.is_empty() {
            lines.push(Line::from(spans));
        }
        lines
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn top_centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y,
        width,
        height: height.min(area.height),
    }
}

fn split_footer(inner: Rect, footer_height: u16) -> (Rect, Rect) {
    let footer_height = footer_height.min(inner.height);
    if footer_height == 0 {
        return (inner, Rect::default());
    }
    let footer = Rect {
        y: inner.bottom() - footer_height,
        height: footer_height,
        ..inner
    };
    let body = Rect {
        height: inner.height - footer_height,
        ..inner
    };
    (body, footer)
}

#[cfg(test)]
mod tests {
    use super::Floating;
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};

    #[test]
    fn floating_centers_rounded_chrome_and_styles_keys_separately_from_help() {
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();

        terminal
            .draw(|frame| {
                Floating::new("Test", 16, 6, &[("left", "help")]).render(
                    frame,
                    frame.area(),
                    &Theme::default(),
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "╭");
        assert_eq!(buffer[(17, 6)].symbol(), "╯");
        let row = 5;
        let start = (0..20)
            .find(|&column| buffer[(column, row)].symbol() == "l")
            .unwrap();
        assert_eq!(buffer[(start, row)].fg, Color::Reset);
        assert_eq!(buffer[(start + 5, row)].fg, Theme::default().muted());
    }

    #[test]
    fn floating_wraps_key_bindings_without_clipping_menu_help() {
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        let mut body = Rect::default();

        terminal
            .draw(|frame| {
                body = Floating::new(
                    "Test",
                    24,
                    8,
                    &[
                        ("first", "option"),
                        ("second", "option"),
                        ("third", "option"),
                    ],
                )
                .render(frame, frame.area(), &Theme::default())
                .body;
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(body.height, 3);
        assert!(rendered.contains("first option"));
        assert!(rendered.contains("second option"));
        assert!(rendered.contains("third option"));
    }
}
