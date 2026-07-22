use std::{
    cell::Cell as CounterCell,
    io::{self, IsTerminal, Stdout, Write, stdin, stdout},
    panic,
    rc::Rc,
    sync::{
        Once,
        atomic::{AtomicBool, Ordering},
    },
};

use crossterm::{
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

type TuiTerminal = Terminal<MeasuredBackend<CrosstermBackend<ByteCountingWriter<Stdout>>>>;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DrawMetrics {
    pub changed_cells: u64,
    pub output_bytes: u64,
}

pub(super) struct ByteCountingWriter<W> {
    pub(super) inner: W,
    pub(super) bytes: Rc<CounterCell<u64>>,
}

pub(super) struct MeasuredBackend<B> {
    pub(super) inner: B,
    pub(super) changed_cells: u64,
}

pub(super) struct TerminalSession {
    terminal: TuiTerminal,
    output_bytes: Rc<CounterCell<u64>>,
    active: bool,
}

static INSTALL_PANIC_HOOK: Once = Once::new();
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

struct RestoreOnDrop {
    armed: bool,
}

impl<W: Write> Write for ByteCountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes
            .set(self.bytes.get().saturating_add(written as u64));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<B: Write> Write for MeasuredBackend<B> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<B: Backend> Backend for MeasuredBackend<B> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut changed_cells = 0_u64;
        let content = content.inspect(|_| {
            changed_cells = changed_cells.saturating_add(1);
        });
        let result = self.inner.draw(content);
        self.changed_cells = self.changed_cells.saturating_add(changed_cells);
        result
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl TerminalSession {
    pub(super) fn enter() -> io::Result<Self> {
        if !stdin().is_terminal() || !stdout().is_terminal() {
            return Err(io::Error::other(
                "interactive mode requires terminal stdin and stdout; use `nanocodex run` for JSONL",
            ));
        }
        install_panic_hook();
        let mut restore = RestoreOnDrop { armed: true };
        enable_raw_mode()?;
        let mut output = stdout();
        activate_commands(&mut output)?;
        TERMINAL_ACTIVE.store(true, Ordering::Release);
        let output_bytes = Rc::new(CounterCell::new(0));
        let writer = ByteCountingWriter {
            inner: output,
            bytes: Rc::clone(&output_bytes),
        };
        let terminal = Terminal::new(MeasuredBackend {
            inner: CrosstermBackend::new(writer),
            changed_cells: 0,
        })?;
        restore.armed = false;
        Ok(Self {
            terminal,
            output_bytes,
            active: true,
        })
    }

    pub(super) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<DrawMetrics> {
        let bytes_before = self.output_bytes.get();
        self.terminal.backend_mut().changed_cells = 0;
        begin_synchronized_update(self.terminal.backend_mut())?;
        let draw_result = self.terminal.draw(render).map(|_| ());
        let end_result = end_synchronized_update(self.terminal.backend_mut());
        draw_result.and(end_result)?;
        Ok(DrawMetrics {
            changed_cells: self.terminal.backend().changed_cells,
            output_bytes: self.output_bytes.get().saturating_sub(bytes_before),
        })
    }

    pub(super) fn write_control_sequence(&mut self, sequence: &[u8]) -> io::Result<()> {
        self.terminal.backend_mut().write_all(sequence)?;
        Write::flush(self.terminal.backend_mut())
    }

    pub(super) fn suspend(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.terminal.show_cursor()?;
        restore(self.terminal.backend_mut());
        self.active = false;
        Ok(())
    }

    pub(super) fn resume(&mut self) -> io::Result<()> {
        if self.active {
            return Ok(());
        }
        let mut restore = RestoreOnDrop { armed: true };
        enable_raw_mode()?;
        activate_commands(self.terminal.backend_mut())?;
        TERMINAL_ACTIVE.store(true, Ordering::Release);
        self.terminal.clear()?;
        self.terminal.autoresize()?;
        restore.armed = false;
        self.active = true;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            drop(self.terminal.show_cursor());
            restore(self.terminal.backend_mut());
        }
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
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    ));
}

fn activate_commands(output: &mut impl io::Write) -> io::Result<()> {
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
                restore(&mut stdout());
            }
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, io::Write, rc::Rc};

    use ratatui::{Terminal, backend::TestBackend, text::Text, widgets::Paragraph};

    use super::{
        ByteCountingWriter, MeasuredBackend, begin_synchronized_update, end_synchronized_update,
        restore_commands,
    };

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

    #[test]
    fn writer_counts_only_bytes_accepted_by_the_terminal() {
        let bytes = Rc::new(Cell::new(0));
        let mut writer = ByteCountingWriter {
            inner: Vec::new(),
            bytes: Rc::clone(&bytes),
        };

        writer.write_all(b"abcdef").unwrap();

        assert_eq!(bytes.get(), 6);
        assert_eq!(writer.inner, b"abcdef");
    }

    #[test]
    fn measured_backend_counts_ratatui_diff_cells() {
        let backend = MeasuredBackend {
            inner: TestBackend::new(20, 2),
            changed_cells: 0,
        };
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| frame.render_widget(Paragraph::new(Text::raw("hello")), frame.area()))
            .unwrap();
        assert!(terminal.backend().changed_cells > 0);

        terminal.backend_mut().changed_cells = 0;
        terminal
            .draw(|frame| frame.render_widget(Paragraph::new(Text::raw("hello")), frame.area()))
            .unwrap();
        assert_eq!(terminal.backend().changed_cells, 0);
    }
}
