//! Screen selection and read-only live video inside the tiled workspace.
use crate::tui::{
    format::sanitize_terminal_text,
    screen::{Command, Snapshot, Surface},
    theme::Theme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) enum Effect {
    Command(Command),
    Zoom,
    Close,
}
pub(super) struct ScreenPane {
    pub snapshot: Snapshot,
    pub selected: Option<Surface>,
    query: String,
    index: usize,
    command: Option<String>,
    pub image_area: Rect,
    presented: Option<Arc<crate::tui::screen::VideoFrame>>,
    frame_count: u32,
    measured_at: Instant,
    fps: u32,
}
impl ScreenPane {
    pub fn new() -> Self {
        Self {
            snapshot: Snapshot {
                status: "Loading Hands…".into(),
                ..Default::default()
            },
            selected: None,
            query: String::new(),
            index: 0,
            command: None,
            image_area: Rect::default(),
            presented: None,
            frame_count: 0,
            measured_at: Instant::now(),
            fps: 0,
        }
    }
    fn matches(&self) -> Vec<&Surface> {
        let query = self.query.to_lowercase();
        self.snapshot
            .surfaces
            .iter()
            .filter(|s| {
                format!("{} {}", s.machine_name, s.name)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(4096)
            .collect();
        if let Some(command) = &mut self.command {
            command.push_str(&text);
        } else if text.starts_with('/') {
            self.command = Some(text);
        } else if self.selected.is_none() {
            self.query.push_str(&text);
            self.index = 0;
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Effect::Close);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return None;
        }
        if self.snapshot.status.starts_with("Screen:") && key.code == KeyCode::Char('r') {
            return Some(Effect::Command(
                self.selected.clone().map_or(Command::List, Command::Watch),
            ));
        }
        if let Some(command) = &mut self.command {
            match key.code {
                KeyCode::Esc => self.command = None,
                KeyCode::Char(c) => command.push(c),
                KeyCode::Backspace => {
                    command.pop();
                }
                KeyCode::Enter => {
                    let command = self.command.take().unwrap();
                    return match command.as_str() {
                        "/zoom" => Some(Effect::Zoom),
                        "/screen" => {
                            *self = Self::new();
                            Some(Effect::Command(Command::List))
                        }
                        _ => None,
                    };
                }
                _ => {}
            }
            return None;
        }
        if key.code == KeyCode::Esc {
            return Some(Effect::Close);
        }
        if key.code == KeyCode::Char('/') {
            self.command = Some("/".into());
            return None;
        }
        if self.selected.is_some() {
            return match key.code {
                KeyCode::Char('m') => Some(Effect::Command(Command::ToggleAudio)),
                KeyCode::Char('z') => Some(Effect::Zoom),
                KeyCode::Char('r') => Some(Effect::Command(Command::Watch(
                    self.selected.clone().unwrap(),
                ))),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Down => {
                self.index = (self.index + 1).min(self.matches().len().saturating_sub(1))
            }
            KeyCode::Up => self.index = self.index.saturating_sub(1),
            KeyCode::Backspace => {
                self.query.pop();
                self.index = 0;
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                self.index = 0;
            }
            KeyCode::Enter => {
                if let Some(surface) = self.matches().get(self.index).cloned().cloned() {
                    self.selected = Some(surface.clone());
                    self.snapshot.frame = None;
                    self.snapshot.status = "Connecting…".into();
                    return Some(Effect::Command(Command::Watch(surface)));
                }
            }
            _ => {}
        }
        None
    }
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.image_area = Rect::default();
        if area.is_empty() {
            return;
        }
        let footer = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let body = Rect {
            height: area.height.saturating_sub(2),
            ..area
        };
        if let Some(surface) = &self.selected {
            if let Some(protocol) = &self.snapshot.frame
                && self
                    .presented
                    .as_ref()
                    .is_none_or(|previous| !Arc::ptr_eq(previous, protocol))
            {
                self.frame_count += 1;
                self.presented = Some(protocol.clone());
            }
            if self.measured_at.elapsed() >= Duration::from_secs(1) {
                self.fps = (f64::from(self.frame_count) / self.measured_at.elapsed().as_secs_f64())
                    .round() as u32;
                self.frame_count = 0;
                self.measured_at = Instant::now();
            }
            let label = format!(
                "{} · {}×{} · {} · {} fps · Audio: {}",
                sanitize_terminal_text(&surface.machine_name),
                self.snapshot.source_size.0,
                self.snapshot.source_size.1,
                sanitize_terminal_text(&self.snapshot.status),
                self.fps,
                if self.snapshot.audio.is_empty() {
                    "waiting".into()
                } else {
                    sanitize_terminal_text(&self.snapshot.audio)
                }
            );
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(theme.muted())),
                Rect { height: 1, ..area },
            );
            self.image_area = Rect {
                y: body.y.saturating_add(1),
                height: body.height.saturating_sub(1),
                ..body
            };
            if let Some(protocol) = &self.snapshot.frame {
                frame.render_widget(protocol.as_ref(), self.image_area);
            } else {
                frame.render_widget(
                    Paragraph::new(sanitize_terminal_text(&self.snapshot.status))
                        .wrap(ratatui::widgets::Wrap { trim: false }),
                    self.image_area,
                );
            }
        } else {
            let mut lines = vec![
                Line::from(format!("Select Hand: {}", self.query)),
                Line::from(sanitize_terminal_text(&self.snapshot.status)),
            ];
            let rows = usize::from(body.height.saturating_sub(2));
            let start = self.index.saturating_sub(rows.saturating_sub(1));
            for (index, surface) in self
                .matches()
                .into_iter()
                .enumerate()
                .skip(start)
                .take(rows)
            {
                let line = Line::from(format!(
                    "{} {} · {}",
                    if index == self.index { "›" } else { " " },
                    sanitize_terminal_text(&surface.machine_name),
                    sanitize_terminal_text(&surface.name)
                ));
                lines.push(if index == self.index {
                    line.style(Style::default().fg(theme.accent()))
                } else {
                    line
                });
            }
            frame.render_widget(Paragraph::new(lines), body);
        }
        let hint = self.command.clone().unwrap_or_else(|| {
            if self.selected.is_some() {
                "Tab: pane · /zoom or z: zoom · m: sound · r: retry · Esc: close".into()
            } else {
                "↑↓: select · Enter: watch · Tab: pane · Esc: close".into()
            }
        });
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(theme.muted())),
            footer,
        );
    }
}
