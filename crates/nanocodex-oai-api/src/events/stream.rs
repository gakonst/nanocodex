use std::{
    io::Write,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
#[cfg(feature = "client")]
use serde_json::value::to_raw_value;
use tokio::sync::mpsc;
use web_time::Instant;

/// Current version of the canonical agent-event protocol.
pub const AGENT_EVENT_PROTOCOL_VERSION: u32 = 1;
static PROCESS_MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Returns a process-relative monotonic timestamp for private cross-layer timing.
#[doc(hidden)]
#[must_use]
pub fn monotonic_now_ns() -> u64 {
    let elapsed = PROCESS_MONOTONIC_EPOCH
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos();
    u64::try_from(elapsed).unwrap_or(u64::MAX)
}

/// Failure while encoding, writing, or consuming the contractual event stream.
#[derive(Debug, thiserror::Error)]
pub enum EventError {
    /// A typed event could not be encoded as JSON.
    #[error("failed to encode agent event")]
    Encode(#[source] serde_json::Error),

    /// An encoded event could not be written to the supplied output.
    #[error("failed to write agent event")]
    Write(#[source] std::io::Error),

    /// The stream closed before the accepted turn emitted a terminal event.
    #[error("agent event stream closed before the turn emitted a terminal event")]
    ClosedBeforeTerminal,

    /// An externally formed event uses an unsupported protocol version.
    #[error("agent event protocol version mismatch: expected {expected}, received {actual}")]
    ProtocolVersionMismatch {
        /// Protocol version accepted by this channel.
        expected: u32,
        /// Protocol version carried by the rejected event.
        actual: u32,
    },

    /// An externally formed event belongs to another request/session.
    #[error("agent event request identity mismatch: expected {expected}, received {actual}")]
    RequestIdMismatch {
        /// Request identity owned by this channel.
        expected: Arc<str>,
        /// Request identity carried by the rejected event.
        actual: Arc<str>,
    },

    /// An externally formed event did not advance the session sequence.
    #[error("agent event sequence did not advance: minimum {expected}, received {actual}")]
    SequenceMismatch {
        /// Next sequence number required by this channel.
        expected: u64,
        /// Sequence number carried by the rejected event.
        actual: u64,
    },

    /// The event sequence cannot be advanced beyond `u64::MAX`.
    #[error("agent event sequence is exhausted")]
    SequenceExhausted,

    /// A turn publisher received another event after its terminal event.
    #[error("agent turn event was published after its terminal event")]
    TurnAlreadyTerminal,
}

/// One ordered event emitted by an agent run.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentEvent {
    /// Version of the stable event protocol.
    pub protocol_version: u32,
    /// Stable session/request identity shared by this event stream.
    pub request_id: Arc<str>,
    /// Monotonic sequence number within the stream.
    pub seq: u64,
    /// Stable event category.
    #[serde(rename = "type")]
    pub kind: AgentEventKind,
    /// Complete typed-event payload encoded as retained raw JSON.
    pub payload: Arc<RawValue>,
}

/// Private in-process timing carried beside an event without changing JSONL.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct AgentEventTiming {
    /// Process-relative nanoseconds when the event was emitted.
    pub emitted_ns: u64,
    /// Process-relative nanoseconds when the transport observed the source event.
    pub source_received_ns: Option<u64>,
}

/// An agent event plus private in-process delivery timing.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TimedAgentEvent {
    /// Contractual event visible to consumers.
    pub event: AgentEvent,
    /// Private in-process timing carried beside the contractual event.
    pub timing: AgentEventTiming,
}

/// Stable event categories emitted by the agent runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentEventKind {
    /// Complete provider event in original order.
    #[serde(rename = "api.event")]
    ApiEvent,
    /// Incremental assistant text.
    #[serde(rename = "assistant.delta")]
    AssistantDelta,
    /// Completed assistant message.
    #[serde(rename = "assistant.message")]
    AssistantMessage,
    /// Incremental reasoning summary.
    #[serde(rename = "reasoning.summary.delta")]
    ReasoningSummaryDelta,
    /// Accepted turn started.
    #[serde(rename = "run.started")]
    RunStarted,
    /// Input was added to an active turn.
    #[serde(rename = "run.steered")]
    RunSteered,
    /// Recoverable run-level error was observed.
    #[serde(rename = "run.error")]
    RunError,
    /// Turn completed successfully.
    #[serde(rename = "run.completed")]
    RunCompleted,
    /// Turn terminated with an error.
    #[serde(rename = "run.failed")]
    RunFailed,
    /// Tool invocation started.
    #[serde(rename = "tool.call")]
    ToolCall,
    /// Tool invocation completed.
    #[serde(rename = "tool.result")]
    ToolResult,
    /// Optional model connection warmup started.
    #[serde(rename = "model.warmup.started")]
    ModelWarmupStarted,
    /// Optional model connection warmup completed.
    #[serde(rename = "model.warmup.completed")]
    ModelWarmupCompleted,
    /// Optional model connection warmup failed.
    #[serde(rename = "model.warmup.failed")]
    ModelWarmupFailed,
    /// Logical model call started.
    #[serde(rename = "model.call.started")]
    ModelCallStarted,
    /// Logical model call completed.
    #[serde(rename = "model.call.completed")]
    ModelCallCompleted,
    /// Logical model call failed.
    #[serde(rename = "model.call.failed")]
    ModelCallFailed,
    /// Model-side context compaction started.
    #[serde(rename = "model.compaction.started")]
    ModelCompactionStarted,
    /// Model-side context compaction completed.
    #[serde(rename = "model.compaction.completed")]
    ModelCompactionCompleted,
    /// Model-side context compaction failed.
    #[serde(rename = "model.compaction.failed")]
    ModelCompactionFailed,
    /// One transport attempt started.
    #[serde(rename = "model.attempt.started")]
    ModelAttemptStarted,
    /// One transport attempt failed.
    #[serde(rename = "model.attempt.failed")]
    ModelAttemptFailed,
    /// The SDK scheduled another transport attempt.
    #[serde(rename = "model.attempt.retrying")]
    ModelAttemptRetrying,
    /// A model transport connection started.
    #[serde(rename = "model.connection.started")]
    ModelConnectionStarted,
    /// A model transport connection completed.
    #[serde(rename = "model.connection.completed")]
    ModelConnectionCompleted,
    /// A model transport connection failed.
    #[serde(rename = "model.connection.failed")]
    ModelConnectionFailed,
}

/// The receiving half of an agent's typed event stream.
pub struct AgentEvents {
    request_id: Arc<str>,
    receiver: mpsc::UnboundedReceiver<TimedAgentEvent>,
}

impl AgentEvents {
    /// Stable session/request identifier shared by every event in this stream.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Receives the next event, or `None` after all emitters are dropped.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.recv_timed().await.map(|event| event.event)
    }

    /// Receives one event with private process-relative timing metadata.
    #[doc(hidden)]
    pub async fn recv_timed(&mut self) -> Option<TimedAgentEvent> {
        self.receiver.recv().await
    }

    /// Receives one immediately available event without waiting.
    #[doc(hidden)]
    pub fn try_recv_timed(&mut self) -> Option<TimedAgentEvent> {
        self.receiver.try_recv().ok()
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

impl Stream for AgentEvents {
    type Item = AgentEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.receiver
            .poll_recv(context)
            .map(|event| event.map(|event| event.event))
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
    /// Returns a stable typed projection of this event.
    ///
    /// Application-facing run, assistant, reasoning, tool, model, and context
    /// events decode into named types. Lower-level diagnostics remain lossless.
    /// With the `client` feature, raw `OpenAI` frames decode into the
    /// provider-specific projection; events-only builds retain them as
    /// transport diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error when a payload does not satisfy the contract declared
    /// by its event kind.
    pub fn data(&self) -> Result<super::AgentEventData, serde_json::Error> {
        use super::{
            AgentEventData, AssistantEvent, ContextEvent, ModelEvent, ReasoningEvent, RunEvent,
            ToolEvent, TransportEvent,
        };

        Ok(match self.kind {
            AgentEventKind::ApiEvent => {
                #[cfg(feature = "client")]
                {
                    AgentEventData::OpenAi(self.decode_payload()?)
                }
                #[cfg(not(feature = "client"))]
                {
                    AgentEventData::Transport(TransportEvent::new(
                        self.kind,
                        Arc::clone(&self.payload),
                    ))
                }
            }
            AgentEventKind::AssistantDelta => {
                AgentEventData::Assistant(AssistantEvent::Delta(self.decode_payload()?))
            }
            AgentEventKind::AssistantMessage => {
                AgentEventData::Assistant(AssistantEvent::Message(self.decode_payload()?))
            }
            AgentEventKind::ReasoningSummaryDelta => {
                AgentEventData::Reasoning(ReasoningEvent::SummaryDelta(self.decode_payload()?))
            }
            AgentEventKind::RunStarted => {
                AgentEventData::Run(RunEvent::Started(self.decode_payload()?))
            }
            AgentEventKind::RunSteered => {
                AgentEventData::Run(RunEvent::Steered(self.decode_payload()?))
            }
            AgentEventKind::RunError => {
                AgentEventData::Run(RunEvent::Error(self.decode_payload()?))
            }
            AgentEventKind::RunCompleted => {
                AgentEventData::Run(RunEvent::Completed(Box::new(self.decode_payload()?)))
            }
            AgentEventKind::RunFailed => {
                AgentEventData::Run(RunEvent::Failed(Box::new(self.decode_payload()?)))
            }
            AgentEventKind::ToolCall => {
                AgentEventData::Tool(ToolEvent::Call(self.decode_payload()?))
            }
            AgentEventKind::ToolResult => {
                AgentEventData::Tool(ToolEvent::Result(self.decode_payload()?))
            }
            AgentEventKind::ModelWarmupStarted => {
                AgentEventData::Model(ModelEvent::WarmupStarted(self.decode_payload()?))
            }
            AgentEventKind::ModelWarmupCompleted => {
                AgentEventData::Model(ModelEvent::WarmupCompleted(self.decode_payload()?))
            }
            AgentEventKind::ModelWarmupFailed => {
                AgentEventData::Model(ModelEvent::WarmupFailed(self.decode_payload()?))
            }
            AgentEventKind::ModelCallStarted => {
                AgentEventData::Model(ModelEvent::CallStarted(self.decode_payload()?))
            }
            AgentEventKind::ModelCallCompleted => {
                AgentEventData::Model(ModelEvent::CallCompleted(self.decode_payload()?))
            }
            AgentEventKind::ModelCallFailed => {
                AgentEventData::Model(ModelEvent::CallFailed(self.decode_payload()?))
            }
            AgentEventKind::ModelCompactionStarted => {
                AgentEventData::Context(ContextEvent::CompactionStarted(self.decode_payload()?))
            }
            AgentEventKind::ModelCompactionCompleted => {
                AgentEventData::Context(ContextEvent::CompactionCompleted(self.decode_payload()?))
            }
            AgentEventKind::ModelCompactionFailed => {
                AgentEventData::Context(ContextEvent::CompactionFailed(self.decode_payload()?))
            }
            AgentEventKind::ModelAttemptStarted
            | AgentEventKind::ModelAttemptFailed
            | AgentEventKind::ModelAttemptRetrying
            | AgentEventKind::ModelConnectionStarted
            | AgentEventKind::ModelConnectionCompleted
            | AgentEventKind::ModelConnectionFailed => {
                AgentEventData::Transport(TransportEvent::new(self.kind, Arc::clone(&self.payload)))
            }
        })
    }

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

#[derive(Clone)]
struct EventChannel {
    request_id: Arc<str>,
    next_seq: Arc<AtomicU64>,
    session: mpsc::UnboundedSender<TimedAgentEvent>,
    turn: Option<mpsc::UnboundedSender<TimedAgentEvent>>,
    turn_admission: Option<Arc<AtomicU64>>,
}

const TURN_COMMITTED: u64 = 1 << 63;
const TURN_CLAIMING: u64 = 1 << 62;
const TURN_ADMISSIONS: u64 = TURN_CLAIMING - 1;
const TERMINAL_SPINS_BEFORE_YIELD: usize = 64;

struct TurnAdmission<'a> {
    state: &'a AtomicU64,
}

impl Drop for TurnAdmission<'_> {
    fn drop(&mut self) {
        self.state.fetch_sub(1, Ordering::Release);
    }
}

struct TerminalClaim<'a> {
    state: &'a AtomicU64,
    committed: bool,
}

impl TerminalClaim<'_> {
    fn commit(mut self) {
        self.state.fetch_or(TURN_COMMITTED, Ordering::Release);
        self.state.fetch_and(!TURN_CLAIMING, Ordering::Release);
        self.committed = true;
    }
}

impl Drop for TerminalClaim<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.state.fetch_and(!TURN_CLAIMING, Ordering::Release);
        }
    }
}

impl EventChannel {
    fn channel(request_id: Arc<str>) -> (Self, AgentEvents) {
        let (session, receiver) = mpsc::unbounded_channel();
        (
            Self {
                request_id: Arc::clone(&request_id),
                next_seq: Arc::new(AtomicU64::new(1)),
                session,
                turn: None,
                turn_admission: None,
            },
            AgentEvents {
                request_id,
                receiver,
            },
        )
    }

    fn mirrored_channel(&self) -> (Self, AgentEvents) {
        let (turn, receiver) = mpsc::unbounded_channel();
        (
            Self {
                request_id: Arc::clone(&self.request_id),
                next_seq: Arc::clone(&self.next_seq),
                session: self.session.clone(),
                turn: Some(turn),
                turn_admission: Some(Arc::new(AtomicU64::new(0))),
            },
            AgentEvents {
                request_id: Arc::clone(&self.request_id),
                receiver,
            },
        )
    }

    fn turn_is_terminal(&self) -> bool {
        self.turn_admission
            .as_ref()
            .is_some_and(|state| state.load(Ordering::Acquire) & TURN_COMMITTED != 0)
    }

    fn admit_non_terminal(&self) -> Result<Option<TurnAdmission<'_>>, EventError> {
        let Some(state) = self.turn_admission.as_deref() else {
            return Ok(None);
        };
        let mut current = state.load(Ordering::Acquire);
        loop {
            if current & TURN_COMMITTED != 0 {
                return Err(EventError::TurnAlreadyTerminal);
            }
            if current & TURN_CLAIMING != 0 {
                wait_for_terminal_claim(state, current);
                current = state.load(Ordering::Acquire);
                continue;
            }
            if current & TURN_ADMISSIONS == TURN_ADMISSIONS {
                return Err(EventError::SequenceExhausted);
            }
            match state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Some(TurnAdmission { state })),
                Err(observed) => current = observed,
            }
        }
    }

    /// Atomically closes admission, then waits for publications admitted before
    /// the terminal to finish routing. The low 62 bits count those admissions.
    fn claim_terminal(&self) -> Result<Option<TerminalClaim<'_>>, EventError> {
        let Some(state) = self.turn_admission.as_deref() else {
            return Ok(None);
        };
        let mut current = state.load(Ordering::Acquire);
        loop {
            if current & TURN_COMMITTED != 0 {
                return Err(EventError::TurnAlreadyTerminal);
            }
            if current & TURN_CLAIMING != 0 {
                wait_for_terminal_claim(state, current);
                current = state.load(Ordering::Acquire);
                continue;
            }
            match state.compare_exchange_weak(
                current,
                current | TURN_CLAIMING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        let mut spins = 0;
        while state.load(Ordering::Acquire) & TURN_ADMISSIONS != 0 {
            if spins < TERMINAL_SPINS_BEFORE_YIELD {
                spins += 1;
                std::hint::spin_loop();
            } else {
                spins = 0;
                std::thread::yield_now();
            }
        }
        Ok(Some(TerminalClaim {
            state,
            committed: false,
        }))
    }

    fn reserve_publisher_event(
        &self,
        event: &AgentEvent,
    ) -> Result<Option<TurnAdmission<'_>>, EventError> {
        let terminal_claim = if event.kind.is_terminal() {
            self.claim_terminal()?
        } else {
            None
        };
        let admission = if terminal_claim.is_none() && !event.kind.is_terminal() {
            self.admit_non_terminal()?
        } else {
            None
        };
        self.reserve_exact_sequence(event.seq)?;
        if let Some(claim) = terminal_claim {
            claim.commit();
        }
        Ok(admission)
    }

    fn reserve_exact_sequence(&self, seq: u64) -> Result<(), EventError> {
        let next_seq = seq.checked_add(1).ok_or(EventError::SequenceExhausted)?;
        self.next_seq
            .compare_exchange(seq, next_seq, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|expected| EventError::SequenceMismatch {
                expected,
                actual: seq,
            })
    }

    #[cfg(feature = "client")]
    fn allocate_sequence(&self) -> Result<u64, EventError> {
        self.next_seq
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |seq| {
                seq.checked_add(1)
            })
            .map_err(|_| EventError::SequenceExhausted)
    }

    #[cfg(feature = "client")]
    fn record_local_terminal(&self, kind: AgentEventKind) {
        if kind.is_terminal()
            && let Some(state) = &self.turn_admission
        {
            state.fetch_or(TURN_COMMITTED, Ordering::Release);
        }
    }

    #[cfg(feature = "client")]
    fn receivers_are_closed(&self) -> bool {
        self.session.is_closed()
            && self
                .turn
                .as_ref()
                .is_none_or(mpsc::UnboundedSender::is_closed)
    }

    fn publish(&self, event: TimedAgentEvent) {
        drop(self.session.send(event.clone()));
        if let Some(turn) = &self.turn {
            drop(turn.send(event));
        }
    }
}

fn wait_for_terminal_claim(state: &AtomicU64, observed: u64) {
    let mut spins = 0;
    while state.load(Ordering::Acquire) == observed {
        if spins < TERMINAL_SPINS_BEFORE_YIELD {
            spins += 1;
            std::hint::spin_loop();
        } else {
            spins = 0;
            std::thread::yield_now();
        }
    }
}

/// Cloneable publisher for already-formed canonical agent events.
///
/// A publisher validates protocol version, request identity, and contiguous
/// sequence order before routing the same retained event to the session stream
/// and, when present, one turn stream. Callers must provide one ordered source;
/// publishing concurrently does not add delivery ordering beyond the underlying
/// event channel.
#[derive(Clone)]
pub struct AgentEventPublisher {
    channel: EventChannel,
}

impl AgentEventPublisher {
    /// Creates a publisher and its independently consumed session event stream.
    #[must_use]
    pub fn channel(request_id: impl Into<Arc<str>>) -> (Self, AgentEvents) {
        let request_id = request_id.into();
        let (channel, events) = EventChannel::channel(request_id);
        (Self { channel }, events)
    }

    /// Returns the stable request/session identity accepted by this publisher.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.channel.request_id
    }

    /// Returns whether this per-turn publisher has emitted its terminal event.
    #[doc(hidden)]
    #[must_use]
    pub fn turn_is_terminal(&self) -> bool {
        self.channel.turn_is_terminal()
    }

    /// Creates a publisher that also mirrors its events into a turn stream.
    ///
    /// The returned publisher shares validation and session ordering with this
    /// publisher. Dropping it closes only its turn stream once all of its clones
    /// have also been dropped.
    #[must_use]
    pub fn mirrored_channel(&self) -> (Self, AgentEvents) {
        let (channel, events) = self.channel.mirrored_channel();
        (Self { channel }, events)
    }

    /// Validates and publishes one already-formed canonical event.
    ///
    /// The retained raw payload is not decoded or re-encoded.
    ///
    /// # Errors
    ///
    /// Returns an error when the protocol version or request identity differs,
    /// or when the sequence is not exactly the next expected value.
    pub fn publish(&self, event: AgentEvent) -> Result<(), EventError> {
        self.publish_timed(TimedAgentEvent {
            event,
            timing: AgentEventTiming {
                emitted_ns: monotonic_now_ns(),
                source_received_ns: None,
            },
        })
    }

    fn publish_timed(&self, event: TimedAgentEvent) -> Result<(), EventError> {
        self.validate(&event.event)?;
        let admission = self.channel.reserve_publisher_event(&event.event)?;
        self.channel.publish(event);
        drop(admission);
        Ok(())
    }

    fn validate(&self, event: &AgentEvent) -> Result<(), EventError> {
        if event.protocol_version != AGENT_EVENT_PROTOCOL_VERSION {
            return Err(EventError::ProtocolVersionMismatch {
                expected: AGENT_EVENT_PROTOCOL_VERSION,
                actual: event.protocol_version,
            });
        }
        if event.request_id != self.channel.request_id {
            return Err(EventError::RequestIdMismatch {
                expected: Arc::clone(&self.channel.request_id),
                actual: Arc::clone(&event.request_id),
            });
        }
        Ok(())
    }
}

/// Internal sequenced emission handle shared by orchestration and transport crates.
#[derive(Clone)]
#[cfg(feature = "client")]
pub struct EventSink {
    publisher: AgentEventPublisher,
}

#[cfg(feature = "client")]
impl EventSink {
    /// Creates an emission handle and its independently consumed event stream.
    #[must_use]
    pub fn channel(request_id: String) -> (Self, AgentEvents) {
        let (publisher, events) = AgentEventPublisher::channel(request_id);
        (Self { publisher }, events)
    }

    /// Returns the stable request/session identity attached to emitted events.
    #[must_use]
    pub fn request_id(&self) -> &str {
        self.publisher.request_id()
    }

    /// Creates a sink that mirrors its events into one independently owned stream.
    ///
    /// The returned sink preserves the parent sink's request identity and
    /// sequence counter. Dropping every clone of it closes only the mirror;
    /// the original session stream remains available.
    #[must_use]
    pub fn mirrored_channel(&self) -> (Self, AgentEvents) {
        let (publisher, events) = self.publisher.mirrored_channel();
        (Self { publisher }, events)
    }

    /// Emits an event when a receiver is present and otherwise discards it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be converted to JSON or the
    /// sequence is exhausted.
    pub fn emit<P: Serialize>(&self, kind: AgentEventKind, payload: P) -> Result<(), EventError> {
        self.emit_with_sequence(kind, payload).map(|_| ())
    }

    /// Emits an event and returns its session-monotonic sequence number.
    ///
    /// This is intended for transport telemetry that must correlate the point
    /// of emission with a downstream consumer without retaining payload data.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be converted to JSON or the
    /// sequence is exhausted.
    pub fn emit_with_sequence<P: Serialize>(
        &self,
        kind: AgentEventKind,
        payload: P,
    ) -> Result<u64, EventError> {
        self.emit_with_source_sequence(kind, payload, None)
    }

    /// Emits an event correlated with the process-monotonic source receipt time.
    pub fn emit_with_source_sequence<P: Serialize>(
        &self,
        kind: AgentEventKind,
        payload: P,
        source_received_ns: Option<u64>,
    ) -> Result<u64, EventError> {
        if self.publisher.channel.receivers_are_closed() {
            let seq = self.publisher.channel.allocate_sequence()?;
            self.publisher.channel.record_local_terminal(kind);
            return Ok(seq);
        }

        let payload = Arc::from(to_raw_value(&payload).map_err(EventError::Encode)?);
        let seq = self.publisher.channel.allocate_sequence()?;
        let event = TimedAgentEvent {
            event: AgentEvent {
                protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
                request_id: Arc::clone(&self.publisher.channel.request_id),
                seq,
                kind,
                payload,
            },
            timing: AgentEventTiming {
                emitted_ns: monotonic_now_ns(),
                source_received_ns,
            },
        };
        self.publisher.channel.publish(event);
        self.publisher.channel.record_local_terminal(kind);
        Ok(seq)
    }

    /// Creates a local sequenced emitter over an existing canonical publisher.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_publisher(publisher: AgentEventPublisher) -> Self {
        Self { publisher }
    }

    /// Returns the backend-neutral canonical publisher used by this emitter.
    #[doc(hidden)]
    #[must_use]
    pub fn publisher(&self) -> AgentEventPublisher {
        self.publisher.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::Ordering};

    #[cfg(feature = "client")]
    use serde::{Serialize, Serializer};
    #[cfg(feature = "client")]
    use serde_json::json;
    use serde_json::value::RawValue;

    #[cfg(feature = "client")]
    use super::super::{AgentEventData, AssistantEvent, ToolEvent, TransportEvent};
    #[cfg(feature = "client")]
    use super::EventSink;
    use super::{
        AGENT_EVENT_PROTOCOL_VERSION, AgentEvent, AgentEventKind, AgentEventPublisher,
        AgentEventTiming, EventError, TimedAgentEvent,
    };

    #[cfg(feature = "client")]
    #[test]
    fn events_are_ordered_and_receiver_drop_is_not_an_error() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        assert_eq!(receiver.request_id(), "request-1");
        events
            .emit(AgentEventKind::RunStarted, json!({ "n": 1 }))
            .unwrap();
        events
            .emit(AgentEventKind::RunCompleted, json!({ "n": 2 }))
            .unwrap();
        let first = receiver.receiver.try_recv().unwrap().event;
        let second = receiver.receiver.try_recv().unwrap().event;
        assert_eq!((first.seq, first.kind), (1, AgentEventKind::RunStarted));
        assert_eq!((second.seq, second.kind), (2, AgentEventKind::RunCompleted));
        assert_eq!(
            second.decode_payload::<serde_json::Value>().unwrap()["n"],
            2
        );
        drop(receiver);
        events.emit(AgentEventKind::RunFailed, json!({})).unwrap();
    }

    #[cfg(feature = "client")]
    #[test]
    fn receiver_drop_skips_payload_serialization() {
        struct MustNotSerialize;

        impl Serialize for MustNotSerialize {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                panic!("closed event streams must not serialize payloads")
            }
        }

        let (events, receiver) = EventSink::channel("request-1".to_owned());
        drop(receiver);

        assert_eq!(
            events
                .emit_with_sequence(AgentEventKind::ApiEvent, MustNotSerialize)
                .unwrap(),
            1
        );
    }

    #[cfg(feature = "client")]
    #[test]
    fn timing_is_private_and_preserves_the_jsonl_contract() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        let source_received_ns = super::monotonic_now_ns();
        events
            .emit_with_source_sequence(
                AgentEventKind::AssistantDelta,
                json!({ "text": "x" }),
                Some(source_received_ns),
            )
            .unwrap();

        let timed = receiver.receiver.try_recv().unwrap();
        assert_eq!(timed.timing.source_received_ns, Some(source_received_ns));
        assert!(timed.timing.emitted_ns >= source_received_ns);
        let encoded = serde_json::to_value(&timed.event).unwrap();
        assert!(encoded.get("timing").is_none());
        assert!(encoded.get("source_received_ns").is_none());
        assert_eq!(encoded["type"], "assistant.delta");
    }

    #[cfg(feature = "client")]
    #[test]
    fn timed_events_can_be_drained_without_async_receive_round_trips() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        for n in 1..=3 {
            events
                .emit(AgentEventKind::AssistantDelta, json!({ "n": n }))
                .unwrap();
        }

        let sequences = std::iter::from_fn(|| receiver.try_recv_timed())
            .map(|event| event.event.seq)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3]);
        assert!(receiver.try_recv_timed().is_none());
    }

    #[cfg(feature = "client")]
    #[test]
    fn mirrored_stream_preserves_session_order_and_closes_independently() {
        let (events, mut session) = EventSink::channel("request-1".to_owned());
        let (turn_events, mut turn) = events.mirrored_channel();

        turn_events
            .emit(AgentEventKind::RunStarted, json!({ "turn": 1 }))
            .unwrap();
        turn_events
            .emit(AgentEventKind::RunCompleted, json!({ "turn": 1 }))
            .unwrap();

        let session_first = session.receiver.try_recv().unwrap().event;
        let session_second = session.receiver.try_recv().unwrap().event;
        let turn_first = turn.receiver.try_recv().unwrap().event;
        let turn_second = turn.receiver.try_recv().unwrap().event;
        assert_eq!(
            (session_first.seq, session_second.seq),
            (turn_first.seq, turn_second.seq)
        );
        assert_eq!(turn_second.kind, AgentEventKind::RunCompleted);

        drop(turn_events);
        assert!(turn.receiver.try_recv().is_err());
        events
            .emit(AgentEventKind::RunStarted, json!({ "turn": 2 }))
            .unwrap();
        assert_eq!(
            session.receiver.try_recv().unwrap().event.seq,
            session_second.seq + 1
        );
    }

    #[test]
    fn publisher_preserves_raw_payload_and_routes_one_event_to_session_and_turn() {
        let (publisher, mut session) = AgentEventPublisher::channel("request-1");
        let (publisher, mut turn) = publisher.mirrored_channel();
        let payload = Arc::<RawValue>::from(
            serde_json::from_str::<Box<RawValue>>(r#"{ "text": "exact", "n": 1 }"#).unwrap(),
        );
        let event = AgentEvent {
            protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
            request_id: Arc::from("request-1"),
            seq: 1,
            kind: AgentEventKind::AssistantDelta,
            payload: Arc::clone(&payload),
        };

        publisher.publish(event).unwrap();

        let session_event = session.receiver.try_recv().unwrap().event;
        let turn_event = turn.receiver.try_recv().unwrap().event;
        assert!(Arc::ptr_eq(&session_event.payload, &payload));
        assert!(Arc::ptr_eq(&turn_event.payload, &payload));
        assert_eq!(
            session_event.payload.get(),
            r#"{ "text": "exact", "n": 1 }"#
        );
        assert_eq!(session_event.seq, turn_event.seq);
    }

    #[test]
    fn publisher_rejects_protocol_identity_and_replayed_sequence_without_advancing() {
        let (publisher, mut events) = AgentEventPublisher::channel("request-1");
        let event = |protocol_version, request_id: &'static str, seq| AgentEvent {
            protocol_version,
            request_id: Arc::from(request_id),
            seq,
            kind: AgentEventKind::RunStarted,
            payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
        };

        assert!(matches!(
            publisher.publish(event(2, "request-1", 1)),
            Err(EventError::ProtocolVersionMismatch { .. })
        ));
        assert!(matches!(
            publisher.publish(event(AGENT_EVENT_PROTOCOL_VERSION, "request-2", 1)),
            Err(EventError::RequestIdMismatch { .. })
        ));
        assert!(matches!(
            publisher.publish(event(AGENT_EVENT_PROTOCOL_VERSION, "request-1", 2)),
            Err(EventError::SequenceMismatch {
                expected: 1,
                actual: 2
            })
        ));
        publisher
            .publish(event(AGENT_EVENT_PROTOCOL_VERSION, "request-1", 1))
            .unwrap();
        assert_eq!(events.receiver.try_recv().unwrap().event.seq, 1);
        assert!(events.receiver.try_recv().is_err());
    }

    #[test]
    fn publisher_records_terminal_after_receivers_close() {
        let (publisher, session) = AgentEventPublisher::channel("request-1");
        let (publisher, turn) = publisher.mirrored_channel();
        drop(session);
        drop(turn);

        publisher
            .publish(AgentEvent {
                protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
                request_id: Arc::from("request-1"),
                seq: 1,
                kind: AgentEventKind::RunCompleted,
                payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
            })
            .unwrap();

        assert!(publisher.turn_is_terminal());
        assert!(matches!(
            publisher.publish(AgentEvent {
                protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
                request_id: Arc::from("request-1"),
                seq: 2,
                kind: AgentEventKind::AssistantDelta,
                payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
            }),
            Err(EventError::TurnAlreadyTerminal)
        ));
    }

    #[test]
    fn terminal_reservation_closes_cloned_publishers_before_delivery() {
        let (publisher, mut session) = AgentEventPublisher::channel("request-1");
        let (publisher, mut turn) = publisher.mirrored_channel();
        let non_terminal = AgentEvent {
            protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
            request_id: Arc::from("request-1"),
            seq: 1,
            kind: AgentEventKind::AssistantDelta,
            payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
        };
        let terminal = AgentEvent {
            protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
            request_id: Arc::from("request-1"),
            seq: 2,
            kind: AgentEventKind::RunCompleted,
            payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
        };
        let admission = publisher
            .channel
            .reserve_publisher_event(&non_terminal)
            .unwrap()
            .expect("non-terminal publication should hold admission");
        let terminal_publisher = publisher.clone();
        let terminal_thread = std::thread::spawn(move || terminal_publisher.publish(terminal));
        while publisher
            .channel
            .turn_admission
            .as_ref()
            .unwrap()
            .load(Ordering::Acquire)
            & super::TURN_CLAIMING
            == 0
        {
            std::thread::yield_now();
        }
        assert!(!publisher.turn_is_terminal());
        let successor = publisher.clone();
        let successor_thread = std::thread::spawn(move || {
            successor.publish(AgentEvent {
                protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
                request_id: Arc::from("request-1"),
                seq: 3,
                kind: AgentEventKind::AssistantDelta,
                payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
            })
        });
        assert!(session.receiver.try_recv().is_err());
        assert!(turn.receiver.try_recv().is_err());

        publisher.channel.publish(TimedAgentEvent {
            event: non_terminal,
            timing: AgentEventTiming {
                emitted_ns: 0,
                source_received_ns: None,
            },
        });
        drop(admission);
        terminal_thread.join().unwrap().unwrap();
        assert!(matches!(
            successor_thread.join().unwrap(),
            Err(EventError::TurnAlreadyTerminal)
        ));

        assert_eq!(
            session.receiver.try_recv().unwrap().event.kind,
            AgentEventKind::AssistantDelta
        );
        assert_eq!(
            turn.receiver.try_recv().unwrap().event.kind,
            AgentEventKind::AssistantDelta
        );
        assert_eq!(
            session.receiver.try_recv().unwrap().event.kind,
            AgentEventKind::RunCompleted
        );
        assert_eq!(
            turn.receiver.try_recv().unwrap().event.kind,
            AgentEventKind::RunCompleted
        );
        assert!(session.receiver.try_recv().is_err());
        assert!(turn.receiver.try_recv().is_err());
    }

    #[test]
    fn failed_terminal_claim_reopens_admission_without_appearing_committed() {
        let (publisher, mut session) = AgentEventPublisher::channel("request-1");
        let (publisher, _turn) = publisher.mirrored_channel();
        let claim = publisher.channel.claim_terminal().unwrap().unwrap();
        assert!(!publisher.turn_is_terminal());

        let waiting = publisher.clone();
        let publish_thread = std::thread::spawn(move || {
            waiting.publish(AgentEvent {
                protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
                request_id: Arc::from("request-1"),
                seq: 1,
                kind: AgentEventKind::AssistantDelta,
                payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
            })
        });
        assert!(matches!(
            publisher.channel.reserve_exact_sequence(2),
            Err(EventError::SequenceMismatch {
                expected: 1,
                actual: 2
            })
        ));
        drop(claim);

        publish_thread.join().unwrap().unwrap();
        assert_eq!(
            session.receiver.try_recv().unwrap().event.kind,
            AgentEventKind::AssistantDelta
        );
        assert!(!publisher.turn_is_terminal());
    }

    #[cfg(feature = "client")]
    #[test]
    fn failed_terminal_claim_preserves_a_local_terminal() {
        let (sink, _session) = EventSink::channel("request-1".to_owned());
        let (sink, _turn) = sink.mirrored_channel();
        let publisher = sink.publisher();
        let claim = publisher.channel.claim_terminal().unwrap().unwrap();
        assert!(!publisher.turn_is_terminal());

        sink.emit(AgentEventKind::RunCompleted, json!({})).unwrap();
        assert!(matches!(
            publisher.channel.reserve_exact_sequence(3),
            Err(EventError::SequenceMismatch {
                expected: 2,
                actual: 3
            })
        ));
        drop(claim);

        assert!(publisher.turn_is_terminal());
        assert!(matches!(
            publisher.publish(AgentEvent {
                protocol_version: AGENT_EVENT_PROTOCOL_VERSION,
                request_id: Arc::from("request-1"),
                seq: 2,
                kind: AgentEventKind::AssistantDelta,
                payload: Arc::from(serde_json::from_str::<Box<RawValue>>("{}").unwrap()),
            }),
            Err(EventError::TurnAlreadyTerminal)
        ));
    }

    #[cfg(feature = "client")]
    #[test]
    fn closed_turn_stream_records_terminal_without_changing_sink_behavior() {
        struct MustNotSerialize;

        impl Serialize for MustNotSerialize {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                panic!("closed event streams must not serialize payloads")
            }
        }

        let (events, session) = EventSink::channel("request-1".to_owned());
        let (turn_events, turn) = events.mirrored_channel();
        drop(session);
        drop(turn);

        turn_events
            .emit(AgentEventKind::RunCompleted, MustNotSerialize)
            .unwrap();
        assert!(turn_events.publisher().turn_is_terminal());
        assert_eq!(
            turn_events
                .emit_with_sequence(AgentEventKind::AssistantDelta, MustNotSerialize)
                .unwrap(),
            2
        );
    }

    #[cfg(feature = "client")]
    #[test]
    fn closed_sink_fails_before_sequence_wrap_without_serializing() {
        struct MustNotSerialize;

        impl Serialize for MustNotSerialize {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                panic!("closed event streams must not serialize payloads")
            }
        }

        let (events, receiver) = EventSink::channel("request-1".to_owned());
        drop(receiver);
        events
            .publisher
            .channel
            .next_seq
            .store(u64::MAX, Ordering::Relaxed);

        assert!(matches!(
            events.emit_with_sequence(AgentEventKind::ApiEvent, MustNotSerialize),
            Err(EventError::SequenceExhausted)
        ));
        assert_eq!(
            events.publisher.channel.next_seq.load(Ordering::Relaxed),
            u64::MAX
        );
    }

    #[cfg(feature = "client")]
    #[test]
    fn typed_projection_preserves_domain_values_and_raw_diagnostics() {
        let (events, mut receiver) = EventSink::channel("request-1".to_owned());
        events
            .emit(
                AgentEventKind::AssistantDelta,
                json!({
                    "model_call_index": 2,
                    "item_id": "item-1",
                    "phase": "final_answer",
                    "text": "hello"
                }),
            )
            .unwrap();
        events
            .emit(
                AgentEventKind::ToolCall,
                json!({
                    "call_id": "call-1",
                    "tool": "deployment_region",
                    "arguments": {"service": "api"},
                    "model_call_index": 2
                }),
            )
            .unwrap();
        events
            .emit(
                AgentEventKind::ModelAttemptRetrying,
                json!({"attempt": 1, "next_attempt": 2}),
            )
            .unwrap();

        let assistant = receiver.receiver.try_recv().unwrap().event;
        let AgentEventData::Assistant(AssistantEvent::Delta(delta)) = assistant.data().unwrap()
        else {
            panic!("assistant delta should use the typed assistant projection");
        };
        assert_eq!(delta.text, "hello");
        assert_eq!(delta.model_call_index, 2);

        let tool = receiver.receiver.try_recv().unwrap().event;
        let AgentEventData::Tool(ToolEvent::Call(call)) = tool.data().unwrap() else {
            panic!("tool call should use the typed tool projection");
        };
        assert_eq!(call.tool, "deployment_region");
        assert_eq!(
            call.decode_arguments::<serde_json::Value>().unwrap()["service"],
            "api"
        );

        let diagnostic = receiver.receiver.try_recv().unwrap().event;
        let AgentEventData::Transport(transport) = diagnostic.data().unwrap() else {
            panic!("retry should remain a lossless transport diagnostic");
        };
        assert_eq!(
            TransportEvent::kind(&transport),
            AgentEventKind::ModelAttemptRetrying
        );
        assert_eq!(
            transport.decode_payload::<serde_json::Value>().unwrap()["next_attempt"],
            2
        );
    }
}
