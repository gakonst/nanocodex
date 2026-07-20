use std::{
    io::{self, IsTerminal, Stdout, Write, stdin, stdout},
    panic,
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

pub(super) type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

pub(super) struct TerminalSession {
    terminal: TuiTerminal,
}

static INSTALL_PANIC_HOOK: Once = Once::new();
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

struct RestoreOnDrop {
    armed: bool,
}

impl TerminalSession {
    pub(super) fn enter() -> io::Result<Self> {
        if !stdin().is_terminal() || !stdout().is_terminal() {
            return Err(io::Error::other(
                "interactive mode requires terminal stdin and stdout; use `nanocodex run` for JSONL",
            ));
        }
        install_panic_hook();
        TERMINAL_ACTIVE.store(true, Ordering::Release);
        let mut restore = RestoreOnDrop { armed: true };
        enable_raw_mode()?;
        let mut output = stdout();
        execute!(
            output,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        )?;
        drop(execute!(
            output,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        ));
        let terminal = Terminal::new(CrosstermBackend::new(output))?;
        restore.armed = false;
        Ok(Self { terminal })
    }

    pub(super) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        begin_synchronized_update(self.terminal.backend_mut())?;
        let draw_result = self.terminal.draw(render).map(|_| ());
        let end_result = end_synchronized_update(self.terminal.backend_mut());
        draw_result.and(end_result)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        drop(self.terminal.show_cursor());
        restore(self.terminal.backend_mut());
    }
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        if self.armed {
            restore(&mut stdout());
        }
    }
}

fn restore(output: &mut impl io::Write) {
    TERMINAL_ACTIVE.store(false, Ordering::Release);
    drop(disable_raw_mode());
    restore_commands(output);
}

fn restore_commands(output: &mut impl io::Write) {
    drop(execute!(
        output,
        EndSynchronizedUpdate,
        Show,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    ));
}

fn begin_synchronized_update(output: &mut impl Write) -> io::Result<()> {
    execute!(output, BeginSynchronizedUpdate)
}

fn end_synchronized_update(output: &mut impl Write) -> io::Result<()> {
    execute!(output, EndSynchronizedUpdate)
}

fn install_panic_hook() {
    INSTALL_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if TERMINAL_ACTIVE.swap(false, Ordering::AcqRel) {
                restore(&mut stdout());
            }
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::{begin_synchronized_update, end_synchronized_update, restore_commands};

    #[test]
    fn synchronized_update_uses_csi_2026() {
        let mut output = Vec::new();

        begin_synchronized_update(&mut output).unwrap();
        end_synchronized_update(&mut output).unwrap();

        assert_eq!(output, b"\x1b[?2026h\x1b[?2026l");
    }

    #[test]
    fn restoration_ends_sync_before_leaving_the_alternate_screen() {
        let mut output = Vec::new();

        restore_commands(&mut output);

        assert!(output.starts_with(b"\x1b[?2026l\x1b[?25h"));
        assert!(output.ends_with(b"\x1b[?1049l"));
    }
}
