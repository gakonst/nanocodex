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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const RESUME_KEY_BINDINGS: [(&str, &str); 4] = [
    ("↑↓/ctrl-n/p", "move"),
    ("enter/tab", "resume"),
    ("pgup/pgdn", "preview"),
    ("esc", "close"),
];
const MENTION_KEY_BINDINGS: [(&str, &str); 4] = [
    ("↑↓/ctrl-n/p", "move"),
    ("enter/tab", "insert"),
    ("pgup/pgdn", "preview"),
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
    preview_scroll: u16,
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
            preview_scroll: 0,
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
                self.preview_scroll = 0;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down | KeyCode::Char('n')
                if key.code == KeyCode::Down || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.preview_scroll = 0;
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1).min(self.matches.len() - 1);
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::PageUp => {
                self.preview_scroll = self.preview_scroll.saturating_sub(5);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::PageDown => {
                self.preview_scroll = self.preview_scroll.saturating_add(5);
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
        self.preview_scroll = 0;
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
                // Preserve the selected thread when merging content matches by recency.
                let selected = self.matches.get(self.selected).copied();
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
                        hit.snippet
                            .chars()
                            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                            .collect()
                    });
                    if !self.matches.contains(&index) {
                        self.matches.push(index);
                    }
                }
                self.matches.sort_unstable();
                if let Some(selected) = selected {
                    self.selected = self
                        .matches
                        .iter()
                        .position(|&index| index == selected)
                        .unwrap_or(0);
                }
            }
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_lowercase();
        // Sessions are already newest first; fuzzy matching only filters them.
        self.matches = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(index, session)| session.match_score(&query).map(|_| index))
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

    fn render_sessions(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        inline_preview: bool,
    ) {
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
            if inline_preview && let Some(snippet) = self.content_hits.get(&session.session_id) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "Content: {}",
                        snippet.split_whitespace().collect::<Vec<_>>().join(" ")
                    ),
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
    fn render_preview(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Match preview ")
            .border_style(Style::default().fg(theme.border()));
        let body = block.inner(area);
        frame.render_widget(block, area);
        if body.is_empty() {
            return;
        }
        let Some(&index) = self.matches.get(self.selected) else {
            self.preview_scroll = 0;
            frame.render_widget(
                Paragraph::new("Select a matching thread to preview.")
                    .style(Style::default().fg(theme.muted()))
                    .wrap(Wrap { trim: false }),
                body,
            );
            return;
        };
        let session = &self.sessions[index];
        let mut lines = vec![
            Line::from(Span::styled(
                if session.preview.trim().is_empty() {
                    "Untitled thread"
                } else {
                    &session.preview
                },
                Style::default()
                    .fg(theme.text())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::default(),
        ];
        if let Some(snippet) = self
            .content_hits
            .get(&session.session_id)
            .filter(|s| !s.trim().is_empty())
        {
            lines.extend(
                snippet
                    .lines()
                    .map(|line| highlighted_excerpt(line, &self.query, theme)),
            );
        } else {
            let message = if self.searching {
                "Searching thread contents…"
            } else if self.search_error.is_some() {
                "Content preview unavailable. Title matches are still available."
            } else if self.query.trim().is_empty() {
                "Type to search this thread's contents and preview the matching passage."
            } else {
                "Matched the title or ID. No matching content excerpt returned."
            };
            lines.push(Line::from(Span::styled(
                message,
                Style::default().fg(theme.muted()),
            )));
        }
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let maximum = paragraph
            .line_count(body.width)
            .saturating_sub(usize::from(body.height));
        self.preview_scroll = self
            .preview_scroll
            .min(u16::try_from(maximum).unwrap_or(u16::MAX));
        frame.render_widget(paragraph.scroll((self.preview_scroll, 0)), body);
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
        let layout = Floating::new(title, 136, 28, key_bindings).render(frame, area, theme);
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
        if sessions.width >= 88 {
            let list_width = sessions.width * 2 / 5;
            self.render_sessions(
                frame,
                Rect {
                    width: list_width,
                    ..sessions
                },
                theme,
                false,
            );
            self.render_preview(
                frame,
                Rect {
                    x: sessions.x + list_width + 1,
                    width: sessions.width - list_width - 1,
                    ..sessions
                },
                theme,
            );
        } else if sessions.height >= 10 {
            let list_height = sessions.height / 2;
            self.render_sessions(
                frame,
                Rect {
                    height: list_height,
                    ..sessions
                },
                theme,
                false,
            );
            self.render_preview(
                frame,
                Rect {
                    y: sessions.y + list_height,
                    height: sessions.height - list_height,
                    ..sessions
                },
                theme,
            );
        } else {
            self.render_sessions(frame, sessions, theme, true);
        }
    }
}

// Mark literal query terms without inventing a character match for semantic results.
fn highlighted_excerpt<'a>(text: &'a str, query: &str, theme: &Theme) -> Line<'a> {
    let graphemes: Vec<_> = text.graphemes(true).collect();
    let normalized: Vec<_> = graphemes.iter().map(|g| g.to_lowercase()).collect();
    let mut matched = vec![false; graphemes.len()];
    for term in query.split_whitespace() {
        let term: Vec<_> = term.graphemes(true).map(str::to_lowercase).collect();
        if term.is_empty() || term.len() > normalized.len() {
            continue;
        }
        for (start, window) in normalized.windows(term.len()).enumerate() {
            if window == term {
                matched[start..start + term.len()].fill(true);
            }
        }
    }
    Line::from(
        graphemes
            .into_iter()
            .zip(matched)
            .map(|(text, matched)| {
                let style = if matched {
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    Style::default().fg(theme.text())
                };
                Span::styled(text, style)
            })
            .collect::<Vec<_>>(),
    )
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
        assert!(content.contains("Match preview"));
        assert!(content.contains("We discussed database migrations"));
        assert!(!content.contains("Searching thread contents"));
    }

    fn screen(picker: &mut SessionPicker, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn find_text(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(usize, u16)> {
        (0..buffer.area.height).find_map(|y| {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            row.find(needle).map(|x| (x, y))
        })
    }

    #[test]
    fn selected_excerpt_is_on_the_right_and_tracks_navigation() {
        let mut picker = SessionPicker::new(
            vec![
                summary("one", "First thread"),
                summary("two", "Second thread"),
            ],
            SessionPickerMode::Resume,
        );
        picker.insert_paste("database");
        picker.search_results(
            1,
            "database".into(),
            Ok(vec![
                hit("one", "First database passage"),
                hit("two", "Second database passage"),
            ]),
        );
        let buffer = screen(&mut picker, 140, 30);
        let (list_x, _) = find_text(&buffer, "First thread").unwrap();
        let (excerpt_x, _) = find_text(&buffer, "First database passage").unwrap();
        assert!(excerpt_x > list_x + 40);
        assert!(find_text(&buffer, "Second database passage").is_none());
        picker.update(key(KeyCode::Down));
        let buffer = screen(&mut picker, 140, 30);
        assert!(find_text(&buffer, "Second database passage").is_some());
        assert!(find_text(&buffer, "First database passage").is_none());
        picker.insert_paste(" unmatched");
        let buffer = screen(&mut picker, 140, 30);
        assert!(find_text(&buffer, "Second database passage").is_none());
    }

    #[test]
    fn preview_stacks_on_narrow_screens_and_scrolls_long_excerpts() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "Thread title")],
            SessionPickerMode::Resume,
        );
        picker.insert_paste("needle");
        let excerpt = (0..50)
            .map(|n| format!("needle line {n}\n"))
            .collect::<String>();
        picker.search_results(1, "needle".into(), Ok(vec![hit("one", &excerpt)]));
        let buffer = screen(&mut picker, 70, 26);
        assert!(
            find_text(&buffer, "Match preview").unwrap().1
                > find_text(&buffer, "Thread title").unwrap().1
        );
        assert!(find_text(&buffer, "needle line 0").is_some());
        picker.update(key(KeyCode::PageDown));
        let buffer = screen(&mut picker, 70, 26);
        assert!(find_text(&buffer, "needle line 0").is_none());
        assert!(picker.preview_scroll > 0);
        for _ in 0..100 {
            picker.update(key(KeyCode::PageDown));
        }
        let buffer = screen(&mut picker, 70, 26);
        assert!(find_text(&buffer, "needle line 49").is_some());
        picker.update(key(KeyCode::Up));
        assert_eq!(picker.preview_scroll, 0);
        for (width, height) in [(3, 3), (30, 8), (80, 12)] {
            screen(&mut picker, width, height);
        }
    }

    #[test]
    fn excerpt_highlights_case_insensitive_unicode_terms_without_changing_text() {
        use ratatui::style::Modifier;
        let text = "A CAFÉ database 👩‍💻 passage";
        let line =
            super::highlighted_excerpt(text, "café database", &crate::tui::theme::Theme::default());
        assert_eq!(
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            text
        );
        let highlighted = line
            .spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(highlighted, "CAFÉdatabase");
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
    fn recent_content_match_sorts_before_title_match_and_preserves_selection() {
        let mut recent = summary("recent", "Other title");
        recent.updated_at_unix_ms = 20;
        let mut picker = SessionPicker::new(
            vec![summary("old", "database"), recent],
            SessionPickerMode::Resume,
        );
        picker.insert_paste("database");
        picker.search_results(
            1,
            "database".into(),
            Ok(vec![hit("recent", "database content")]),
        );
        assert_eq!(picker.matches, [0, 1]);
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("old".into())]
        );
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
    fn fuzzy_terms_match_in_any_order_and_keep_recent_matches_first() {
        let mut recent = summary("one", "docs for the parser");
        recent.updated_at_unix_ms = 20;
        let mut picker = SessionPicker::new(
            vec![summary("two", "parser docs"), recent],
            SessionPickerMode::Resume,
        );
        picker.insert_paste("dcs prsr");
        assert_eq!(picker.matches.len(), 2);
        picker.query = "parser".into();
        picker.refresh_matches();
        assert_eq!(
            picker.select().effects,
            [SessionPickerEffect::Resume("one".into())]
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
