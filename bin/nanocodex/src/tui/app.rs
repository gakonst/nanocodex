use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use nanocodex::{AgentEvent, AgentEventKind, Prompt, UserInput};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};
use serde::Deserialize;
use serde_json::Value;

use super::composer::ComposerLayout;
use super::selection::ScreenSelection;
use super::transcript::{ToolStatus, Transcript, TranscriptItem};

const MAX_TOOL_ARGUMENT_CHARS: usize = 180;
const MAX_MULTILINE_TOOL_ARGUMENT_CHARS: usize = 4_000;
const MAX_MULTILINE_TOOL_ARGUMENT_LINES: usize = 24;
const MAX_PATCH_ARGUMENT_CHARS: usize = 64 * 1_024;
const MAX_PATCH_ARGUMENT_LINES: usize = 1_000;
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1_000;
const CANCEL_CONFIRMATION_WINDOW: Duration = Duration::from_secs(1);
const SMOOTH_SCROLL_BACKLOG_ROWS: usize = 8;
const MAX_SMOOTH_SCROLL_CATCH_UP_ROWS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
struct AttachedImage {
    placeholder: String,
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingPaste {
    placeholder: String,
    text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SubmittedPrompt {
    display: String,
    local_images: Vec<PathBuf>,
}

impl SubmittedPrompt {
    fn new(display: String, local_images: Vec<PathBuf>) -> Self {
        Self {
            display,
            local_images,
        }
    }

    pub(super) fn text(text: String) -> Self {
        Self::new(text, Vec::new())
    }

    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) fn set_display(&mut self, display: String) {
        self.display = display;
    }

    pub(super) fn prepend_text(&mut self, prefix: &str) {
        self.display.insert_str(0, prefix);
    }

    pub(super) fn into_prompt(self) -> Prompt {
        if self.local_images.is_empty() {
            return Prompt::new(self.display);
        }

        let mut content = self
            .local_images
            .into_iter()
            .map(|path| UserInput::LocalImage { path, detail: None })
            .collect::<Vec<_>>();
        if !self.display.is_empty() {
            content.push(UserInput::Text { text: self.display });
        }
        Prompt::content(content)
    }
}

impl From<String> for SubmittedPrompt {
    fn from(value: String) -> Self {
        Self::text(value)
    }
}

impl From<&str> for SubmittedPrompt {
    fn from(value: &str) -> Self {
        Self::text(value.to_owned())
    }
}

impl PartialEq<str> for SubmittedPrompt {
    fn eq(&self, other: &str) -> bool {
        self.display == other
    }
}

impl PartialEq<&str> for SubmittedPrompt {
    fn eq(&self, other: &&str) -> bool {
        self.display == *other
    }
}

struct PendingScrollAnchor {
    width: u16,
    viewport_height: u16,
    height_before_capped: usize,
    changed_tail: Option<(usize, usize)>,
    new_entries_start: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum PaneId {
    Main,
    Btw(u64),
}

pub(super) struct PendingSteer {
    id: u64,
    run_generation: u64,
    prompt: String,
    state: PendingSteerState,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PendingSteerState {
    Submitting,
    Admitted,
}

impl PendingSteer {
    pub(super) fn prompt(&self) -> &str {
        &self.prompt
    }

    pub(super) fn is_admitted(&self) -> bool {
        self.state == PendingSteerState::Admitted
    }
}

pub(super) struct Conversation {
    pub(super) transcript: Transcript,
    pub(super) selected_user: Option<usize>,
    pub(super) pending_turns: usize,
    pub(super) running: bool,
    pub(super) status: String,
    pub(super) scroll_from_bottom: usize,
    pub(super) has_unseen_output: bool,
    smooth_scroll_from_bottom: usize,
    viewport_width: Option<u16>,
    viewport_height: Option<u16>,
    pending_scroll_anchor: Option<PendingScrollAnchor>,
    streamed_this_turn: bool,
    pending_run_error: Option<String>,
    pub(super) queued_prompts: VecDeque<String>,
    queued_prompt_ids: VecDeque<u64>,
    displayed_queued_prompt: Option<u64>,
    pub(super) pending_steers: VecDeque<PendingSteer>,
    run_generation: u64,
    applied_steer_runs_waiting_for_ack: VecDeque<u64>,
}

impl Conversation {
    fn new(status: impl Into<String>) -> Self {
        Self {
            transcript: Transcript::default(),
            selected_user: None,
            pending_turns: 0,
            running: false,
            status: status.into(),
            scroll_from_bottom: 0,
            has_unseen_output: false,
            smooth_scroll_from_bottom: 0,
            viewport_width: None,
            viewport_height: None,
            pending_scroll_anchor: None,
            streamed_this_turn: false,
            pending_run_error: None,
            queued_prompts: VecDeque::new(),
            queued_prompt_ids: VecDeque::new(),
            displayed_queued_prompt: None,
            pending_steers: VecDeque::new(),
            run_generation: 0,
            applied_steer_runs_waiting_for_ack: VecDeque::new(),
        }
    }

    fn fork_before(&self, transcript_index: usize) -> Self {
        let mut branch = Self::new("Ready");
        branch.transcript = self.transcript.prefix_before(transcript_index);
        branch
    }

    fn queue_prompt(&mut self, id: u64, prompt: String) {
        let display_immediately = !self.running && self.queued_prompts.is_empty();
        if display_immediately {
            self.note_new_entry();
            self.transcript.push_editable_user(prompt.clone(), id);
            self.displayed_queued_prompt = Some(id);
        }
        self.queued_prompts.push_back(prompt);
        self.queued_prompt_ids.push_back(id);
        self.pending_turns += 1;
        self.status = if self.running {
            "Prompt queued".to_owned()
        } else {
            "Starting".to_owned()
        };
        self.jump_to_bottom();
    }

    fn queue_steer(&mut self, id: u64, prompt: String) {
        self.pending_steers.push_back(PendingSteer {
            id,
            run_generation: self.run_generation,
            prompt,
            state: PendingSteerState::Submitting,
        });
        "Submitting steer".clone_into(&mut self.status);
        self.jump_to_bottom();
    }

    fn steer_admitted(&mut self, id: u64) {
        let Some(steer) = self.pending_steers.iter_mut().find(|steer| steer.id == id) else {
            return;
        };
        steer.state = PendingSteerState::Admitted;
        let applied = self.reconcile_applied_steers();
        if self.running {
            self.status = if applied == 0 {
                "Steer pending".to_owned()
            } else {
                "Steer applied".to_owned()
            };
        }
    }

    fn steer_queued(&mut self, id: u64, prompt: String) {
        self.remove_pending_steer(id);
        self.queue_prompt(id, prompt);
        self.reconcile_applied_steers();
    }

    fn steer_failed(&mut self, id: u64, error: String) {
        self.remove_pending_steer(id);
        self.push_output(TranscriptItem::Error(error));
        self.reconcile_applied_steers();
        self.status = if self.running {
            "Working".to_owned()
        } else {
            "Ready".to_owned()
        };
    }

    fn turn_finished(&mut self, error: Option<String>) {
        self.pending_turns = self.pending_turns.saturating_sub(1);
        if let Some(error) = error {
            self.push_output(TranscriptItem::Error(error));
        }
    }

    fn on_agent_event(&mut self, event: &AgentEvent) -> bool {
        match event.kind {
            AgentEventKind::RunStarted => {
                if let (Some(prompt), Some(prompt_id)) = (
                    self.queued_prompts.pop_front(),
                    self.queued_prompt_ids.pop_front(),
                ) && self.displayed_queued_prompt.take() != Some(prompt_id)
                {
                    self.note_new_entry();
                    self.transcript.push_editable_user(prompt, prompt_id);
                }
                self.running = true;
                self.run_generation = self.run_generation.saturating_add(1);
                self.streamed_this_turn = false;
                self.pending_run_error = None;
                "Thinking...".clone_into(&mut self.status);
            }
            AgentEventKind::RunSteered => {
                self.applied_steer_runs_waiting_for_ack
                    .push_back(self.run_generation);
                self.reconcile_applied_steers();
                "Steer applied".clone_into(&mut self.status);
            }
            AgentEventKind::AssistantDelta => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    self.push_assistant_delta(&payload.text);
                }
            }
            AgentEventKind::AssistantMessage => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    if self.streamed_this_turn {
                        self.note_tail_will_change();
                    } else {
                        self.push_output(TranscriptItem::Assistant(payload.text.clone()));
                    }
                    let _ = self.transcript.finalize_assistant(&payload.text);
                }
            }
            AgentEventKind::ReasoningSummaryDelta => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    self.push_reasoning_delta(&payload.text);
                    "Thinking...".clone_into(&mut self.status);
                }
            }
            AgentEventKind::ToolCall => {
                self.on_tool_call(event);
            }
            AgentEventKind::ToolResult => {
                self.on_tool_result(event);
            }
            AgentEventKind::RunError => {
                if let Ok(payload) = event.decode_payload::<ErrorPayload>() {
                    self.pending_run_error = Some(payload.message);
                }
            }
            AgentEventKind::RunCompleted => {
                if let Some(error) = self.pending_run_error.take() {
                    self.push_output(TranscriptItem::Error(error));
                }
                self.running = false;
                self.reconcile_applied_steers();
                "Ready".clone_into(&mut self.status);
            }
            AgentEventKind::RunFailed => {
                self.run_failed(event);
            }
            AgentEventKind::ApiEvent
            | AgentEventKind::ModelWarmupStarted
            | AgentEventKind::ModelWarmupCompleted
            | AgentEventKind::ModelWarmupFailed
            | AgentEventKind::ModelCallStarted
            | AgentEventKind::ModelCallCompleted
            | AgentEventKind::ModelCallFailed
            | AgentEventKind::ModelCompactionStarted
            | AgentEventKind::ModelCompactionCompleted
            | AgentEventKind::ModelCompactionFailed
            | AgentEventKind::ModelAttemptStarted
            | AgentEventKind::ModelAttemptFailed
            | AgentEventKind::ModelAttemptRetrying
            | AgentEventKind::ModelConnectionStarted
            | AgentEventKind::ModelConnectionCompleted
            | AgentEventKind::ModelConnectionFailed => return false,
        }
        true
    }

    fn on_tool_call(&mut self, event: &AgentEvent) {
        let Ok(payload) = event.decode_payload::<ToolCallPayload>() else {
            return;
        };
        let arguments = summarize_tool_arguments(&payload.tool, &payload.arguments);
        self.status = format!("Running {}", payload.tool);
        let call_id = payload.call_id;
        let name = payload.tool;
        let status = ToolStatus::Running;
        if self.transcript.has_tool_parent(&call_id) {
            self.note_tail_will_change();
            let _ = self
                .transcript
                .push_tool_child(call_id, name, arguments, status);
        } else {
            self.push_output(TranscriptItem::Tool {
                call_id,
                name,
                arguments,
                status,
            });
        }
    }

    fn on_tool_result(&mut self, event: &AgentEvent) {
        let Ok(payload) = event.decode_payload::<ToolResultPayload>() else {
            return;
        };
        let status = match payload.status.as_str() {
            "completed" => ToolStatus::Completed,
            "cancelled" => ToolStatus::Cancelled,
            _ => ToolStatus::Failed,
        };
        let result = payload
            .result
            .as_ref()
            .map(|result| summarize_tool_result(payload.tool.as_deref(), result, status));
        self.note_tail_will_change();
        let _ =
            self.transcript
                .set_tool_result(&payload.call_id, status, payload.duration_ns, result);
        self.note_unseen_output();
        "Working".clone_into(&mut self.status);
    }

    fn run_failed(&mut self, event: &AgentEvent) {
        self.running = false;
        self.reconcile_applied_steers();
        let cancelled = event
            .decode_payload::<TerminalPayload>()
            .is_ok_and(|payload| payload.status == "cancelled");
        if cancelled {
            self.pending_run_error = None;
            "Cancelled".clone_into(&mut self.status);
        } else {
            if let Some(error) = self.pending_run_error.take() {
                self.push_output(TranscriptItem::Error(error));
            }
            "Turn failed".clone_into(&mut self.status);
        }
    }

    fn reconcile_applied_steers(&mut self) -> usize {
        let mut applied = 0;
        while let Some(run_generation) = self.applied_steer_runs_waiting_for_ack.front().copied() {
            let Some(index) = self
                .pending_steers
                .iter()
                .position(|steer| steer.run_generation == run_generation)
            else {
                break;
            };
            let Some(steer) = self.pending_steers.get(index) else {
                break;
            };
            if !steer.is_admitted() {
                break;
            }
            let Some(steer) = self.pending_steers.remove(index) else {
                break;
            };
            self.push_output(TranscriptItem::User(steer.prompt));
            let _ = self.applied_steer_runs_waiting_for_ack.pop_front();
            applied += 1;
        }
        if !self.running {
            self.pending_steers.retain(|steer| {
                self.applied_steer_runs_waiting_for_ack
                    .contains(&steer.run_generation)
            });
        }
        applied
    }

    fn remove_pending_steer(&mut self, id: u64) {
        if let Some(index) = self.pending_steers.iter().position(|steer| steer.id == id) {
            drop(self.pending_steers.remove(index));
        }
    }

    pub(super) fn push_assistant_delta(&mut self, delta: &str) {
        let append_to_current = self.streamed_this_turn;
        self.streamed_this_turn = true;
        if append_to_current && self.transcript.tail_is_assistant() {
            self.note_tail_will_change();
            let _ = self.transcript.append_assistant_delta(delta);
        } else {
            self.push_output(TranscriptItem::Assistant(delta.to_owned()));
        }
    }

    fn push_reasoning_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.transcript.tail_is_reasoning() {
            self.note_tail_will_change();
            let _ = self.transcript.append_reasoning_delta(delta);
        } else {
            self.push_output(TranscriptItem::Reasoning(delta.to_owned()));
        }
    }

    #[allow(dead_code)]
    pub(super) fn settle_viewport(&mut self, width: u16, height: u16) {
        self.settle_viewport_with_selection(width, height, false);
    }

    pub(super) fn settle_viewport_with_selection(
        &mut self,
        width: u16,
        height: u16,
        preserve_view: bool,
    ) {
        let viewport_changed =
            self.viewport_width != Some(width) || self.viewport_height != Some(height);
        if let Some(pending) = self.pending_scroll_anchor.take() {
            let changed_tail_rows = pending.changed_tail.map_or(0, |(index, before)| {
                self.transcript
                    .height_at(index, pending.width)
                    .unwrap_or(before)
                    .saturating_sub(before)
            });
            let new_entry_rows = pending
                .new_entries_start
                .map_or(0, |first| self.transcript.height_from(first, pending.width));
            if self.scroll_from_bottom > 0 || preserve_view {
                self.scroll_from_bottom = self
                    .scroll_from_bottom
                    .saturating_add(changed_tail_rows)
                    .saturating_add(new_entry_rows);
            } else if self.selected_user.is_none()
                && pending.width == width
                && pending.viewport_height == height
            {
                let viewport_shift = pending
                    .height_before_capped
                    .saturating_add(changed_tail_rows)
                    .saturating_add(new_entry_rows)
                    .saturating_sub(usize::from(height));
                self.queue_smooth_scroll(viewport_shift);
            }
        }
        self.viewport_width = Some(width);
        self.viewport_height = Some(height);
        if viewport_changed || preserve_view {
            self.clamp_scroll();
        }
    }

    fn scroll_up(&mut self, rows: usize) {
        self.scroll_from_bottom = self
            .scroll_from_bottom
            .saturating_add(self.smooth_scroll_from_bottom)
            .saturating_add(rows);
        self.smooth_scroll_from_bottom = 0;
        self.pending_scroll_anchor = None;
        self.clamp_scroll();
    }

    fn scroll_down(&mut self, rows: usize) {
        self.scroll_from_bottom = self
            .scroll_from_bottom
            .saturating_add(self.smooth_scroll_from_bottom);
        self.smooth_scroll_from_bottom = 0;
        self.pending_scroll_anchor = None;
        self.clamp_scroll();
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(rows);
    }

    pub(super) fn display_scroll_from_bottom(&self) -> usize {
        self.scroll_from_bottom
            .saturating_add(self.smooth_scroll_from_bottom)
    }

    fn queue_smooth_scroll(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        self.smooth_scroll_from_bottom = self.smooth_scroll_from_bottom.saturating_add(rows);
    }

    fn advance_smooth_scroll(&mut self) {
        let drain = smooth_scroll_drain(self.smooth_scroll_from_bottom);
        self.smooth_scroll_from_bottom = self.smooth_scroll_from_bottom.saturating_sub(drain);
    }

    fn smooth_scroll_pending(&self) -> bool {
        self.smooth_scroll_from_bottom > 0
    }

    fn clamp_scroll(&mut self) {
        if let (Some(width), Some(height)) = (self.viewport_width, self.viewport_height) {
            self.scroll_from_bottom =
                self.transcript
                    .clamp_scroll_from_bottom(self.scroll_from_bottom, width, height);
        }
    }

    fn select_older_user(&mut self) {
        if let Some(index) = self.transcript.previous_user(self.selected_user) {
            self.selected_user = Some(index);
        }
    }

    fn select_newer_user_or_composer(&mut self) {
        let Some(selected) = self.selected_user else {
            return;
        };
        if let Some(index) = self.transcript.next_user(selected) {
            self.selected_user = Some(index);
        } else {
            self.jump_to_bottom();
        }
    }

    pub(super) fn push_output(&mut self, item: TranscriptItem) {
        self.note_new_entry();
        self.transcript.push(item);
    }

    fn note_tail_will_change(&mut self) {
        self.has_unseen_output |= self.scroll_from_bottom > 0 || self.selected_user.is_some();
        if self.selected_user.is_some() {
            return;
        }
        let Some(width) = self.viewport_width else {
            return;
        };
        if self.pending_scroll_anchor.as_ref().is_some_and(|pending| {
            pending.new_entries_start.is_some() || pending.changed_tail.is_some()
        }) {
            return;
        }
        let changed_tail = self
            .transcript
            .tail_height(width)
            .map(|height| (self.transcript.len().saturating_sub(1), height));
        if self.pending_scroll_anchor.is_none() {
            let viewport_height = self.viewport_height.unwrap_or(0);
            self.pending_scroll_anchor = Some(PendingScrollAnchor {
                width,
                viewport_height,
                height_before_capped: self
                    .transcript
                    .height_up_to(width, usize::from(viewport_height)),
                changed_tail: None,
                new_entries_start: None,
            });
        }
        let Some(pending) = self.pending_scroll_anchor.as_mut() else {
            return;
        };
        if pending.new_entries_start.is_none() && pending.changed_tail.is_none() {
            pending.changed_tail = changed_tail;
        }
    }

    fn note_new_entry(&mut self) {
        self.has_unseen_output |= self.scroll_from_bottom > 0 || self.selected_user.is_some();
        if self.selected_user.is_some() {
            return;
        }
        let Some(width) = self.viewport_width else {
            return;
        };
        let first = self.transcript.len();
        if self.pending_scroll_anchor.is_none() {
            let viewport_height = self.viewport_height.unwrap_or(0);
            self.pending_scroll_anchor = Some(PendingScrollAnchor {
                width,
                viewport_height,
                height_before_capped: self
                    .transcript
                    .height_up_to(width, usize::from(viewport_height)),
                changed_tail: None,
                new_entries_start: None,
            });
        }
        let Some(pending) = self.pending_scroll_anchor.as_mut() else {
            return;
        };
        pending.new_entries_start.get_or_insert(first);
    }

    fn note_unseen_output(&mut self) {
        if self.scroll_from_bottom > 0 || self.selected_user.is_some() {
            self.has_unseen_output = true;
        }
    }

    fn jump_to_bottom(&mut self) {
        self.selected_user = None;
        self.scroll_from_bottom = 0;
        self.smooth_scroll_from_bottom = 0;
        self.has_unseen_output = false;
        self.pending_scroll_anchor = None;
    }
}

fn smooth_scroll_drain(pending_rows: usize) -> usize {
    if pending_rows == 0 {
        return 0;
    }
    if pending_rows <= SMOOTH_SCROLL_BACKLOG_ROWS {
        return 1;
    }
    pending_rows
        .saturating_sub(SMOOTH_SCROLL_BACKLOG_ROWS)
        .clamp(2, MAX_SMOOTH_SCROLL_CATCH_UP_ROWS)
}

pub(super) struct BtwPane {
    pub(super) id: u64,
    pub(super) request_id: Option<Arc<str>>,
    pub(super) conversation: Conversation,
}

struct MainBranchPane {
    id: u64,
    parent_id: Option<u64>,
    conversation: Conversation,
    draft: ComposerDraft,
}

struct ComposerDraft {
    input: String,
    local_images: Vec<AttachedImage>,
    pending_pastes: Vec<PendingPaste>,
    cursor: usize,
    scroll: usize,
    preferred_column: Option<usize>,
}

struct HistoricalEditor {
    source_branch_id: u64,
    prompt_id: u64,
    transcript_index: usize,
    composer_draft: ComposerDraft,
}

struct PendingHistoricalEdit {
    source_branch_id: u64,
    new_branch_id: u64,
    prompt_id: u64,
    transcript_index: usize,
    prompt: String,
}

pub(super) struct HistoricalEditRequest {
    pub(super) source_branch: u64,
    pub(super) new_branch: u64,
    pub(super) prompt: u64,
}

pub(super) struct BranchPreview<'a> {
    pub(super) id: u64,
    pub(super) parent_id: Option<u64>,
    pub(super) active: bool,
    pub(super) selected: bool,
    pub(super) prompt: Option<&'a str>,
    pub(super) tree_prefix: String,
    pub(super) depth: usize,
}

fn tree_ordered_previews(mut previews: Vec<BranchPreview<'_>>) -> Vec<BranchPreview<'_>> {
    let nodes = previews
        .iter()
        .map(|preview| (preview.id, preview.parent_id))
        .collect::<Vec<_>>();
    let ids = nodes.iter().map(|(id, _)| *id).collect::<HashSet<_>>();
    let mut roots = nodes
        .iter()
        .filter(|(_, parent)| parent.is_none_or(|parent| !ids.contains(&parent)))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    roots.sort_unstable();

    let mut ordered = Vec::with_capacity(nodes.len());
    let mut visited = HashSet::with_capacity(nodes.len());
    let mut guides = Vec::new();
    for root in roots {
        append_branch_tree(root, &nodes, &mut guides, None, &mut visited, &mut ordered);
    }
    for (id, _) in &nodes {
        append_branch_tree(*id, &nodes, &mut guides, None, &mut visited, &mut ordered);
    }

    ordered
        .into_iter()
        .filter_map(|(id, prefix, depth)| {
            let position = previews.iter().position(|preview| preview.id == id)?;
            let mut preview = previews.remove(position);
            preview.tree_prefix = prefix;
            preview.depth = depth;
            Some(preview)
        })
        .collect()
}

fn append_branch_tree(
    id: u64,
    nodes: &[(u64, Option<u64>)],
    guides: &mut Vec<bool>,
    connector_is_last: Option<bool>,
    visited: &mut HashSet<u64>,
    ordered: &mut Vec<(u64, String, usize)>,
) {
    if !visited.insert(id) {
        return;
    }
    let mut prefix = guides
        .iter()
        .map(|has_more| if *has_more { "│ " } else { "  " })
        .collect::<String>();
    if let Some(is_last) = connector_is_last {
        prefix.push_str(if is_last { "└─" } else { "├─" });
    }
    let depth = guides.len() + usize::from(connector_is_last.is_some());
    ordered.push((id, prefix, depth));

    let pushed_guide = if let Some(is_last) = connector_is_last {
        guides.push(!is_last);
        true
    } else {
        false
    };
    let mut children = nodes
        .iter()
        .filter(|(_, parent)| *parent == Some(id))
        .map(|(child, _)| *child)
        .collect::<Vec<_>>();
    children.sort_unstable();
    let last = children.len().saturating_sub(1);
    for (index, child) in children.into_iter().enumerate() {
        append_branch_tree(child, nodes, guides, Some(index == last), visited, ordered);
    }
    if pushed_guide {
        let _ = guides.pop();
    }
}

pub(super) struct App {
    pub(super) cwd: PathBuf,
    pub(super) main: Conversation,
    main_branch_id: u64,
    main_branch_parent_id: Option<u64>,
    main_branch_request_id: Option<Arc<str>>,
    main_branches: Vec<MainBranchPane>,
    historical_editor: Option<HistoricalEditor>,
    pending_historical_edit: Option<PendingHistoricalEdit>,
    pending_branch_switch: Option<u64>,
    branch_navigator: Option<u64>,
    pub(super) btw: Option<BtwPane>,
    pub(super) focus: PaneId,
    pub(super) input: String,
    local_images: Vec<AttachedImage>,
    pending_pastes: Vec<PendingPaste>,
    pub(super) cursor: usize,
    pub(super) frame: usize,
    composer_width: u16,
    composer_scroll: usize,
    preferred_column: Option<usize>,
    next_btw_id: u64,
    next_main_branch_id: u64,
    next_input_id: u64,
    cancel_confirmation: Option<CancelConfirmation>,
    screen_selection: ScreenSelection,
}

#[derive(Clone, Copy)]
struct CancelConfirmation {
    target: PaneId,
    expires_at: Instant,
}

impl App {
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            main: Conversation::new("Ready"),
            main_branch_id: 0,
            main_branch_parent_id: None,
            main_branch_request_id: None,
            main_branches: Vec::new(),
            historical_editor: None,
            pending_historical_edit: None,
            pending_branch_switch: None,
            branch_navigator: None,
            btw: None,
            focus: PaneId::Main,
            input: String::new(),
            local_images: Vec::new(),
            pending_pastes: Vec::new(),
            cursor: 0,
            frame: 0,
            composer_width: 80,
            composer_scroll: 0,
            preferred_column: None,
            next_btw_id: 1,
            next_main_branch_id: 1,
            next_input_id: 1,
            cancel_confirmation: None,
            screen_selection: ScreenSelection::default(),
        }
    }

    pub(super) fn insert_char(&mut self, character: char) {
        self.prepare_composer_edit();
        self.snap_cursor_out_of_pending_paste();
        self.input.insert(self.cursor, character);
        self.cursor += character.len_utf8();
        self.preferred_column = None;
        self.synchronize_composer_elements();
    }

    pub(super) fn insert_str(&mut self, text: &str) {
        self.prepare_composer_edit();
        self.snap_cursor_out_of_pending_paste();
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.preferred_column = None;
        self.synchronize_composer_elements();
    }

    pub(super) fn handle_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let char_count = text.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            self.prepare_composer_edit();
            self.synchronize_composer_elements();
            self.snap_cursor_out_of_pending_paste();
            let placeholder = self.next_large_paste_placeholder(char_count);
            self.input.insert_str(self.cursor, &placeholder);
            self.cursor += placeholder.len();
            self.pending_pastes.push(PendingPaste { placeholder, text });
            self.preferred_column = None;
            return;
        }
        if text.len() > 1
            && let Some(path) = normalize_pasted_path(&text)
        {
            let path = if path.is_absolute() {
                path
            } else {
                self.cwd.join(path)
            };
            if image::image_dimensions(&path).is_ok() {
                self.attach_local_image(path);
                self.insert_char(' ');
                return;
            }
        }
        self.insert_str(&text);
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.prepare_composer_edit();
        let previous = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        self.cursor = self.delete_composer_range(previous, self.cursor);
        self.preferred_column = None;
    }

    pub(super) fn delete(&mut self) {
        if self.cursor == self.input.len() {
            return;
        }
        self.prepare_composer_edit();
        let next = self.input[self.cursor..]
            .chars()
            .next()
            .map_or(self.input.len(), |character| {
                self.cursor + character.len_utf8()
            });
        self.cursor = self.delete_composer_range(self.cursor, next);
        self.preferred_column = None;
    }

    pub(super) fn move_left(&mut self) {
        let next = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        self.cursor = self
            .pending_paste_containing(next)
            .map_or(next, |(start, _)| start);
        self.preferred_column = None;
    }

    pub(super) fn move_right(&mut self) {
        if let Some(character) = self.input[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
        if let Some((_, end)) = self.pending_paste_containing(self.cursor) {
            self.cursor = end;
        }
        self.preferred_column = None;
    }

    pub(super) fn move_word_left(&mut self) {
        let mut cursor = self.cursor;
        while let Some((index, character)) = self.input[..cursor].char_indices().next_back() {
            if !character.is_whitespace() {
                break;
            }
            cursor = index;
        }
        while let Some((index, character)) = self.input[..cursor].char_indices().next_back() {
            if character.is_whitespace() {
                break;
            }
            cursor = index;
        }
        self.cursor = self
            .pending_paste_containing(cursor)
            .map_or(cursor, |(start, _)| start);
        self.preferred_column = None;
    }

    pub(super) fn move_word_right(&mut self) {
        let mut cursor = self.cursor;
        while let Some(character) = self.input[cursor..].chars().next() {
            if character.is_whitespace() {
                break;
            }
            cursor += character.len_utf8();
        }
        while let Some(character) = self.input[cursor..].chars().next() {
            if !character.is_whitespace() {
                break;
            }
            cursor += character.len_utf8();
        }
        self.cursor = self
            .pending_paste_containing(cursor)
            .map_or(cursor, |(_, end)| end);
        self.preferred_column = None;
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.preferred_column = None;
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |index| self.cursor + index);
        self.preferred_column = None;
    }

    pub(super) fn move_up(&mut self) {
        let focus = self.focus;
        if let Some(conversation) = self.conversation_mut(focus)
            && conversation.selected_user.is_some()
        {
            conversation.select_older_user();
            return;
        }
        if !self.move_vertical(-1)
            && let Some(conversation) = self.conversation_mut(focus)
        {
            conversation.select_older_user();
        }
    }

    pub(super) fn move_down(&mut self) {
        let focus = self.focus;
        if let Some(conversation) = self.conversation_mut(focus)
            && conversation.selected_user.is_some()
        {
            conversation.select_newer_user_or_composer();
            return;
        }
        let _ = self.move_vertical(1);
    }

    pub(super) fn move_inline_editor_up(&mut self) {
        let _ = self.move_vertical(-1);
    }

    pub(super) fn move_inline_editor_down(&mut self) {
        let _ = self.move_vertical(1);
    }

    fn move_vertical(&mut self, direction: isize) -> bool {
        let layout = ComposerLayout::new(&self.input, self.composer_width);
        let position = layout.cursor_position(&self.input, self.cursor);
        let Some(target_row) = position.row.checked_add_signed(direction) else {
            return false;
        };
        if target_row >= layout.row_count() {
            return false;
        }
        let column = self.preferred_column.unwrap_or(position.column);
        self.cursor = layout.byte_at_column(&self.input, target_row, column);
        self.preferred_column = Some(column);
        true
    }

    pub(super) fn delete_word_before_cursor(&mut self) {
        let mut start = self.cursor;
        while let Some((index, character)) = self.input[..start].char_indices().next_back() {
            if !character.is_whitespace() {
                break;
            }
            start = index;
        }
        while let Some((index, character)) = self.input[..start].char_indices().next_back() {
            if character.is_whitespace() {
                break;
            }
            start = index;
        }
        if start != self.cursor {
            self.prepare_composer_edit();
            self.cursor = self.delete_composer_range(start, self.cursor);
            self.preferred_column = None;
        }
    }

    pub(super) fn delete_to_line_start(&mut self) {
        let line_start = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let start = if line_start == self.cursor && line_start > 0 {
            line_start - 1
        } else {
            line_start
        };
        if start != self.cursor {
            self.prepare_composer_edit();
            self.cursor = self.delete_composer_range(start, self.cursor);
            self.preferred_column = None;
        }
    }

    pub(super) fn delete_to_line_end(&mut self) {
        let line_end = self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |index| self.cursor + index);
        let end = if line_end == self.cursor && line_end < self.input.len() {
            line_end + 1
        } else {
            line_end
        };
        if end != self.cursor {
            self.prepare_composer_edit();
            self.cursor = self.delete_composer_range(self.cursor, end);
            self.preferred_column = None;
        }
    }

    pub(super) fn set_composer_width(&mut self, width: u16) {
        self.composer_width = width.max(1);
    }

    pub(super) fn settle_composer_viewport(
        &mut self,
        cursor_row: usize,
        row_count: usize,
        viewport_height: usize,
    ) {
        let viewport_height = viewport_height.max(1);
        self.composer_scroll = self
            .composer_scroll
            .min(row_count.saturating_sub(viewport_height));
        if cursor_row < self.composer_scroll {
            self.composer_scroll = cursor_row;
        } else if cursor_row >= self.composer_scroll.saturating_add(viewport_height) {
            self.composer_scroll = cursor_row.saturating_add(1).saturating_sub(viewport_height);
        }
    }

    pub(super) fn composer_scroll(&self) -> usize {
        self.composer_scroll
    }

    pub(super) fn replace_input(&mut self, input: String) {
        self.prepare_composer_edit();
        self.input = input;
        self.pending_pastes.clear();
        self.cursor = self.input.len();
        self.preferred_column = None;
        self.synchronize_composer_elements();
    }

    pub(super) fn editor_failed(&mut self, error: impl std::fmt::Display) {
        if let Some(conversation) = self.conversation_mut(self.focus) {
            conversation.status = format!("Editor failed: {error}");
        }
    }

    pub(super) fn clear_input(&mut self) {
        self.input.clear();
        self.local_images.clear();
        self.pending_pastes.clear();
        self.cursor = 0;
        self.preferred_column = None;
    }

    /// Implements Amp's two-stage interrupt gesture. The first Escape arms a
    /// target-scoped confirmation; the second within one second confirms it.
    /// Draft input is preserved while a turn is running.
    pub(super) fn handle_escape(&mut self, now: Instant) -> Option<PaneId> {
        let target = self.focus;
        if !self.is_running(target) {
            self.cancel_confirmation = None;
            self.clear_input();
            return None;
        }

        if self.cancel_confirmation.is_some_and(|confirmation| {
            confirmation.target == target && now <= confirmation.expires_at
        }) {
            self.cancel_confirmation = None;
            return Some(target);
        }

        self.cancel_confirmation = Some(CancelConfirmation {
            target,
            expires_at: now + CANCEL_CONFIRMATION_WINDOW,
        });
        None
    }

    pub(super) fn cancel_confirmation_active(&self) -> bool {
        self.cancel_confirmation
            .is_some_and(|confirmation| confirmation.target == self.focus)
    }

    pub(super) fn take_submission(&mut self) -> Option<SubmittedPrompt> {
        self.synchronize_composer_elements();
        let display = self.expanded_composer_input();
        if display.chars().all(char::is_whitespace) && self.local_images.is_empty() {
            return None;
        }
        self.cursor = 0;
        self.input.clear();
        self.pending_pastes.clear();
        let local_images = std::mem::take(&mut self.local_images)
            .into_iter()
            .map(|image| image.path)
            .collect();
        Some(SubmittedPrompt::new(display, local_images))
    }

    pub(super) fn transcript_selection_active(&self) -> bool {
        self.active_conversation().selected_user.is_some()
    }

    pub(super) fn historical_editor_index(&self) -> Option<usize> {
        self.historical_editor
            .as_ref()
            .map(|editor| editor.transcript_index)
    }

    pub(super) fn historical_editor_active(&self) -> bool {
        self.historical_editor.is_some()
    }

    pub(super) fn start_historical_edit(&mut self) -> bool {
        if self.focus != PaneId::Main
            || self.btw.is_some()
            || self.historical_editor.is_some()
            || self.pending_historical_edit.is_some()
        {
            "Close /btw before editing history".clone_into(&mut self.main.status);
            return false;
        }
        let Some(transcript_index) = self.main.selected_user else {
            return false;
        };
        let Some((prompt_id, prompt)) = self
            .main
            .transcript
            .user_edit_target(transcript_index)
            .map(|(id, prompt)| (id, prompt.to_owned()))
        else {
            return false;
        };
        let composer_draft = self.capture_composer_draft();
        self.restore_composer_draft(ComposerDraft {
            cursor: prompt.len(),
            input: prompt,
            local_images: Vec::new(),
            pending_pastes: Vec::new(),
            scroll: 0,
            preferred_column: None,
        });
        self.historical_editor = Some(HistoricalEditor {
            source_branch_id: self.main_branch_id,
            prompt_id,
            transcript_index,
            composer_draft,
        });
        if !self.main.running {
            "Editing selected prompt".clone_into(&mut self.main.status);
        }
        true
    }

    pub(super) fn cancel_historical_edit(&mut self) {
        let Some(editor) = self.historical_editor.take() else {
            return;
        };
        self.restore_composer_draft(editor.composer_draft);
        self.main.jump_to_bottom();
        if !self.main.running {
            "Ready".clone_into(&mut self.main.status);
        }
    }

    pub(super) fn commit_historical_edit(&mut self) -> Option<HistoricalEditRequest> {
        let prompt = self.expanded_composer_input();
        if prompt.chars().all(char::is_whitespace) {
            "Historical prompt cannot be empty".clone_into(&mut self.main.status);
            return None;
        }
        let editor = self.historical_editor.take()?;
        let prompt = prompt.trim().to_owned();
        self.restore_composer_draft(editor.composer_draft);
        let new_branch_id = self.next_main_branch_id;
        self.next_main_branch_id = self.next_main_branch_id.saturating_add(1);
        let pending = PendingHistoricalEdit {
            source_branch_id: editor.source_branch_id,
            new_branch_id,
            prompt_id: editor.prompt_id,
            transcript_index: editor.transcript_index,
            prompt,
        };
        let request = HistoricalEditRequest {
            source_branch: pending.source_branch_id,
            new_branch: new_branch_id,
            prompt: editor.prompt_id,
        };
        self.pending_historical_edit = Some(pending);
        if !self.main.running {
            "Forking before selected prompt".clone_into(&mut self.main.status);
        }
        Some(request)
    }

    pub(super) fn main_branch_opened(
        &mut self,
        id: u64,
        parent_id: u64,
        prompt_id: u64,
        request_id: Arc<str>,
    ) -> Option<String> {
        let pending = self.pending_historical_edit.take().filter(|pending| {
            pending.new_branch_id == id
                && pending.source_branch_id == parent_id
                && pending.prompt_id == prompt_id
        })?;
        let branch = self.main.fork_before(pending.transcript_index);
        let mut previous = std::mem::replace(&mut self.main, branch);
        if !previous.running {
            "Ready".clone_into(&mut previous.status);
        }
        let draft = self.capture_composer_draft();
        self.main_branches.push(MainBranchPane {
            id: self.main_branch_id,
            parent_id: self.main_branch_parent_id,
            conversation: previous,
            draft,
        });
        self.main_branch_id = id;
        self.main_branch_parent_id = Some(parent_id);
        self.main_branch_request_id = Some(request_id);
        self.restore_composer_draft(ComposerDraft {
            cursor: 0,
            input: String::new(),
            local_images: Vec::new(),
            pending_pastes: Vec::new(),
            scroll: 0,
            preferred_column: None,
        });
        self.main.jump_to_bottom();
        Some(pending.prompt)
    }

    pub(super) fn main_branch_open_failed(&mut self, id: u64, error: &str) {
        if self
            .pending_historical_edit
            .as_ref()
            .is_some_and(|pending| pending.new_branch_id == id)
        {
            let Some(pending) = self.pending_historical_edit.take() else {
                return;
            };
            let composer_draft = self.capture_composer_draft();
            self.restore_composer_draft(ComposerDraft {
                cursor: pending.prompt.len(),
                input: pending.prompt,
                local_images: Vec::new(),
                pending_pastes: Vec::new(),
                scroll: 0,
                preferred_column: None,
            });
            self.historical_editor = Some(HistoricalEditor {
                source_branch_id: pending.source_branch_id,
                prompt_id: pending.prompt_id,
                transcript_index: pending.transcript_index,
                composer_draft,
            });
            self.main.status = format!("Historical edit failed: {error}");
        }
    }

    pub(super) fn cycle_main_branch(&mut self, direction: isize) -> Option<u64> {
        let ids = self
            .branch_previews()
            .into_iter()
            .map(|preview| preview.id)
            .collect::<Vec<_>>();
        let position = ids.iter().position(|id| *id == self.main_branch_id)?;
        let target = (position.cast_signed() + direction)
            .rem_euclid(ids.len().cast_signed())
            .cast_unsigned();
        let id = *ids.get(target)?;
        self.request_main_branch_switch(id)
    }

    pub(super) fn toggle_branch_navigator(&mut self) -> bool {
        if self.branch_navigator.is_some() {
            self.branch_navigator = None;
            return true;
        }
        if self.focus != PaneId::Main
            || self.btw.is_some()
            || self.historical_editor.is_some()
            || self.main_branches.is_empty()
        {
            return false;
        }
        self.main.selected_user = None;
        self.branch_navigator = Some(self.main_branch_id);
        true
    }

    pub(super) fn branch_navigator_active(&self) -> bool {
        self.branch_navigator.is_some()
    }

    pub(super) fn move_branch_navigator(&mut self, direction: isize) {
        let Some(selected) = self.branch_navigator else {
            return;
        };
        let ids = self
            .branch_previews()
            .into_iter()
            .map(|preview| preview.id)
            .collect::<Vec<_>>();
        let Some(position) = ids.iter().position(|id| *id == selected) else {
            return;
        };
        let target = (position.cast_signed() + direction)
            .rem_euclid(ids.len().cast_signed())
            .cast_unsigned();
        self.branch_navigator = ids.get(target).copied();
    }

    pub(super) fn branch_navigator_selected_id(&self) -> Option<u64> {
        self.branch_navigator
    }

    pub(super) fn branch_navigator_conversation_mut(&mut self) -> &mut Conversation {
        let Some(selected) = self.branch_navigator else {
            return &mut self.main;
        };
        if selected == self.main_branch_id {
            return &mut self.main;
        }
        let Some(position) = self
            .main_branches
            .iter()
            .position(|branch| branch.id == selected)
        else {
            return &mut self.main;
        };
        &mut self.main_branches[position].conversation
    }

    pub(super) fn switch_to_navigated_branch(&mut self) -> Option<u64> {
        let id = self.branch_navigator?;
        if id == self.main_branch_id {
            return None;
        }
        self.request_main_branch_switch(id)
    }

    pub(super) fn close_branch_navigator(&mut self) {
        self.branch_navigator = None;
    }

    pub(super) fn branch_previews(&self) -> Vec<BranchPreview<'_>> {
        let selected = self.branch_navigator;
        let mut previews = self
            .main_branches
            .iter()
            .map(|branch| BranchPreview {
                id: branch.id,
                parent_id: branch.parent_id,
                active: false,
                selected: selected == Some(branch.id),
                prompt: branch.conversation.transcript.latest_user_message(),
                tree_prefix: String::new(),
                depth: 0,
            })
            .chain(std::iter::once(BranchPreview {
                id: self.main_branch_id,
                parent_id: self.main_branch_parent_id,
                active: true,
                selected: selected == Some(self.main_branch_id),
                prompt: self.main.transcript.latest_user_message(),
                tree_prefix: String::new(),
                depth: 0,
            }))
            .collect::<Vec<_>>();
        previews.sort_unstable_by_key(|preview| preview.id);
        tree_ordered_previews(previews)
    }

    fn request_main_branch_switch(&mut self, id: u64) -> Option<u64> {
        if id == self.main_branch_id
            || self.focus != PaneId::Main
            || self.main.running
            || self.main.pending_turns > 0
            || self.btw.is_some()
            || self.historical_editor.is_some()
            || self.pending_branch_switch.is_some()
        {
            return None;
        }
        self.pending_branch_switch = Some(id);
        self.main.status = format!("Switching to branch {id}");
        Some(id)
    }

    pub(super) fn main_branch_switched(&mut self, id: u64, request_id: Arc<str>) {
        if self.pending_branch_switch != Some(id) {
            return;
        }
        self.pending_branch_switch = None;
        let Some(position) = self.main_branches.iter().position(|branch| branch.id == id) else {
            return;
        };
        let requested = self.main_branches.swap_remove(position);
        let mut previous = std::mem::replace(&mut self.main, requested.conversation);
        "Ready".clone_into(&mut previous.status);
        let previous_draft = self.capture_composer_draft();
        self.restore_composer_draft(requested.draft);
        self.main_branches.push(MainBranchPane {
            id: self.main_branch_id,
            parent_id: self.main_branch_parent_id,
            conversation: previous,
            draft: previous_draft,
        });
        self.main_branch_id = requested.id;
        self.main_branch_parent_id = requested.parent_id;
        self.main_branch_request_id = Some(request_id);
        self.main.jump_to_bottom();
    }

    fn capture_composer_draft(&self) -> ComposerDraft {
        ComposerDraft {
            input: self.input.clone(),
            local_images: self.local_images.clone(),
            pending_pastes: self.pending_pastes.clone(),
            cursor: self.cursor,
            scroll: self.composer_scroll,
            preferred_column: self.preferred_column,
        }
    }

    fn restore_composer_draft(&mut self, draft: ComposerDraft) {
        self.input = draft.input;
        self.local_images = draft.local_images;
        self.pending_pastes = draft.pending_pastes;
        self.cursor = draft.cursor.min(self.input.len());
        self.composer_scroll = draft.scroll;
        self.preferred_column = draft.preferred_column;
    }

    pub(super) fn main_branch_switch_failed(&mut self, id: u64, error: &str) {
        if self.pending_branch_switch == Some(id) {
            self.pending_branch_switch = None;
            self.main.status = format!("Branch switch failed: {error}");
        }
    }

    pub(super) fn main_branch_request_id(&self) -> Option<&str> {
        self.main_branch_request_id.as_deref()
    }

    pub(super) fn main_branch_graph(&self) -> String {
        let mut branches = self
            .main_branches
            .iter()
            .map(|branch| (branch.id, branch.parent_id))
            .chain(std::iter::once((
                self.main_branch_id,
                self.main_branch_parent_id,
            )))
            .collect::<Vec<_>>();
        branches.sort_unstable_by_key(|(id, _)| *id);
        branches
            .into_iter()
            .map(|(id, parent)| {
                let active = if id == self.main_branch_id { "*" } else { "" };
                parent.map_or_else(
                    || format!("{id}{active}"),
                    |parent| format!("{id}{active}←{parent}"),
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(super) fn dismiss_transcript_selection(&mut self) {
        let focus = self.focus;
        if let Some(conversation) = self.conversation_mut(focus) {
            conversation.jump_to_bottom();
        }
    }

    pub(super) fn begin_btw(&mut self) -> u64 {
        let id = self.next_btw_id;
        self.next_btw_id = self.next_btw_id.saturating_add(1);
        self.btw = Some(BtwPane {
            id,
            request_id: None,
            conversation: Conversation::new("Forking latest checkpoint"),
        });
        self.focus = PaneId::Btw(id);
        id
    }

    pub(super) fn btw_id(&self) -> Option<u64> {
        self.btw.as_ref().map(|btw| btw.id)
    }

    pub(super) fn btw_busy(&self) -> bool {
        self.btw
            .as_ref()
            .is_some_and(|btw| btw.conversation.running || btw.conversation.pending_turns > 0)
    }

    pub(super) fn reject_btw_close_while_busy(&mut self) {
        if let Some(btw) = self.btw.as_mut() {
            btw.conversation.push_output(TranscriptItem::Error(
                "BTW has an active or queued turn; wait for it to finish before /close".to_owned(),
            ));
            "BTW still running".clone_into(&mut btw.conversation.status);
        }
    }

    pub(super) fn focus_btw(&mut self) {
        if let Some(id) = self.btw_id() {
            self.focus = PaneId::Btw(id);
        }
    }

    pub(super) fn toggle_focus(&mut self) {
        self.focus = match (self.focus, self.btw_id()) {
            (PaneId::Main, Some(id)) => PaneId::Btw(id),
            (PaneId::Btw(_), _) | (PaneId::Main, None) => PaneId::Main,
        };
    }

    pub(super) fn close_btw(&mut self, id: u64) {
        if self.btw_id() == Some(id) {
            if self
                .cancel_confirmation
                .is_some_and(|confirmation| confirmation.target == PaneId::Btw(id))
            {
                self.cancel_confirmation = None;
            }
            self.btw = None;
            self.focus = PaneId::Main;
        }
    }

    pub(super) fn btw_opened(&mut self, id: u64, request_id: Arc<str>) {
        if let Some(btw) = self.btw.as_mut().filter(|btw| btw.id == id) {
            btw.request_id = Some(request_id);
            btw.conversation.status = if btw.conversation.pending_turns == 0 {
                "Ready".to_owned()
            } else {
                "Starting".to_owned()
            };
        }
    }

    pub(super) fn btw_failed(&mut self, id: u64, error: String) {
        if let Some(conversation) = self.conversation_mut(PaneId::Btw(id)) {
            conversation.push_output(TranscriptItem::Error(error));
            conversation.pending_turns = 0;
            conversation.queued_prompts.clear();
            conversation.queued_prompt_ids.clear();
            conversation.displayed_queued_prompt = None;
            conversation.pending_steers.clear();
            conversation.applied_steer_runs_waiting_for_ack.clear();
            conversation.running = false;
            "Fork failed".clone_into(&mut conversation.status);
        }
    }

    pub(super) fn queue_prompt(&mut self, target: PaneId, prompt: String) -> Option<u64> {
        let id = self.next_input_id;
        self.next_input_id = self.next_input_id.saturating_add(1);
        let conversation = self.conversation_mut(target)?;
        conversation.queue_prompt(id, prompt);
        Some(id)
    }

    pub(super) fn queue_steer(&mut self, target: PaneId, prompt: String) -> Option<u64> {
        self.conversation(target)?;
        let id = self.next_input_id;
        self.next_input_id = self.next_input_id.saturating_add(1);
        self.conversation_mut(target)?.queue_steer(id, prompt);
        Some(id)
    }

    pub(super) fn steer_admitted(&mut self, target: PaneId, id: u64) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.steer_admitted(id);
        }
    }

    pub(super) fn steer_queued(&mut self, target: PaneId, id: u64, prompt: String) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.steer_queued(id, prompt);
        }
    }

    pub(super) fn steer_failed(&mut self, target: PaneId, id: u64, error: String) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.steer_failed(id, error);
        }
    }

    pub(super) fn cancel_pending(&mut self, target: PaneId) {
        if self
            .cancel_confirmation
            .is_some_and(|confirmation| confirmation.target == target)
        {
            self.cancel_confirmation = None;
        }
        if let Some(conversation) = self.conversation_mut(target) {
            "Cancelling".clone_into(&mut conversation.status);
        }
    }

    pub(super) fn cancel_accepted(&mut self, target: PaneId) {
        if let Some(conversation) = self.conversation_mut(target) {
            // RunFailed is the authoritative lifecycle event. Avoid overwriting
            // a queued turn's RunStarted state if it has already arrived.
            if conversation.status == "Cancelling" {
                "Cancellation accepted".clone_into(&mut conversation.status);
            }
        }
    }

    pub(super) fn cancel_failed(&mut self, target: PaneId, error: String) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.push_output(TranscriptItem::Error(error));
            conversation.status = if conversation.running {
                "Working".to_owned()
            } else {
                "Ready".to_owned()
            };
        }
    }

    pub(super) fn is_running(&self, target: PaneId) -> bool {
        self.conversation(target)
            .is_some_and(|conversation| conversation.running)
    }

    pub(super) fn has_input(&self) -> bool {
        !self.input.chars().all(char::is_whitespace)
    }

    pub(super) fn turn_finished(
        &mut self,
        target: PaneId,
        main_branch_id: Option<u64>,
        error: Option<String>,
    ) {
        match target {
            PaneId::Main => {
                let branch_id = main_branch_id.unwrap_or(self.main_branch_id);
                let conversation = if self.main_branch_id == branch_id {
                    Some(&mut self.main)
                } else {
                    self.main_branches
                        .iter_mut()
                        .find(|branch| branch.id == branch_id)
                        .map(|branch| &mut branch.conversation)
                };
                if let Some(conversation) = conversation {
                    conversation.turn_finished(error);
                }
            }
            PaneId::Btw(_) => {
                if let Some(conversation) = self.conversation_mut(target) {
                    conversation.turn_finished(error);
                }
            }
        }
    }

    pub(super) fn on_agent_event(&mut self, target: PaneId, event: &AgentEvent) -> bool {
        let updated = self
            .conversation_mut(target)
            .is_some_and(|conversation| conversation.on_agent_event(event));
        if matches!(
            event.kind,
            AgentEventKind::RunCompleted | AgentEventKind::RunFailed
        ) && self
            .cancel_confirmation
            .is_some_and(|confirmation| confirmation.target == target)
        {
            self.cancel_confirmation = None;
        }
        updated
    }

    pub(super) fn on_main_agent_event(&mut self, branch_id: u64, event: &AgentEvent) -> bool {
        if self.main_branch_id == branch_id {
            return self.on_agent_event(PaneId::Main, event);
        }
        self.main_branches
            .iter_mut()
            .find(|branch| branch.id == branch_id)
            .is_some_and(|branch| branch.conversation.on_agent_event(event))
    }

    pub(super) fn main_branch_event_stream_closed(&mut self, branch_id: u64) {
        let conversation = if self.main_branch_id == branch_id {
            Some(&mut self.main)
        } else {
            self.main_branches
                .iter_mut()
                .find(|branch| branch.id == branch_id)
                .map(|branch| &mut branch.conversation)
        };
        if let Some(conversation) = conversation {
            conversation.push_output(TranscriptItem::Error(
                "branch event stream closed".to_owned(),
            ));
            conversation.running = false;
            "Agent stopped".clone_into(&mut conversation.status);
        }
    }

    pub(super) fn scroll_up(&mut self, rows: usize) {
        self.scroll_up_in(self.focus, rows);
    }

    pub(super) fn scroll_up_in(&mut self, target: PaneId, rows: usize) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.scroll_up(rows);
        }
    }

    pub(super) fn scroll_down(&mut self, rows: usize) {
        self.scroll_down_in(self.focus, rows);
    }

    pub(super) fn scroll_down_in(&mut self, target: PaneId, rows: usize) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.scroll_down(rows);
            if conversation.scroll_from_bottom == 0 {
                conversation.has_unseen_output = false;
            }
        }
    }

    pub(super) fn jump_to_bottom(&mut self) {
        if let Some(conversation) = self.conversation_mut(self.focus) {
            conversation.jump_to_bottom();
        }
    }

    pub(super) fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.expire_cancel_confirmation(Instant::now());
    }

    pub(super) fn begin_mouse_selection(&mut self, position: Position) -> bool {
        let changed = self.screen_selection.begin(position);
        if self.screen_selection.is_active() {
            self.main.scroll_up(0);
            if let Some(btw) = &mut self.btw {
                btw.conversation.scroll_up(0);
            }
        }
        changed
    }

    pub(super) fn drag_mouse_selection(&mut self, position: Position) -> bool {
        self.screen_selection.drag(position)
    }

    pub(super) fn finish_mouse_selection(&mut self, position: Position) -> bool {
        self.screen_selection.finish(position)
    }

    pub(super) fn clear_mouse_selection(&mut self) -> bool {
        self.screen_selection.clear()
    }

    pub(super) fn mouse_selection_intersects(&self, area: Rect) -> bool {
        self.screen_selection.intersects(area)
    }

    pub(super) fn render_mouse_selection(&mut self, buffer: &mut Buffer, areas: &[Rect]) {
        self.screen_selection.render(buffer, areas);
    }

    pub(super) fn take_pending_copy(&mut self) -> Option<String> {
        self.screen_selection.take_pending_copy()
    }

    pub(super) fn advance_smooth_scroll(&mut self) {
        self.main.advance_smooth_scroll();
        if let Some(btw) = &mut self.btw {
            btw.conversation.advance_smooth_scroll();
        }
    }

    pub(super) fn smooth_scroll_pending(&self) -> bool {
        self.main.smooth_scroll_pending()
            || self
                .btw
                .as_ref()
                .is_some_and(|btw| btw.conversation.smooth_scroll_pending())
    }

    pub(super) fn active_conversation(&self) -> &Conversation {
        self.conversation(self.focus).unwrap_or(&self.main)
    }

    pub(super) fn set_active_status(&mut self, status: impl Into<String>) {
        if let Some(conversation) = self.conversation_mut(self.focus) {
            conversation.status = status.into();
        }
    }

    pub(super) fn push_active_error(&mut self, error: impl Into<String>) {
        if let Some(conversation) = self.conversation_mut(self.focus) {
            conversation.push_output(TranscriptItem::Error(error.into()));
            "Trace unavailable".clone_into(&mut conversation.status);
            conversation.jump_to_bottom();
        }
    }

    fn conversation(&self, target: PaneId) -> Option<&Conversation> {
        match target {
            PaneId::Main => Some(&self.main),
            PaneId::Btw(id) => self
                .btw
                .as_ref()
                .filter(|btw| btw.id == id)
                .map(|btw| &btw.conversation),
        }
    }

    fn conversation_mut(&mut self, target: PaneId) -> Option<&mut Conversation> {
        match target {
            PaneId::Main => Some(&mut self.main),
            PaneId::Btw(id) => self
                .btw
                .as_mut()
                .filter(|btw| btw.id == id)
                .map(|btw| &mut btw.conversation),
        }
    }

    fn prepare_composer_edit(&mut self) {
        if self.historical_editor.is_none() && self.transcript_selection_active() {
            self.dismiss_transcript_selection();
        }
    }

    pub(super) fn attach_local_image(&mut self, path: PathBuf) {
        self.prepare_composer_edit();
        self.synchronize_composer_elements();
        self.snap_cursor_out_of_pending_paste();
        let placeholder = format!("[Image #{}]", self.local_images.len() + 1);
        self.input.insert_str(self.cursor, &placeholder);
        self.cursor += placeholder.len();
        self.local_images.push(AttachedImage { placeholder, path });
        self.preferred_column = None;
    }

    fn synchronize_composer_elements(&mut self) {
        self.local_images
            .retain(|image| self.input.contains(&image.placeholder));
        let active = resolved_pending_pastes(&self.input, &self.pending_pastes)
            .into_iter()
            .map(|(_, _, index)| index)
            .collect::<HashSet<_>>();
        let mut index = 0_usize;
        self.pending_pastes.retain(|_| {
            let retain = active.contains(&index);
            index = index.saturating_add(1);
            retain
        });
    }

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix = 0_usize;
        for paste in &self.pending_pastes {
            if paste.placeholder == base {
                max_suffix = max_suffix.max(1);
            } else if let Some(suffix) = paste.placeholder.strip_prefix(&prefix)
                && let Ok(suffix) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(suffix);
            }
        }
        if max_suffix == 0 {
            base
        } else {
            format!("{base} #{}", max_suffix + 1)
        }
    }

    fn pending_paste_containing(&self, position: usize) -> Option<(usize, usize)> {
        resolved_pending_pastes(&self.input, &self.pending_pastes)
            .into_iter()
            .find_map(|(start, end, _)| {
                (start < position && position < end).then_some((start, end))
            })
    }

    fn snap_cursor_out_of_pending_paste(&mut self) {
        if let Some((_, end)) = self.pending_paste_containing(self.cursor) {
            self.cursor = end;
        }
    }

    fn delete_composer_range(&mut self, mut start: usize, mut end: usize) -> usize {
        for (paste_start, paste_end, _) in
            resolved_pending_pastes(&self.input, &self.pending_pastes)
        {
            if start < paste_end && end > paste_start {
                start = start.min(paste_start);
                end = end.max(paste_end);
            }
        }
        self.input.drain(start..end);
        self.synchronize_composer_elements();
        start
    }

    fn expanded_composer_input(&self) -> String {
        let expansions = resolved_pending_pastes(&self.input, &self.pending_pastes);
        if expansions.is_empty() {
            return self.input.clone();
        }
        let extra = expansions
            .iter()
            .fold(0_usize, |extra, (start, end, index)| {
                extra.saturating_add(
                    self.pending_pastes[*index]
                        .text
                        .len()
                        .saturating_sub(end.saturating_sub(*start)),
                )
            });
        let mut output = String::with_capacity(self.input.len().saturating_add(extra));
        let mut cursor = 0_usize;
        for (start, end, index) in expansions {
            output.push_str(&self.input[cursor..start]);
            output.push_str(&self.pending_pastes[index].text);
            cursor = end;
        }
        output.push_str(&self.input[cursor..]);
        output
    }

    fn expire_cancel_confirmation(&mut self, now: Instant) {
        if self
            .cancel_confirmation
            .is_some_and(|confirmation| confirmation.expires_at < now)
        {
            self.cancel_confirmation = None;
        }
    }
}

fn resolved_pending_pastes(input: &str, pastes: &[PendingPaste]) -> Vec<(usize, usize, usize)> {
    let mut candidates = pastes
        .iter()
        .enumerate()
        .flat_map(|(index, paste)| {
            input
                .match_indices(&paste.placeholder)
                .map(move |(start, placeholder)| (start, start + placeholder.len(), index))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let mut used = HashSet::with_capacity(pastes.len());
    let mut resolved = Vec::with_capacity(pastes.len());
    let mut cursor = 0_usize;
    for candidate @ (start, end, index) in candidates {
        if start < cursor || !used.insert(index) {
            continue;
        }
        resolved.push(candidate);
        cursor = end;
    }
    resolved
}

fn normalize_pasted_path(pasted: &str) -> Option<PathBuf> {
    let mut paths = shlex::split(pasted.trim())?;
    if paths.len() != 1 {
        return None;
    }
    let path = paths.pop()?;
    if path.starts_with("file://") {
        return reqwest::Url::parse(&path).ok()?.to_file_path().ok();
    }
    Some(PathBuf::from(path))
}

#[derive(Deserialize)]
struct TextPayload {
    text: String,
}

#[derive(Deserialize)]
struct ErrorPayload {
    message: String,
}

#[derive(Deserialize)]
struct TerminalPayload {
    status: String,
}

#[derive(Deserialize)]
struct ToolCallPayload {
    call_id: String,
    tool: String,
    arguments: Value,
}

#[derive(Deserialize)]
struct ToolResultPayload {
    call_id: String,
    #[serde(default)]
    tool: Option<String>,
    status: String,
    #[serde(default)]
    duration_ns: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
}

fn summarize_tool_arguments(tool: &str, arguments: &Value) -> String {
    if tool == "exec"
        && let Some(source) = arguments.as_str()
    {
        return bounded_multiline_tool_text(source);
    }
    if let Some(object) = arguments.as_object() {
        if tool == "write_stdin"
            && let Some(session_id) = object.get("session_id")
        {
            return format!("session {session_id}");
        }
        let preferred = match tool {
            "exec_command" => object.get("cmd").and_then(Value::as_str),
            "view_image" => object.get("path").and_then(Value::as_str),
            "read_file" => object
                .get("path")
                .or_else(|| object.get("file_path"))
                .and_then(Value::as_str),
            "wait" => object.get("cell_id").and_then(Value::as_str),
            _ => None,
        };
        if let Some(preferred) = preferred {
            if tool == "exec_command" && preferred.contains('\n') {
                return bounded_multiline_tool_text(preferred);
            }
            return compact_tool_text(preferred);
        }
    }
    if tool == "apply_patch"
        && let Some(patch) = arguments.as_str()
    {
        return bounded_multiline_text(patch, MAX_PATCH_ARGUMENT_CHARS, MAX_PATCH_ARGUMENT_LINES);
    }
    compact_arguments(arguments)
}

fn summarize_tool_result(tool: Option<&str>, result: &Value, status: ToolStatus) -> String {
    if tool == Some("exec_command") {
        let decoded = result
            .as_str()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or_else(|| result.clone());
        if let Some(object) = decoded.as_object() {
            let mut parts = Vec::new();
            if let Some(exit_code) = object.get("exit_code").and_then(Value::as_i64) {
                parts.push(format!("exit {exit_code}"));
            }
            if let Some(output) = object.get("output").and_then(Value::as_str) {
                let lines = output.lines().count();
                if lines > 0 {
                    parts.push(format!("{lines} line{}", if lines == 1 { "" } else { "s" }));
                }
            }
            if !parts.is_empty() {
                return parts.join(" · ");
            }
        }
    }
    if tool == Some("apply_patch")
        && result
            .as_str()
            .is_some_and(|result| result.contains("Success"))
    {
        return "applied".to_owned();
    }
    if matches!(status, ToolStatus::Failed | ToolStatus::Cancelled) {
        return compact_arguments(result);
    }
    String::new()
}

fn compact_arguments(arguments: &Value) -> String {
    let value = match arguments {
        Value::String(value) => value.clone(),
        _ => arguments.to_string(),
    };
    compact_tool_text(&value)
}

fn compact_tool_text(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= MAX_TOOL_ARGUMENT_CHARS {
        return value;
    }
    let mut output: String = value.chars().take(MAX_TOOL_ARGUMENT_CHARS).collect();
    output.push('…');
    output
}

fn bounded_multiline_tool_text(value: &str) -> String {
    bounded_multiline_text(
        value,
        MAX_MULTILINE_TOOL_ARGUMENT_CHARS,
        MAX_MULTILINE_TOOL_ARGUMENT_LINES,
    )
}

fn bounded_multiline_text(value: &str, max_chars: usize, max_lines: usize) -> String {
    let mut output = String::new();
    let mut characters = 0_usize;
    for (index, line) in value.trim().lines().enumerate() {
        if index >= max_lines {
            output.push_str("\n…");
            break;
        }
        if index > 0 {
            output.push('\n');
        }
        for character in line.chars() {
            if characters >= max_chars {
                output.push('…');
                return output;
            }
            output.push(character);
            characters = characters.saturating_add(1);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use nanocodex::{AgentEvent, AgentEventKind, PromptInput, UserInput};
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
    use serde_json::{Value, json};

    use super::{App, PaneId, smooth_scroll_drain, summarize_tool_arguments};
    use crate::tui::transcript::TranscriptItem;

    fn event(kind: AgentEventKind, payload: &Value) -> AgentEvent {
        serde_json::from_value(json!({
            "protocol_version": 1,
            "request_id": "test",
            "seq": 1,
            "type": kind,
            "payload": payload,
        }))
        .unwrap()
    }

    #[test]
    fn code_mode_and_multiline_shell_arguments_preserve_line_structure() {
        let code = "const tasks = inputs.map(run);\nawait Promise.all(tasks);";
        assert_eq!(summarize_tool_arguments("exec", &json!(code)), code);
        assert_eq!(
            summarize_tool_arguments(
                "exec_command",
                &json!({ "cmd": "cargo test \\\n  --workspace" }),
            ),
            "cargo test \\\n  --workspace"
        );
    }

    #[test]
    fn pasted_image_path_becomes_a_typed_image_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dragged image.png");
        image::RgbaImage::new(1, 1).save(&path).unwrap();
        let mut app = App::new(directory.path().to_path_buf());

        app.handle_paste(&format!("'{}'", path.display()));

        assert_eq!(app.input, "[Image #1] ");
        let submission = app.take_submission().unwrap();
        assert_eq!(submission.display(), "[Image #1] ");
        let PromptInput::Content(content) = submission.into_prompt().instruction else {
            panic!("image submission should use typed content");
        };
        assert!(matches!(
            &content[..],
            [
                UserInput::LocalImage {
                    path: submitted_path,
                    detail: None,
                },
                UserInput::Text { text },
            ] if submitted_path == &path && text == "[Image #1] "
        ));
    }

    #[test]
    fn pasted_file_url_becomes_an_image_attachment() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("url-image.png");
        image::RgbaImage::new(1, 1).save(&path).unwrap();
        let url = reqwest::Url::from_file_path(&path).unwrap();
        let mut app = App::new(directory.path().to_path_buf());

        app.handle_paste(url.as_str());

        assert_eq!(app.input, "[Image #1] ");
    }

    #[test]
    fn pasted_non_image_path_remains_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.txt");
        std::fs::write(&path, "not an image").unwrap();
        let pasted = format!("'{}'", path.display());
        let mut app = App::new(directory.path().to_path_buf());

        app.handle_paste(&pasted);

        assert_eq!(app.input, pasted);
        let submission = app.take_submission().unwrap();
        assert!(matches!(
            submission.into_prompt().instruction,
            PromptInput::Text(text) if text == pasted
        ));
    }

    #[test]
    fn large_paste_uses_codex_placeholder_and_expands_on_submit() {
        let pasted = "界".repeat(1_001);
        let mut app = App::new(".".into());

        app.handle_paste(&pasted);

        assert_eq!(app.input, "[Pasted Content 1001 chars]");
        assert_eq!(app.pending_pastes.len(), 1);
        let submission = app.take_submission().expect("large paste submission");
        assert_eq!(submission.display(), pasted);
        assert!(app.pending_pastes.is_empty());
    }

    #[test]
    fn paste_at_threshold_stays_directly_editable() {
        let pasted = "x".repeat(1_000);
        let mut app = App::new(".".into());

        app.handle_paste(&pasted);

        assert_eq!(app.input, pasted);
        assert!(app.pending_pastes.is_empty());
    }

    #[test]
    fn whitespace_only_large_paste_is_not_submitted() {
        let mut app = App::new(".".into());
        app.handle_paste(&" ".repeat(1_001));

        assert!(app.take_submission().is_none());
        assert_eq!(app.input, "[Pasted Content 1001 chars]");
        assert_eq!(app.pending_pastes.len(), 1);
    }

    #[test]
    fn equal_sized_large_pastes_get_distinct_placeholders_and_expand_in_order() {
        let first = "a".repeat(1_001);
        let second = "b".repeat(1_001);
        let mut app = App::new(".".into());

        app.handle_paste(&first);
        app.insert_char(' ');
        app.handle_paste(&second);

        assert_eq!(
            app.input,
            "[Pasted Content 1001 chars] [Pasted Content 1001 chars] #2"
        );
        assert_eq!(
            app.take_submission().expect("two pastes").display(),
            format!("{first} {second}")
        );
    }

    #[test]
    fn editing_or_deleting_a_large_paste_treats_its_placeholder_atomically() {
        let mut app = App::new(".".into());
        app.handle_paste(&"x".repeat(1_001));
        let placeholder_len = app.input.len();

        app.cursor = placeholder_len / 2;
        app.insert_char('!');
        assert_eq!(app.cursor, placeholder_len + 1);
        assert!(app.input.ends_with('!'));

        app.cursor = placeholder_len;
        app.backspace();
        assert_eq!(app.input, "!");
        assert!(app.pending_pastes.is_empty());
    }

    #[test]
    fn btw_conversation_isolated_and_focus_toggles() {
        let mut app = App::new(".".into());
        assert_eq!(app.focus, PaneId::Main);
        let id = app.begin_btw();
        assert_eq!(app.focus, PaneId::Btw(id));
        assert!(
            app.queue_prompt(PaneId::Btw(id), "side question".to_owned())
                .is_some()
        );
        assert_eq!(app.main.pending_turns, 0);
        assert_eq!(app.btw.as_ref().unwrap().conversation.pending_turns, 1);
        app.toggle_focus();
        assert_eq!(app.focus, PaneId::Main);
        app.toggle_focus();
        assert_eq!(app.focus, PaneId::Btw(id));
        assert!(app.btw_busy());
        app.turn_finished(PaneId::Btw(id), None, None);
        assert!(!app.btw_busy());
        app.close_btw(id);
        assert_eq!(app.focus, PaneId::Main);
        assert!(app.btw.is_none());
    }

    #[test]
    fn submitted_prompt_is_selectable_before_run_started_without_clearing_the_view() {
        let mut app = App::new(".".into());
        app.main
            .transcript
            .push_editable_user("older prompt".to_owned(), 41);
        app.main
            .transcript
            .push(TranscriptItem::Assistant("older answer".to_owned()));

        let prompt_id = app
            .queue_prompt(PaneId::Main, "fix this typo".to_owned())
            .unwrap();
        app.move_up();

        assert_eq!(app.main.selected_user, Some(2));
        assert_eq!(
            app.main.transcript.user_edit_target(2),
            Some((prompt_id, "fix this typo"))
        );
        let area = Rect::new(0, 0, 30, 6);
        let mut buffer = Buffer::empty(area);
        app.main
            .transcript
            .widget(0, app.main.selected_user, None, "empty")
            .render(area, &mut buffer);
        assert!(buffer.content.iter().any(|cell| cell.symbol() == "f"));

        app.main.on_agent_event(&event(
            AgentEventKind::RunStarted,
            &json!({ "status": "started" }),
        ));
        assert_eq!(
            app.main.transcript.len(),
            3,
            "prompt must not be duplicated"
        );
        assert_eq!(app.main.selected_user, Some(2));
    }

    #[test]
    fn prompt_queued_behind_a_running_turn_stays_out_of_the_transcript() {
        let mut app = App::new(".".into());
        app.main.running = true;
        app.main
            .transcript
            .push(TranscriptItem::Assistant("current answer".to_owned()));

        let _ = app.queue_prompt(PaneId::Main, "queued follow-up".to_owned());

        assert_eq!(app.main.transcript.len(), 1);
        assert_eq!(
            app.main.queued_prompts.front().map(String::as_str),
            Some("queued follow-up")
        );
    }

    #[test]
    fn streamed_wrap_growth_preserves_a_scrolled_view_and_marks_output_unseen() {
        let mut app = App::new(".".into());
        app.main.settle_viewport(20, 6);
        for index in 0..8 {
            app.main
                .push_output(TranscriptItem::User(format!("earlier message {index}")));
        }
        app.main.push_assistant_delta("streaming tail");
        app.main.settle_viewport(20, 6);
        app.main.scroll_from_bottom = 5;

        let area = Rect::new(0, 0, 20, 6);
        let mut before = Buffer::empty(area);
        app.main
            .transcript
            .widget(app.main.scroll_from_bottom, None, None, "empty")
            .render(area, &mut before);
        let old_offset = app.main.scroll_from_bottom;

        app.main.push_assistant_delta(
            " with enough additional words to wrap onto several newly visible rows",
        );
        app.main.settle_viewport(20, 6);

        let mut after = Buffer::empty(area);
        app.main
            .transcript
            .widget(app.main.scroll_from_bottom, None, None, "empty")
            .render(area, &mut after);
        assert!(app.main.scroll_from_bottom > old_offset);
        assert!(app.main.has_unseen_output);
        assert_eq!(after, before);
    }

    #[test]
    fn reasoning_wrap_growth_preserves_a_scrolled_view() {
        let mut app = App::new(".".into());
        app.main.settle_viewport(20, 6);
        for index in 0..8 {
            app.main
                .push_output(TranscriptItem::User(format!("earlier message {index}")));
        }
        app.main.push_reasoning_delta("streaming thought");
        app.main.settle_viewport(20, 6);
        app.main.scroll_from_bottom = 5;

        let area = Rect::new(0, 0, 20, 6);
        let mut before = Buffer::empty(area);
        app.main
            .transcript
            .widget(app.main.scroll_from_bottom, None, None, "empty")
            .render(area, &mut before);

        app.main.push_reasoning_delta(
            " with enough additional words to wrap onto several newly visible rows",
        );
        app.main.settle_viewport(20, 6);

        let mut after = Buffer::empty(area);
        app.main
            .transcript
            .widget(app.main.scroll_from_bottom, None, None, "empty")
            .render(area, &mut after);
        assert!(app.main.has_unseen_output);
        assert_eq!(after, before);
    }

    #[test]
    fn new_entries_anchor_only_the_conversation_that_receives_them() {
        let mut app = App::new(".".into());
        let btw_id = app.begin_btw();
        app.main.settle_viewport(40, 10);
        app.btw
            .as_mut()
            .unwrap()
            .conversation
            .settle_viewport(40, 10);
        app.main.scroll_from_bottom = 9;
        app.btw.as_mut().unwrap().conversation.scroll_from_bottom = 7;

        app.on_agent_event(
            PaneId::Btw(btw_id),
            &event(
                AgentEventKind::ToolCall,
                &json!({ "call_id": "side-1", "tool": "exec", "arguments": "pwd" }),
            ),
        );
        app.btw
            .as_mut()
            .unwrap()
            .conversation
            .settle_viewport(40, 10);

        assert_eq!(app.main.scroll_from_bottom, 9);
        assert!(!app.main.has_unseen_output);
        let btw = &app.btw.as_ref().unwrap().conversation;
        assert!(btw.scroll_from_bottom > 7);
        assert!(btw.has_unseen_output);
    }

    #[test]
    fn page_down_and_jump_to_bottom_clear_unseen_output_at_the_tail() {
        let mut app = App::new(".".into());
        app.main.scroll_from_bottom = 15;
        app.main.has_unseen_output = true;

        app.scroll_down(12);
        assert_eq!(app.main.scroll_from_bottom, 3);
        assert!(app.main.has_unseen_output);
        app.scroll_down(12);
        assert_eq!(app.main.scroll_from_bottom, 0);
        assert!(!app.main.has_unseen_output);

        app.main.scroll_from_bottom = 4;
        app.main.has_unseen_output = true;
        app.jump_to_bottom();
        assert_eq!(app.main.scroll_from_bottom, 0);
        assert!(!app.main.has_unseen_output);
    }

    #[test]
    fn repeated_scroll_up_clamps_without_hidden_overscroll() {
        let mut app = App::new(".".into());
        for index in 0..12 {
            app.main
                .push_output(TranscriptItem::User(format!("message {index}")));
        }
        app.main.settle_viewport(20, 6);
        let limit = app.main.transcript.max_scroll_from_bottom(20, 6);

        for _ in 0..100 {
            app.scroll_up(3);
        }
        assert_eq!(app.main.scroll_from_bottom, limit);

        app.scroll_down(3);
        assert_eq!(
            app.main.scroll_from_bottom,
            limit.saturating_sub(3),
            "scrolling down should move immediately after reaching the top",
        );
    }

    #[test]
    fn follow_bottom_only_animates_after_content_overflows_the_viewport() {
        let mut app = App::new(".".into());
        app.main.settle_viewport(20, 6);

        app.main.push_assistant_delta("one\ntwo\nthree");
        app.main.settle_viewport(20, 6);
        assert_eq!(app.main.display_scroll_from_bottom(), 0);
        assert!(!app.smooth_scroll_pending());

        app.main.push_assistant_delta("\nfour\nfive\nsix");
        app.main.settle_viewport(20, 6);
        let first_frame_scroll = app.main.display_scroll_from_bottom();
        assert!(first_frame_scroll > 0);
        assert!(app.smooth_scroll_pending());

        app.advance_smooth_scroll();
        assert_eq!(
            app.main.display_scroll_from_bottom(),
            first_frame_scroll - 1
        );
    }

    #[test]
    fn follow_bottom_catches_up_when_a_burst_exceeds_the_smooth_backlog() {
        let mut app = App::new(".".into());
        app.main.settle_viewport(20, 6);
        app.main.push_assistant_delta("one\ntwo\nthree");
        app.main.settle_viewport(20, 6);

        let overflow_before = app.main.transcript.height_from(0, 20).saturating_sub(6);
        app.main
            .push_assistant_delta(&format!("\n{}", "new row\n".repeat(100)));
        app.main.settle_viewport(20, 6);
        let overflow_after = app.main.transcript.height_from(0, 20).saturating_sub(6);

        assert_eq!(
            app.main.display_scroll_from_bottom(),
            overflow_after.saturating_sub(overflow_before)
        );

        let pending = overflow_after.saturating_sub(overflow_before);
        let drained = smooth_scroll_drain(pending);
        app.advance_smooth_scroll();
        assert_eq!(
            app.main.display_scroll_from_bottom(),
            pending.saturating_sub(drained)
        );
        assert!(drained > 1);
    }

    #[test]
    fn smooth_scroll_drain_preserves_small_steps_and_bounds_catch_up() {
        assert_eq!(smooth_scroll_drain(0), 0);
        assert_eq!(smooth_scroll_drain(8), 1);
        assert_eq!(smooth_scroll_drain(9), 2);
        assert_eq!(smooth_scroll_drain(40), 32);
        assert_eq!(smooth_scroll_drain(1_000), 32);
    }

    #[test]
    fn manual_scroll_takes_over_from_the_current_animated_position() {
        let mut app = App::new(".".into());
        app.main.settle_viewport(20, 6);
        app.main.push_assistant_delta("one\ntwo\nthree");
        app.main.settle_viewport(20, 6);
        app.main.push_assistant_delta("\nfour\nfive\nsix");
        app.main.settle_viewport(20, 6);
        let animated_position = app.main.display_scroll_from_bottom();
        let max_scroll = app.main.transcript.max_scroll_from_bottom(20, 6);

        app.scroll_up(3);

        assert_eq!(
            app.main.scroll_from_bottom,
            animated_position.saturating_add(3).min(max_scroll)
        );
        assert_eq!(app.main.smooth_scroll_from_bottom, 0);
    }

    #[test]
    fn reversing_manual_scroll_discards_an_unsettled_automatic_anchor() {
        let mut app = App::new(".".into());
        app.main.settle_viewport(20, 6);
        app.main.push_assistant_delta("one\ntwo\nthree");
        app.main.settle_viewport(20, 6);
        app.main
            .push_assistant_delta("\nfour\nfive\nsix\nseven\neight");
        app.main.settle_viewport(20, 6);
        assert!(app.smooth_scroll_pending());

        app.main.push_assistant_delta("\nnine\nten\neleven");
        assert!(app.main.pending_scroll_anchor.is_some());
        app.scroll_down(usize::MAX);
        app.scroll_up(3);
        let manual_position = app.main.display_scroll_from_bottom();

        app.main.settle_viewport(20, 6);

        assert_eq!(app.main.display_scroll_from_bottom(), manual_position);
        assert_eq!(app.main.smooth_scroll_from_bottom, 0);
        assert!(app.main.pending_scroll_anchor.is_none());
    }

    #[test]
    fn vertical_motion_uses_visual_rows_and_preserves_the_preferred_column() {
        let mut app = App::new(".".into());
        app.input = "012345\nxy\nabcdef".to_owned();
        app.cursor = 4;
        app.set_composer_width(20);

        app.move_down();
        assert_eq!(app.cursor, 9);
        app.move_down();
        assert_eq!(app.cursor, 14);
        app.move_up();
        assert_eq!(app.cursor, 9);
        app.move_up();
        assert_eq!(app.cursor, 4);

        app.replace_input("abcdefghij".to_owned());
        app.cursor = 2;
        app.set_composer_width(4);
        app.move_down();
        assert_eq!(app.cursor, 6);
    }

    #[test]
    fn transcript_history_starts_above_the_first_visual_row_without_replacing_the_draft() {
        let mut app = App::new(".".into());
        app.main
            .transcript
            .push_editable_user("older prompt".to_owned(), 1);
        app.main
            .transcript
            .push(TranscriptItem::Assistant("older answer".to_owned()));
        app.main
            .transcript
            .push_editable_user("newer prompt".to_owned(), 2);
        app.main
            .transcript
            .push(TranscriptItem::Assistant("newer answer".to_owned()));
        app.main
            .transcript
            .push(TranscriptItem::User("applied steer".to_owned()));
        app.main
            .transcript
            .push(TranscriptItem::Assistant("steer answer".to_owned()));
        app.input = "first row\nsecond row".to_owned();
        app.cursor = app.input.len();
        app.set_composer_width(20);

        app.move_up();
        assert_eq!(app.input, "first row\nsecond row");
        assert_eq!(app.cursor, "first row".len());
        assert!(!app.transcript_selection_active());

        app.move_up();
        assert_eq!(app.input, "first row\nsecond row");
        assert_eq!(
            app.main
                .transcript
                .user_message(app.main.selected_user.unwrap()),
            Some("newer prompt")
        );

        app.move_up();
        assert_eq!(
            app.main
                .transcript
                .user_message(app.main.selected_user.unwrap()),
            Some("older prompt")
        );
        let oldest = app.main.selected_user;
        app.move_up();
        assert_eq!(app.main.selected_user, oldest, "history must not wrap");

        app.move_down();
        assert_eq!(
            app.main
                .transcript
                .user_message(app.main.selected_user.unwrap()),
            Some("newer prompt")
        );
        app.move_down();
        assert!(!app.transcript_selection_active());
        assert_eq!(app.input, "first row\nsecond row");
        assert_eq!(app.cursor, "first row".len());
    }

    #[test]
    fn historical_edit_switches_to_a_prefixed_branch_and_preserves_branch_drafts() {
        let mut app = App::new(".".into());
        app.main
            .transcript
            .push(TranscriptItem::Assistant("inherited answer".to_owned()));
        app.main
            .transcript
            .push_editable_user("prompt to revise".to_owned(), 41);
        app.main
            .transcript
            .push(TranscriptItem::Assistant("abandoned answer".to_owned()));
        app.input = "unsent draft".to_owned();

        app.move_up();
        assert!(app.transcript_selection_active());
        assert!(app.start_historical_edit());
        assert_eq!(app.input, "prompt to revise");
        assert_eq!(app.cursor, app.input.len());
        assert!(app.historical_editor_active());
        app.replace_input("revised prompt".to_owned());
        let request = app.commit_historical_edit().unwrap();
        let prompt = app
            .main_branch_opened(
                request.new_branch,
                request.source_branch,
                request.prompt,
                Arc::from("branch-session"),
            )
            .unwrap();

        assert_eq!(prompt, "revised prompt");
        assert!(app.input.is_empty());
        assert!(!app.transcript_selection_active());
        assert_eq!(app.main.transcript.len(), 1);
        assert_eq!(app.main_branch_id, 1);
        assert_eq!(app.main_branch_graph(), "0 1*←0");
        assert_eq!(app.main.scroll_from_bottom, 0);

        app.replace_input("branch draft".to_owned());
        assert_eq!(app.cycle_main_branch(1), Some(0));
        app.main_branch_switched(0, Arc::from("root-session"));
        assert_eq!(app.input, "unsent draft");
        assert_eq!(app.main_branch_graph(), "0* 1←0");

        assert_eq!(app.cycle_main_branch(1), Some(1));
        app.main_branch_switched(1, Arc::from("branch-session"));
        assert_eq!(app.input, "branch draft");
    }

    #[test]
    fn historical_edit_archives_a_running_source_without_misrouting_its_completion() {
        let mut app = App::new(".".into());
        app.main
            .transcript
            .push_editable_user("prompt still running".to_owned(), 41);
        app.main.running = true;
        app.main.pending_turns = 1;
        app.main.status = "Thinking".to_owned();

        app.move_up();
        assert!(app.start_historical_edit());
        app.replace_input("revised prompt".to_owned());
        let request = app.commit_historical_edit().unwrap();
        let _ = app.main_branch_opened(
            request.new_branch,
            request.source_branch,
            request.prompt,
            Arc::from("branch-session"),
        );

        let source = &app.main_branches[0].conversation;
        assert!(source.running);
        assert_eq!(source.pending_turns, 1);
        assert_eq!(source.status, "Thinking");

        app.main.pending_turns = 1;
        app.turn_finished(PaneId::Main, Some(0), None);
        assert_eq!(app.main.pending_turns, 1);
        assert_eq!(app.main_branches[0].conversation.pending_turns, 0);
    }

    #[test]
    fn branch_previews_follow_depth_first_tree_order() {
        let mut app = App::new(".".into());
        app.main
            .transcript
            .push_editable_user("root prompt".to_owned(), 41);

        app.move_up();
        assert!(app.start_historical_edit());
        let first = app.commit_historical_edit().unwrap();
        let _ = app.main_branch_opened(
            first.new_branch,
            first.source_branch,
            first.prompt,
            Arc::from("branch-1"),
        );

        assert_eq!(app.cycle_main_branch(1), Some(0));
        app.main_branch_switched(0, Arc::from("root"));
        app.move_up();
        assert!(app.start_historical_edit());
        let sibling = app.commit_historical_edit().unwrap();
        let _ = app.main_branch_opened(
            sibling.new_branch,
            sibling.source_branch,
            sibling.prompt,
            Arc::from("branch-2"),
        );

        assert_eq!(app.cycle_main_branch(-1), Some(1));
        app.main_branch_switched(1, Arc::from("branch-1"));
        app.main
            .transcript
            .push_editable_user("nested prompt".to_owned(), 42);
        app.move_up();
        assert!(app.start_historical_edit());
        let nested = app.commit_historical_edit().unwrap();
        let _ = app.main_branch_opened(
            nested.new_branch,
            nested.source_branch,
            nested.prompt,
            Arc::from("branch-3"),
        );

        assert!(app.toggle_branch_navigator());
        let previews = app.branch_previews();
        assert_eq!(
            previews
                .iter()
                .map(|preview| preview.id)
                .collect::<Vec<_>>(),
            vec![0, 1, 3, 2]
        );
        assert_eq!(previews[0].tree_prefix, "");
        assert_eq!(previews[1].tree_prefix, "├─");
        assert_eq!(previews[2].tree_prefix, "│ └─");
        assert_eq!(previews[3].tree_prefix, "└─");
    }

    #[test]
    fn readline_deletions_edit_only_the_expected_span() {
        let mut app = App::new(".".into());
        app.input = "alpha beta  \ngamma delta".to_owned();
        app.cursor = "alpha beta  \ngamma".len();

        app.delete_word_before_cursor();
        assert_eq!(app.input, "alpha beta  \n delta");
        assert_eq!(app.cursor, "alpha beta  \n".len());

        app.cursor = "alpha beta".len();
        app.delete_to_line_start();
        assert_eq!(app.input, "  \n delta");
        assert_eq!(app.cursor, 0);

        app.delete_to_line_end();
        assert_eq!(app.input, "\n delta");

        app.delete_to_line_end();
        assert_eq!(app.input, " delta");

        app.input = "first\nsecond".to_owned();
        app.cursor = "first\n".len();
        app.delete_to_line_start();
        assert_eq!(app.input, "firstsecond");
        assert_eq!(app.cursor, "first".len());
    }

    #[test]
    fn resize_retains_tail_distance_and_uses_the_new_width_for_later_growth() {
        let mut app = App::new(".".into());
        for index in 0..8 {
            app.main
                .push_output(TranscriptItem::User(format!("earlier message {index}")));
        }
        app.main.settle_viewport(40, 6);
        app.main.push_assistant_delta("short tail");
        app.main.settle_viewport(40, 6);
        app.main.scroll_from_bottom = 6;

        app.main.settle_viewport(10, 6);
        assert_eq!(app.main.scroll_from_bottom, 6);
        app.main
            .push_assistant_delta(" plus enough text to wrap at the narrower width");
        app.main.settle_viewport(10, 6);

        assert!(app.main.scroll_from_bottom > 6);
        assert!(app.main.has_unseen_output);
    }

    #[test]
    fn stale_btw_updates_do_not_reach_a_reopened_pane() {
        let mut app = App::new(".".into());
        let first = app.begin_btw();
        app.close_btw(first);
        let second = app.begin_btw();
        app.btw_failed(first, "stale".to_owned());
        assert_eq!(app.btw_id(), Some(second));
        assert!(app.btw.as_ref().unwrap().conversation.transcript.is_empty());
    }

    #[test]
    fn accepted_steers_and_queued_turns_have_distinct_lifecycles() {
        let mut app = App::new(".".into());
        app.main.running = true;

        let steer_id = app
            .queue_steer(PaneId::Main, "narrow the search".to_owned())
            .unwrap();
        assert!(
            app.queue_prompt(PaneId::Main, "then summarize".to_owned())
                .is_some()
        );
        assert_eq!(app.main.pending_steers.len(), 1);
        assert_eq!(app.main.queued_prompts.len(), 1);
        assert_eq!(app.main.pending_turns, 1);

        app.steer_admitted(PaneId::Main, steer_id);
        assert_eq!(app.main.pending_steers.len(), 1);
        assert!(app.main.transcript.is_empty());
        assert_eq!(app.main.status, "Steer pending");

        app.main.on_agent_event(&event(
            AgentEventKind::RunSteered,
            &json!({ "steer_index": 1, "instruction_bytes": 17 }),
        ));
        assert!(app.main.pending_steers.is_empty());
        assert_eq!(app.main.transcript.len(), 1);
        assert_eq!(app.main.status, "Steer applied");
        assert_eq!(app.main.queued_prompts.len(), 1);
        assert_eq!(app.main.pending_turns, 1);
    }

    #[test]
    fn run_steered_waits_for_a_racing_queue_ack_before_promoting_input() {
        let mut app = App::new(".".into());
        app.main.running = true;
        let steer_id = app
            .queue_steer(PaneId::Main, "race-safe steer".to_owned())
            .unwrap();

        app.main.on_agent_event(&event(
            AgentEventKind::RunSteered,
            &json!({ "steer_index": 1, "instruction_bytes": 15 }),
        ));
        assert_eq!(app.main.pending_steers.len(), 1);
        assert!(app.main.transcript.is_empty());

        app.steer_admitted(PaneId::Main, steer_id);
        assert!(app.main.pending_steers.is_empty());
        assert_eq!(app.main.transcript.len(), 1);
        assert_eq!(app.main.status, "Steer applied");
    }

    #[test]
    fn steer_rejected_after_turn_completion_becomes_the_next_turn() {
        let mut app = App::new(".".into());
        app.main.running = true;
        let steer_id = app
            .queue_steer(PaneId::Main, "one more constraint".to_owned())
            .unwrap();

        app.main.running = false;
        app.steer_queued(PaneId::Main, steer_id, "one more constraint".to_owned());

        assert!(app.main.pending_steers.is_empty());
        assert_eq!(
            app.main.queued_prompts.front().map(String::as_str),
            Some("one more constraint")
        );
        assert_eq!(app.main.pending_turns, 1);
    }

    #[test]
    fn late_cancel_ack_does_not_overwrite_the_next_turn_state() {
        let mut app = App::new(".".into());
        app.main.running = true;
        "Thinking".clone_into(&mut app.main.status);

        app.cancel_accepted(PaneId::Main);

        assert!(app.main.running);
        assert_eq!(app.main.status, "Thinking");
    }

    #[test]
    fn cancelled_turn_is_terminal_without_rendering_a_generic_error() {
        let mut app = App::new(".".into());
        assert!(
            app.queue_prompt(PaneId::Main, "run it".to_owned())
                .is_some()
        );
        app.main.on_agent_event(&event(
            AgentEventKind::RunStarted,
            &json!({ "status": "started" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::ToolCall,
            &json!({ "call_id": "call-1", "tool": "exec", "arguments": "sleep 30" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::ToolResult,
            &json!({ "call_id": "call-1", "status": "cancelled" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::RunError,
            &json!({ "message": "turn cancelled" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::RunFailed,
            &json!({ "status": "cancelled" }),
        ));

        assert!(!app.main.running);
        assert_eq!(app.main.status, "Cancelled");
        assert_eq!(app.main.transcript.len(), 2);
    }

    #[test]
    fn reasoning_summary_deltas_are_visible_while_the_turn_is_running() {
        let mut app = App::new(".".into());
        app.main.on_agent_event(&event(
            AgentEventKind::RunStarted,
            &json!({ "status": "started" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::ReasoningSummaryDelta,
            &json!({ "model_call_index": 0, "text": "Inspecting the request path" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::ReasoningSummaryDelta,
            &json!({ "model_call_index": 0, "text": " and event ordering" }),
        ));

        assert_eq!(app.main.status, "Thinking...");
        assert_eq!(app.main.transcript.len(), 1);

        let area = Rect::new(0, 0, 50, 4);
        let mut buffer = Buffer::empty(area);
        app.main
            .transcript
            .widget(0, None, None, "empty")
            .render(area, &mut buffer);
        let rendered = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Inspecting the request path and event ordering"));
    }

    #[test]
    fn reasoning_resumes_in_a_new_inline_block_after_a_tool() {
        let mut app = App::new(".".into());
        app.main.on_agent_event(&event(
            AgentEventKind::ReasoningSummaryDelta,
            &json!({ "model_call_index": 0, "text": "First thought" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::ToolCall,
            &json!({ "call_id": "call-1", "tool": "exec", "arguments": "pwd" }),
        ));
        app.main.on_agent_event(&event(
            AgentEventKind::ReasoningSummaryDelta,
            &json!({ "model_call_index": 1, "text": "Second thought" }),
        ));

        assert_eq!(app.main.transcript.len(), 3);
        assert_eq!(app.main.status, "Thinking...");

        let area = Rect::new(0, 0, 40, 12);
        let mut buffer = Buffer::empty(area);
        app.main
            .transcript
            .widget(0, None, None, "empty")
            .render(area, &mut buffer);
        let rendered = buffer
            .content
            .chunks(usize::from(area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let first = rendered.find("• First thought").unwrap();
        let tool = rendered.find("◌ Code Mode").unwrap();
        let second = rendered.find("• Second thought").unwrap();
        assert!(first < tool && tool < second);
    }

    #[test]
    fn escape_requires_confirmation_and_preserves_the_draft() {
        let now = Instant::now();
        let mut app = App::new(".".into());
        app.main.running = true;
        app.input = "keep this draft".to_owned();
        app.cursor = app.input.len();

        assert_eq!(app.handle_escape(now), None);
        assert!(app.cancel_confirmation_active());
        assert_eq!(app.input, "keep this draft");

        assert_eq!(
            app.handle_escape(now + Duration::from_millis(999)),
            Some(PaneId::Main)
        );
        assert!(!app.cancel_confirmation_active());
        assert_eq!(app.input, "keep this draft");
    }

    #[test]
    fn expired_escape_confirmation_rearms_instead_of_cancelling() {
        let now = Instant::now();
        let mut app = App::new(".".into());
        app.main.running = true;

        assert_eq!(app.handle_escape(now), None);
        app.expire_cancel_confirmation(now + Duration::from_millis(1_001));
        assert!(!app.cancel_confirmation_active());
        assert_eq!(app.handle_escape(now + Duration::from_millis(1_002)), None);
        assert!(app.cancel_confirmation_active());
    }
}
