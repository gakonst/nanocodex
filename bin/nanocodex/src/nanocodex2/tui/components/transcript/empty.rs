// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Demand-driven empty transcript animation.

use crate::{config::ReasoningEffort, tui::theme::Theme};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
};
use std::{
    f64::consts::TAU,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

const FRAME_INTERVAL: Duration = Duration::from_millis(120);
const FRAME_COUNT: usize = 48;
const MAX_WIDTH: u16 = 60;
const MAX_HEIGHT: u16 = 9;
const HORIZONTAL_MARGIN: u16 = 4;
const VERTICAL_MARGIN: u16 = 2;
const RAMP: [&str; 9] = ["·", ":", "-", "=", "+", "*", "#", "%", "@"];
const WORDMARK: &str = "nanocodex2";

pub(super) struct EmptyLogo {
    started_at: Instant,
    next_frame: Instant,
    frame: usize,
}

impl EmptyLogo {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            next_frame: now + FRAME_INTERVAL,
            frame: 0,
        }
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.next_frame
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        if now < self.next_frame {
            return false;
        }

        let elapsed = now.saturating_duration_since(self.started_at).as_millis();
        let frame = usize::try_from(elapsed / FRAME_INTERVAL.as_millis()).unwrap_or(usize::MAX)
            % FRAME_COUNT;
        self.next_frame = now + FRAME_INTERVAL;
        if frame == self.frame {
            return false;
        }
        self.frame = frame;
        true
    }

    pub(super) fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        effort: ReasoningEffort,
    ) {
        let Some(mask) = mask(area) else {
            return;
        };
        let phase = TAU * self.frame as f64 / FRAME_COUNT as f64;
        let center_x = f64::from(mask.width.saturating_sub(1)) / 2.0;
        let center_y = f64::from(mask.height.saturating_sub(1)) / 2.0;

        for row in 0..mask.height {
            let inset = corner_inset(row, mask.height).min(mask.width.saturating_sub(1) / 2);
            for column in inset..mask.width.saturating_sub(inset) {
                let x = f64::from(column) - center_x;
                let y = (f64::from(row) - center_y) * 2.2;
                let distance =
                    ((x - phase.cos() * 8.0).powi(2) + (y - phase.sin() * 3.0).powi(2)).sqrt();
                let value = (x * 0.23 + phase).sin()
                    + (x * 0.11 + y * 0.41 - phase * 2.0).sin()
                    + (distance * 0.31 - phase).sin();
                let level = (((value + 3.0) / 6.0) * (RAMP.len() - 1) as f64)
                    .round()
                    .clamp(0.0, (RAMP.len() - 1) as f64) as usize;
                let style = plasma_style(level, theme, effort);
                frame.buffer_mut()[Position::new(mask.x + column, mask.y + row)]
                    .set_symbol(RAMP[level])
                    .set_style(style);
            }
        }
        render_wordmark(frame, mask, theme);
    }
}

fn render_wordmark(frame: &mut Frame<'_>, mask: Rect, theme: &Theme) {
    let width = u16::try_from(WORDMARK.width()).unwrap_or(u16::MAX);
    if width > mask.width {
        return;
    }
    let x = mask.x + mask.width.saturating_sub(width) / 2;
    let y = mask.y + mask.height / 2;
    frame
        .buffer_mut()
        .set_string(x, y, WORDMARK, Style::reset().fg(theme.code_text()));
}

fn corner_inset(row: u16, height: u16) -> u16 {
    let edge_distance = row.min(height.saturating_sub(1).saturating_sub(row));
    match edge_distance {
        0 => 3,
        1 => 1,
        _ => 0,
    }
}

fn plasma_style(level: usize, theme: &Theme, effort: ReasoningEffort) -> Style {
    let style = Style::default().fg(theme.effort(effort));
    if level < 3 {
        return style.add_modifier(Modifier::DIM);
    }
    if level >= 6 {
        return style.add_modifier(Modifier::BOLD);
    }
    style
}

fn mask(area: Rect) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    let margin_x = HORIZONTAL_MARGIN.min(area.width.saturating_sub(1));
    let margin_y = VERTICAL_MARGIN.min(area.height.saturating_sub(1));
    let width = MAX_WIDTH.min(area.width.saturating_sub(margin_x)).max(1);
    let height = MAX_HEIGHT.min(area.height.saturating_sub(margin_y)).max(1);
    Some(Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    ))
}

#[cfg(test)]
mod tests {
    use super::{EmptyLogo, FRAME_INTERVAL, WORDMARK, corner_inset, mask};
    use crate::{config::ReasoningEffort, tui::theme::Theme};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    use std::time::Instant;
    use unicode_width::UnicodeWidthStr;

    fn render(logo: &EmptyLogo, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                logo.render(
                    frame,
                    frame.area(),
                    &Theme::default(),
                    ReasoningEffort::Xhigh,
                );
            })
            .unwrap();
        terminal
    }

    fn symbols(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn plasma_is_centered_in_a_wide_shallow_rounded_rectangle() {
        let logo = EmptyLogo::new(Instant::now());
        let terminal = render(&logo, 80, 20);
        let mask = mask(Rect::new(0, 0, 80, 20)).unwrap();

        assert_eq!(mask, Rect::new(10, 5, 60, 9));
        assert!(mask.width > mask.height * 6);
        for y in mask.y..mask.bottom() {
            let inset = corner_inset(y - mask.y, mask.height);
            for x in mask.x + inset..mask.right() - inset {
                assert_ne!(terminal.backend().buffer()[(x, y)].symbol(), " ");
            }
        }
        assert_eq!(terminal.backend().buffer()[(mask.x, mask.y)].symbol(), " ");
        assert_ne!(
            terminal.backend().buffer()[(mask.x + 3, mask.y)].symbol(),
            " "
        );
        assert_eq!(
            terminal.backend().buffer()[(mask.x - 1, mask.y)].symbol(),
            " "
        );

        let narrow = render(&logo, 1, 1);
        assert_ne!(symbols(&narrow), " ");
    }

    #[test]
    fn plasma_uses_the_effort_color_with_multiple_character_densities() {
        let logo = EmptyLogo::new(Instant::now());
        let terminal = render(&logo, 80, 20);
        let buffer = terminal.backend().buffer();
        let colors = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| cell.fg)
            .collect::<std::collections::HashSet<_>>();
        let glyphs = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .map(|cell| cell.symbol())
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(
            colors,
            [
                ratatui::style::Color::Red,
                ratatui::style::Color::Rgb(0xD7, 0xD7, 0xD7),
            ]
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
        );
        assert!(glyphs.len() >= 5);
    }

    #[test]
    fn nanocodex2_wordmark_is_centered_and_contrasting() {
        let logo = EmptyLogo::new(Instant::now());
        let terminal = render(&logo, 80, 20);
        let mask = mask(Rect::new(0, 0, 80, 20)).unwrap();
        let width = u16::try_from(WORDMARK.width()).unwrap();
        let x = mask.x + (mask.width - width) / 2;
        let y = mask.y + mask.height / 2;
        let buffer = terminal.backend().buffer();
        let rendered = (x..x + width)
            .map(|column| buffer[(column, y)].symbol())
            .collect::<String>();

        assert_eq!(rendered, WORDMARK);
        for column in x..x + width {
            assert_eq!(buffer[(column, y)].fg, Theme::default().code_text());
        }
    }

    #[test]
    fn animation_advances_only_after_its_deadline() {
        let start = Instant::now();
        let mut logo = EmptyLogo::new(start);
        let first = symbols(&render(&logo, 60, 12));

        assert!(!logo.advance(start + FRAME_INTERVAL / 2));
        assert!(logo.advance(start + FRAME_INTERVAL));
        assert_ne!(symbols(&render(&logo, 60, 12)), first);
        assert!(logo.deadline() > start + FRAME_INTERVAL);
    }
}
