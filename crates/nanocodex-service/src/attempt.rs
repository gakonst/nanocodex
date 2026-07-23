use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use nanocodex_core::{
    AgentEventKind, EventError, EventSink, ResponseItem, Thinking,
    responses::{RequestProfile, ResponseHistory, ResponsesInput, WarmupResponse},
};
use serde::Serialize;

use crate::stream::{CompactionResult, TurnResult};

const RESPONSE_MAX_ATTEMPTS: u32 = 5;

/// Kind of Responses operation passed through the Tower service stack.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResponsesAttemptKind {
    Warmup,
    Generation,
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
}

impl ResponsesObserver {
    pub(crate) fn emit<P: Serialize>(
        &self,
        kind: AgentEventKind,
        payload: P,
    ) -> Result<(), EventError> {
        self.events.emit(kind, payload)
    }
}

#[derive(Default)]
pub struct TransportStats {
    pub(crate) connection_attempts: AtomicU32,
    pub(crate) websocket_reconnects: AtomicU32,
    pub(crate) response_attempts: AtomicU32,
    pub(crate) response_retries: AtomicU32,
    pub(crate) connection_duration_ns: AtomicU64,
    pub(crate) retry_backoff_duration_ns: AtomicU64,
}

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
    pub connection_attempts: u32,
    pub websocket_reconnects: u32,
    pub response_attempts: u32,
    pub response_retries: u32,
    pub connection_duration_ns: u64,
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
}

impl ResponsesAttempt {
    fn warmup(
        thinking: Thinking,
        fast_mode: bool,
        profile: Arc<RequestProfile>,
        observer: ResponsesObserver,
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
            max_attempts: RESPONSE_MAX_ATTEMPTS,
            full_replay: previous_response_id.is_none(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compaction(
        call_index: u32,
        full_history: ResponseHistory,
        incremental_history: ResponseHistory,
        incremental_start: usize,
        previous_response_id: &str,
        trigger: ResponseItem,
        thinking: Thinking,
        fast_mode: bool,
        profile: Arc<RequestProfile>,
        observer: ResponsesObserver,
    ) -> Self {
        Self {
            kind: ResponsesAttemptKind::Compaction,
            call_index: Some(call_index),
            full_history,
            incremental_history,
            incremental_start,
            tail: Some(trigger),
            previous_response_id: Some(previous_response_id.to_owned()),
            thinking,
            fast_mode,
            profile,
            observer,
            attempt: 1,
            max_attempts: RESPONSE_MAX_ATTEMPTS,
            full_replay: false,
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

    #[must_use]
    pub const fn kind(&self) -> ResponsesAttemptKind {
        self.kind
    }

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

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn input_items(&self) -> impl Iterator<Item = &ResponseItem> {
        self.input().iter()
    }

    #[must_use]
    pub fn input_item_count(&self) -> usize {
        self.input().len()
    }

    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        (!self.full_replay)
            .then_some(self.previous_response_id.as_deref())
            .flatten()
    }

    #[must_use]
    pub const fn is_full_replay(&self) -> bool {
        self.full_replay
    }

    pub(crate) fn replay_mode(&self) -> &'static str {
        if self.full_replay {
            "full_history"
        } else {
            "incremental"
        }
    }

    pub(crate) fn prepare_retry(&mut self) -> bool {
        if self.attempt >= self.max_attempts {
            return false;
        }
        self.attempt += 1;
        self.full_replay = true;
        true
    }

    pub(crate) fn force_full_replay(&mut self) {
        self.full_replay = true;
    }
}

pub enum ResponsesOutput {
    Warmup(WarmupResponse),
    Generation(TurnResult),
    Compaction(CompactionResult),
}

pub struct ResponsesServiceResponse {
    pub(crate) output: ResponsesOutput,
    pub(crate) attempt: u32,
    pub(crate) connection_generation: u32,
    pub(crate) server_reasoning_included: bool,
}

impl ResponsesServiceResponse {
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn connection_generation(&self) -> u32 {
        self.connection_generation
    }

    #[must_use]
    pub const fn server_reasoning_included(&self) -> bool {
        self.server_reasoning_included
    }

    #[must_use]
    pub fn into_output(self) -> ResponsesOutput {
        self.output
    }
}

#[must_use]
pub struct ResponsesAttemptFactory {
    profile: Arc<RequestProfile>,
    observer: ResponsesObserver,
}

impl ResponsesAttemptFactory {
    pub fn new(profile: RequestProfile, events: EventSink, stats: Arc<TransportStats>) -> Self {
        Self {
            profile: Arc::new(profile),
            observer: ResponsesObserver { events, stats },
        }
    }

    #[must_use]
    pub fn profile(&self) -> &RequestProfile {
        &self.profile
    }

    #[must_use]
    pub fn warmup(&self, thinking: Thinking, fast_mode: bool) -> ResponsesAttempt {
        ResponsesAttempt::warmup(
            thinking,
            fast_mode,
            Arc::clone(&self.profile),
            self.observer.clone(),
        )
    }

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
        ResponsesAttempt::generation(
            call_index,
            full_history,
            incremental_history,
            incremental_start,
            previous_response_id,
            thinking,
            fast_mode,
            Arc::clone(&self.profile),
            self.observer.clone(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn compaction(
        &self,
        call_index: u32,
        full_history: ResponseHistory,
        incremental_history: ResponseHistory,
        incremental_start: usize,
        previous_response_id: &str,
        trigger: ResponseItem,
        thinking: Thinking,
        fast_mode: bool,
    ) -> ResponsesAttempt {
        ResponsesAttempt::compaction(
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
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ResponseHistory, ResponsesAttemptFactory, TransportStats};
    use nanocodex_core::{EventSink, Thinking, responses::RequestProfile};
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
}
