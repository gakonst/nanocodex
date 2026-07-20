use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use nanocodex_core::AgentEventKind;
use tower::retry::{Policy, Retry};
use web_time::Instant;

use crate::{
    attempt::{ResponsesAttempt, ResponsesServiceResponse},
    service::ResponsesService,
    service_error::{FailurePhase, ResponsesServiceError},
    telemetry::{AttemptRetrying, duration_ns, elapsed_ns},
};

#[cfg(not(target_family = "wasm"))]
type RetryFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
#[cfg(target_family = "wasm")]
type RetryFuture = Pin<Box<dyn Future<Output = ()>>>;

#[derive(Clone, Copy, Default)]
pub struct ResponsesRetryPolicy;

impl Policy<ResponsesAttempt, ResponsesServiceResponse, ResponsesServiceError>
    for ResponsesRetryPolicy
{
    type Future = RetryFuture;

    fn retry(
        &mut self,
        request: &mut ResponsesAttempt,
        result: &mut Result<ResponsesServiceResponse, ResponsesServiceError>,
    ) -> Option<Self::Future> {
        let failure = result.as_ref().err()?;
        let checkpoint_missing =
            failure.is_checkpoint_missing() && request.previous_response_id().is_some();
        let advice = failure.retry_advice;
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
            target: "nanocodex_service",
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
        Some(Box::pin(async move {
            let started_at = Instant::now();
            sleep(delay).await;
            stats
                .retry_backoff_duration_ns
                .fetch_add(elapsed_ns(started_at), Ordering::Relaxed);
        }))
    }

    fn clone_request(&mut self, request: &ResponsesAttempt) -> Option<ResponsesAttempt> {
        Some(request.clone())
    }
}

#[cfg(not(target_family = "wasm"))]
async fn sleep(delay: Duration) {
    tokio::time::sleep(delay).await;
}

#[cfg(target_family = "wasm")]
async fn sleep(delay: Duration) {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = sleep)]
        fn host_sleep(milliseconds: u32) -> js_sys::Promise;
    }

    let milliseconds = u32::try_from(delay.as_millis()).unwrap_or(u32::MAX);
    drop(JsFuture::from(host_sleep(milliseconds)).await);
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
