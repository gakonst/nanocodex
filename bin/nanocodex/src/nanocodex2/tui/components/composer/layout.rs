// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Grapheme-aware soft and hard wrapping for the composer.

use crate::tui::format::terminal_text_width;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VisualLine {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) width: usize,
}

#[derive(Debug)]
pub(super) struct VisualLayout {
    pub(super) lines: Vec<VisualLine>,
    pub(super) cursor_row: usize,
    pub(super) cursor_column: usize,
}

impl VisualLayout {
    pub(super) fn new(text: &str, cursor: usize, width: usize) -> Self {
        let lines = wrap_text(text, width.max(1));
        let (cursor_row, cursor_column) = locate_cursor(text, &lines, cursor);
        Self {
            lines,
            cursor_row,
            cursor_column,
        }
    }
}

pub(super) fn wrap_text(text: &str, width: usize) -> Vec<VisualLine> {
    let mut lines = Vec::new();
    let mut logical_start = 0;

    loop {
        let newline = text[logical_start..]
            .find('\n')
            .map(|offset| logical_start + offset);
        let logical_end = newline.unwrap_or(text.len());
        wrap_logical_line(text, logical_start, logical_end, width, &mut lines);

        let Some(newline) = newline else {
            break;
        };
        logical_start = newline + 1;
        if logical_start == text.len() {
            lines.push(VisualLine {
                start: logical_start,
                end: logical_start,
                width: 0,
            });
            break;
        }
    }

    if lines.is_empty() {
        lines.push(VisualLine {
            start: 0,
            end: 0,
            width: 0,
        });
    }
    if text.len() == lines.last().map_or(0, |line| line.end)
        && lines.last().is_some_and(|line| line.width == width)
    {
        lines.push(VisualLine {
            start: text.len(),
            end: text.len(),
            width: 0,
        });
    }

    lines
}

fn wrap_logical_line(
    text: &str,
    start: usize,
    end: usize,
    width: usize,
    lines: &mut Vec<VisualLine>,
) {
    if start == end {
        lines.push(VisualLine {
            start,
            end,
            width: 0,
        });
        return;
    }

    let mut line_start = start;
    while line_start < end {
        let mut used = 0;
        let mut candidate_end = line_start;
        let mut last_word_break = None;

        for (offset, grapheme) in text[line_start..end].grapheme_indices(true) {
            let grapheme_start = line_start + offset;
            let grapheme_width = terminal_text_width(grapheme);
            if candidate_end > line_start && used + grapheme_width > width {
                break;
            }

            candidate_end = grapheme_start + grapheme.len();
            used += grapheme_width;
            if grapheme.chars().all(char::is_whitespace) {
                last_word_break = Some(candidate_end);
            }
            if used >= width {
                break;
            }
        }

        if candidate_end == line_start {
            let Some(grapheme) = text[line_start..end].graphemes(true).next() else {
                return;
            };
            candidate_end += grapheme.len();
        }

        let overflowed = candidate_end < end;
        let line_end = if overflowed {
            last_word_break
                .filter(|word_break| *word_break > line_start)
                .unwrap_or(candidate_end)
        } else {
            candidate_end
        };
        lines.push(VisualLine {
            start: line_start,
            end: line_end,
            width: terminal_text_width(&text[line_start..line_end]),
        });
        line_start = line_end;
    }
}

fn locate_cursor(text: &str, lines: &[VisualLine], cursor: usize) -> (usize, usize) {
    for (row, line) in lines.iter().enumerate() {
        if cursor < line.end || (line.start == line.end && cursor == line.start) {
            return (
                row,
                terminal_text_width(&text[line.start..cursor.min(line.end)]),
            );
        }
        if cursor == line.end {
            let next_starts_here = lines.get(row + 1).is_some_and(|next| next.start == cursor);
            if next_starts_here {
                continue;
            }
            return (row, line.width);
        }
    }

    let row = lines.len().saturating_sub(1);
    (row, lines[row].width)
}

pub(super) fn byte_at_column(text: &str, line: &VisualLine, target: usize) -> usize {
    let mut column = 0;
    for (offset, grapheme) in text[line.start..line.end].grapheme_indices(true) {
        let next = column + terminal_text_width(grapheme);
        if next > target {
            return line.start + offset;
        }
        column = next;
    }
    line.end
}

pub(super) fn grapheme_at_column(text: &str, line: &VisualLine, target: usize) -> Range<usize> {
    let mut column = 0;
    for (offset, grapheme) in text[line.start..line.end].grapheme_indices(true) {
        let start = line.start + offset;
        let next = column + terminal_text_width(grapheme);
        if next > target {
            return start..start + grapheme.len();
        }
        column = next;
    }
    line.end..line.end
}

#[cfg(test)]
mod tests {
    use super::{VisualLayout, grapheme_at_column};

    #[test]
    fn hit_testing_returns_whole_graphemes() {
        let text = "a界e\u{301}";
        let layout = VisualLayout::new(text, 0, 10);
        let line = &layout.lines[0];

        assert_eq!(grapheme_at_column(text, line, 0), 0..1);
        assert_eq!(grapheme_at_column(text, line, 1), 1..4);
        assert_eq!(grapheme_at_column(text, line, 2), 1..4);
        assert_eq!(grapheme_at_column(text, line, 3), 4..7);
        assert_eq!(grapheme_at_column(text, line, 4), 7..7);
    }
}
