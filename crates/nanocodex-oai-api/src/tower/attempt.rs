use std::{
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use crate::{
    AgentEventKind, EventError, EventSink, ResponseEvent, ResponseItem, ResponsesTransport,
    Thinking,
    responses::{RequestProfile, ResponseHistory, ResponsesInput, WarmupResponse},
    tower::transport_policy::SessionTransport,
};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::stream::{CompactionOutput, GenerationOutput};

const RESPONSE_MAX_ATTEMPTS: NonZeroU32 = NonZeroU32::new(5).unwrap();

/// Kind of Responses operation passed through the Tower service stack.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResponsesAttemptKind {
    /// Optional WebSocket connection warmup.
    Warmup,
    /// A `response.create` operation.
    Generation,
    /// A `response.compact` operation.
    Compaction,
}

impl ResponsesAttemptKind {
    pub(crate) const fn phase(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Generation => "generation",
            Self::Compaction => "compaction",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ResponsesObserver {
    pub(crate) events: EventSink,
    pub(crate) stats: Arc<TransportStats>,
    response_events: Option<mpsc::Sender<ResponseEvent>>,
}

impl ResponsesObserver {
    pub(crate) fn emit<P: Serialize>(
        &self,
        kind: AgentEventKind,
        payload: P,
    ) -> Result<(), EventError> {
        self.events.emit(kind, payload)
    }

    pub(crate) async fn emit_response(&self, event: ResponseEvent) {
        if let Some(events) = &self.response_events {
            drop(events.send(event).await);
        }
    }
}

/// Shared atomic counters for one transport family.
#[derive(Default)]
pub struct TransportStats {
    pub(crate) connection_attempts: AtomicU32,
    pub(crate) websocket_reconnects: AtomicU32,
    pub(crate) response_attempts: AtomicU32,
    pub(crate) response_retries: AtomicU32,
    pub(crate) connection_duration_ns: AtomicU64,
    pub(crate) retry_backoff_duration_ns: AtomicU64,
}

/// Point-in-time transport counter values used to calculate a delta.
#[derive(Clone, Copy, Default)]
pub struct TransportStatsSnapshot {
    connection_attempts: u32,
    websocket_reconnects: u32,
    response_attempts: u32,
    response_retries: u32,
    connection_duration_ns: u64,
    retry_backoff_duration_ns: u64,
}

impl TransportStats {
    /// Captures the current counter values.
    #[must_use]
    pub fn snapshot(&self) -> TransportStatsSnapshot {
        TransportStatsSnapshot {
            connection_attempts: self.connection_attempts.load(Ordering::Relaxed),
            websocket_reconnects: self.websocket_reconnects.load(Ordering::Relaxed),
            response_attempts: self.response_attempts.load(Ordering::Relaxed),
            response_retries: self.response_retries.load(Ordering::Relaxed),
            connection_duration_ns: self.connection_duration_ns.load(Ordering::Relaxed),
            retry_backoff_duration_ns: self.retry_backoff_duration_ns.load(Ordering::Relaxed),
        }
    }

    /// Calculates counters accumulated since an earlier snapshot.
    #[must_use]
    pub fn since(&self, before: TransportStatsSnapshot) -> TransportStatsDelta {
        let after = self.snapshot();
        TransportStatsDelta {
            connection_attempts: after
                .connection_attempts
                .saturating_sub(before.connection_attempts),
            websocket_reconnects: after
                .websocket_reconnects
                .saturating_sub(before.websocket_reconnects),
            response_attempts: after
                .response_attempts
                .saturating_sub(before.response_attempts),
            response_retries: after
                .response_retries
                .saturating_sub(before.response_retries),
            connection_duration_ns: after
                .connection_duration_ns
                .saturating_sub(before.connection_duration_ns),
            retry_backoff_duration_ns: after
                .retry_backoff_duration_ns
                .saturating_sub(before.retry_backoff_duration_ns),
        }
    }
}

/// Per-run transport counters derived from the process-wide service counters.
#[derive(Clone, Copy, Default)]
pub struct TransportStatsDelta {
    /// Physical connection attempts.
    pub connection_attempts: u32,
    /// Successful WebSocket replacements after the initial connection.
    pub websocket_reconnects: u32,
    /// Physical Responses attempts.
    pub response_attempts: u32,
    /// Responses retries after the first physical attempt.
    pub response_retries: u32,
    /// Nanoseconds spent establishing connections.
    pub connection_duration_ns: u64,
    /// Nanoseconds spent waiting for owned retry backoff.
    pub retry_backoff_duration_ns: u64,
}

/// One logical Responses operation, including the complete input required for a safe retry.
#[derive(Clone)]
pub struct ResponsesAttempt {
    pub(crate) kind: ResponsesAttemptKind,
    pub(crate) call_index: Option<u32>,
    full_history: ResponseHistory,
    incremental_history: ResponseHistory,
    incremental_start: usize,
    tail: Option<ResponseItem>,
    previous_response_id: Option<String>,
    thinking: Thinking,
    fast_mode: bool,
    pub(crate) profile: Arc<RequestProfile>,
    pub(crate) observer: ResponsesObserver,
    pub(crate) attempt: u32,
    pub(crate) max_attempts: u32,
    full_replay: bool,
    pub(crate) logical_turn: u64,
    session_transport: Arc<SessionTransport>,
}

impl ResponsesAttempt {
    fn warmup(
        thinking: Thinking,
        fast_mode: bool,
        profile: Arc<RequestProfile>,
        observer: ResponsesObserver,
        session_transport: Arc<SessionTransport>,
    ) -> Self {
        Self {
            kind: ResponsesAttemptKind::Warmup,
            call_index: None,
            full_history: ResponseHistory::default(),
            incremental_history: ResponseHistory::default(),
            incremental_start: 0,
            tail: None,
            previous_response_id: None,
            thinking,
            fast_mode,
            profile,
            observer,
            attempt: 1,
            max_attempts: 1,
            full_replay: false,
            logical_turn: 0,
            session_transport,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn generation(
        call_index: u32,
        full_history: ResponseHistory,
        incremental_history: ResponseHistory,
        incremental_start: usize,
        previous_response_id: Option<&str>,
        thinking: Thinking,
        fast_mode: bool,
        profile: Arc<RequestProfile>,
        observer: ResponsesObserver,
        session_transport: Arc<SessionTransport>,
    ) -> Self {
        Self {
            kind: ResponsesAttemptKind::Generation,
            call_index: Some(call_index),
            full_history,
            incremental_history,
            incremental_start,
            tail: None,
            previous_response_id: previous_response_id.map(str::to_owned),
            thinking,
            fast_mode,
            profile,
            observer,
            attempt: 1,
            max_attempts: RESPONSE_MAX_ATTEMPTS.get(),
            full_replay: previous_response_id.is_none(),
            logical_turn: 0,
            session_transport,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compaction(
        call_index: u32,
        full_history: ResponseHistory,
        incremental_history: ResponseHistory,
        incremental_start: usize,
        previous_response_id: Option<&str>,
        trigger: ResponseItem,
        thinking: Thinking,
        fast_mode: bool,
        profile: Arc<RequestProfile>,
        observer: ResponsesObserver,
        session_transport: Arc<SessionTransport>,
    ) -> Self {
        Self {
            kind: ResponsesAttemptKind::Compaction,
            call_index: Some(call_index),
            full_history,
            incremental_history,
            incremental_start,
            tail: Some(trigger),
            previous_response_id: previous_response_id.map(str::to_owned),
            thinking,
            fast_mode,
            profile,
            observer,
            attempt: 1,
            max_attempts: RESPONSE_MAX_ATTEMPTS.get(),
            full_replay: previous_response_id.is_none(),
            logical_turn: 0,
            session_transport,
        }
    }

    pub(crate) fn input(&self) -> ResponsesInput<'_> {
        if matches!(self.kind, ResponsesAttemptKind::Warmup) {
            return ResponsesInput::new(self.profile.prefix(), &[], None);
        }
        if self.full_replay {
            ResponsesInput::history(
                self.profile.prefix(),
                &self.full_history,
                self.tail.as_ref(),
            )
        } else {
            ResponsesInput::history_suffix(
                &[],
                &self.incremental_history,
                self.incremental_start,
                self.tail.as_ref(),
            )
        }
    }

    /// Returns the provider operation represented by this attempt.
    #[must_use]
    pub const fn kind(&self) -> ResponsesAttemptKind {
        self.kind
    }

    /// Returns the session-monotonic model call index, if this is not warmup.
    #[must_use]
    pub const fn model_call_index(&self) -> Option<u32> {
        self.call_index
    }

    /// Returns the reasoning effort fixed for this replayable attempt.
    #[must_use]
    pub const fn thinking(&self) -> Thinking {
        self.thinking
    }

    /// Returns whether this replayable attempt uses priority service.
    #[must_use]
    pub const fn fast_mode(&self) -> bool {
        self.fast_mode
    }

    /// Returns the current physical attempt number.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Iterates over the exact input items this physical attempt will send.
    pub fn input_items(&self) -> impl Iterator<Item = &ResponseItem> {
        self.input().iter()
    }

    /// Iterates over the complete authoritative input for a full-history replay.
    ///
    /// Responses-compatible services that do not implement provider-side
    /// continuation IDs can use this view on every physical attempt.
    pub fn full_replay_input_items(&self) -> impl Iterator<Item = &ResponseItem> {
        ResponsesInput::history(
            self.profile.prefix(),
            &self.full_history,
            self.tail.as_ref(),
        )
        .iter()
    }

    /// Emits one normalized event for callers streaming this attempt.
    ///
    /// Custom Tower services installed with [`crate::OpenAiBuilder::service`]
    /// use this method when they can expose live provider events. A service
    /// that only returns a completed aggregate may omit it; the managed
    /// response stream synthesizes its terminal completion event.
    pub async fn emit(&self, event: ResponseEvent) {
        self.observer.emit_response(event).await;
    }

    /// Returns the exact number of input items this physical attempt will send.
    #[must_use]
    pub fn input_item_count(&self) -> usize {
        self.input().len()
    }

    /// Returns the private provider continuation ID used by this attempt.
    ///
    /// Full-replay attempts deliberately return `None`.
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        (!self.full_replay)
            .then_some(self.previous_response_id.as_deref())
            .flatten()
    }

    /// Returns whether this attempt sends complete authoritative history.
    #[must_use]
    pub const fn is_full_replay(&self) -> bool {
        self.full_replay
    }

    pub(crate) const fn replay_mode(&self) -> &'static str {
        if self.full_replay {
            "full_history"
        } else {
            "incremental"
        }
    }

    pub(crate) const fn prepare_retry(&mut self) -> bool {
        if self.attempt >= self.max_attempts {
            return false;
        }
        self.attempt += 1;
        self.full_replay = true;
        true
    }

    pub(crate) const fn prepare_transport_fallback(&mut self) {
        self.attempt = 1;
        self.full_replay = true;
    }

    pub(crate) fn effective_transport(&self, preferred: ResponsesTransport) -> ResponsesTransport {
        self.session_transport.effective(preferred)
    }

    pub(crate) fn activate_https_fallback(&self) -> bool {
        self.session_transport.activate_https_fallback()
    }

    pub(crate) fn limit_attempts(&mut self, max_attempts: NonZeroU32) {
        self.max_attempts = self.max_attempts.min(max_attempts.get());
    }

    pub(crate) const fn force_full_replay(&mut self) {
        self.full_replay = true;
    }
}

/// Complete output returned by one low-level Responses Tower call.
pub enum ResponsesOutput {
    /// WebSocket warmup completed.
    Warmup(WarmupResponse),
    /// `response.create` completed.
    Generation(GenerationOutput),
    /// `response.compact` completed.
    Compaction(CompactionOutput),
}

/// Complete result produced by a Tower service for one Responses attempt.
pub struct ResponsesServiceResponse {
    pub(crate) output: ResponsesOutput,
    pub(crate) attempt: u32,
    pub(crate) connection_generation: u32,
    pub(crate) server_reasoning_included: bool,
}

impl ResponsesServiceResponse {
    /// Wraps completed output from a caller-supplied Tower service.
    #[must_use]
    pub const fn new(output: ResponsesOutput) -> Self {
        Self {
            output,
            attempt: 1,
            connection_generation: 0,
            server_reasoning_included: false,
        }
    }

    /// Records the physical attempt that produced this completed output.
    #[must_use]
    pub const fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    /// Records the transport connection generation that produced this output.
    #[must_use]
    pub const fn with_connection_generation(mut self, connection_generation: u32) -> Self {
        self.connection_generation = connection_generation;
        self
    }

    /// Records whether the server already accounted for retained reasoning.
    #[must_use]
    pub const fn with_server_reasoning_included(mut self, included: bool) -> Self {
        self.server_reasoning_included = included;
        self
    }

    /// Returns the physical attempt that produced this output.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Returns the transport connection generation that produced this output.
    #[must_use]
    pub const fn connection_generation(&self) -> u32 {
        self.connection_generation
    }

    /// Returns whether the server already accounted for retained reasoning.
    #[must_use]
    pub const fn server_reasoning_included(&self) -> bool {
        self.server_reasoning_included
    }

    /// Consumes the transport metadata and returns the completed output.
    #[must_use]
    pub fn into_output(self) -> ResponsesOutput {
        self.output
    }
}

/// Builds replayable low-level Responses attempts against one stable profile.
#[must_use]
pub struct ResponsesAttemptFactory {
    profile: Arc<RequestProfile>,
    observer: ResponsesObserver,
    logical_turn: u64,
    session_transport: Arc<SessionTransport>,
}

impl ResponsesAttemptFactory {
    /// Creates a low-level attempt factory with shared events and counters.
    pub fn new(profile: RequestProfile, events: EventSink, stats: Arc<TransportStats>) -> Self {
        Self {
            profile: Arc::new(profile),
            observer: ResponsesObserver {
                events,
                stats,
                response_events: None,
            },
            logical_turn: 0,
            session_transport: Arc::new(SessionTransport::new()),
        }
    }

    /// Replaces the event destination without changing request or retry state.
    pub fn set_events(&mut self, events: EventSink) {
        self.observer.events = events;
    }

    pub(crate) fn with_response_events(
        mut self,
        response_events: mpsc::Sender<ResponseEvent>,
    ) -> Self {
        self.observer.response_events = Some(response_events);
        self
    }

    /// Returns an attempt factory scoped to one client-side logical turn.
    pub fn for_logical_turn(&self, logical_turn: u64) -> Self {
        Self {
            profile: Arc::clone(&self.profile),
            observer: self.observer.clone(),
            logical_turn,
            session_transport: Arc::clone(&self.session_transport),
        }
    }

    /// Returns the immutable request profile shared by generated attempts.
    #[must_use]
    pub fn profile(&self) -> &RequestProfile {
        &self.profile
    }

    /// Builds a WebSocket warmup attempt.
    #[must_use]
    pub fn warmup(&self, thinking: Thinking, fast_mode: bool) -> ResponsesAttempt {
        let mut attempt = ResponsesAttempt::warmup(
            thinking,
            fast_mode,
            Arc::clone(&self.profile),
            self.observer.clone(),
            Arc::clone(&self.session_transport),
        );
        attempt.logical_turn = self.logical_turn;
        attempt
    }

    /// Builds a replayable `response.create` attempt.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn generation(
        &self,
        call_index: u32,
        full_history: ResponseHistory,
        incremental_history: ResponseHistory,
        incremental_start: usize,
        previous_response_id: Option<&str>,
        thinking: Thinking,
        fast_mode: bool,
    ) -> ResponsesAttempt {
        let mut attempt = ResponsesAttempt::generation(
            call_index,
            full_history,
            incremental_history,
            incremental_start,
            previous_response_id,
            thinking,
            fast_mode,
            Arc::clone(&self.profile),
            self.observer.clone(),
            Arc::clone(&self.session_transport),
        );
        attempt.logical_turn = self.logical_turn;
        attempt
    }

    #[allow(clippy::too_many_arguments)]
    /// Builds a replayable `response.compact` attempt.
    #[must_use]
    pub fn compaction(
        &self,
        call_index: u32,
        full_history: ResponseHistory,
        incremental_history: ResponseHistory,
        incremental_start: usize,
        previous_response_id: Option<&str>,
        trigger: ResponseItem,
        thinking: Thinking,
        fast_mode: bool,
    ) -> ResponsesAttempt {
        let mut attempt = ResponsesAttempt::compaction(
            call_index,
            full_history,
            incremental_history,
            incremental_start,
            previous_response_id,
            trigger,
            thinking,
            fast_mode,
            Arc::clone(&self.profile),
            self.observer.clone(),
            Arc::clone(&self.session_transport),
        );
        attempt.logical_turn = self.logical_turn;
        attempt
    }
}

#[cfg(test)]
mod tests {
    use super::{ResponseHistory, ResponsesAttemptFactory, TransportStats};
    use crate::{
        ContentItem, EventSink, MessageRole, ResponseItem, ResponsesTransport, Thinking,
        responses::RequestProfile,
    };
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn retry_preserves_the_attempts_turn_policy() {
        let (events, _receiver) = EventSink::channel("attempt-test".to_owned());
        let factory = ResponsesAttemptFactory::new(
            RequestProfile::new("attempt-test", "attempt-test", Arc::from([])),
            events,
            Arc::new(TransportStats::default()),
        );
        let mut attempt = factory.generation(
            1,
            ResponseHistory::default(),
            ResponseHistory::default(),
            0,
            Some("resp-previous"),
            Thinking::High,
            true,
        );

        assert_eq!(attempt.thinking(), Thinking::High);
        assert!(attempt.fast_mode());
        assert!(attempt.prepare_retry());
        assert_eq!(attempt.thinking(), Thinking::High);
        assert!(attempt.fast_mode());
    }

    #[test]
    fn compaction_without_a_checkpoint_replays_authoritative_history() {
        let (events, _receiver) = EventSink::channel("attempt-test".to_owned());
        let factory = ResponsesAttemptFactory::new(
            RequestProfile::new("attempt-test", "attempt-test", Arc::from([])),
            events,
            Arc::new(TransportStats::default()),
        );
        let history = ResponseHistory::new(vec![ResponseItem::message(
            MessageRole::User,
            [ContentItem::InputText {
                text: "retained history".into(),
            }],
        )]);
        let attempt = factory.compaction(
            1,
            history.clone(),
            history,
            1,
            None,
            ResponseItem::compaction_trigger(),
            Thinking::Medium,
            false,
        );

        assert!(attempt.is_full_replay());
        assert_eq!(attempt.previous_response_id(), None);
        assert_eq!(
            serde_json::to_value(attempt.input_items().collect::<Vec<_>>()).unwrap(),
            json!([
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "retained history" }]
                },
                { "type": "compaction_trigger" }
            ])
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn fallback_state_survives_new_attempts_but_not_new_sessions() {
        let (events, _receiver) = EventSink::channel("attempt-test".to_owned());
        let factory = ResponsesAttemptFactory::new(
            RequestProfile::new("attempt-test", "attempt-test", Arc::from([])),
            events,
            Arc::new(TransportStats::default()),
        );
        let first = factory.generation(
            1,
            ResponseHistory::default(),
            ResponseHistory::default(),
            0,
            None,
            Thinking::High,
            false,
        );
        assert!(first.activate_https_fallback());

        let next = factory.generation(
            2,
            ResponseHistory::default(),
            ResponseHistory::default(),
            0,
            None,
            Thinking::High,
            false,
        );
        assert!(matches!(
            next.effective_transport(ResponsesTransport::WebSocket),
            ResponsesTransport::Https
        ));

        let (fresh_events, _fresh_receiver) = EventSink::channel("fresh-attempt-test".to_owned());
        let fresh_factory = ResponsesAttemptFactory::new(
            RequestProfile::new("fresh-attempt-test", "fresh-attempt-test", Arc::from([])),
            fresh_events,
            Arc::new(TransportStats::default()),
        );
        let fresh = fresh_factory.generation(
            1,
            ResponseHistory::default(),
            ResponseHistory::default(),
            0,
            None,
            Thinking::High,
            false,
        );
        assert!(matches!(
            fresh.effective_transport(ResponsesTransport::WebSocket),
            ResponsesTransport::WebSocket
        ));
    }
}
