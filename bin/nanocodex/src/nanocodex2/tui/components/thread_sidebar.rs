//! Persistent account thread navigation. Refreshes never move the user's selection.

use crate::tui::{session::SessionSummary, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

pub(super) enum SidebarAction {
    Select(String),
    Search,
    Refresh,
    Changed,
}

pub(super) struct ThreadSidebar {
    pub(super) current: String,
    sessions: Vec<SessionSummary>,
    state: ListState,
    area: Rect,
    rows: Rect,
    visible: bool,
    focused: bool,
    error: Option<String>,
    loaded: bool,
}

impl Default for ThreadSidebar {
    fn default() -> Self {
        Self {
            current: String::new(),
            sessions: Vec::new(),
            state: ListState::default(),
            area: Rect::default(),
            rows: Rect::default(),
            visible: true,
            focused: false,
            error: None,
            loaded: false,
        }
    }
}

impl ThreadSidebar {
    pub(super) fn loaded(&mut self, mut sessions: Vec<SessionSummary>) {
        let selected = self.selected().map(str::to_owned);
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at_unix_ms));
        // Preserve established order while statuses/titles update; append new threads.
        let mut ordered = Vec::new();
        for previous in &self.sessions {
            if let Some(index) = sessions
                .iter()
                .position(|s| s.session_id == previous.session_id)
            {
                ordered.push(sessions.remove(index));
            }
        }
        ordered.extend(sessions);
        self.sessions = ordered;
        self.loaded = true;
        self.error = None;
        self.state.select(
            selected
                .as_ref()
                .and_then(|id| self.sessions.iter().position(|s| &s.session_id == id))
                .or_else(|| {
                    self.sessions
                        .iter()
                        .position(|s| s.session_id == self.current)
                })
                .or_else(|| (!self.sessions.is_empty()).then_some(0)),
        );
    }

    pub(super) fn failed(&mut self, error: String) {
        self.error = Some(error);
    }

    pub(super) fn selected(&self) -> Option<&str> {
        self.state
            .selected()
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.session_id.as_str())
    }

    pub(super) const fn focused(&self) -> bool {
        self.focused
    }

    pub(super) fn unfocus(&mut self) {
        self.focused = false;
    }

    pub(super) fn select_current(&mut self) {
        self.state.select(
            self.sessions
                .iter()
                .position(|s| s.session_id == self.current),
        );
    }

    fn move_selection(&mut self, delta: isize) {
        if !self.sessions.is_empty() {
            let index = self
                .state
                .selected()
                .unwrap_or(0)
                .saturating_add_signed(delta)
                .min(self.sessions.len() - 1);
            self.state.select(Some(index));
        }
    }

    pub(super) fn event(&mut self, event: &Event, shortcuts: bool) -> Option<SidebarAction> {
        if self.focused && matches!(event, Event::Paste(_)) {
            return Some(SidebarAction::Changed);
        }
        if let Event::Key(key) = event {
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                return None;
            }
            if shortcuts && key.modifiers == KeyModifiers::ALT {
                match key.code {
                    KeyCode::Char('t') => {
                        self.visible = true;
                        self.focused = !self.focused;
                        if self.state.selected().is_none() && !self.sessions.is_empty() {
                            self.state.select(Some(0));
                        }
                        return Some(SidebarAction::Changed);
                    }
                    KeyCode::Char('s') => {
                        self.visible = !self.visible;
                        self.focused = false;
                        return Some(SidebarAction::Changed);
                    }
                    KeyCode::Char('[') | KeyCode::Char(']') => {
                        self.select_current();
                        self.move_selection(if key.code == KeyCode::Char('[') {
                            -1
                        } else {
                            1
                        });
                        return self
                            .selected()
                            .map(|id| SidebarAction::Select(id.to_owned()));
                    }
                    _ => {}
                }
            }
            if !self.focused || !shortcuts {
                return None;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => self.focused = false,
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::Home => self.state.select((!self.sessions.is_empty()).then_some(0)),
                KeyCode::End => self.state.select(self.sessions.len().checked_sub(1)),
                KeyCode::PageUp => self.move_selection(-((self.rows.height / 2).max(1) as isize)),
                KeyCode::PageDown => self.move_selection((self.rows.height / 2).max(1) as isize),
                KeyCode::Enter => {
                    return self
                        .selected()
                        .map(|id| SidebarAction::Select(id.to_owned()));
                }
                KeyCode::Char('/') => {
                    self.focused = false;
                    return Some(SidebarAction::Search);
                }
                KeyCode::Char('r') => return Some(SidebarAction::Refresh),
                // Keep process interrupt and exit routed to the conversation.
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.focused = false;
                    return None;
                }
                _ => {}
            }
            return Some(SidebarAction::Changed);
        }
        if let Event::Mouse(mouse) = event {
            let position = Position::new(mouse.column, mouse.row);
            if !self.area.contains(position) {
                if matches!(mouse.kind, MouseEventKind::Down(_)) {
                    self.focused = false;
                }
                return None;
            }
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if shortcuts && self.rows.contains(position) =>
                {
                    let index = self.state.offset() + usize::from((mouse.row - self.rows.y) / 2);
                    if let Some(session) = self.sessions.get(index) {
                        self.state.select(Some(index));
                        self.focused = false;
                        return Some(SidebarAction::Select(session.session_id.clone()));
                    }
                }
                MouseEventKind::ScrollDown => self.move_selection(1),
                MouseEventKind::ScrollUp => self.move_selection(-1),
                _ => {}
            }
            return Some(SidebarAction::Changed);
        }
        None
    }

    pub(super) fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        status: &str,
        draft: &str,
    ) -> Rect {
        self.area = Rect::default();
        self.rows = Rect::default();
        if !self.visible || area.width < 88 || area.height < 8 {
            // Alt+T opens navigation as an overlay on narrow terminals.
            if !self.focused {
                return area;
            }
        }
        let width = (area.width / 3).clamp(28, 42).min(area.width);
        self.area = Rect { width, ..area };
        let block = Block::default()
            .borders(Borders::RIGHT)
            .title(if self.focused {
                " Threads · Esc back "
            } else {
                " Threads · Alt+T "
            })
            .border_style(Style::default().fg(if self.focused {
                theme.accent()
            } else {
                theme.border()
            }));
        let inner = block.inner(self.area);
        frame.render_widget(block, self.area);
        // Current session is always visible, including before its first server summary.
        let current_title = self
            .sessions
            .iter()
            .find(|s| s.session_id == self.current)
            .map(|s| s.preview.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if draft.is_empty() {
                    "New thread"
                } else {
                    draft
                }
            });
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    format!(" › {}", one_line(current_title)),
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Line::styled(format!("   {status}"), Style::default().fg(theme.muted())),
            ]),
            Rect {
                height: 2.min(inner.height),
                ..inner
            },
        );
        self.rows = Rect {
            y: inner.y.saturating_add(3),
            height: inner.height.saturating_sub(5) / 2 * 2,
            ..inner
        };
        let items: Vec<_> = self
            .sessions
            .iter()
            .map(|session| {
                let active = session.session_id == self.current;
                let title = if session.preview.trim().is_empty() {
                    "Untitled thread"
                } else {
                    &session.preview
                };
                ListItem::new(vec![
                    Line::from(vec![
                        Span::raw(if active { " › " } else { "   " }),
                        Span::raw(one_line(title)),
                    ]),
                    Line::styled(
                        format!(
                            "   {}",
                            if active {
                                status
                            } else {
                                session.status.as_str()
                            }
                        ),
                        Style::default().fg(theme.muted()),
                    ),
                ])
                .style(Style::default().fg(if active {
                    theme.accent()
                } else {
                    theme.text()
                }))
            })
            .collect();
        frame.render_stateful_widget(
            List::new(items).highlight_style(
                Style::default()
                    .bg(theme.code_background())
                    .add_modifier(Modifier::BOLD),
            ),
            self.rows,
            &mut self.state,
        );
        let footer = if self.error.is_some() {
            "Unavailable · r retry"
        } else if !self.loaded {
            "Loading threads…"
        } else if self.sessions.is_empty() {
            "No saved threads"
        } else {
            "↑↓ Enter · / search · Alt+S hide"
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(theme.muted())),
            Rect {
                y: inner.bottom().saturating_sub(1),
                height: 1.min(inner.height),
                ..inner
            },
        );
        if area.width < 88 {
            Rect::default()
        } else {
            Rect {
                x: area.x + width,
                width: area.width - width,
                ..area
            }
        }
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect()
}
