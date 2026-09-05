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

#[cfg(test)]
mod tests {
    use super::{
        ANIMATION_DURATION, ANIMATION_FRAME_INTERVAL, Component, EffortEffect, EffortEvent,
        EffortSelector,
    };
    use crate::{config::ReasoningEffort, tui::theme::Theme};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};
    use std::time::{Duration, Instant};

    fn key(code: KeyCode, now: Instant) -> EffortEvent {
        EffortEvent::Terminal {
            event: Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            now,
        }
    }

    fn colored_dial_dots(terminal: &Terminal<TestBackend>, color: Color) -> usize {
        let buffer = terminal.backend().buffer();
        (2..=10)
            .flat_map(|y| (21..=37).map(move |x| (x, y)))
            .filter(|position| {
                matches!(buffer[*position].symbol(), "•" | "●") && buffer[*position].fg == color
            })
            .count()
    }

    #[test]
    fn dial_has_five_evenly_spaced_stops_with_one_at_the_top() {
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();

        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let thick_dots = buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .count();
        assert_eq!(thick_dots, 5);
        assert_eq!(buffer[(29, 1)].symbol(), " ");
        assert_eq!(buffer[(29, 2)].symbol(), "●");
        assert_eq!(buffer[(29, 2)].fg, Color::Gray);
        assert_eq!(buffer[(20, 12)].fg, Color::DarkGray);
        let footer = (6..54)
            .map(|x| buffer[(x, 15)].symbol())
            .collect::<String>();
        assert_eq!(footer, "│ ←/→ effort · p pro · enter apply · esc cancel│");
        assert!((7..53).all(|x| buffer[(x, 14)].symbol() == " "));
    }

    #[test]
    fn effort_selector_hides_the_terminal_cursor() {
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();

        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn dial_is_symmetric_in_terminal_cells() {
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();

        for y in 2..=10 {
            for x in 21..=37 {
                if !matches!(buffer[(x, y)].symbol(), "•" | "●") {
                    continue;
                }
                assert!(matches!(buffer[(58 - x, y)].symbol(), "•" | "●"));
                assert!(matches!(buffer[(x, 12 - y)].symbol(), "•" | "●"));
            }
        }
    }

    #[test]
    fn animation_fills_thick_dots_in_the_new_effort_color() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Medium, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();

        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(29, 2)].fg, Color::Cyan);
        assert_eq!(terminal.backend().buffer()[(37, 7)].symbol(), "•");
        assert_eq!(terminal.backend().buffer()[(37, 7)].fg, Color::DarkGray);

        selector.update(key(KeyCode::Right, start));
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(29, 2)].fg, Color::Yellow);
        assert_eq!(terminal.backend().buffer()[(37, 7)].fg, Color::DarkGray);

        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION / 2));
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(37, 7)].fg, Color::Yellow);
    }

    #[test]
    fn arrows_wrap_around_the_effort_levels() {
        let now = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);

        selector.update(key(KeyCode::Left, now));
        assert_eq!(selector.selected_effort(), ReasoningEffort::Max);
        selector.update(key(KeyCode::Right, now));
        assert_eq!(selector.selected_effort(), ReasoningEffort::Low);
        selector.update(key(KeyCode::Down, now));
        assert_eq!(selector.selected_effort(), ReasoningEffort::Medium);
        selector.update(key(KeyCode::Up, now));
        assert_eq!(selector.selected_effort(), ReasoningEffort::Low);
    }

    #[test]
    fn transitions_use_pi_timing_and_cubic_easing() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);

        selector.update(key(KeyCode::Right, start));
        assert_eq!(
            selector.animation_deadline(),
            Some(start + ANIMATION_FRAME_INTERVAL)
        );

        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION / 2));
        assert!((selector.displayed_phase - 0.875).abs() < f64::EPSILON);

        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION));
        assert_eq!(selector.displayed_phase, 1.0);
        assert_eq!(selector.animation_deadline(), None);
    }

    #[test]
    fn max_to_low_wraps_clockwise_through_the_top_anchor() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Max, false);

        selector.update(key(KeyCode::Right, start));
        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION / 2));

        assert_eq!(selector.selected_effort(), ReasoningEffort::Low);
        assert!((selector.displayed_phase - 4.875).abs() < f64::EPSILON);
        assert!((selector.displayed_fill - 0.5).abs() < f64::EPSILON);

        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION));
        assert_eq!(selector.displayed_phase, 0.0);
        assert_eq!(selector.displayed_fill, 0.0);
    }

    #[test]
    fn max_to_low_progressively_removes_the_colored_arc() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Max, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        selector.update(key(KeyCode::Right, start));

        let mut colored = Vec::new();
        for elapsed in [Duration::ZERO, ANIMATION_DURATION / 2, ANIMATION_DURATION] {
            if !elapsed.is_zero() {
                selector.update(EffortEvent::AnimationFrame(start + elapsed));
            }
            terminal
                .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
                .unwrap();

            colored.push(colored_dial_dots(&terminal, Color::Gray));
        }

        assert!(colored[0] > colored[1]);
        assert!(colored[1] > colored[2]);
        assert_eq!(colored[2], 1);
        assert_eq!(terminal.backend().buffer()[(29, 2)].fg, Color::Gray);
    }

    #[test]
    fn max_to_low_drains_clockwise_from_low_toward_max() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Max, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        selector.update(key(KeyCode::Right, start));
        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION / 2));
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(37, 5)].fg, Color::DarkGray);
        assert_eq!(buffer[(21, 5)].fg, Color::Gray);
    }

    #[test]
    fn low_to_max_is_the_reverse_of_the_forward_wrap() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        selector.update(key(KeyCode::Left, start));

        let mut colored = Vec::new();
        for elapsed in [Duration::ZERO, ANIMATION_DURATION / 2, ANIMATION_DURATION] {
            if !elapsed.is_zero() {
                selector.update(EffortEvent::AnimationFrame(start + elapsed));
            }
            terminal
                .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
                .unwrap();
            colored.push(colored_dial_dots(&terminal, Color::Magenta));
        }

        assert!(colored[0] < colored[1]);
        assert!(colored[1] < colored[2]);
        assert_eq!(selector.displayed_phase, 4.0);
        assert_eq!(selector.displayed_fill, 4.0);
    }

    #[test]
    fn rapid_input_across_low_never_uses_wrapped_phase_as_fill() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Max, false);
        selector.update(key(KeyCode::Right, start));
        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION / 2));
        selector.update(key(KeyCode::Right, start + ANIMATION_DURATION / 2));

        assert_eq!(selector.selected_effort(), ReasoningEffort::Medium);
        assert!((selector.displayed_fill - 0.5).abs() < f64::EPSILON);
        assert!(selector.target_phase > ReasoningEffort::ALL.len() as f64);

        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION));
        assert!(selector.displayed_fill <= 1.0);
    }

    #[test]
    fn reversing_a_wrap_continues_from_the_current_fill_keyframe() {
        let start = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Low, false);
        selector.update(key(KeyCode::Left, start));
        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION / 2));
        assert!((selector.displayed_fill - 3.5).abs() < f64::EPSILON);

        selector.update(key(KeyCode::Right, start + ANIMATION_DURATION / 2));
        selector.update(EffortEvent::AnimationFrame(start + ANIMATION_DURATION));

        assert_eq!(selector.selected_effort(), ReasoningEffort::Low);
        assert!(selector.displayed_fill < 3.5);
    }

    #[test]
    fn enter_applies_and_escape_cancels() {
        let now = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Medium, false);
        selector.update(key(KeyCode::Right, now));

        assert_eq!(
            selector.update(key(KeyCode::Enter, now)).effects,
            [EffortEffect::Apply(ReasoningEffort::High, false)]
        );
        assert_eq!(
            selector.update(key(KeyCode::Esc, now)).effects,
            [EffortEffect::Dismiss]
        );
    }

    #[test]
    fn p_toggles_the_local_pro_preference_applied_with_effort() {
        let now = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::High, true);

        selector.update(key(KeyCode::Char('p'), now));

        assert_eq!(
            selector.update(key(KeyCode::Enter, now)).effects,
            [EffortEffect::Apply(ReasoningEffort::High, false)]
        );
    }

    #[test]
    fn pro_state_and_toggle_help_are_green() {
        let now = Instant::now();
        let mut selector = EffortSelector::new(ReasoningEffort::Medium, false);
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();

        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let label = (6..54)
            .map(|x| buffer[(x, 13)].symbol())
            .collect::<String>();
        assert!(label.contains("Pro: off"));
        let row = &buffer.content[13 * 60..14 * 60];
        let pro_start = row
            .windows(3)
            .position(|cells| {
                cells[0].symbol() == "P" && cells[1].symbol() == "r" && cells[2].symbol() == "o"
            })
            .unwrap();
        assert!((pro_start..pro_start + 3).all(|x| row[x].fg == Color::Green));

        selector.update(key(KeyCode::Char('p'), now));
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let label = (6..54)
            .map(|x| buffer[(x, 13)].symbol())
            .collect::<String>();
        assert!(label.contains("Pro: on"));
    }

    #[test]
    fn narrow_terminals_do_not_overflow_the_selector() {
        let mut selector = EffortSelector::new(ReasoningEffort::Medium, false);
        let mut terminal = Terminal::new(TestBackend::new(3, 4)).unwrap();

        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(terminal.backend().buffer().area.width, 3);
    }
}
