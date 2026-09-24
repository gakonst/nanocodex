// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Pending-message stack shown while a turn is active.

use super::{
    node::{Component, ComponentUpdate, RenderRequest},
    waved_text::WavedText,
};
use crate::tui::{format::sanitize_terminal_text_inline, prompt::Submission, theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders},
};
use std::{borrow::Cow, time::Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const STEERING_TEXT: &str = "steering";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct QueueId(u64);

impl QueueId {
    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum QueueEffect {
    Blur,
    Discard { id: QueueId },
    Edit { id: QueueId, prompt: Submission },
    Steer { id: QueueId, prompt: Submission },
}

pub(super) enum QueueEvent {
    Terminal(Event),
    AnimationFrame(Instant),
}

struct QueueItem {
    id: QueueId,
    prompt: Submission,
    state: QueueItemState,
    steer_lane: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum QueueItemState {
    Queued,
    Editing,
    EditingUnconfirmed,
    SubmittingSteer,
    UnconfirmedSteer,
}

pub(super) struct MessageQueue {
    items: Vec<QueueItem>,
    selected: usize,
    first_visible: usize,
    focused: bool,
    next_id: u64,
    steering_label: WavedText,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            first_visible: 0,
            focused: false,
            next_id: 0,
            steering_label: WavedText::new(STEERING_TEXT, Color::Rgb(220, 220, 220)),
        }
    }
}

impl MessageQueue {
    pub(super) fn push(&mut self, prompt: impl Into<Submission>) {
        self.items.push(QueueItem {
            id: QueueId(self.next_id),
            prompt: prompt.into(),
            state: QueueItemState::Queued,
            steer_lane: false,
        });
        self.next_id = self.next_id.saturating_add(1);
        self.selected = self.items.len() - 1;
    }

    pub(super) fn begin_steer(&mut self, prompt: Submission) -> (QueueId, Submission) {
        let id = QueueId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.items.insert(
            self.steer_lane_len(),
            QueueItem {
                id,
                prompt: prompt.clone(),
                state: QueueItemState::SubmittingSteer,
                steer_lane: true,
            },
        );
        self.selected = self.steer_lane_len().saturating_sub(1);
        self.sync_steering_wave();
        (id, prompt)
    }

    /// Queue order can change; IDs preserve submission order across both lanes.
    pub(super) fn latest(&self) -> Option<(QueueId, bool)> {
        self.items
            .iter()
            .max_by_key(|item| item.id)
            .map(|item| (item.id, item.state == QueueItemState::Queued))
    }

    pub(super) fn prompt(&self, id: QueueId) -> Option<Submission> {
        self.items
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.prompt.clone())
    }

    pub(super) fn withdraw(&mut self, id: QueueId) -> Option<Submission> {
        let prompt = self.remove_id(id);
        self.sync_steering_wave();
        prompt
    }

    pub(super) fn is_unconfirmed_steer(&self, id: QueueId) -> bool {
        self.items.iter().any(|item| {
            item.id == id
                && matches!(
                    item.state,
                    QueueItemState::UnconfirmedSteer | QueueItemState::EditingUnconfirmed
                )
        })
    }

    pub(super) fn finish_edit(&mut self, id: QueueId, prompt: impl Into<Submission>) -> bool {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return false;
        };
        let prompt = prompt.into();
        if prompt.display_text().trim().is_empty() {
            self.items.remove(index);
            self.repair_selection();
            return true;
        }
        self.items[index].prompt = prompt;
        self.items[index].state = QueueItemState::Queued;
        self.selected = index;
        true
    }

    pub(super) fn cancel_edit(&mut self, id: QueueId) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return false;
        };
        if !matches!(
            item.state,
            QueueItemState::Editing | QueueItemState::EditingUnconfirmed
        ) {
            return false;
        }
        item.state = if item.state == QueueItemState::EditingUnconfirmed {
            QueueItemState::UnconfirmedSteer
        } else {
            QueueItemState::Queued
        };
        true
    }

    pub(super) fn drain_ready(&mut self) -> Vec<Submission> {
        let mut drained = Vec::new();
        let mut blocked = false;
        for item in std::mem::take(&mut self.items) {
            if item.state == QueueItemState::Queued && !blocked {
                drained.push(item.prompt);
            } else {
                // Unknown delivery stays visible without replaying it or preventing
                // explicitly queued, known-unsent instructions from running later.
                blocked |= item.state != QueueItemState::UnconfirmedSteer;
                self.items.push(item);
            }
        }
        self.repair_selection();
        drained
    }

    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    pub(super) fn focused(&self) -> bool {
        self.focused
    }

    pub(super) fn has_pending_steer(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.state == QueueItemState::SubmittingSteer)
    }

    pub(super) fn connection_lost(&mut self) {
        for item in &mut self.items {
            if item.state == QueueItemState::SubmittingSteer {
                item.state = QueueItemState::UnconfirmedSteer;
            }
        }
        self.sync_steering_wave();
    }

    pub(super) fn steer_admitted(&mut self, id: QueueId) -> Option<(QueueId, Submission)> {
        // This request's HTTP acknowledgement or correlated durable receipt confirms
        // admission. Generic run.steered telemetry cannot identify the input.
        let text = self.remove_id(id).map(|prompt| (id, prompt));
        self.sync_steering_wave();
        text
    }

    pub(super) fn steer_unconfirmed(&mut self, id: QueueId) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.state = QueueItemState::UnconfirmedSteer;
        }
        self.sync_steering_wave();
    }

    pub(super) fn steer_failed(&mut self, id: QueueId) {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return;
        };
        // Keep the instruction's place even if acknowledgements fail out of order.
        item.state = QueueItemState::Queued;
        self.sync_steering_wave();
    }

    pub(super) fn set_focused(&mut self, focused: bool) {
        self.focused = focused && !self.items.is_empty();
    }

    pub(super) fn focus_row(&mut self, row: u16, area: Rect) -> bool {
        if !area.contains(ratatui::layout::Position::new(area.x, row)) {
            return false;
        }
        let offset = row.saturating_sub(area.y + 1);
        let index = self
            .first_visible
            .saturating_add(usize::from(offset / 2))
            .min(self.items.len().saturating_sub(1));
        self.selected = index;
        self.focused = !self.items.is_empty();
        true
    }

    pub(super) fn desired_height(&self) -> u16 {
        if self.items.is_empty() {
            0
        } else {
            u16::try_from(self.items.len().saturating_mul(2) + 1).unwrap_or(u16::MAX)
        }
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.steering_label.animation_deadline()
    }

    fn remove_selected(&mut self) -> Option<(usize, QueueItem)> {
        if self.items.is_empty() {
            return None;
        }
        let index = self.selected;
        if !matches!(
            self.items[index].state,
            QueueItemState::Queued | QueueItemState::UnconfirmedSteer
        ) {
            return None;
        }
        let item = self.items.remove(index);
        self.repair_selection();
        Some((index, item))
    }

    fn steer_lane_len(&self) -> usize {
        // Recovery can return every pending steer to queued state. Keep their
        // ordering even then, while new steering still precedes regular follow-ups.
        self.items
            .iter()
            .rposition(|item| item.steer_lane)
            .map_or(0, |index| index + 1)
    }

    fn remove_id(&mut self, id: QueueId) -> Option<Submission> {
        let index = self.items.iter().position(|item| item.id == id)?;
        let item = self.items.remove(index);
        self.repair_selection();
        Some(item.prompt)
    }

    fn sync_steering_wave(&mut self) {
        let steering = self.has_pending_steer();
        self.steering_label.set_active(steering, Instant::now());
    }

    fn repair_selection(&mut self) {
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        if self.items.is_empty() {
            self.focused = false;
        }
    }

    fn move_selection(&mut self, down: bool) -> bool {
        let next = if down {
            self.selected.saturating_add(1).min(self.items.len() - 1)
        } else {
            self.selected.saturating_sub(1)
        };
        if next == self.selected {
            return false;
        }
        self.selected = next;
        true
    }

    fn reorder(&mut self, down: bool) -> bool {
        let previous = self.selected;
        let target = if down {
            previous.saturating_add(1).min(self.items.len() - 1)
        } else {
            previous.saturating_sub(1)
        };
        if target == previous
            || self.items[previous].state != QueueItemState::Queued
            || self.items[target].state != QueueItemState::Queued
        {
            return false;
        }
        self.selected = target;
        self.items.swap(previous, self.selected);
        true
    }

    fn update_terminal(&mut self, event: Event) -> ComponentUpdate<QueueEffect> {
        let Event::Key(key) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        let changed = match key.code {
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.reorder(false),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => self.reorder(true),
            KeyCode::Up => self.move_selection(false),
            KeyCode::Down => self.move_selection(true),
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                let Some((_, item)) = self.remove_selected() else {
                    return ComponentUpdate::none();
                };
                return ComponentUpdate {
                    effects: if item.state == QueueItemState::UnconfirmedSteer {
                        vec![QueueEffect::Discard { id: item.id }]
                    } else {
                        Vec::new()
                    },
                    render: RenderRequest::Immediate,
                };
            }
            KeyCode::Char('e') => {
                let Some(item) = self.items.get_mut(self.selected) else {
                    return ComponentUpdate::none();
                };
                if !matches!(
                    item.state,
                    QueueItemState::Queued | QueueItemState::UnconfirmedSteer
                ) {
                    return ComponentUpdate::none();
                }
                item.state = if item.state == QueueItemState::UnconfirmedSteer {
                    QueueItemState::EditingUnconfirmed
                } else {
                    QueueItemState::Editing
                };
                return ComponentUpdate {
                    effects: vec![QueueEffect::Edit {
                        id: item.id,
                        prompt: item.prompt.clone(),
                    }],
                    render: RenderRequest::Immediate,
                };
            }
            KeyCode::Enter => {
                let Some(item) = self.items.get(self.selected) else {
                    return ComponentUpdate::none();
                };
                if item.state != QueueItemState::Queued {
                    return ComponentUpdate::none();
                }

                let mut item = self.items.remove(self.selected);
                item.state = QueueItemState::SubmittingSteer;
                item.steer_lane = true;
                let id = item.id;
                let prompt = item.prompt.clone();
                let index = self.steer_lane_len();
                self.items.insert(index, item);
                self.selected = index;
                self.sync_steering_wave();
                return ComponentUpdate {
                    effects: vec![QueueEffect::Steer { id, prompt }],
                    render: RenderRequest::Immediate,
                };
            }
            KeyCode::Esc => {
                self.set_focused(false);
                return ComponentUpdate {
                    effects: vec![QueueEffect::Blur],
                    render: RenderRequest::Immediate,
                };
            }
            _ => false,
        };
        ComponentUpdate::render(if changed {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn focused_title(&self, width: u16) -> Option<String> {
        let selected = self.items.get(self.selected)?;
        let variants: &[&[&str]] = if selected.state == QueueItemState::Queued {
            &[
                &[
                    "↑↓ select",
                    "⇧↑↓ reorder",
                    "e edit",
                    "enter steer",
                    "d delete",
                    "alt+u undo last",
                    "esc back",
                ],
                &["↑↓ select", "e edit", "enter steer", "d delete", "esc back"],
                &["↑↓ select", "enter steer", "esc back"],
                &["↑↓ select", "esc back"],
            ]
        } else if selected.state == QueueItemState::UnconfirmedSteer {
            &[
                &["delivery unknown", "e edit/retry", "d dismiss", "esc back"],
                &["e edit/retry", "d dismiss", "esc back"],
            ]
        } else if matches!(
            selected.state,
            QueueItemState::Editing | QueueItemState::EditingUnconfirmed
        ) {
            &[&["enter save", "esc cancel"], &["esc cancel"]]
        } else {
            &[&["↑↓ navigate", "esc back"]]
        };
        variants.iter().find_map(|actions| {
            let title = format!(" {} ", actions.join(" · "));
            (UnicodeWidthStr::width(title.as_str()) <= usize::from(width)).then_some(title)
        })
    }
}

impl Component for MessageQueue {
    type Event = QueueEvent;
    type Effect = QueueEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            QueueEvent::Terminal(event) => self.update_terminal(event),
            QueueEvent::AnimationFrame(now) => {
                let changed = self.steering_label.advance(now);
                ComponentUpdate::render(if changed {
                    RenderRequest::Streaming
                } else {
                    RenderRequest::None
                })
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() || self.items.is_empty() {
            return;
        }

        let visible_rows = usize::from(area.height.saturating_sub(1) / 2);
        if visible_rows > 0 {
            self.first_visible = self
                .first_visible
                .min(self.selected)
                .max(self.selected.saturating_add(1).saturating_sub(visible_rows))
                .min(self.items.len().saturating_sub(visible_rows));
        }
        let position = if self.items.len() > visible_rows && visible_rows > 0 {
            format!(
                " queue {}–{} of {} · ",
                self.first_visible + 1,
                (self.first_visible + visible_rows).min(self.items.len()),
                self.items.len()
            )
        } else {
            " queue · ".to_owned()
        };
        let border = theme.border();
        let title = if self.steering_label.is_active() {
            let mut spans = Vec::with_capacity(STEERING_TEXT.len() + 2);
            spans.push(Span::styled(position, Style::default().fg(border)));
            spans.extend(self.steering_label.spans());
            spans.push(Span::styled(" ", Style::default().fg(border)));
            Line::from(spans)
        } else if self.items.iter().all(|item| {
            matches!(
                item.state,
                QueueItemState::UnconfirmedSteer | QueueItemState::EditingUnconfirmed
            )
        }) {
            Line::styled(
                format!("{position}delivery unknown "),
                Style::default().fg(border),
            )
        } else {
            Line::styled(
                format!("{position}enter steer latest "),
                Style::default().fg(border),
            )
        };
        let mut block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(title);
        if self.focused
            && let Some(title) = self.focused_title(area.width.saturating_sub(2))
        {
            block = block
                .title_bottom(Line::styled(title, Style::default().fg(border)))
                .title_alignment(Alignment::Center);
        }
        frame.render_widget(block, area);

        let content_width = usize::from(area.width.saturating_sub(4));
        for (visible_index, (index, item)) in self
            .items
            .iter()
            .enumerate()
            .skip(self.first_visible)
            .take(visible_rows)
            .enumerate()
        {
            let offset = u16::try_from(visible_index.saturating_mul(2)).unwrap_or(u16::MAX);
            let row_y = area.y + 1 + offset;
            if visible_index > 0 {
                let y = row_y - 1;
                frame
                    .buffer_mut()
                    .set_string(area.x, y, "├", Style::default().fg(border));
                for x in area.x + 1..area.right().saturating_sub(1) {
                    frame
                        .buffer_mut()
                        .set_string(x, y, "─", Style::default().fg(border));
                }
                frame.buffer_mut().set_string(
                    area.right().saturating_sub(1),
                    y,
                    "┤",
                    Style::default().fg(border),
                );
            }

            let mut style = if self.focused && index == self.selected {
                Style::default()
                    .fg(theme.accent())
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default().fg(theme.text())
            };
            if item.state != QueueItemState::Queued {
                style = style.fg(theme.muted()).add_modifier(Modifier::ITALIC);
            }
            let text = if item.state == QueueItemState::UnconfirmedSteer {
                Cow::Owned(format!("[delivery unknown] {}", item.prompt.display_text()))
            } else {
                Cow::Borrowed(item.prompt.display_text())
            };
            frame.buffer_mut().set_stringn(
                area.x + 2,
                row_y,
                truncate(&text, content_width),
                content_width,
                style,
            );
        }
    }
}

fn truncate(text: &str, width: usize) -> Cow<'_, str> {
    let text = sanitize_terminal_text_inline(text);
    if UnicodeWidthStr::width(text.as_ref()) <= width {
        return text;
    }
    if width == 0 {
        return Cow::Borrowed("");
    }

    let mut result = String::new();
    let available = width.saturating_sub(1);
    for grapheme in text.graphemes(true) {
        if UnicodeWidthStr::width(result.as_str()) + UnicodeWidthStr::width(grapheme) > available {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use super::{Component, MessageQueue, QueueEffect, QueueEvent, QueueId, Submission};
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> QueueEvent {
        QueueEvent::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }

    fn rendered_rows(queue: &mut MessageQueue, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| queue.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    #[test]
    fn overflowing_queue_keeps_selection_visible_and_maps_clicks_to_visible_rows() {
        let mut queue = MessageQueue::default();
        for index in 0..12 {
            queue.push(format!("entry {index:02}"));
        }
        let rendered = rendered_rows(&mut queue, 60, 7).join("\n");
        assert!(rendered.contains("entry 11"), "{rendered}");
        assert!(rendered.contains("10–12 of 12"), "{rendered}");
        assert!(!rendered.contains("entry 00"));
        assert!(queue.focus_row(1, ratatui::layout::Rect::new(0, 0, 60, 7)));
        assert_eq!(queue.selected, 9);
        queue.update(key(KeyCode::Up, KeyModifiers::NONE));
        let rendered = rendered_rows(&mut queue, 60, 7).join("\n");
        assert!(rendered.contains("entry 08"), "{rendered}");
        assert!(!rendered.contains("entry 11"));
        // Growing the terminal should reveal earlier items without losing selection.
        let rendered = rendered_rows(&mut queue, 60, 25).join("\n");
        assert!(rendered.contains("entry 00"));
        assert!(rendered.contains("entry 11"));
        assert_eq!(queue.selected, 8);
    }

    #[test]
    fn queue_accepts_any_number_of_items_and_reorders_the_selected_item() {
        let mut queue = MessageQueue::default();
        for index in 0..100 {
            queue.push(format!("item {index}"));
        }
        assert_eq!(queue.len(), 100);

        queue.update(key(KeyCode::Up, KeyModifiers::SHIFT));
        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            update.effects.as_slice(),
            [QueueEffect::Steer { prompt, .. }] if prompt.display_text() == "item 99"
        ));
    }

    #[test]
    fn steering_a_deeper_item_moves_it_to_the_top_of_the_stack() {
        let mut queue = MessageQueue::default();
        queue.push("first".to_owned());
        queue.push("priority".to_owned());
        queue.push("last".to_owned());
        queue.update(key(KeyCode::Up, KeyModifiers::NONE));

        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(30, 7)).unwrap();
        terminal
            .draw(|frame| queue.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(30)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();

        assert!(matches!(
            update.effects.as_slice(),
            [QueueEffect::Steer { prompt, .. }] if prompt.display_text() == "priority"
        ));
        assert!(rows[1].contains("priority"));
        assert!(rows[3].contains("first"));
        assert!(rows[5].contains("last"));
    }

    #[test]
    fn queued_controls_are_visible_without_changing_the_submission() {
        let mut queue = MessageQueue::default();
        let prompt = "one\ttwo\u{1b}three\nnext";
        queue.push(prompt.to_owned());

        let rows = rendered_rows(&mut queue, 40, 3);
        assert!(rows[1].contains("one    two�three next"));

        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            update.effects.as_slice(),
            [QueueEffect::Steer { prompt: submission, .. }]
                if submission.display_text() == prompt
        ));
    }

    #[test]
    fn failed_steers_keep_their_original_order_regardless_of_failure_order() {
        for order in [[0, 1, 2], [2, 0, 1], [1, 2, 0]] {
            let mut queue = MessageQueue::default();
            queue.push("regular followup".to_owned());
            let ids = ["first", "second", "third"]
                .map(|text| queue.begin_steer(text.to_owned().into()).0);
            for index in order {
                queue.steer_failed(ids[index]);
            }
            assert_eq!(
                queue
                    .drain_ready()
                    .iter()
                    .map(Submission::display_text)
                    .collect::<Vec<_>>(),
                ["first", "second", "third", "regular followup"]
            );
        }
    }

    #[test]
    fn recovered_steers_keep_order_when_new_steering_arrives() {
        let mut queue = MessageQueue::default();
        let (_, _) = queue.begin_steer("uncertain".to_owned().into());
        let (waiting, _) = queue.begin_steer("before disconnect".to_owned().into());
        queue.connection_lost();
        queue.steer_failed(waiting);
        queue.push("regular followup".to_owned());
        let (new, _) = queue.begin_steer("after reconnect".to_owned().into());
        queue.steer_failed(new);
        assert_eq!(
            queue
                .drain_ready()
                .iter()
                .map(Submission::display_text)
                .collect::<Vec<_>>(),
            ["before disconnect", "after reconnect", "regular followup"]
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn failed_steer_does_not_split_the_pending_steer_lane() {
        let mut queue = MessageQueue::default();
        queue.push("first steer".to_owned());
        let first = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let [QueueEffect::Steer { id: first_id, .. }] = first.effects.as_slice() else {
            panic!("enter should begin the first steer");
        };
        queue.push("second steer".to_owned());
        let second = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let [QueueEffect::Steer { id: second_id, .. }] = second.effects.as_slice() else {
            panic!("enter should begin the second steer");
        };

        queue.steer_failed(*first_id);
        queue.push("third steer".to_owned());
        let third = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let [QueueEffect::Steer { id: third_id, .. }] = third.effects.as_slice() else {
            panic!("enter should begin the third steer");
        };

        assert_eq!(
            queue
                .steer_admitted(*second_id)
                .map(|(_, prompt)| prompt.display_text().to_owned()),
            Some("second steer".to_owned())
        );
        assert_eq!(
            queue
                .steer_admitted(*third_id)
                .map(|(_, prompt)| prompt.display_text().to_owned()),
            Some("third steer".to_owned())
        );
        assert_eq!(
            queue
                .drain_ready()
                .into_iter()
                .map(|prompt| prompt.display_text().to_owned())
                .collect::<Vec<_>>(),
            ["first steer"]
        );
    }

    #[test]
    fn edit_blocks_the_item_before_emitting_the_effect() {
        let mut queue = MessageQueue::default();
        queue.push("edit me".to_owned());

        let update = queue.update(key(KeyCode::Char('e'), KeyModifiers::NONE));

        assert_eq!(queue.len(), 1);
        assert!(queue.drain_ready().is_empty());
        assert_eq!(
            update.effects,
            [QueueEffect::Edit {
                id: QueueId::new(0),
                prompt: "edit me".to_owned().into(),
            }]
        );
    }

    #[test]
    fn multimodal_queue_edit_preserves_images_when_cancelled() {
        let mut queue = MessageQueue::default();
        let prompt = Submission::multimodal(
            "see [Image #1]".to_owned(),
            [(4..14, "data:image/png;base64,a".to_owned())],
        );
        queue.push(prompt.clone());

        let update = queue.update(key(KeyCode::Char('e'), KeyModifiers::NONE));

        assert_eq!(
            update.effects,
            [QueueEffect::Edit {
                id: QueueId::new(0),
                prompt: prompt.clone(),
            }]
        );
        assert!(queue.drain_ready().is_empty());
        assert!(queue.cancel_edit(QueueId::new(0)));
        assert_eq!(queue.drain_ready(), [prompt]);
    }
}
