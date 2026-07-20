use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use tokio::sync::mpsc;

const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error("failed to encode agent event")]
    Encode(#[source] serde_json::Error),

    #[error("failed to write agent event")]
    Write(#[source] std::io::Error),

    #[error("agent event stream closed before the turn emitted a terminal event")]
    ClosedBeforeTerminal,
}

/// One ordered event emitted by an agent run.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentEvent {
    pub protocol_version: u32,
    pub request_id: Arc<str>,
    pub seq: u64,
    #[serde(rename = "type")]
    pub kind: AgentEventKind,
    pub payload: Box<RawValue>,
}

/// Stable event categories emitted by the agent runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentEventKind {
    #[serde(rename = "api.event")]
    ApiEvent,
    #[serde(rename = "assistant.delta")]
    AssistantDelta,
    #[serde(rename = "assistant.message")]
    AssistantMessage,
    #[serde(rename = "reasoning.summary.delta")]
    ReasoningSummaryDelta,
    #[serde(rename = "run.started")]
    RunStarted,
    #[serde(rename = "run.steered")]
    RunSteered,
    #[serde(rename = "run.error")]
    RunError,
    #[serde(rename = "run.completed")]
    RunCompleted,
    #[serde(rename = "run.failed")]
    RunFailed,
    #[serde(rename = "tool.call")]
    ToolCall,
    #[serde(rename = "tool.result")]
    ToolResult,
    #[serde(rename = "model.warmup.started")]
    ModelWarmupStarted,
    #[serde(rename = "model.warmup.completed")]
    ModelWarmupCompleted,
    #[serde(rename = "model.warmup.failed")]
    ModelWarmupFailed,
    #[serde(rename = "model.call.started")]
    ModelCallStarted,
    #[serde(rename = "model.call.completed")]
    ModelCallCompleted,
    #[serde(rename = "model.call.failed")]
    ModelCallFailed,
    #[serde(rename = "model.compaction.started")]
    ModelCompactionStarted,
    #[serde(rename = "model.compaction.completed")]
    ModelCompactionCompleted,
    #[serde(rename = "model.compaction.failed")]
    ModelCompactionFailed,
    #[serde(rename = "model.attempt.started")]
    ModelAttemptStarted,
    #[serde(rename = "model.attempt.failed")]
    ModelAttemptFailed,
    #[serde(rename = "model.attempt.retrying")]
    ModelAttemptRetrying,
    #[serde(rename = "model.connection.started")]
    ModelConnectionStarted,
    #[serde(rename = "model.connection.completed")]
    ModelConnectionCompleted,
    #[serde(rename = "model.connection.failed")]
    ModelConnectionFailed,
}

/// The receiving half of an agent's typed event stream.
pub struct AgentEvents {
    receiver: mpsc::UnboundedReceiver<AgentEvent>,
}

impl AgentEvents {
    /// Receives the next event, or `None` after all emitters are dropped.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.receiver.recv().await
    }

    /// Writes every event as one flushed JSONL record.
    ///
    /// # Errors
    ///
    /// Returns an error when an event cannot be encoded or written.
    pub async fn write_jsonl(mut self, mut output: impl Write) -> Result<(), EventError> {
        while let Some(event) = self.recv().await {
            write_jsonl_event(&mut output, &event)?;
        }
        Ok(())
    }

    /// Writes one turn through its terminal event and leaves the session stream
    /// available for follow-on turns.
    ///
    /// # Errors
    ///
    /// Returns an error when an event cannot be written or the agent stops
    /// before emitting `run.completed` or `run.failed`.
    pub async fn write_turn_jsonl(&mut self, mut output: impl Write) -> Result<(), EventError> {
        while let Some(event) = self.recv().await {
            let terminal = event.kind.is_terminal();
            write_jsonl_event(&mut output, &event)?;
            if terminal {
                return Ok(());
            }
        }
        Err(EventError::ClosedBeforeTerminal)
    }
}

impl AgentEventKind {
    /// Returns whether this event completes a turn.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::RunCompleted | Self::RunFailed)
    }
}

impl AgentEvent {
    /// Decodes the event payload into a caller-selected typed shape.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained payload does not match `T`.
    pub fn decode_payload<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.payload.get())
    }
}

fn write_jsonl_event(output: &mut impl Write, event: &AgentEvent) -> Result<(), EventError> {
    serde_json::to_writer(&mut *output, event).map_err(EventError::Encode)?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(EventError::Write)
}

/// Internal emission handle shared by orchestration and transport crates.
#[doc(hidden)]
#[derive(Clone)]
pub struct EventSink {
    request_id: Arc<str>,
    next_seq: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<AgentEvent>,
}

impl EventSink {
    #[must_use]
    pub fn channel(request_id: String) -> (Self, AgentEvents) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                request_id: request_id.into(),
                next_seq: Arc::new(AtomicU64::new(1)),
                sender,
            },
            AgentEvents { receiver },
        )
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Emits an event when a receiver is present and otherwise discards it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be converted to JSON.
    pub fn emit<P: Serialize>(&self, kind: AgentEventKind, payload: P) -> Result<(), EventError> {
        let payload = to_raw_value(&payload).map_err(EventError::Encode)?;
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        drop(self.sender.send(AgentEvent {
            protocol_version: PROTOCOL_VERSION,
            request_id: Arc::clone(&self.request_id),
            seq,
            kind,
            payload,
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentEventKind, EventSink};

    #[tokio::test]
    async fn events_are_ordered_and_receiver_drop_is_not_an_error() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        events
            .emit(AgentEventKind::RunStarted, json!({ "n": 1 }))
            .unwrap();
        events
            .emit(AgentEventKind::RunCompleted, json!({ "n": 2 }))
            .unwrap();
        let first = receiver.recv().await.unwrap();
        let second = receiver.recv().await.unwrap();
        assert_eq!((first.seq, first.kind), (1, AgentEventKind::RunStarted));
        assert_eq!((second.seq, second.kind), (2, AgentEventKind::RunCompleted));
        assert_eq!(
            second.decode_payload::<serde_json::Value>().unwrap()["n"],
            2
        );
        drop(receiver);
        events.emit(AgentEventKind::RunFailed, json!({})).unwrap();
    }
}
