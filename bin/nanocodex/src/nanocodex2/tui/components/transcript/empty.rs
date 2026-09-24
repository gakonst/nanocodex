// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Empty transcript decoration, animated alongside live UI activity.

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

const FRAME_INTERVAL: Duration = Duration::from_millis(80);
const FRAME_COUNT: usize = 32;
const MAX_WIDTH: u16 = 32;
const MAX_HEIGHT: u16 = 9;
const HORIZONTAL_MARGIN: u16 = 4;
const VERTICAL_MARGIN: u16 = 2;
const PIXELS: [&str; 4] = ["░", "▒", "▓", "█"];
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
        // Each pixel is two terminal columns, keeping the orbit round in a
        // monospace grid. Leave the center clear so the wordmark never flickers.
        if mask.width >= 18 && mask.height >= 5 {
            let radius_x = f64::from(mask.width.saturating_sub(2)) / 2.0;
            let radius_y = f64::from(mask.height.saturating_sub(1)) / 2.0;
            for pixel in 0..FRAME_COUNT {
                let angle = TAU * pixel as f64 / FRAME_COUNT as f64;
                let column = ((radius_x + angle.sin() * radius_x) / 2.0).round() as u16 * 2;
                let row = (radius_y - angle.cos() * radius_y).round() as u16;
                let age = (self.frame + FRAME_COUNT - pixel) % FRAME_COUNT;
                let (glyph, style) = match age {
                    0..=1 => (
                        PIXELS[3],
                        Style::default()
                            .fg(theme.effort(effort))
                            .add_modifier(Modifier::BOLD),
                    ),
                    2..=3 => (PIXELS[2], Style::default().fg(theme.effort(effort))),
                    4..=6 => (PIXELS[1], Style::default().fg(theme.effort(effort))),
                    _ => (
                        PIXELS[0],
                        Style::default()
                            .fg(theme.effort(effort))
                            .add_modifier(Modifier::DIM),
                    ),
                };
                for offset in 0..2 {
                    if column + offset < mask.width {
                        frame.buffer_mut()[Position::new(mask.x + column + offset, mask.y + row)]
                            .set_symbol(glyph)
                            .set_style(style);
                    }
                }
            }
        }
        render_wordmark(frame, mask, theme);
    }
}

fn render_wordmark(frame: &mut Frame<'_>, mask: Rect, theme: &Theme) {
    let width = u16::try_from(WORDMARK.width()).unwrap_or(u16::MAX);
    if width > mask.width {
        frame.buffer_mut()[Position::new(mask.x + mask.width / 2, mask.y + mask.height / 2)]
            .set_symbol("▓")
            .set_style(Style::default().fg(theme.code_text()));
        return;
    }
    let x = mask.x + mask.width.saturating_sub(width) / 2;
    let y = mask.y + mask.height / 2;
    frame
        .buffer_mut()
        .set_string(x, y, WORDMARK, Style::reset().fg(theme.code_text()));
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
