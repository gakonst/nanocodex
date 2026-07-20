use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

use nanocodex::{AgentEvent, AgentEventKind};
use serde::Deserialize;
use serde_json::Value;

use super::transcript::{ToolStatus, Transcript, TranscriptItem};

const MAX_REASONING_STATUS_CHARS: usize = 160;
const MAX_TOOL_ARGUMENT_CHARS: usize = 180;
const CANCEL_CONFIRMATION_WINDOW: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PaneId {
    Main,
    Btw(u64),
}

pub(super) struct Conversation {
    pub(super) transcript: Transcript,
    pub(super) pending_turns: usize,
    pub(super) running: bool,
    pub(super) status: String,
    pub(super) scroll_from_bottom: usize,
    streamed_this_turn: bool,
    reasoning: String,
    pending_run_error: Option<String>,
    pub(super) queued_prompts: VecDeque<String>,
    pub(super) pending_steers: VecDeque<String>,
}

impl Conversation {
    fn new(status: impl Into<String>) -> Self {
        Self {
            transcript: Transcript::default(),
            pending_turns: 0,
            running: false,
            status: status.into(),
            scroll_from_bottom: 0,
            streamed_this_turn: false,
            reasoning: String::new(),
            pending_run_error: None,
            queued_prompts: VecDeque::new(),
            pending_steers: VecDeque::new(),
        }
    }

    fn queue_prompt(&mut self, prompt: String) {
        self.queued_prompts.push_back(prompt);
        self.pending_turns += 1;
        self.status = if self.running {
            "Prompt queued".to_owned()
        } else {
            "Starting".to_owned()
        };
        self.scroll_from_bottom = 0;
    }

    fn queue_steer(&mut self, prompt: String) {
        self.pending_steers.push_back(prompt);
        "Steer pending".clone_into(&mut self.status);
        self.scroll_from_bottom = 0;
    }

    fn steer_accepted(&mut self, prompt: String) {
        drop(self.pending_steers.pop_front());
        self.transcript.push(TranscriptItem::User(prompt));
        self.status = if self.running {
            "Steer accepted".to_owned()
        } else {
            "Ready".to_owned()
        };
    }

    fn steer_queued(&mut self, prompt: String) {
        drop(self.pending_steers.pop_front());
        self.queue_prompt(prompt);
    }

    fn steer_failed(&mut self, error: String) {
        drop(self.pending_steers.pop_front());
        self.transcript.push(TranscriptItem::Error(error));
        self.status = if self.running {
            "Working".to_owned()
        } else {
            "Ready".to_owned()
        };
    }

    fn turn_finished(&mut self, error: Option<String>) {
        self.pending_turns = self.pending_turns.saturating_sub(1);
        if let Some(error) = error {
            self.transcript.push(TranscriptItem::Error(error));
        }
    }

    fn on_agent_event(&mut self, event: &AgentEvent) -> bool {
        match event.kind {
            AgentEventKind::RunStarted => {
                if let Some(prompt) = self.queued_prompts.pop_front() {
                    self.transcript.push(TranscriptItem::User(prompt));
                }
                self.running = true;
                self.streamed_this_turn = false;
                self.reasoning.clear();
                self.pending_run_error = None;
                "Thinking".clone_into(&mut self.status);
            }
            AgentEventKind::RunSteered => {
                "Working".clone_into(&mut self.status);
            }
            AgentEventKind::AssistantDelta => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    self.push_assistant_delta(&payload.text);
                }
            }
            AgentEventKind::AssistantMessage => {
                if let Ok(payload) = event.decode_payload::<TextPayload>()
                    && !self.streamed_this_turn
                {
                    self.transcript
                        .push(TranscriptItem::Assistant(payload.text));
                }
            }
            AgentEventKind::ReasoningSummaryDelta => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    self.reasoning.push_str(&payload.text);
                    self.status = reasoning_tail(&self.reasoning);
                }
            }
            AgentEventKind::ToolCall => {
                if let Ok(payload) = event.decode_payload::<ToolCallPayload>() {
                    let arguments = compact_arguments(&payload.arguments);
                    self.status = format!("Running {}", payload.tool);
                    self.transcript.push(TranscriptItem::Tool {
                        call_id: payload.call_id,
                        name: payload.tool,
                        arguments,
                        status: ToolStatus::Running,
                    });
                }
            }
            AgentEventKind::ToolResult => {
                if let Ok(payload) = event.decode_payload::<ToolResultPayload>() {
                    let status = match payload.status.as_str() {
                        "completed" => ToolStatus::Completed,
                        "cancelled" => ToolStatus::Cancelled,
                        _ => ToolStatus::Failed,
                    };
                    self.transcript.set_tool_status(&payload.call_id, status);
                    "Working".clone_into(&mut self.status);
                }
            }
            AgentEventKind::RunError => {
                if let Ok(payload) = event.decode_payload::<ErrorPayload>() {
                    self.pending_run_error = Some(payload.message);
                }
            }
            AgentEventKind::RunCompleted => {
                if let Some(error) = self.pending_run_error.take() {
                    self.transcript.push(TranscriptItem::Error(error));
                }
                self.running = false;
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

    fn run_failed(&mut self, event: &AgentEvent) {
        self.running = false;
        let cancelled = event
            .decode_payload::<TerminalPayload>()
            .is_ok_and(|payload| payload.status == "cancelled");
        if cancelled {
            self.pending_run_error = None;
            "Cancelled".clone_into(&mut self.status);
        } else {
            if let Some(error) = self.pending_run_error.take() {
                self.transcript.push(TranscriptItem::Error(error));
            }
            "Turn failed".clone_into(&mut self.status);
        }
    }

    fn push_assistant_delta(&mut self, delta: &str) {
        let append_to_current = self.streamed_this_turn;
        self.streamed_this_turn = true;
        if !append_to_current || !self.transcript.append_assistant_delta(delta) {
            self.transcript
                .push(TranscriptItem::Assistant(delta.to_owned()));
        }
    }
}

pub(super) struct BtwPane {
    pub(super) id: u64,
    pub(super) conversation: Conversation,
}

pub(super) struct App {
    pub(super) cwd: PathBuf,
    pub(super) main: Conversation,
    pub(super) btw: Option<BtwPane>,
    pub(super) focus: PaneId,
    pub(super) input: String,
    pub(super) cursor: usize,
    pub(super) frame: usize,
    history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    next_btw_id: u64,
    cancel_confirmation: Option<CancelConfirmation>,
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
            btw: None,
            focus: PaneId::Main,
            input: String::new(),
            cursor: 0,
            frame: 0,
            history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            next_btw_id: 1,
            cancel_confirmation: None,
        }
    }

    pub(super) fn insert_char(&mut self, character: char) {
        self.detach_history();
        self.input.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    pub(super) fn insert_str(&mut self, text: &str) {
        self.detach_history();
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.detach_history();
        let previous = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        self.input.drain(previous..self.cursor);
        self.cursor = previous;
    }

    pub(super) fn delete(&mut self) {
        if self.cursor == self.input.len() {
            return;
        }
        self.detach_history();
        let next = self.input[self.cursor..]
            .chars()
            .next()
            .map_or(self.input.len(), |character| {
                self.cursor + character.len_utf8()
            });
        self.input.drain(self.cursor..next);
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    pub(super) fn move_right(&mut self) {
        if let Some(character) = self.input[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |index| self.cursor + index);
    }

    pub(super) fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.history_cursor = None;
        self.history_draft.clear();
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

    pub(super) fn previous_history(&mut self) {
        if self.history.is_empty() || self.input.contains('\n') {
            return;
        }
        let index = if let Some(index) = self.history_cursor {
            index.saturating_sub(1)
        } else {
            self.history_draft.clone_from(&self.input);
            self.history.len() - 1
        };
        self.history_cursor = Some(index);
        self.input.clone_from(&self.history[index]);
        self.cursor = self.input.len();
    }

    pub(super) fn next_history(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_cursor = Some(index + 1);
            self.input.clone_from(&self.history[index + 1]);
        } else {
            self.history_cursor = None;
            self.input.clone_from(&self.history_draft);
            self.history_draft.clear();
        }
        self.cursor = self.input.len();
    }

    pub(super) fn take_submission(&mut self) -> Option<String> {
        if self.input.chars().all(char::is_whitespace) {
            return None;
        }
        self.cursor = 0;
        let prompt = std::mem::take(&mut self.input);
        self.history.push(prompt.clone());
        self.history_cursor = None;
        self.history_draft.clear();
        Some(prompt)
    }

    pub(super) fn begin_btw(&mut self) -> u64 {
        let id = self.next_btw_id;
        self.next_btw_id = self.next_btw_id.saturating_add(1);
        self.btw = Some(BtwPane {
            id,
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
            btw.conversation.transcript.push(TranscriptItem::Error(
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

    pub(super) fn btw_opened(&mut self, id: u64) {
        if let Some(conversation) = self.conversation_mut(PaneId::Btw(id)) {
            conversation.status = if conversation.pending_turns == 0 {
                "Ready".to_owned()
            } else {
                "Starting".to_owned()
            };
        }
    }

    pub(super) fn btw_failed(&mut self, id: u64, error: String) {
        if let Some(conversation) = self.conversation_mut(PaneId::Btw(id)) {
            conversation.transcript.push(TranscriptItem::Error(error));
            conversation.pending_turns = 0;
            conversation.queued_prompts.clear();
            conversation.pending_steers.clear();
            conversation.running = false;
            "Fork failed".clone_into(&mut conversation.status);
        }
    }

    pub(super) fn queue_prompt(&mut self, target: PaneId, prompt: String) -> bool {
        let Some(conversation) = self.conversation_mut(target) else {
            return false;
        };
        conversation.queue_prompt(prompt);
        true
    }

    pub(super) fn queue_steer(&mut self, target: PaneId, prompt: String) -> bool {
        let Some(conversation) = self.conversation_mut(target) else {
            return false;
        };
        conversation.queue_steer(prompt);
        true
    }

    pub(super) fn steer_accepted(&mut self, target: PaneId, prompt: String) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.steer_accepted(prompt);
        }
    }

    pub(super) fn steer_queued(&mut self, target: PaneId, prompt: String) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.steer_queued(prompt);
        }
    }

    pub(super) fn steer_failed(&mut self, target: PaneId, error: String) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.steer_failed(error);
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
            conversation.transcript.push(TranscriptItem::Error(error));
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

    pub(super) fn turn_finished(&mut self, target: PaneId, error: Option<String>) {
        if let Some(conversation) = self.conversation_mut(target) {
            conversation.turn_finished(error);
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

    pub(super) fn scroll_up(&mut self, rows: usize) {
        if let Some(conversation) = self.conversation_mut(self.focus) {
            conversation.scroll_from_bottom = conversation.scroll_from_bottom.saturating_add(rows);
        }
    }

    pub(super) fn scroll_down(&mut self, rows: usize) {
        if let Some(conversation) = self.conversation_mut(self.focus) {
            conversation.scroll_from_bottom = conversation.scroll_from_bottom.saturating_sub(rows);
        }
    }

    pub(super) fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.expire_cancel_confirmation(Instant::now());
    }

    pub(super) fn active_conversation(&self) -> &Conversation {
        self.conversation(self.focus).unwrap_or(&self.main)
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

    fn detach_history(&mut self) {
        self.history_cursor = None;
        self.history_draft.clear();
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
    status: String,
}

fn compact_arguments(arguments: &Value) -> String {
    let value = match arguments {
        Value::String(value) => value.clone(),
        _ => arguments.to_string(),
    };
    if value.chars().count() <= MAX_TOOL_ARGUMENT_CHARS {
        return value;
    }
    let mut output: String = value.chars().take(MAX_TOOL_ARGUMENT_CHARS).collect();
    output.push('…');
    output
}

fn reasoning_tail(reasoning: &str) -> String {
    let compact = reasoning.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = compact.chars().count();
    if count <= MAX_REASONING_STATUS_CHARS {
        return compact;
    }
    let mut tail: String = compact
        .chars()
        .skip(count - MAX_REASONING_STATUS_CHARS)
        .collect();
    tail.insert(0, '…');
    tail
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use nanocodex::{AgentEvent, AgentEventKind};
    use serde_json::{Value, json};

    use super::{App, PaneId};

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
    fn btw_conversation_isolated_and_focus_toggles() {
        let mut app = App::new(".".into());
        assert_eq!(app.focus, PaneId::Main);
        let id = app.begin_btw();
        assert_eq!(app.focus, PaneId::Btw(id));
        assert!(app.queue_prompt(PaneId::Btw(id), "side question".to_owned()));
        assert_eq!(app.main.pending_turns, 0);
        assert_eq!(app.btw.as_ref().unwrap().conversation.pending_turns, 1);
        app.toggle_focus();
        assert_eq!(app.focus, PaneId::Main);
        app.toggle_focus();
        assert_eq!(app.focus, PaneId::Btw(id));
        assert!(app.btw_busy());
        app.turn_finished(PaneId::Btw(id), None);
        assert!(!app.btw_busy());
        app.close_btw(id);
        assert_eq!(app.focus, PaneId::Main);
        assert!(app.btw.is_none());
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

        assert!(app.queue_steer(PaneId::Main, "narrow the search".to_owned()));
        assert!(app.queue_prompt(PaneId::Main, "then summarize".to_owned()));
        assert_eq!(app.main.pending_steers.len(), 1);
        assert_eq!(app.main.queued_prompts.len(), 1);
        assert_eq!(app.main.pending_turns, 1);

        app.steer_accepted(PaneId::Main, "narrow the search".to_owned());
        assert!(app.main.pending_steers.is_empty());
        assert_eq!(app.main.queued_prompts.len(), 1);
        assert_eq!(app.main.pending_turns, 1);
    }

    #[test]
    fn steer_rejected_after_turn_completion_becomes_the_next_turn() {
        let mut app = App::new(".".into());
        app.main.running = true;
        assert!(app.queue_steer(PaneId::Main, "one more constraint".to_owned()));

        app.main.running = false;
        app.steer_queued(PaneId::Main, "one more constraint".to_owned());

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
        assert!(app.queue_prompt(PaneId::Main, "run it".to_owned()));
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
