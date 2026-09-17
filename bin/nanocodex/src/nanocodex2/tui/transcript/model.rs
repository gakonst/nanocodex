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
    active_assistants: HashMap<AssistantCallKey, AssistantKey>,
    voice_messages: HashMap<(String, String, u64), (EntryId, bool)>,
    managed_final_messages: HashMap<Arc<str>, EntryId>,
    managed_completed_turns: HashSet<String>,
    managed_stopped_turns: HashSet<String>,
    managed_answer_entries: HashMap<Arc<str>, HashSet<EntryId>>,
    reasoning: HashMap<ReasoningKey, EntryId>,
    tools: HashMap<String, EntryId>,
    settled_calls: HashSet<String>,
    shell_sessions: HashMap<ShellSessionKey, EntryId>,
    shell_followups: HashMap<String, EntryId>,
    code_children: HashMap<EntryId, Vec<EntryId>>,
    code_cells: HashMap<String, EntryId>,
    local_shells: HashMap<ShellId, EntryId>,
    message_threads: HashMap<ThreadId, EntryId>,
    message_order: VecDeque<ThreadId>,
    running_tools: HashSet<EntryId>,
    active_runs: VecDeque<ActiveRun>,
    tool_owners: HashMap<EntryId, RunScope>,
    transient: Option<TransientStatus>,
    transient_retry_origin: Option<(u64, u64)>,
    run_activity: VecDeque<RunActivity>,
    pending_error: Option<String>,
    pending_managed_errors: HashMap<RunScope, String>,
    pending_compaction_errors: HashMap<RunScope, String>,
    run_failure_entries: HashMap<RunScope, EntryId>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RunScope {
    turn: Option<Arc<str>>,
    child: Option<u64>,
    request: Option<Arc<str>>,
}

impl RunScope {
    fn new(record: &TranscriptRecord) -> Self {
        Self {
            turn: record.managed_turn_id(),
            child: record.managed_agent_id(),
            request: record.agent_request_id(),
        }
    }
}

struct RunActivity {
    scope: RunScope,
    status: TransientStatus,
    retry_origin: Option<(u64, u64)>,
}

struct ActiveRun {
    scope: RunScope,
    started_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AssistantKey {
    call: AssistantCallKey,
    item: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AssistantCallKey {
    turn: Option<Arc<str>>,
    child: Option<u64>,
    request: Option<Arc<str>>,
    index: u32,
    phase: MessagePhase,
}

impl AssistantCallKey {
    fn new(record: &TranscriptRecord, index: u32, phase: MessagePhase) -> Self {
        Self {
            turn: record.managed_turn_id(),
            child: record.managed_agent_id(),
            request: record.agent_request_id(),
            index,
            phase,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ReasoningKey {
    scope: RunScope,
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

    pub(crate) fn transient_retry_origin(&self) -> Option<(u64, u64)> {
        self.transient_retry_origin
    }

    fn is_finished_managed_run(&self, scope: &RunScope) -> bool {
        scope.child.is_none()
            && scope.turn.as_deref().is_some_and(|turn| {
                self.managed_completed_turns.contains(turn)
                    || self.managed_stopped_turns.contains(turn)
            })
    }

    pub(crate) fn ignores_finished_run_event(&self, record: &TranscriptRecord) -> bool {
        record.source() == "agent"
            && (record.kind().starts_with("run.")
                || record.kind().starts_with("model.")
                || record.kind() == "api.event")
            && self.is_finished_managed_run(&RunScope::new(record))
    }

    fn set_run_status(&mut self, record: &TranscriptRecord, status: Option<TransientStatus>) {
        let scope = RunScope::new(record);
        self.run_activity.retain(|activity| activity.scope != scope);
        if !self.is_finished_managed_run(&scope)
            && let Some(status) = status
        {
            let retry_origin = matches!(status, TransientStatus::Retrying(_))
                .then(|| (record.sequence(), record.recorded_at_unix_ms()));
            self.run_activity.push_back(RunActivity {
                scope,
                status,
                retry_origin,
            });
        }
        self.refresh_transient();
    }

    fn refresh_transient(&mut self) {
        // Prefer useful activity over an unrelated run's generic thinking state.
        let activity = self
            .run_activity
            .iter()
            .rev()
            .find(|activity| activity.status != TransientStatus::Thinking)
            .or_else(|| self.run_activity.back());
        self.transient = activity
            .map(|activity| activity.status.clone())
            .or_else(|| self.is_active().then_some(TransientStatus::Thinking));
        self.transient_retry_origin = activity.and_then(|activity| activity.retry_origin);
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.active_runs.is_empty()
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
                self.push(EntryKind::User {
                    text: format!("[steering accepted]\n{}", payload.text),
                });
            }),
            "user.steer_withdrawn" => self.decode_local::<UserSteered>(record).map(|payload| {
                self.push(EntryKind::User {
                    text: format!(
                        "[steering withdrawn before model received it]\n{}",
                        payload.text
                    ),
                });
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
            "voice.transcript" => self.voice_transcript(record),
            "managed.final_message" => self.managed_final_message(record),
            "managed.turn_stopped" => self.managed_turn_stopped(record),
            "display.error" => self.decode_local::<DisplayError>(record).map(|payload| {
                self.push(EntryKind::Error {
                    message: payload.message,
                });
            }),
            "worker.stopped" => self.decode_local::<WorkerStopped>(record).map(|payload| {
                if let Some(error) = payload.error {
                    self.pending_error = Some(error);
                }
            }),
            "session.ended" => self.decode_local::<SessionEnded>(record).map(|payload| {
                if payload.outcome == "failed" {
                    self.finish_failed(payload.error, None);
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
            code_display_result: None,
        }));
        self.local_shells.insert(payload.id, id);
        self.running_tools.insert(id);
    }

    fn shell_finished(&mut self, payload: ShellFinished) {
        let Some(id) = self.local_shells.remove(&payload.id) else {
            return;
        };
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
        self.tool_owners.remove(&id);
    }

    fn apply_agent(&mut self, record: &TranscriptRecord) -> ModelChange {
        // Durable terminals close the root run even if its lifecycle telemetry
        // arrives late. Background tool results, message content, and child runs
        // still have independent work to contribute.
        if self.ignores_finished_run_event(record) {
            return ModelChange::default();
        }
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
            let scope = RunScope::new(record);
            self.reasoning.retain(|key, _| key.scope != scope);
        }
        let result = match record.kind() {
            "assistant.delta" => self.assistant_delta(record),
            "assistant.message" => self.assistant_message(record),
            "reasoning.summary.delta" => self.reasoning_delta(record),
            "run.started" => {
                let scope = RunScope::new(record);
                // A retry is a new run. Keep the previous failure in history,
                // but do not let a later terminal rewrite that earlier attempt.
                self.run_failure_entries.retain(|previous, _| {
                    previous.turn != scope.turn || previous.child != scope.child
                });
                self.active_runs.push_back(ActiveRun {
                    scope,
                    started_at_unix_ms: record.recorded_at_unix_ms(),
                });
                self.set_run_status(record, Some(TransientStatus::Thinking));
                Ok(true)
            }
            "run.error" => self.decode_local::<RunError>(record).map(|payload| {
                self.set_pending_error(record, payload.message.clone());
                self.set_run_status(record, Some(TransientStatus::Error(payload.message)));
                true
            }),
            "run.completed" => {
                self.complete_turn(record);
                Ok(true)
            }
            "run.failed" => {
                self.remove_run(record);
                self.finish_failed(None, Some(&RunScope::new(record)));
                Ok(true)
            }
            "tool.call" => self.tool_call(record),
            "tool.result" => self.tool_result(record),
            "model.warmup.started" => {
                self.set_run_status(record, Some(TransientStatus::Warming));
                Ok(true)
            }
            "model.warmup.completed" => {
                self.set_run_status(
                    record,
                    self.is_active().then_some(TransientStatus::Thinking),
                );
                Ok(true)
            }
            "model.warmup.failed"
            | "model.call.failed"
            | "model.attempt.failed"
            | "model.connection.failed" => self.capture_error(record),
            "model.call.started" => {
                self.materialize_compaction_failure(&RunScope::new(record));
                self.set_run_status(record, Some(TransientStatus::Thinking));
                Ok(true)
            }
            "model.call.completed" => {
                self.set_run_status(
                    record,
                    self.is_active().then_some(TransientStatus::Thinking),
                );
                Ok(true)
            }
            "model.compaction.started" => {
                self.set_run_status(record, Some(TransientStatus::Compacting));
                Ok(true)
            }
            "model.compaction.completed" => self.compaction_completed(record),
            "model.compaction.failed" => self.compaction_failed(record),
            "model.attempt.retrying" => self.retrying(record),
            "model.connection.started" => self.connection_started(record),
            "model.connection.completed" => {
                self.set_run_status(
                    record,
                    self.is_active().then_some(TransientStatus::Thinking),
                );
                self.take_pending_error(Some(&RunScope::new(record)));
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

    fn completed_managed_answer(&self, call: &AssistantCallKey) -> Option<EntryId> {
        if call.child.is_some() || call.phase != MessagePhase::Final {
            return None;
        }
        call.turn
            .as_ref()
            .and_then(|turn| self.managed_final_messages.get(turn))
            .copied()
    }

    fn track_managed_answer(&mut self, call: &AssistantCallKey, id: EntryId) {
        if call.child.is_none()
            && call.phase == MessagePhase::Final
            && let Some(turn) = &call.turn
        {
            self.managed_answer_entries
                .entry(turn.clone())
                .or_default()
                .insert(id);
        }
    }

    fn finish_managed_activity(&mut self, turn_id: &str) {
        self.run_failure_entries
            .retain(|scope, _| scope.turn.as_deref() != Some(turn_id) || scope.child.is_some());
        self.pending_managed_errors
            .retain(|scope, _| scope.turn.as_deref() != Some(turn_id) || scope.child.is_some());
        self.pending_compaction_errors
            .retain(|scope, _| scope.turn.as_deref() != Some(turn_id) || scope.child.is_some());
        // Durable completion is authoritative even when the stream omitted its
        // run terminal or tool results. Child agents and other turns keep running.
        self.active_runs
            .retain(|run| run.scope.turn.as_deref() != Some(turn_id) || run.scope.child.is_some());
        let background = self.background_shell_ids();
        let unfinished = self
            .running_tools
            .iter()
            .copied()
            .filter(|id| !background.contains(id))
            .filter(|id| {
                self.tool_owners.get(id).is_some_and(|scope| {
                    scope.turn.as_deref() == Some(turn_id) && scope.child.is_none()
                })
            })
            .collect::<Vec<_>>();
        self.fail_unfinished_tools(&unfinished);
        self.run_activity.retain(|activity| {
            activity.scope.turn.as_deref() != Some(turn_id) || activity.scope.child.is_some()
        });
        self.refresh_transient();
    }

    fn managed_turn_stopped(&mut self, record: &TranscriptRecord) -> Result<(), serde_json::Error> {
        let payload = record.decode_payload::<ManagedTurnStopped>()?;
        let failure = self
            .run_failure_entries
            .iter()
            .filter(|(scope, _)| {
                scope.turn.as_deref() == Some(payload.turn_id.as_str()) && scope.child.is_none()
            })
            .map(|(_, id)| *id)
            .max_by_key(|id| id.index());
        self.finish_managed_activity(&payload.turn_id);
        if self.managed_stopped_turns.insert(payload.turn_id)
            && let Some(message) = payload.error
        {
            if let Some(id) = failure {
                self.update(id, |kind| *kind = EntryKind::Error { message });
            } else {
                self.push(EntryKind::Error { message });
            }
        }
        Ok(())
    }

    fn voice_transcript(&mut self, record: &TranscriptRecord) -> Result<(), serde_json::Error> {
        let caption = record.decode_payload::<crate::voice_state::Transcript>()?;
        if !matches!(caption.speaker.as_str(), "user" | "assistant") || caption.text.is_empty() {
            return Ok(());
        }
        let key = (caption.session, caption.speaker.clone(), caption.id);
        let kind = if caption.speaker == "user" {
            EntryKind::User { text: caption.text }
        } else {
            EntryKind::Assistant {
                text: format!("**Voice**\n\n{}", caption.text),
                complete: !caption.is_partial,
            }
        };
        if let Some((id, complete)) = self.voice_messages.get(&key).copied() {
            // A delayed partial cannot roll a finalized message backwards.
            if complete && caption.is_partial {
                return Ok(());
            }
            self.update(id, |entry| *entry = kind);
            self.voice_messages.insert(key, (id, !caption.is_partial));
        } else {
            let id = self.push(kind);
            self.voice_messages.insert(key, (id, !caption.is_partial));
        }
        Ok(())
    }

    fn managed_final_message(
        &mut self,
        record: &TranscriptRecord,
    ) -> Result<(), serde_json::Error> {
        let payload = record.decode_payload::<ManagedFinalMessage>()?;
        let mut scopes = self
            .pending_compaction_errors
            .keys()
            .filter(|scope| {
                scope.turn.as_deref() == Some(payload.turn_id.as_str()) && scope.child.is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        scopes.sort_by(|left, right| left.request.cmp(&right.request));
        for scope in scopes {
            self.materialize_compaction_failure(&scope);
        }
        self.finish_managed_activity(&payload.turn_id);
        self.managed_completed_turns.insert(payload.turn_id.clone());
        if payload.text.is_empty()
            || self
                .managed_final_messages
                .contains_key(payload.turn_id.as_str())
        {
            return Ok(());
        }
        let candidates = self
            .managed_answer_entries
            .remove(payload.turn_id.as_str())
            .unwrap_or_default();
        let matching = candidates.iter().filter_map(|id| {
            let index = self.index_of(*id)?;
            matches!(&self.entries[index].kind, EntryKind::Assistant { text, complete: true } if text == &payload.text).then_some((index, *id))
        }).max_by_key(|(index, _)| *index).map(|(_, id)| id);
        let unfinished = candidates
            .iter()
            .filter_map(|id| {
                let index = self.index_of(*id)?;
                matches!(
                    &self.entries[index].kind,
                    EntryKind::Assistant {
                        complete: false,
                        ..
                    }
                )
                .then_some((index, *id))
            })
            .max_by_key(|(index, _)| *index)
            .map(|(_, id)| id);
        let id = matching.or(unfinished).unwrap_or_else(|| {
            self.push(EntryKind::Assistant {
                text: String::new(),
                complete: false,
            })
        });
        self.update(id, |kind| {
            if let EntryKind::Assistant { text, complete } = kind {
                *text = payload.text;
                *complete = true;
            }
        });
        self.active_assistants.retain(|call, _| {
            call.turn.as_deref() != Some(payload.turn_id.as_str()) || call.child.is_some()
        });
        self.managed_final_messages
            .insert(Arc::from(payload.turn_id), id);
        Ok(())
    }

    fn assistant_delta(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<AssistantDelta>()?;
        let phase = message_phase(payload.phase);
        let key = AssistantKey {
            call: AssistantCallKey::new(record, payload.model_call_index, phase),
            item: payload.item_id,
        };
        if self.completed_managed_answer(&key.call).is_some() {
            return Ok(false);
        }
        let id = if let Some(&id) = self.assistants.get(&key) {
            id
        } else {
            let id = self.push(EntryKind::Assistant {
                text: String::new(),
                complete: false,
            });
            self.assistants.insert(key.clone(), id);
            self.active_assistants.insert(key.call.clone(), key.clone());
            id
        };
        if self.index_of(id).is_some_and(|index| {
            matches!(
                self.entries[index].kind,
                EntryKind::Assistant { complete: true, .. }
            )
        }) {
            return Ok(false);
        }
        self.track_managed_answer(&key.call, id);
        self.update(id, |kind| {
            if let EntryKind::Assistant { text, .. } = kind {
                text.push_str(&payload.text);
            }
        });
        self.set_run_status(record, Some(TransientStatus::Responding));
        Ok(true)
    }

    fn assistant_message(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<AssistantMessage>()?;
        let phase = message_phase(payload.phase);
        let key = AssistantKey {
            call: AssistantCallKey::new(record, payload.model_call_index, phase),
            item: payload.item_id,
        };
        if let Some(id) = self.completed_managed_answer(&key.call) {
            self.assistants.insert(key, id);
            return Ok(false);
        }
        let id = self
            .assistants
            .get(&key)
            .copied()
            .or_else(|| {
                self.active_assistants
                    .get(&key.call)
                    .filter(|candidate| candidate.item.is_none() || key.item.is_none())
                    .and_then(|candidate| self.assistants.get(candidate))
                    .copied()
                    .filter(|id| {
                        self.index_of(*id).is_some_and(|index| {
                            matches!(
                                self.entries[index].kind,
                                EntryKind::Assistant {
                                    complete: false,
                                    ..
                                }
                            )
                        })
                    })
            })
            .unwrap_or_else(|| {
                self.push(EntryKind::Assistant {
                    text: String::new(),
                    complete: false,
                })
            });
        self.track_managed_answer(&key.call, id);
        // Keep aliases when a stream initially lacked an item ID, so repeated
        // final messages still update the same entry. Only open streams qualify
        // for fallback; another answer must never overwrite a completed one.
        self.assistants.insert(key.clone(), id);
        if self
            .active_assistants
            .get(&key.call)
            .and_then(|candidate| self.assistants.get(candidate))
            .copied()
            == Some(id)
        {
            self.active_assistants.remove(&key.call);
        }
        self.update(id, |kind| {
            if let EntryKind::Assistant { text, complete, .. } = kind {
                *text = payload.text;
                *complete = true;
            }
        });
        self.set_run_status(
            record,
            self.is_active().then_some(TransientStatus::Thinking),
        );
        Ok(true)
    }

    fn reasoning_delta(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ReasoningSummaryDelta>()?;
        let key = ReasoningKey {
            scope: RunScope::new(record),
            call: payload.model_call_index,
        };
        // Other turns and local updates may have added rows since this summary
        // began. Keep streaming into its own entry instead of starting a fragment.
        let id = self.reasoning.get(&key).copied().unwrap_or_else(|| {
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
        // Recovery may replay admission for a call whose progress is already visible.
        if self.tools.contains_key(&call_id) {
            return Ok(false);
        }
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
                self.tool_owners.insert(id, RunScope::new(record));
                self.set_run_status(record, Some(TransientStatus::Tool("Shell".to_owned())));
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
                code_display_result: None,
            }),
            hidden,
            parent,
        );
        if let Some(parent) = parent {
            self.register_code_child(parent, id);
        }
        self.tools.insert(call_id, id);
        self.running_tools.insert(id);
        self.tool_owners.insert(id, RunScope::new(record));
        self.set_run_status(record, Some(transient));
        Ok(true)
    }

    fn tool_result(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ToolResultPayload>()?;
        if !self.settled_calls.insert(payload.call_id.clone()) {
            return Ok(false);
        }
        let resumed_shell = self.shell_followups.remove(&payload.call_id);
        let shell_followup = payload.tool == "write_stdin";
        let result = preferred_result(payload.structured_result, payload.result);
        let resumed_result = resumed_shell.map(|_| result.clone());
        let nested_shell_followup = resumed_shell.is_some();
        let state = tool_result_state(&payload.tool, &payload.status, &result);
        let entry_state = if resumed_shell.is_some() && state == ToolState::Yielded {
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
                        code_display_result: None,
                    }),
                    false,
                    parent,
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
                } else if payload.tool == "exec_command" {
                    merge_shell_result(None, result)
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
            if state != ToolState::Yielded {
                self.shell_sessions.retain(|_, entry| *entry != shell);
            }
            self.running_tools.remove(&shell);
            self.tool_owners.remove(&shell);
        }
        if payload.tool == "wait"
            && state == ToolState::Failed
            && let Some(index) = self.index_of(id)
        {
            self.entries[index].hidden = false;
        }
        // Keep poll correlation independently of the RPC activity/timer.
        if state == ToolState::Yielded {
            if let Some(session_id) = shell_session {
                self.shell_sessions.insert(session_id, id);
            }
        } else {
            self.shell_sessions
                .retain(|_, shell_entry| *shell_entry != id);
        }
        if entry_state == ToolState::Running {
            self.running_tools.insert(id);
            self.tool_owners.insert(id, RunScope::new(record));
        } else {
            self.running_tools.remove(&id);
            self.tool_owners.remove(&id);
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
        // A child may finish after the exec envelope, or receive shell follow-up
        // output. Refresh the parent's projection without changing its raw result.
        let code_parents: Vec<_> = self
            .code_children
            .iter()
            .filter(|(parent, children)| {
                **parent == id
                    || children.contains(&id)
                    || resumed_shell.is_some_and(|shell| children.contains(&shell))
            })
            .map(|(parent, _)| *parent)
            .collect();
        for parent in code_parents {
            self.refresh_code_display_result(parent);
        }
        self.set_run_status(
            record,
            self.is_active().then_some(TransientStatus::Thinking),
        );
        Ok(true)
    }

    fn refresh_code_display_result(&mut self, parent: EntryId) {
        let Some(TranscriptEntry {
            kind: EntryKind::Tool(tool),
            ..
        }) = self.entry(parent)
        else {
            return;
        };
        let Some(result) = tool.result.as_ref() else {
            return;
        };
        let children: Vec<_> = self
            .code_children
            .get(&parent)
            .into_iter()
            .flatten()
            .filter_map(|child| match &self.entry(*child)?.kind {
                EntryKind::Tool(tool) => tool.result.as_ref(),
                _ => None,
            })
            .collect();
        let display = distinct_code_output(result, &children);
        if tool.code_display_result.as_ref() != Some(&display) {
            self.update(parent, |kind| {
                if let EntryKind::Tool(tool) = kind {
                    tool.code_display_result = Some(display);
                }
            });
        }
    }

    fn compaction_completed(
        &mut self,
        record: &TranscriptRecord,
    ) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<CompactionCompleted>()?;
        self.push(EntryKind::ContextCompacted {
            duration_ns: payload.duration_ns,
        });
        self.set_run_status(
            record,
            self.is_active().then_some(TransientStatus::Thinking),
        );
        Ok(true)
    }

    fn compaction_failed(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<CompactionFailed>()?;
        self.pending_compaction_errors
            .insert(RunScope::new(record), payload.error.clone());
        self.set_pending_error(record, payload.error);
        self.set_run_status(
            record,
            self.is_active().then_some(TransientStatus::Thinking),
        );
        Ok(true)
    }

    fn retrying(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<RetryPayload>()?;
        self.set_pending_error(record, payload.error);
        self.set_run_status(record, Some(TransientStatus::Retrying(payload.delay_ns)));
        Ok(true)
    }

    fn connection_started(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ConnectionPayload>()?;
        self.set_run_status(
            record,
            Some(if payload.purpose == "reconnect" {
                TransientStatus::Reconnecting
            } else {
                TransientStatus::Connecting
            }),
        );
        Ok(true)
    }

    fn set_pending_error(&mut self, record: &TranscriptRecord, message: String) {
        let scope = RunScope::new(record);
        if self.is_finished_managed_run(&scope) {
            return;
        }
        if scope.turn.is_some() {
            self.pending_managed_errors.insert(scope, message);
        } else {
            self.pending_error = Some(message);
        }
    }

    fn take_pending_error(&mut self, scope: Option<&RunScope>) -> Option<String> {
        if let Some(scope) = scope.filter(|scope| scope.turn.is_some()) {
            self.pending_managed_errors.remove(scope)
        } else {
            self.pending_error.take()
        }
    }

    fn capture_error(&mut self, record: &TranscriptRecord) -> Result<bool, serde_json::Error> {
        let payload = record.decode_payload::<ErrorPayload>()?;
        self.set_pending_error(record, payload.error);
        Ok(false)
    }

    fn finish_success(&mut self, scope: &RunScope) {
        self.run_failure_entries.remove(scope);
        self.materialize_compaction_failure(scope);
        self.finish_activity(Some(scope));
        self.take_pending_error(Some(scope));
    }

    fn remove_run(&mut self, record: &TranscriptRecord) -> Option<u64> {
        let scope = RunScope::new(record);
        let index = self.active_runs.iter().position(|run| run.scope == scope)?;
        self.active_runs
            .remove(index)
            .map(|run| run.started_at_unix_ms)
    }

    fn complete_turn(&mut self, record: &TranscriptRecord) {
        let payload_duration_ns = record
            .decode_payload::<RunDurationPayload>()
            .ok()
            .and_then(|payload| payload.duration_ns);
        let recorded_duration_ns = self.remove_run(record).map(|started_at| {
            record
                .recorded_at_unix_ms()
                .saturating_sub(started_at)
                .saturating_mul(1_000_000)
        });
        let duration_ns = payload_duration_ns.or(recorded_duration_ns);
        self.finish_success(&RunScope::new(record));
        let Some(duration_ns) = duration_ns else {
            return;
        };
        self.push(EntryKind::TurnCompleted { duration_ns });
    }

    fn finish_failed(&mut self, error: Option<String>, scope: Option<&RunScope>) {
        if let Some(scope) = scope {
            self.pending_compaction_errors.remove(scope);
        } else {
            self.pending_compaction_errors.clear();
        }
        let pending = self.take_pending_error(scope);
        if error.is_none()
            && pending.is_none()
            && scope.is_none_or(|scope| scope.turn.is_none())
            && self
                .entries
                .last()
                .is_some_and(|entry| matches!(entry.kind, EntryKind::Error { .. }))
        {
            self.finish_activity(scope);
            return;
        }
        let message = error.or(pending);
        if let Some(scope) = scope.filter(|scope| scope.turn.is_some()) {
            // Only a failure from this run can be revised or deduplicated. In
            // particular, identical adjacent child/other-turn errors are distinct.
            if !self.is_finished_managed_run(scope) {
                if let Some(id) = self.run_failure_entries.get(scope).copied() {
                    if let Some(message) = message {
                        self.update(id, |kind| *kind = EntryKind::Error { message });
                    }
                } else {
                    let message = message.unwrap_or_else(|| "The agent run failed".to_owned());
                    let id = self.push(EntryKind::Error { message });
                    self.run_failure_entries.insert(scope.clone(), id);
                }
            }
        } else {
            let message = message.unwrap_or_else(|| "The agent run failed".to_owned());
            if !self.entries.last().is_some_and(|entry| {
                matches!(&entry.kind, EntryKind::Error { message: existing } if existing == &message)
            }) {
                self.push(EntryKind::Error { message });
            }
        }
        self.finish_activity(scope);
    }

    fn finish_activity(&mut self, scope: Option<&RunScope>) {
        if let Some(scope) = scope.filter(|scope| scope.turn.is_some()) {
            if !self.active_runs.iter().any(|run| &run.scope == scope) {
                let background = self.background_shell_ids();
                let unfinished = self
                    .running_tools
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.tool_owners.get(id) == Some(scope) && !background.contains(id)
                    })
                    .collect::<Vec<_>>();
                self.fail_unfinished_tools(&unfinished);
            }
        } else if self.active_runs.is_empty() {
            self.fail_orphaned_tools();
        }
        if let Some(scope) = scope {
            if !self.active_runs.iter().any(|run| &run.scope == scope) {
                self.run_activity
                    .retain(|activity| &activity.scope != scope);
            }
        } else if !self.is_active() {
            self.run_activity.clear();
        }
        self.refresh_transient();
    }

    fn background_shell_ids(&self) -> HashSet<EntryId> {
        // Process sessions belong to the runtime, not the turn that launched
        // or polled them. A returned session ID remains usable across turns.
        self.shell_sessions.values().copied().collect()
    }

    fn fail_orphaned_tools(&mut self) {
        let background = self.background_shell_ids();
        let local_shells = self.local_shells.values().copied().collect::<HashSet<_>>();
        let orphaned = self
            .running_tools
            .iter()
            .copied()
            .filter(|id| !local_shells.contains(id) && !background.contains(id))
            .collect::<Vec<_>>();
        self.fail_unfinished_tools(&orphaned);
    }

    fn fail_unfinished_code_children(&mut self, parent: EntryId) {
        let background = self.background_shell_ids();
        let unfinished = self
            .code_children
            .get(&parent)
            .into_iter()
            .flatten()
            .filter(|id| self.running_tools.contains(id) && !background.contains(id))
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
            self.tool_owners.remove(id);
        }
        self.shell_sessions.retain(|_, id| !unfinished.contains(id));
        self.shell_followups
            .retain(|_, id| !unfinished.contains(id));
    }

    pub(crate) fn agent_stream_closed(&mut self) -> bool {
        let changed = !self.active_runs.is_empty()
            || self.running_tools.iter().any(|id| {
                !self
                    .local_shells
                    .values()
                    .any(|local_shell| local_shell == id)
            });
        self.active_runs.clear();
        self.fail_orphaned_tools();
        self.run_activity.clear();
        self.refresh_transient();
        changed
    }

    fn materialize_compaction_failure(&mut self, scope: &RunScope) {
        let Some(message) = self.pending_compaction_errors.remove(scope) else {
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
            self.push(EntryKind::Error {
                message: message.clone(),
            });
        }
        if !visible || record.managed_turn_id().is_some() {
            self.set_pending_error(record, message);
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
        self.entries[child_index].parent = Some(parent);
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

// Only remove whole emitted items: additional commentary or discovery output
// must survive even when another item repeats a child's result.
fn distinct_code_output(result: &Value, children: &[&Value]) -> Value {
    // Each child accounts for one echo; additional identical emits are retained.
    let mut remaining = children.to_vec();
    let mut duplicate = |item: &Value| {
        let Some(index) = remaining.iter().position(|child| {
            values_duplicate(item, child)
                || item
                    .as_str()
                    .or_else(|| item.get("text").and_then(Value::as_str))
                    .is_some_and(|text| text_duplicates_value(code_mode_output_text(text), child))
        }) else {
            return false;
        };
        remaining.remove(index);
        true
    };
    if duplicate(result) {
        return Value::Null;
    }
    match result {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter(|item| !duplicate(item))
                .cloned()
                .collect(),
        ),
        _ => result.clone(),
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
    if tool_session_id(result).is_some()
        && result.get("exit_code").and_then(Value::as_i64).is_none()
    {
        return ToolState::Yielded;
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
    let previous_output = current
        .as_ref()
        .and_then(|value| value.get("output"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if previous_output.is_empty() && next.get("output").is_none() {
        return next;
    }
    let mut next = match next {
        Value::Object(fields) => fields,
        other => serde_json::Map::from_iter([("error".to_owned(), other)]),
    };
    let output = next
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let output = format!("{previous_output}{output}");
    let mut start = output.len().saturating_sub(64 * 1024);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let output = if start == 0 {
        output
    } else {
        format!("…\n{}", &output[start..])
    };
    next.insert("output".to_owned(), Value::String(output));
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
struct ManagedTurnStopped {
    turn_id: String,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ManagedFinalMessage {
    turn_id: String,
    text: String,
}

#[derive(Deserialize)]
struct DisplayError {
    message: String,
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
    #[serde(default)]
    duration_ns: u64,
    #[serde(default)]
    result: Value,
    #[serde(default)]
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
    use super::{EntryKind, ToolState, TranscriptModel, TranscriptRecord, distinct_code_output};
    use nanocodex::agent::events::{AgentEvent, AgentEventKind};
    use serde_json::{Value, json, value::to_raw_value};
    use std::sync::Arc;

    #[test]
    fn voice_snapshots_remain_inline_complete_and_distinct_across_speakers_and_calls() {
        use crate::{tui::transcript::LocalEvent, voice_state::Transcript};
        let mut model = TranscriptModel::default();
        let long = "A long spoken answer. ".repeat(200);
        for (seq, session, speaker, id, text, partial) in [
            (1, "first", "user", 0, "Check", true),
            (2, "first", "assistant", 0, "Checking", true),
            (3, "first", "user", 0, "Check Omarchy", false),
            (4, "first", "assistant", 0, long.as_str(), false),
            (5, "first", "assistant", 0, "late partial", true),
            (6, "first", "assistant", 0, long.as_str(), false),
            (7, "first", "user", 1, "Check Omarchy", false),
            (8, "second", "assistant", 0, "New call", false),
        ] {
            model.apply(
                &TranscriptRecord::from_local(
                    seq,
                    0,
                    LocalEvent::VoiceTranscript(Transcript {
                        session: session.into(),
                        speaker: speaker.into(),
                        id,
                        text: text.into(),
                        is_partial: partial,
                    }),
                )
                .unwrap(),
            );
        }
        assert_eq!(model.entries().len(), 4);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::User { text } if text == "Check Omarchy")
        );
        assert!(
            matches!(&model.entries()[1].kind, EntryKind::Assistant { text, complete: true } if text.ends_with(&long))
        );
        assert!(
            matches!(&model.entries()[2].kind, EntryKind::User { text } if text == "Check Omarchy")
        );
        assert!(
            matches!(&model.entries()[3].kind, EntryKind::Assistant { text, complete: true } if text.ends_with("New call"))
        );
    }

    #[test]
    fn managed_failure_settles_once_with_or_without_nested_terminal() {
        use crate::tui::transcript::{LocalEvent, TurnId};
        for nested_terminal in [false, true] {
            let mut model = TranscriptModel::default();
            model.apply(
                &agent_record(1, AgentEventKind::RunStarted, json!({}))
                    .with_managed_turn_id(Some("failed-turn")),
            );
            if nested_terminal {
                model.apply(
                    &agent_record(2, AgentEventKind::RunFailed, json!({}))
                        .with_managed_turn_id(Some("failed-turn")),
                );
            }
            model.apply(
                &TranscriptRecord::from_local(
                    3,
                    30,
                    LocalEvent::UserSubmitted {
                        id: TurnId::new(3),
                        text: "queued followup".to_owned(),
                    },
                )
                .unwrap(),
            );
            model.apply(
                &TranscriptRecord::from_local(
                    4,
                    40,
                    LocalEvent::ManagedTurnStopped {
                        turn_id: "failed-turn".to_owned(),
                        error: Some("authoritative restore failure".to_owned()),
                    },
                )
                .unwrap(),
            );
            let errors = model
                .entries()
                .iter()
                .filter_map(|entry| match &entry.kind {
                    EntryKind::Error { message } => Some(message.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(errors, ["authoritative restore failure"]);
            assert!(model.active_runs.is_empty());
            model.apply(&agent_record(5, AgentEventKind::RunStarted, json!({})));
            assert_eq!(model.active_runs.len(), 1);
            model.apply(&agent_record(6, AgentEventKind::RunCompleted, json!({})));
            assert!(matches!(
                model.entries().last().unwrap().kind,
                EntryKind::TurnCompleted {
                    duration_ns: 10_000_000,
                }
            ));
            assert!(model.active_runs.is_empty());
        }
    }

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

    fn durable_answer(sequence: u64, turn: &str, text: &str) -> TranscriptRecord {
        TranscriptRecord::from_local(
            sequence,
            sequence,
            crate::tui::transcript::LocalEvent::ManagedFinalMessage {
                turn_id: turn.to_owned(),
                text: text.to_owned(),
            },
        )
        .unwrap()
    }

    #[test]
    fn yielded_processes_keep_their_identity_across_turns_and_stream_closure() {
        for terminal in [
            "stream_completed",
            "durable_completed",
            "failed",
            "cancelled",
            "stream_closed",
            "unscoped_completed",
        ] {
            let mut model = TranscriptModel::default();
            let scope = (terminal != "unscoped_completed").then_some("first");
            model.apply(
                &agent_record(1, AgentEventKind::RunStarted, json!({})).with_managed_turn_id(scope),
            );
            model.apply(
                &call(2, "build", "exec_command", json!({"cmd": "build"}))
                    .with_managed_turn_id(scope),
            );
            model.apply(
                &result(
                    3,
                    "build",
                    "exec_command",
                    json!({"session_id": 7, "exit_code": null, "output": "started\n"}),
                    Value::Null,
                    Value::Null,
                )
                .with_managed_turn_id(scope),
            );
            model.apply(
                &call(4, "orphan", "read_file", json!({"path": "file"}))
                    .with_managed_turn_id(scope),
            );
            match terminal {
                "stream_completed" | "unscoped_completed" => {
                    model.apply(
                        &agent_record(5, AgentEventKind::RunCompleted, json!({}))
                            .with_managed_turn_id(scope),
                    );
                }
                "durable_completed" => {
                    model.apply(&durable_answer(5, "first", "running in background"));
                }
                "failed" | "cancelled" => {
                    model.apply(
                        &TranscriptRecord::from_local(
                            5,
                            50,
                            crate::tui::transcript::LocalEvent::ManagedTurnStopped {
                                turn_id: "first".to_owned(),
                                error: (terminal == "failed").then(|| "turn failed".to_owned()),
                            },
                        )
                        .unwrap(),
                    );
                }
                "stream_closed" => {
                    model.agent_stream_closed();
                }
                _ => unreachable!(),
            }
            assert!(!model.is_active(), "{terminal}");
            let states = model
                .entries()
                .iter()
                .filter_map(|entry| match &entry.kind {
                    EntryKind::Tool(tool) => Some(tool.state),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                states,
                [ToolState::Yielded, ToolState::Failed],
                "{terminal}"
            );
            model.apply(
                &agent_record(6, AgentEventKind::RunStarted, json!({}))
                    .with_managed_turn_id(Some("next")),
            );
            model.apply(
                &call(7, "poll", "write_stdin", json!({"session_id": 7}))
                    .with_managed_turn_id(Some("next")),
            );
            model.apply(
                &result(
                    8,
                    "poll",
                    "write_stdin",
                    json!({"session_id": 7, "exit_code": 0, "output": "finished\n"}),
                    Value::Null,
                    Value::Null,
                )
                .with_managed_turn_id(Some("next")),
            );
            let tools = model
                .entries()
                .iter()
                .filter_map(|entry| match &entry.kind {
                    EntryKind::Tool(tool) => Some(tool),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                tools.len(),
                2,
                "polling must update the original command: {terminal}"
            );
            assert_eq!(tools[0].state, ToolState::Succeeded);
            assert_eq!(
                tools[0].result.as_ref().unwrap()["output"],
                "started\nfinished\n"
            );
            assert!(tools[0].result.as_ref().unwrap().get("error").is_none());
        }
    }

    #[test]
    fn terminating_a_code_cell_preserves_its_registered_background_shell() {
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
            json!({"cmd": "background build"}),
        ));
        model.apply(&result(
            3,
            "outer/code-0",
            "exec_command",
            json!({"session_id": 7, "output": "started\n"}),
            Value::Null,
            Value::Null,
        ));
        model.apply(&call(
            4,
            "outer/code-1",
            "read_file",
            json!({"path": "file"}),
        ));
        model.apply(&result(
            5,
            "outer",
            "exec",
            json!("Script running with cell ID cell-1\nOutput:\n"),
            Value::Null,
            Value::Null,
        ));
        model.apply(&call(
            6,
            "terminate",
            "wait",
            json!({"cell_id": "cell-1", "terminate": true}),
        ));
        model.apply(&result(
            7,
            "terminate",
            "wait",
            json!("Script terminated\nOutput:\n"),
            Value::Null,
            Value::Null,
        ));
        assert_eq!(model.running_tool_ids().count(), 0);
        assert!(
            matches!(&model.entries()[1].kind, EntryKind::Tool(tool) if tool.state == ToolState::Yielded)
        );
        assert!(
            matches!(&model.entries()[2].kind, EntryKind::Tool(tool) if tool.state == ToolState::Failed)
        );
        model.apply(&call(8, "poll", "write_stdin", json!({"session_id": 7})));
        model.apply(&result(
            9,
            "poll",
            "write_stdin",
            json!({"exit_code": 0, "output": "finished\n"}),
            Value::Null,
            Value::Null,
        ));
        let EntryKind::Tool(shell) = &model.entries()[1].kind else {
            panic!("expected original shell")
        };
        assert_eq!(shell.state, ToolState::Succeeded);
        assert_eq!(
            shell.result.as_ref().unwrap()["output"],
            "started\nfinished\n"
        );
        assert!(!model.has_running_tools());
    }

    #[test]
    fn ending_one_run_restores_the_other_runs_retry_status() {
        use super::TransientStatus;
        for terminal in 0..5 {
            let mut model = TranscriptModel::default();
            model.apply(
                &agent_record(1, AgentEventKind::RunStarted, json!({}))
                    .with_managed_turn_id(Some("retrying")),
            );
            model.apply(
                &agent_record(
                    2,
                    AgentEventKind::ModelAttemptRetrying,
                    json!({"delay_ns": 10_000_000_000_u64, "error": "retry"}),
                )
                .with_managed_turn_id(Some("retrying")),
            );
            model.apply(
                &agent_record(3, AgentEventKind::RunStarted, json!({}))
                    .with_managed_turn_id(Some("other")),
            );
            assert_eq!(
                model.transient(),
                Some(&TransientStatus::Retrying(10_000_000_000)),
                "generic thinking should not hide a retry"
            );
            model.apply(
                &agent_record(4, AgentEventKind::ModelWarmupStarted, json!({}))
                    .with_managed_turn_id(Some("other")),
            );
            assert_eq!(model.transient(), Some(&TransientStatus::Warming));
            match terminal {
                0 | 1 => {
                    model.apply(
                        &agent_record(
                            5,
                            if terminal == 0 {
                                AgentEventKind::RunCompleted
                            } else {
                                AgentEventKind::RunFailed
                            },
                            json!({}),
                        )
                        .with_managed_turn_id(Some("other")),
                    );
                }
                2 => {
                    model.apply(&durable_answer(5, "other", "answer"));
                }
                _ => {
                    model.apply(
                        &TranscriptRecord::from_local(
                            5,
                            50,
                            crate::tui::transcript::LocalEvent::ManagedTurnStopped {
                                turn_id: "other".to_owned(),
                                error: (terminal == 3).then(|| "failure".to_owned()),
                            },
                        )
                        .unwrap(),
                    );
                }
            }
            assert_eq!(
                model.transient(),
                Some(&TransientStatus::Retrying(10_000_000_000)),
                "terminal={terminal}"
            );
            assert_eq!(model.transient_retry_origin(), Some((2, 20)));
            model.apply(&durable_answer(6, "retrying", "finished"));
            assert_eq!(model.transient(), None);
            assert_eq!(model.transient_retry_origin(), None);
        }
    }

    #[test]
    fn parent_completion_keeps_child_activity_and_stream_closure_clears_it() {
        use super::TransientStatus;
        let mut model = TranscriptModel::default();
        model.apply(
            &agent_record(1, AgentEventKind::RunStarted, json!({}))
                .with_managed_turn_id(Some("turn")),
        );
        model.apply(
            &agent_record(2, AgentEventKind::RunStarted, json!({}))
                .with_managed_turn_id(Some("turn"))
                .with_managed_agent_id(Some(7)),
        );
        model.apply(
            &call(3, "child-tool", "read_file", json!({"path": "file"}))
                .with_managed_turn_id(Some("turn"))
                .with_managed_agent_id(Some(7)),
        );
        let child_status = model.transient().cloned();
        assert!(matches!(child_status, Some(TransientStatus::Tool(_))));
        model.apply(
            &agent_record(4, AgentEventKind::ModelWarmupStarted, json!({}))
                .with_managed_turn_id(Some("turn")),
        );
        model.apply(&durable_answer(5, "turn", "parent finished"));
        assert_eq!(model.transient(), child_status.as_ref());
        model.agent_stream_closed();
        assert_eq!(model.transient(), None);
    }

    #[test]
    fn progress_summaries_keep_their_scope_and_identity_across_unrelated_updates() {
        use crate::tui::transcript::{LocalEvent, ShellId};
        for replay in [false, true] {
            let apply = |model: &mut TranscriptModel, record: TranscriptRecord| {
                let record = if replay {
                    serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap()
                } else {
                    record
                };
                model.apply(&record);
            };
            let summary = |sequence, turn, child, call, text| {
                agent_record(
                    sequence,
                    AgentEventKind::ReasoningSummaryDelta,
                    json!({"model_call_index": call, "text": text}),
                )
                .with_managed_turn_id(Some(turn))
                .with_managed_agent_id(child)
            };
            let mut model = TranscriptModel::default();
            for (sequence, turn, child, text) in [
                (1, "first", None, "A start "),
                (2, "second", None, "B start "),
                (3, "first", Some(7), "C start "),
            ] {
                apply(&mut model, summary(sequence, turn, child, 1, text));
            }
            assert_eq!(model.entries().len(), 3);
            let ids = model
                .entries()
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>();
            apply(
                &mut model,
                call(4, "other-child-call", "read_file", json!({"path": "file"}))
                    .with_managed_turn_id(Some("first"))
                    .with_managed_agent_id(Some(8)),
            );
            apply(
                &mut model,
                TranscriptRecord::from_local(
                    5,
                    50,
                    LocalEvent::ShellStarted {
                        id: ShellId::new(1),
                        command: "printf local".to_owned(),
                        workspace: "/tmp".into(),
                    },
                )
                .unwrap(),
            );
            apply(
                &mut model,
                TranscriptRecord::from_local(
                    6,
                    60,
                    LocalEvent::ShellFinished {
                        id: ShellId::new(1),
                        output: "local".to_owned(),
                        exit_code: Some(0),
                        duration_ns: 1,
                        truncated: false,
                        error: None,
                    },
                )
                .unwrap(),
            );
            apply(
                &mut model,
                TranscriptRecord::from_local(
                    7,
                    70,
                    LocalEvent::UserSteered {
                        text: "continue".to_owned(),
                    },
                )
                .unwrap(),
            );
            for (sequence, turn, child, text) in [
                (8, "first", None, "A end"),
                (9, "second", None, "B end"),
                (10, "first", Some(7), "C end"),
            ] {
                apply(&mut model, summary(sequence, turn, child, 1, text));
            }
            for (id, expected) in
                ids.iter()
                    .zip(["A start A end", "B start B end", "C start C end"])
            {
                let entry = model.entry(*id).unwrap();
                assert!(matches!(&entry.kind, EntryKind::Reasoning { text } if text == expected));
                assert!(entry.revision > 1);
            }
            // An owning run's content boundary still starts a new summary segment.
            apply(
                &mut model,
                agent_record(
                    11,
                    AgentEventKind::AssistantMessage,
                    json!({"model_call_index": 1, "phase": "commentary", "text": "A update"}),
                )
                .with_managed_turn_id(Some("first")),
            );
            apply(&mut model, summary(12, "first", None, 1, "A new segment"));
            apply(&mut model, summary(13, "second", None, 1, " B tail"));
            apply(&mut model, summary(14, "second", None, 2, "B next call"));
            apply(&mut model, summary(15, "first", Some(7), 1, " C tail"));
            let summaries = model
                .entries()
                .iter()
                .filter_map(|entry| match &entry.kind {
                    EntryKind::Reasoning { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                summaries,
                [
                    "A start A end",
                    "B start B end B tail",
                    "C start C end C tail",
                    "A new segment",
                    "B next call"
                ]
            );
        }
    }

    #[test]
    fn durable_terminals_fence_lifecycle_but_keep_background_results_and_child_activity() {
        use super::TransientStatus;
        use crate::tui::transcript::LocalEvent;
        for outcome in ["completed", "empty", "failed", "cancelled"] {
            for replay in [false, true] {
                let apply = |model: &mut TranscriptModel, record: TranscriptRecord| {
                    let record = if replay {
                        serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap()
                    } else {
                        record
                    };
                    model.apply(&record)
                };
                let mut model = TranscriptModel::default();
                apply(
                    &mut model,
                    agent_record(1, AgentEventKind::RunStarted, json!({}))
                        .with_managed_turn_id(Some("done")),
                );
                apply(
                    &mut model,
                    call(2, "background", "exec_command", json!({"cmd": "build"}))
                        .with_managed_turn_id(Some("done")),
                );
                apply(
                    &mut model,
                    result(
                        3,
                        "background",
                        "exec_command",
                        json!({"session_id": 7, "exit_code": null, "output": "started\n"}),
                        Value::Null,
                        Value::Null,
                    )
                    .with_managed_turn_id(Some("done")),
                );
                let background = model.entries()[0].id;
                apply(
                    &mut model,
                    agent_record(4, AgentEventKind::RunStarted, json!({}))
                        .with_managed_turn_id(Some("current")),
                );
                apply(
                    &mut model,
                    agent_record(
                        5,
                        AgentEventKind::ModelAttemptRetrying,
                        json!({"delay_ns": 10_000_000_000_u64, "error": "current retry"}),
                    )
                    .with_managed_turn_id(Some("current")),
                );
                let terminal = match outcome {
                    "completed" => durable_answer(6, "done", "finished"),
                    "empty" => durable_answer(6, "done", ""),
                    _ => TranscriptRecord::from_local(
                        6,
                        60,
                        LocalEvent::ManagedTurnStopped {
                            turn_id: "done".to_owned(),
                            error: (outcome == "failed").then(|| "failed".to_owned()),
                        },
                    )
                    .unwrap(),
                };
                apply(&mut model, terminal);
                let count = model.entries().len();
                let activity = model.transient().cloned();
                let retry = model.transient_retry_origin();
                for (kind, payload) in [
                    (AgentEventKind::RunStarted, json!({})),
                    (AgentEventKind::RunError, json!({"message": "late error"})),
                    (AgentEventKind::ModelWarmupStarted, json!({})),
                    (AgentEventKind::ModelCallStarted, json!({})),
                    (
                        AgentEventKind::ModelAttemptRetrying,
                        json!({"delay_ns": 60_000_000_000_u64, "error": "old retry"}),
                    ),
                    (AgentEventKind::ModelCompactionStarted, json!({})),
                    (
                        AgentEventKind::ModelCompactionCompleted,
                        json!({"duration_ns": 1}),
                    ),
                    (
                        AgentEventKind::ModelCompactionFailed,
                        json!({"after_model_call_index": 1, "duration_ns": 1, "error": "late compaction"}),
                    ),
                    (
                        AgentEventKind::ModelConnectionStarted,
                        json!({"purpose": "reconnect"}),
                    ),
                    (
                        AgentEventKind::ModelConnectionFailed,
                        json!({"error": "late connection"}),
                    ),
                    (AgentEventKind::RunFailed, json!({})),
                    (AgentEventKind::RunCompleted, json!({})),
                ] {
                    let change = apply(
                        &mut model,
                        agent_record(7, kind, payload).with_managed_turn_id(Some("done")),
                    );
                    assert!(!change.changed, "outcome={outcome}, event={kind:?}");
                    assert_eq!(model.entries().len(), count);
                    assert_eq!(model.transient(), activity.as_ref());
                    assert_eq!(model.transient_retry_origin(), retry);
                }
                // Late content and polling still contribute to history without reviving root activity.
                apply(&mut model, agent_record(8, AgentEventKind::AssistantDelta, json!({"model_call_index": 2, "phase": "commentary", "text": "late content"})).with_managed_turn_id(Some("done")));
                apply(
                    &mut model,
                    call(9, "poll", "write_stdin", json!({"session_id": 7}))
                        .with_managed_turn_id(Some("done")),
                );
                apply(
                    &mut model,
                    result(
                        10,
                        "poll",
                        "write_stdin",
                        json!({"session_id": 7, "exit_code": 0, "output": "finished\n"}),
                        Value::Null,
                        Value::Null,
                    )
                    .with_managed_turn_id(Some("done")),
                );
                let EntryKind::Tool(tool) = &model.entry(background).unwrap().kind else {
                    panic!("background command missing")
                };
                assert_eq!(tool.state, ToolState::Succeeded);
                assert_eq!(
                    tool.result.as_ref().unwrap()["output"],
                    "started\nfinished\n"
                );
                assert!(model.entries().iter().any(|entry| matches!(&entry.kind, EntryKind::Assistant { text, .. } if text == "late content")));
                assert_eq!(model.transient(), activity.as_ref());
                assert_eq!(model.transient_retry_origin(), retry);
                apply(
                    &mut model,
                    agent_record(11, AgentEventKind::RunStarted, json!({}))
                        .with_managed_turn_id(Some("done"))
                        .with_managed_agent_id(Some(7)),
                );
                apply(
                    &mut model,
                    agent_record(12, AgentEventKind::ModelWarmupStarted, json!({}))
                        .with_managed_turn_id(Some("done"))
                        .with_managed_agent_id(Some(7)),
                );
                assert!(matches!(model.transient(), Some(TransientStatus::Warming)));
                apply(
                    &mut model,
                    agent_record(13, AgentEventKind::RunCompleted, json!({}))
                        .with_managed_turn_id(Some("done"))
                        .with_managed_agent_id(Some(7)),
                );
                assert_eq!(model.transient(), activity.as_ref());
                apply(
                    &mut model,
                    durable_answer(14, "current", "current finished"),
                );
                assert!(!model.is_active());
                assert_eq!(model.transient(), None);
                assert_eq!(model.transient_retry_origin(), None);
            }
        }
    }

    #[test]
    fn durable_failure_revises_only_its_own_run_and_fences_late_failures() {
        for replay in [false, true] {
            let apply = |model: &mut TranscriptModel, record: TranscriptRecord| {
                let record = if replay {
                    serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap()
                } else {
                    record
                };
                model.apply(&record);
            };
            let mut model = TranscriptModel::default();
            for (sequence, turn, child) in
                [(1, "root", None), (3, "root", Some(7)), (5, "other", None)]
            {
                apply(
                    &mut model,
                    agent_record(
                        sequence,
                        AgentEventKind::RunError,
                        json!({"message": "same failure"}),
                    )
                    .with_managed_turn_id(Some(turn))
                    .with_managed_agent_id(child),
                );
                apply(
                    &mut model,
                    agent_record(sequence + 1, AgentEventKind::RunFailed, json!({}))
                        .with_managed_turn_id(Some(turn))
                        .with_managed_agent_id(child),
                );
            }
            assert_eq!(
                model.entries().len(),
                3,
                "identical failures from separate runs must remain distinct"
            );
            let original = model
                .entries()
                .iter()
                .map(|entry| (entry.id, entry.revision))
                .collect::<Vec<_>>();
            let durable = || {
                TranscriptRecord::from_local(
                    7,
                    70,
                    crate::tui::transcript::LocalEvent::ManagedTurnStopped {
                        turn_id: "root".to_owned(),
                        error: Some("authoritative failure".to_owned()),
                    },
                )
                .unwrap()
            };
            apply(&mut model, durable());
            assert_eq!(model.entries()[0].id, original[0].0);
            assert!(model.entries()[0].revision > original[0].1);
            let revision = model.entries()[0].revision;
            apply(&mut model, durable());
            apply(
                &mut model,
                agent_record(
                    8,
                    AgentEventKind::RunError,
                    json!({"message": "late provisional error"}),
                )
                .with_managed_turn_id(Some("root")),
            );
            apply(
                &mut model,
                agent_record(9, AgentEventKind::RunFailed, json!({}))
                    .with_managed_turn_id(Some("root")),
            );
            let messages = model
                .entries()
                .iter()
                .map(|entry| match &entry.kind {
                    EntryKind::Error { message } => message.as_str(),
                    _ => panic!("unexpected entry"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                messages,
                ["authoritative failure", "same failure", "same failure"]
            );
            assert_eq!(model.entries()[0].revision, revision);
            for (entry, original) in model.entries()[1..].iter().zip(&original[1..]) {
                assert_eq!((entry.id, entry.revision), *original);
            }
        }
    }

    #[test]
    fn durable_failure_does_not_rewrite_a_previous_retry_attempt() {
        for streamed_failure in [false, true] {
            let mut model = TranscriptModel::default();
            model.apply(
                &agent_record(
                    1,
                    AgentEventKind::RunError,
                    json!({"message": "earlier attempt"}),
                )
                .with_managed_turn_id(Some("turn")),
            );
            model.apply(
                &agent_record(2, AgentEventKind::RunFailed, json!({}))
                    .with_managed_turn_id(Some("turn")),
            );
            let retry_record = |sequence, kind, payload| {
                TranscriptRecord::from_agent(
                    sequence,
                    sequence * 10,
                    AgentEvent {
                        protocol_version: 1,
                        request_id: Arc::from("new-worker-request"),
                        seq: sequence,
                        kind,
                        payload: to_raw_value(&payload).unwrap().into(),
                    },
                )
                .with_managed_turn_id(Some("turn"))
            };
            model.apply(&retry_record(3, AgentEventKind::RunStarted, json!({})));
            if streamed_failure {
                model.apply(&retry_record(
                    4,
                    AgentEventKind::RunError,
                    json!({"message": "last attempt provisional"}),
                ));
                model.apply(&retry_record(5, AgentEventKind::RunFailed, json!({})));
                // A duplicate streaming terminal must not replace detail with a generic error.
                model.apply(&retry_record(6, AgentEventKind::RunFailed, json!({})));
                assert!(
                    matches!(&model.entries()[1].kind, EntryKind::Error { message } if message == "last attempt provisional")
                );
            }
            model.apply(
                &TranscriptRecord::from_local(
                    7,
                    70,
                    crate::tui::transcript::LocalEvent::ManagedTurnStopped {
                        turn_id: "turn".to_owned(),
                        error: Some("final attempt failure".to_owned()),
                    },
                )
                .unwrap(),
            );
            model.apply(&retry_record(8, AgentEventKind::RunFailed, json!({})));
            let messages = model
                .entries()
                .iter()
                .map(|entry| match &entry.kind {
                    EntryKind::Error { message } => message.as_str(),
                    _ => panic!("unexpected entry"),
                })
                .collect::<Vec<_>>();
            assert_eq!(messages, ["earlier attempt", "final attempt failure"]);
            assert!(!model.is_active());
        }
    }

    #[test]
    fn simultaneous_managed_runs_retain_their_own_failure_details() {
        let mut model = TranscriptModel::default();
        for (index, turn, child, message) in [
            (1, "root", None, "root error"),
            (2, "root", Some(7), "child error"),
            (3, "other", None, "other error"),
        ] {
            model.apply(
                &agent_record(index, AgentEventKind::RunError, json!({"message": message}))
                    .with_managed_turn_id(Some(turn))
                    .with_managed_agent_id(child),
            );
        }
        for (index, turn, child) in [(4, "root", None), (5, "root", Some(7)), (6, "other", None)] {
            model.apply(
                &agent_record(index, AgentEventKind::RunFailed, json!({}))
                    .with_managed_turn_id(Some(turn))
                    .with_managed_agent_id(child),
            );
        }
        let errors = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Error { message } => Some(message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(errors, ["root error", "child error", "other error"]);
    }

    #[test]
    fn root_recovery_and_terminals_do_not_clear_child_failure_details() {
        for terminal in 0..5 {
            let mut model = TranscriptModel::default();
            model.apply(
                &agent_record(
                    1,
                    AgentEventKind::RunError,
                    json!({"message": "child failure detail"}),
                )
                .with_managed_turn_id(Some("turn"))
                .with_managed_agent_id(Some(7)),
            );
            match terminal {
                0 | 1 => {
                    let kind = if terminal == 0 {
                        AgentEventKind::RunCompleted
                    } else {
                        AgentEventKind::ModelConnectionCompleted
                    };
                    model.apply(
                        &agent_record(2, kind, json!({})).with_managed_turn_id(Some("turn")),
                    );
                }
                2 => {
                    model.apply(&durable_answer(2, "turn", "root answer"));
                }
                _ => {
                    model.apply(
                        &TranscriptRecord::from_local(
                            2,
                            20,
                            crate::tui::transcript::LocalEvent::ManagedTurnStopped {
                                turn_id: "turn".to_owned(),
                                error: (terminal == 3).then(|| "root failed".to_owned()),
                            },
                        )
                        .unwrap(),
                    );
                }
            }
            model.apply(
                &agent_record(3, AgentEventKind::RunFailed, json!({}))
                    .with_managed_turn_id(Some("turn"))
                    .with_managed_agent_id(Some(7)),
            );
            assert!(
                matches!(&model.entries().last().unwrap().kind, EntryKind::Error { message } if message == "child failure detail"),
                "terminal={terminal}"
            );
        }
    }

    #[test]
    fn compaction_failures_are_materialized_by_their_own_run() {
        let mut model = TranscriptModel::default();
        for (sequence, child, message) in [
            (1, None, "root compaction"),
            (2, Some(7), "child compaction"),
        ] {
            model.apply(
                &agent_record(
                    sequence,
                    AgentEventKind::ModelCompactionFailed,
                    json!({"after_model_call_index": 1, "duration_ns": 1, "error": message}),
                )
                .with_managed_turn_id(Some("turn"))
                .with_managed_agent_id(child),
            );
        }
        model.apply(
            &agent_record(3, AgentEventKind::ModelCallStarted, json!({}))
                .with_managed_turn_id(Some("other")),
        );
        assert!(
            model.entries().is_empty(),
            "another turn must not consume a compaction warning"
        );
        model.apply(
            &agent_record(4, AgentEventKind::ModelCallStarted, json!({}))
                .with_managed_turn_id(Some("turn"))
                .with_managed_agent_id(Some(7)),
        );
        model.apply(&durable_answer(5, "turn", "root answer"));
        let warnings = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::ContextCompactionFailed { message } => Some(message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(warnings, ["child compaction", "root compaction"]);
        model.apply(
            &agent_record(6, AgentEventKind::ModelCallStarted, json!({}))
                .with_managed_turn_id(Some("turn")),
        );
        assert_eq!(
            model.entries().len(),
            3,
            "the recovered warning is shown only once"
        );
    }

    #[test]
    fn managed_failures_do_not_borrow_unscoped_worker_errors() {
        let mut model = TranscriptModel::default();
        model.apply(
            &TranscriptRecord::from_local(
                1,
                10,
                crate::tui::transcript::LocalEvent::WorkerTurnFinished {
                    id: crate::tui::transcript::TurnId::new(1),
                    error: Some("late worker failure".to_owned()),
                },
            )
            .unwrap(),
        );
        model.apply(
            &agent_record(2, AgentEventKind::RunFailed, json!({}))
                .with_managed_turn_id(Some("new turn")),
        );
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Error { message } if message == "The agent run failed")
        );
    }

    #[test]
    fn durable_completion_settles_only_its_root_run_and_tools() {
        let mut model = TranscriptModel::default();
        for (index, turn, child) in [
            (1, "parent", None),
            (2, "parent", Some(7)),
            (3, "other", None),
        ] {
            model.apply(
                &agent_record(index, AgentEventKind::RunStarted, json!({}))
                    .with_managed_turn_id(Some(turn))
                    .with_managed_agent_id(child),
            );
            model.apply(
                &call(
                    index + 3,
                    &format!("call-{index}"),
                    "read_file",
                    json!({"path": "file"}),
                )
                .with_managed_turn_id(Some(turn))
                .with_managed_agent_id(child),
            );
        }
        model.apply(
            &TranscriptRecord::from_local(
                7,
                70,
                crate::tui::transcript::LocalEvent::ShellStarted {
                    id: crate::tui::transcript::ShellId::new(1),
                    command: "local command".to_owned(),
                    workspace: std::path::PathBuf::from("/tmp"),
                },
            )
            .unwrap(),
        );
        model.apply(&durable_answer(8, "parent", ""));
        model.apply(&durable_answer(9, "parent", ""));
        // A late streamed terminal must not consume a different active run.
        model.apply(
            &agent_record(10, AgentEventKind::RunCompleted, json!({}))
                .with_managed_turn_id(Some("parent")),
        );
        let states = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Tool(tool) => Some(tool.state),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                ToolState::Failed,
                ToolState::Running,
                ToolState::Running,
                ToolState::Running
            ]
        );
        assert!(model.is_active());
        model.apply(
            &agent_record(11, AgentEventKind::RunCompleted, json!({}))
                .with_managed_turn_id(Some("parent"))
                .with_managed_agent_id(Some(7)),
        );
        assert!(model.is_active());
        model.apply(
            &agent_record(12, AgentEventKind::RunCompleted, json!({}))
                .with_managed_turn_id(Some("other")),
        );
        assert!(!model.is_active());
        assert_eq!(
            model.running_tool_ids().count(),
            1,
            "the local shell keeps running"
        );
    }

    #[test]
    fn durable_completion_settles_a_tool_even_when_run_started_was_not_retained() {
        let mut model = TranscriptModel::default();
        model.apply(
            &call(1, "call", "read_file", json!({"path": "file"}))
                .with_managed_turn_id(Some("turn")),
        );
        model.apply(&durable_answer(2, "turn", "done"));
        assert!(!model.has_running_tools());
        assert!(!model.is_active());
    }

    #[test]
    fn overlapping_run_duration_matches_its_own_start() {
        let mut model = TranscriptModel::default();
        for (sequence, turn, kind) in [
            (1, "first", AgentEventKind::RunStarted),
            (4, "second", AgentEventKind::RunStarted),
            (6, "second", AgentEventKind::RunCompleted),
            (9, "first", AgentEventKind::RunCompleted),
        ] {
            model.apply(&agent_record(sequence, kind, json!({})).with_managed_turn_id(Some(turn)));
        }
        let durations = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::TurnCompleted { duration_ns } => Some(*duration_ns),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(durations, [20_000_000, 80_000_000]);
        assert!(!model.is_active());
    }

    #[test]
    fn durable_answer_replaces_partial_text_and_fences_late_stream_updates() {
        let mut model = TranscriptModel::default();
        let stream = |seq, kind, text| {
            agent_record(seq, kind, json!({"model_call_index": 1, "item_id": "answer", "phase": "final_answer", "text": text})).with_managed_turn_id(Some("turn"))
        };
        model.apply(&stream(1, AgentEventKind::AssistantDelta, "partial"));
        model.apply(&durable_answer(2, "turn", "complete answer"));
        model.apply(&durable_answer(2, "turn", "complete answer"));
        model.apply(&stream(
            3,
            AgentEventKind::AssistantMessage,
            "complete answer",
        ));
        model.apply(&stream(4, AgentEventKind::AssistantDelta, " stale"));
        assert_eq!(model.entries().len(), 1);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Assistant { text, complete: true } if text == "complete answer")
        );
    }

    #[test]
    fn durable_answer_deduplicates_complete_root_output_without_taking_child_output() {
        let mut model = TranscriptModel::default();
        let message = |seq, turn, text| {
            agent_record(seq, AgentEventKind::AssistantMessage, json!({"model_call_index": 1, "item_id": "answer", "phase": "final_answer", "text": text})).with_managed_turn_id(Some(turn))
        };
        model.apply(&message(1, "first", "first answer"));
        model.apply(&durable_answer(2, "first", "first answer"));
        assert_eq!(model.entries().len(), 1);
        model.apply(&message(3, "second", "child answer").with_managed_agent_id(Some(1)));
        model.apply(&durable_answer(4, "second", "child answer"));
        assert_eq!(
            model.entries().len(),
            3,
            "a child answer is a separate message even when its text matches"
        );
        model.apply(&message(5, "second", "child keeps working").with_managed_agent_id(Some(1)));
        assert!(
            matches!(&model.entries()[1].kind, EntryKind::Assistant { text, .. } if text == "child keeps working")
        );
        assert!(
            matches!(&model.entries()[2].kind, EntryKind::Assistant { text, .. } if text == "child answer")
        );
    }

    #[test]
    fn durable_answer_keeps_prior_complete_messages_and_does_not_render_empty_output() {
        let mut model = TranscriptModel::default();
        model.apply(&agent_record(1, AgentEventKind::AssistantMessage, json!({"model_call_index": 1, "item_id": "earlier", "phase": "final_answer", "text": "earlier answer"})).with_managed_turn_id(Some("turn")));
        model.apply(&durable_answer(2, "turn", "authoritative final answer"));
        model.apply(&durable_answer(3, "empty", ""));
        assert_eq!(model.entries().len(), 2);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Assistant { text, .. } if text == "earlier answer")
        );
        assert!(
            matches!(&model.entries()[1].kind, EntryKind::Assistant { text, .. } if text == "authoritative final answer")
        );
    }

    #[test]
    fn assistant_final_reconciles_anonymous_stream_and_ignores_late_deltas() {
        let mut model = TranscriptModel::default();
        let payload = |item, text| json!({"model_call_index": 1, "item_id": item, "phase": "final_answer", "text": text});
        model.apply(&agent_record(
            1,
            AgentEventKind::AssistantDelta,
            payload(None::<&str>, "partial"),
        ));
        let final_record = agent_record(
            2,
            AgentEventKind::AssistantMessage,
            payload(Some("answer"), "complete answer"),
        );
        model.apply(&final_record);
        model.apply(&final_record);
        model.apply(&agent_record(
            3,
            AgentEventKind::AssistantDelta,
            payload(None, " stale delta"),
        ));
        assert_eq!(model.entries().len(), 1);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Assistant { text, complete: true } if text == "complete answer")
        );
    }

    #[test]
    fn distinct_explicit_assistant_items_do_not_merge_with_another_open_stream() {
        let mut model = TranscriptModel::default();
        let payload = |item, text| json!({"model_call_index": 1, "item_id": item, "phase": "final_answer", "text": text});
        model.apply(&agent_record(
            1,
            AgentEventKind::AssistantDelta,
            payload("first", "first partial"),
        ));
        model.apply(&agent_record(
            2,
            AgentEventKind::AssistantMessage,
            payload("second", "second complete"),
        ));
        model.apply(&agent_record(
            3,
            AgentEventKind::AssistantMessage,
            payload("first", "first complete"),
        ));
        assert_eq!(model.entries().len(), 2);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Assistant { text, complete: true } if text == "first complete")
        );
        assert!(
            matches!(&model.entries()[1].kind, EntryKind::Assistant { text, complete: true } if text == "second complete")
        );
    }

    #[test]
    fn assistant_identity_includes_managed_turn_and_agent_request() {
        let mut model = TranscriptModel::default();
        for (index, (turn, request, text)) in [
            ("turn-a", "root", "first"),
            ("turn-b", "root", "second"),
            ("turn-b", "child", "third"),
        ]
        .into_iter()
        .enumerate()
        {
            let record = TranscriptRecord::from_agent(index as u64, 1, AgentEvent {
                protocol_version: 1, request_id: Arc::from(request), seq: index as u64,
                kind: AgentEventKind::AssistantMessage,
                payload: to_raw_value(&json!({"model_call_index": 1, "item_id": null, "phase": "final_answer", "text": text})).unwrap().into(),
            }).with_managed_turn_id(Some(turn));
            // The scope must also survive local transcript persistence.
            let record = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
            model.apply(&record);
        }
        let answers = model
            .entries()
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(answers, ["first", "second", "third"]);
    }

    #[test]
    fn display_error_does_not_complete_active_work_or_discard_partial_output() {
        let mut model = TranscriptModel::default();
        model.apply(&agent_record(1, AgentEventKind::RunStarted, json!({})));
        model.apply(&agent_record(2, AgentEventKind::AssistantDelta, json!({"model_call_index": 0, "item_id": "answer", "phase": "final_answer", "text": "partial"})));
        let record = TranscriptRecord::from_local(
            3,
            30,
            crate::tui::transcript::LocalEvent::DisplayError {
                message: "Could not display session update 7".to_owned(),
            },
        )
        .unwrap();
        assert!(model.apply(&record).changed);
        assert!(model.is_active());
        assert!(model.entries().iter().any(|entry| matches!(&entry.kind, EntryKind::Error { message } if message.contains("Could not display"))));
        assert!(model.entries().iter().any(|entry| matches!(&entry.kind, EntryKind::Assistant { text, complete: false, .. } if text == "partial")));
        model.apply(&agent_record(4, AgentEventKind::AssistantMessage, json!({"model_call_index": 0, "item_id": "answer", "phase": "final_answer", "text": "partial and complete"})));
        assert!(model.entries().iter().any(|entry| matches!(&entry.kind, EntryKind::Assistant { text, complete: true, .. } if text == "partial and complete")));
    }

    #[test]
    fn replayed_shell_session_is_pollable_without_active_rpc() {
        let records = [
            call(1, "shell", "exec_command", json!({"cmd": "sleep 1"})),
            result(
                2,
                "shell",
                "exec_command",
                Value::Null,
                json!({"session_id": 7, "output": "started"}),
                Value::Null,
            ),
        ];
        let mut model = TranscriptModel::default();
        for record in records {
            let replay = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
            model.apply(&replay);
        }
        assert_eq!(model.running_tool_ids().count(), 0);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Tool(tool) if tool.state == ToolState::Yielded)
        );
        model.apply(&call(3, "poll", "write_stdin", json!({"session_id": 7})));
        model.apply(&result(
            4,
            "poll",
            "write_stdin",
            Value::Null,
            json!({"exit_code": 0, "output": "done"}),
            Value::Null,
        ));
        assert_eq!(model.entries().len(), 1);
        assert_eq!(model.running_tool_ids().count(), 0);
        assert!(matches!(&model.entries()[0].kind, EntryKind::Tool(tool)
            if tool.state == ToolState::Succeeded && tool.result.as_ref().unwrap()["output"] == "starteddone"));
    }

    #[test]
    fn command_progress_survives_replayed_calls_and_missing_result_fields() {
        let mut model = TranscriptModel::default();
        let start = call(1, "build", "exec_command", json!({"cmd": "cargo test"}));
        let yielded = result(
            2,
            "build",
            "exec_command",
            Value::Null,
            json!({"session_id": 7, "exit_code": null, "output": "Compiling\n"}),
            Value::Null,
        );
        model.apply(&start);
        model.apply(&yielded);
        assert_eq!(model.running_tool_ids().count(), 0);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Tool(tool) if tool.state == ToolState::Yielded)
        );
        model.apply(&call(3, "poll", "write_stdin", json!({"session_id": 7})));
        let progress = result(
            4,
            "poll",
            "write_stdin",
            Value::Null,
            json!({"session_id": 7, "output": "Testing\n"}),
            Value::Null,
        );
        model.apply(&progress);
        model.apply(&start);
        model.apply(&yielded);
        model.apply(&progress);
        assert_eq!(model.running_tool_ids().count(), 0);
        assert!(
            matches!(&model.entries()[0].kind, EntryKind::Tool(tool) if tool.state == ToolState::Yielded)
        );
        model.apply(&call(5, "exit", "write_stdin", json!({"session_id": 7})));
        model.apply(&agent_record(
            6,
            AgentEventKind::ToolResult,
            json!({
                "call_id": "exit", "tool": "write_stdin", "status": "failed",
                "result": {"error": "process session unavailable"},
            }),
        ));
        assert_eq!(model.entries().len(), 1);
        let EntryKind::Tool(tool) = &model.entries()[0].kind else {
            panic!("expected command")
        };
        assert_eq!(tool.state, ToolState::Failed);
        let output = tool.result.as_ref().unwrap();
        assert_eq!(output["output"], "Compiling\nTesting\n");
        assert_eq!(output["error"], "process session unavailable");
        assert_eq!(tool.duration_ns, Some(50_000_000));
    }

    #[test]
    fn long_command_output_retains_the_latest_diagnostics() {
        let merged = super::merge_shell_result(
            Some(json!({"output": "α".repeat(40_000)})),
            json!({"exit_code": 101, "output": "\nlatest failure"}),
        );
        let output = merged["output"].as_str().unwrap();
        assert!(output.starts_with("…\n"));
        assert!(output.ends_with("latest failure"));
        assert!(output.len() <= 64 * 1024 + 4);
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
        assert_eq!(model.entries()[1].parent, Some(model.entries()[0].id));
        assert_eq!(model.entries()[2].parent, Some(model.entries()[0].id));
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
    fn nested_nonterminal_polls_leave_owning_shell_settled_and_pollable() {
        let mut model = TranscriptModel::default();
        model.apply(&call(
            1,
            "shell",
            "exec_command",
            json!({"cmd": "interactive"}),
        ));
        model.apply(&result(
            2,
            "shell",
            "exec_command",
            Value::Null,
            json!({"session_id": 7, "output": "ready"}),
            Value::Null,
        ));
        let shell_id = model.entries()[0].id;
        model.apply(&call(3, "outer", "exec", json!("poll")));
        for index in 0..2 {
            let call_id = format!("outer/code-{index}");
            model.apply(&call(
                4 + index * 2,
                &call_id,
                "write_stdin",
                json!({"session_id": 7}),
            ));
            model.apply(&result(
                5 + index * 2,
                &call_id,
                "write_stdin",
                Value::Null,
                json!({"session_id": 7, "output": "tick"}),
                Value::Null,
            ));
            assert!(!model.running_tool_ids().any(|id| id == shell_id));
            assert!(matches!(&model.entries()[0].kind, EntryKind::Tool(tool)
                if tool.state == ToolState::Yielded));
        }
        model.apply(&call(8, "done", "write_stdin", json!({"session_id": 7})));
        model.apply(&result(
            9,
            "done",
            "write_stdin",
            Value::Null,
            json!({"exit_code": 0, "output": "done"}),
            Value::Null,
        ));
        assert!(matches!(&model.entries()[0].kind, EntryKind::Tool(tool)
            if tool.state == ToolState::Succeeded && tool.result.as_ref().unwrap()["output"] == "readyticktickdone"));
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
    fn batch_display_deduplicates_children_and_preserves_raw_and_unique_output() {
        let first = json!({"status": "ready"});
        let second = json!({"image_url": "data:image/png;base64,abc"});
        let raw = json!([
            {"type": "input_text", "text": first.to_string()},
            {"type": "input_image", "image_url": "data:image/png;base64,abc"},
            {"type": "input_text", "text": "Discovered another tool"},
            {"status": "ready", "extra": "unique summary"}
        ]);
        let mut model = TranscriptModel::default();
        model.apply(&call(1, "batch", "exec", json!("source")));
        model.apply(&call(2, "batch/code-0", "accountInfo", json!({})));
        model.apply(&call(3, "batch/code-1", "other", json!({})));
        model.apply(&result(
            4,
            "batch/code-0",
            "accountInfo",
            first.clone(),
            first,
            Value::Null,
        ));
        model.apply(&result(
            5,
            "batch",
            "exec",
            raw.clone(),
            Value::Null,
            Value::Null,
        ));
        let revision = model.entries()[0].revision;
        model.apply(&result(
            6,
            "batch/code-1",
            "other",
            second.clone(),
            second,
            Value::Null,
        ));
        let parent = &model.entries()[0];
        assert!(parent.revision > revision);
        assert!(!parent.hidden);
        let EntryKind::Tool(tool) = &parent.kind else {
            panic!("expected batch");
        };
        assert_eq!(tool.result.as_ref(), Some(&raw));
        assert_eq!(
            tool.code_display_result,
            Some(json!([raw[2].clone(), raw[3].clone()]))
        );
    }

    #[test]
    fn batch_display_preserves_additional_identical_emits() {
        let child = json!({"ok": true});
        let output = json!([child.clone(), child.to_string(), child]);
        assert_eq!(
            distinct_code_output(&output, &[&child]),
            json!([child.to_string(), child.clone()])
        );
        assert_eq!(
            distinct_code_output(&output, &[&child, &child]),
            json!([child.clone()])
        );
    }

    #[test]
    fn batch_display_keeps_nonmatching_and_standalone_outputs() {
        let child = json!({"ok": true});
        let output = json!([child, "discovery", {"ok": true, "extra": 1}]);
        assert_eq!(distinct_code_output(&output, &[]), output);
        assert_eq!(
            distinct_code_output(&output, &[&child]),
            json!(["discovery", {"ok": true, "extra": 1}])
        );
        assert_eq!(
            distinct_code_output(&json!(child.to_string()), &[&child]),
            Value::Null
        );
        assert_eq!(
            distinct_code_output(&json!("prefix {\"ok\":true}"), &[&child]),
            json!("prefix {\"ok\":true}")
        );
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
