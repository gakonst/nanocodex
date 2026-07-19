use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use nanocodex_core::AgentEventKind;
use tower::retry::{Policy, Retry};

use crate::{
    attempt::{ResponsesAttempt, ResponsesServiceResponse},
    service::ResponsesService,
    service_error::{FailurePhase, ResponsesServiceError},
    telemetry::{AttemptRetrying, duration_ns, elapsed_ns},
};

#[derive(Clone, Copy, Default)]
pub struct ResponsesRetryPolicy;

impl Policy<ResponsesAttempt, ResponsesServiceResponse, ResponsesServiceError>
    for ResponsesRetryPolicy
{
    type Future = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

    fn retry(
        &mut self,
        request: &mut ResponsesAttempt,
        result: &mut Result<ResponsesServiceResponse, ResponsesServiceError>,
    ) -> Option<Self::Future> {
        let failure = result.as_ref().err()?;
        let advice = failure.retry_advice?;
        if request.attempt >= request.max_attempts {
            return None;
        }
        let delay = advice
            .server_delay
            .unwrap_or_else(|| retry_delay(request.attempt, request.call_index));
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
                error_class: advice.class,
                delay_ns: duration_ns(delay),
                server_requested_delay: advice.server_delay.is_some(),
                opens_new_socket: true,
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
        if !request.prepare_retry() {
            return None;
        }
        let stats = Arc::clone(&request.observer.stats);
        Some(Box::pin(async move {
            let started_at = Instant::now();
            tokio::time::sleep(delay).await;
            stats
                .retry_backoff_duration_ns
                .fetch_add(elapsed_ns(started_at), Ordering::Relaxed);
        }))
    }

    fn clone_request(&mut self, request: &ResponsesAttempt) -> Option<ResponsesAttempt> {
        Some(request.clone())
    }
}

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
