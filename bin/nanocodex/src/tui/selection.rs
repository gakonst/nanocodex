use std::time::{Duration, Instant};

use ratatex::is_formula_placeholder;
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);
const MULTI_CLICK_COPY_DELAY: Duration = Duration::from_millis(200);
const COPY_HIGHLIGHT_DURATION: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectionScrollDirection {
    Older,
    Newer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SelectionScrollRequest {
    pub(super) surface_index: usize,
    pub(super) direction: SelectionScrollDirection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SelectionClick {
    pub(super) surface_index: usize,
    pub(super) surface: Rect,
    pub(super) position: Position,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SelectionMode {
    #[default]
    Character,
    Word,
    Line,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum HighlightMode {
    #[default]
    Selection,
    Copy,
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
    surface_index: Option<usize>,
    selectable_areas: [Rect; 3],
    selectable_area_count: usize,
    dragging: bool,
    moved: bool,
    mode: SelectionMode,
    click_count: u8,
    completed_click: Option<CompletedClick>,
    copy_after_render: bool,
    pending_copy: Option<String>,
    highlight_mode: HighlightMode,
    copy_at: Option<Instant>,
    clear_at: Option<Instant>,
    auto_scroll: Option<SelectionScrollDirection>,
    scroll_snapshot: Option<Buffer>,
    copied_before: Vec<String>,
    copied_after: Vec<String>,
    pending_click: Option<SelectionClick>,
    formula_cells: Vec<Position>,
    anchor_was_formula: bool,
}

impl ScreenSelection {
    pub(super) fn begin(&mut self, position: Position) -> bool {
        self.begin_at(position, Instant::now())
    }

    fn begin_at(&mut self, position: Position, now: Instant) -> bool {
        let anchor_was_formula = self.formula_cells.contains(&position);
        self.pending_copy = None;
        self.copy_after_render = false;
        self.highlight_mode = HighlightMode::Selection;
        self.copy_at = None;
        self.clear_at = None;
        self.auto_scroll = None;
        self.scroll_snapshot = None;
        self.copied_before.clear();
        self.copied_after.clear();
        self.pending_click = None;
        self.anchor_was_formula = anchor_was_formula;
        if !self.is_selectable(position) {
            return self.clear();
        }
        let surface_index = self.selectable_areas[..self.selectable_area_count]
            .iter()
            .position(|area| area.contains(position));
        let surface = surface_index.map(|index| self.selectable_areas[index]);
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
        self.surface_index = surface_index;
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
        if !self.dragging {
            return false;
        }
        let auto_scroll = self.auto_scroll_direction(position);
        let changed = self.head != Some(position) || self.auto_scroll != auto_scroll;
        if self.head != Some(position) {
            self.moved = true;
            self.head = Some(position);
        }
        self.auto_scroll = auto_scroll;
        changed
    }

    pub(super) fn finish(&mut self, position: Position) -> bool {
        self.finish_at(position, Instant::now())
    }

    fn finish_at(&mut self, position: Position, now: Instant) -> bool {
        if !self.dragging {
            return false;
        }
        self.dragging = false;
        self.auto_scroll = None;
        self.scroll_snapshot = None;
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
            if self.anchor_was_formula {
                self.completed_click = None;
                self.click_count = 0;
                return true;
            }
            if let (Some(surface_index), Some(surface)) = (self.surface_index, self.surface) {
                self.pending_click = Some(SelectionClick {
                    surface_index,
                    surface,
                    position,
                });
            }
            return self.clear_active();
        }
        if !self.moved && self.mode != SelectionMode::Character {
            self.copy_at = Some(now + MULTI_CLICK_COPY_DELAY);
        } else {
            self.copy_after_render = true;
        }
        self.highlight_mode = HighlightMode::Selection;
        true
    }

    pub(super) fn clear(&mut self) -> bool {
        self.completed_click = None;
        self.clear_active()
    }

    fn clear_active(&mut self) -> bool {
        let changed = self.anchor.take().is_some() || self.head.take().is_some();
        self.surface = None;
        self.surface_index = None;
        self.dragging = false;
        self.moved = false;
        self.mode = SelectionMode::Character;
        self.click_count = 0;
        self.copy_after_render = false;
        self.pending_copy = None;
        self.highlight_mode = HighlightMode::Selection;
        self.copy_at = None;
        self.clear_at = None;
        self.auto_scroll = None;
        self.scroll_snapshot = None;
        self.copied_before.clear();
        self.copied_after.clear();
        self.anchor_was_formula = false;
        changed
    }

    pub(super) const fn is_active(&self) -> bool {
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
        self.formula_cells.clear();
        for area in &self.selectable_areas[..self.selectable_area_count] {
            let left = area.x.max(buffer.area.x);
            let right = area.right().min(buffer.area.right());
            let top = area.y.max(buffer.area.y);
            let bottom = area.bottom().min(buffer.area.bottom());
            for y in top..bottom {
                for x in left..right {
                    if is_formula_placeholder(buffer[(x, y)].symbol()) {
                        self.formula_cells.push(Position::new(x, y));
                    }
                }
            }
        }
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
            let visible = selected_text(buffer, surface, start, end);
            let text = joined_selection(&self.copied_before, &visible, &self.copied_after);
            if !text.is_empty() {
                self.pending_copy = Some(text);
            }
            self.copy_after_render = false;
        }
        highlight(buffer, surface, start, end, self.highlight_mode);
        self.scroll_snapshot =
            (self.dragging && self.auto_scroll.is_some()).then(|| buffer.clone());
    }

    pub(super) const fn take_pending_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    pub(super) const fn take_pending_click(&mut self) -> Option<SelectionClick> {
        self.pending_click.take()
    }

    pub(super) const fn surface_index(&self) -> Option<usize> {
        self.surface_index
    }

    pub(super) const fn selectable_area_count(&self) -> usize {
        self.selectable_area_count
    }

    pub(super) fn scroll_request(&self) -> Option<SelectionScrollRequest> {
        Some(SelectionScrollRequest {
            surface_index: self.surface_index?,
            direction: self.auto_scroll?,
        })
    }

    pub(super) fn scrolled(&mut self, direction: SelectionScrollDirection, rows: usize) -> bool {
        if rows == 0 {
            self.auto_scroll = None;
            self.scroll_snapshot = None;
            return false;
        }
        let Some(surface) = self.surface else {
            return false;
        };
        let Some(buffer) = self.scroll_snapshot.take() else {
            return false;
        };
        let Some(anchor) = self.anchor else {
            return false;
        };
        let Some(head) = self.head else {
            return false;
        };
        let (start, end) = resolve_selection(&buffer, surface, anchor, head, self.mode);
        let rows = u16::try_from(rows).unwrap_or(u16::MAX).min(surface.height);
        match direction {
            SelectionScrollDirection::Newer => {
                let last_departing = surface
                    .y
                    .saturating_add(rows)
                    .saturating_sub(1)
                    .min(surface.bottom().saturating_sub(1));
                if selected_rows_intersect(start, end, surface.y, last_departing) {
                    append_selection(
                        &mut self.copied_before,
                        selected_text_in_rows(
                            &buffer,
                            surface,
                            start,
                            end,
                            surface.y,
                            last_departing,
                        ),
                        false,
                    );
                }
                self.anchor = Some(Position::new(
                    anchor.x,
                    anchor.y.saturating_sub(rows).max(surface.y),
                ));
            }
            SelectionScrollDirection::Older => {
                let first_departing = surface.bottom().saturating_sub(rows);
                if selected_rows_intersect(
                    start,
                    end,
                    first_departing,
                    surface.bottom().saturating_sub(1),
                ) {
                    append_selection(
                        &mut self.copied_after,
                        selected_text_in_rows(
                            &buffer,
                            surface,
                            start,
                            end,
                            first_departing,
                            surface.bottom().saturating_sub(1),
                        ),
                        true,
                    );
                }
                self.anchor = Some(Position::new(
                    anchor.x,
                    anchor
                        .y
                        .saturating_add(rows)
                        .min(surface.bottom().saturating_sub(1)),
                ));
            }
        }
        true
    }

    pub(super) fn copy_finished(&mut self, copied: bool) -> bool {
        self.copy_finished_at(copied, Instant::now())
    }

    fn copy_finished_at(&mut self, copied: bool, now: Instant) -> bool {
        if !copied || !self.is_active() {
            return false;
        }
        self.highlight_mode = HighlightMode::Copy;
        self.clear_at = Some(now + COPY_HIGHLIGHT_DURATION);
        true
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.copy_at.is_some_and(|copy_at| now >= copy_at) {
            self.copy_at = None;
            self.copy_after_render = true;
            changed = true;
        }
        if self.clear_at.is_none_or(|clear_at| now < clear_at) {
            return changed;
        }
        self.clear_active()
    }

    pub(super) const fn needs_tick(&self) -> bool {
        self.copy_at.is_some()
            || self.clear_at.is_some()
            || (self.dragging && self.auto_scroll.is_some())
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

    fn auto_scroll_direction(&self, position: Position) -> Option<SelectionScrollDirection> {
        let surface = self.surface?;
        if position.y <= surface.y {
            Some(SelectionScrollDirection::Older)
        } else if position.y >= surface.bottom().saturating_sub(1) {
            Some(SelectionScrollDirection::Newer)
        } else {
            None
        }
    }
}

const fn positions_are_near(left: Position, right: Position) -> bool {
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
    selected_text_in_rows(buffer, surface, start, end, start.y, end.y)
}

fn selected_text_in_rows(
    buffer: &Buffer,
    surface: Rect,
    start: Position,
    end: Position,
    first_row: u16,
    last_row: u16,
) -> String {
    let mut output = String::new();
    let last_buffer_x = buffer.area.right().saturating_sub(1);
    let first_y = start.y.max(first_row).max(buffer.area.y);
    let last_y = end
        .y
        .min(last_row)
        .min(buffer.area.bottom().saturating_sub(1));
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

fn selected_rows_intersect(start: Position, end: Position, first_row: u16, last_row: u16) -> bool {
    start.y.max(first_row) <= end.y.min(last_row)
}

fn append_selection(target: &mut Vec<String>, chunk: String, prepend: bool) {
    if prepend {
        target.insert(0, chunk);
    } else {
        target.push(chunk);
    }
}

fn joined_selection(before: &[String], visible: &str, after: &[String]) -> String {
    before
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(visible))
        .chain(after.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn highlight(
    buffer: &mut Buffer,
    surface: Rect,
    start: Position,
    end: Position,
    mode: HighlightMode,
) {
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
            let cell = &mut buffer[(x, y)];
            match mode {
                HighlightMode::Selection => {
                    cell.set_bg(Color::Indexed(8));
                }
                HighlightMode::Copy => {
                    cell.set_fg(Color::Yellow)
                        .set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
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

const fn row_bounds(start: Position, end: Position, y: u16, last_x: u16) -> (u16, u16) {
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

    use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

    use super::{ScreenSelection, SelectionScrollDirection};

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
            ratatui::style::Color::Indexed(8)
        );
        assert_ne!(
            buffer.cell((0, 0)).unwrap().bg,
            ratatui::style::Color::Indexed(8)
        );
        assert_ne!(
            buffer.cell((13, 1)).unwrap().bg,
            ratatui::style::Color::Indexed(8)
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
            ratatui::style::Color::Indexed(8)
        );
        assert_ne!(
            buffer.cell((5, 1)).unwrap().bg,
            ratatui::style::Color::Indexed(8)
        );
    }

    #[test]
    fn successful_copy_flashes_then_clears_while_failure_keeps_selection() {
        let area = Rect::new(0, 0, 12, 1);
        let mut buffer = Buffer::with_lines(["styled text"]);
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);
        assert!(selection.begin((0, 0).into()));
        assert!(selection.finish((5, 0).into()));
        selection.render(&mut buffer, &[area]);
        assert_eq!(selection.take_pending_copy().as_deref(), Some("styled"));

        let copied_at = Instant::now();
        assert!(!selection.copy_finished_at(false, copied_at));
        assert!(selection.is_active());
        assert!(!selection.needs_tick());

        assert!(selection.copy_finished_at(true, copied_at));
        assert!(selection.needs_tick());
        let mut feedback = Buffer::with_lines(["styled text"]);
        selection.render(&mut feedback, &[area]);
        let cell = feedback.cell((0, 0)).unwrap();
        assert_eq!(cell.fg, ratatui::style::Color::Yellow);
        assert!(cell.modifier.contains(Modifier::REVERSED));

        assert!(!selection.advance(copied_at + Duration::from_millis(299)));
        assert!(selection.is_active());
        assert!(selection.advance(copied_at + Duration::from_millis(300)));
        assert!(!selection.is_active());
        assert!(!selection.needs_tick());
    }

    #[test]
    fn edge_drag_preserves_rows_that_scroll_out_of_view() {
        let area = Rect::new(0, 0, 8, 3);
        let mut selection = ScreenSelection::default();
        let mut first = Buffer::with_lines(["older", "anchor", "middle"]);
        selection.render(&mut first, &[area]);

        assert!(selection.begin((0, 1).into()));
        assert!(selection.drag((5, 2).into()));
        selection.render(&mut first, &[area]);
        assert_eq!(
            selection.scroll_request().unwrap().direction,
            SelectionScrollDirection::Newer
        );
        assert!(selection.scrolled(SelectionScrollDirection::Newer, 1));

        let mut second = Buffer::with_lines(["anchor", "middle", "newer"]);
        selection.render(&mut second, &[area]);
        assert!(selection.scrolled(SelectionScrollDirection::Newer, 1));

        let mut third = Buffer::with_lines(["middle", "newer", "newest"]);
        selection.render(&mut third, &[area]);
        assert!(selection.finish((5, 2).into()));
        selection.render(&mut third, &[area]);
        assert_eq!(
            selection.take_pending_copy().as_deref(),
            Some("anchor\nmiddle\nnewer\nnewest")
        );
    }

    #[test]
    fn upward_edge_drag_prepends_older_rows_before_the_anchor() {
        let area = Rect::new(0, 0, 8, 3);
        let mut selection = ScreenSelection::default();
        let mut first = Buffer::with_lines(["middle", "anchor", "newer"]);
        selection.render(&mut first, &[area]);

        assert!(selection.begin((5, 1).into()));
        assert!(selection.drag((0, 0).into()));
        selection.render(&mut first, &[area]);
        assert_eq!(
            selection.scroll_request().unwrap().direction,
            SelectionScrollDirection::Older
        );
        assert!(selection.scrolled(SelectionScrollDirection::Older, 1));

        let mut second = Buffer::with_lines(["older", "middle", "anchor"]);
        selection.render(&mut second, &[area]);
        assert!(selection.scrolled(SelectionScrollDirection::Older, 1));

        let mut third = Buffer::with_lines(["oldest", "older", "middle"]);
        selection.render(&mut third, &[area]);
        assert!(selection.finish((0, 0).into()));
        selection.render(&mut third, &[area]);
        assert_eq!(
            selection.take_pending_copy().as_deref(),
            Some("oldest\nolder\nmiddle\nanchor")
        );
    }

    #[test]
    fn plain_click_reports_its_surface_for_cursor_placement() {
        let area = Rect::new(2, 4, 12, 2);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 10));
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);

        assert!(selection.begin((7, 5).into()));
        assert!(selection.finish((7, 5).into()));
        let click = selection.take_pending_click().unwrap();
        assert_eq!(click.surface_index, 0);
        assert_eq!(click.surface, area);
        assert_eq!(click.position, (7, 5).into());
    }

    #[test]
    fn clicking_a_formula_placeholder_pins_selection_for_the_source_redraw() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buffer = Buffer::empty(area);
        buffer[(2, 1)].set_symbol("\u{10eeee}\u{0305}\u{0305}");
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);

        assert!(selection.begin((2, 1).into()));
        assert!(selection.finish((2, 1).into()));

        assert!(selection.is_active());
        assert!(selection.take_pending_click().is_none());
        assert!(selection.take_pending_copy().is_none());
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
            ratatui::style::Color::Indexed(8)
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
    fn triple_click_supersedes_the_pending_double_click_copy() {
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
        assert!(selection.take_pending_copy().is_none());

        assert!(selection.begin_at(position, start + Duration::from_millis(200)));
        assert!(selection.finish_at(position, start + Duration::from_millis(210)));
        selection.render(&mut buffer, &[area]);
        assert!(selection.take_pending_copy().is_none());
        assert!(selection.advance(start + Duration::from_millis(410)));
        selection.render(&mut buffer, &[area]);
        assert_eq!(
            selection.take_pending_copy().as_deref(),
            Some("let snake_case = value")
        );
    }

    #[test]
    fn double_click_copies_the_word_after_the_triple_click_window() {
        let area = Rect::new(0, 0, 25, 1);
        let mut buffer = Buffer::with_lines(["let snake_case = value"]);
        let mut selection = ScreenSelection::default();
        selection.render(&mut buffer, &[area]);
        let start = Instant::now();
        let position = (6, 0).into();

        assert!(selection.begin_at(position, start));
        assert!(selection.finish_at(position, start + Duration::from_millis(10)));
        assert!(selection.begin_at(position, start + Duration::from_millis(100)));
        assert!(selection.finish_at(position, start + Duration::from_millis(110)));
        assert!(!selection.advance(start + Duration::from_millis(309)));
        selection.render(&mut buffer, &[area]);
        assert!(selection.take_pending_copy().is_none());

        assert!(selection.advance(start + Duration::from_millis(310)));
        selection.render(&mut buffer, &[area]);
        assert_eq!(selection.take_pending_copy().as_deref(), Some("snake_case"));
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
