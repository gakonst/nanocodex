use std::{env, process::Stdio, time::Duration};

use ratatex::{PixelSize, TerminalProfile};
use tokio::process::Command;

// Ratatex's stdio query can leave a blocking stdin reader behind when a terminal
// never answers. Use terminal hints instead so EventStream owns all keyboard input.
pub(super) async fn detect() -> TerminalProfile {
    let tmux = env::var_os("TMUX").is_some();
    let client = if tmux { tmux_client().await } else { None };
    let cell = client
        .as_ref()
        .and_then(|client| client.cell)
        .or_else(|| {
            crossterm::terminal::window_size()
                .ok()
                .and_then(window_cell)
        })
        .unwrap_or_default();
    let term = env::var("TERM").ok();
    let program = env::var("TERM_PROGRAM").ok();
    let override_value = env::var("NANOCODEX_TUI_GRAPHICS").ok();
    let kitty = match override_value.as_deref() {
        Some("kitty") => true,
        Some("off") => false,
        _ if tmux => client
            .as_ref()
            .is_some_and(|client| kitty_hint(&client.term)),
        _ => program
            .as_deref()
            .into_iter()
            .chain(term.as_deref())
            .any(kitty_hint),
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

async fn tmux_client() -> Option<TmuxClient> {
    let output = tokio::time::timeout(
        Duration::from_secs(1),
        Command::new("tmux")
            .args([
                "display-message",
                "-p",
                "#{client_termtype}\t#{client_cell_width}\t#{client_cell_height}",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_tmux_client(&String::from_utf8_lossy(&output.stdout))
}

fn parse_tmux_client(output: &str) -> Option<TmuxClient> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enables_only_known_kitty_placeholder_terminals() {
        for terminal in ["kitty", "xterm-kitty", "Ghostty", "xterm-ghostty"] {
            assert!(kitty_hint(terminal), "{terminal}");
        }
        for terminal in ["xterm-256color", "Apple_Terminal", "WezTerm", "tmux", ""] {
            assert!(!kitty_hint(terminal), "{terminal}");
        }
    }

    #[test]
    fn cell_geometry_ignores_missing_pixel_dimensions() {
        let size = || crossterm::terminal::WindowSize {
            columns: 110,
            rows: 32,
            width: 1100,
            height: 640,
        };
        assert_eq!(window_cell(size()), Some(PixelSize::new(10, 20)));
        assert_eq!(
            window_cell(crossterm::terminal::WindowSize { width: 0, ..size() }),
            None
        );
        assert_eq!(
            window_cell(crossterm::terminal::WindowSize { rows: 0, ..size() }),
            None
        );
    }

    #[test]
    fn tmux_hint_uses_client_terminal_and_valid_cell_dimensions() {
        let client = parse_tmux_client("xterm-kitty\t9\t18\n").unwrap();
        assert!(kitty_hint(&client.term));
        assert_eq!(client.cell, Some(PixelSize::new(9, 18)));
        for output in ["xterm-kitty\t0\t18", "xterm-kitty\t9\t", "xterm-kitty"] {
            assert_eq!(parse_tmux_client(output).unwrap().cell, None);
        }
    }
}
