//! Local review scope selection. Repository resolution belongs to the agent.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{review::Target, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const CHOICES: [(&str, &str); 4] = [
    ("Base branch", "Changes since the merge-base with a ref"),
    ("Uncommitted", "Staged, unstaged, and untracked changes"),
    ("Commit", "Changes introduced by one commit"),
    ("Custom", "Choose a review scope or focus"),
];
const SCOPE_KEYS: [(&str, &str); 3] = [("↑↓", "select"), ("enter", "continue"), ("esc", "cancel")];
const INPUT_KEYS: [(&str, &str); 2] = [("enter", "review"), ("esc", "back")];

#[derive(Debug, Eq, PartialEq)]
pub(super) enum CodeReviewEffect {
    Run(Target),
    Dismiss,
}

pub(super) struct CodeReviewSelector {
    selected: usize,
    entering: bool,
    input: String,
    // A byte offset that always lies on an extended grapheme boundary.
    cursor: usize,
    error: Option<&'static str>,
}

impl CodeReviewSelector {
    pub(super) fn new() -> Self {
        Self {
            selected: 0,
            entering: false,
            input: String::new(),
            cursor: 0,
            error: None,
        }
    }

    fn effect(effect: CodeReviewEffect) -> ComponentUpdate<CodeReviewEffect> {
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<CodeReviewEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        if key.code == KeyCode::Esc {
            if self.entering {
                self.entering = false;
                self.error = None;
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            return Self::effect(CodeReviewEffect::Dismiss);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Self::effect(CodeReviewEffect::Dismiss);
        }
        if self.entering {
            return self.update_input(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Left => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Right => {
                self.selected = (self.selected + 1).min(CHOICES.len() - 1);
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = CHOICES.len() - 1,
            KeyCode::Enter => {
                if self.selected == 1 {
                    return Self::effect(CodeReviewEffect::Run(Target::Uncommitted));
                }
                self.entering = true;
                self.input.clear();
                self.cursor = 0;
                self.error = None;
            }
            KeyCode::Backspace => return Self::effect(CodeReviewEffect::Dismiss),
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_input(&mut self, key: KeyEvent) -> ComponentUpdate<CodeReviewEffect> {
        match key.code {
            KeyCode::Enter => return self.submit(),
            KeyCode::Left => self.cursor = self.previous_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::Backspace => {
                let start = self.previous_boundary();
                self.input.replace_range(start..self.cursor, "");
                self.cursor = start;
                self.normalize_cursor();
                self.error = None;
            }
            KeyCode::Delete => {
                let end = self.next_boundary();
                self.input.replace_range(self.cursor..end, "");
                self.normalize_cursor();
                self.error = None;
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => self.cursor = 0,
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.input.len();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                self.cursor = 0;
                self.error = None;
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.truncate(self.cursor);
                self.error = None;
            }
            KeyCode::Char(character)
                if !character.is_control()
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
            {
                self.insert(&character.to_string());
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn previous_boundary(&self) -> usize {
        self.input
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .take_while(|&index| index < self.cursor)
            .last()
            .unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.input
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|&index| index > self.cursor)
            .unwrap_or(self.input.len())
    }

    fn normalize_cursor(&mut self) {
        // Combining marks and joiners can merge with text on either side of an edit.
        self.cursor = self
            .input
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|&index| index >= self.cursor)
            .unwrap_or(self.input.len());
    }

    fn insert(&mut self, text: &str) {
        // Keep a single-line input without allowing terminal control characters.
        let text: String = text
            .replace("\r\n", "\n")
            .chars()
            .filter_map(|character| match character {
                '\n' | '\r' | '\t' => Some(' '),
                character if !character.is_control() => Some(character),
                _ => None,
            })
            .collect();
        self.input.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.normalize_cursor();
        self.error = None;
    }

    fn submit(&mut self) -> ComponentUpdate<CodeReviewEffect> {
        let value = self.input.trim();
        if value.is_empty() {
            self.error = Some(match self.selected {
                0 => "Enter a base branch or ref.",
                2 => "Enter a commit ref.",
                _ => "Enter a review scope or focus.",
            });
        } else if self.selected != 3
            && (value.starts_with('-') || value.chars().any(char::is_whitespace))
        {
            self.error = Some("Enter one ref without whitespace or a leading '-'.");
        } else {
            let target = match self.selected {
                0 => Target::Base(value.to_owned()),
                2 => Target::Commit(value.to_owned()),
                _ => Target::Custom(value.to_owned()),
            };
            return Self::effect(CodeReviewEffect::Run(target));
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render_input(&self, frame: &mut Frame<'_>, body: Rect, theme: &Theme) {
        if body.is_empty() {
            return;
        }
        let (label, hint) = match self.selected {
            0 => (
                "Base branch or ref",
                "Enter a ref such as main or origin/main.",
            ),
            2 => ("Commit ref", "Enter a commit SHA or ref such as HEAD."),
            _ => (
                "Review scope or focus",
                "Describe the changes or behavior to review.",
            ),
        };
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(theme.text())),
            Rect { height: 1, ..body },
        );
        if body.height < 2 {
            return;
        }
        let field = Rect {
            y: body.y + 1,
            height: 1,
            ..body
        };
        // Scroll by whole graphemes and leave a cell for the insertion cursor.
        let budget = usize::from(field.width.saturating_sub(1));
        let mut start = self.cursor;
        let mut width = 0;
        for (index, grapheme) in self.input[..self.cursor].grapheme_indices(true).rev() {
            let next_width = width + grapheme.width();
            if next_width > budget {
                break;
            }
            start = index;
            width = next_width;
        }
        frame.render_widget(
            Paragraph::new(self.input[start..].to_owned())
                .style(Style::default().fg(theme.accent())),
            field,
        );
        frame.set_cursor_position(Position::new(field.x + width as u16, field.y));
        if body.height > 3 {
            let message = self.error.unwrap_or(hint);
            let style = if self.error.is_some() {
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted())
            };
            frame.render_widget(
                Paragraph::new(message)
                    .style(style)
                    .wrap(Wrap { trim: false }),
                Rect {
                    y: body.y + 3,
                    height: body.height - 3,
                    ..body
                },
            );
        }
    }
}

impl Component for CodeReviewSelector {
    type Event = Event;
    type Effect = CodeReviewEffect;

    fn update(&mut self, event: Event) -> ComponentUpdate<Self::Effect> {
        match event {
            Event::Key(key) => self.update_key(key),
            Event::Paste(text) if self.entering => {
                self.insert(&text);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        let title = if self.entering {
            CHOICES[self.selected].0
        } else {
            "Review"
        };
        let keys = if self.entering {
            &INPUT_KEYS[..]
        } else {
            &SCOPE_KEYS[..]
        };
        let layout = Floating::new(title, 68, 10, keys).render(frame, area, theme);
        if self.entering {
            self.render_input(frame, layout.body, theme);
            return;
        }
        let items = CHOICES.iter().map(|(label, detail)| {
            ListItem::new(Line::from(vec![
                Span::raw(format!("{label:<14}")),
                Span::styled(*detail, Style::default().fg(theme.muted())),
            ]))
        });
        let list = List::new(items)
            .style(Style::default().fg(theme.text()))
            .highlight_symbol("› ")
            .highlight_style(
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_stateful_widget(
            list,
            layout.body,
            &mut ListState::default().with_selected(Some(self.selected)),
        );
    }
}
