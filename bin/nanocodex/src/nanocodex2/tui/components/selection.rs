// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Semantic selection shared by transcript and composer surfaces.

use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Surface {
    Transcript,
    Composer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TextSpan {
    pub(super) block: usize,
    pub(super) start: usize,
    pub(super) end: usize,
}

impl TextSpan {
    pub(super) const fn new(block: usize, start: usize, end: usize) -> Self {
        Self { block, start, end }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Point {
    surface: Surface,
    span: TextSpan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TextRange {
    anchor: TextSpan,
    head: TextSpan,
}

impl TextRange {
    pub(super) fn bounds(self) -> (TextSpan, TextSpan) {
        if (self.anchor.block, self.anchor.start) <= (self.head.block, self.head.start) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub(super) fn source_range(self, block: usize, source_len: usize) -> Option<Range<usize>> {
        let (start, end) = self.bounds();
        if block < start.block || block > end.block {
            return None;
        }
        let range_start = if block == start.block { start.start } else { 0 };
        let range_end = if block == end.block {
            end.end
        } else {
            source_len
        };
        (range_start < range_end).then_some(range_start..range_end)
    }

    pub(super) fn includes(self, block: usize, source: &Range<usize>) -> bool {
        self.source_range(block, usize::MAX)
            .is_some_and(|selected| selected.start < source.end && source.start < selected.end)
    }
}

#[derive(Default)]
pub(super) struct Selection {
    surface: Option<Surface>,
    pending: Option<Point>,
    range: Option<TextRange>,
}

impl Selection {
    pub(super) fn begin(&mut self, surface: Surface, span: TextSpan) {
        self.surface = Some(surface);
        self.pending = Some(Point { surface, span });
        self.range = None;
    }

    pub(super) fn drag(&mut self, span: TextSpan) -> bool {
        let Some(anchor) = self.pending.or_else(|| {
            self.range.map(|range| Point {
                surface: self.surface.expect("a range has a surface"),
                span: range.anchor,
            })
        }) else {
            return false;
        };
        let range = TextRange {
            anchor: anchor.span,
            head: span,
        };
        let changed = self.range != Some(range);
        self.range = Some(range);
        changed
    }

    pub(super) fn finish(&mut self, span: TextSpan) -> bool {
        let Some(anchor) = self.pending.or_else(|| {
            self.range.map(|range| Point {
                surface: self.surface.expect("a range has a surface"),
                span: range.anchor,
            })
        }) else {
            return false;
        };
        if self.range.is_none() && span == anchor.span {
            self.pending = None;
            return false;
        }
        self.range = Some(TextRange {
            anchor: anchor.span,
            head: span,
        });
        self.pending = None;
        true
    }

    pub(super) fn clear(&mut self) -> bool {
        let changed = self.pending.take().is_some() || self.range.take().is_some();
        self.surface = None;
        changed
    }

    pub(super) const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) const fn is_active(&self) -> bool {
        self.range.is_some()
    }

    pub(super) const fn surface(&self) -> Option<Surface> {
        self.surface
    }

    pub(super) fn range(&self) -> Option<TextRange> {
        self.range.or_else(|| {
            self.pending.map(|point| TextRange {
                anchor: point.span,
                head: point.span,
            })
        })
    }

    pub(super) fn take_range(&mut self) -> Option<TextRange> {
        self.pending = None;
        self.surface = None;
        self.range.take()
    }
}

#[cfg(test)]
mod tests {
    use super::{Selection, Surface, TextSpan};

    #[test]
    fn semantic_ranges_are_ordered_and_span_whole_intermediate_blocks() {
        let mut selection = Selection::default();
        selection.begin(Surface::Transcript, TextSpan::new(3, 4, 5));
        selection.drag(TextSpan::new(1, 2, 3));
        let range = selection.range().unwrap();

        assert_eq!(range.source_range(1, 10), Some(2..10));
        assert_eq!(range.source_range(2, 10), Some(0..10));
        assert_eq!(range.source_range(3, 10), Some(0..5));
        assert_eq!(range.source_range(4, 10), None);
    }

    #[test]
    fn pending_click_does_not_create_a_range() {
        let mut selection = Selection::default();
        let span = TextSpan::new(0, 1, 2);
        selection.begin(Surface::Composer, span);

        assert!(!selection.finish(span));
        assert!(selection.take_range().is_none());
    }
}
