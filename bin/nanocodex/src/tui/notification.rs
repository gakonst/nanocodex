use std::env;

use super::terminal::TerminalSession;

pub(super) struct Notifier {
    backend: Backend,
    tmux: bool,
    enabled: bool,
}

#[derive(Clone, Copy)]
enum Backend {
    Osc9,
    Bell,
}

impl Notifier {
    pub(super) fn from_env() -> Self {
        let term_program = env::var("TERM_PROGRAM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let backend = if ["ghostty", "iterm", "kitty", "warp", "wezterm"]
            .iter()
            .any(|name| term_program.contains(name))
        {
            Backend::Osc9
        } else {
            Backend::Bell
        };
        Self {
            backend,
            tmux: env::var_os("TMUX").is_some(),
            enabled: true,
        }
    }

    pub(super) fn notify(&mut self, terminal: &mut TerminalSession, message: &str) {
        if !self.enabled {
            return;
        }
        let bytes = notification_bytes(self.backend, self.tmux, message);
        if let Err(error) = terminal.write_control_sequence(&bytes) {
            self.enabled = false;
            tracing::warn!(%error, "terminal completion notifications disabled after write failure");
        }
    }
}

fn notification_bytes(backend: Backend, tmux: bool, message: &str) -> Vec<u8> {
    if matches!(backend, Backend::Bell) {
        return vec![b'\x07'];
    }
    let message = message
        .chars()
        .filter(|character| !character.is_control())
        .take(180)
        .collect::<String>();
    let sequence = format!("\x1b]9;{message}\x07");
    if !tmux {
        return sequence.into_bytes();
    }
    let escaped = sequence.replace('\x1b', "\x1b\x1b");
    format!("\x1bPtmux;{escaped}\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{Backend, notification_bytes};

    #[test]
    fn osc9_notification_sanitizes_control_characters() {
        assert_eq!(
            notification_bytes(Backend::Osc9, false, "Done\n\x1bboom"),
            b"\x1b]9;Doneboom\x07"
        );
    }

    #[test]
    fn tmux_notification_wraps_and_escapes_osc9() {
        assert_eq!(
            notification_bytes(Backend::Osc9, true, "Done"),
            b"\x1bPtmux;\x1b\x1b]9;Done\x07\x1b\\"
        );
    }

    #[test]
    fn unknown_terminals_fall_back_to_bell() {
        assert_eq!(notification_bytes(Backend::Bell, false, "Done"), b"\x07");
    }
}
