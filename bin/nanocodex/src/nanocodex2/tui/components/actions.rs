// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Searchable modal menu for actions exposed by the TUI.

use super::{
    composer::SettingsCommand,
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACTIONS: [Action; 9] = [
    Action::Effort,
    Action::FastMode,
    Action::Theme,
    Action::NewSession,
    Action::ResumeSession,
    Action::Keybindings,
    Action::DebugContext,
    Action::Reflection,
    Action::Model,
];
const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "move"), ("enter/tab", "open"), ("esc", "close")];
const SEARCH_LABEL: &str = "Search: ";
const SELECTION_MARKER: &str = "› ";

pub(super) enum ActionsEvent {
    Terminal(Event),
}

pub(super) struct ActionAvailability {
    pub(super) new_session: bool,
    pub(super) fork: bool,
    pub(super) fast_mode: bool,
    pub(super) model: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Handoff,
    Review,
    Effort,
    Model,
    FastMode,
    Theme,
    NewSession,
    ResumeSession,
    Fork,
    Keybindings,
    ReloadConfig,
    EditConfig,
    DebugContext,
    Reflection,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ActionsEffect {
    Dismiss,
    Trigger(Action),
    Settings(SettingsCommand),
}

pub(super) struct ActionsMenu {
    query: String,
    selected: usize,
    matches: Vec<usize>,
    availability: ActionAvailability,
}

impl ActionsMenu {
    pub(super) fn new(availability: ActionAvailability) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            matches: (0..ACTIONS.len()).collect(),
            availability,
        }
    }

    pub(super) fn set_fork_available(&mut self, available: bool) {
        self.availability.fork = available;
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<ActionsEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::Esc => Self::dismiss(),
            KeyCode::Backspace if !self.query.is_empty() => {
                self.remove_last_grapheme();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::dismiss(),
            KeyCode::Enter | KeyCode::Tab => self.trigger_selected(),
            KeyCode::Up => {
                self.select_previous();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                self.select_next();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
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

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<ActionsEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_matches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn dismiss() -> ComponentUpdate<ActionsEffect> {
        ComponentUpdate {
            effects: vec![ActionsEffect::Dismiss],
            render: RenderRequest::Immediate,
        }
    }

    fn remove_last_grapheme(&mut self) {
        let Some((index, _)) = self.query.grapheme_indices(true).next_back() else {
            return;
        };
        self.query.truncate(index);
        self.refresh_matches();
    }

    fn refresh_matches(&mut self) {
        self.matches.clear();
        self.matches.extend(
            ACTIONS
                .iter()
                .enumerate()
                .filter(|(_, action)| action.matches(&self.query))
                .map(|(index, _)| index),
        );
        self.selected = 0;
    }

    fn select_previous(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    fn select_next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.matches.len() - 1);
    }

    fn trigger_selected(&self) -> ComponentUpdate<ActionsEffect> {
        if let Some(command) = SettingsCommand::parse(&format!("/{}", self.query)) {
            return ComponentUpdate {
                effects: vec![ActionsEffect::Settings(command)],
                render: RenderRequest::Immediate,
            };
        }
        let Some(action) = self.matches.get(self.selected) else {
            return ComponentUpdate::none();
        };
        self.trigger(ACTIONS[*action])
    }

    fn trigger(&self, action: Action) -> ComponentUpdate<ActionsEffect> {
        if !self.is_enabled(action) {
            return ComponentUpdate::none();
        }
        ComponentUpdate {
            effects: vec![ActionsEffect::Trigger(action)],
            render: RenderRequest::Immediate,
        }
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let visible_query = visible_query_tail(&self.query, query_width);
        let label_style = Style::default().fg(theme.muted());
        let line = Line::from(vec![
            Span::styled(marker, label_style),
            Span::styled(SEARCH_LABEL, label_style),
            Span::styled(visible_query, Style::default().fg(theme.text())),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_actions(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let items = self.matches.iter().enumerate().map(|(row, index)| {
            let action = ACTIONS[*index];
            let enabled = self.is_enabled(action);
            let selected = row == self.selected;
            let label_color = if !enabled {
                theme.muted()
            } else if selected {
                theme.accent()
            } else {
                theme.text()
            };
            let mut spans = vec![Span::styled(
                self.display_label(action),
                Style::default().fg(label_color),
            )];
            if let Some(alias) = action.alias() {
                spans.push(Span::styled(
                    format!(" (alias: {alias})"),
                    Style::default().fg(theme.muted()),
                ));
            }
            ListItem::new(Line::from(spans))
        });
        let selected_enabled = self
            .matches
            .get(self.selected)
            .is_some_and(|index| self.is_enabled(ACTIONS[*index]));
        let highlight = if selected_enabled {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let list = List::new(items)
            .style(Style::default().fg(theme.text()))
            .highlight_style(highlight)
            .highlight_symbol(SELECTION_MARKER);
        let selected = (!self.matches.is_empty()).then_some(self.selected);
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, area, &mut state);
    }

    const fn is_enabled(&self, action: Action) -> bool {
        match action {
            Action::Handoff | Action::Review | Action::Reflection => self.availability.new_session,
            Action::Effort | Action::FastMode => true,
            Action::Model => self.availability.model,
            Action::Theme => true,
            Action::NewSession => self.availability.new_session,
            Action::ResumeSession => self.availability.new_session,
            Action::Fork => self.availability.fork,
            Action::Keybindings => true,
            Action::ReloadConfig => true,
            Action::EditConfig => true,
            Action::DebugContext => true,
        }
    }

    const fn display_label(&self, action: Action) -> &'static str {
        match action {
            Action::NewSession if !self.availability.new_session => {
                "New session · finish active work first"
            }
            Action::ResumeSession if !self.availability.new_session => {
                "Resume session · finish active work first"
            }
            Action::Fork if !self.availability.fork => "Fork session · one fork at a time",
            Action::Review if !self.availability.new_session => {
                "Review changes · finish active work first"
            }
            Action::Handoff if !self.availability.new_session => {
                "Prepare handoff · finish active work first"
            }
            Action::Reflection if !self.availability.new_session => {
                "Reflect on session · finish active work first"
            }
            Action::FastMode if self.availability.fast_mode => "Disable fast mode",
            Action::Model if !self.availability.model => "Select model · start a new session first",
            _ => action.label(),
        }
    }
}

impl Action {
    const fn label(self) -> &'static str {
        match self {
            Self::Handoff => "Prepare handoff",
            Self::Review => "Review changes",
            Self::Effort => "Change effort",
            Self::Model => "Select model",
            Self::FastMode => "Enable fast mode",
            Self::Theme => "Select theme",
            Self::NewSession => "New session",
            Self::ResumeSession => "Resume session",
            Self::Fork => "Fork session",
            Self::Keybindings => "Keyboard shortcuts",
            Self::ReloadConfig => "Reload config",
            Self::EditConfig => "Edit config",
            Self::DebugContext => "Debug context",
            Self::Reflection => "Reflect on session",
        }
    }

    const fn alias(self) -> Option<&'static str> {
        match self {
            Self::Handoff => Some("handoff"),
            Self::Review => Some("review"),
            Self::Effort => Some("thinking"),
            Self::Model => Some("intelligence"),
            Self::FastMode => Some("priority"),
            Self::Theme => Some("appearance"),
            Self::NewSession => Some("clear"),
            Self::ResumeSession => Some("restore"),
            Self::Fork => Some("btw"),
            Self::ReloadConfig => Some("refresh"),
            Self::Reflection => Some("reflection"),
            Self::Keybindings | Self::EditConfig | Self::DebugContext => None,
        }
    }

    fn matches(self, query: &str) -> bool {
        contains_ignore_ascii_case(self.label(), query)
            || self
                .alias()
                .is_some_and(|alias| contains_ignore_ascii_case(alias, query))
    }
}

impl Component for ActionsMenu {
    type Event = ActionsEvent;
    type Effect = ActionsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ActionsEvent::Terminal(Event::Key(key)) => self.update_key(key),
            ActionsEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            ActionsEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let layout = Floating::new("Actions", 58, 19, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search_area = Rect {
            height: 1,
            ..layout.body
        };
        let actions_area = Rect {
            y: layout.body.y + 1,
            height: layout.body.height.saturating_sub(1),
            ..layout.body
        };
        self.render_search(frame, search_area, theme);
        self.render_actions(frame, actions_area, theme);
    }
}

fn contains_ignore_ascii_case(value: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if query.len() > value.len() {
        return false;
    }
    value
        .as_bytes()
        .windows(query.len())
        .any(|window| window.eq_ignore_ascii_case(query.as_bytes()))
}

fn visible_query_tail(query: &str, width: usize) -> &str {
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
        Action, ActionAvailability, ActionsEffect, ActionsEvent, ActionsMenu, Component,
        SettingsCommand,
    };
    use crate::config::ReasoningEffort;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use nanocodex::Model;

    fn availability(fast_mode: bool, model: bool) -> ActionAvailability {
        ActionAvailability {
            new_session: true,
            fork: true,
            fast_mode,
            model,
        }
    }

    #[test]
    fn fast_mode_uses_priority_alias_and_dynamic_label() {
        assert_eq!(Action::FastMode.alias(), Some("priority"));
        let enabled = ActionsMenu::new(availability(true, true));
        assert_eq!(enabled.display_label(Action::FastMode), "Disable fast mode");
        let disabled = ActionsMenu::new(availability(false, true));
        assert_eq!(disabled.display_label(Action::FastMode), "Enable fast mode");
    }

    #[test]
    fn priority_search_triggers_fast_mode() {
        let mut menu = ActionsMenu::new(availability(false, true));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE,
        ))));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        ))));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::NONE,
        ))));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::NONE,
        ))));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        ))));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::NONE,
        ))));
        menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::NONE,
        ))));
        let update = menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))));
        assert_eq!(update.effects, [ActionsEffect::Trigger(Action::FastMode)]);
    }

    #[test]
    fn direct_settings_queries_trigger_even_without_an_action_match() {
        let mut menu = ActionsMenu::new(availability(false, true));
        menu.update(ActionsEvent::Terminal(Event::Paste(
            "model astra".to_owned(),
        )));
        let update = menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            update.effects,
            [ActionsEffect::Settings(SettingsCommand::SetModel(
                Model::Astra
            ))]
        );

        let mut menu = ActionsMenu::new(availability(false, true));
        menu.update(ActionsEvent::Terminal(Event::Paste(
            "reasoning max".to_owned(),
        )));
        let update = menu.update(ActionsEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            update.effects,
            [ActionsEffect::Settings(SettingsCommand::SetEffort(
                ReasoningEffort::Max
            ))]
        );
    }

    #[test]
    fn only_model_action_is_disabled_after_session_starts() {
        let menu = ActionsMenu::new(availability(false, false));
        assert_eq!(
            menu.display_label(Action::Model),
            "Select model · start a new session first"
        );
        assert!(!menu.is_enabled(Action::Model));
        assert!(menu.is_enabled(Action::Effort));
        assert!(menu.is_enabled(Action::FastMode));
    }
}
