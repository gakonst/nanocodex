// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Camera-centered subagent hierarchy and read-only transcript inspector.

use super::{
    floating::Floating,
    node::Node,
    subagent_tree_layout::{
        LayoutNode, NODE_HEIGHT, NODE_WIDTH, NodePosition, TreeLayout, VERTICAL_GAP, WorldPoint,
    },
    transcript::{Transcript, TranscriptEvent},
};
use crate::{
    config::DEFAULT_MAX_SUBAGENTS,
    tui::{format::sanitize_terminal_text_inline, theme::Theme, transcript::TranscriptRecord},
};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use nanocodex::Model;
use nanocodex_subagents::{AgentDescriptor, AgentId, AgentStatus, AgentUpdate, MessageSender};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const TREE_KEYS: [(&str, &str); 7] = [
    ("←/→", "row"),
    ("↑", "parent"),
    ("↓", "child"),
    ("enter", "inspect"),
    ("-/+", "limit"),
    ("f", "filter"),
    ("esc", "close"),
];
const TRANSCRIPT_KEYS: [(&str, &str); 4] = [
    ("pgup/pgdn", "scroll"),
    ("ctrl+home/end", ""),
    ("ctrl+o", "expand all"),
    ("esc", "back"),
];
const FOCUSED_ENTRY_KEYS: [(&str, &str); 3] = [
    ("↑↓", "item"),
    ("enter", "toggle"),
    ("esc", "blur, then back"),
];
const CAMERA_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const CAMERA_MIN_DURATION: Duration = Duration::from_millis(120);
const CAMERA_MAX_DURATION: Duration = Duration::from_millis(240);
const INSPECTOR_HEIGHT: u16 = 6;

struct AgentNode {
    descriptor: AgentDescriptor,
    status: AgentStatus,
    transcript: Node<Transcript>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentFilter {
    Active,
    All,
}

impl AgentFilter {
    const fn includes(self, status: &AgentStatus) -> bool {
        match self {
            Self::Active => status.is_active(),
            Self::All => true,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::All => "all",
        }
    }

    const fn helper(self) -> &'static str {
        match self {
            Self::Active => "filter: active",
            Self::All => "filter: all",
        }
    }

    const fn toggled(self) -> Self {
        match self {
            Self::Active => Self::All,
            Self::All => Self::Active,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SubagentOverlay {
    Tree,
    Transcript(AgentId),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SubagentEffect {
    Dismiss,
    Inspect(AgentId),
    Back,
    OpenLink(String),
    SetMaxSubagents(usize),
}

struct CameraAnimation {
    from: WorldPoint,
    to: WorldPoint,
    started_at: Instant,
    duration: Duration,
    next_frame: Instant,
}

#[derive(Default)]
struct Camera {
    center: Option<WorldPoint>,
    animation: Option<CameraAnimation>,
}

pub(super) struct SubagentTree {
    nodes: Vec<AgentNode>,
    focused: Option<AgentId>,
    remembered_children: HashMap<AgentId, AgentId>,
    camera: Camera,
    filter: AgentFilter,
    effort: crate::config::ReasoningEffort,
    max_subagents: usize,
    workspace: std::path::PathBuf,
}

impl SubagentTree {
    pub(super) fn new(effort: crate::config::ReasoningEffort) -> Self {
        Self {
            nodes: Vec::new(),
            focused: None,
            remembered_children: HashMap::new(),
            camera: Camera::default(),
            filter: AgentFilter::Active,
            effort,
            max_subagents: DEFAULT_MAX_SUBAGENTS,
            workspace: std::env::current_dir().unwrap_or_default(),
        }
    }

    pub(super) fn set_workspace(&mut self, workspace: &std::path::Path) {
        self.workspace = workspace.to_path_buf();
        for node in &mut self.nodes {
            node.transcript.component_mut().set_workspace(workspace);
        }
    }

    pub(super) fn refresh_terminal_images(&mut self) {
        for node in &mut self.nodes {
            node.transcript.component_mut().refresh_terminal_images();
        }
    }

    pub(super) fn apply(&mut self, update: AgentUpdate) -> bool {
        match update {
            AgentUpdate::Added(descriptor) => {
                if let Some(node) = self.node_mut(descriptor.id) {
                    node.descriptor = descriptor;
                } else {
                    let id = descriptor.id;
                    let mut transcript = Transcript::with_effort(self.effort);
                    transcript.set_workspace(&self.workspace);
                    self.nodes.push(AgentNode {
                        descriptor,
                        status: AgentStatus::Running,
                        transcript: Node::new(transcript),
                    });
                    self.focused.get_or_insert(id);
                }
                true
            }
            AgentUpdate::Event { id, event } => {
                let Some(node) = self.node_mut(id) else {
                    return false;
                };
                let record = TranscriptRecord::from_agent(event.seq, unix_time_ms(), event);
                node.transcript
                    .update(TranscriptEvent::Record(Arc::new(record)));
                true
            }
            AgentUpdate::Status { id, status } => {
                let Some(node) = self.node_mut(id) else {
                    return false;
                };
                if node.status == status {
                    return false;
                }
                node.status = status;
                true
            }
            AgentUpdate::Message(update) => {
                let mut projected = false;
                let mut previous = None;
                for participant in update.thread.participants {
                    let MessageSender::Agent { agent_id } = participant else {
                        continue;
                    };
                    if previous == Some(agent_id) {
                        continue;
                    }
                    previous = Some(agent_id);
                    let Some(node) = self.node_mut(agent_id) else {
                        continue;
                    };
                    node.transcript.update(TranscriptEvent::DirectedMessage {
                        perspective: participant,
                        update: update.clone(),
                    });
                    projected = true;
                }
                projected
            }
        }
    }

    pub(super) fn active_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|node| node.status.is_active())
            .count()
    }

    pub(super) fn set_effort(&mut self, effort: crate::config::ReasoningEffort) {
        self.effort = effort;
        for node in &mut self.nodes {
            node.transcript.component_mut().set_effort(effort);
        }
    }

    pub(super) fn set_max_subagents(&mut self, limit: usize) {
        self.max_subagents = limit;
    }

    fn concurrency_label(&self) -> String {
        if self.max_subagents == usize::MAX {
            "unlimited".to_owned()
        } else {
            self.max_subagents.to_string()
        }
    }

    pub(super) const fn max_subagents(&self) -> usize {
        self.max_subagents
    }

    pub(super) fn contains(&self, id: AgentId) -> bool {
        self.nodes.iter().any(|node| node.descriptor.id == id)
    }

    pub(super) fn is_direct_child(&self, id: AgentId) -> bool {
        self.node(id)
            .is_some_and(|node| node.descriptor.parent.is_none())
    }

    pub(super) fn animation_deadline(&self) -> Option<Instant> {
        self.nodes
            .iter()
            .filter_map(|node| node.transcript.component().animation_deadline())
            .chain(
                self.camera
                    .animation
                    .as_ref()
                    .map(|animation| animation.next_frame),
            )
            .min()
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let camera_changed = self.advance_camera(now);
        self.nodes.iter_mut().fold(camera_changed, |changed, node| {
            let node_changed = node
                .transcript
                .update(TranscriptEvent::AnimationFrame(now))
                .render
                != super::node::RenderRequest::None;
            changed || node_changed
        })
    }

    pub(super) fn finish_camera_animation(&mut self) {
        let Some(animation) = self.camera.animation.take() else {
            return;
        };
        self.camera.center = Some(animation.to);
    }

    pub(super) fn open_tree(&mut self) {
        self.filter = AgentFilter::Active;
        let layout = self.layout();
        self.focus_oldest(&layout);
    }

    pub(super) fn update_tree(&mut self, event: Event) -> Option<SubagentEffect> {
        self.update_tree_at(event, Instant::now())
    }

    fn update_tree_at(&mut self, event: Event, now: Instant) -> Option<SubagentEffect> {
        let Event::Key(key) = event else {
            return None;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }

        match key.code {
            KeyCode::Esc => Some(SubagentEffect::Dismiss),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_focus(Direction::Parent, now);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_focus(Direction::Child, now);
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.move_focus(Direction::PreviousOnLevel, now);
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.move_focus(Direction::NextOnLevel, now);
                None
            }
            KeyCode::Home => {
                self.move_focus(Direction::Root, now);
                None
            }
            KeyCode::Char('f') if key.modifiers.is_empty() => {
                self.filter = self.filter.toggled();
                let layout = self.layout();
                self.focus_oldest(&layout);
                None
            }
            KeyCode::Char('-') if key.modifiers.is_empty() => {
                self.max_subagents = if self.max_subagents == usize::MAX {
                    self.active_count().max(1)
                } else {
                    self.max_subagents.saturating_sub(1)
                };
                Some(SubagentEffect::SetMaxSubagents(self.max_subagents))
            }
            KeyCode::Char('+') | KeyCode::Char('=') if key.modifiers.is_empty() => {
                self.max_subagents = self.max_subagents.saturating_add(1);
                Some(SubagentEffect::SetMaxSubagents(self.max_subagents))
            }
            KeyCode::Enter => self.focused.map(SubagentEffect::Inspect),
            _ => None,
        }
    }

    pub(super) fn update_transcript(
        &mut self,
        id: AgentId,
        event: Event,
    ) -> Option<SubagentEffect> {
        if matches!(
            &event,
            Event::Key(key)
                if key.code == KeyCode::Esc
                    && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        ) {
            let Some(node) = self.node_mut(id) else {
                return Some(SubagentEffect::Back);
            };
            if node.transcript.component().expandables_focused() {
                node.transcript.update(TranscriptEvent::BlurExpandables);
                return None;
            }
            return Some(SubagentEffect::Back);
        }
        let Some(node) = self.node_mut(id) else {
            return Some(SubagentEffect::Back);
        };
        if let Some(destination) = node.transcript.component().link_destination(&event) {
            return Some(SubagentEffect::OpenLink(destination.to_string()));
        }
        if let Some(command) = node.transcript.component().scroll_command(&event) {
            node.transcript.update(TranscriptEvent::Scroll(command));
        } else if let Some(command) = node.transcript.component().expandable_command(&event) {
            node.transcript.update(TranscriptEvent::Expandable(command));
        }
        None
    }

    pub(super) fn toggle_expand_all(&mut self, id: AgentId) -> bool {
        let Some(node) = self.node_mut(id) else {
            return false;
        };
        node.transcript.update(TranscriptEvent::ToggleExpandAll);
        true
    }

    pub(super) fn render_tree(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut keys = TREE_KEYS;
        keys[5].1 = self.filter.helper();
        let layout = Floating::new("Sub-agent tree", area.width, area.height, &keys)
            .render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }

        let tree_layout = self.layout();
        self.ensure_focus(&tree_layout);
        let Some(focused) = self.focused else {
            let message = if self.nodes.is_empty() {
                format!(
                    "Concurrency: {} / {} active. No subagents have been delegated yet.",
                    self.active_count(),
                    self.concurrency_label()
                )
            } else {
                format!(
                    "Concurrency: {} / {} active. No subagents are currently running. Press f to show all.",
                    self.active_count(),
                    self.concurrency_label()
                )
            };
            frame.render_widget(
                Paragraph::new(message)
                    .style(Style::default().fg(theme.muted()))
                    .wrap(Wrap { trim: true }),
                inset(layout.body, 2, 1),
            );
            return;
        };

        let (canvas, inspector) = split_inspector(layout.body);
        if canvas.is_empty() {
            return;
        }
        let focus_center = tree_layout
            .center(focused)
            .expect("focused agent should have a layout position");
        self.sync_camera_target(focus_center, Instant::now());
        let camera_center = self.camera.center.unwrap_or(focus_center);

        render_edges(frame, canvas, theme, &tree_layout, camera_center);
        for (id, position) in tree_layout.positioned_nodes() {
            let Some(node) = self.node(id) else {
                continue;
            };
            render_node(
                frame,
                canvas,
                theme,
                camera_center,
                NodeRender {
                    node,
                    position,
                    focused: id == focused,
                    child_count: tree_layout.children(id).len(),
                },
            );
        }
        self.render_inspector(frame, inspector, theme, focused, &tree_layout);
    }

    pub(super) fn render_transcript(
        &mut self,
        id: AgentId,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
    ) {
        let Some(node) = self.node_mut(id) else {
            return;
        };
        let title = format!(
            "{} · {} · #{}",
            node.descriptor.role,
            model_name(Model::Sol),
            node.descriptor.id
        );
        let keys: &[(&str, &str)] = if node.transcript.component().expandables_focused() {
            &FOCUSED_ENTRY_KEYS
        } else {
            &TRANSCRIPT_KEYS
        };
        let layout = Floating::new(&title, area.width, area.height, keys)
            .colors(theme.border(), theme.model(Model::Sol))
            .render(frame, area, theme);
        node.transcript.render(frame, layout.body, theme);
    }

    fn layout(&self) -> TreeLayout {
        let visible = self.visible_ids();
        let nodes = self
            .nodes
            .iter()
            .filter(|node| visible.contains(&node.descriptor.id))
            .map(|node| LayoutNode {
                id: node.descriptor.id,
                parent: node.descriptor.parent,
            })
            .collect::<Vec<_>>();
        TreeLayout::new(&nodes)
    }

    fn visible_ids(&self) -> Vec<AgentId> {
        self.nodes
            .iter()
            .filter(|node| self.filter.includes(&node.status))
            .map(|node| node.descriptor.id)
            .collect()
    }

    fn ensure_focus(&mut self, layout: &TreeLayout) {
        if self
            .focused
            .is_some_and(|focused| layout.position(focused).is_some())
        {
            return;
        }
        self.focused = layout.roots().first().copied();
        self.camera.center = self.focused.and_then(|id| layout.center(id));
        self.camera.animation = None;
    }

    fn focus_oldest(&mut self, layout: &TreeLayout) {
        self.focused = self
            .nodes
            .iter()
            .filter(|node| self.filter.includes(&node.status))
            .map(|node| node.descriptor.id)
            .filter(|id| layout.position(*id).is_some())
            .min();
        self.camera.center = self.focused.and_then(|id| layout.center(id));
        self.camera.animation = None;
    }

    fn move_focus(&mut self, direction: Direction, now: Instant) {
        let layout = self.layout();
        self.ensure_focus(&layout);
        let Some(current) = self.focused else {
            return;
        };

        let target = match direction {
            Direction::Parent => layout.parent(current),
            Direction::Child => {
                let children = layout.children(current);
                self.remembered_children
                    .get(&current)
                    .copied()
                    .filter(|child| children.contains(child))
                    .or_else(|| {
                        let parent_x = layout.position(current)?.center_x;
                        children.iter().copied().min_by_key(|child| {
                            layout
                                .position(*child)
                                .map_or(i32::MAX, |position| (position.center_x - parent_x).abs())
                        })
                    })
            }
            Direction::PreviousOnLevel => {
                previous_or_next_on_level(&layout, current, HorizontalDirection::Previous)
            }
            Direction::NextOnLevel => {
                previous_or_next_on_level(&layout, current, HorizontalDirection::Next)
            }
            Direction::Root => layout.roots().first().copied(),
        };
        let Some(target) = target else {
            return;
        };

        if let Some(parent) = layout.parent(target) {
            self.remembered_children.insert(parent, target);
        }
        if direction == Direction::Parent {
            self.remembered_children.insert(target, current);
        }
        self.focused = Some(target);
        self.recenter_on_focus(&layout, now);
    }

    fn recenter_on_focus(&mut self, layout: &TreeLayout, now: Instant) {
        self.advance_camera(now);
        let Some(target) = self.focused.and_then(|id| layout.center(id)) else {
            return;
        };
        self.start_camera_animation(target, now);
    }

    fn sync_camera_target(&mut self, target: WorldPoint, now: Instant) {
        if self
            .camera
            .animation
            .as_ref()
            .is_some_and(|animation| distance(animation.to, target) < f64::EPSILON)
        {
            return;
        }
        if self
            .camera
            .center
            .is_some_and(|center| distance(center, target) < f64::EPSILON)
        {
            return;
        }
        self.advance_camera(now);
        self.start_camera_animation(target, now);
    }

    fn start_camera_animation(&mut self, target: WorldPoint, now: Instant) {
        let Some(from) = self.camera.center else {
            self.camera.center = Some(target);
            return;
        };
        if distance(from, target) < f64::EPSILON {
            self.camera.animation = None;
            return;
        }

        let duration_ms = (CAMERA_MIN_DURATION.as_millis() as f64 + distance(from, target))
            .min(CAMERA_MAX_DURATION.as_millis() as f64);
        let duration = Duration::from_millis(duration_ms.round() as u64);
        self.camera.animation = Some(CameraAnimation {
            from,
            to: target,
            started_at: now,
            duration,
            next_frame: now + CAMERA_FRAME_INTERVAL,
        });
    }

    fn advance_camera(&mut self, now: Instant) -> bool {
        let Some(animation) = &mut self.camera.animation else {
            return false;
        };
        if now < animation.next_frame {
            return false;
        }
        let elapsed = now.saturating_duration_since(animation.started_at);
        let progress = (elapsed.as_secs_f64() / animation.duration.as_secs_f64()).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.camera.center = Some(WorldPoint {
            x: animation.from.x + (animation.to.x - animation.from.x) * eased,
            y: animation.from.y + (animation.to.y - animation.from.y) * eased,
        });

        if progress >= 1.0 {
            self.camera.center = Some(animation.to);
            self.camera.animation = None;
        } else {
            animation.next_frame = now + CAMERA_FRAME_INTERVAL;
        }
        true
    }

    fn render_inspector(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: AgentId,
        layout: &TreeLayout,
    ) {
        if area.is_empty() {
            return;
        }
        let Some(node) = self.node(focused) else {
            return;
        };
        let (symbol, color, status) = state_style(&node.status);
        let title = format!(
            " {symbol} #{} · {} · {status} ",
            focused, node.descriptor.role
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(color))
            .title(title)
            .title_style(Style::default().fg(color).add_modifier(Modifier::BOLD));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.is_empty() {
            return;
        }

        let parent = layout
            .parent(focused)
            .map_or_else(|| "root".to_owned(), |id| format!("parent #{id}"));
        let children = layout.children(focused).len();
        let task = truncate_with_ellipsis(&node.descriptor.task, inner.width.saturating_sub(6));
        let lines = vec![
            Line::from(vec![
                Span::styled("Task  ", Style::default().fg(theme.muted())),
                Span::styled(task, Style::default().fg(theme.text())),
            ]),
            Line::from(vec![
                Span::styled("Tree  ", Style::default().fg(theme.muted())),
                Span::raw(format!("{parent} · {children} children")),
                Span::styled(
                    format!(
                        "    Concurrency  {} / {} active",
                        self.active_count(),
                        self.concurrency_label()
                    ),
                    Style::default().fg(theme.muted()),
                ),
            ]),
            Line::from(vec![
                Span::styled("View  ", Style::default().fg(theme.muted())),
                Span::raw(format!(
                    "{} agents · {} filter",
                    self.nodes.len(),
                    self.filter.label()
                )),
                Span::styled("    Model  ", Style::default().fg(theme.muted())),
                Span::styled(
                    model_name(Model::Sol),
                    Style::default()
                        .fg(theme.model(Model::Sol))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Session  ", Style::default().fg(theme.muted())),
                Span::raw(truncate_with_ellipsis(
                    &node.descriptor.session_id,
                    inner.width.saturating_sub(9),
                )),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn node(&self, id: AgentId) -> Option<&AgentNode> {
        self.nodes.iter().find(|node| node.descriptor.id == id)
    }

    fn node_mut(&mut self, id: AgentId) -> Option<&mut AgentNode> {
        self.nodes.iter_mut().find(|node| node.descriptor.id == id)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Direction {
    Parent,
    Child,
    PreviousOnLevel,
    NextOnLevel,
    Root,
}

#[derive(Clone, Copy)]
enum HorizontalDirection {
    Previous,
    Next,
}

fn previous_or_next_on_level(
    layout: &TreeLayout,
    current: AgentId,
    direction: HorizontalDirection,
) -> Option<AgentId> {
    let current_position = layout.position(current)?;
    let mut level = layout
        .positioned_nodes()
        .filter(|(_, position)| position.top == current_position.top)
        .collect::<Vec<_>>();
    level.sort_unstable_by_key(|(id, position)| (position.center_x, *id));
    let index = level.iter().position(|&(id, _)| id == current)?;
    match direction {
        HorizontalDirection::Previous => index.checked_sub(1).map(|index| level[index].0),
        HorizontalDirection::Next => level.get(index + 1).map(|(id, _)| *id),
    }
}

fn split_inspector(area: Rect) -> (Rect, Rect) {
    if area.height <= 4 {
        return (area, Rect::default());
    }
    let inspector_height = INSPECTOR_HEIGHT.min(area.height.saturating_sub(3));
    let canvas = Rect {
        height: area.height - inspector_height,
        ..area
    };
    let inspector = Rect {
        y: canvas.bottom(),
        height: inspector_height,
        ..area
    };
    (canvas, inspector)
}

struct NodeRender<'a> {
    node: &'a AgentNode,
    position: NodePosition,
    focused: bool,
    child_count: usize,
}

fn render_node(
    frame: &mut Frame<'_>,
    canvas: Rect,
    theme: &Theme,
    camera: WorldPoint,
    render: NodeRender<'_>,
) {
    let NodeRender {
        node,
        position,
        focused,
        child_count,
    } = render;
    let left = position.center_x - NODE_WIDTH / 2;
    let border_style = if focused {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.border())
    };
    let text_style = Style::default().fg(theme.text());
    let (symbol, status_color, status) = state_style(&node.status);
    let detail_style = Style::default().fg(status_color);
    let role_width = u16::try_from(NODE_WIDTH.saturating_sub(4)).unwrap_or_default();
    let role = truncate_with_ellipsis(&node.descriptor.role, role_width);
    let title = centered_text(
        &format!("{symbol} #{} {role}", node.descriptor.id),
        NODE_WIDTH - 2,
    );
    let detail = centered_text(
        &format!("{status} · {child_count} children"),
        NODE_WIDTH - 2,
    );
    let top = format!(
        "╭{}╮",
        "─".repeat(usize::try_from(NODE_WIDTH - 2).unwrap_or_default())
    );
    let bottom = format!(
        "╰{}╯",
        "─".repeat(usize::try_from(NODE_WIDTH - 2).unwrap_or_default())
    );
    draw_world_string(
        frame,
        canvas,
        left,
        position.top,
        camera,
        &top,
        border_style,
    );
    for (row, (text, style)) in [(title, text_style), (detail, detail_style)]
        .into_iter()
        .enumerate()
    {
        let y = position.top + i32::try_from(row).unwrap_or_default() + 1;
        draw_world_string(frame, canvas, left, y, camera, "│", border_style);
        draw_world_string(frame, canvas, left + 1, y, camera, &text, style);
        draw_world_string(
            frame,
            canvas,
            left + NODE_WIDTH - 1,
            y,
            camera,
            "│",
            border_style,
        );
    }
    draw_world_string(
        frame,
        canvas,
        left,
        position.top + NODE_HEIGHT - 1,
        camera,
        &bottom,
        border_style,
    );
}

fn render_edges(
    frame: &mut Frame<'_>,
    canvas: Rect,
    theme: &Theme,
    layout: &TreeLayout,
    camera: WorldPoint,
) {
    let mut cells = HashMap::<(i32, i32), u8>::new();
    let mut arrows = Vec::new();
    for (parent, position) in layout.positioned_nodes() {
        let children = layout.children(parent);
        if children.is_empty() {
            continue;
        }
        let child_centers = children
            .iter()
            .filter_map(|&id| layout.position(id).map(|position| position.center_x))
            .collect::<Vec<_>>();
        if child_centers.is_empty() {
            continue;
        }

        let start_y = position.top + NODE_HEIGHT;
        let junction_y = start_y + VERTICAL_GAP / 2;
        if child_centers.len() == 1 {
            let child_top = layout.position(children[0]).unwrap().top;
            add_vertical(&mut cells, position.center_x, start_y, child_top - 2);
            arrows.push((child_centers[0], child_top - 1));
            continue;
        }
        add_vertical(&mut cells, position.center_x, start_y, junction_y);
        let first = child_centers[0].min(position.center_x);
        let last = child_centers[child_centers.len() - 1].max(position.center_x);
        add_horizontal(&mut cells, first, last, junction_y);
        for (index, child_x) in child_centers.into_iter().enumerate() {
            let child_top = layout.position(children[index]).unwrap().top;
            add_vertical(&mut cells, child_x, junction_y, child_top - 2);
            arrows.push((child_x, child_top - 1));
        }
    }

    let style = Style::default().fg(theme.border());
    for ((x, y), connections) in cells {
        draw_world_string(frame, canvas, x, y, camera, edge_symbol(connections), style);
    }
    for (x, y) in arrows {
        draw_world_string(frame, canvas, x, y, camera, "↓", style);
    }
}

const UP: u8 = 1;
const RIGHT: u8 = 2;
const DOWN: u8 = 4;
const LEFT: u8 = 8;

fn add_vertical(cells: &mut HashMap<(i32, i32), u8>, x: i32, start: i32, end: i32) {
    if start > end {
        return;
    }
    for y in start..=end {
        let mut connections = 0;
        if y > start {
            connections |= UP;
        }
        if y < end {
            connections |= DOWN;
        }
        if connections == 0 {
            connections = UP | DOWN;
        }
        *cells.entry((x, y)).or_default() |= connections;
    }
}

fn add_horizontal(cells: &mut HashMap<(i32, i32), u8>, start: i32, end: i32, y: i32) {
    if start > end {
        return;
    }
    for x in start..=end {
        let mut connections = 0;
        if x > start {
            connections |= LEFT;
        }
        if x < end {
            connections |= RIGHT;
        }
        if connections == 0 {
            connections = LEFT | RIGHT;
        }
        *cells.entry((x, y)).or_default() |= connections;
    }
}

const fn edge_symbol(connections: u8) -> &'static str {
    match connections {
        5 => "│",
        10 => "─",
        6 => "╭",
        12 => "╮",
        3 => "╰",
        9 => "╯",
        7 => "├",
        13 => "┤",
        14 => "┬",
        11 => "┴",
        15 => "┼",
        _ if connections & (LEFT | RIGHT) != 0 => "─",
        _ => "│",
    }
}

fn draw_world_string(
    frame: &mut Frame<'_>,
    canvas: Rect,
    world_x: i32,
    world_y: i32,
    camera: WorldPoint,
    text: &str,
    style: Style,
) {
    let screen_x = i32::from(canvas.x)
        + i32::from(canvas.width) / 2
        + (f64::from(world_x) - camera.x).round() as i32;
    let screen_y = i32::from(canvas.y)
        + i32::from(canvas.height) / 2
        + (f64::from(world_y) - camera.y).round() as i32;
    if screen_y < i32::from(canvas.y) || screen_y >= i32::from(canvas.bottom()) {
        return;
    }

    let text = sanitize_terminal_text_inline(text);
    let mut x = screen_x;
    for grapheme in text.graphemes(true) {
        let width = i32::try_from(UnicodeWidthStr::width(grapheme)).unwrap_or(i32::MAX);
        if x >= i32::from(canvas.x) && x.saturating_add(width) <= i32::from(canvas.right()) {
            let position = (
                u16::try_from(x).unwrap_or_default(),
                u16::try_from(screen_y).unwrap_or_default(),
            );
            frame.buffer_mut()[position]
                .set_symbol(grapheme)
                .set_style(style);
        }
        x = x.saturating_add(width);
    }
}

fn centered_text(text: &str, width: i32) -> String {
    let width = u16::try_from(width).unwrap_or_default();
    let text = truncate_with_ellipsis(text, width);
    let text_width = u16::try_from(UnicodeWidthStr::width(text.as_str())).unwrap_or(u16::MAX);
    let padding = width.saturating_sub(text_width);
    let left = padding / 2;
    let right = padding - left;
    format!(
        "{}{text}{}",
        " ".repeat(usize::from(left)),
        " ".repeat(usize::from(right))
    )
}

const fn state_style(status: &AgentStatus) -> (&'static str, Color, &'static str) {
    match status {
        AgentStatus::Pending => ("○", Color::Yellow, "pending"),
        AgentStatus::Running => ("◐", Color::Yellow, "running"),
        AgentStatus::Completed { .. } => ("●", Color::Green, "completed"),
        AgentStatus::Interrupted => ("■", Color::Blue, "interrupted"),
        AgentStatus::Failed { .. } => ("×", Color::Red, "failed"),
        AgentStatus::Closing => ("◑", Color::Yellow, "closing"),
        AgentStatus::Closed => ("■", Color::DarkGray, "closed"),
    }
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(horizontal),
        area.y.saturating_add(vertical),
        area.width.saturating_sub(horizontal.saturating_mul(2)),
        area.height.saturating_sub(vertical.saturating_mul(2)),
    )
}

fn truncate_with_ellipsis(text: &str, width: u16) -> String {
    let text = sanitize_terminal_text_inline(text);
    let text = text.as_ref();
    if UnicodeWidthStr::width(text) <= usize::from(width) {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let target = width.saturating_sub(1);
    let mut rendered = String::new();
    let mut used = 0_u16;
    for grapheme in text.graphemes(true) {
        let grapheme_width = u16::try_from(UnicodeWidthStr::width(grapheme)).unwrap_or(u16::MAX);
        if used.saturating_add(grapheme_width) > target {
            break;
        }
        rendered.push_str(grapheme);
        used = used.saturating_add(grapheme_width);
    }
    rendered.push('…');
    rendered
}

fn distance(from: WorldPoint, to: WorldPoint) -> f64 {
    (to.x - from.x).hypot(to.y - from.y)
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn model_name(model: Model) -> &'static str {
    match model {
        Model::Luna => "Luna",
        Model::Terra => "Terra",
        Model::Sol => "Sol",
        Model::Astra => "Astra",
        _ => model.as_str(),
    }
}
