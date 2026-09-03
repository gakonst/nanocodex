// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use super::{
    DirectedMessageEntry, EntryId, EntryKind, MessageDelivery, MessagePhase, SessionStarted,
    ShellId, ToolEntry, ToolState, TranscriptEntry, TranscriptRecord, TransientStatus,
    code_mode_output_text,
};
use crate::{config::ReasoningEffort, tui::format::humanize_tool};
use nanocodex::{
    agent::events::{
        AssistantDelta, AssistantMessage, CompactionCompleted, CompactionFailed,
        ReasoningSummaryDelta, RunError,
    },
    oai::responses::MessagePhase as AgentMessagePhase,
};
use nanocodex_subagents::{
    AgentMessageUpdate, MessageDeliveryState, MessageDisposition, MessageSender, ThreadId,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};

const MAX_RETAINED_MESSAGE_THREADS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventVisibility {
    Persistent,
    Transient,
    StateOnly,
    ErrorFallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodeCellTerminal {
    Completed,
    Terminated,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelChange {
    pub(crate) changed: bool,
    pub(crate) removed: Option<EntryId>,
}

#[derive(Default)]
pub(crate) struct TranscriptModel {
    entries: Vec<TranscriptEntry>,
    entry_indices: HashMap<EntryId, usize>,
    next_entry_id: usize,
    assistants: HashMap<AssistantKey, EntryId>,
    active_assistants: HashMap<(u32, MessagePhase), EntryId>,
    reasoning: HashMap<ReasoningKey, EntryId>,
    tools: HashMap<String, EntryId>,
    shell_sessions: HashMap<ShellSessionKey, EntryId>,
    shell_followups: HashMap<String, EntryId>,
    code_children: HashMap<EntryId, Vec<EntryId>>,
    code_cells: HashMap<String, EntryId>,
    local_shells: HashMap<ShellId, EntryId>,
    message_threads: HashMap<ThreadId, EntryId>,
    message_order: VecDeque<ThreadId>,
    running_tools: HashSet<EntryId>,
    active_runs: usize,
    run_started_at_unix_ms: VecDeque<u64>,
    transient: Option<TransientStatus>,
    pending_error: Option<String>,
    pending_compaction_error: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AssistantKey {
    call: u32,
    item: Option<String>,
    phase: MessagePhase,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ReasoningKey {
    request: Option<Arc<str>>,
    call: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ShellSessionKey {
    environment: Option<String>,
    session_id: i64,
}

impl ShellSessionKey {
    fn from_arguments(arguments: &Value, session_id: i64) -> Self {
        Self {
            environment: arguments
                .get("environment")
                .and_then(Value::as_str)
                .map(str::to_owned),
            session_id,
        }
    }
}

impl TranscriptModel {
    /// Copies the latest stable visual history without carrying live projection state.
    pub(crate) fn fork_snapshot(&self) -> Self {
        let end = if self.is_active() {
            self.entries
                .iter()
                .rposition(|entry| matches!(entry.kind, EntryKind::User { .. }))
                .unwrap_or(self.entries.len())
        } else {
            self.entries.len()
        };
        let entries = self.entries[..end]
            .iter()
            .filter(|entry| match &entry.kind {
                EntryKind::Assistant { complete, .. } => *complete,
                EntryKind::Tool(tool) => tool.state != ToolState::Running,
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        let entry_indices = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        let message_threads = entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::DirectedMessage(message) => Some((message.thread.id, entry.id)),
                _ => None,
            })
            .collect();
        let message_order = entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::DirectedMessage(message) => Some(message.thread.id),
                _ => None,
            })
            .collect();
        Self {
            entries,
            entry_indices,
            next_entry_id: self.next_entry_id,
            message_threads,
            message_order,
            ..Self::default()
        }
    }

    pub(crate) fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    pub(crate) fn entry(&self, id: EntryId) -> Option<&TranscriptEntry> {
        self.index_of(id).and_then(|index| self.entries.get(index))
    }

    pub(crate) fn index_of(&self, id: EntryId) -> Option<usize> {
        self.entry_indices.get(&id).copied()
    }

    pub(crate) fn transient(&self) -> Option<&TransientStatus> {
        self.transient.as_ref()
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active_runs > 0
    }

    pub(crate) fn has_running_tools(&self) -> bool {
        !self.running_tools.is_empty()
    }

    pub(crate) fn running_tool_ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        self.running_tools.iter().copied()
    }

    pub(crate) fn apply(&mut self, record: &TranscriptRecord) -> ModelChange {
        if record.source() == "tact" {
            return self.apply_local(record);
        }
        if record.source() != "agent" {
            return ModelChange::default();
        }
        self.apply_agent(record)
    }

    pub(crate) fn apply_message(
        &mut self,
        perspective: MessageSender,
        update: AgentMessageUpdate,
    ) -> ModelChange {
        let Some(id) = self.message_threads.get(&update.thread.id).copied() else {
            let thread_id = update.thread.id;
            let id = self.push(EntryKind::DirectedMessage(DirectedMessageEntry {
                perspective,
                thread: update.thread,
                deliveries: vec![MessageDelivery {
                    message_id: update.message_id,
                    state: update.delivery,
                }],
            }));
            self.message_threads.insert(thread_id, id);
            self.message_order.push_back(thread_id);
            return ModelChange {
                changed: true,
                removed: self.trim_message_history(),
            };
        };

        let Some(index) = self.index_of(id) else {
            return ModelChange::default();
        };
        let EntryKind::DirectedMessage(message) = &self.entries[index].kind else {
            return ModelChange::default();
        };
        let previous_delivery = message
            .deliveries
            .iter()
            .find(|delivery| delivery.message_id == update.message_id);
        let changed = message.thread != update.thread
            || previous_delivery
                .is_none_or(|delivery| delivery_advances(&delivery.state, &update.delivery));
        if !changed {
            return ModelChange::default();
        }

        self.reasoning.clear();
        let EntryKind::DirectedMessage(message) = &mut self.entries[index].kind else {
            return ModelChange::default();
        };
        message.thread = update.thread;
        message.deliveries.retain(|delivery| {
            message
                .thread
                .messages
                .iter()
                .any(|retained| retained.id == delivery.message_id)
        });
        let delivery = message
            .deliveries
            .iter_mut()
            .find(|delivery| delivery.message_id == update.message_id);
        match delivery {
            Some(delivery) if delivery_advances(&delivery.state, &update.delivery) => {
                delivery.state = update.delivery;
            }
            None => message.deliveries.push(MessageDelivery {
                message_id: update.message_id,
                state: update.delivery,
            }),
            Some(_) => {}
        }
        self.entries[index].revision = self.entries[index].revision.saturating_add(1);
        ModelChange {
            changed: true,
            removed: self.trim_message_history(),
        }
    }

    fn apply_local(&mut self, record: &TranscriptRecord) -> ModelChange {
        let changed = match record.kind() {
            "session.started" => self.decode_local::<SessionStarted>(record).map(|payload| {
                if let Some(session_id) = payload.parent_session_id {
                    *self = self.fork_snapshot();
                    self.push(EntryKind::ForkedFrom { session_id });
                }
            }),
            "user.submitted" => self.decode_local::<UserSubmitted>(record).map(|payload| {
                self.push(EntryKind::User { text: payload.text });
            }),
            "user.steered" => self.decode_local::<UserSteered>(record).map(|payload| {
                self.push(EntryKind::User { text: payload.text });
            }),
            "reflection.started" => self.decode_local::<ReflectionStarted>(record).map(|_| {
                self.push(EntryKind::ReflectionStarted);
            }),
            "shell.started" => self
                .decode_local::<ShellStarted>(record)
                .map(|payload| self.shell_started(payload, record.recorded_at_unix_ms())),
            "shell.finished" => self
                .decode_local::<ShellFinished>(record)
                .map(|payload| self.shell_finished(payload)),
            "effort.changed" => self.decode_local::<EffortChanged>(record).map(|payload| {
                self.push(EntryKind::EffortChanged { to: payload.to });
            }),
            "fast_mode.changed" => self.decode_local::<FastModeChanged>(record).map(|payload| {
                self.push(EntryKind::FastModeChanged {
                    enabled: payload.to,
                });
            }),
            "worker.turn_finished" => {
                self.decode_local::<WorkerTurnFinished>(record)
                    .map(|payload| {
                        if let Some(error) = payload.error {
                            self.pending_error = Some(error);
                        }
                    })
            }
            "worker.turns_interrupted" => return self.apply_interruption(record),
            "worker.steer_failed" => {
                self.decode_local::<WorkerSteerFailed>(record)
                    .map(|payload| {
                        self.push(EntryKind::Error {
                            message: format!("Could not steer response: {}", payload.error),
                        });
                    })
            }
            "worker.stopped" => self.decode_local::<WorkerStopped>(record).map(|payload| {
                if let Some(error) = payload.error {
                    self.pending_error = Some(error);
                }
            }),
            "session.ended" => self.decode_local::<SessionEnded>(record).map(|payload| {
                if payload.outcome == "failed" {
                    self.finish_failed(payload.error);
                }
                self.agent_stream_closed();
            }),
            _ => return ModelChange::default(),
        };
        match changed {
            Ok(()) => ModelChange {
                changed: true,
                ..ModelChange::default()
            },
            Err(error) => self.projection_error(record, error, true),
        }
    }

    fn apply_interruption(&mut self, record: &TranscriptRecord) -> ModelChange {
        let payload = match self.decode_local::<WorkerTurnsInterrupted>(record) {
            Ok(payload) => payload,
            Err(error) => return self.projection_error(record, error, true),
        };
        if let Some(error) = payload.error {
            self.push(EntryKind::Error {
                message: format!("Could not interrupt response: {error}"),
            });
            return ModelChange {
                changed: true,
                ..ModelChange::default()
            };
        }
        if payload.count == 0 {
            return ModelChange::default();
        }
        self.push(EntryKind::Interrupted {
            count: payload.count,
        });
        ModelChange {
            changed: true,
            ..ModelChange::default()
        }
    }

    fn shell_started(&mut self, payload: ShellStarted, started_at_unix_ms: u64) {
        let id = self.push(EntryKind::Tool(ToolEntry {
            name: "exec_command".to_owned(),
            arguments: serde_json::json!({
                "cmd": payload.command,
                "workdir": payload.workspace,
            }),
            started_at_unix_ms,
            state: ToolState::Running,
            duration_ns: None,
            result: None,
            metadata: None,
            execution: ToolEntry::local_execution(),
            substeps: Vec::new(),
            child_count: 0,
        }));
        self.local_shells.insert(payload.id, id);
        self.running_tools.insert(id);
    }

    fn shell_finished(&mut self, payload: ShellFinished) {
        let Some(id) = self.local_shells.remove(&payload.id) else {
            return;
        };
        self.reasoning.clear();
        let failed = payload.error.is_some() || payload.exit_code != Some(0);
        self.update(id, |kind| {
            if let EntryKind::Tool(tool) = kind {
                tool.state = if failed {
                    ToolState::Failed
                } else {
                    ToolState::Succeeded
                };
                tool.duration_ns = Some(payload.duration_ns);
                tool.result = Some(serde_json::json!({
                    "output": payload.output,
                    "exit_code": payload.exit_code,
                    "truncated": payload.truncated,
                    "error": payload.error,
                }));
            }
        });
        self.running_tools.remove(&id);
    }

    fn apply_agent(&mut self, record: &TranscriptRecord) -> ModelChange {
        let previous_activity = self.transient.clone();
        if matches!(
            record.kind(),
            "assistant.delta"
                | "assistant.message"
                | "run.started"
                | "run.completed"
                | "run.failed"
                | "tool.call"
                | "tool.result"
        ) {
            self.reasoning.clear();
        }
        let result = match record.kind() {
            "assistant.delta" => self.assistant_delta(record),
            "assistant.message" => self.assistant_message(record),
            "reasoning.summary.delta" => self.reasoning_delta(record),
            "run.started" => {
                self.active_runs = self.active_runs.saturating_add(1);
                self.run_started_at_unix_ms
                    .push_back(record.recorded_at_unix_ms());
                self.transient = Some(TransientStatus::Thinking);
                Ok(true)
            }
            "run.error" => self.decode_local::<RunError>(record).map(|payload| {
                self.pending_error = Some(payload.message.clone());
                self.transient = Some(TransientStatus::Error(payload.message));
                true
            }),
            "run.completed" => {
                self.complete_turn(record);
                Ok(true)
            }
            "run.failed" => {
                self.run_started_at_unix_ms.pop_front();
                self.finish_failed(None);
                Ok(true)
            }
            "tool.call" => self.tool_call(record),
            "tool.result" => self.tool_result(record),
            "model.warmup.started" => {
                self.transient = Some(TransientStatus::Warming);
                Ok(true)
            }
            "model.warmup.completed" => {
                self.transient = self.is_active().then_some(TransientStatus::Thinking);
                Ok(true)
            }
            "model.warmup.failed"
            | "model.call.failed"
            | "model.attempt.failed"
            | "model.connection.failed" => self.capture_error(record),
            "model.call.started" => {
                self.materialize_compaction_failure();
                self.transient = Some(TransientStatus::Thinking);
                Ok(true)
            }
            "model.call.completed" => {
                self.transient = self.is_active().then_some(TransientStatus::Thinking);
                Ok(true)
            }
            "model.compaction.started" => {
                self.transient = Some(TransientStatus::Compacting);
                Ok(true)
            }
            "model.compaction.completed" => self.compaction_completed(record),
            "model.compaction.failed" => self.compaction_failed(record),
            "model.attempt.retrying" => self.retrying(record),
            "model.connection.started" => self.connection_started(record),
            "model.connection.completed" => {
                self.transient = self.is_active().then_some(TransientStatus::Thinking);
                self.pending_error = None;
                Ok(true)
            }
            _ => Ok(false),
        };
        let activity_changed = previous_activity != self.transient;
        match result {
            Ok(changed) => ModelChange {
                changed: changed || activity_changed,
                ..ModelChange::default()
            },
            Err(error) => self.projection_error(
                record,
                error,
                visibility(record.source(), record.kind()) == EventVisibility::Persistent,
            ),
        }
    }

    fn assistant_delta(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<AssistantDelta>()?;
        let phase = message_phase(payload.phase);
        let key = AssistantKey {
            call: payload.model_call_index,
            item: payload.item_id,
            phase,
        };
        let id = if let Some(&id) = self.assistants.get(&key) {
            id
        } else {
            let id = self.push(EntryKind::Assistant {
                text: String::new(),
                complete: false,
            });
            self.assistants.insert(key, id);
            self.active_assistants
                .insert((payload.model_call_index, phase), id);
            id
        };
        self.update(id, |kind| {
            if let EntryKind::Assistant { text, .. } = kind {
                text.push_str(&payload.text);
            }
        });
        self.transient = Some(TransientStatus::Responding);
        Ok(true)
    }

    fn assistant_message(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<AssistantMessage>()?;
        let phase = message_phase(payload.phase);
        let key = AssistantKey {
            call: payload.model_call_index,
            item: payload.item_id,
            phase,
        };
        let id = self
            .assistants
            .get(&key)
            .copied()
            .or_else(|| {
                self.active_assistants
                    .get(&(payload.model_call_index, phase))
                    .copied()
            })
            .unwrap_or_else(|| {
                let id = self.push(EntryKind::Assistant {
                    text: String::new(),
                    complete: false,
                });
                self.assistants.insert(key, id);
                id
            });
        self.update(id, |kind| {
            if let EntryKind::Assistant { text, complete, .. } = kind {
                *text = payload.text;
                *complete = true;
            }
        });
        self.transient = self.is_active().then_some(TransientStatus::Thinking);
        Ok(true)
    }

    fn reasoning_delta(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ReasoningSummaryDelta>()?;
        let key = ReasoningKey {
            request: record.agent_request_id(),
            call: payload.model_call_index,
        };
        let id = self
            .reasoning
            .get(&key)
            .copied()
            .filter(|id| self.entries.last().is_some_and(|entry| entry.id == *id))
            .unwrap_or_else(|| {
                let id = self.push(EntryKind::Reasoning {
                    text: String::new(),
                });
                self.reasoning.insert(key, id);
                id
            });
        self.update(id, |kind| {
            if let EntryKind::Reasoning { text } = kind {
                if text.ends_with("**") && payload.text.starts_with("**") {
                    text.push_str("  \n");
                }
                text.push_str(&payload.text);
            }
        });
        Ok(true)
    }

    fn tool_call(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let ToolCallPayload {
            call_id,
            tool,
            arguments,
        } = record.decode_payload::<ToolCallPayload>()?;
        let parent = self.code_parent(&call_id);
        if tool == "write_stdin"
            && let Some(session_id) = arguments.get("session_id").and_then(Value::as_i64)
            && let Some(id) = self
                .shell_sessions
                .get(&ShellSessionKey::from_arguments(&arguments, session_id))
                .copied()
        {
            if parent.is_some() {
                self.shell_followups.insert(call_id.clone(), id);
            } else {
                let substep = arguments
                    .get("chars")
                    .and_then(Value::as_str)
                    .filter(|chars| !chars.is_empty())
                    .map_or_else(
                        || "polled process".to_owned(),
                        |chars| format!("sent {chars:?}"),
                    );
                self.update(id, |kind| {
                    if let EntryKind::Tool(tool) = kind {
                        tool.state = ToolState::Running;
                        tool.substeps.push(substep);
                    }
                });
                self.tools.insert(call_id, id);
                self.running_tools.insert(id);
                self.transient = Some(TransientStatus::Tool("Shell".to_owned()));
                return Ok(true);
            }
        }
        let hidden = tool == "wait" && parent.is_none();
        let transient = if hidden {
            TransientStatus::WaitingForBackgroundWork
        } else {
            TransientStatus::Tool(humanize_tool(&tool))
        };
        let execution = ToolEntry::inferred_execution(&tool, &arguments, None);
        let id = self.push_with_parent(
            EntryKind::Tool(ToolEntry {
                name: tool,
                arguments,
                started_at_unix_ms: record.recorded_at_unix_ms(),
                state: ToolState::Running,
                duration_ns: None,
                result: None,
                metadata: None,
                execution,
                substeps: Vec::new(),
                child_count: 0,
            }),
            hidden,
            None,
        );
        if let Some(parent) = parent {
            self.register_code_child(parent, id);
        }
        self.tools.insert(call_id, id);
        self.running_tools.insert(id);
        self.transient = Some(transient);
        Ok(true)
    }

    fn tool_result(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ToolResultPayload>()?;
        let resumed_shell = self.shell_followups.remove(&payload.call_id);
        let shell_followup = payload.tool == "write_stdin";
        let result = preferred_result(payload.structured_result, payload.result);
        let resumed_result = resumed_shell.map(|_| result.clone());
        let nested_shell_followup = resumed_shell.is_some();
        let state = tool_result_state(&payload.tool, &payload.status, &result);
        let entry_state = if resumed_shell.is_some() && state == ToolState::Running {
            ToolState::Succeeded
        } else {
            state
        };
        let shell_session_id = (payload.tool == "exec_command")
            .then(|| tool_session_id(&result))
            .flatten();
        let id = self
            .tools
            .get(&payload.call_id)
            .copied()
            .unwrap_or_else(|| {
                let parent = self.code_parent(&payload.call_id);
                let id = self.push_with_parent(
                    EntryKind::Tool(ToolEntry {
                        name: payload.tool.clone(),
                        arguments: Value::Null,
                        started_at_unix_ms: record.recorded_at_unix_ms(),
                        state: ToolState::Running,
                        duration_ns: None,
                        result: None,
                        metadata: None,
                        execution: ToolEntry::inferred_execution(
                            &payload.tool,
                            &Value::Null,
                            payload.metadata.as_ref(),
                        ),
                        substeps: Vec::new(),
                        child_count: 0,
                    }),
                    false,
                    None,
                );
                if let Some(parent) = parent {
                    self.register_code_child(parent, id);
                }
                self.tools.insert(payload.call_id.clone(), id);
                id
            });
        let shell_session = shell_session_id.map(|session_id| {
            let arguments = self.entry(id).and_then(|entry| match &entry.kind {
                EntryKind::Tool(tool) => Some(&tool.arguments),
                _ => None,
            });
            ShellSessionKey::from_arguments(arguments.unwrap_or(&Value::Null), session_id)
        });
        let running_code_cell = (payload.tool == "exec")
            .then(|| running_code_cell_id(&result))
            .flatten()
            .map(str::to_owned);
        let observed_code_cell = (payload.tool == "wait")
            .then(|| self.requested_code_cell(id))
            .flatten()
            .map(str::to_owned);
        let code_cell_terminal = code_cell_terminal(&result);
        self.update(id, |kind| {
            if let EntryKind::Tool(tool) = kind {
                tool.state = entry_state;
                tool.duration_ns = Some(if shell_followup {
                    elapsed_nanoseconds(tool.started_at_unix_ms, record.recorded_at_unix_ms())
                        .max(payload.duration_ns)
                } else {
                    payload.duration_ns
                });
                tool.result = Some(if shell_followup {
                    if nested_shell_followup {
                        without_shell_output(result)
                    } else {
                        merge_shell_result(tool.result.take(), result)
                    }
                } else {
                    result
                });
                tool.metadata = payload.metadata;
                tool.infer_execution();
            }
        });
        if payload.tool == "exec"
            && let Some(children) = self.code_children.get(&id)
        {
            let only_code_child = <&[EntryId; 1]>::try_from(children.as_slice())
                .ok()
                .and_then(|[child]| self.entry(*child))
                .and_then(|entry| match &entry.kind {
                    EntryKind::Tool(tool) => Some((tool.result.as_ref(), tool.state)),
                    _ => None,
                });
            if code_mode_has_distinct_output(
                self.entry(id),
                only_code_child.and_then(|(result, _)| result),
                only_code_child.is_some_and(|(_, state)| state == ToolState::Failed),
            ) && let Some(index) = self.index_of(id)
            {
                self.entries[index].hidden = false;
                self.entries[index].revision = self.entries[index].revision.saturating_add(1);
            }
        }
        if let Some(shell) = resumed_shell {
            let resumed_result = resumed_result.expect("resumed shell result was retained");
            self.update(shell, |kind| {
                if let EntryKind::Tool(tool) = kind {
                    tool.state = state;
                    tool.duration_ns = Some(
                        elapsed_nanoseconds(tool.started_at_unix_ms, record.recorded_at_unix_ms())
                            .max(payload.duration_ns),
                    );
                    tool.result = Some(merge_shell_result(tool.result.take(), resumed_result));
                }
            });
            if state != ToolState::Running {
                self.shell_sessions.retain(|_, entry| *entry != shell);
                self.running_tools.remove(&shell);
            }
        }
        if payload.tool == "wait"
            && state == ToolState::Failed
            && let Some(index) = self.index_of(id)
        {
            self.entries[index].hidden = false;
        }
        if entry_state == ToolState::Running {
            if let Some(session_id) = shell_session {
                self.shell_sessions.insert(session_id, id);
            }
            self.running_tools.insert(id);
        } else {
            self.shell_sessions
                .retain(|_, shell_entry| *shell_entry != id);
            self.running_tools.remove(&id);
        }
        if let Some(cell_id) = running_code_cell {
            self.code_cells.insert(cell_id, id);
        }
        if let Some(cell_id) = observed_code_cell
            && let Some(terminal) = code_cell_terminal
            && let Some(parent) = self.code_cells.remove(&cell_id)
            && terminal == CodeCellTerminal::Terminated
        {
            self.fail_unfinished_code_children(parent);
        }
        self.transient = self.is_active().then_some(TransientStatus::Thinking);
        Ok(true)
    }

    fn compaction_completed(
        &mut self,
        record: &TranscriptRecord,
    ) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<CompactionCompleted>()?;
        self.push(EntryKind::ContextCompacted {
            duration_ns: payload.duration_ns,
        });
        self.transient = self.is_active().then_some(TransientStatus::Thinking);
        Ok(true)
    }

    fn compaction_failed(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<CompactionFailed>()?;
        self.pending_compaction_error = Some(payload.error.clone());
        self.pending_error = Some(payload.error);
        self.transient = self.is_active().then_some(TransientStatus::Thinking);
        Ok(true)
    }

    fn retrying(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<RetryPayload>()?;
        self.pending_error = Some(payload.error);
        self.transient = Some(TransientStatus::Retrying(payload.delay_ns));
        Ok(true)
    }

    fn connection_started(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ConnectionPayload>()?;
        self.transient = Some(if payload.purpose == "reconnect" {
            TransientStatus::Reconnecting
        } else {
            TransientStatus::Connecting
        });
        Ok(true)
    }

    fn capture_error(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ErrorPayload>()?;
        self.pending_error = Some(payload.error);
        Ok(false)
    }

    fn finish_success(&mut self) {
        self.materialize_compaction_failure();
        self.finish_activity();
        self.pending_error = None;
    }

    fn complete_turn(&mut self, record: &TranscriptRecord) {
        let payload_duration_ns = record
            .decode_payload::<RunDurationPayload>()
            .ok()
            .and_then(|payload| payload.duration_ns);
        let recorded_duration_ns = self.run_started_at_unix_ms.pop_front().map(|started_at| {
            record
                .recorded_at_unix_ms()
                .saturating_sub(started_at)
                .saturating_mul(1_000_000)
        });
        let duration_ns = payload_duration_ns.or(recorded_duration_ns);
        self.finish_success();
        let Some(duration_ns) = duration_ns else {
            return;
        };
        self.push(EntryKind::TurnCompleted { duration_ns });
    }

    fn finish_failed(&mut self, error: Option<String>) {
        self.pending_compaction_error = None;
        if error.is_none()
            && self.pending_error.is_none()
            && self
                .entries
                .last()
                .is_some_and(|entry| matches!(entry.kind, EntryKind::Error { .. }))
        {
            self.finish_activity();
            return;
        }
        let message = error
            .or_else(|| self.pending_error.take())
            .unwrap_or_else(|| "The agent run failed".to_owned());
        if !self.entries.last().is_some_and(|entry| {
            matches!(&entry.kind, EntryKind::Error { message: existing } if existing == &message)
        }) {
            self.push(EntryKind::Error { message });
        }
        self.finish_activity();
    }

    fn finish_activity(&mut self) {
        self.active_runs = self.active_runs.saturating_sub(1);
        if self.active_runs == 0 {
            self.fail_orphaned_tools();
        }
        self.transient = self.is_active().then_some(TransientStatus::Thinking);
    }

    fn fail_orphaned_tools(&mut self) {
        let local_shells = self.local_shells.values().copied().collect::<HashSet<_>>();
        let orphaned = self
            .running_tools
            .iter()
            .copied()
            .filter(|id| !local_shells.contains(id))
            .collect::<Vec<_>>();
        self.fail_unfinished_tools(&orphaned);
    }

    fn fail_unfinished_code_children(&mut self, parent: EntryId) {
        let unfinished = self
            .code_children
            .get(&parent)
            .into_iter()
            .flatten()
            .filter(|id| self.running_tools.contains(id))
            .copied()
            .collect::<Vec<_>>();
        self.fail_unfinished_tools(&unfinished);
    }

    fn fail_unfinished_tools(&mut self, unfinished: &[EntryId]) {
        for id in unfinished {
            self.update(*id, |kind| {
                let EntryKind::Tool(tool) = kind else {
                    return;
                };
                tool.state = ToolState::Failed;
                let result = tool.result.get_or_insert_with(|| serde_json::json!({}));
                if let Value::Object(result) = result
                    && result.get("error").is_none_or(Value::is_null)
                {
                    result.insert(
                        "error".to_owned(),
                        Value::String("tool call ended without a terminal result".to_owned()),
                    );
                }
            });
            self.running_tools.remove(id);
        }
        self.shell_sessions.retain(|_, id| !unfinished.contains(id));
        self.shell_followups
            .retain(|_, id| !unfinished.contains(id));
    }

    pub(crate) fn agent_stream_closed(&mut self) -> bool {
        let changed = self.active_runs > 0
            || self.running_tools.iter().any(|id| {
                !self
                    .local_shells
                    .values()
                    .any(|local_shell| local_shell == id)
            });
        self.active_runs = 0;
        self.run_started_at_unix_ms.clear();
        self.fail_orphaned_tools();
        self.transient = self.is_active().then_some(TransientStatus::Thinking);
        changed
    }

    fn materialize_compaction_failure(&mut self) {
        let Some(message) = self.pending_compaction_error.take() else {
            return;
        };
        self.push(EntryKind::ContextCompactionFailed { message });
    }

    fn projection_error(
        &mut self,
        record: &TranscriptRecord,
        error: serde_json::Error,
        visible: bool,
    ) -> ModelChange {
        let message = format!("Could not render {}: {error}", record.kind());
        if visible {
            self.push(EntryKind::Error { message });
        } else {
            self.pending_error = Some(message);
        }
        ModelChange {
            changed: visible,
            ..ModelChange::default()
        }
    }

    fn decode_local<T: serde::de::DeserializeOwned>(
        &self,
        record: &TranscriptRecord,
    ) -> Result<T, serde_json::Error> {
        record.decode_payload()
    }

    fn push(&mut self, kind: EntryKind) -> EntryId {
        self.push_with_visibility(kind, false)
    }

    fn push_with_visibility(&mut self, kind: EntryKind, hidden: bool) -> EntryId {
        self.push_with_parent(kind, hidden, None)
    }

    fn push_with_parent(
        &mut self,
        kind: EntryKind,
        hidden: bool,
        parent: Option<EntryId>,
    ) -> EntryId {
        if let Some(parent) = parent {
            self.join_workflow(parent);
        }
        let id = EntryId::from_index(self.next_entry_id);
        self.next_entry_id = self.next_entry_id.saturating_add(1);
        self.entry_indices.insert(id, self.entries.len());
        self.entries.push(TranscriptEntry {
            id,
            revision: 1,
            kind,
            hidden,
            parent,
            trailing_spacer: true,
        });
        id
    }

    fn join_workflow(&mut self, parent: EntryId) {
        let Some(previous) = self.entries.iter_mut().rev().find(|entry| !entry.hidden) else {
            return;
        };
        if previous.id != parent && previous.parent != Some(parent) {
            return;
        }
        previous.trailing_spacer = false;
        previous.revision = previous.revision.saturating_add(1);
    }

    fn code_parent(&self, call_id: &str) -> Option<EntryId> {
        let (parent_call_id, child) = call_id.rsplit_once("/code-")?;
        child.parse::<u64>().ok()?;
        let parent = self.tools.get(parent_call_id).copied()?;
        let entry = self.entry(parent)?;
        matches!(&entry.kind, EntryKind::Tool(tool) if tool.name == "exec").then_some(parent)
    }

    fn requested_code_cell(&self, id: EntryId) -> Option<&str> {
        let EntryKind::Tool(tool) = &self.entry(id)?.kind else {
            return None;
        };
        tool.arguments.get("cell_id")?.as_str()
    }

    fn register_code_child(&mut self, parent: EntryId, child: EntryId) {
        self.update(parent, |kind| {
            if let EntryKind::Tool(tool) = kind {
                tool.child_count = tool.child_count.saturating_add(1);
            }
        });
        let children = self.code_children.entry(parent).or_default();
        children.push(child);
        let single_child = children.len() == 1;
        let index = self.index_of(parent).expect("code parent is retained");
        self.entries[index].hidden = single_child;
        self.entries[index].revision = self.entries[index].revision.saturating_add(1);

        let child_index = self.index_of(child).expect("code child is retained");
        self.entries[child_index].parent = None;
    }

    fn trim_message_history(&mut self) -> Option<EntryId> {
        if self.message_order.len() <= MAX_RETAINED_MESSAGE_THREADS {
            return None;
        }
        let position = self.message_order.iter().position(|thread_id| {
            let Some(id) = self.message_threads.get(thread_id) else {
                return true;
            };
            let Some(entry) = self.entry(*id) else {
                return true;
            };
            let EntryKind::DirectedMessage(message) = &entry.kind else {
                return true;
            };
            !message.deliveries.iter().any(|delivery| {
                matches!(
                    delivery.state,
                    MessageDeliveryState::Admitted {
                        disposition: MessageDisposition::Queued
                    }
                )
            })
        })?;
        let thread_id = self
            .message_order
            .remove(position)
            .expect("the retained message thread should still exist");
        let id = self.message_threads.remove(&thread_id)?;
        let removed_index = self.entry_indices.remove(&id)?;
        self.entries.remove(removed_index);
        for (index, entry) in self.entries.iter().enumerate().skip(removed_index) {
            self.entry_indices.insert(entry.id, index);
        }
        Some(id)
    }

    fn update(&mut self, id: EntryId, update: impl FnOnce(&mut EntryKind)) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        update(&mut self.entries[index].kind);
        self.entries[index].revision = self.entries[index].revision.saturating_add(1);
    }
}

fn delivery_advances(current: &MessageDeliveryState, next: &MessageDeliveryState) -> bool {
    current != next && matches!(current, MessageDeliveryState::Admitted { .. })
}

fn message_phase(phase: Option<AgentMessagePhase>) -> MessagePhase {
    match phase {
        Some(AgentMessagePhase::Commentary) => MessagePhase::Commentary,
        Some(AgentMessagePhase::FinalAnswer) | None => MessagePhase::Final,
    }
}

fn visibility(source: &str, kind: &str) -> EventVisibility {
    if source == "tact" {
        return match kind {
            "user.submitted"
            | "reflection.started"
            | "worker.turns_interrupted"
            | "effort.changed"
            | "fast_mode.changed" => EventVisibility::Persistent,
            "worker.turn_finished" | "worker.stopped" | "session.ended" => {
                EventVisibility::ErrorFallback
            }
            _ => EventVisibility::StateOnly,
        };
    }
    match kind {
        "assistant.delta"
        | "assistant.message"
        | "reasoning.summary.delta"
        | "tool.call"
        | "tool.result"
        | "model.compaction.completed"
        | "model.compaction.failed" => EventVisibility::Persistent,
        "run.started"
        | "model.warmup.started"
        | "model.call.started"
        | "model.compaction.started"
        | "model.attempt.retrying"
        | "model.connection.started" => EventVisibility::Transient,
        "run.error"
        | "run.failed"
        | "model.warmup.failed"
        | "model.call.failed"
        | "model.attempt.failed"
        | "model.connection.failed" => EventVisibility::ErrorFallback,
        _ => EventVisibility::StateOnly,
    }
}

fn tool_session_id(result: &Value) -> Option<i64> {
    if let Value::String(text) = result {
        let decoded = serde_json::from_str::<Value>(text).ok()?;
        return decoded.get("session_id").and_then(Value::as_i64);
    }
    result.get("session_id").and_then(Value::as_i64)
}

fn running_code_cell_id(result: &Value) -> Option<&str> {
    code_mode_status(result)?
        .strip_prefix("Script running with cell ID ")?
        .split_whitespace()
        .next()
}

fn code_cell_terminal(result: &Value) -> Option<CodeCellTerminal> {
    let status = code_mode_status(result)?;
    if status.starts_with("Script completed") {
        Some(CodeCellTerminal::Completed)
    } else if status.starts_with("Script terminated") {
        Some(CodeCellTerminal::Terminated)
    } else {
        None
    }
}

fn code_mode_status(result: &Value) -> Option<&str> {
    match result {
        Value::String(status) => Some(status),
        Value::Array(items) => items
            .iter()
            .find_map(|item| item.get("text").and_then(Value::as_str)),
        Value::Object(fields) => fields.get("text").and_then(Value::as_str),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn code_mode_has_distinct_output(
    entry: Option<&TranscriptEntry>,
    only_child_result: Option<&Value>,
    only_child_failed: bool,
) -> bool {
    let Some(TranscriptEntry {
        kind: EntryKind::Tool(tool),
        ..
    }) = entry
    else {
        return false;
    };
    let Some(result) = &tool.result else {
        return false;
    };
    code_mode_value_has_output(result, only_child_result, only_child_failed)
}

fn code_mode_value_has_output(
    result: &Value,
    only_child_result: Option<&Value>,
    only_child_failed: bool,
) -> bool {
    if only_child_result.is_some_and(|child| values_duplicate(result, child)) {
        return false;
    }
    match result {
        Value::String(text) => text_has_distinct_output(text, only_child_result, only_child_failed),
        Value::Array(items) => items
            .iter()
            .any(|item| code_mode_value_has_output(item, only_child_result, only_child_failed)),
        Value::Object(fields) => {
            if let Some(text) = fields.get("text").and_then(Value::as_str) {
                return text_has_distinct_output(text, only_child_result, only_child_failed);
            }
            let mut has_supported_output = false;
            for key in ["content", "output", "image_url", "audio_url"] {
                let Some(value) = fields.get(key) else {
                    continue;
                };
                has_supported_output = true;
                if code_mode_value_has_output(value, only_child_result, only_child_failed) {
                    return true;
                }
            }
            !has_supported_output && !fields.is_empty()
        }
        Value::Bool(_) | Value::Number(_) => true,
        Value::Null => false,
    }
}

fn values_duplicate(candidate: &Value, child: &Value) -> bool {
    candidate == child
        || content_envelope_matches(candidate, child)
        || content_envelope_matches(child, candidate)
}

fn content_envelope_matches(envelope: &Value, payload: &Value) -> bool {
    let Some(fields) = envelope.as_object() else {
        return false;
    };
    if !matches!(
        fields.get("type").and_then(Value::as_str),
        Some("input_text" | "input_image" | "input_audio")
    ) {
        return false;
    }
    let Some(payload) = payload.as_object() else {
        return false;
    };
    fields.len() == payload.len() + 1
        && fields
            .iter()
            .filter(|(key, _)| key.as_str() != "type")
            .all(|(key, value)| payload.get(key) == Some(value))
}

fn text_has_distinct_output(
    text: &str,
    only_child_result: Option<&Value>,
    only_child_failed: bool,
) -> bool {
    let failed_envelope = text.starts_with("Script failed");
    let text = code_mode_output_text(text);
    if text.trim().is_empty() {
        return false;
    }
    if failed_envelope
        && only_child_failed
        && only_child_result
            .and_then(Value::as_str)
            .is_some_and(|child| !child.trim().is_empty() && text.contains(child.trim()))
    {
        return false;
    }
    !only_child_result.is_some_and(|child| text_duplicates_value(text, child))
}

fn text_duplicates_value(text: &str, value: &Value) -> bool {
    let text = text.trim();
    value.as_str() == Some(text)
        || (text_may_encode_value(text, value)
            && serde_json::from_str::<Value>(text).is_ok_and(|decoded| decoded == *value))
}

fn text_may_encode_value(text: &str, value: &Value) -> bool {
    let Some(first) = text.as_bytes().first() else {
        return false;
    };
    match value {
        Value::Object(_) => *first == b'{',
        Value::Array(_) => *first == b'[',
        Value::String(_) => *first == b'"',
        Value::Number(_) => *first == b'-' || first.is_ascii_digit(),
        Value::Bool(true) => *first == b't',
        Value::Bool(false) => *first == b'f',
        Value::Null => *first == b'n',
    }
}

fn tool_result_state(tool: &str, status: &str, result: &Value) -> ToolState {
    if !matches!(status, "success" | "completed") {
        return ToolState::Failed;
    }
    if result_reports_failure(result) {
        return ToolState::Failed;
    }
    if !matches!(tool, "exec_command" | "write_stdin") {
        return ToolState::Succeeded;
    }
    if let Some(exit_code) = result.get("exit_code").and_then(Value::as_i64) {
        return if exit_code == 0 {
            ToolState::Succeeded
        } else {
            ToolState::Failed
        };
    }
    if tool_session_id(result).is_some() && result.get("exit_code").is_none() {
        return ToolState::Running;
    }
    ToolState::Failed
}

fn result_reports_failure(result: &Value) -> bool {
    let Some(fields) = result.as_object() else {
        return false;
    };
    if fields
        .get("isError")
        .or_else(|| fields.get("is_error"))
        .and_then(Value::as_bool)
        == Some(true)
        || fields.get("success").and_then(Value::as_bool) == Some(false)
    {
        return true;
    }
    if fields.get("error").is_some_and(|error| match error {
        Value::Null | Value::Bool(false) => false,
        Value::String(message) => !message.trim().is_empty(),
        _ => true,
    }) {
        return true;
    }
    fields
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "failed" | "error" | "cancelled" | "canceled"))
}

fn elapsed_nanoseconds(started_at_unix_ms: u64, finished_at_unix_ms: u64) -> u64 {
    finished_at_unix_ms
        .saturating_sub(started_at_unix_ms)
        .saturating_mul(1_000_000)
}

fn normalize_result(result: Value) -> Value {
    let Value::String(encoded) = result else {
        return result;
    };
    serde_json::from_str(&encoded).unwrap_or(Value::String(encoded))
}

fn preferred_result(structured: Value, model_visible: Value) -> Value {
    let structured = normalize_result(structured);
    let model_visible = normalize_result(model_visible);
    if has_useful_result(&structured) {
        structured
    } else {
        model_visible
    }
}

fn has_useful_result(result: &Value) -> bool {
    match result {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn merge_shell_result(current: Option<Value>, next: Value) -> Value {
    let Some(Value::Object(mut current)) = current else {
        return next;
    };
    let mut next = match next {
        Value::Object(next) => next,
        other => return other,
    };
    let previous_output = current
        .remove("output")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    if let Some(Value::String(output)) = next.get_mut("output") {
        output.insert_str(0, &previous_output);
    }
    Value::Object(next)
}

fn without_shell_output(result: Value) -> Value {
    let Value::Object(mut fields) = result else {
        return result;
    };
    fields.remove("output");
    Value::Object(fields)
}

#[derive(Deserialize)]
struct UserSubmitted {
    text: String,
}

#[derive(Deserialize)]
struct UserSteered {
    text: String,
}

#[derive(Deserialize)]
struct ReflectionStarted {
    #[serde(rename = "id")]
    _id: u64,
}

#[derive(Deserialize)]
struct ShellStarted {
    id: ShellId,
    command: String,
    workspace: PathBuf,
}

#[derive(Deserialize)]
struct ShellFinished {
    id: ShellId,
    output: String,
    exit_code: Option<i32>,
    duration_ns: u64,
    truncated: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
struct EffortChanged {
    to: ReasoningEffort,
}

#[derive(Deserialize)]
struct FastModeChanged {
    to: bool,
}

#[derive(Deserialize)]
struct WorkerTurnFinished {
    error: Option<String>,
}

#[derive(Deserialize)]
struct WorkerTurnsInterrupted {
    count: usize,
    error: Option<String>,
}

#[derive(Deserialize)]
struct WorkerSteerFailed {
    error: String,
}

#[derive(Deserialize)]
struct WorkerStopped {
    error: Option<String>,
}

#[derive(Deserialize)]
struct SessionEnded {
    outcome: String,
    error: Option<String>,
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
    tool: String,
    status: String,
    duration_ns: u64,
    result: Value,
    structured_result: Value,
    metadata: Option<Value>,
}

#[derive(Deserialize)]
struct RunDurationPayload {
    #[serde(default)]
    duration_ns: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorPayload {
    error: String,
}

#[derive(Deserialize)]
struct RetryPayload {
    delay_ns: u64,
    error: String,
}

#[derive(Deserialize)]
struct ConnectionPayload {
    purpose: String,
}

#[cfg(test)]
mod tests {
    use super::{EntryKind, ToolState, TranscriptModel, TranscriptRecord};
    use nanocodex::agent::events::{AgentEvent, AgentEventKind};
    use serde_json::{Value, json, value::to_raw_value};
    use std::sync::Arc;

    fn agent_record(sequence: u64, kind: AgentEventKind, payload: Value) -> TranscriptRecord {
        TranscriptRecord::from_agent(
            sequence,
            sequence * 10,
            AgentEvent {
                protocol_version: 1,
                request_id: Arc::from("request"),
                seq: sequence,
                kind,
                payload: to_raw_value(&payload).unwrap().into(),
            },
        )
    }

    fn call(sequence: u64, call_id: &str, tool: &str, arguments: Value) -> TranscriptRecord {
        agent_record(
            sequence,
            AgentEventKind::ToolCall,
            json!({"call_id": call_id, "tool": tool, "arguments": arguments}),
        )
    }

    fn result(
        sequence: u64,
        call_id: &str,
        tool: &str,
        result: Value,
        structured_result: Value,
        metadata: Value,
    ) -> TranscriptRecord {
        agent_record(
            sequence,
            AgentEventKind::ToolResult,
            json!({
                "call_id": call_id,
                "tool": tool,
                "status": "completed",
                "duration_ns": 10,
                "result": result,
                "structured_result": structured_result,
                "metadata": metadata,
            }),
        )
    }

    #[test]
    fn semantic_children_hide_single_wrapper_but_keep_multi_tool_batch() {
        let mut model = TranscriptModel::default();
        model.apply(&call(1, "outer", "exec", json!("await tools.one({})")));
        model.apply(&call(
            2,
            "outer/code-0",
            "exec_command",
            json!({"cmd": "pwd"}),
        ));
        assert!(model.entries()[0].hidden);
        model.apply(&call(
            3,
            "outer/code-1",
            "mcp__docs__search",
            json!({"query": "x"}),
        ));

        assert_eq!(model.entries().len(), 3);
        assert!(!model.entries()[0].hidden);
        assert!(!model.entries()[1].hidden);
        assert!(!model.entries()[2].hidden);
        assert!(model.entries()[1].parent.is_none());
        assert!(model.entries()[2].parent.is_none());
        let EntryKind::Tool(wrapper) = &model.entries()[0].kind else {
            panic!("wrapper should remain a tool entry");
        };
        assert_eq!(wrapper.child_count, 2);
    }

    #[test]
    fn structured_results_drive_failure_state_and_machine_origin() {
        let mut model = TranscriptModel::default();
        model.apply(&call(1, "remote", "custom_operation", json!({})));
        model.apply(&result(
            2,
            "remote",
            "custom_operation",
            json!("less useful model text"),
            json!({"isError": true, "content": [{"type": "text", "text": "permission denied"}]}),
            json!({"executor": {"machine_name": "Alice's Mac"}}),
        ));

        let EntryKind::Tool(tool) = &model.entries()[0].kind else {
            panic!("result should update the tool entry");
        };
        assert_eq!(tool.state, ToolState::Failed);
        assert_eq!(tool.result.as_ref().unwrap()["isError"], true);
        assert_eq!(tool.execution_qualifier(), "Machine Alice's Mac");
    }

    #[test]
    fn null_structured_result_falls_back_to_model_visible_output() {
        let mut model = TranscriptModel::default();
        model.apply(&call(1, "direct", "custom_operation", json!({})));
        model.apply(&result(
            2,
            "direct",
            "custom_operation",
            json!("visible output"),
            Value::Null,
            Value::Null,
        ));

        let EntryKind::Tool(tool) = &model.entries()[0].kind else {
            panic!("result should update the tool entry");
        };
        assert_eq!(tool.state, ToolState::Succeeded);
        assert_eq!(tool.result, Some(json!("visible output")));
    }

    #[test]
    fn nested_shell_interaction_keeps_output_only_on_the_owning_shell() {
        let mut model = TranscriptModel::default();
        model.apply(&call(
            1,
            "outer",
            "exec",
            json!("await tools.exec_command({})"),
        ));
        model.apply(&call(
            2,
            "outer/code-0",
            "exec_command",
            json!({"cmd": "interactive", "tty": true}),
        ));
        model.apply(&result(
            3,
            "outer/code-0",
            "exec_command",
            Value::Null,
            json!({"session_id": 7, "output": "ready\n"}),
            Value::Null,
        ));
        model.apply(&call(
            4,
            "outer/code-1",
            "write_stdin",
            json!({"session_id": 7, "chars": "go\n"}),
        ));
        model.apply(&result(
            5,
            "outer/code-1",
            "write_stdin",
            Value::Null,
            json!({"exit_code": 0, "output": "done\n"}),
            Value::Null,
        ));

        let EntryKind::Tool(shell) = &model.entries()[1].kind else {
            panic!("first semantic child should be the owning shell");
        };
        let EntryKind::Tool(interaction) = &model.entries()[2].kind else {
            panic!("second semantic child should be the shell interaction");
        };
        assert_eq!(shell.result.as_ref().unwrap()["output"], "ready\ndone\n");
        assert!(interaction.result.as_ref().unwrap().get("output").is_none());
        assert_eq!(interaction.result.as_ref().unwrap()["exit_code"], 0);
    }

    #[test]
    fn shell_sessions_are_correlated_by_environment_and_session_id() {
        let mut model = TranscriptModel::default();
        model.apply(&call(
            1,
            "sandbox-shell",
            "exec_command",
            json!({"environment": "sandbox", "cmd": "interactive", "tty": true}),
        ));
        model.apply(&result(
            2,
            "sandbox-shell",
            "exec_command",
            Value::Null,
            json!({"session_id": 7, "output": "sandbox ready\n"}),
            Value::Null,
        ));
        model.apply(&call(
            3,
            "machine-shell",
            "exec_command",
            json!({"environment": "user:build-box", "cmd": "interactive", "tty": true}),
        ));
        model.apply(&result(
            4,
            "machine-shell",
            "exec_command",
            Value::Null,
            json!({"session_id": 7, "output": "machine ready\n"}),
            Value::Null,
        ));
        model.apply(&call(
            5,
            "sandbox-input",
            "write_stdin",
            json!({"environment": "sandbox", "session_id": 7, "chars": "sandbox input\n"}),
        ));
        model.apply(&result(
            6,
            "sandbox-input",
            "write_stdin",
            Value::Null,
            json!({"session_id": 7, "output": "sandbox output\n"}),
            Value::Null,
        ));
        model.apply(&call(
            7,
            "machine-input",
            "write_stdin",
            json!({"environment": "user:build-box", "session_id": 7, "chars": "machine input\n"}),
        ));
        model.apply(&result(
            8,
            "machine-input",
            "write_stdin",
            Value::Null,
            json!({"session_id": 7, "output": "machine output\n"}),
            Value::Null,
        ));

        assert_eq!(model.entries().len(), 2);
        let EntryKind::Tool(sandbox) = &model.entries()[0].kind else {
            panic!("first entry should remain the sandbox shell");
        };
        let EntryKind::Tool(machine) = &model.entries()[1].kind else {
            panic!("second entry should remain the machine shell");
        };
        assert_eq!(sandbox.execution_qualifier(), "Sandbox");
        assert_eq!(machine.execution_qualifier(), "Machine build-box");
        assert_eq!(sandbox.substeps, ["sent \"sandbox input\\n\""]);
        assert_eq!(machine.substeps, ["sent \"machine input\\n\""]);
        assert_eq!(
            sandbox.result.as_ref().unwrap()["output"],
            "sandbox ready\nsandbox output\n"
        );
        assert_eq!(
            machine.result.as_ref().unwrap()["output"],
            "machine ready\nmachine output\n"
        );
    }

    #[test]
    fn hidden_code_wrapper_returns_for_failure_or_authoritative_output() {
        let mut failed = TranscriptModel::default();
        failed.apply(&call(1, "failed", "exec", json!("await tools.one({})")));
        failed.apply(&call(2, "failed/code-0", "custom_operation", json!({})));
        failed.apply(&agent_record(
            3,
            AgentEventKind::ToolResult,
            json!({
                "call_id": "failed",
                "tool": "exec",
                "status": "failed",
                "duration_ns": 10,
                "result": "Script failed\nWall time 0.1 seconds\nOutput:\nboom",
                "structured_result": null,
                "metadata": null
            }),
        ));
        assert!(!failed.entries()[0].hidden);

        let mut output = TranscriptModel::default();
        output.apply(&call(1, "output", "exec", json!("text('summary')")));
        output.apply(&call(2, "output/code-0", "custom_operation", json!({})));
        output.apply(&result(
            3,
            "output",
            "exec",
            json!([
                {"type": "text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
                {"type": "text", "text": "authoritative summary"}
            ]),
            Value::Null,
            Value::Null,
        ));
        assert!(!output.entries()[0].hidden);

        let mut status_only = TranscriptModel::default();
        status_only.apply(&call(1, "status", "exec", json!("await tools.one({})")));
        status_only.apply(&call(2, "status/code-0", "custom_operation", json!({})));
        status_only.apply(&result(
            3,
            "status",
            "exec",
            json!("Script completed\nWall time 0.1 seconds\nOutput:\n"),
            Value::Null,
            Value::Null,
        ));
        assert!(status_only.entries()[0].hidden);
    }

    #[test]
    fn failed_single_child_echo_keeps_only_the_semantic_child() {
        let child_error = "Error: sandbox workspace is unavailable";
        let mut model = TranscriptModel::default();
        model.apply(&call(
            1,
            "failed-echo",
            "exec",
            json!("await tools.exec_command({environment: 'sandbox', cmd: 'sleep 20'})"),
        ));
        model.apply(&call(
            2,
            "failed-echo/code-0",
            "exec_command",
            json!({"environment": "sandbox", "cmd": "sleep 20"}),
        ));
        model.apply(&agent_record(
            3,
            AgentEventKind::ToolResult,
            json!({
                "call_id": "failed-echo/code-0",
                "tool": "exec_command",
                "status": "failed",
                "duration_ns": 10,
                "result": child_error,
                "structured_result": child_error,
                "metadata": null
            }),
        ));
        model.apply(&agent_record(
            4,
            AgentEventKind::ToolResult,
            json!({
                "call_id": "failed-echo",
                "tool": "exec",
                "status": "failed",
                "duration_ns": 10,
                "result": format!(
                    "Script failed\nWall time 0.1 seconds\nOutput:\nError: {child_error}\n    at unwrap (index.js:1:1)"
                ),
                "structured_result": null,
                "metadata": null
            }),
        ));

        assert!(model.entries()[0].hidden);
        assert!(!model.entries()[1].hidden);
    }

    #[test]
    fn exact_single_child_echo_does_not_restore_code_wrapper() {
        let account = json!({
            "status": "ready",
            "authenticated": ["github"],
            "machines": [{"id": "sandbox", "kind": "sandbox"}],
            "vault": []
        });
        let mut model = TranscriptModel::default();
        model.apply(&call(
            1,
            "account-wrapper",
            "exec",
            json!("text(await tools.accountInfo({}))"),
        ));
        model.apply(&call(2, "account-wrapper/code-0", "accountInfo", json!({})));
        model.apply(&result(
            3,
            "account-wrapper/code-0",
            "accountInfo",
            account.clone(),
            account.clone(),
            Value::Null,
        ));
        model.apply(&result(
            4,
            "account-wrapper",
            "exec",
            json!([
                {
                    "type": "input_text",
                    "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"
                },
                {"type": "input_text", "text": serde_json::to_string(&account).unwrap()}
            ]),
            Value::Null,
            Value::Null,
        ));

        assert!(model.entries()[0].hidden);
        assert!(!model.entries()[1].hidden);
        let EntryKind::Tool(child) = &model.entries()[1].kind else {
            panic!("accountInfo child should remain visible");
        };
        assert_eq!(child.duration_ns, Some(10));
        assert_eq!(child.result, Some(account));
    }

    #[test]
    fn exact_object_echoes_keep_only_the_semantic_child() {
        let child_result = json!({"output": "artifact", "status": "ready"});
        for parent_result in [child_result.clone(), json!([child_result.clone()])] {
            let mut model = TranscriptModel::default();
            model.apply(&call(
                1,
                "object-wrapper",
                "exec",
                json!("await tools.inspect({})"),
            ));
            model.apply(&call(2, "object-wrapper/code-0", "inspect", json!({})));
            model.apply(&result(
                3,
                "object-wrapper/code-0",
                "inspect",
                child_result.clone(),
                child_result.clone(),
                Value::Null,
            ));
            model.apply(&result(
                4,
                "object-wrapper",
                "exec",
                parent_result,
                Value::Null,
                Value::Null,
            ));

            assert!(model.entries()[0].hidden);
            assert!(!model.entries()[1].hidden);
            let EntryKind::Tool(child) = &model.entries()[1].kind else {
                panic!("inspect child should remain visible");
            };
            assert_eq!(child.result.as_ref(), Some(&child_result));
        }
    }

    #[test]
    fn multimodal_content_echoes_keep_only_the_semantic_child() {
        for (child_result, emitted_item) in [
            (
                json!({"image_url": "data:image/png;base64,AAAA", "detail": "high"}),
                json!({
                    "type": "input_image",
                    "image_url": "data:image/png;base64,AAAA",
                    "detail": "high"
                }),
            ),
            (
                json!({"audio_url": "data:audio/wav;base64,AAAA"}),
                json!({
                    "type": "input_audio",
                    "audio_url": "data:audio/wav;base64,AAAA"
                }),
            ),
        ] {
            let mut model = TranscriptModel::default();
            model.apply(&call(1, "media-wrapper", "exec", json!("emit media")));
            model.apply(&call(2, "media-wrapper/code-0", "media_tool", json!({})));
            model.apply(&result(
                3,
                "media-wrapper/code-0",
                "media_tool",
                child_result.clone(),
                child_result.clone(),
                Value::Null,
            ));
            model.apply(&result(
                4,
                "media-wrapper",
                "exec",
                json!([
                    {
                        "type": "input_text",
                        "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"
                    },
                    emitted_item
                ]),
                Value::Null,
                Value::Null,
            ));

            assert!(model.entries()[0].hidden);
            assert!(!model.entries()[1].hidden);
            let EntryKind::Tool(child) = &model.entries()[1].kind else {
                panic!("media child should remain visible");
            };
            assert_eq!(child.result.as_ref(), Some(&child_result));
        }
    }

    #[test]
    fn empty_structured_results_do_not_replace_useful_visible_output() {
        for (tool_name, arguments) in [
            ("apply_patch", json!("*** Begin Patch\n*** End Patch")),
            ("update_plan", json!({"plan": []})),
        ] {
            let mut model = TranscriptModel::default();
            model.apply(&call(1, "call", tool_name, arguments));
            model.apply(&result(
                2,
                "call",
                tool_name,
                json!("visible confirmation"),
                json!({}),
                Value::Null,
            ));

            let EntryKind::Tool(tool) = &model.entries()[0].kind else {
                panic!("result should update the tool entry");
            };
            assert_eq!(tool.result, Some(json!("visible confirmation")));
        }
    }
}
