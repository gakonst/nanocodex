// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use crate::config::{ReasoningEffort, ReasoningMode};
use nanocodex::agent::events::{AgentEvent, AgentEventKind};
use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use std::{path::PathBuf, sync::Arc};

pub(crate) const SCHEMA_VERSION: u32 = 2;
pub(super) const AGENT_SOURCE: &str = "agent";
pub(super) const TACT_SOURCE: &str = "tact";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub(crate) struct TurnId(u64);

impl TurnId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct ShellId(u64);

impl ShellId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SessionStarted {
    pub(crate) session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_sequence: Option<u64>,
    pub(crate) model: String,
    pub(crate) effort: ReasoningEffort,
    pub(crate) reasoning_mode: ReasoningMode,
    pub(crate) fast_mode: bool,
    pub(crate) workspace: PathBuf,
    pub(crate) application_version: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionOutcome {
    Closed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SessionEnded {
    pub(crate) outcome: SessionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LocalEvent {
    SessionStarted(SessionStarted),
    UserSubmitted {
        id: TurnId,
        text: String,
    },
    UserSteered {
        text: String,
    },
    ReflectionStarted {
        id: TurnId,
    },
    ShellStarted {
        id: ShellId,
        command: String,
        workspace: PathBuf,
    },
    ShellFinished {
        id: ShellId,
        output: String,
        exit_code: Option<i32>,
        duration_ns: u64,
        truncated: bool,
        error: Option<String>,
    },
    EffortChanged {
        from: ReasoningEffort,
        to: ReasoningEffort,
    },
    FastModeChanged {
        from: bool,
        to: bool,
    },
    ContextObserved {
        prompt_cache: bool,
        previous_response: bool,
    },
    WorkerTurnAccepted {
        id: TurnId,
    },
    WorkerTurnFinished {
        id: TurnId,
        error: Option<String>,
    },
    WorkerTurnsInterrupted {
        count: usize,
        error: Option<String>,
    },
    WorkerSteerFailed {
        error: String,
    },
    WorkerStopped {
        error: Option<String>,
    },
    SessionEnded(SessionEnded),
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct TranscriptRecord {
    schema_version: u32,
    sequence: u64,
    recorded_at_unix_ms: u64,
    source: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentMetadata>,
    payload: Arc<RawValue>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AgentMetadata {
    protocol_version: u32,
    request_id: Arc<str>,
    sequence: u64,
}

impl TranscriptRecord {
    pub(crate) fn from_agent(sequence: u64, recorded_at_unix_ms: u64, event: AgentEvent) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            sequence,
            recorded_at_unix_ms,
            source: AGENT_SOURCE.to_owned(),
            kind: agent_kind(event.kind).to_owned(),
            agent: Some(AgentMetadata {
                protocol_version: event.protocol_version,
                request_id: event.request_id,
                sequence: event.seq,
            }),
            payload: event.payload,
        }
    }

    pub(crate) fn from_local(
        sequence: u64,
        recorded_at_unix_ms: u64,
        event: LocalEvent,
    ) -> Result<Self, serde_json::Error> {
        let (kind, payload) = match event {
            LocalEvent::SessionStarted(payload) => ("session.started", to_raw_value(&payload)?),
            LocalEvent::UserSubmitted { id, text } => {
                ("user.submitted", to_raw_value(&UserSubmitted { id, text })?)
            }
            LocalEvent::UserSteered { text } => {
                ("user.steered", to_raw_value(&UserSteered { text })?)
            }
            LocalEvent::ReflectionStarted { id } => (
                "reflection.started",
                to_raw_value(&ReflectionStarted { id })?,
            ),
            LocalEvent::ShellStarted {
                id,
                command,
                workspace,
            } => (
                "shell.started",
                to_raw_value(&ShellStarted {
                    id,
                    command,
                    workspace,
                })?,
            ),
            LocalEvent::ShellFinished {
                id,
                output,
                exit_code,
                duration_ns,
                truncated,
                error,
            } => (
                "shell.finished",
                to_raw_value(&ShellFinished {
                    id,
                    output,
                    exit_code,
                    duration_ns,
                    truncated,
                    error,
                })?,
            ),
            LocalEvent::EffortChanged { from, to } => {
                ("effort.changed", to_raw_value(&EffortChanged { from, to })?)
            }
            LocalEvent::FastModeChanged { from, to } => (
                "fast_mode.changed",
                to_raw_value(&FastModeChanged { from, to })?,
            ),
            LocalEvent::ContextObserved {
                prompt_cache,
                previous_response,
            } => (
                "context.observed",
                to_raw_value(&ContextObserved {
                    prompt_cache,
                    previous_response,
                })?,
            ),
            LocalEvent::WorkerTurnAccepted { id } => {
                ("worker.turn_accepted", to_raw_value(&WorkerTurn { id })?)
            }
            LocalEvent::WorkerTurnFinished { id, error } => (
                "worker.turn_finished",
                to_raw_value(&WorkerTurnFinished { id, error })?,
            ),
            LocalEvent::WorkerTurnsInterrupted { count, error } => (
                "worker.turns_interrupted",
                to_raw_value(&WorkerTurnsInterrupted { count, error })?,
            ),
            LocalEvent::WorkerSteerFailed { error } => (
                "worker.steer_failed",
                to_raw_value(&WorkerSteerFailed { error })?,
            ),
            LocalEvent::WorkerStopped { error } => {
                ("worker.stopped", to_raw_value(&WorkerStopped { error })?)
            }
            LocalEvent::SessionEnded(payload) => ("session.ended", to_raw_value(&payload)?),
        };
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            sequence,
            recorded_at_unix_ms,
            source: TACT_SOURCE.to_owned(),
            kind: kind.to_owned(),
            agent: None,
            payload: payload.into(),
        })
    }

    pub(crate) const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub(crate) const fn recorded_at_unix_ms(&self) -> u64 {
        self.recorded_at_unix_ms
    }

    pub(crate) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) fn kind(&self) -> &str {
        &self.kind
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn agent_request_id(&self) -> Option<Arc<str>> {
        self.agent
            .as_ref()
            .map(|metadata| Arc::clone(&metadata.request_id))
    }

    pub(crate) fn payload_json(&self) -> &str {
        self.payload.get()
    }

    pub(crate) fn decode_payload<'a, T>(&'a self) -> Result<T, serde_json::Error>
    where
        T: serde::Deserialize<'a>,
    {
        serde_json::from_str(self.payload.get())
    }
}

#[derive(Serialize)]
struct UserSubmitted {
    id: TurnId,
    text: String,
}

#[derive(Serialize)]
struct UserSteered {
    text: String,
}

#[derive(Serialize)]
struct ReflectionStarted {
    id: TurnId,
}

#[derive(Serialize)]
struct ShellStarted {
    id: ShellId,
    command: String,
    workspace: PathBuf,
}

#[derive(Serialize)]
struct ShellFinished {
    id: ShellId,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    duration_ns: u64,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct EffortChanged {
    from: ReasoningEffort,
    to: ReasoningEffort,
}

#[derive(Serialize)]
struct FastModeChanged {
    from: bool,
    to: bool,
}

#[derive(Serialize)]
struct ContextObserved {
    prompt_cache: bool,
    previous_response: bool,
}

#[derive(Serialize)]
struct WorkerTurn {
    id: TurnId,
}

#[derive(Serialize)]
struct WorkerTurnFinished {
    id: TurnId,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct WorkerTurnsInterrupted {
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct WorkerSteerFailed {
    error: String,
}

#[derive(Serialize)]
struct WorkerStopped {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

const fn agent_kind(kind: AgentEventKind) -> &'static str {
    match kind {
        AgentEventKind::ApiEvent => "api.event",
        AgentEventKind::AssistantDelta => "assistant.delta",
        AgentEventKind::AssistantMessage => "assistant.message",
        AgentEventKind::ReasoningSummaryDelta => "reasoning.summary.delta",
        AgentEventKind::RunStarted => "run.started",
        AgentEventKind::RunSteered => "run.steered",
        AgentEventKind::RunError => "run.error",
        AgentEventKind::RunCompleted => "run.completed",
        AgentEventKind::RunFailed => "run.failed",
        AgentEventKind::ToolCall => "tool.call",
        AgentEventKind::ToolResult => "tool.result",
        AgentEventKind::ModelWarmupStarted => "model.warmup.started",
        AgentEventKind::ModelWarmupCompleted => "model.warmup.completed",
        AgentEventKind::ModelWarmupFailed => "model.warmup.failed",
        AgentEventKind::ModelCallStarted => "model.call.started",
        AgentEventKind::ModelCallCompleted => "model.call.completed",
        AgentEventKind::ModelCallFailed => "model.call.failed",
        AgentEventKind::ModelCompactionStarted => "model.compaction.started",
        AgentEventKind::ModelCompactionCompleted => "model.compaction.completed",
        AgentEventKind::ModelCompactionFailed => "model.compaction.failed",
        AgentEventKind::ModelAttemptStarted => "model.attempt.started",
        AgentEventKind::ModelAttemptFailed => "model.attempt.failed",
        AgentEventKind::ModelAttemptRetrying => "model.attempt.retrying",
        AgentEventKind::ModelConnectionStarted => "model.connection.started",
        AgentEventKind::ModelConnectionCompleted => "model.connection.completed",
        AgentEventKind::ModelConnectionFailed => "model.connection.failed",
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalEvent, ShellId, TranscriptRecord, TurnId};
    use nanocodex::agent::events::{AgentEvent, AgentEventKind};
    use serde_json::{json, value::to_raw_value};
    use std::sync::Arc;

    #[test]
    fn agent_record_retains_protocol_metadata_and_raw_payload() {
        let payload = json!({"text": "hello"});
        let record = TranscriptRecord::from_agent(
            7,
            123,
            AgentEvent {
                protocol_version: 1,
                request_id: Arc::from("session-a"),
                seq: 4,
                kind: AgentEventKind::AssistantDelta,
                payload: to_raw_value(&payload).unwrap().into(),
            },
        );
        let encoded = serde_json::to_value(record).unwrap();

        assert_eq!(encoded["schema_version"], 2);
        assert_eq!(encoded["sequence"], 7);
        assert_eq!(encoded["recorded_at_unix_ms"], 123);
        assert_eq!(encoded["source"], "agent");
        assert_eq!(encoded["type"], "assistant.delta");
        assert_eq!(encoded["agent"]["request_id"], "session-a");
        assert_eq!(encoded["agent"]["sequence"], 4);
        assert_eq!(encoded["payload"], payload);
    }

    #[test]
    fn local_record_uses_typed_payload() {
        let record = TranscriptRecord::from_local(
            1,
            123,
            LocalEvent::UserSubmitted {
                id: TurnId::new(9),
                text: "hello".to_owned(),
            },
        )
        .unwrap();
        let encoded = serde_json::to_value(record).unwrap();

        assert_eq!(encoded["source"], "tact");
        assert_eq!(encoded["type"], "user.submitted");
        assert_eq!(encoded["payload"], json!({"id": 9, "text": "hello"}));
    }

    #[test]
    fn shell_lifecycle_uses_structured_local_records() {
        let started = TranscriptRecord::from_local(
            1,
            123,
            LocalEvent::ShellStarted {
                id: ShellId::new(4),
                command: "pwd".to_owned(),
                workspace: "/work".into(),
            },
        )
        .unwrap();
        let finished = TranscriptRecord::from_local(
            2,
            124,
            LocalEvent::ShellFinished {
                id: ShellId::new(4),
                output: "/work\n".to_owned(),
                exit_code: Some(0),
                duration_ns: 10,
                truncated: false,
                error: None,
            },
        )
        .unwrap();

        assert_eq!(started.kind(), "shell.started");
        assert_eq!(finished.kind(), "shell.finished");
        assert_eq!(
            serde_json::to_value(finished).unwrap()["payload"],
            json!({
                "id": 4,
                "output": "/work\n",
                "exit_code": 0,
                "duration_ns": 10,
                "truncated": false,
            })
        );
    }

    #[test]
    fn applied_steer_has_a_distinct_local_record() {
        let record = TranscriptRecord::from_local(
            1,
            123,
            LocalEvent::UserSteered {
                text: "change direction".to_owned(),
            },
        )
        .unwrap();
        let encoded = serde_json::to_value(record).unwrap();

        assert_eq!(encoded["type"], "user.steered");
        assert_eq!(encoded["payload"]["text"], "change direction");
    }

    #[test]
    fn reflection_start_contains_only_its_turn_id() {
        let record = TranscriptRecord::from_local(
            1,
            123,
            LocalEvent::ReflectionStarted { id: TurnId::new(9) },
        )
        .unwrap();
        let encoded = serde_json::to_value(record).unwrap();

        assert_eq!(encoded["type"], "reflection.started");
        assert_eq!(encoded["payload"], json!({"id": 9}));
    }
}
