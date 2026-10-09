//! Terminal completion notifications while the terminal is unfocused.
//!
//! OSC 9 desktop notifications for terminals known to show them, BEL
//! elsewhere; both are wrapped for tmux passthrough. Returning focus clears a
//! notification that has not been written yet.

use std::env;

use crossterm::event::Event;

use super::terminal::TerminalSession;

pub(crate) struct Notifier {
    backend: Backend,
    tmux: bool,
    enabled: bool,
    focused: bool,
    busy: [bool; 2],
    failed: bool,
}

#[derive(Clone, Copy)]
enum Backend {
    Osc9,
    Bell,
}

impl Notifier {
    pub(crate) fn from_env() -> Self {
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
            focused: true,
            busy: [false; 2],
            failed: false,
        }
    }

    /// Tracks terminal focus from every input event.
    pub(crate) fn observe_event(&mut self, event: &Event) {
        match event {
            Event::FocusGained => self.focused = true,
            Event::FocusLost => self.focused = false,
            _ => {}
        }
    }

    /// Records that the most recent turn ended in an error.
    pub(crate) const fn turn_failed(&mut self) {
        self.failed = true;
    }

    /// Called after each presented frame with main and side-pane activity.
    /// A pane that stops working while the terminal is unfocused notifies once.
    pub(crate) fn after_frame(
        &mut self,
        terminal: &mut TerminalSession,
        main_busy: bool,
        btw_busy: bool,
    ) {
        for (index, (busy, scope)) in [(main_busy, "Nanocodex"), (btw_busy, "Nanocodex BTW")]
            .into_iter()
            .enumerate()
        {
            let finished = self.busy[index] && !busy;
            self.busy[index] = busy;
            if !finished {
                continue;
            }
            let failed = std::mem::take(&mut self.failed);
            if self.focused {
                continue;
            }
            let message = if failed {
                format!("{scope} needs attention")
            } else {
                format!("{scope} finished")
            };
            self.notify(terminal, &message);
        }
    }

    fn notify(&mut self, terminal: &mut TerminalSession, message: &str) {
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
