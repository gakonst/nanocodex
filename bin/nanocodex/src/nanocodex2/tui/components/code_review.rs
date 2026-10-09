//! Review scope selection and searchable workspace branches.

use super::{
    file_finder::fuzzy_score,
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::nanocodex2::tui::{
    review::{Branch, Target},
    theme::Theme,
};
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
const BRANCH_KEYS: [(&str, &str); 4] = [
    ("type", "filter"),
    ("↑↓", "select"),
    ("enter", "review"),
    ("esc", "back"),
];
const INPUT_KEYS: [(&str, &str); 2] = [("enter", "review"), ("esc", "back")];

#[derive(Debug, Eq, PartialEq)]
pub(super) enum CodeReviewEffect {
    Run(Target),
    LoadBranches(uuid::Uuid),
    Dismiss,
}

pub(super) struct CodeReviewSelector {
    selected: usize,
    entering: bool,
    input: String,
    // A byte offset that always lies on an extended grapheme boundary.
    cursor: usize,
    error: Option<&'static str>,
    branch_request: Option<uuid::Uuid>,
    branches: Option<Result<Vec<Branch>, String>>,
    matches: Vec<usize>,
    branch_selected: usize,
}

impl CodeReviewSelector {
    pub(super) fn new() -> Self {
        Self {
            selected: 0,
            entering: false,
            input: String::new(),
            cursor: 0,
            error: None,
            branch_request: None,
            branches: None,
            matches: Vec::new(),
            branch_selected: 0,
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
            if self.selected == 0 {
                return self.update_branches(key);
            }
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
                if self.selected == 0 {
                    let request_id = uuid::Uuid::new_v4();
                    self.branch_request = Some(request_id);
                    self.branches = None;
                    self.matches.clear();
                    self.branch_selected = 0;
                    return Self::effect(CodeReviewEffect::LoadBranches(request_id));
                }
            }
            KeyCode::Backspace => return Self::effect(CodeReviewEffect::Dismiss),
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    pub(super) fn branches_loaded(
        &mut self,
        request_id: uuid::Uuid,
        result: Result<Vec<Branch>, String>,
    ) -> ComponentUpdate<CodeReviewEffect> {
        if self.branch_request != Some(request_id) || !self.entering || self.selected != 0 {
            return ComponentUpdate::none();
        }
        self.branches = Some(result);
        self.refresh_branches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn refresh_branches(&mut self) {
        self.matches.clear();
        if let Some(Ok(branches)) = &self.branches {
            let query = self.input.to_lowercase();
            let mut matches: Vec<_> = branches
                .iter()
                .enumerate()
                .filter_map(|(index, branch)| {
                    fuzzy_score(&branch.name.to_lowercase(), &query).map(|score| (index, score))
                })
                .collect();
            matches.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
            self.matches = matches.into_iter().map(|(index, _)| index).collect();
        }
        self.branch_selected = 0;
    }

    fn update_branches(&mut self, key: KeyEvent) -> ComponentUpdate<CodeReviewEffect> {
        match key.code {
            KeyCode::Enter => {
                if let Some(Ok(branches)) = &self.branches
                    && let Some(index) = self.matches.get(self.branch_selected)
                {
                    return Self::effect(CodeReviewEffect::Run(Target::Base(
                        branches[*index].reference.clone(),
                    )));
                }
                return ComponentUpdate::none();
            }
            KeyCode::Up => self.branch_selected = self.branch_selected.saturating_sub(1),
            KeyCode::Down => {
                self.branch_selected =
                    (self.branch_selected + 1).min(self.matches.len().saturating_sub(1))
            }
            KeyCode::Home => self.branch_selected = 0,
            KeyCode::End => self.branch_selected = self.matches.len().saturating_sub(1),
            _ => {
                let before = self.input.clone();
                let update = self.update_input(key);
                if self.input != before {
                    self.refresh_branches();
                }
                return update;
            }
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
                2 => "Enter a commit ref.",
                _ => "Enter a review scope or focus.",
            });
        } else if self.selected != 3
            && (value.starts_with('-') || value.chars().any(char::is_whitespace))
        {
            self.error = Some("Enter one ref without whitespace or a leading '-'.");
        } else {
            let target = match self.selected {
                2 => Target::Commit(value.to_owned()),
                _ => Target::Custom(value.to_owned()),
            };
            return Self::effect(CodeReviewEffect::Run(target));
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render_branches(&self, frame: &mut Frame<'_>, body: Rect, theme: &Theme) {
        // Reuse the grapheme-aware single-line editor above the results.
        self.render_input(
            frame,
            Rect {
                height: body.height.min(2),
                ..body
            },
            theme,
        );
        let list_area = Rect {
            y: body.y + body.height.min(3),
            height: body.height.saturating_sub(3),
            ..body
        };
        if list_area.is_empty() {
            return;
        }
        let message = match &self.branches {
            None => Some("Loading branches…".to_owned()),
            Some(Err(error)) => Some(format!(
                "Could not load branches. {error} You can also use /review --base <ref>."
            )),
            Some(Ok(branches)) if branches.is_empty() => Some(
                "No branches found. Create or fetch a branch, then reopen this picker.".to_owned(),
            ),
            Some(Ok(_)) if self.matches.is_empty() => Some("No matching branches.".to_owned()),
            Some(Ok(_)) => None,
        };
        if let Some(message) = message {
            frame.render_widget(
                Paragraph::new(message)
                    .style(Style::default().fg(theme.muted()))
                    .wrap(Wrap { trim: false }),
                list_area,
            );
            return;
        }
        let Some(Ok(branches)) = &self.branches else {
            return;
        };
        let items = self.matches.iter().map(|index| {
            let branch = &branches[*index];
            ListItem::new(Line::from(vec![
                Span::raw(&branch.name),
                Span::styled(
                    if branch.current { " (current)" } else { "" },
                    Style::default().fg(theme.muted()),
                ),
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
            list_area,
            &mut ListState::default().with_selected(Some(self.branch_selected)),
        );
    }

    fn render_input(&self, frame: &mut Frame<'_>, body: Rect, theme: &Theme) {
        if body.is_empty() {
            return;
        }
        let (label, hint) = match self.selected {
            0 => ("Search branches", "Type to filter branches."),
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
                if self.selected == 0 {
                    self.refresh_branches();
                }
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
        let choosing_branch = self.entering && self.selected == 0;
        let keys = if choosing_branch {
            &BRANCH_KEYS[..]
        } else if self.entering {
            &INPUT_KEYS[..]
        } else {
            &SCOPE_KEYS[..]
        };
        let layout = Floating::new(title, 68, if choosing_branch { 17 } else { 10 }, keys)
            .render(frame, area, theme);
        if choosing_branch {
            self.render_branches(frame, layout.body, theme);
            return;
        }
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
