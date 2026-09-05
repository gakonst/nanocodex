// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Searchable picker for resumable persisted sessions.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    session::{SessionSummary, format_age},
    theme::Theme,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const RESUME_KEY_BINDINGS: [(&str, &str); 3] =
    [("↑↓", "move"), ("enter/tab", "resume"), ("esc", "close")];
const MENTION_KEY_BINDINGS: [(&str, &str); 3] =
    [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];
const SEARCH_LABEL: &str = "Search: ";

pub(super) enum SessionPickerEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SessionPickerEffect {
    Dismiss,
    Resume(String),
    Mention(String),
}

#[derive(Clone, Copy)]
pub(super) enum SessionPickerMode {
    Resume,
    Mention,
}

pub(super) struct SessionPicker {
    sessions: Vec<SessionSummary>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    mode: SessionPickerMode,
}

impl SessionPicker {
    pub(super) fn new(sessions: Vec<SessionSummary>, mode: SessionPickerMode) -> Self {
        let matches = (0..sessions.len()).collect();
        Self {
            sessions,
            query: String::new(),
            matches,
            selected: 0,
            mode,
        }
    }

    fn update_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ComponentUpdate<SessionPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::Esc => Self::effect(SessionPickerEffect::Dismiss),
            KeyCode::Backspace if !self.query.is_empty() => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                    self.refresh_matches();
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::effect(SessionPickerEffect::Dismiss),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1).min(self.matches.len() - 1);
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Enter | KeyCode::Tab => self.select(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(character);
                self.refresh_matches();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<SessionPickerEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_matches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn select(&mut self) -> ComponentUpdate<SessionPickerEffect> {
        let Some(index) = self.matches.get(self.selected) else {
            return ComponentUpdate::none();
        };
        let session_id = self.sessions[*index].session_id.clone();
        let effect = match self.mode {
            SessionPickerMode::Resume => SessionPickerEffect::Resume(session_id),
            SessionPickerMode::Mention => SessionPickerEffect::Mention(session_id),
        };
        Self::effect(effect)
    }

    fn effect(effect: SessionPickerEffect) -> ComponentUpdate<SessionPickerEffect> {
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_ascii_lowercase();
        self.matches = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.matches(&query))
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let query = visible_tail(&self.query, query_width);
        let label_style = Style::default().fg(theme.muted());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, label_style),
                Span::styled(SEARCH_LABEL, label_style),
                Span::styled(query, Style::default().fg(theme.text())),
            ])),
            area,
        );
    }

    fn render_sessions(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            let message = match self.mode {
                SessionPickerMode::Resume => "  No resumable sessions found",
                SessionPickerMode::Mention => "  No other sessions found",
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let items = self.matches.iter().map(|index| {
            let session = &self.sessions[*index];
            let title = format!(
                "{} · {}",
                format_age(session.started_at_unix_ms),
                session.session_id,
            );
            let detail = format!(
                "{} · {} · {:?} · {}",
                session.preview,
                session.model,
                session.effort,
                session.workspace.display()
            );
            ListItem::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.text())
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(detail, Style::default().fg(theme.muted()))),
            ])
        });
        let list = List::new(items)
            .highlight_symbol("› ")
            .highlight_style(Style::default().fg(theme.accent()));
        let selected = (!self.matches.is_empty()).then_some(self.selected);
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, area, &mut state);
    }
}

impl SessionSummary {
    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.session_id.to_ascii_lowercase().contains(query)
            || self.preview.to_ascii_lowercase().contains(query)
            || self.model.to_ascii_lowercase().contains(query)
            || self
                .workspace
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains(query)
    }
}

impl Component for SessionPicker {
    type Event = SessionPickerEvent;
    type Effect = SessionPickerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            SessionPickerEvent::Terminal(Event::Key(key)) => self.update_key(key),
            SessionPickerEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            SessionPickerEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let (title, key_bindings) = match self.mode {
            SessionPickerMode::Resume => ("Resume session", &RESUME_KEY_BINDINGS),
            SessionPickerMode::Mention => ("Mention session", &MENTION_KEY_BINDINGS),
        };
        let layout = Floating::new(title, 76, 18, key_bindings).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search = Rect {
            height: 1,
            ..layout.body
        };
        let sessions = Rect {
            y: layout.body.y + 1,
            height: layout.body.height.saturating_sub(1),
            ..layout.body
        };
        self.render_search(frame, search, theme);
        self.render_sessions(frame, sessions, theme);
    }
}

fn visible_tail(query: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, grapheme) in query.grapheme_indices(true).rev() {
        used += grapheme.width();
        if used > width {
            return &query[index + grapheme.len()..];
        }
    }
    query
}

#[cfg(test)]
mod tests {
    use super::{
        Component, SessionPicker, SessionPickerEffect, SessionPickerEvent, SessionPickerMode,
    };
    use crate::{
        config::{ReasoningEffort, ReasoningMode},
        tui::session::SessionSummary,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn key(code: KeyCode) -> SessionPickerEvent {
        SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn summary(id: &str, preview: &str) -> SessionSummary {
        SessionSummary {
            session_id: id.to_owned(),
            started_at_unix_ms: 1,
            model: "gpt".to_owned(),
            effort: ReasoningEffort::Medium,
            reasoning_mode: ReasoningMode::Standard,
            workspace: PathBuf::from("/work"),
            preview: preview.to_owned(),
        }
    }

    #[test]
    fn search_selects_a_session_by_preview() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser"), summary("two", "write docs")],
            SessionPickerMode::Resume,
        );
        for character in "docs".chars() {
            picker.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Resume("two".to_owned())]
        );
    }

    #[test]
    fn mention_mode_returns_a_reference_instead_of_resuming() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser")],
            SessionPickerMode::Mention,
        );

        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Mention("one".to_owned())]
        );
    }

    #[test]
    fn tab_resumes_the_selected_session() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser"), summary("two", "write docs")],
            SessionPickerMode::Resume,
        );
        for character in "docs".chars() {
            picker.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            picker.update(key(KeyCode::Tab)).effects,
            [SessionPickerEffect::Resume("two".to_owned())]
        );
    }

    #[test]
    fn arrows_navigate_while_typing_continues_to_search() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser"), summary("two", "write docs")],
            SessionPickerMode::Resume,
        );

        picker.update(key(KeyCode::Down));
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Resume("two".to_owned())]
        );

        for character in "fix".chars() {
            picker.update(key(KeyCode::Char(character)));
        }
        assert_eq!(picker.query, "fix");
        assert_eq!(picker.matches, [0]);
        assert_eq!(picker.selected, 0);
    }
}
