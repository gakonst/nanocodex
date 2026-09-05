// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Animated linear selector for the model fixed to a new session.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use nanocodex::Model;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::{Duration, Instant};

const MODELS: [Model; 4] = [Model::Luna, Model::Terra, Model::Sol, Model::Astra];
const ANIMATION_DURATION: Duration = Duration::from_millis(280);
const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const KEY_BINDINGS: [(&str, &str); 3] = [("←/→", "model"), ("enter", "apply"), ("esc", "cancel")];

pub(super) enum ModelSelectorEvent {
    Terminal { event: Event, now: Instant },
    AnimationFrame(Instant),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ModelSelectorEffect {
    Apply(Model),
    Dismiss,
}

pub(super) struct ModelSelector {
    selected: usize,
    displayed_position: f64,
    animation: Option<Animation>,
}

struct Animation {
    from: f64,
    to: f64,
    started_at: Instant,
    next_frame: Instant,
}

impl ModelSelector {
    pub(super) fn new(initial: Model) -> Self {
        let selected = model_index(initial);
        Self {
            selected,
            displayed_position: selected as f64,
            animation: None,
        }
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.animation
            .as_ref()
            .map(|animation| animation.next_frame)
    }

    fn update_key(&mut self, key: KeyEvent, now: Instant) -> ComponentUpdate<ModelSelectorEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::Left | KeyCode::Up => self.select_relative(-1, now),
            KeyCode::Right | KeyCode::Down => self.select_relative(1, now),
            KeyCode::Enter => ComponentUpdate {
                effects: vec![ModelSelectorEffect::Apply(MODELS[self.selected])],
                render: RenderRequest::Immediate,
            },
            KeyCode::Esc | KeyCode::Backspace => ComponentUpdate {
                effects: vec![ModelSelectorEffect::Dismiss],
                render: RenderRequest::Immediate,
            },
            _ => ComponentUpdate::none(),
        }
    }

    fn select_relative(
        &mut self,
        direction: isize,
        now: Instant,
    ) -> ComponentUpdate<ModelSelectorEffect> {
        self.advance_animation(now);
        let next = self
            .selected
            .saturating_add_signed(direction)
            .min(MODELS.len() - 1);
        if next == self.selected {
            return ComponentUpdate::none();
        }
        self.selected = next;
        self.animation = Some(Animation {
            from: self.displayed_position,
            to: next as f64,
            started_at: now,
            next_frame: now + ANIMATION_FRAME_INTERVAL,
        });
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn advance_animation(&mut self, now: Instant) -> bool {
        let Some(animation) = &mut self.animation else {
            return false;
        };
        let elapsed = now.saturating_duration_since(animation.started_at);
        let progress = (elapsed.as_secs_f64() / ANIMATION_DURATION.as_secs_f64()).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.displayed_position = animation.from + (animation.to - animation.from) * eased;
        if progress >= 1.0 {
            self.displayed_position = self.selected as f64;
            self.animation = None;
        } else {
            animation.next_frame = now + ANIMATION_FRAME_INTERVAL;
        }
        true
    }

    fn render_slider(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 5 || area.height < 2 {
            return;
        }
        let left = area.x.saturating_add(2);
        let right = area.right().saturating_sub(3).max(left);
        let width = right.saturating_sub(left);
        let indicator_column = left.saturating_add(
            (f64::from(width) * self.displayed_position / (MODELS.len() - 1) as f64).round() as u16,
        );
        let selected_color = theme.model(MODELS[self.selected]);
        let buffer = frame.buffer_mut();
        for column in left..=right {
            let color = if column <= indicator_column {
                selected_color
            } else {
                theme.muted()
            };
            buffer.set_string(column, area.y, "━", Style::default().fg(color));
        }
        for index in 0..MODELS.len() {
            let column = model_column(left, width, index);
            let color = if column <= indicator_column {
                selected_color
            } else {
                theme.muted()
            };
            buffer.set_string(column, area.y, "●", Style::default().fg(color));
        }
        buffer.set_string(
            indicator_column,
            area.y,
            "◆",
            Style::default()
                .fg(selected_color)
                .add_modifier(Modifier::BOLD),
        );

        let labels = [
            (model_column(left, width, 0), Model::Luna, "Luna"),
            (model_column(left, width, 1), Model::Terra, "Terra"),
            (model_column(left, width, 2), Model::Sol, "Sol"),
            (model_column(left, width, 3), Model::Astra, "Astra"),
        ];
        for (column, model, label) in labels {
            let label_width = u16::try_from(label.len()).unwrap_or(u16::MAX);
            let start = column.saturating_sub(label_width / 2).max(area.x);
            buffer.set_string(
                start,
                area.y.saturating_add(1),
                label,
                Style::default().fg(theme.model(model)),
            );
        }
    }
}

fn model_column(left: u16, width: u16, index: usize) -> u16 {
    left.saturating_add(
        (f64::from(width) * index as f64 / (MODELS.len() - 1) as f64).round() as u16,
    )
}

impl Component for ModelSelector {
    type Event = ModelSelectorEvent;
    type Effect = ModelSelectorEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ModelSelectorEvent::Terminal {
                event: Event::Key(key),
                now,
            } => self.update_key(key, now),
            ModelSelectorEvent::Terminal { .. } => ComponentUpdate::none(),
            ModelSelectorEvent::AnimationFrame(now) => {
                if self.advance_animation(now) {
                    ComponentUpdate::render(RenderRequest::Streaming)
                } else {
                    ComponentUpdate::none()
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let layout = Floating::new("Select model", 52, 7, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let model = MODELS[self.selected];
        let title = Line::from(vec![
            Span::styled("Selected: ", Style::default().fg(theme.border())),
            Span::styled(
                model_name(model),
                Style::default()
                    .fg(theme.model(model))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(title).alignment(Alignment::Center),
            Rect {
                height: 1,
                ..layout.body
            },
        );
        let slider_offset = if layout.body.height >= 4 { 2 } else { 1 };
        self.render_slider(
            frame,
            Rect {
                y: layout.body.y.saturating_add(slider_offset),
                height: 2,
                ..layout.body
            }
            .intersection(layout.body),
            theme,
        );
    }
}

fn model_index(model: Model) -> usize {
    MODELS
        .iter()
        .position(|candidate| *candidate == model)
        .unwrap_or_else(|| unreachable!("closed Model roster must have a selector entry"))
}

fn model_name(model: Model) -> &'static str {
    match model {
        Model::Luna => "Luna",
        Model::Terra => "Terra",
        Model::Sol => "Sol",
        Model::Astra => "Astra",
        _ => model.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render(selector: &mut ModelSelector) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(60, 9)).unwrap();
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
    }

    fn rendered_label_color(selector: &mut ModelSelector, label: &str) -> Color {
        let terminal = render(selector);
        let buffer = terminal.backend().buffer();
        let label = label.chars().collect::<Vec<_>>();
        let label_width = u16::try_from(label.len()).unwrap();
        for y in 0..buffer.area.height {
            for x in 0..=buffer.area.width.saturating_sub(label_width) {
                if label.iter().enumerate().all(|(offset, character)| {
                    buffer[(x + u16::try_from(offset).unwrap(), y)].symbol()
                        == character.to_string()
                }) {
                    return buffer[(x, y)].fg;
                }
            }
        }
        panic!("label not rendered: {label:?}");
    }

    fn rendered_stop_colors(selector: &mut ModelSelector) -> Vec<Color> {
        render(selector)
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .map(|cell| cell.fg)
            .collect()
    }

    #[test]
    fn terra_label_is_centered_under_its_stop() {
        let terminal = render(&mut ModelSelector::new(Model::Terra));
        let buffer = terminal.backend().buffer();
        let stop = buffer
            .content
            .iter()
            .position(|cell| cell.symbol() == "◆")
            .unwrap();
        let width = usize::from(buffer.area.width);
        let label_row = stop / width + 1;
        let terra = buffer.content[label_row * width..(label_row + 1) * width]
            .windows(5)
            .position(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>() == "Terra")
            .unwrap();

        assert_eq!(terra + 2, stop % width);
    }

    #[test]
    fn selection_moves_linearly_and_does_not_wrap() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Sol);

        selector.update_key(key(KeyCode::Right), now);
        assert_eq!(selector.selected, 3);
        selector.update_key(key(KeyCode::Left), now);
        assert_eq!(selector.selected, 2);
        selector.update_key(key(KeyCode::Left), now);
        selector.update_key(key(KeyCode::Left), now);
        selector.update_key(key(KeyCode::Left), now);
        assert_eq!(selector.selected, 0);
    }

    #[test]
    fn every_stop_keeps_its_model_color() {
        let mut selector = ModelSelector::new(Model::Sol);

        assert_eq!(rendered_label_color(&mut selector, "Luna"), Color::White);
        assert_eq!(rendered_label_color(&mut selector, "Terra"), Color::Green);
        assert_eq!(rendered_label_color(&mut selector, "Sol"), Color::Yellow);
        assert_eq!(
            rendered_label_color(&mut selector, "Astra"),
            Color::LightMagenta
        );
    }

    #[test]
    fn filled_bar_uses_the_selected_model_color() {
        let mut selector = ModelSelector::new(Model::Astra);
        let terminal = render(&mut selector);
        let rail = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "━")
            .collect::<Vec<_>>();

        assert!(!rail.is_empty());
        assert!(rail.iter().all(|cell| cell.fg == Color::LightMagenta));
    }

    #[test]
    fn stops_use_the_filled_bar_color_only_when_covered() {
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Luna)),
            [Color::DarkGray, Color::DarkGray, Color::DarkGray]
        );
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Terra)),
            [Color::Green, Color::DarkGray, Color::DarkGray]
        );
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Sol)),
            [Color::Yellow, Color::Yellow, Color::DarkGray]
        );
        assert_eq!(
            rendered_stop_colors(&mut ModelSelector::new(Model::Astra)),
            [
                Color::LightMagenta,
                Color::LightMagenta,
                Color::LightMagenta
            ]
        );
    }

    #[test]
    fn title_does_not_describe_the_model_order() {
        let terminal = render(&mut ModelSelector::new(Model::Terra));
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(!rendered.contains("smarter"));
    }

    #[test]
    fn narrow_selector_does_not_overwrite_wrapped_menu_help() {
        let mut terminal = Terminal::new(TestBackend::new(30, 7)).unwrap();
        terminal
            .draw(|frame| {
                ModelSelector::new(Model::Sol).render(frame, frame.area(), &Theme::default());
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("←/→ model"));
        assert!(rendered.contains("enter apply"));
        assert!(rendered.contains("esc cancel"));
        assert!(rendered.contains('◆'));
        assert!(rendered.contains("Sol"));
        assert_eq!(terminal.backend().buffer()[(0, 6)].symbol(), "╰");
        assert_eq!(terminal.backend().buffer()[(29, 6)].symbol(), "╯");
    }

    #[test]
    fn applying_returns_the_selected_model() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Sol);
        selector.update_key(key(KeyCode::Left), now);

        let update = selector.update_key(key(KeyCode::Enter), now);

        assert_eq!(update.effects, [ModelSelectorEffect::Apply(Model::Terra)]);
    }

    #[test]
    fn astra_initialization_and_apply_preserve_astra() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Astra);

        assert_eq!(selector.selected, 3);
        let update = selector.update_key(key(KeyCode::Enter), now);

        assert_eq!(update.effects, [ModelSelectorEffect::Apply(Model::Astra)]);
    }

    #[test]
    fn animation_reaches_the_selected_stop() {
        let now = Instant::now();
        let mut selector = ModelSelector::new(Model::Luna);
        selector.update_key(key(KeyCode::Right), now);
        assert!(selector.animation_deadline().is_some());

        selector.update(ModelSelectorEvent::AnimationFrame(now + ANIMATION_DURATION));

        assert_eq!(selector.displayed_position, 1.0);
        assert!(selector.animation_deadline().is_none());
    }
}
