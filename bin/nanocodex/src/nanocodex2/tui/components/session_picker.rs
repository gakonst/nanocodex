// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Searchable picker for resumable persisted sessions.

use super::{
    file_finder::fuzzy_score,
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    session::{SessionSummary, format_age},
    theme::Theme,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use nanocodex_managed::SessionSearchHit;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const RESUME_KEY_BINDINGS: [(&str, &str); 3] = [
    ("↑↓/ctrl-n/p", "move"),
    ("enter/tab", "resume"),
    ("esc", "close"),
];
const MENTION_KEY_BINDINGS: [(&str, &str); 3] = [
    ("↑↓/ctrl-n/p", "move"),
    ("enter/tab", "insert"),
    ("esc", "close"),
];
const SEARCH_LABEL: &str = "Search: ";

pub(super) enum SessionPickerEvent {
    Terminal(Event),
    SearchResults {
        request_id: u64,
        query: String,
        result: Result<Vec<SessionSearchHit>, String>,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SessionPickerEffect {
    Search { request_id: u64, query: String },
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
    id: u64,
    revision: u64,
    searching: bool,
    search_error: Option<String>,
    content_hits: HashMap<String, String>,
    sessions: Vec<SessionSummary>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    mode: SessionPickerMode,
}

impl SessionPicker {
    #[cfg(test)]
    pub(super) fn new(sessions: Vec<SessionSummary>, mode: SessionPickerMode) -> Self {
        Self::new_with_id(sessions, mode, 0)
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    pub(super) fn new_with_id(
        mut sessions: Vec<SessionSummary>,
        mode: SessionPickerMode,
        id: u64,
    ) -> Self {
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at_unix_ms));
        let matches = (0..sessions.len()).collect();
        Self {
            id,
            revision: 0,
            searching: false,
            search_error: None,
            content_hits: HashMap::new(),
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
                }
                self.query_changed()
            }
            KeyCode::Backspace => ComponentUpdate::none(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Self::effect(SessionPickerEffect::Dismiss)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.clear();
                self.query_changed()
            }
            KeyCode::Up | KeyCode::Char('p')
                if key.code == KeyCode::Up || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.selected = self.selected.saturating_sub(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down | KeyCode::Char('n')
                if key.code == KeyCode::Down || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
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
                self.query_changed()
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<SessionPickerEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.query_changed()
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

    fn query_changed(&mut self) -> ComponentUpdate<SessionPickerEffect> {
        self.revision = self.revision.wrapping_add(1);
        self.content_hits.clear();
        self.search_error = None;
        self.searching = !self.query.trim().is_empty();
        self.refresh_matches();
        Self::effect(SessionPickerEffect::Search {
            request_id: self.revision,
            query: self.query.clone(),
        })
    }

    fn search_results(
        &mut self,
        request_id: u64,
        query: String,
        result: Result<Vec<SessionSearchHit>, String>,
    ) -> ComponentUpdate<SessionPickerEffect> {
        if request_id != self.revision || query != self.query || !self.searching {
            return ComponentUpdate::none();
        }
        self.searching = false;
        match result {
            Err(error) => self.search_error = Some(error),
            Ok(hits) => {
                // Keep title results stable while appending distinct content matches.
                // Only owned, attachable threads from the list belong in this picker.
                let indices: HashMap<_, _> = self
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(index, session)| (session.session_id.as_str(), index))
                    .collect();
                for hit in hits {
                    let Some(&index) = indices.get(hit.session_id.as_str()) else {
                        continue;
                    };
                    self.content_hits.entry(hit.session_id).or_insert_with(|| {
                        hit.snippet.split_whitespace().collect::<Vec<_>>().join(" ")
                    });
                    if !self.matches.contains(&index) {
                        self.matches.push(index);
                    }
                }
            }
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_lowercase();
        let mut ranked: Vec<_> = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(index, session)| session.match_score(&query).map(|score| (index, score)))
            .collect();
        // Stable sorting preserves recency when relevance is tied.
        ranked.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        self.matches = ranked.into_iter().map(|(index, _)| index).collect();
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
            let message = if self.sessions.is_empty() {
                "  No threads yet"
            } else {
                "  No matching threads · Ctrl+U to clear"
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let items = self.matches.iter().map(|index| {
            let session = &self.sessions[*index];
            let title = if session.preview.trim().is_empty() {
                "Untitled thread"
            } else {
                session.preview.as_str()
            };
            let detail = format!(
                "{} · {}",
                format_age(session.updated_at_unix_ms),
                session.session_id,
            );
            let mut lines = vec![
                Line::from(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.text())
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(detail, Style::default().fg(theme.muted()))),
            ];
            if let Some(snippet) = self.content_hits.get(&session.session_id) {
                lines.push(Line::from(Span::styled(
                    format!("Content: {snippet}"),
                    Style::default().fg(theme.muted()),
                )));
            }
            ListItem::new(lines)
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
    fn match_score(&self, query: &str) -> Option<usize> {
        let fields = [self.preview.to_lowercase(), self.session_id.to_lowercase()];
        // Like fzf, space-separated terms may match independently, in any order.
        query.split_whitespace().try_fold(0, |total, term| {
            fields
                .iter()
                .filter_map(|field| fuzzy_score(field, term))
                .max()
                .map(|score| total + score)
        })
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
            SessionPickerEvent::SearchResults {
                request_id,
                query,
                result,
            } => self.search_results(request_id, query, result),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let (title, key_bindings) = match self.mode {
            SessionPickerMode::Resume => (
                "Recent threads · type to fuzzy search",
                &RESUME_KEY_BINDINGS,
            ),
            SessionPickerMode::Mention => (
                "Mention thread · type to fuzzy search",
                &MENTION_KEY_BINDINGS,
            ),
        };
        let layout = Floating::new(title, 76, 18, key_bindings).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search = Rect {
            height: 1,
            ..layout.body
        };
        let status = if self.searching {
            "Searching thread contents…".to_owned()
        } else if let Some(error) = &self.search_error {
            format!("Content search unavailable: {error}")
        } else if self.query.trim().is_empty() {
            "Recent threads · search titles and conversation contents".to_owned()
        } else {
            format!(
                "{} threads · titles and conversation contents",
                self.matches.len()
            )
        };
        if layout.body.height > 1 {
            frame.render_widget(
                Paragraph::new(status).style(Style::default().fg(theme.muted())),
                Rect {
                    y: layout.body.y + 1,
                    height: 1,
                    ..layout.body
                },
            );
        }
        let sessions = Rect {
            y: layout.body.y + 2,
            height: layout.body.height.saturating_sub(2),
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
            updated_at_unix_ms: 1,
            model: "gpt".to_owned(),
            effort: ReasoningEffort::Medium,
            reasoning_mode: ReasoningMode::Standard,
            workspace: PathBuf::from("/work"),
            preview: preview.to_owned(),
        }
    }

    fn hit(id: &str, snippet: &str) -> nanocodex_managed::SessionSearchHit {
        nanocodex_managed::SessionSearchHit {
            session_id: id.into(),
            title: "Title".into(),
            turn_id: "turn".into(),
            cursor: "1".into(),
            score: 0.9,
            snippet: snippet.into(),
        }
    }

    #[test]
    fn content_only_matches_are_selectable_deduplicated_and_render_excerpts() {
        use crate::tui::theme::Theme;
        use ratatui::{Terminal, backend::TestBackend};
        let mut picker = SessionPicker::new(
            vec![summary("one", "A different title")],
            SessionPickerMode::Resume,
        );
        let update = picker.insert_paste("database migration");
        assert_eq!(
            update.effects,
            [SessionPickerEffect::Search {
                request_id: 1,
                query: "database migration".into()
            }]
        );
        assert!(picker.matches.is_empty());
        picker.update(SessionPickerEvent::SearchResults {
            request_id: 1,
            query: picker.query.clone(),
            result: Ok(vec![
                hit("one", "We discussed database migrations"),
                hit("one", "Another turn"),
                hit("unowned", "Other thread"),
            ]),
        });
        assert_eq!(picker.matches, [0]);
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("one".into())]
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("Content: We discussed database migrations"));
        assert!(!content.contains("Searching thread contents"));
    }

    #[test]
    fn old_results_cannot_repopulate_a_changed_or_cleared_query() {
        let mut picker =
            SessionPicker::new(vec![summary("one", "A title")], SessionPickerMode::Resume);
        picker.insert_paste("database");
        picker.insert_paste(" migration");
        picker.search_results(1, "database".into(), Ok(vec![hit("one", "old")]));
        assert!(picker.matches.is_empty());
        assert!(picker.searching);
        picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ))));
        picker.search_results(2, "database migration".into(), Ok(vec![hit("one", "old")]));
        assert!(picker.content_hits.is_empty());
        assert_eq!(picker.matches, [0]);
        assert!(!picker.searching);
    }

    #[test]
    fn content_response_preserves_selection_and_errors_keep_title_matches() {
        let mut picker = SessionPicker::new(
            vec![
                summary("one", "database"),
                summary("two", "database notes"),
                summary("three", "Other"),
            ],
            SessionPickerMode::Mention,
        );
        picker.insert_paste("database");
        picker.update(key(KeyCode::Down));
        picker.search_results(
            1,
            "database".into(),
            Ok(vec![
                hit("three", "database content"),
                hit("one", "database"),
            ]),
        );
        assert_eq!(picker.matches, [0, 1, 2]);
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Mention("two".into())]
        );
        picker.query_changed();
        picker.search_results(2, "database".into(), Err("offline".into()));
        assert_eq!(picker.matches, [0, 1]);
        assert_eq!(picker.search_error.as_deref(), Some("offline"));
    }

    #[test]
    fn recent_threads_first_and_clearing_restores_recency() {
        let mut recent = summary("recent", "fix parser");
        recent.updated_at_unix_ms = 20;
        let mut picker = SessionPicker::new(
            vec![summary("old", "write docs"), recent],
            SessionPickerMode::Resume,
        );
        assert_eq!(picker.sessions[picker.matches[0]].session_id, "recent");
        picker.insert_paste("wrdcs");
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("old".into())]
        );
        picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("recent".into())]
        );
    }

    #[test]
    fn fuzzy_terms_match_in_any_order_and_rank_tight_matches_first() {
        let mut picker = SessionPicker::new(
            vec![
                summary("one", "docs for the parser"),
                summary("two", "parser docs"),
            ],
            SessionPickerMode::Resume,
        );
        picker.insert_paste("dcs prsr");
        assert_eq!(picker.matches.len(), 2);
        picker.query = "parser".into();
        picker.refresh_matches();
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("two".into())]
        );
        picker.insert_paste(" zzz");
        assert!(picker.select().effects.is_empty());
    }

    #[test]
    fn control_navigation_does_not_modify_query_and_empty_backspace_stays_open() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "first"), summary("two", "second")],
            SessionPickerMode::Resume,
        );
        assert!(picker.update(key(KeyCode::Backspace)).effects.is_empty());
        picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
        ))));
        assert!(picker.query.is_empty());
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("two".into())]
        );
    }

    #[test]
    fn renders_titles_before_ids_and_survives_small_terminals() {
        use crate::tui::theme::Theme;
        use ratatui::{Terminal, backend::TestBackend};
        let mut picker = SessionPicker::new(
            vec![summary("thread-id", "Fix parser docs")],
            SessionPickerMode::Resume,
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("type to fuzzy search"));
        assert!(content.find("Fix parser docs").unwrap() < content.find("thread-id").unwrap());
        let mut terminal = Terminal::new(TestBackend::new(3, 3)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();
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
