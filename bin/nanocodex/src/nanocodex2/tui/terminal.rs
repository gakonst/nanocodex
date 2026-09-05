// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Terminal lifecycle and synchronized Ratatui frames.

use crossterm::{
    clipboard::CopyToClipboard,
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute, queue,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};
use std::{
    io::{self, IsTerminal, Stdout, Write, stdin, stdout},
    panic,
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
};

type TuiTerminal = Terminal<StableCursorBackend<CrosstermBackend<Stdout>>>;

/// Avoids restarting the terminal's cursor blink cycle on every rendered frame.
struct StableCursorBackend<B> {
    inner: B,
    cursor_visibility: CursorVisibility,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CursorVisibility {
    Hidden,
    Visible,
    Unknown,
}

impl<B> StableCursorBackend<B> {
    const fn hidden(inner: B) -> Self {
        Self {
            inner,
            cursor_visibility: CursorVisibility::Hidden,
        }
    }

    const fn assume_cursor_hidden(&mut self) {
        self.cursor_visibility = CursorVisibility::Hidden;
    }

    const fn invalidate_cursor_visibility(&mut self) {
        self.cursor_visibility = CursorVisibility::Unknown;
    }
}

impl<B: Write> Write for StableCursorBackend<B> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<B: Backend> Backend for StableCursorBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, count: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(count)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        if self.cursor_visibility == CursorVisibility::Hidden {
            return Ok(());
        }
        self.inner.hide_cursor()?;
        self.cursor_visibility = CursorVisibility::Hidden;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        if self.cursor_visibility == CursorVisibility::Visible {
            return Ok(());
        }
        self.inner.show_cursor()?;
        self.cursor_visibility = CursorVisibility::Visible;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
pub(crate) struct MeasuredBackend<B> {
    inner: B,
    changed_cells: u64,
    cursor_reads: u64,
    cursor_hides: u64,
    cursor_shows: u64,
}

pub(crate) struct TerminalSession {
    terminal: TuiTerminal,
    active: bool,
}

struct RestoreOnDrop {
    armed: bool,
}

static INSTALL_PANIC_HOOK: Once = Once::new();
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
impl<B: Backend> Backend for MeasuredBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut changed_cells = 0_u64;
        let content = content.inspect(|_| changed_cells = changed_cells.saturating_add(1));
        let result = self.inner.draw(content);
        self.changed_cells = self.changed_cells.saturating_add(changed_cells);
        result
    }

    fn append_lines(&mut self, count: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(count)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_hides = self.cursor_hides.saturating_add(1);
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_shows = self.cursor_shows.saturating_add(1);
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.cursor_reads = self.cursor_reads.saturating_add(1);
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

impl TerminalSession {
    pub(crate) fn enter() -> io::Result<Self> {
        if !stdin().is_terminal() || !stdout().is_terminal() {
            return Err(io::Error::other(
                "interactive mode requires terminal stdin and stdout; use `nanocodex2 run <PROMPT>` for JSONL output",
            ));
        }

        install_panic_hook();
        let mut restore = RestoreOnDrop { armed: true };
        enable_raw_mode()?;
        let mut output = stdout();
        activate_commands(&mut output)?;
        TERMINAL_ACTIVE.store(true, Ordering::Release);
        let terminal = Terminal::new(StableCursorBackend::hidden(CrosstermBackend::new(output)))?;
        crate::tui::components::initialize_image_renderer();
        restore.armed = false;

        Ok(Self {
            terminal,
            active: true,
        })
    }

    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        begin_synchronized_update(self.terminal.backend_mut())?;
        let draw = self.terminal.draw(render).map(|_| ());
        let end = end_synchronized_update(self.terminal.backend_mut());
        draw.and(end)
    }

    pub(crate) fn invalidate_cursor_visibility(&mut self) {
        self.terminal.backend_mut().invalidate_cursor_visibility();
    }

    pub(crate) fn copy_to_clipboard(&mut self, text: &str) -> io::Result<()> {
        copy_to_clipboard(self.terminal.backend_mut(), text)
    }

    pub(crate) fn suspend(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }

        self.terminal.show_cursor()?;
        restore_terminal(self.terminal.backend_mut());
        self.active = false;
        Ok(())
    }

    pub(crate) fn resume(&mut self) -> io::Result<()> {
        if self.active {
            return Ok(());
        }

        let mut restore = RestoreOnDrop { armed: true };
        enable_raw_mode()?;
        activate_commands(self.terminal.backend_mut())?;
        self.terminal.backend_mut().assume_cursor_hidden();
        TERMINAL_ACTIVE.store(true, Ordering::Release);
        reset_after_resume(&mut self.terminal)?;
        restore.armed = false;
        self.active = true;
        Ok(())
    }
}

fn reset_after_resume<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
    let area = terminal.size()?.into();
    terminal.resize(area)
}

fn copy_to_clipboard(output: &mut impl Write, text: &str) -> io::Result<()> {
    execute!(output, CopyToClipboard::to_clipboard_from(text))
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        drop(self.terminal.show_cursor());
        restore_terminal(self.terminal.backend_mut());
    }
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        if self.armed {
            restore_terminal(&mut stdout());
        }
    }
}

fn activate_commands(output: &mut impl Write) -> io::Result<()> {
    execute!(
        output,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange,
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
    Ok(())
}

fn restore_terminal(output: &mut impl Write) {
    TERMINAL_ACTIVE.store(false, Ordering::Release);
    drop(disable_raw_mode());
    restore_commands(output);
}

fn restore_commands(output: &mut impl Write) {
    drop(execute!(
        output,
        EndSynchronizedUpdate,
        Show,
        PopKeyboardEnhancementFlags,
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    ));
}

fn begin_synchronized_update(output: &mut impl Write) -> io::Result<()> {
    queue!(output, BeginSynchronizedUpdate)
}

fn end_synchronized_update(output: &mut impl Write) -> io::Result<()> {
    execute!(output, EndSynchronizedUpdate)
}

fn install_panic_hook() {
    INSTALL_PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if TERMINAL_ACTIVE.swap(false, Ordering::AcqRel) {
                restore_terminal(&mut stdout());
            }
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::{
        MeasuredBackend, StableCursorBackend, activate_commands, begin_synchronized_update,
        copy_to_clipboard, end_synchronized_update, reset_after_resume, restore_commands,
    };
    use crate::{
        config::ReasoningEffort,
        tui::{
            components::{AppNode, RootNode},
            theme::Theme,
        },
    };
    use ratatui::{Terminal, backend::TestBackend, layout::Position};
    use std::path::Path;

    #[test]
    fn synchronized_updates_use_csi_2026() {
        let mut output = Vec::new();

        begin_synchronized_update(&mut output).unwrap();
        end_synchronized_update(&mut output).unwrap();

        assert_eq!(output, b"\x1b[?2026h\x1b[?2026l");
    }

    #[test]
    fn activation_requests_terminal_focus_changes() {
        let mut output = Vec::new();

        activate_commands(&mut output).unwrap();

        assert!(output.windows(8).any(|window| window == b"\x1b[?1004h"));
    }

    #[test]
    fn restoration_ends_sync_and_restores_input_modes() {
        let mut output = Vec::new();

        restore_commands(&mut output);

        assert!(output.starts_with(b"\x1b[?2026l\x1b[?25h"));
        assert!(output.windows(5).any(|window| window == b"\x1b[<1u"));
        assert!(output.windows(8).any(|window| window == b"\x1b[?1004l"));
        assert!(output.windows(8).any(|window| window == b"\x1b[?1000l"));
        assert!(output.windows(8).any(|window| window == b"\x1b[?2004l"));
        assert!(output.ends_with(b"\x1b[?1049l"));
    }

    #[test]
    fn clipboard_copy_uses_osc_52() {
        let mut output = Vec::new();

        copy_to_clipboard(&mut output, "copy me").unwrap();

        assert_eq!(output, b"\x1b]52;c;Y29weSBtZQ==\x1b\\");
    }

    #[test]
    fn unchanged_frames_produce_zero_ratatui_diff_cells() {
        let backend = MeasuredBackend {
            inner: TestBackend::new(40, 5),
            changed_cells: 0,
            cursor_reads: 0,
            cursor_hides: 0,
            cursor_shows: 0,
        };
        let mut terminal = Terminal::new(backend).unwrap();
        let root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let mut app = AppNode::new(Theme::default(), Path::new("/work").to_path_buf(), root);

        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!(terminal.backend().changed_cells > 0);

        terminal.backend_mut().changed_cells = 0;
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert_eq!(terminal.backend().changed_cells, 0);
    }

    #[test]
    fn resuming_does_not_query_the_terminal_cursor() {
        let backend = MeasuredBackend {
            inner: TestBackend::new(40, 5),
            changed_cells: 0,
            cursor_reads: 0,
            cursor_hides: 0,
            cursor_shows: 0,
        };
        let mut terminal = Terminal::new(backend).unwrap();

        reset_after_resume(&mut terminal).unwrap();

        assert_eq!(terminal.backend().cursor_reads, 0);
    }

    #[test]
    fn unchanged_cursor_visibility_does_not_reach_the_terminal() {
        let measured = MeasuredBackend {
            inner: TestBackend::new(10, 2),
            changed_cells: 0,
            cursor_reads: 0,
            cursor_hides: 0,
            cursor_shows: 0,
        };
        let backend = StableCursorBackend::hidden(measured);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| frame.set_cursor_position(Position::new(0, 0)))
            .unwrap();
        terminal
            .draw(|frame| frame.set_cursor_position(Position::new(1, 0)))
            .unwrap();
        terminal.draw(|_| {}).unwrap();
        terminal.draw(|_| {}).unwrap();

        assert_eq!(terminal.backend().inner.cursor_shows, 1);
        assert_eq!(terminal.backend().inner.cursor_hides, 1);
    }

    #[test]
    fn invalidated_cursor_visibility_is_reasserted_once() {
        let measured = MeasuredBackend {
            inner: TestBackend::new(10, 2),
            changed_cells: 0,
            cursor_reads: 0,
            cursor_hides: 0,
            cursor_shows: 0,
        };
        let backend = StableCursorBackend::hidden(measured);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| frame.set_cursor_position(Position::new(0, 0)))
            .unwrap();
        terminal.backend_mut().invalidate_cursor_visibility();
        terminal
            .draw(|frame| frame.set_cursor_position(Position::new(1, 0)))
            .unwrap();
        terminal
            .draw(|frame| frame.set_cursor_position(Position::new(2, 0)))
            .unwrap();

        terminal.backend_mut().invalidate_cursor_visibility();
        terminal.draw(|_| {}).unwrap();
        terminal.draw(|_| {}).unwrap();

        assert_eq!(terminal.backend().inner.cursor_shows, 2);
        assert_eq!(terminal.backend().inner.cursor_hides, 1);
    }
}
