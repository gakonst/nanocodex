// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Reusable demand-driven text wave for compact status labels.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(140);
const SHADE_PERCENTAGES: [u16; 8] = [100, 85, 70, 55, 40, 48, 62, 80];

pub(super) struct WavedText {
    text: String,
    base_color: Color,
    active: bool,
    started_at: Instant,
    next_frame: Instant,
    frame: usize,
}

impl WavedText {
    pub(super) fn new(text: impl Into<String>, base_color: Color) -> Self {
        let now = Instant::now();
        Self {
            text: text.into(),
            base_color,
            active: false,
            started_at: now,
            next_frame: now + FRAME_INTERVAL,
            frame: 0,
        }
    }

    pub(super) fn set_active(&mut self, active: bool, now: Instant) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.started_at = now;
        self.next_frame = now + FRAME_INTERVAL;
        self.frame = 0;
    }

    pub(super) const fn is_active(&self) -> bool {
        self.active
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.active.then_some(self.next_frame)
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        if !self.active || now < self.next_frame {
            return false;
        }

        let ticks =
            now.saturating_duration_since(self.started_at).as_millis() / FRAME_INTERVAL.as_millis();
        let frame = usize::try_from(ticks).unwrap_or(usize::MAX) % SHADE_PERCENTAGES.len();
        self.next_frame = now + FRAME_INTERVAL;
        if frame == self.frame {
            return false;
        }
        self.frame = frame;
        true
    }

    pub(super) fn spans(&self) -> Vec<Span<'static>> {
        self.text
            .chars()
            .enumerate()
            .map(|(index, character)| {
                let style = if self.active {
                    let percentage =
                        SHADE_PERCENTAGES[(index + self.frame) % SHADE_PERCENTAGES.len()];
                    shade(self.base_color, percentage)
                } else {
                    Style::default().fg(self.base_color)
                };
                Span::styled(character.to_string(), style)
            })
            .collect()
    }
}

fn shade(color: Color, percentage: u16) -> Style {
    if let Color::Rgb(red, green, blue) = color {
        return Style::default().fg(Color::Rgb(
            scale(red, percentage),
            scale(green, percentage),
            scale(blue, percentage),
        ));
    }

    let style = Style::default().fg(color);
    if percentage < 60 {
        style.add_modifier(Modifier::DIM)
    } else if percentage > 90 {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn scale(channel: u8, percentage: u16) -> u8 {
    let scaled = u16::from(channel).saturating_mul(percentage) / 100;
    u8::try_from(scaled).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Color, FRAME_INTERVAL, WavedText};
    use std::time::Instant;

    #[test]
    fn inactive_text_uses_its_base_color_without_scheduling_frames() {
        let base = Color::Rgb(200, 100, 50);
        let waved = WavedText::new("wave", base);

        assert_eq!(waved.animation_deadline(), None);
        assert!(waved.spans().iter().all(|span| span.style.fg == Some(base)));
    }

    #[test]
    fn active_text_shades_the_base_color_and_advances_on_demand() {
        let now = Instant::now();
        let mut waved = WavedText::new("wave", Color::Rgb(200, 100, 50));
        waved.set_active(true, now);
        let initial = waved.spans();

        assert_eq!(waved.animation_deadline(), Some(now + FRAME_INTERVAL));
        assert_eq!(initial[0].style.fg, Some(Color::Rgb(200, 100, 50)));
        assert_eq!(initial[1].style.fg, Some(Color::Rgb(170, 85, 42)));
        assert!(waved.advance(now + FRAME_INTERVAL));
        assert_ne!(waved.spans(), initial);

        waved.set_active(false, now + FRAME_INTERVAL);
        assert_eq!(waved.animation_deadline(), None);
        assert!(!waved.advance(now + FRAME_INTERVAL * 2));
    }
}
