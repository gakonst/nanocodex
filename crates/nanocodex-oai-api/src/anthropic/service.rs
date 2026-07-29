use std::{
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll},
};

use tower::{Service, retry::Retry};
use tracing::{Instrument, info_span};
use web_time::Instant;

use super::{
    AnthropicAuth, AnthropicAuthMode, translate::build_request, transport::AnthropicHttp,
    wire::SystemBlock,
};
use crate::{
    AgentEventKind, ModelConfig, ResponseItem, ResponsesError,
    openai::{OpenAiError, ResponsesServiceConfig, ResponsesServiceFactory},
    responses::ToolDefinition,
    tower::{
        ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesRetryPolicy,
        ResponsesServiceError, ResponsesServiceResponse,
        service_error::FailurePhase,
        stream::{self, GenerationOutput},
    },
    transport::telemetry::{ApiEvent, AttemptFailed, AttemptStarted, elapsed_ns},
};

const TRANSPORT: &str = "anthropic_messages_sse";

type ServiceFuture = Pin<
    Box<
        dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>>
            + Send
            + 'static,
    >,
>;

/// Session-service factory used by [`super::AnthropicBuilder`].
#[derive(Clone)]
pub struct AnthropicServiceFactory {
    auth: AnthropicAuth,
    model: Arc<str>,
    api_base_url: Arc<str>,
    max_tokens: u32,
    max_attempts: NonZeroU32,
    http_client: reqwest::Client,
}

impl AnthropicServiceFactory {
    pub(crate) fn new(
        auth: AnthropicAuth,
        model: Arc<str>,
        api_base_url: String,
        max_tokens: u32,
        max_attempts: NonZeroU32,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            auth,
            model,
            api_base_url: api_base_url.into(),
            max_tokens,
            max_attempts,
            http_client,
        }
    }
}

impl ResponsesServiceFactory for AnthropicServiceFactory {
    type Service = Retry<ResponsesRetryPolicy, AnthropicService>;

    fn validate_config(&self, _config: &ResponsesServiceConfig) -> Result<(), OpenAiError> {
        Ok(())
    }

    fn make(&self, config: Arc<ResponsesServiceConfig>) -> Self::Service {
        let service = AnthropicService {
            config,
            auth: self.auth.clone(),
            model: Arc::clone(&self.model),
            api_base_url: Arc::clone(&self.api_base_url),
            max_tokens: self.max_tokens,
            max_attempts: self.max_attempts,
            http: AnthropicHttp::new(self.http_client.clone()),
        };
        Retry::new(ResponsesRetryPolicy::new(self.max_attempts), service)
    }
}

/// One session-owned Anthropic Messages service.
#[derive(Clone)]
pub struct AnthropicService {
    config: Arc<ModelConfig>,
    auth: AnthropicAuth,
    model: Arc<str>,
    api_base_url: Arc<str>,
    max_tokens: u32,
    max_attempts: NonZeroU32,
    http: AnthropicHttp,
}

impl Service<ResponsesAttempt> for AnthropicService {
    type Response = ResponsesServiceResponse;
    type Error = ResponsesServiceError;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: ResponsesAttempt) -> Self::Future {
        request.force_full_replay();
        request.limit_attempts(self.max_attempts);
        let service = self.clone();
        let parent = tracing::Span::current();
        Box::pin(async move {
            let span = info_span!(
                target: "nanocodex_oai_api",
                parent: &parent,
                "responses.attempt",
                otel.kind = "client",
                otel.status_code = tracing::field::Empty,
                phase = request.kind().phase(),
                model.call_index = request.model_call_index(),
                attempt = request.attempt(),
                max_attempts = request.max_attempts,
                transport = TRANSPORT,
                replay.mode = request.replay_mode(),
                model.input.item_count = request.input_item_count(),
                status = tracing::field::Empty,
                duration_ns = tracing::field::Empty,
            );
            service.run(request).instrument(span).await
        })
    }
}

impl AnthropicService {
    async fn run(
        &self,
        request: ResponsesAttempt,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        request
            .observer
            .stats
            .response_attempts
            .fetch_add(1, Ordering::Relaxed);
        let started_at = Instant::now();
        request.observer.emit(
            AgentEventKind::ModelAttemptStarted,
            AttemptStarted {
                phase: request.kind,
                model_call_index: request.call_index,
                attempt: request.attempt,
                max_attempts: request.max_attempts,
                replay_mode: request.replay_mode(),
                previous_response_id: request.previous_response_id(),
                connection_generation: 0,
            },
        )?;
        let result = self.run_inner(&request, started_at).await;
        tracing::Span::current().record(
            "status",
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        tracing::Span::current().record(
            "otel.status_code",
            if result.is_ok() { "OK" } else { "ERROR" },
        );
        tracing::Span::current().record("duration_ns", elapsed_ns(started_at));
        if let Err(failure) = &result {
            let message = failure.to_string();
            request.observer.emit(
                AgentEventKind::ModelAttemptFailed,
                AttemptFailed {
                    phase: request.kind,
                    model_call_index: request.call_index,
                    attempt: request.attempt,
                    max_attempts: request.max_attempts,
                    duration_ns: elapsed_ns(started_at),
                    failure_phase: failure.phase,
                    error_class: failure.error_class(),
                    retryable: failure.is_retryable(),
                    connection_generation: failure.connection_generation,
                    error: &message,
                },
            )?;
        }
        result
    }

    async fn run_inner(
        &self,
        request: &ResponsesAttempt,
        started_at: Instant,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        if matches!(request.kind(), ResponsesAttemptKind::Warmup) {
            return Err(ResponsesServiceError::invalid_attempt_state(
                "the Anthropic provider does not perform a warmup request",
                FailurePhase::Protocol,
                0,
            ));
        }
        if matches!(request.kind(), ResponsesAttemptKind::Compaction) {
            return Err(ResponsesServiceError::invalid_attempt_state(
                "the Anthropic provider does not support remote Responses compaction",
                FailurePhase::Protocol,
                0,
            ));
        }

        let mut snapshot = self.auth.snapshot().await.map_err(auth_error)?;
        let body = self.encode(request, snapshot.mode())?;
        let raw = crate::socket::parse_raw_json(&body)?;
        tracing::trace!(
            target: "nanocodex_oai_api",
            direction = "outbound",
            transport = TRANSPORT,
            phase = request.kind().phase(),
            model.call_index = request.model_call_index(),
            api.request = %raw.get(),
            "Anthropic Messages API request"
        );
        request.observer.emit(
            AgentEventKind::ApiEvent,
            ApiEvent {
                direction: "outbound",
                transport: TRANSPORT,
                phase: request.kind().phase(),
                model_call_index: request.model_call_index(),
                event: raw,
            },
        )?;
        let custom_tools = custom_tool_names(request);
        let first = self
            .http
            .send(
                &self.api_base_url,
                &snapshot,
                body.clone(),
                custom_tools.clone(),
            )
            .await;
        let mut stream = match first {
            Err(ResponsesError::HttpRejected { status: 401, .. })
                if snapshot.mode() == AnthropicAuthMode::OAuth =>
            {
                self.auth
                    .recover_unauthorized(&snapshot)
                    .await
                    .map_err(auth_error)?;
                snapshot = self.auth.snapshot().await.map_err(auth_error)?;
                self.http
                    .send(&self.api_base_url, &snapshot, body, custom_tools)
                    .await
            }
            result => result,
        }
        .map_err(ResponsesServiceError::from)?;

        let call_index = request.model_call_index().ok_or_else(|| {
            ResponsesServiceError::invalid_attempt_state(
                "Anthropic generation is missing a model call index",
                crate::tower::service_error::FailurePhase::Protocol,
                0,
            )
        })?;
        let output: GenerationOutput = stream::receive(
            &mut stream,
            TRANSPORT,
            &request.observer,
            call_index,
            started_at,
        )
        .await?;
        Ok(
            ResponsesServiceResponse::new(ResponsesOutput::Generation(output))
                .with_attempt(request.attempt())
                .with_server_reasoning_included(true),
        )
    }

    fn encode(
        &self,
        request: &ResponsesAttempt,
        auth_mode: AnthropicAuthMode,
    ) -> Result<String, ResponsesServiceError> {
        let mut body = build_request(
            &self.model,
            self.max_tokens,
            request.thinking(),
            self.config.system_prompt(),
            request.input_items(),
            &[],
        );
        if auth_mode == AnthropicAuthMode::OAuth {
            body.system.insert(
                0,
                SystemBlock::text(format!(
                    "x-anthropic-billing-header: cc_version={}; cc_entrypoint=nanocodex;",
                    env!("CARGO_PKG_VERSION")
                )),
            );
        }
        serde_json::to_string(&body)
            .map_err(ResponsesError::EncodeRequest)
            .map_err(ResponsesServiceError::from)
    }
}

fn auth_error(error: super::AnthropicAuthError) -> ResponsesServiceError {
    ResponsesServiceError::from(ResponsesError::Authorization {
        detail: error.to_string(),
    })
}

fn custom_tool_names(request: &ResponsesAttempt) -> Vec<String> {
    request
        .input_items()
        .filter_map(|item| match item {
            ResponseItem::AdditionalTools { tools, .. } => Some(tools),
            _ => None,
        })
        .flat_map(|tools| tools.iter())
        .filter_map(|tool| match tool {
            ToolDefinition::Custom { name, .. } => Some(name.to_string()),
            ToolDefinition::Function { .. } | ToolDefinition::ToolSearch { .. } => None,
        })
        .collect()
}
