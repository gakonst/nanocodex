//! Local voice actions and provider selection. Opening a menu never starts audio.
use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::{
    tui::theme::Theme,
    voice::{Command, Provider, Selection},
};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{List, ListItem, ListState},
};

pub(super) struct VoiceMenu {
    title: &'static str,
    entries: Vec<(String, Command)>,
    selected: usize,
}

impl VoiceMenu {
    pub(super) fn new(active: bool) -> Self {
        Self {
            title: "Voice",
            selected: 0,
            entries: vec![
                (
                    if active { "Stop voice" } else { "Start voice" }.into(),
                    if active {
                        Command::Stop
                    } else {
                        Command::Start(None)
                    },
                ),
                (
                    "ChatGPT voices…".into(),
                    Command::ListProvider(Provider::Chatgpt),
                ),
                (
                    "ElevenLabs voices…".into(),
                    Command::ListProvider(Provider::ElevenLabs),
                ),
                (
                    "Record a voice clone…".into(),
                    Command::CloneOpen("My voice".into()),
                ),
                ("Voice status".into(), Command::Status),
                ("Help and file cloning".into(), Command::Help),
            ],
        }
    }

    pub(super) fn chatgpt() -> Self {
        Self {
            title: "ChatGPT voices · enter to start / switch",
            selected: 0,
            entries: nanocodex_voice_protocol::CHATGPT_REALTIME_VOICES
                .iter()
                .map(|name| {
                    (
                        (*name).to_owned(),
                        Command::Select(Selection::Chatgpt(name)),
                    )
                })
                .collect(),
        }
    }

    // Only recognize the local catalog's exact header and validate IDs through the
    // command parser. Provider labels remain display data, never commands.
    pub(super) fn elevenlabs_catalog(text: &str) -> Option<Self> {
        let catalog = text.strip_prefix("ElevenLabs voices (use /voice elevenlabs VOICE_ID):\n")?;
        let mut entries = Vec::new();
        for line in catalog.lines() {
            let Some((id, label)) = line.split_once(" — ") else {
                continue;
            };
            let Ok(command @ Command::Select(Selection::ElevenLabs(_))) =
                Command::parse(&format!("elevenlabs {id}"))
            else {
                continue;
            };
            entries.push((format!("{label} · {id}"), command));
        }
        entries.push((
            "Record a voice clone…".into(),
            Command::CloneOpen("My voice".into()),
        ));
        Some(Self {
            title: "ElevenLabs voices · enter to start / switch",
            entries,
            selected: 0,
        })
    }
}

impl Component for VoiceMenu {
    type Event = Event;
    // None dismisses the menu; no action is executed.
    type Effect = Option<Command>;

    fn update(&mut self, event: Event) -> ComponentUpdate<Self::Effect> {
        let Event::Key(key) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1))
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.entries.len().saturating_sub(1),
            KeyCode::Enter => {
                return ComponentUpdate {
                    effects: vec![Some(self.entries[self.selected].1.clone())],
                    render: RenderRequest::Immediate,
                };
            }
            KeyCode::Esc | KeyCode::Backspace => {
                return ComponentUpdate {
                    effects: vec![None],
                    render: RenderRequest::Immediate,
                };
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        let layout = Floating::new(
            self.title,
            72,
            (self.entries.len() + 4).min(20) as u16,
            &[("↑↓", "select"), ("enter", "open"), ("esc", "close")],
        )
        .render(frame, area, theme);
        let list = List::new(
            self.entries
                .iter()
                .map(|(label, _)| ListItem::new(label.clone())),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    #[test]
    fn menu_exposes_clone_and_explicit_start_stop() {
        for (active, expected) in [(false, Command::Start(None)), (true, Command::Stop)] {
            let mut menu = VoiceMenu::new(active);
            assert_eq!(
                menu.update(key(KeyCode::Enter)).effects,
                vec![Some(expected)]
            );
            for _ in 0..3 {
                menu.update(key(KeyCode::Down));
            }
            assert_eq!(
                menu.update(key(KeyCode::Enter)).effects,
                vec![Some(Command::CloneOpen("My voice".into()))]
            );
            assert_eq!(menu.update(key(KeyCode::Esc)).effects, vec![None]);
        }
    }
    #[test]
    fn catalogs_select_provider_ids_and_empty_catalog_offers_clone() {
        assert!(VoiceMenu::elevenlabs_catalog("ChatGPT voices: cove\nElevenLabs voices (use /voice elevenlabs VOICE_ID):\nvoice_123 — Speaker").is_none());
        let mut menu = VoiceMenu::chatgpt();
        assert!(matches!(
            menu.update(key(KeyCode::Enter)).effects.as_slice(),
            [Some(Command::Select(Selection::Chatgpt(_)))]
        ));
        let mut menu = VoiceMenu::elevenlabs_catalog("ElevenLabs voices (use /voice elevenlabs VOICE_ID):\nvoice_123 — Synthetic speaker\n../bad — Ignore").unwrap();
        assert_eq!(menu.entries.len(), 2);
        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            vec![Some(Command::Select(Selection::ElevenLabs(
                "voice_123".into()
            )))]
        );
        let mut empty = VoiceMenu::elevenlabs_catalog(
            "ElevenLabs voices (use /voice elevenlabs VOICE_ID):\nNo voices found.",
        )
        .unwrap();
        assert_eq!(
            empty.update(key(KeyCode::Enter)).effects,
            vec![Some(Command::CloneOpen("My voice".into()))]
        );
    }
}
