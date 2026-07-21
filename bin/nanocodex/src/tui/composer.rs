use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CursorPosition {
    pub(super) row: usize,
    pub(super) column: usize,
}

#[derive(Debug)]
pub(super) struct ComposerLayout {
    rows: Vec<Range<usize>>,
}

impl ComposerLayout {
    pub(super) fn new(text: &str, width: u16) -> Self {
        let width = usize::from(width.max(1));
        let mut rows = Vec::new();
        let mut row_start = 0;
        let mut row_width = 0usize;

        for (index, character) in text.char_indices() {
            if character == '\n' {
                rows.push(row_start..index);
                row_start = index + character.len_utf8();
                row_width = 0;
                continue;
            }

            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if row_width > 0 && row_width.saturating_add(character_width) > width {
                rows.push(row_start..index);
                row_start = index;
                row_width = 0;
            }
            row_width = row_width.saturating_add(character_width);
        }

        rows.push(row_start..text.len());
        if !text.ends_with('\n') && row_width == width {
            rows.push(text.len()..text.len());
        }

        Self { rows }
    }

    pub(super) fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn row(&self, index: usize) -> Option<&Range<usize>> {
        self.rows.get(index)
    }

    pub(super) fn cursor_position(&self, text: &str, cursor: usize) -> CursorPosition {
        let cursor = cursor.min(text.len());
        for (row, range) in self.rows.iter().enumerate() {
            if cursor < range.end {
                return CursorPosition {
                    row,
                    column: UnicodeWidthStr::width(&text[range.start..cursor]),
                };
            }
            if cursor == range.end {
                let continues_on_next_visual_row = self
                    .rows
                    .get(row + 1)
                    .is_some_and(|next| next.start == cursor);
                if continues_on_next_visual_row {
                    continue;
                }
                return CursorPosition {
                    row,
                    column: UnicodeWidthStr::width(&text[range.clone()]),
                };
            }
        }

        CursorPosition {
            row: self.rows.len().saturating_sub(1),
            column: 0,
        }
    }

    pub(super) fn byte_at_column(&self, text: &str, row: usize, column: usize) -> usize {
        let Some(range) = self.rows.get(row) else {
            return text.len();
        };
        let mut current_column = 0usize;
        for (offset, character) in text[range.clone()].char_indices() {
            let next_column =
                current_column.saturating_add(UnicodeWidthChar::width(character).unwrap_or(0));
            if next_column > column {
                return range.start + offset;
            }
            current_column = next_column;
            if current_column == column {
                return range.start + offset + character.len_utf8();
            }
        }
        range.end
    }
}

#[cfg(test)]
mod tests {
    use super::{ComposerLayout, CursorPosition};

    #[test]
    fn layout_hard_wraps_and_maps_cursor_boundaries_consistently() {
        let text = "abcd界zz";
        let layout = ComposerLayout::new(text, 4);

        assert_eq!(layout.row_count(), 3);
        assert_eq!(
            layout.row(0).map(|range| &text[range.clone()]),
            Some("abcd")
        );
        assert_eq!(
            layout.row(1).map(|range| &text[range.clone()]),
            Some("界zz")
        );
        assert_eq!(
            layout.cursor_position(text, 4),
            CursorPosition { row: 1, column: 0 }
        );
        assert_eq!(
            layout.cursor_position(text, text.len()),
            CursorPosition { row: 2, column: 0 }
        );
    }

    #[test]
    fn layout_distinguishes_newlines_from_visual_wrap_boundaries() {
        let text = "abcd\nxy";
        let layout = ComposerLayout::new(text, 4);

        assert_eq!(layout.row_count(), 2);
        assert_eq!(
            layout.cursor_position(text, 4),
            CursorPosition { row: 0, column: 4 }
        );
        assert_eq!(
            layout.cursor_position(text, 5),
            CursorPosition { row: 1, column: 0 }
        );
        assert_eq!(layout.byte_at_column(text, 1, 1), 6);
    }
}
