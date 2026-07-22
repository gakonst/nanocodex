use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Color,
};
use unicode_width::UnicodeWidthStr;

const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SelectionMode {
    #[default]
    Character,
    Word,
    Line,
}

#[derive(Clone, Copy)]
struct CompletedClick {
    at: Instant,
    position: Position,
    surface: Rect,
    count: u8,
}

#[derive(Default)]
pub(super) struct ScreenSelection {
    anchor: Option<Position>,
    head: Option<Position>,
    surface: Option<Rect>,
    selectable_areas: [Rect; 3],
    selectable_area_count: usize,
    dragging: bool,
    moved: bool,
    mode: SelectionMode,
    click_count: u8,
    completed_click: Option<CompletedClick>,
    copy_after_render: bool,
    pending_copy: Option<String>,
}

impl ScreenSelection {
    pub(super) fn begin(&mut self, position: Position) -> bool {
        self.begin_at(position, Instant::now())
    }

    fn begin_at(&mut self, position: Position, now: Instant) -> bool {
        self.pending_copy = None;
        self.copy_after_render = false;
        if !self.is_selectable(position) {
            return self.clear();
        }
        let surface = self.selectable_areas[..self.selectable_area_count]
            .iter()
            .copied()
            .find(|area| area.contains(position));
        let click_count = self.completed_click.map_or(1, |last| {
            if last.surface == surface.unwrap_or_default()
                && now.saturating_duration_since(last.at) <= MULTI_CLICK_INTERVAL
                && positions_are_near(last.position, position)
            {
                if last.count >= 3 { 1 } else { last.count + 1 }
            } else {
                1
            }
        });
        self.surface = surface;
        self.anchor = Some(position);
        self.head = Some(position);
        self.dragging = true;
        self.moved = false;
        self.click_count = click_count;
        self.mode = match click_count {
            2 => SelectionMode::Word,
            3 => SelectionMode::Line,
            _ => SelectionMode::Character,
        };
        true
    }

    pub(super) fn drag(&mut self, position: Position) -> bool {
        if !self.dragging || self.head == Some(position) {
            return false;
        }
        self.moved = true;
        self.head = Some(position);
        true
    }

    pub(super) fn finish(&mut self, position: Position) -> bool {
        self.finish_at(position, Instant::now())
    }

    fn finish_at(&mut self, position: Position, now: Instant) -> bool {
        if !self.dragging {
            return false;
        }
        self.dragging = false;
        self.moved |= self.head != Some(position);
        self.head = Some(position);
        if self.moved {
            self.completed_click = None;
        } else if let Some(surface) = self.surface {
            self.completed_click = Some(CompletedClick {
                at: now,
                position,
                surface,
                count: self.click_count,
            });
        }
        if self.anchor == self.head && self.mode == SelectionMode::Character {
            return self.clear_active();
        }
        self.copy_after_render = true;
        true
    }

    pub(super) fn clear(&mut self) -> bool {
        self.completed_click = None;
        self.clear_active()
    }

    fn clear_active(&mut self) -> bool {
        let changed = self.anchor.take().is_some() || self.head.take().is_some();
        self.surface = None;
        self.dragging = false;
        self.moved = false;
        self.mode = SelectionMode::Character;
        self.click_count = 0;
        self.copy_after_render = false;
        self.pending_copy = None;
        changed
    }

    pub(super) fn is_active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    pub(super) fn intersects(&self, area: Rect) -> bool {
        let Some((start, end)) = self.ordered() else {
            return false;
        };
        if area.is_empty() {
            return false;
        }
        let first_row = start.y.max(area.y);
        let last_row = end.y.min(area.bottom().saturating_sub(1));
        first_row <= last_row
            && (first_row..=last_row).any(|y| {
                let (first_x, last_x) = row_bounds(start, end, y, u16::MAX);
                first_x < area.right() && last_x >= area.x
            })
    }

    pub(super) fn render(&mut self, buffer: &mut Buffer, selectable_areas: &[Rect]) {
        self.selectable_area_count = selectable_areas.len().min(self.selectable_areas.len());
        self.selectable_areas[..self.selectable_area_count]
            .copy_from_slice(&selectable_areas[..self.selectable_area_count]);
        let Some((raw_start, raw_end)) = self.ordered() else {
            return;
        };

        let Some(surface) = self.surface else {
            return;
        };
        let (start, end) = resolve_selection(
            buffer,
            surface,
            self.anchor.unwrap_or(raw_start),
            self.head.unwrap_or(raw_end),
            self.mode,
        );
        if self.copy_after_render {
            let text = selected_text(buffer, surface, start, end);
            if !text.is_empty() {
                self.pending_copy = Some(text);
            }
            self.copy_after_render = false;
        }
        highlight(buffer, surface, start, end);
    }

    pub(super) fn take_pending_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    fn ordered(&self) -> Option<(Position, Position)> {
        let anchor = self.anchor?;
        let head = self.head?;
        if (anchor.y, anchor.x) <= (head.y, head.x) {
            Some((anchor, head))
        } else {
            Some((head, anchor))
        }
    }

    fn is_selectable(&self, position: Position) -> bool {
        self.selectable_areas[..self.selectable_area_count]
            .iter()
            .any(|area| area.contains(position))
    }
}

fn positions_are_near(left: Position, right: Position) -> bool {
    left.x.abs_diff(right.x) <= 1 && left.y.abs_diff(right.y) <= 1
}

fn resolve_selection(
    buffer: &Buffer,
    surface: Rect,
    anchor: Position,
    head: Position,
    mode: SelectionMode,
) -> (Position, Position) {
    match mode {
        SelectionMode::Character => ordered_positions(anchor, head),
        SelectionMode::Line => {
            let (start, end) = ordered_positions(anchor, head);
            (
                Position::new(surface.x, start.y.max(surface.y)),
                Position::new(
                    surface.right().saturating_sub(1),
                    end.y.min(surface.bottom().saturating_sub(1)),
                ),
            )
        }
        SelectionMode::Word => {
            let anchor_word = word_bounds(buffer, surface, clamp_to_surface(anchor, surface));
            let head_word = word_bounds(buffer, surface, clamp_to_surface(head, surface));
            match (anchor_word, head_word) {
                (Some(anchor_word), Some(head_word)) => {
                    if (anchor_word.0.y, anchor_word.0.x) <= (head_word.0.y, head_word.0.x) {
                        (anchor_word.0, head_word.1)
                    } else {
                        (head_word.0, anchor_word.1)
                    }
                }
                _ => ordered_positions(anchor, head),
            }
        }
    }
}

fn ordered_positions(left: Position, right: Position) -> (Position, Position) {
    if (left.y, left.x) <= (right.y, right.x) {
        (left, right)
    } else {
        (right, left)
    }
}

fn clamp_to_surface(position: Position, surface: Rect) -> Position {
    Position::new(
        position
            .x
            .clamp(surface.x, surface.right().saturating_sub(1)),
        position
            .y
            .clamp(surface.y, surface.bottom().saturating_sub(1)),
    )
}

fn word_bounds(buffer: &Buffer, surface: Rect, position: Position) -> Option<(Position, Position)> {
    if !surface.contains(position) || !buffer.area.contains(position) {
        return None;
    }
    let class = word_class(buffer[(position.x, position.y)].symbol());
    if class == WordClass::Blank {
        return None;
    }
    let mut first = position.x;
    while first > surface.x
        && word_class(buffer[(first.saturating_sub(1), position.y)].symbol()) == class
    {
        first = first.saturating_sub(1);
    }
    let mut last = position.x;
    while last.saturating_add(1) < surface.right()
        && word_class(buffer[(last.saturating_add(1), position.y)].symbol()) == class
    {
        last = last.saturating_add(1);
    }
    Some((
        Position::new(first, position.y),
        Position::new(last, position.y),
    ))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WordClass {
    Blank,
    Word,
    Punctuation,
}

fn word_class(symbol: &str) -> WordClass {
    if symbol.is_empty() || symbol == " " {
        WordClass::Blank
    } else if symbol
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_')
    {
        WordClass::Word
    } else {
        WordClass::Punctuation
    }
}

fn selected_text(buffer: &Buffer, surface: Rect, start: Position, end: Position) -> String {
    let mut output = String::new();
    let last_buffer_x = buffer.area.right().saturating_sub(1);
    let first_y = start.y.max(buffer.area.y);
    let last_y = end.y.min(buffer.area.bottom().saturating_sub(1));
    if first_y > last_y {
        return output;
    }

    let mut included_row = false;
    for y in first_y..=last_y {
        let (first_x, last_x) = row_bounds(start, end, y, last_buffer_x);
        if y < surface.y || y >= surface.bottom() {
            continue;
        }
        let mut row = String::new();
        let mut x = first_x.max(surface.x).max(buffer.area.x);
        let last_x = last_x
            .min(surface.right().saturating_sub(1))
            .min(last_buffer_x);
        if let Some(last_x) = selected_row_end(buffer, y, x, last_x) {
            while x <= last_x {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x = x.saturating_add(
                    u16::try_from(UnicodeWidthStr::width(symbol))
                        .unwrap_or(u16::MAX)
                        .max(1),
                );
            }
        }
        if included_row {
            output.push('\n');
        }
        output.push_str(&row);
        included_row = true;
    }
    output
}

fn highlight(buffer: &mut Buffer, surface: Rect, start: Position, end: Position) {
    let last_buffer_x = buffer.area.right().saturating_sub(1);
    let first_y = start.y.max(buffer.area.y);
    let last_y = end.y.min(buffer.area.bottom().saturating_sub(1));
    if first_y > last_y {
        return;
    }
    for y in first_y..=last_y {
        if y < surface.y || y >= surface.bottom() {
            continue;
        }
        let (first_x, last_x) = row_bounds(start, end, y, last_buffer_x);
        let first_x = first_x.max(surface.x).max(buffer.area.x);
        let last_x = last_x
            .min(surface.right().saturating_sub(1))
            .min(last_buffer_x);
        let Some(last_x) = selected_row_end(buffer, y, first_x, last_x) else {
            continue;
        };
        for x in first_x..=last_x {
            buffer[(x, y)].set_fg(Color::Black).set_bg(Color::LightBlue);
        }
    }
}

fn selected_row_end(buffer: &Buffer, y: u16, first_x: u16, last_x: u16) -> Option<u16> {
    if first_x > last_x || y < buffer.area.y || y >= buffer.area.bottom() {
        return None;
    }
    for x in (first_x..=last_x).rev() {
        let symbol = buffer[(x, y)].symbol();
        if symbol != " " && !symbol.is_empty() {
            let width = u16::try_from(UnicodeWidthStr::width(symbol))
                .unwrap_or(u16::MAX)
                .max(1);
            return Some(x.saturating_add(width.saturating_sub(1)).min(last_x));
        }
    }
    None
}

fn row_bounds(start: Position, end: Position, y: u16, last_x: u16) -> (u16, u16) {
    match (y == start.y, y == end.y) {
        (true, true) => (start.x, end.x),
        (true, false) => (start.x, last_x),
        (false, true) => (0, end.x),
        (false, false) => (0, last_x),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ratatui::{buffer::Buffer, layout::Rect};

    use super::ScreenSelection;

    #[test]
    fn selection_copies_only_text_from_the_surface_where_it_started() {
        let mut buffer = Buffer::with_lines([
            "header chrome   ",
            "  first line   ",
            "  second line  ",
            "composer text  ",
            "footer chrome   ",
        ]);
        let selectable = [Rect::new(2, 1, 12, 2), Rect::new(0, 3, 13, 1)];
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &selectable);

        assert!(selection.begin((2, 1).into()));
        assert!(selection.drag((12, 3).into()));
        assert!(selection.finish((12, 3).into()));
        selection.render(&mut buffer, &selectable);

        assert_eq!(
            selection.take_pending_copy().as_deref(),
            Some("first line\nsecond line")
        );
        assert_eq!(
            buffer.cell((2, 1)).unwrap().bg,
            ratatui::style::Color::LightBlue
        );
        assert_ne!(
            buffer.cell((0, 0)).unwrap().bg,
            ratatui::style::Color::LightBlue
        );
        assert_ne!(
            buffer.cell((13, 1)).unwrap().bg,
            ratatui::style::Color::LightBlue
        );
    }

    #[test]
    fn selection_does_not_highlight_or_copy_padding_after_text() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buffer = Buffer::with_lines(["  indented", "short"]);
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);

        assert!(selection.begin((0, 0).into()));
        assert!(selection.finish((11, 1).into()));
        selection.render(&mut buffer, &[area]);

        assert_eq!(
            selection.take_pending_copy().as_deref(),
            Some("  indented\nshort")
        );
        assert_eq!(
            buffer.cell((4, 1)).unwrap().bg,
            ratatui::style::Color::LightBlue
        );
        assert_ne!(
            buffer.cell((5, 1)).unwrap().bg,
            ratatui::style::Color::LightBlue
        );
    }

    #[test]
    fn selection_stays_with_the_surface_where_the_drag_started() {
        let mut buffer = Buffer::with_lines(["left    right"]);
        let surfaces = [Rect::new(0, 0, 4, 1), Rect::new(8, 0, 5, 1)];
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &surfaces);

        assert!(selection.begin((0, 0).into()));
        assert!(selection.finish((12, 0).into()));
        selection.render(&mut buffer, &surfaces);

        assert_eq!(selection.take_pending_copy().as_deref(), Some("left"));
        assert_ne!(
            buffer.cell((8, 0)).unwrap().bg,
            ratatui::style::Color::LightBlue
        );
    }

    #[test]
    fn wide_graphemes_do_not_add_continuation_spaces() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "界abc", ratatui::style::Style::default());
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);
        assert!(selection.begin((0, 0).into()));
        assert!(selection.finish((4, 0).into()));
        selection.render(&mut buffer, &[area]);

        assert_eq!(selection.take_pending_copy().as_deref(), Some("界abc"));
    }

    #[test]
    fn a_plain_click_clears_the_selection_without_copying() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buffer = Buffer::with_lines(["text"]);
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);
        assert!(selection.begin((0, 0).into()));
        assert!(selection.finish((0, 0).into()));
        selection.render(&mut buffer, &[area]);

        assert!(!selection.is_active());
        assert!(selection.take_pending_copy().is_none());
    }

    #[test]
    fn double_click_copies_a_word_and_triple_click_copies_the_line() {
        let area = Rect::new(0, 0, 25, 1);
        let mut buffer = Buffer::with_lines(["let snake_case = value"]);
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);
        let start = Instant::now();
        let position = (6, 0).into();

        assert!(selection.begin_at(position, start));
        assert!(selection.finish_at(position, start + Duration::from_millis(10)));
        selection.render(&mut buffer, &[area]);
        assert!(selection.take_pending_copy().is_none());

        assert!(selection.begin_at(position, start + Duration::from_millis(100)));
        selection.render(&mut buffer, &[area]);
        assert!(selection.take_pending_copy().is_none());
        assert!(selection.finish_at(position, start + Duration::from_millis(110)));
        selection.render(&mut buffer, &[area]);
        assert_eq!(selection.take_pending_copy().as_deref(), Some("snake_case"));

        assert!(selection.begin_at(position, start + Duration::from_millis(200)));
        assert!(selection.finish_at(position, start + Duration::from_millis(210)));
        selection.render(&mut buffer, &[area]);
        assert_eq!(
            selection.take_pending_copy().as_deref(),
            Some("let snake_case = value")
        );
    }

    #[test]
    fn clicks_outside_the_multi_click_window_start_a_new_single_click() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buffer = Buffer::with_lines(["word"]);
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);
        let start = Instant::now();
        let position = (1, 0).into();

        assert!(selection.begin_at(position, start));
        assert!(selection.finish_at(position, start + Duration::from_millis(10)));
        assert!(selection.begin_at(position, start + Duration::from_millis(600)));
        assert!(selection.finish_at(position, start + Duration::from_millis(610)));
        selection.render(&mut buffer, &[area]);

        assert!(selection.take_pending_copy().is_none());
        assert!(!selection.is_active());
    }
}
