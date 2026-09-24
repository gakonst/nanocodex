// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Picker for prompts from the current session or all persisted sessions.

use super::{
    file_finder::{fuzzy_score, visible_query_tail},
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{session::RecentPrompt, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const KEY_BINDINGS: [(&str, &str); 6] = [
    ("type", "search"),
    ("↑↓", "move"),
    ("pgup/pgdn", "preview"),
    ("enter/tab", "select"),
    ("ctrl+f", "scope"),
    ("esc", "close"),
];
const LIST_HEIGHT: u16 = 7;
const SEARCH_LABEL: &str = "Search: ";

pub(super) enum RecentPromptPickerEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum RecentPromptPickerEffect {
    Dismiss,
    Insert(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecentPromptScope {
    Global,
    CurrentSession,
}

pub(super) struct RecentPromptPicker {
    prompts: Vec<RecentPrompt>,
    current_session_id: String,
    scope: RecentPromptScope,
    query: String,
    visible: Vec<usize>,
    selected: usize,
    preview_scroll: u16,
}

impl RecentPromptPicker {
    pub(super) fn new(prompts: Vec<RecentPrompt>, current_session_id: String) -> Self {
        let visible = (0..prompts.len()).collect();
        Self {
            prompts,
            current_session_id,
            scope: RecentPromptScope::Global,
            query: String::new(),
            visible,
            selected: 0,
            preview_scroll: 0,
        }
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<RecentPromptPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::Esc => Self::effect(RecentPromptPickerEffect::Dismiss),
            KeyCode::Backspace if !self.query.is_empty() => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                    self.refresh_visible();
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::effect(RecentPromptPickerEffect::Dismiss),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.preview_scroll = 0;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                if !self.visible.is_empty() {
                    self.selected = (self.selected + 1).min(self.visible.len() - 1);
                }
                self.preview_scroll = 0;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::PageUp => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::PageDown => {
                self.preview_scroll = self.preview_scroll.saturating_add(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Enter | KeyCode::Tab => self.select(),
            KeyCode::Char('f') if key.modifiers == KeyModifiers::CONTROL => self.toggle_scope(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(character);
                self.refresh_visible();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<RecentPromptPickerEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_visible();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn toggle_scope(&mut self) -> ComponentUpdate<RecentPromptPickerEffect> {
        self.scope = match self.scope {
            RecentPromptScope::Global => RecentPromptScope::CurrentSession,
            RecentPromptScope::CurrentSession => RecentPromptScope::Global,
        };
        self.refresh_visible();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn refresh_visible(&mut self) {
        let query = self.query.to_ascii_lowercase();
        self.visible = self
            .prompts
            .iter()
            .enumerate()
            .filter_map(|(index, prompt)| {
                if self.scope == RecentPromptScope::CurrentSession
                    && prompt.session_id != self.current_session_id
                {
                    return None;
                }
                fuzzy_score(&prompt.text, &query).map(|_| index)
            })
            .collect::<Vec<_>>();
        self.selected = 0;
        self.preview_scroll = 0;
    }

    fn select(&self) -> ComponentUpdate<RecentPromptPickerEffect> {
        let Some(prompt) = self.selected_prompt() else {
            return ComponentUpdate::none();
        };
        Self::effect(RecentPromptPickerEffect::Insert(prompt.text.clone()))
    }

    fn selected_prompt(&self) -> Option<&RecentPrompt> {
        let index = self.visible.get(self.selected)?;
        self.prompts.get(*index)
    }

    fn effect(effect: RecentPromptPickerEffect) -> ComponentUpdate<RecentPromptPickerEffect> {
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn render_scope(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let scope = match self.scope {
            RecentPromptScope::Global => "Global",
            RecentPromptScope::CurrentSession => "Current session",
        };
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme.border()));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("Scope: ", Style::default().fg(theme.muted())),
                Span::styled(
                    scope,
                    Style::default()
                        .fg(theme.text())
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .block(block),
            area,
        );
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let query = visible_query_tail(&self.query, query_width);
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

    fn render_prompts(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        if self.visible.is_empty() {
            let message = if self.query.is_empty() {
                "No prompts in this scope"
            } else {
                "No prompts match"
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }

        let items = self.visible.iter().enumerate().map(|(position, index)| {
            let prompt = &self.prompts[*index];
            let mut spans = vec![Span::styled(
                format!("{}. {}", position + 1, one_line_preview(&prompt.text)),
                Style::default().fg(theme.text()),
            )];
            if self.scope == RecentPromptScope::Global {
                spans.push(Span::styled(
                    format!("  · {} · {}", prompt.workspace.display(), prompt.session_id),
                    Style::default().fg(theme.muted()),
                ));
            }
            ListItem::new(Line::from(spans))
        });
        let list = List::new(items)
            .highlight_symbol("› ")
            .highlight_style(Style::default().fg(theme.accent()));
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_preview(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::TOP)
            .title(" Preview ")
            .border_style(Style::default().fg(theme.border()))
            .title_style(Style::default().fg(theme.muted()));
        let text = self
            .selected_prompt()
            .map_or("", |prompt| prompt.text.as_str());
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(theme.text()))
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((self.preview_scroll, 0)),
            area,
        );
    }
}

impl Component for RecentPromptPicker {
    type Event = RecentPromptPickerEvent;
    type Effect = RecentPromptPickerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            RecentPromptPickerEvent::Terminal(Event::Key(key)) => self.update_key(key),
            RecentPromptPickerEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            RecentPromptPickerEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let layout =
            Floating::new("Recent prompts", 82, 22, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }

        let search_area = Rect {
            height: layout.body.height.min(1),
            ..layout.body
        };
        let scope_area = Rect {
            y: search_area.bottom(),
            height: layout.body.height.saturating_sub(search_area.height).min(2),
            ..layout.body
        };
        let remaining_height = layout
            .body
            .height
            .saturating_sub(search_area.height + scope_area.height);
        let list_height = LIST_HEIGHT.min(remaining_height.saturating_add(1) / 2);
        let list_area = Rect {
            y: scope_area.bottom(),
            height: list_height,
            ..layout.body
        };
        let preview_area = Rect {
            y: list_area.bottom(),
            height: remaining_height.saturating_sub(list_height),
            ..layout.body
        };

        self.render_search(frame, search_area, theme);
        self.render_scope(frame, scope_area, theme);
        self.render_prompts(frame, list_area, theme);
        self.render_preview(frame, preview_area, theme);
    }
}

fn one_line_preview(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
