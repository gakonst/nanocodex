// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Animated circular selector for reasoning effort.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::{config::ReasoningEffort, tui::theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::{
    f64::consts::{FRAC_PI_2, TAU},
    time::{Duration, Instant},
};

const ANIMATION_DURATION: Duration = Duration::from_millis(420);
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(16);
// Terminal cells are roughly twice as tall as they are wide, so a 2:1 cell
// ratio produces a visually circular dial.
const DIAL_WIDTH: u16 = 17;
const DIAL_HEIGHT: u16 = 9;
const DIAL_SAMPLES: usize = 96;
const KEY_BINDINGS: [(&str, &str); 4] = [
    ("←/→", "effort"),
    ("p", "pro"),
    ("enter", "apply"),
    ("esc", "cancel"),
];
const FILLER_DOT: &str = "•";
const THICK_DOT: &str = "●";

pub(super) enum EffortEvent {
    Terminal { event: Event, now: Instant },
    AnimationFrame(Instant),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum EffortEffect {
    Apply(ReasoningEffort, bool),
    Dismiss,
}

pub(super) struct EffortSelector {
    selected: usize,
    pro: bool,
    displayed_phase: f64,
    displayed_fill: f64,
    target_phase: f64,
    animation: Option<Animation>,
}

struct Animation {
    phase_from: f64,
    phase_to: f64,
    fill_from: f64,
    fill_to: f64,
    wrapping_fill: bool,
    started_at: Instant,
    next_frame: Instant,
}

impl EffortSelector {
    pub(super) fn new(initial: ReasoningEffort, pro: bool) -> Self {
        let selected = initial.index();
        let phase = selected as f64;
        Self {
            selected,
            pro,
            displayed_phase: phase,
            displayed_fill: phase,
            target_phase: phase,
            animation: None,
        }
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.animation
            .as_ref()
            .map(|animation| animation.next_frame)
    }

    fn update_key(&mut self, key: KeyEvent, now: Instant) -> ComponentUpdate<EffortEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::Left | KeyCode::Up => {
                self.select_relative(-1, now);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Right | KeyCode::Down => {
                self.select_relative(1, now);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Char('p') => {
                self.pro = !self.pro;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Enter => ComponentUpdate {
                effects: vec![EffortEffect::Apply(self.selected_effort(), self.pro)],
                render: RenderRequest::Immediate,
            },
            KeyCode::Esc | KeyCode::Backspace => ComponentUpdate {
                effects: vec![EffortEffect::Dismiss],
                render: RenderRequest::Immediate,
            },
            _ => ComponentUpdate::none(),
        }
    }

    fn select_relative(&mut self, direction: isize, now: Instant) {
        self.advance_animation(now);
        let previous = self.selected;
        if direction < 0 {
            self.selected = if self.selected == 0 {
                ReasoningEffort::ALL.len() - 1
            } else {
                self.selected - 1
            };
        } else {
            self.selected = (self.selected + 1) % ReasoningEffort::ALL.len();
        }
        self.target_phase += direction as f64;
        let wrapping_fill = (previous == ReasoningEffort::ALL.len() - 1 && self.selected == 0)
            || (previous == 0 && self.selected == ReasoningEffort::ALL.len() - 1);
        self.animation = Some(Animation {
            phase_from: self.displayed_phase,
            phase_to: self.target_phase,
            fill_from: self.displayed_fill,
            fill_to: self.selected as f64,
            wrapping_fill,
            started_at: now,
            next_frame: now + ANIMATION_FRAME_INTERVAL,
        });
    }

    fn advance_animation(&mut self, now: Instant) -> bool {
        let Some(animation) = &mut self.animation else {
            return false;
        };
        let elapsed = now.saturating_duration_since(animation.started_at);
        let progress = (elapsed.as_secs_f64() / ANIMATION_DURATION.as_secs_f64()).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.displayed_phase =
            animation.phase_from + (animation.phase_to - animation.phase_from) * eased;
        self.displayed_fill =
            animation.fill_from + (animation.fill_to - animation.fill_from) * eased;

        if progress >= 1.0 {
            let phase = self.selected as f64;
            self.displayed_phase = phase;
            self.displayed_fill = phase;
            self.target_phase = phase;
            self.animation = None;
        } else {
            animation.next_frame = now + ANIMATION_FRAME_INTERVAL;
        }
        true
    }

    fn selected_effort(&self) -> ReasoningEffort {
        ReasoningEffort::ALL[self.selected]
    }

    fn render_dial(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let center_x = f64::from(area.width.saturating_sub(1)) / 2.0;
        let center_y = f64::from(area.height.saturating_sub(1)) / 2.0;
        let radius_x = center_x;
        let radius_y = center_y;
        let selected_color = theme.effort(self.selected_effort());
        let indicator = dial_position(
            area,
            center_x,
            center_y,
            radius_x,
            radius_y,
            self.displayed_phase,
        );
        let filled_phase = self
            .displayed_fill
            .clamp(0.0, (ReasoningEffort::ALL.len() - 1) as f64);
        let wrapping_fill = self
            .animation
            .as_ref()
            .is_some_and(|animation| animation.wrapping_fill);
        let buffer = frame.buffer_mut();
        for sample in 0..DIAL_SAMPLES {
            let phase = sample as f64 / DIAL_SAMPLES as f64 * ReasoningEffort::ALL.len() as f64;
            let point = dial_position(area, center_x, center_y, radius_x, radius_y, phase);
            draw_dot(
                buffer,
                point,
                FILLER_DOT,
                Style::default().fg(theme.muted()),
            );
        }
        for sample in 0..DIAL_SAMPLES {
            let phase = sample as f64 / DIAL_SAMPLES as f64 * ReasoningEffort::ALL.len() as f64;
            if !phase_is_filled(phase, filled_phase, wrapping_fill) {
                continue;
            }
            let point = dial_position(area, center_x, center_y, radius_x, radius_y, phase);
            draw_dot(
                buffer,
                point,
                FILLER_DOT,
                Style::default().fg(selected_color),
            );
        }

        for index in 0..ReasoningEffort::ALL.len() {
            let point = dial_position(area, center_x, center_y, radius_x, radius_y, index as f64);
            let color = if phase_is_filled(index as f64, filled_phase, wrapping_fill) {
                selected_color
            } else {
                theme.muted()
            };
            draw_dot(buffer, point, THICK_DOT, Style::default().fg(color));
        }

        draw_dot(
            buffer,
            indicator,
            THICK_DOT,
            Style::default()
                .fg(
                    if phase_is_filled(
                        self.displayed_phase
                            .rem_euclid(ReasoningEffort::ALL.len() as f64),
                        filled_phase,
                        wrapping_fill,
                    ) {
                        selected_color
                    } else {
                        theme.muted()
                    },
                )
                .add_modifier(Modifier::BOLD),
        );
    }

    fn render_labels(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let effort = self.selected_effort();
        let lines = vec![
            Line::from(vec![
                Span::styled("Selected Effort:", Style::default().fg(theme.border())),
                Span::styled(
                    format!(" {}", effort.as_str()),
                    Style::default()
                        .fg(theme.effort(effort))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Pro: ", Style::default().fg(Color::Green)),
                Span::styled(
                    if self.pro { "on" } else { "off" },
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
    }
}

fn phase_is_filled(phase: f64, filled_phase: f64, wrapping: bool) -> bool {
    if !wrapping {
        return phase <= filled_phase + f64::EPSILON;
    }

    let max_phase = (ReasoningEffort::ALL.len() - 1) as f64;
    phase <= f64::EPSILON
        || (phase >= max_phase - filled_phase - f64::EPSILON && phase <= max_phase + f64::EPSILON)
}

impl Component for EffortSelector {
    type Event = EffortEvent;
    type Effect = EffortEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            EffortEvent::Terminal {
                event: Event::Key(key),
                now,
            } => self.update_key(key, now),
            EffortEvent::Terminal { .. } => ComponentUpdate::none(),
            EffortEvent::AnimationFrame(now) => {
                if self.advance_animation(now) {
                    ComponentUpdate::render(RenderRequest::Immediate)
                } else {
                    ComponentUpdate::none()
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let layout = Floating::new("Effort", 48, 17, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }

        let dial_width = DIAL_WIDTH.min(layout.body.width);
        let dial_height = DIAL_HEIGHT.min(layout.body.height.saturating_sub(4));
        let dial = Rect {
            x: layout.body.x + layout.body.width.saturating_sub(dial_width) / 2,
            y: layout.body.y.saturating_add(1),
            width: dial_width,
            height: dial_height,
        }
        .intersection(layout.body);
        self.render_dial(frame, dial, theme);
        let labels = Rect {
            y: dial.bottom().min(layout.body.bottom().saturating_sub(3)) + 1,
            height: 2,
            ..layout.body
        }
        .intersection(layout.body);
        self.render_labels(frame, labels, theme);
    }
}

fn dial_position(
    area: Rect,
    center_x: f64,
    center_y: f64,
    radius_x: f64,
    radius_y: f64,
    phase: f64,
) -> Position {
    let angle = -FRAC_PI_2 + TAU * phase / ReasoningEffort::ALL.len() as f64;
    let x = center_x + angle.cos() * radius_x;
    let y = center_y + angle.sin() * radius_y;
    Position::new(
        area.x
            + x.round()
                .clamp(0.0, f64::from(area.width.saturating_sub(1))) as u16,
        area.y
            + y.round()
                .clamp(0.0, f64::from(area.height.saturating_sub(1))) as u16,
    )
}

fn draw_dot(buffer: &mut Buffer, position: Position, symbol: &str, style: Style) {
    buffer[position].set_symbol(symbol).set_style(style);
}
