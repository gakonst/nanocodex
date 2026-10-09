//! Branch navigator overlay (Ctrl+Alt+B): lists the conversation's branches
//! and the current branch's prompts. Enter switches to a branch; e or Enter on
//! a prompt edits it inline, and Enter again starts a new branch from there.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::floating::Floating;
use crate::nanocodex2::tui::{
    features::{
        FeatureHost, FeatureOverlay, FeatureUpdate, OverlayOutcome,
        branches::{self, SharedRegistry},
    },
    local::agent::LocalLaunch,
    theme::Theme,
};

enum Focus {
    Branches,
    Prompts,
    Editing { text: String, cursor: usize },
}

pub(crate) struct BranchNavigator {
    registry: SharedRegistry,
    host: FeatureHost,
    launch: LocalLaunch,
    workspace: PathBuf,
    rollout: Option<(String, PathBuf)>,
    prompts: Vec<String>,
    branch: usize,
    prompt: usize,
    focus: Focus,
    error: Option<String>,
}

impl BranchNavigator {
    pub(crate) fn new(
        registry: SharedRegistry,
        host: FeatureHost,
        launch: LocalLaunch,
        workspace: PathBuf,
        rollout: Option<(String, PathBuf)>,
        prompts: Vec<String>,
    ) -> Self {
        let (branch, focus) = {
            let state = registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let branch = state
                .current
                .as_ref()
                .and_then(|current| {
                    state
                        .branches
                        .iter()
                        .position(|branch| &branch.thread == current)
                })
                .unwrap_or(0);
            // A single-branch conversation starts on its prompts.
            let focus = if state.branches.len() > 1 {
                Focus::Branches
            } else {
                Focus::Prompts
            };
            (branch, focus)
        };
        let prompt = prompts.len().saturating_sub(1);
        Self {
            registry,
            host,
            launch,
            workspace,
            rollout,
            prompts,
            branch,
            prompt,
            focus,
            error: None,
        }
    }

    fn branch_rows(&self) -> Vec<(String, String, bool)> {
        let state = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .branches
            .iter()
            .map(|branch| {
                let current = state.current.as_deref() == Some(branch.thread.as_str());
                let parent = branch
                    .parent
                    .as_deref()
                    .map_or_else(String::new, |parent| format!(" from {}", short(parent)));
                (
                    branch.label.clone(),
                    format!("{}{parent}", short(&branch.thread)),
                    current,
                )
            })
            .collect()
    }

    fn switch_selected(&mut self) -> OverlayOutcome {
        let target = {
            let state = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(branch) = state.branches.get(self.branch) else {
                return OverlayOutcome::Consumed;
            };
            if state.current.as_deref() == Some(branch.thread.as_str()) {
                return OverlayOutcome::Close;
            }
            branch.thread.clone()
        };
        branches::switch(&self.launch, &target);
        OverlayOutcome::Close
    }

    fn commit_edit(&mut self, text: String) -> OverlayOutcome {
        if text.trim().is_empty() {
            self.error = Some("The edited prompt is empty".to_owned());
            return OverlayOutcome::Consumed;
        }
        match branches::edit(
            &self.registry,
            &self.launch,
            &self.workspace,
            self.rollout
                .as_ref()
                .map(|(session, path)| (session.as_str(), path.as_path())),
            self.prompt,
            text,
        ) {
            Ok(launch) => {
                self.host.send(FeatureUpdate::Relaunch(Box::new(launch)));
                OverlayOutcome::Close
            }
            Err(error) => {
                self.error = Some(error);
                OverlayOutcome::Consumed
            }
        }
    }
}

fn short(thread: &str) -> String {
    thread.chars().take(8).collect()
}

fn single_line(text: &str, width: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > width {
        let mut cut = line
            .chars()
            .take(width.saturating_sub(3))
            .collect::<String>();
        cut.push_str("...");
        cut
    } else {
        line
    }
}

impl FeatureOverlay for BranchNavigator {
    fn name(&self) -> &'static str {
        "branch_navigator"
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let width = area.width.saturating_sub(4).clamp(30, 110);
        let height = area.height.saturating_sub(2).clamp(10, 36);
        let bindings: &[(&str, &str)] = match self.focus {
            Focus::Editing { .. } => &[("enter", "start branch"), ("esc", "cancel edit")],
            _ => &[
                ("up/down", "select"),
                ("tab", "branches/prompts"),
                ("enter", "switch / edit"),
                ("e", "edit prompt"),
                ("esc", "close"),
            ],
        };
        let layout = Floating::new("Branches", width, height, bindings)
            .colors(theme.accent(), theme.accent())
            .render(frame, area, theme);
        let rows = self.branch_rows();
        let branch_height = u16::try_from(rows.len().saturating_add(1))
            .unwrap_or(u16::MAX)
            .min(layout.body.height / 2);
        let [branch_area, prompt_area, error_area] = Layout::vertical([
            Constraint::Length(branch_height),
            Constraint::Min(1),
            Constraint::Length(u16::from(self.error.is_some())),
        ])
        .areas(layout.body);
        let focused = Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(theme.muted());
        let mut lines = vec![Line::styled(
            "Branches",
            if matches!(self.focus, Focus::Branches) {
                focused
            } else {
                muted
            },
        )];
        for (index, (label, detail, current)) in rows.iter().enumerate() {
            let selected = matches!(self.focus, Focus::Branches) && index == self.branch;
            let mark = if selected { "\u{203a} " } else { "  " };
            let now = if *current { " (current)" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{mark}{label}{now}"),
                    if selected { focused } else { Style::default() },
                ),
                Span::styled(format!("  {detail}"), muted),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), branch_area);
        let text_width = usize::from(prompt_area.width.saturating_sub(6));
        let mut lines = vec![Line::styled(
            "Prompts on this branch",
            if matches!(self.focus, Focus::Branches) {
                muted
            } else {
                focused
            },
        )];
        if self.prompts.is_empty() {
            lines.push(Line::styled(
                if self.rollout.is_some() {
                    "  No completed prompts yet."
                } else {
                    "  This session has no rollout to branch from."
                },
                muted,
            ));
        }
        let visible = usize::from(prompt_area.height.saturating_sub(1)).max(1);
        let start = self.prompt.saturating_sub(visible.saturating_sub(1));
        for (index, prompt) in self.prompts.iter().enumerate().skip(start).take(visible) {
            let selected = !matches!(self.focus, Focus::Branches) && index == self.prompt;
            if let (true, Focus::Editing { text, cursor }) = (selected, &self.focus) {
                let (before, after) = text.split_at((*cursor).min(text.len()));
                lines.push(Line::from(vec![
                    Span::styled(format!("\u{203a} {}. ", index + 1), focused),
                    Span::raw(before.replace('\n', " ")),
                    Span::styled("|", Style::default().fg(Color::Yellow)),
                    Span::raw(after.replace('\n', " ")),
                ]));
            } else {
                let mark = if selected { "\u{203a} " } else { "  " };
                lines.push(Line::styled(
                    format!("{mark}{}. {}", index + 1, single_line(prompt, text_width)),
                    if selected { focused } else { Style::default() },
                ));
            }
        }
        frame.render_widget(Paragraph::new(lines), prompt_area);
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::styled(error.clone(), Style::default().fg(Color::Red))),
                error_area,
            );
        }
    }

    fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return OverlayOutcome::Close;
        }
        if let Focus::Editing { text, cursor } = &mut self.focus {
            match key.code {
                KeyCode::Esc => self.focus = Focus::Prompts,
                KeyCode::Enter
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    text.insert(*cursor, '\n');
                    *cursor += 1;
                }
                KeyCode::Enter => {
                    let text = std::mem::take(text);
                    self.focus = Focus::Prompts;
                    return self.commit_edit(text);
                }
                KeyCode::Backspace => {
                    if let Some((index, _)) = text[..*cursor].char_indices().next_back() {
                        text.replace_range(index..*cursor, "");
                        *cursor = index;
                    }
                }
                KeyCode::Left => {
                    if let Some((index, _)) = text[..*cursor].char_indices().next_back() {
                        *cursor = index;
                    }
                }
                KeyCode::Right => {
                    if let Some(character) = text[*cursor..].chars().next() {
                        *cursor += character.len_utf8();
                    }
                }
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = text.len(),
                KeyCode::Char(character) => {
                    text.insert(*cursor, character);
                    *cursor += character.len_utf8();
                }
                _ => {}
            }
            return OverlayOutcome::Consumed;
        }
        self.error = None;
        let branches = self.branch_rows().len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return OverlayOutcome::Close,
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Branches => Focus::Prompts,
                    _ => Focus::Branches,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Branches => self.branch = self.branch.saturating_sub(1),
                _ => self.prompt = self.prompt.saturating_sub(1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Branches => self.branch = (self.branch + 1).min(branches.saturating_sub(1)),
                _ => self.prompt = (self.prompt + 1).min(self.prompts.len().saturating_sub(1)),
            },
            KeyCode::Enter if matches!(self.focus, Focus::Branches) => {
                return self.switch_selected();
            }
            KeyCode::Enter | KeyCode::Char('e') if matches!(self.focus, Focus::Prompts) => {
                if let Some(prompt) = self.prompts.get(self.prompt) {
                    self.focus = Focus::Editing {
                        cursor: prompt.len(),
                        text: prompt.clone(),
                    };
                }
            }
            _ => {}
        }
        OverlayOutcome::Consumed
    }

    fn paste(&mut self, pasted: &str) {
        if let Focus::Editing { text, cursor } = &mut self.focus {
            text.insert_str(*cursor, pasted);
            *cursor += pasted.len();
        }
    }
}
