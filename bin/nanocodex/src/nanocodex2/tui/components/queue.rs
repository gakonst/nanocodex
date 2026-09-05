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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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
    Edit { id: QueueId, text: String },
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
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum QueueItemState {
    Queued,
    Editing,
    SubmittingSteer,
    AdmittedSteer,
    CancelledSteer,
}

pub(super) struct MessageQueue {
    items: Vec<QueueItem>,
    selected: usize,
    focused: bool,
    next_id: u64,
    applied_steers_waiting_for_ack: usize,
    steering_label: WavedText,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            focused: false,
            next_id: 0,
            applied_steers_waiting_for_ack: 0,
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
            },
        );
        self.selected = self.steer_lane_len().saturating_sub(1);
        self.sync_steering_wave();
        (id, prompt)
    }

    pub(super) fn finish_edit(&mut self, id: QueueId, text: String) -> bool {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return false;
        };
        if text.trim().is_empty() {
            self.items.remove(index);
            self.repair_selection();
            return true;
        }
        self.items[index].prompt = text.into();
        self.items[index].state = QueueItemState::Queued;
        self.selected = index;
        true
    }

    pub(super) fn cancel_edit(&mut self, id: QueueId) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return false;
        };
        if item.state != QueueItemState::Editing {
            return false;
        }
        item.state = QueueItemState::Queued;
        true
    }

    pub(super) fn drain_ready(&mut self) -> Vec<Submission> {
        let ready = self
            .items
            .iter()
            .take_while(|item| {
                matches!(
                    item.state,
                    QueueItemState::Queued | QueueItemState::CancelledSteer
                )
            })
            .count();
        let drained = self.items.drain(..ready).map(|item| item.prompt).collect();
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
        self.items.iter().any(|item| {
            matches!(
                item.state,
                QueueItemState::SubmittingSteer | QueueItemState::AdmittedSteer
            )
        })
    }

    pub(super) fn steer_admitted(&mut self, id: QueueId) -> Option<Submission> {
        let item = self.items.iter_mut().find(|item| item.id == id)?;
        if item.state == QueueItemState::CancelledSteer {
            return None;
        }
        item.state = QueueItemState::AdmittedSteer;
        if self.applied_steers_waiting_for_ack == 0 {
            return None;
        }

        self.applied_steers_waiting_for_ack -= 1;
        let text = self.remove_id(id);
        self.sync_steering_wave();
        text
    }

    pub(super) fn steer_applied(&mut self) -> Option<Submission> {
        if let Some(id) = self
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.state,
                    QueueItemState::AdmittedSteer | QueueItemState::CancelledSteer
                )
            })
            .map(|item| item.id)
        {
            let text = self.remove_id(id);
            self.sync_steering_wave();
            return text;
        }
        if self
            .items
            .iter()
            .any(|item| item.state == QueueItemState::SubmittingSteer)
        {
            self.applied_steers_waiting_for_ack =
                self.applied_steers_waiting_for_ack.saturating_add(1);
        }
        None
    }

    pub(super) fn steer_promoted(&mut self, id: QueueId) -> Option<Submission> {
        let text = self.remove_id(id);
        self.sync_steering_wave();
        text
    }

    pub(super) fn steer_failed(&mut self, id: QueueId) {
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return;
        };
        let selected = self.items.get(self.selected).map(|item| item.id);
        let mut item = self.items.remove(index);
        item.state = QueueItemState::Queued;
        let index = self.steer_lane_len();
        self.items.insert(index, item);
        if let Some(selected) = selected {
            self.selected = self
                .items
                .iter()
                .position(|item| item.id == selected)
                .unwrap_or(index);
        }
        self.sync_steering_wave();
    }

    pub(super) fn cancel_steers(&mut self) {
        self.items.sort_by_key(|item| {
            matches!(item.state, QueueItemState::Queued | QueueItemState::Editing)
        });
        for item in &mut self.items {
            if matches!(
                item.state,
                QueueItemState::SubmittingSteer | QueueItemState::AdmittedSteer
            ) {
                item.state = QueueItemState::CancelledSteer;
            }
        }
        self.selected = 0;
        self.applied_steers_waiting_for_ack = 0;
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
        let index = usize::from(offset / 2).min(self.items.len().saturating_sub(1));
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
        if self.items[index].state != QueueItemState::Queued {
            return None;
        }
        let item = self.items.remove(index);
        self.repair_selection();
        Some((index, item))
    }

    fn steer_lane_len(&self) -> usize {
        self.items
            .iter()
            .take_while(|item| {
                matches!(
                    item.state,
                    QueueItemState::SubmittingSteer
                        | QueueItemState::AdmittedSteer
                        | QueueItemState::CancelledSteer
                )
            })
            .count()
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
                self.remove_selected().is_some()
            }
            KeyCode::Char('e') => {
                let Some(item) = self.items.get_mut(self.selected) else {
                    return ComponentUpdate::none();
                };
                if item.state != QueueItemState::Queued {
                    return ComponentUpdate::none();
                }
                if item.prompt.has_images() {
                    return ComponentUpdate::none();
                }
                item.state = QueueItemState::Editing;
                return ComponentUpdate {
                    effects: vec![QueueEffect::Edit {
                        id: item.id,
                        text: item.prompt.display_text().to_owned(),
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
                    "esc back",
                ],
                &["↑↓ select", "e edit", "enter steer", "d delete", "esc back"],
                &["↑↓ select", "enter steer", "esc back"],
                &["↑↓ select", "esc back"],
            ]
        } else if selected.state == QueueItemState::Editing {
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

        let border = theme.border();
        let title = if self.steering_label.is_active() {
            let mut spans = Vec::with_capacity(STEERING_TEXT.len() + 2);
            spans.push(Span::styled(" queue · ", Style::default().fg(border)));
            spans.extend(self.steering_label.spans());
            spans.push(Span::styled(" ", Style::default().fg(border)));
            Line::from(spans)
        } else {
            Line::styled(" queue · enter steer latest ", Style::default().fg(border))
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
        for (index, item) in self.items.iter().enumerate() {
            let offset = u16::try_from(index.saturating_mul(2)).unwrap_or(u16::MAX);
            let row_y = area.y + 1 + offset;
            if index > 0 {
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

            if row_y >= area.bottom().saturating_sub(1) {
                break;
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
            frame.buffer_mut().set_stringn(
                area.x + 2,
                row_y,
                truncate(item.prompt.display_text(), content_width),
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
    use super::{
        Component, MessageQueue, QueueEffect, QueueEvent, QueueId, STEERING_TEXT, Submission,
        truncate,
    };
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

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
    fn direct_steer_enters_the_steer_lane_exactly_once() {
        let mut queue = MessageQueue::default();

        let (id, prompt) = queue.begin_steer("change direction".to_owned().into());

        assert_eq!(id, QueueId::new(0));
        assert_eq!(prompt.display_text(), "change direction");
        assert_eq!(queue.len(), 1);
        assert!(queue.has_pending_steer());
        assert!(queue.drain_ready().is_empty());
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
    fn queue_title_explains_how_to_steer_the_latest_message() {
        let mut queue = MessageQueue::default();
        queue.push("queued".to_owned());

        let rows = rendered_rows(&mut queue, 100, 3);

        assert!(rows[0].contains(" queue · enter steer latest "));
    }

    #[test]
    fn subsequent_steers_follow_pending_steers_in_submission_order() {
        let mut queue = MessageQueue::default();
        queue.push("regular".to_owned());
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

        assert!(queue.steer_admitted(*first_id).is_none());
        assert!(queue.steer_admitted(*second_id).is_none());
        assert_eq!(
            queue
                .steer_applied()
                .map(|prompt| prompt.display_text().to_owned()),
            Some("first steer".to_owned())
        );
        assert_eq!(
            queue
                .steer_applied()
                .map(|prompt| prompt.display_text().to_owned()),
            Some("second steer".to_owned())
        );
        assert_eq!(
            queue
                .drain_ready()
                .into_iter()
                .map(|prompt| prompt.display_text().to_owned())
                .collect::<Vec<_>>(),
            ["regular"]
        );
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
        assert!(queue.steer_admitted(*second_id).is_none());
        queue.push("third steer".to_owned());
        let third = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let [QueueEffect::Steer { id: third_id, .. }] = third.effects.as_slice() else {
            panic!("enter should begin the third steer");
        };
        assert!(queue.steer_admitted(*third_id).is_none());

        assert_eq!(
            queue
                .steer_applied()
                .map(|prompt| prompt.display_text().to_owned()),
            Some("second steer".to_owned())
        );
        assert_eq!(
            queue
                .steer_applied()
                .map(|prompt| prompt.display_text().to_owned()),
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
                text: "edit me".to_owned(),
            }]
        );
    }

    #[test]
    fn multimodal_items_cannot_enter_the_text_only_queue_editor() {
        let mut queue = MessageQueue::default();
        queue.push(Submission::multimodal(
            "see [Image #1]".to_owned(),
            [(4..14, "data:image/png;base64,a".to_owned())],
        ));

        let update = queue.update(key(KeyCode::Char('e'), KeyModifiers::NONE));

        assert!(update.effects.is_empty());
        let prompt = queue.drain_ready().pop().unwrap();
        assert!(prompt.has_images());
    }

    #[test]
    fn cancelling_turns_does_not_reorder_an_item_being_edited() {
        let mut queue = MessageQueue::default();
        queue.push("first".to_owned());
        queue.push("edit me".to_owned());
        queue.push("last".to_owned());
        queue.update(key(KeyCode::Up, KeyModifiers::NONE));

        queue.update(key(KeyCode::Char('e'), KeyModifiers::NONE));
        queue.cancel_steers();
        assert!(queue.finish_edit(QueueId::new(1), "edited".to_owned()));

        let prompts = queue
            .drain_ready()
            .into_iter()
            .map(|prompt| prompt.display_text().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(prompts, ["first", "edited", "last"]);
    }

    #[test]
    fn late_steer_admission_does_not_revive_a_cancelled_steer() {
        let mut queue = MessageQueue::default();
        queue.push("steer me".to_owned());
        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let [QueueEffect::Steer { id, .. }] = update.effects.as_slice() else {
            panic!("enter should begin a steer");
        };

        queue.cancel_steers();
        assert!(queue.steer_admitted(*id).is_none());

        assert!(!queue.has_pending_steer());
        assert_eq!(
            queue
                .drain_ready()
                .into_iter()
                .map(|prompt| prompt.display_text().to_owned())
                .collect::<Vec<_>>(),
            ["steer me"]
        );
    }

    #[test]
    fn cancelling_an_edit_restores_the_original_item() {
        let mut queue = MessageQueue::default();
        queue.push("keep me".to_owned());
        queue.update(key(KeyCode::Char('e'), KeyModifiers::NONE));

        assert!(queue.cancel_edit(QueueId::new(0)));

        let prompts = queue
            .drain_ready()
            .into_iter()
            .map(|prompt| prompt.display_text().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(prompts, ["keep me"]);
    }

    #[test]
    fn pending_steer_stays_visible_with_a_demand_driven_text_wave() {
        let mut queue = MessageQueue::default();
        queue.push("steer me".to_owned());
        queue.set_focused(true);
        let update = queue.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let QueueEffect::Steer { id, .. } = &update.effects[0] else {
            panic!("enter should begin steering");
        };
        let id = *id;
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();

        terminal
            .draw(|frame| queue.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let steering_width = u16::try_from(STEERING_TEXT.len()).unwrap();
        let steering_start = (0_u16..=32)
            .find(|start| {
                (0..steering_width)
                    .map(|offset| terminal.backend().buffer()[(*start + offset, 0)].symbol())
                    .collect::<String>()
                    == STEERING_TEXT
            })
            .expect("the steering label should be rendered");
        let initial_colors = (steering_start..steering_start + steering_width)
            .map(|x| terminal.backend().buffer()[(x, 0)].fg)
            .collect::<Vec<_>>();
        let rendered = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();

        assert_eq!(queue.len(), 1);
        assert!(queue.animation_deadline().is_some());
        assert!(rendered.contains("queue · steering"));
        assert!(initial_colors.iter().all(|color| matches!(
            color,
            Color::Rgb(red, green, blue) if red == green && green == blue
        )));
        assert!(
            initial_colors
                .windows(2)
                .any(|colors| colors[0] != colors[1])
        );

        let deadline = queue.animation_deadline().unwrap();
        let animation = queue.update(QueueEvent::AnimationFrame(deadline));
        assert_eq!(animation.render, super::RenderRequest::Streaming);
        terminal
            .draw(|frame| queue.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let advanced_colors = (steering_start..steering_start + steering_width)
            .map(|x| terminal.backend().buffer()[(x, 0)].fg)
            .collect::<Vec<_>>();
        assert_ne!(advanced_colors, initial_colors);

        assert!(queue.steer_admitted(id).is_none());
        assert_eq!(
            queue.steer_applied().as_ref().map(Submission::display_text),
            Some("steer me")
        );
        assert!(queue.animation_deadline().is_none());
    }

    #[test]
    fn focused_queued_item_discloses_available_actions() {
        let mut queue = MessageQueue::default();
        queue.push("queued".to_owned());
        queue.set_focused(true);

        let rows = rendered_rows(&mut queue, 100, 3);

        assert!(
            rows[2]
                .contains(" ↑↓ select · ⇧↑↓ reorder · e edit · enter steer · d delete · esc back ")
        );
    }

    #[test]
    fn focused_non_queued_item_only_discloses_navigation_and_back() {
        let mut queue = MessageQueue::default();
        queue.push("steering".to_owned());
        queue.set_focused(true);
        queue.update(key(KeyCode::Enter, KeyModifiers::NONE));

        let rows = rendered_rows(&mut queue, 40, 3);

        assert!(rows[2].contains(" ↑↓ navigate · esc back "));
        assert!(!rows[2].contains("reorder"));
        assert!(!rows[2].contains("edit"));
        assert!(!rows[2].contains("steer"));
        assert!(!rows[2].contains("delete"));
    }

    #[test]
    fn focused_title_only_renders_complete_actions_at_narrow_widths() {
        let mut queue = MessageQueue::default();
        queue.push("queued".to_owned());
        queue.set_focused(true);

        let rows = rendered_rows(&mut queue, 24, 3);

        assert!(rows[2].contains(" ↑↓ select · esc back "));
        assert_eq!(rows[2].chars().next(), Some('╰'));
        assert_eq!(rows[2].chars().last(), Some('╯'));
    }

    #[test]
    fn stack_uses_joined_cells_and_truncates_to_one_line() {
        let mut queue = MessageQueue::default();
        queue.push("first".to_owned());
        queue.push("a very long second line".to_owned());
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();

        terminal
            .draw(|frame| queue.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "╭");
        assert_eq!(buffer[(2, 0)].symbol(), "q");
        assert_eq!(buffer[(0, 2)].symbol(), "├");
        assert_eq!(buffer[(19, 2)].symbol(), "┤");
        assert_eq!(buffer[(19, 4)].symbol(), "╯");
        assert_eq!(truncate("hello\nworld", 8), "hello w…");
    }
}
