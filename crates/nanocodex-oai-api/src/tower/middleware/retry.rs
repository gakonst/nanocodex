use std::{
    num::NonZeroU32,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use crate::AgentEventKind;
use ::tower::retry::{Policy, Retry};
use web_time::Instant;

use crate::{
    ModelConfig, ResponsesError, ResponsesTransport,
    attempt::{ResponsesAttempt, ResponsesAttemptKind, ResponsesServiceResponse},
    service::ResponsesService,
    service_error::{FailurePhase, ResponsesServiceError},
    telemetry::{AttemptRetrying, duration_ns, elapsed_ns},
};

use super::delay::{RetryDelay, RetryFuture};

/// Retry policy for the SDK-owned Responses transport stack.
#[derive(Clone, Debug)]
pub struct ResponsesRetryPolicy {
    max_attempts: NonZeroU32,
    delay: RetryDelay,
    standard_transport: Option<ResponsesTransport>,
}

impl ResponsesRetryPolicy {
    /// Default number of total attempts, including the initial request.
    pub const DEFAULT_MAX_ATTEMPTS: NonZeroU32 = NonZeroU32::new(5).unwrap();

    /// Creates a retry policy with a fixed total-attempt limit.
    #[must_use]
    pub const fn new(max_attempts: NonZeroU32) -> Self {
        Self {
            max_attempts,
            delay: RetryDelay::unconfigured(),
            standard_transport: None,
        }
    }

    // Hosted targets retain an `Arc` to the selected host, so this cannot be
    // const across every supported target even though the native adapter is
    // zero-sized.
    #[allow(clippy::missing_const_for_fn)]
    pub(crate) fn for_config(max_attempts: NonZeroU32, config: &ModelConfig) -> Self {
        Self {
            max_attempts,
            delay: RetryDelay::from_config(config),
            standard_transport: None,
        }
    }

    pub(crate) const fn with_standard_transport_fallback(
        mut self,
        transport: ResponsesTransport,
    ) -> Self {
        self.standard_transport = Some(transport);
        self
    }
}

impl Default for ResponsesRetryPolicy {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_ATTEMPTS)
    }
}

impl Policy<ResponsesAttempt, ResponsesServiceResponse, ResponsesServiceError>
    for ResponsesRetryPolicy
{
    type Future = RetryFuture;

    fn retry(
        &mut self,
        request: &mut ResponsesAttempt,
        result: &mut Result<ResponsesServiceResponse, ResponsesServiceError>,
    ) -> Option<Self::Future> {
        request.limit_attempts(self.max_attempts);
        let failure = result.as_ref().err()?;
        let checkpoint_missing =
            failure.is_checkpoint_missing() && request.previous_response_id().is_some();
        let advice = failure.retry_advice;
        let upgrade_required = matches!(
            failure.responses_error(),
            Some(ResponsesError::HandshakeRejected { status: 426, .. })
        );
        let exhausted_websocket_budget = !matches!(request.kind, ResponsesAttemptKind::Warmup)
            && advice.is_some()
            && request.attempt >= request.max_attempts;
        let fallback_reason = if upgrade_required {
            Some("upgrade_required")
        } else if exhausted_websocket_budget {
            Some("retry_exhausted")
        } else {
            None
        };
        if let Some(fallback_reason) = fallback_reason
            && matches!(self.standard_transport, Some(ResponsesTransport::WebSocket))
            && request.activate_https_fallback()
        {
            let failed_attempt = request.attempt;
            let message = failure.source.to_string();
            tracing::warn!(
                target: "nanocodex_oai_api",
                previous_transport = "responses_websocket_v2",
                next_transport = "responses_https_sse",
                reason = fallback_reason,
                phase = request.kind.phase(),
                model.call_index = request.call_index,
                attempt = failed_attempt,
                "falling back from WebSocket to HTTPS Responses transport"
            );
            if matches!(request.kind, ResponsesAttemptKind::Warmup) {
                return None;
            }
            if let Err(error) = request.observer.emit(
                AgentEventKind::ModelAttemptRetrying,
                AttemptRetrying {
                    phase: request.kind,
                    model_call_index: request.call_index,
                    attempt: failed_attempt,
                    next_attempt: 1,
                    max_attempts: request.max_attempts,
                    failure_phase: failure.phase,
                    error_class: "websocket_fallback",
                    delay_ns: 0,
                    server_requested_delay: false,
                    opens_new_socket: false,
                    replay_mode: "full_history",
                    connection_generation: failure.connection_generation,
                    error: &message,
                },
            ) {
                *result = Err(ResponsesServiceError::event(
                    error,
                    FailurePhase::Output,
                    failure.connection_generation,
                ));
                return None;
            }
            request.prepare_transport_fallback();
            request
                .observer
                .stats
                .response_retries
                .fetch_add(1, Ordering::Relaxed);
            return Some(
                self.delay
                    .clone_for_retry()
                    .wait(request.profile.thread_id().to_owned(), Duration::ZERO),
            );
        }
        if !checkpoint_missing && advice.is_none() {
            return None;
        }
        if request.attempt >= request.max_attempts {
            return None;
        }
        let delay = if checkpoint_missing {
            Duration::ZERO
        } else {
            advice
                .and_then(|advice| advice.server_delay)
                .unwrap_or_else(|| retry_delay(request.attempt, request.call_index))
        };
        let error_class = if checkpoint_missing {
            "checkpoint_missing"
        } else {
            advice.map_or("unknown", |advice| advice.class)
        };
        let message = failure.source.to_string();
        if let Err(error) = request.observer.emit(
            AgentEventKind::ModelAttemptRetrying,
            AttemptRetrying {
                phase: request.kind,
                model_call_index: request.call_index,
                attempt: request.attempt,
                next_attempt: request.attempt + 1,
                max_attempts: request.max_attempts,
                failure_phase: failure.phase,
                error_class,
                delay_ns: duration_ns(delay),
                server_requested_delay: advice.is_some_and(|advice| advice.server_delay.is_some()),
                opens_new_socket: !checkpoint_missing,
                replay_mode: "full_history",
                connection_generation: failure.connection_generation,
                error: &message,
            },
        ) {
            *result = Err(ResponsesServiceError::event(
                error,
                FailurePhase::Output,
                failure.connection_generation,
            ));
            return None;
        }
        request
            .observer
            .stats
            .response_retries
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            target: "nanocodex_oai_api",
            phase = request.kind.phase(),
            model.call_index = request.call_index,
            attempt = request.attempt,
            next_attempt = request.attempt + 1,
            error.class = error_class,
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            server_requested_delay = advice.is_some_and(|advice| advice.server_delay.is_some()),
            "retrying Responses attempt"
        );
        if !request.prepare_retry() {
            return None;
        }
        let stats = Arc::clone(&request.observer.stats);
        let delay_runtime = self.delay.clone_for_retry();
        let thread_id = request.profile.thread_id().to_owned();
        Some(Box::pin(async move {
            let started_at = Instant::now();
            delay_runtime.wait(thread_id, delay).await;
            stats
                .retry_backoff_duration_ns
                .fetch_add(elapsed_ns(started_at), Ordering::Relaxed);
        }))
    }

    fn clone_request(&mut self, request: &ResponsesAttempt) -> Option<ResponsesAttempt> {
        let mut request = request.clone();
        request.limit_attempts(self.max_attempts);
        Some(request)
    }
}

/// Standard stateful Responses transport wrapped in the SDK-owned retry policy.
pub type DefaultResponsesService = Retry<ResponsesRetryPolicy, ResponsesService>;

fn retry_delay(attempt: u32, call_index: Option<u32>) -> Duration {
    let base_ms = if cfg!(test) { 1 } else { 200 };
    let exponent = attempt.saturating_sub(1).min(4);
    let raw_ms = base_ms * 2_u64.pow(exponent);
    let seed = u64::from(call_index.unwrap_or_default()) * 31 + u64::from(attempt) * 17;
    let jitter_percent = 90 + seed % 21;
    Duration::from_millis(raw_ms * jitter_percent / 100)
}

#[cfg(test)]
mod tests {
    use super::retry_delay;

    #[test]
    fn local_retry_delay_is_bounded_and_exponential() {
        let first = retry_delay(1, Some(7));
        let second = retry_delay(2, Some(7));
        assert!(first.as_millis() <= 2);
        assert!(second > first);
    }
}
