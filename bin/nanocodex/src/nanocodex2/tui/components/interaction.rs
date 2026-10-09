//! Modal surface for Claude host requests: AskUserQuestion, ExitPlanMode plan
//! approval and tool permission asks (see config/claude/interaction.rs).
//!
//! Answers only come from the terminal owner. The overlay keeps the legacy
//! answer grammar of [crate::config::PendingInteraction::respond] (option
//! numbers, "other: text", "approve", "deny", "/cancel") and adds arrow-key
//! option selection. Blank input never approves, and the overlay starts with an
//! empty answer so a stale composer draft cannot answer a new request.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, PoisonError},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use super::floating::Floating;
use crate::config::PendingInteraction;
use crate::nanocodex2::tui::{
    features::{FeatureOverlay, OverlayOutcome},
    theme::Theme,
};

/// How a request left the overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InteractionOutcome {
    /// The answer (as typed or selected) was delivered to the tool.
    Answered(String),
    /// The user cancelled; the tool received a cancellation error.
    Cancelled,
    /// The producer withdrew the request (turn cancelled, timeout).
    Withdrawn,
}

/// Request shared between the overlay and the feature task that watches it.
pub(crate) type SharedInteraction = Arc<Mutex<PendingInteraction>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Question,
    Plan,
    Permission,
}

pub(crate) struct InteractionOverlay {
    request: SharedInteraction,
    kind: Kind,
    input: String,
    error: Option<String>,
    selected: Option<usize>,
    chosen: BTreeSet<usize>,
    scroll: u16,
    finish: Option<Box<dyn FnOnce(InteractionOutcome) + Send>>,
}

impl InteractionOverlay {
    pub(crate) fn new(
        request: SharedInteraction,
        finish: impl FnOnce(InteractionOutcome) + Send + 'static,
    ) -> Self {
        let kind = {
            let request = request.lock().unwrap_or_else(PoisonError::into_inner);
            if request.question.is_some() {
                Kind::Question
            } else if request.prompt.starts_with("Plan approval required") {
                Kind::Plan
            } else {
                Kind::Permission
            }
        };
        Self {
            request,
            kind,
            input: String::new(),
            error: None,
            selected: None,
            chosen: BTreeSet::new(),
            scroll: 0,
            finish: Some(Box::new(finish)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PendingInteraction> {
        self.request.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn options(&self) -> Vec<(String, String)> {
        self.lock()
            .question
            .as_ref()
            .map_or_else(Vec::new, |question| {
                question
                    .options
                    .iter()
                    .map(|option| (option.label.clone(), option.description.clone()))
                    .collect()
            })
    }

    fn multi_select(&self) -> bool {
        self.lock()
            .question
            .as_ref()
            .is_some_and(|question| question.multi_select)
    }

    fn done(&mut self, outcome: InteractionOutcome) -> OverlayOutcome {
        if let Some(finish) = self.finish.take() {
            finish(outcome);
        }
        OverlayOutcome::Close
    }

    /// The answer text Enter submits: typed text, else the selected options.
    fn answer(&self) -> String {
        let typed = self.input.trim();
        if !typed.is_empty() || self.kind != Kind::Question {
            return typed.to_owned();
        }
        if !self.chosen.is_empty() {
            return self
                .chosen
                .iter()
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(",");
        }
        self.selected
            .map(|index| (index + 1).to_string())
            .unwrap_or_default()
    }

    fn submit(&mut self) -> OverlayOutcome {
        let answer = self.answer();
        let result = {
            let mut request = self.lock();
            if request.is_closed() {
                None
            } else {
                Some(request.respond(&answer))
            }
        };
        match result {
            None => {
                self.error = Some("The request was cancelled; the answer was discarded".into());
                self.done(InteractionOutcome::Withdrawn)
            }
            Some(Ok(())) if answer == "/cancel" => self.done(InteractionOutcome::Cancelled),
            Some(Ok(())) => {
                let shown = self.display_answer(&answer);
                self.done(InteractionOutcome::Answered(shown))
            }
            Some(Err(error)) => {
                if self.lock().is_closed() {
                    return self.done(InteractionOutcome::Withdrawn);
                }
                self.error = Some(error);
                OverlayOutcome::Consumed
            }
        }
    }

    /// Option numbers are shown with their labels in the transcript notice.
    fn display_answer(&self, answer: &str) -> String {
        if self.kind != Kind::Question || answer.starts_with("other:") {
            return answer.to_owned();
        }
        let options = self.options();
        let labels = answer
            .split(',')
            .filter_map(|part| part.trim().parse::<usize>().ok())
            .filter_map(|index| options.get(index.checked_sub(1)?))
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        if labels.is_empty() {
            answer.to_owned()
        } else {
            labels.join(", ")
        }
    }

    fn cancel(&mut self) -> OverlayOutcome {
        let closed = {
            let mut request = self.lock();
            let closed = request.is_closed();
            if !closed {
                drop(request.respond("/cancel"));
            }
            closed
        };
        self.done(if closed {
            InteractionOutcome::Withdrawn
        } else {
            InteractionOutcome::Cancelled
        })
    }

    fn move_selection(&mut self, down: bool) {
        let count = self.options().len();
        if count == 0 {
            return;
        }
        self.selected = Some(match (self.selected, down) {
            (None, true) => 0,
            (None, false) => count - 1,
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
        });
    }

    fn title(&self) -> &'static str {
        match self.kind {
            Kind::Question => "Claude asks",
            Kind::Plan => "Plan approval",
            Kind::Permission => "Tool permission",
        }
    }

    fn bindings(&self) -> &'static [(&'static str, &'static str)] {
        match (self.kind, self.multi_select()) {
            (Kind::Question, true) => &[
                ("↑↓", "choose"),
                ("space", "toggle"),
                ("enter", "answer"),
                ("pgup/pgdn", "scroll"),
                ("ctrl+c", "cancel request"),
            ],
            (Kind::Question, false) => &[
                ("↑↓", "choose"),
                ("enter", "answer"),
                ("pgup/pgdn", "scroll"),
                ("ctrl+c", "cancel request"),
            ],
            _ => &[
                ("approve/deny", "+ enter"),
                ("pgup/pgdn", "scroll"),
                ("ctrl+c", "cancel request"),
            ],
        }
    }
}

impl FeatureOverlay for InteractionOverlay {
    fn name(&self) -> &'static str {
        "claude_interaction"
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let width = area.width.saturating_sub(4).clamp(20, 110);
        let height = area.height.saturating_sub(2).clamp(8, 40);
        let bindings = self.bindings();
        let layout = Floating::new(self.title(), width, height, bindings)
            .colors(theme.accent(), theme.accent())
            .render(frame, area, theme);
        let options = self.options();
        let option_rows = u16::try_from(options.len()).unwrap_or(u16::MAX);
        let [body, choices, answer, error] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(if options.is_empty() {
                0
            } else {
                option_rows.saturating_add(1)
            }),
            Constraint::Length(1),
            Constraint::Length(u16::from(self.error.is_some())),
        ])
        .areas(layout.body);
        let prompt = self.lock().prompt().to_owned();
        let lines = prompt
            .lines()
            .map(|line| Line::from(line.to_owned()))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            body,
        );
        if !options.is_empty() {
            let multi = self.multi_select();
            let rows = options
                .iter()
                .enumerate()
                .map(|(index, (label, description))| {
                    let current = self.selected == Some(index);
                    let mark = if multi {
                        if self.chosen.contains(&index) {
                            "[x] "
                        } else {
                            "[ ] "
                        }
                    } else if current {
                        "› "
                    } else {
                        "  "
                    };
                    let style = if current {
                        Style::default()
                            .fg(theme.accent())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    Line::from(vec![
                        Span::styled(format!("{mark}{}. {label}", index + 1), style),
                        Span::styled(
                            format!("  {description}"),
                            Style::default().fg(theme.muted()),
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            let [_, list] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(choices);
            frame.render_widget(Paragraph::new(rows), list);
        }
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("› ", Style::default().fg(theme.accent())),
                Span::raw(self.input.clone()),
                Span::styled("▏", Style::default().fg(theme.muted())),
            ])),
            answer,
        );
        if let Some(message) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    message.clone(),
                    Style::default().fg(Color::Red),
                )),
                error,
            );
        }
    }

    fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return OverlayOutcome::Consumed;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => self.cancel(),
                KeyCode::Char('u') => {
                    self.input.clear();
                    OverlayOutcome::Consumed
                }
                _ => OverlayOutcome::Consumed,
            };
        }
        match key.code {
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => self.input.clear(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up => self.move_selection(false),
            KeyCode::Down => self.move_selection(true),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5),
            KeyCode::Char(' ') if self.input.is_empty() && self.multi_select() => {
                if let Some(index) = self.selected
                    && !self.chosen.remove(&index)
                {
                    self.chosen.insert(index);
                }
            }
            KeyCode::Char(character) if self.input.len() < 8192 => {
                self.input.push(character);
            }
            _ => {}
        }
        self.error = None;
        OverlayOutcome::Consumed
    }

    fn discards_draft(&self) -> bool {
        true
    }

    fn paste(&mut self, text: &str) {
        for character in text.chars().filter(|character| !character.is_control()) {
            if self.input.len() >= 8192 {
                break;
            }
            self.input.push(character);
        }
    }
}
