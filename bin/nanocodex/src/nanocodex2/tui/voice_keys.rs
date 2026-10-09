//! Configurable voice mute shortcut (`--voice-mute-key`). Realtime voice uses
//! it; parsing is shared by flag validation and key matching.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A parsed `ctrl+<char>` / `alt+<char>` shortcut.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MuteKey {
    pub(crate) control: bool,
    pub(crate) character: char,
}

impl MuteKey {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let value = value.to_ascii_lowercase();
        let mut parts = value.split('+');
        let control = match parts.next()? {
            "ctrl" => true,
            "alt" => false,
            _ => return None,
        };
        let key = parts.next()?;
        let mut chars = key.chars();
        let character = chars.next()?;
        (chars.next().is_none() && parts.next().is_none()).then_some(Self { control, character })
    }

    pub(crate) fn matches(self, event: &KeyEvent) -> bool {
        let modifiers = if self.control {
            KeyModifiers::CONTROL
        } else {
            KeyModifiers::ALT
        };
        event.code == KeyCode::Char(self.character) && event.modifiers == modifiers
    }
}

/// Clap value parser for `--voice-mute-key`.
pub(crate) fn validate_key(value: &str) -> Result<String, String> {
    if value == "none" || MuteKey::parse(value).is_some() {
        Ok(value.to_owned())
    } else {
        Err("use ctrl+<character>, alt+<character>, or none".into())
    }
}
