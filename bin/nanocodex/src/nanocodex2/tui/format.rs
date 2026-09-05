// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Shared formatting for terminal-facing values.

use std::{borrow::Cow, env, path::Path};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn normalize_line_endings(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }

    let mut normalized = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(index) = remaining.find('\r') {
        normalized.push_str(&remaining[..index]);
        normalized.push('\n');
        remaining = &remaining[index + 1..];
        if let Some(after_newline) = remaining.strip_prefix('\n') {
            remaining = after_newline;
        }
    }
    normalized.push_str(remaining);
    Cow::Owned(normalized)
}

pub(crate) fn sanitize_terminal_text(text: &str) -> Cow<'_, str> {
    sanitize_terminal_text_with_break(text, '\n')
}

pub(crate) fn sanitize_terminal_text_inline(text: &str) -> Cow<'_, str> {
    sanitize_terminal_text_with_break(text, ' ')
}

fn sanitize_terminal_text_with_break(text: &str, line_break: char) -> Cow<'_, str> {
    let requires_sanitization = text.chars().any(|character| {
        character == '\r'
            || character == '\t'
            || character.is_control() && character != '\n'
            || character == '\n' && line_break != '\n'
    });
    if !requires_sanitization {
        return Cow::Borrowed(text);
    }

    let mut sanitized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                sanitized.push(line_break);
            }
            '\n' => sanitized.push(line_break),
            '\t' => sanitized.push_str("    "),
            character if character.is_control() => sanitized.push('�'),
            character => sanitized.push(character),
        }
    }
    Cow::Owned(sanitized)
}

pub(crate) fn terminal_text_width(text: &str) -> usize {
    text.graphemes(true)
        .map(|grapheme| match grapheme {
            "\t" => 4,
            grapheme if grapheme.contains(char::is_control) => 1,
            grapheme => grapheme.width(),
        })
        .sum()
}

pub(crate) fn format_duration(nanoseconds: u64) -> String {
    if nanoseconds >= 1_000_000_000 {
        let tenths = duration_display_tick(nanoseconds).saturating_sub(1_000);
        return format!("{}.{:01}s", tenths / 10, tenths % 10);
    }
    format!("{}ms", duration_display_tick(nanoseconds))
}

pub(crate) fn format_turn_duration(nanoseconds: u64) -> String {
    let total_seconds = nanoseconds / 1_000_000_000;
    let days = total_seconds / 86_400;
    let hours = total_seconds % 86_400 / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

pub(crate) fn duration_display_tick(nanoseconds: u64) -> u64 {
    if nanoseconds < 1_000_000_000 {
        return nanoseconds / 1_000_000;
    }
    1_000_u64.saturating_add(nanoseconds.saturating_add(50_000_000) / 100_000_000)
}

pub(crate) fn humanize_tool(name: &str) -> String {
    name.trim_start_matches("mcp__")
        .replace("__", " · ")
        .replace('_', " ")
}

pub(crate) fn shorten_home(path: &Path) -> String {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    let Some(home) = home else {
        return path.display().to_string();
    };
    if path == home {
        return "~".to_owned();
    }
    let Ok(relative) = path.strip_prefix(&home) else {
        return path.display().to_string();
    };
    format!("~/{}", relative.display())
}

#[cfg(test)]
mod tests {
    use super::{
        duration_display_tick, format_duration, format_turn_duration, normalize_line_endings,
        sanitize_terminal_text, sanitize_terminal_text_inline, terminal_text_width,
    };

    #[test]
    fn line_endings_are_normalized_without_changing_lf_text() {
        assert_eq!(normalize_line_endings("one\ntwo"), "one\ntwo");
        assert_eq!(
            normalize_line_endings("one\r\ntwo\rthree"),
            "one\ntwo\nthree"
        );
    }

    #[test]
    fn terminal_text_has_safe_multiline_and_inline_projections() {
        let text = "one\r\ntwo\tthree\u{1b}";
        assert_eq!(sanitize_terminal_text(text), "one\ntwo    three�");
        assert_eq!(sanitize_terminal_text_inline(text), "one two    three�");
        assert_eq!(terminal_text_width("two\tthree\u{1b}"), 13);
    }

    #[test]
    fn durations_round_to_the_same_tick_used_for_live_redraws() {
        for (nanoseconds, expected) in [
            (999_999_999, "999ms"),
            (1_049_999_999, "1.0s"),
            (1_050_000_000, "1.1s"),
            (11_249_999_999, "11.2s"),
            (11_250_000_000, "11.3s"),
        ] {
            assert_eq!(format_duration(nanoseconds), expected);
        }
        assert_eq!(duration_display_tick(1_050_000_000), 1_011);
    }

    #[test]
    fn turn_durations_only_use_whole_seconds_and_larger_units() {
        for (nanoseconds, expected) in [
            (999_999_999, "0s"),
            (5_000_000_000, "5s"),
            (65_000_000_000, "1m 5s"),
            (3_665_000_000_000, "1h 1m 5s"),
            (176_465_000_000_000, "2d 1h 1m 5s"),
        ] {
            assert_eq!(format_turn_duration(nanoseconds), expected);
        }
    }
}
