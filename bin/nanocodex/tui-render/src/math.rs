//! LaTeX math for transcript markdown.
//!
//! Formulas render through ratatex as Kitty Unicode-placeholder images. The
//! placeholder cells are baked into ordinary transcript lines, so folding,
//! scrolling and the layout cache keep working unchanged. Every other terminal,
//! and every formula that is still rendering or failed, keeps the LaTeX source
//! as text. Detection uses environment hints and tmux only: querying the
//! terminal on stdin could leave a blocking reader racing the input stream.

use std::{
    borrow::Cow,
    env,
    io::Read as _,
    process::{Command, Stdio},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::Notify;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatex::{FormulaState, FormulaWidget, GraphicsSupport, PixelSize, Ratatex, TerminalProfile};
// ratatex exposes 0.29 buffers, converted below into the shared 0.30 renderer.
use ratatex_ratatui::{buffer::Buffer, layout::Rect, widgets::Widget as _};
use ratatui::{
    style::{Color, Style},
    text::Span,
};

static RENDERER: Mutex<Option<Ratatex>> = Mutex::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);
/// True until the background detector has either installed or declined a renderer.
static INITIALIZING: AtomicBool = AtomicBool::new(false);
/// Bumped by ratatex workers when a formula finishes and when the renderer starts.
static UPDATES: AtomicU64 = AtomicU64::new(0);
/// Set when a layout used a formula that is still rendering.
static PENDING: AtomicBool = AtomicBool::new(false);
/// Wakes the TUI event loop after every [UPDATES] bump. A ratatex worker calls
/// its update callback only after queueing the formula's upload, so the frame
/// drawn for this wake writes that upload. [Notify::notify_one] stores a permit
/// while the loop is busy, so a wake that lands mid-frame is not lost.
static WAKE: Notify = Notify::const_new();

/// Records a renderer change and wakes the event loop, in that order.
fn updated() {
    UPDATES.fetch_add(1, Ordering::AcqRel);
    WAKE.notify_one();
}

/// Resolves after a formula finishes, uploads are requeued or the renderer
/// starts. The TUI event loop awaits this alongside input and stream events;
/// with nothing rendering it stays pending, so idle terminals never wake.
pub async fn changed() {
    WAKE.notified().await;
}

/// Starts terminal detection and the renderer off the input loop. Idempotent.
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    INITIALIZING.store(true, Ordering::Release);
    let spawned = std::thread::Builder::new()
        .name("nanocodex-math".to_owned())
        .spawn(|| {
            let profile = detect();
            if profile.graphics == GraphicsSupport::Kitty {
                match Ratatex::builder(profile)
                    .on_update(updated)
                    .build()
                {
                    Ok(renderer) => {
                        *RENDERER.lock().unwrap_or_else(PoisonError::into_inner) = Some(renderer);
                        tracing::info!(target: "nanocodex", "display-math renderer ready");
                    }
                    Err(error) => {
                        tracing::warn!(target: "nanocodex", %error, "display-math renderer unavailable; showing LaTeX source");
                    }
                }
            }
            INITIALIZING.store(false, Ordering::Release);
            updated();
        });
    if spawned.is_err() {
        INITIALIZING.store(false, Ordering::Release);
    }
}

/// Stops renderer workers. A later [start] can initialize again.
pub fn shutdown() {
    if let Some(renderer) = RENDERER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        renderer.shutdown();
    }
    STARTED.store(false, Ordering::Release);
}

/// Terminal uploads queued by the renderer, in order. Write before the frame.
pub fn drain_commands(mut write: impl FnMut(&[u8]) -> std::io::Result<()>) -> std::io::Result<u64> {
    let commands = match RENDERER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        Some(renderer) => renderer.drain_terminal_commands(),
        None => return Ok(0),
    };
    let mut bytes = 0_u64;
    for command in commands {
        write(command.as_bytes())?;
        bytes = bytes.saturating_add(u64::try_from(command.len()).unwrap_or(u64::MAX));
    }
    Ok(bytes)
}

/// Re-sends every ready image after the terminal may have dropped them
/// (focus return through tmux, or an external editor on the alternate screen).
pub fn reupload_all() {
    if let Some(renderer) = RENDERER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        renderer.reupload_all();
    }
}

/// Monotonic counter that changes whenever a pending layout may now differ.
pub fn updates() -> u64 {
    UPDATES.load(Ordering::Acquire)
}

/// A formula layout is waiting for the renderer.
pub fn pending() -> bool {
    PENDING.load(Ordering::Acquire)
}

pub fn clear_pending() {
    PENDING.store(false, Ordering::Release);
}

/// Poll cadence while a formula is rendering; idle terminals never wake for math.
/// Finished formulas also wake the loop through [changed].
pub fn deadline(now: Instant) -> Option<Instant> {
    pending().then(|| now + Duration::from_millis(33))
}

pub enum Rendered {
    /// One placeholder span per terminal row.
    Ready {
        rows: Vec<Span<'static>>,
        columns: u16,
    },
    /// Show the source text. Pending formulas re-layout once their image is ready.
    Source { pending: bool },
}

pub fn render(source: &str, max_columns: u16) -> Rendered {
    let state = {
        let guard = RENDERER.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            Some(renderer) => renderer.request(source, max_columns.max(1)),
            None => {
                // Formulas laid out before detection finishes re-layout once it does.
                let pending = INITIALIZING.load(Ordering::Acquire);
                if pending {
                    PENDING.store(true, Ordering::Release);
                }
                return Rendered::Source { pending };
            }
        }
    };
    match state {
        FormulaState::Ready(formula) if formula.columns() <= max_columns => {
            let area = Rect::new(0, 0, formula.columns(), formula.rows());
            let mut buffer = Buffer::empty(area);
            FormulaWidget::new(&formula).render(area, &mut buffer);
            let rows = (0..formula.rows())
                .map(|y| {
                    let mut text = String::new();
                    let mut style = Style::default();
                    for x in 0..formula.columns() {
                        let cell = &buffer[(x, y)];
                        text.push_str(cell.symbol());
                        // The foreground colour encodes the Kitty image id.
                        if let ratatex_ratatui::style::Color::Rgb(red, green, blue) = cell.fg {
                            style = Style::default().fg(Color::Rgb(red, green, blue));
                        }
                    }
                    Span::styled(text, style)
                })
                .collect();
            Rendered::Ready {
                rows,
                columns: formula.columns(),
            }
        }
        FormulaState::Pending => {
            PENDING.store(true, Ordering::Release);
            Rendered::Source { pending: true }
        }
        FormulaState::Ready(_) | FormulaState::Failed(_) | FormulaState::Unsupported => {
            Rendered::Source { pending: false }
        }
    }
}

/// Rewrites TeX delimiters that CommonMark does not parse into the dollar
/// forms pulldown-cmark reports as math: \[..\], bare display environments,
/// and \(..\). Code spans and fenced code are left alone.
pub fn prepare(source: &str) -> Cow<'_, str> {
    if !(source.contains(r"\[") || source.contains(r"\(") || source.contains(r"\begin{")) {
        return Cow::Borrowed(source);
    }
    let display = ratatex::display_math(source);
    let mut regions = display
        .iter()
        .filter(|region| region.delimiter() != ratatex::DisplayMathDelimiter::Dollars)
        .map(|region| (region.range(), region.source().trim().to_owned(), true))
        .collect::<Vec<_>>();
    regions.extend(inline_math(source, &display));
    if regions.is_empty() {
        return Cow::Borrowed(source);
    }
    regions.sort_unstable_by_key(|(range, _, _)| range.start);
    let mut prepared = String::with_capacity(source.len());
    let mut cursor = 0;
    for (range, body, display) in regions {
        if range.start < cursor || body.is_empty() {
            continue;
        }
        prepared.push_str(&source[cursor..range.start]);
        if display {
            prepared.push_str("$$");
            prepared.push_str(&body);
            prepared.push_str("$$");
        } else {
            prepared.push('$');
            prepared.push_str(&body);
            prepared.push('$');
        }
        cursor = range.end;
    }
    prepared.push_str(&source[cursor..]);
    Cow::Owned(prepared)
}

fn inline_math(
    source: &str,
    display: &[ratatex::DisplayMath<'_>],
) -> Vec<(std::ops::Range<usize>, String, bool)> {
    if !source.contains(r"\(") {
        return Vec::new();
    }
    let mut protected = display
        .iter()
        .map(ratatex::DisplayMath::range)
        .collect::<Vec<_>>();
    let mut code_block_start = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    protected.push(start..range.end);
                }
            }
            Event::Code(_) => protected.push(range),
            _ => {}
        }
    }
    protected.sort_unstable_by_key(|range| range.start);
    let mut regions = Vec::new();
    let mut cursor = 0;
    let mut protected_index = 0;
    while cursor < source.len() {
        if let Some(range) = protected.get(protected_index) {
            if cursor >= range.end {
                protected_index += 1;
                continue;
            }
            if range.contains(&cursor) {
                cursor = range.end;
                continue;
            }
        }
        if source[cursor..].starts_with(r"\(")
            && !is_escaped_at(source, cursor)
            && let Some(end_start) = find_inline_math_end(source, cursor + 2)
        {
            let end = end_start + 2;
            let body = source[cursor + 2..end_start].trim();
            if !body.is_empty() && !body.contains('\n') {
                regions.push((cursor..end, body.to_owned(), false));
                cursor = end;
                continue;
            }
        }
        cursor += source[cursor..].chars().next().map_or(1, char::len_utf8);
    }
    regions
}

fn find_inline_math_end(source: &str, mut cursor: usize) -> Option<usize> {
    while cursor < source.len() {
        if source[cursor..].starts_with(r"\)") && !is_escaped_at(source, cursor) {
            return Some(cursor);
        }
        cursor += source[cursor..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn is_escaped_at(source: &str, index: usize) -> bool {
    source[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

fn detect() -> TerminalProfile {
    let tmux = env::var_os("TMUX").is_some();
    let client = if tmux { tmux_client() } else { None };
    let cell = client
        .as_ref()
        .and_then(|client| client.cell)
        .or_else(|| {
            crossterm::terminal::window_size()
                .ok()
                .and_then(window_cell)
        })
        .unwrap_or_default();
    let kitty = match env::var("NANOCODEX_TUI_GRAPHICS").ok().as_deref() {
        Some("kitty") => true,
        Some("off") => false,
        _ if tmux => client
            .as_ref()
            .is_some_and(|client| kitty_hint(&client.term)),
        _ => env::var("TERM_PROGRAM")
            .ok()
            .into_iter()
            .chain(env::var("TERM").ok())
            .any(|terminal| kitty_hint(&terminal)),
    };
    if kitty {
        TerminalProfile::kitty(cell, tmux)
    } else {
        TerminalProfile::unsupported(cell)
    }
}

fn kitty_hint(terminal: &str) -> bool {
    matches!(
        terminal
            .split_ascii_whitespace()
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("kitty" | "xterm-kitty" | "ghostty" | "xterm-ghostty")
    )
}

fn window_cell(size: crossterm::terminal::WindowSize) -> Option<PixelSize> {
    let width = size.width.checked_div(size.columns)?;
    let height = size.height.checked_div(size.rows)?;
    (width > 0 && height > 0).then(|| PixelSize::new(width, height))
}

struct TmuxClient {
    term: String,
    cell: Option<PixelSize>,
}

fn tmux_client() -> Option<TmuxClient> {
    let mut child = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#{client_termtype}\t#{client_cell_width}\t#{client_cell_height}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    let mut fields = output.trim().split('\t');
    let term = fields.next()?.to_owned();
    let width = fields.next().and_then(|value| value.parse::<u16>().ok());
    let height = fields.next().and_then(|value| value.parse::<u16>().ok());
    let cell = width
        .zip(height)
        .filter(|(width, height)| *width > 0 && *height > 0)
        .map(|(width, height)| PixelSize::new(width, height));
    Some(TmuxClient { term, cell })
}
