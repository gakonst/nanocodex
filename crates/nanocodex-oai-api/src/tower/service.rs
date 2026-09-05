use std::{
    num::NonZeroU32,
    ops::{Deref, DerefMut},
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll},
};

use crate::OpenAiAuthMode;
use crate::{
    AgentEventKind, ModelConfig, OpenAiAuthSnapshot, ResponsesHistory, ResponsesTransport,
    responses::{CreatePolicy, ResponseCreate, WarmupResponse, WarmupServerEvent},
};
use ::tower::{Service, retry::Retry};
use tokio::sync::Mutex;
use tracing::{Instrument, info_span};
use web_time::Instant;

mod https;

use crate::{
    EncodedRequest, ResponsesError,
    attempt::{ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesServiceResponse},
    middleware::{DefaultResponsesService, ResponsesRetryPolicy},
    service_error::{FailurePhase, ResponsesServiceError},
    socket::{ConnectionMetadata, ResponsesSocket, decode_event, parse_raw_json},
    stream,
    telemetry::{
        ApiEvent, AttemptFailed, AttemptStarted, ConnectionCompleted, ConnectionFailed,
        ConnectionPurpose, ConnectionStarted, TRANSPORT, display_endpoint, duration_ns, elapsed_ns,
    },
    transport::platform::{self, ServiceFuture},
};

struct ConnectionState {
    socket: Option<ResponsesSocket>,
    // The first sticky-routing token observed from either transport. It is
    // replayed unchanged within one logical turn and cleared at its boundary.
    turn_state: Option<String>,
    generation: u32,
    next_purpose: ConnectionPurpose,
    server_reasoning_included: bool,
    logical_turn: Option<u64>,
}

struct EstablishedConnection {
    socket: ResponsesSocket,
    metadata: ConnectionMetadata,
    attempt: u32,
    duration_ns: u64,
}

impl ConnectionState {
    const fn new() -> Self {
        Self {
            socket: None,
            turn_state: None,
            generation: 0,
            next_purpose: ConnectionPurpose::Initial,
            server_reasoning_included: false,
            logical_turn: None,
        }
    }

    fn capture_turn_state(&mut self) {
        let turn_state = self
            .socket
            .as_ref()
            .and_then(ResponsesSocket::turn_state)
            .map(str::to_owned);
        self.observe_turn_state(turn_state.as_deref());
    }

    fn observe_turn_state(&mut self, turn_state: Option<&str>) {
        if self.turn_state.is_none() {
            self.turn_state = turn_state.map(str::to_owned);
        }
    }

    fn invalidate(&mut self, purpose: ConnectionPurpose) {
        self.capture_turn_state();
        self.socket = None;
        self.next_purpose = purpose;
    }

    fn cancel_in_flight(&mut self) {
        self.capture_turn_state();
        self.socket = None;
        self.next_purpose = ConnectionPurpose::Reconnect;
    }

    fn enter_logical_turn(&mut self, logical_turn: u64) {
        if self.logical_turn == Some(logical_turn) {
            return;
        }
        self.logical_turn = Some(logical_turn);
        self.turn_state = None;
        if let Some(socket) = &mut self.socket {
            socket.reset_turn_state();
        }
    }
}

struct AttemptGuard<'a> {
    connection: &'a mut ConnectionState,
    request: &'a ResponsesAttempt,
    transport: ResponsesTransport,
    started_at: Instant,
    span: tracing::Span,
    completed: bool,
}

impl<'a> AttemptGuard<'a> {
    fn new(
        connection: &'a mut ConnectionState,
        request: &'a ResponsesAttempt,
        transport: ResponsesTransport,
        started_at: Instant,
    ) -> Self {
        Self {
            connection,
            request,
            transport,
            started_at,
            span: tracing::Span::current(),
            completed: false,
        }
    }

    const fn complete(&mut self) {
        self.completed = true;
    }
}

impl Deref for AttemptGuard<'_> {
    type Target = ConnectionState;

    fn deref(&self) -> &Self::Target {
        self.connection
    }
}

impl DerefMut for AttemptGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
    }
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            let generation = if matches!(self.transport, ResponsesTransport::WebSocket) {
                self.connection.cancel_in_flight();
                self.connection.generation
            } else {
                0
            };
            self.span.record("status", "cancelled");
            self.span.record("otel.status_code", "ERROR");
            self.span.record("duration_ns", elapsed_ns(self.started_at));
            let message = "Responses attempt cancelled before a provider terminal event";
            if let Err(error) = self.request.observer.emit(
                AgentEventKind::ModelAttemptFailed,
                AttemptFailed {
                    phase: self.request.kind,
                    model_call_index: self.request.call_index,
                    attempt: self.request.attempt,
                    max_attempts: self.request.max_attempts,
                    duration_ns: elapsed_ns(self.started_at),
                    failure_phase: FailurePhase::Completion,
                    error_class: "cancelled",
                    retryable: false,
                    connection_generation: generation,
                    error: message,
                },
            ) {
                tracing::error!(
                    target: "nanocodex_oai_api",
                    parent: &self.span,
                    error = %error,
                    "failed to emit cancelled Responses attempt event"
                );
            }
            tracing::warn!(
                target: "nanocodex_oai_api",
                parent: &self.span,
                phase = self.request.kind.phase(),
                model.call_index = self.request.call_index,
                attempt = self.request.attempt,
                transport = self.transport.as_str(),
                connection.generation = generation,
                "Responses attempt cancelled before a provider terminal event"
            );
        }
    }
}

/// Stateful transport attempt service at the base of a Responses Tower stack.
#[derive(Clone)]
pub struct ResponsesService {
    config: Arc<ModelConfig>,
    connection: Arc<Mutex<ConnectionState>>,
    max_attempts: NonZeroU32,
    platform: platform::ServicePlatform,
}

impl ResponsesService {
    /// Builds a stateful transport service with default retry limits.
    #[must_use]
    pub(crate) fn new(config: Arc<ModelConfig>) -> Self {
        let platform = platform::ServicePlatform::new(&config);
        Self {
            config,
            connection: Arc::new(Mutex::new(ConnectionState::new())),
            max_attempts: ResponsesRetryPolicy::DEFAULT_MAX_ATTEMPTS,
            platform,
        }
    }

    /// Builds a transport attempt service with a caller-configured HTTPS
    /// client.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub(crate) fn new_with_http_client(
        config: Arc<ModelConfig>,
        http_client: reqwest::Client,
    ) -> Self {
        let platform = platform::ServicePlatform::with_http_client(&config, http_client);
        Self {
            config,
            connection: Arc::new(Mutex::new(ConnectionState::new())),
            max_attempts: ResponsesRetryPolicy::DEFAULT_MAX_ATTEMPTS,
            platform,
        }
    }

    const fn with_max_attempts(mut self, max_attempts: NonZeroU32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Builds the configured standard Responses service with a fixed total
    /// attempt limit.
    #[must_use]
    pub(crate) fn standard_with_max_attempts(
        config: Arc<ModelConfig>,
        max_attempts: NonZeroU32,
    ) -> DefaultResponsesService {
        let service = Self::new(Arc::clone(&config)).with_max_attempts(max_attempts);
        let retry = ResponsesRetryPolicy::for_config(max_attempts, &config)
            .with_standard_transport_fallback(config.responses_transport);
        Retry::new(retry, service)
    }

    /// Builds the standard retry stack with a caller-configured HTTPS client
    /// and fixed total-attempt limit.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
    #[must_use]
    pub(crate) fn standard_with_http_client_and_max_attempts(
        config: Arc<ModelConfig>,
        http_client: reqwest::Client,
        max_attempts: NonZeroU32,
    ) -> DefaultResponsesService {
        let service = Self::new_with_http_client(Arc::clone(&config), http_client)
            .with_max_attempts(max_attempts);
        let retry = ResponsesRetryPolicy::for_config(max_attempts, &config)
            .with_standard_transport_fallback(config.responses_transport);
        Retry::new(retry, service)
    }

    async fn run(
        &self,
        connection: &mut ConnectionState,
        request: &ResponsesAttempt,
        transport: ResponsesTransport,
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
                connection_generation: connection.generation,
            },
        )?;
        let mut guard = AttemptGuard::new(connection, request, transport, started_at);
        let result = if matches!(transport, ResponsesTransport::WebSocket) {
            self.run_websocket(&mut guard, request, started_at).await
        } else {
            https::run(self, &mut guard, request, started_at).await
        };
        guard.complete();
        drop(guard);
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
        connection.capture_turn_state();
        if let Err(failure) = &result {
            if matches!(request.kind, ResponsesAttemptKind::Warmup) {
                connection.invalidate(ConnectionPurpose::WarmupFallback);
            } else if failure.retry_advice.is_some() {
                connection.invalidate(ConnectionPurpose::Reconnect);
            }
            let message = failure.source.to_string();
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
                    retryable: failure.is_retryable() || failure.is_checkpoint_missing(),
                    connection_generation: failure.connection_generation,
                    error: &message,
                },
            )?;
        }
        result
    }

    async fn run_websocket(
        &self,
        connection: &mut AttemptGuard<'_>,
        request: &ResponsesAttempt,
        started_at: Instant,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        if connection.socket.is_none() {
            self.connect(connection, request).await?;
        }
        let generation = connection.generation;
        let encode_started_at = Instant::now();
        let encoded = self.encode_request(connection, request, ResponsesTransport::WebSocket)?;
        let encode_duration_ns = elapsed_ns(encode_started_at);
        let request_bytes = encoded.raw().get().len();
        let span = tracing::Span::current();
        span.record("request.bytes", request_bytes);
        span.record("request.encode.duration_ns", encode_duration_ns);
        tracing::trace!(
            target: "nanocodex_oai_api",
            direction = "outbound",
            transport = TRANSPORT,
            phase = request.kind.phase(),
            model.call_index = request.call_index,
            api.request = %encoded.raw().get(),
            "OpenAI Responses API request"
        );
        request.observer.emit(
            AgentEventKind::ApiEvent,
            ApiEvent {
                direction: "outbound",
                transport: TRANSPORT,
                phase: request.kind.phase(),
                model_call_index: request.call_index,
                event: encoded.raw(),
            },
        )?;
        if connection.socket.is_none() {
            return Err(ResponsesServiceError::invalid_attempt_state(
                "connection completed without installing a WebSocket",
                FailurePhase::Connect,
                generation,
            ));
        }
        let socket = connection.socket.as_mut().ok_or_else(|| {
            ResponsesServiceError::invalid_attempt_state(
                "connection completed without installing a WebSocket",
                FailurePhase::Connect,
                generation,
            )
        })?;
        let send_started_at = Instant::now();
        socket.send(encoded).await.map_err(|error| {
            ResponsesServiceError::responses(error, FailurePhase::Send, generation)
        })?;
        let send_duration_ns = elapsed_ns(send_started_at);
        span.record("request.send.duration_ns", send_duration_ns);
        let output = match request.kind {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(
                receive_warmup(socket, request)
                    .await
                    .map_err(|error| error.with_connection_generation(generation))?,
            ),
            ResponsesAttemptKind::Generation => ResponsesOutput::Generation(
                stream::receive(
                    socket,
                    ResponsesTransport::WebSocket.as_str(),
                    &request.observer,
                    required_call_index(request)?,
                    started_at,
                )
                .await
                .map_err(|error| error.with_connection_generation(generation))?,
            ),
            ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(
                stream::receive_compaction(
                    socket,
                    ResponsesTransport::WebSocket.as_str(),
                    &request.observer,
                    required_call_index(request)?,
                    started_at,
                )
                .await
                .map_err(|error| error.with_connection_generation(generation))?,
            ),
        };
        let pipeline_stats = match &output {
            ResponsesOutput::Warmup(_) => None,
            ResponsesOutput::Generation(result) => Some(result.pipeline_stats),
            ResponsesOutput::Compaction(result) => Some(result.pipeline_stats),
        };
        if let Some(stats) = pipeline_stats {
            record_pipeline_stats(
                &span,
                request_bytes,
                encode_duration_ns,
                send_duration_ns,
                stats,
            );
        }
        Ok(ResponsesServiceResponse {
            output,
            attempt: request.attempt,
            connection_generation: generation,
            server_reasoning_included: connection.server_reasoning_included,
        })
    }

    fn encode_request(
        &self,
        connection: &ConnectionState,
        request: &ResponsesAttempt,
        transport: ResponsesTransport,
    ) -> Result<EncodedRequest, ResponsesServiceError> {
        let encoded = match request.kind {
            ResponsesAttemptKind::Warmup => EncodedRequest::new(&ResponseCreate::warmup(
                &self.config,
                request.model(),
                request.thinking(),
                request.fast_mode(),
                &request.profile,
                connection.turn_state.as_deref(),
            )),
            ResponsesAttemptKind::Generation | ResponsesAttemptKind::Compaction => {
                EncodedRequest::new(&ResponseCreate::generation_with_policy(
                    &self.config,
                    CreatePolicy::new(
                        transport,
                        request.model(),
                        request.thinking(),
                        request.fast_mode(),
                    ),
                    request.input(),
                    request.previous_response_id(),
                    &request.profile,
                    connection.turn_state.as_deref(),
                ))
            }
        };
        encoded.map_err(|error| {
            ResponsesServiceError::responses(error, FailurePhase::Encode, connection.generation)
        })
    }

    async fn connect(
        &self,
        connection: &mut ConnectionState,
        request: &ResponsesAttempt,
    ) -> Result<(), ResponsesServiceError> {
        let purpose = connection.next_purpose;
        let generation = connection.generation + 1;
        let established = self
            .establish_connection(
                request,
                purpose,
                generation,
                connection.turn_state.as_deref(),
            )
            .await?;
        let EstablishedConnection {
            socket,
            metadata,
            attempt,
            duration_ns,
        } = established;
        connection.generation = generation;
        connection.next_purpose = ConnectionPurpose::Reconnect;
        if !matches!(purpose, ConnectionPurpose::Initial) {
            request
                .observer
                .stats
                .websocket_reconnects
                .fetch_add(1, Ordering::Relaxed);
        }
        if metadata.turn_state.is_some() {
            connection.turn_state.clone_from(&metadata.turn_state);
        }
        connection.server_reasoning_included |= metadata.reasoning_included;
        request.observer.emit(
            AgentEventKind::ModelConnectionCompleted,
            ConnectionCompleted {
                transport: TRANSPORT,
                attempt,
                purpose,
                duration_ns,
                http_status: metadata.status,
                request_id: metadata.request_id.as_deref(),
                server_model: metadata.server_model.as_deref(),
                server_reasoning_included: metadata.reasoning_included,
                connection_generation: connection.generation,
            },
        )?;
        connection.socket = Some(socket);
        Ok(())
    }

    async fn establish_connection(
        &self,
        request: &ResponsesAttempt,
        purpose: ConnectionPurpose,
        generation: u32,
        turn_state: Option<&str>,
    ) -> Result<EstablishedConnection, ResponsesServiceError> {
        let started_at = Instant::now();
        let attempt = request
            .observer
            .stats
            .connection_attempts
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        request.observer.emit(
            AgentEventKind::ModelConnectionStarted,
            ConnectionStarted {
                transport: TRANSPORT,
                websocket_url: display_endpoint(&self.config.websocket_url),
                attempt,
                purpose,
                connection_generation: generation,
            },
        )?;
        let connect_span = info_span!(
            target: "nanocodex_oai_api",
            "responses.connect",
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            purpose = ?purpose,
            connection.generation = generation,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let result = self
            .connect_with_auth_recovery(
                request.profile.session_id(),
                request.profile.thread_id(),
                turn_state,
            )
            .instrument(connect_span.clone())
            .await;
        let elapsed = started_at.elapsed();
        connect_span.record(
            "status",
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        connect_span.record(
            "otel.status_code",
            if result.is_ok() { "OK" } else { "ERROR" },
        );
        connect_span.record("duration_ns", duration_ns(elapsed));
        request
            .observer
            .stats
            .connection_duration_ns
            .fetch_add(duration_ns(elapsed), Ordering::Relaxed);
        let (socket, metadata) = match result {
            Ok(connection) => connection,
            Err(error) => {
                let message = error.to_string();
                request.observer.emit(
                    AgentEventKind::ModelConnectionFailed,
                    ConnectionFailed {
                        transport: TRANSPORT,
                        attempt,
                        purpose,
                        duration_ns: duration_ns(elapsed),
                        error: &message,
                        connection_generation: generation,
                    },
                )?;
                return Err(ResponsesServiceError::responses(
                    error,
                    FailurePhase::Connect,
                    generation.saturating_sub(1),
                ));
            }
        };
        Ok(EstablishedConnection {
            socket,
            metadata,
            attempt,
            duration_ns: duration_ns(elapsed),
        })
    }

    async fn connect_with_auth_recovery(
        &self,
        session_id: &str,
        thread_id: &str,
        turn_state: Option<&str>,
    ) -> Result<(ResponsesSocket, ConnectionMetadata), ResponsesError> {
        let auth = self.auth_snapshot().await?;
        match platform::connect_socket(
            &self.platform,
            &self.config,
            &auth,
            session_id,
            thread_id,
            turn_state,
        )
        .await
        {
            Err(ResponsesError::HandshakeRejected { status: 401, .. })
                if auth.mode() == OpenAiAuthMode::ChatGpt =>
            {
                self.config
                    .auth
                    .recover_unauthorized(&auth)
                    .await
                    .map_err(|error| ResponsesError::Authorization {
                        detail: error.to_string(),
                    })?;
                let refreshed = self.auth_snapshot().await?;
                platform::connect_socket(
                    &self.platform,
                    &self.config,
                    &refreshed,
                    session_id,
                    thread_id,
                    turn_state,
                )
                .await
            }
            result => result,
        }
    }

    async fn auth_snapshot(&self) -> Result<OpenAiAuthSnapshot, ResponsesError> {
        self.config
            .auth
            .snapshot()
            .await
            .map_err(|error| ResponsesError::Authorization {
                detail: error.to_string(),
            })
    }
}

fn record_pipeline_stats(
    span: &tracing::Span,
    request_bytes: usize,
    encode_duration_ns: u64,
    send_duration_ns: u64,
    stats: stream::ResponsePipelineStats,
) {
    span.record("response.event.count", stats.event_count);
    span.record("response.bytes", stats.event_bytes);
    span.record(
        "response.receive.wait_duration_ns",
        stats.receive_wait_duration_ns,
    );
    span.record("response.parse.duration_ns", stats.parse_duration_ns);
    span.record("response.emit.duration_ns", stats.emit_duration_ns);
    span.record("response.decode.duration_ns", stats.decode_duration_ns);
    span.record(
        "response.socket_queue.duration_ns",
        stats.socket_queue_duration_ns,
    );
    span.record("response.display_delta.count", stats.display_delta_count);
    span.record("response.display_delta.bytes", stats.display_delta_bytes);
    span.record(
        "response.inter_delta_gap.max_ns",
        stats.inter_delta_gap_max_ns,
    );
    span.record(
        "response.inter_delta_stall_50ms.count",
        stats.inter_delta_stall_50ms_count,
    );
    span.record(
        "response.inter_delta_stall_100ms.count",
        stats.inter_delta_stall_100ms_count,
    );
    span.record(
        "response.inter_delta_stall_250ms.count",
        stats.inter_delta_stall_250ms_count,
    );
    tracing::info!(
        target: "nanocodex_oai_api",
        stage = "responses.pipeline.completed",
        request.bytes = request_bytes,
        request.encode.duration_ns = encode_duration_ns,
        request.send.duration_ns = send_duration_ns,
        response.event.count = stats.event_count,
        response.bytes = stats.event_bytes,
        response.receive.wait_duration_ns = stats.receive_wait_duration_ns,
        response.parse.duration_ns = stats.parse_duration_ns,
        response.emit.duration_ns = stats.emit_duration_ns,
        response.decode.duration_ns = stats.decode_duration_ns,
        response.socket_queue.duration_ns = stats.socket_queue_duration_ns,
        response.display_delta.count = stats.display_delta_count,
        response.display_delta.bytes = stats.display_delta_bytes,
        response.inter_delta_gap.duration_ns = stats.inter_delta_gap_duration_ns,
        response.inter_delta_gap.max_ns = stats.inter_delta_gap_max_ns,
        response.inter_delta_stall_50ms.count = stats.inter_delta_stall_50ms_count,
        response.inter_delta_stall_100ms.count = stats.inter_delta_stall_100ms_count,
        response.inter_delta_stall_250ms.count = stats.inter_delta_stall_250ms_count,
        "Responses attempt pipeline timing"
    );
}

impl Service<ResponsesAttempt> for ResponsesService {
    type Response = ResponsesServiceResponse;
    type Error = ResponsesServiceError;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: ResponsesAttempt) -> Self::Future {
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
                phase = request.kind.phase(),
                model.call_index = request.call_index,
                attempt = request.attempt,
                max_attempts = request.max_attempts,
                transport = tracing::field::Empty,
                replay.mode = tracing::field::Empty,
                model.input.item_count = tracing::field::Empty,
                request.queue.duration_ns = tracing::field::Empty,
                request.bytes = tracing::field::Empty,
                request.encode.duration_ns = tracing::field::Empty,
                request.send.duration_ns = tracing::field::Empty,
                response.event.count = tracing::field::Empty,
                response.bytes = tracing::field::Empty,
                response.receive.wait_duration_ns = tracing::field::Empty,
                response.parse.duration_ns = tracing::field::Empty,
                response.emit.duration_ns = tracing::field::Empty,
                response.decode.duration_ns = tracing::field::Empty,
                response.socket_queue.duration_ns = tracing::field::Empty,
                response.display_delta.count = tracing::field::Empty,
                response.display_delta.bytes = tracing::field::Empty,
                response.inter_delta_gap.max_ns = tracing::field::Empty,
                response.inter_delta_stall_50ms.count = tracing::field::Empty,
                response.inter_delta_stall_100ms.count = tracing::field::Empty,
                response.inter_delta_stall_250ms.count = tracing::field::Empty,
                status = tracing::field::Empty,
                duration_ns = tracing::field::Empty,
            );
            async move {
                let queued_at = Instant::now();
                let mut connection = service.connection.lock().await;
                tracing::Span::current().record("request.queue.duration_ns", elapsed_ns(queued_at));
                connection.enter_logical_turn(request.logical_turn);
                let transport = request.effective_transport(service.config.responses_transport);
                tracing::Span::current().record("transport", transport.as_str());
                if matches!(transport, ResponsesTransport::Https) && connection.socket.is_some() {
                    connection.capture_turn_state();
                    connection.socket = None;
                }
                let replaces_websocket = matches!(transport, ResponsesTransport::WebSocket)
                    && connection.socket.is_none()
                    && connection.generation > 0;
                if matches!(
                    service.config.responses_history,
                    ResponsesHistory::FullReplay
                ) || (matches!(transport, ResponsesTransport::Https)
                    && !service.config.store_responses)
                    || replaces_websocket
                {
                    request.force_full_replay();
                }
                tracing::Span::current().record("replay.mode", request.replay_mode());
                tracing::Span::current()
                    .record("model.input.item_count", request.input_item_count());
                service.run(&mut connection, &request, transport).await
            }
            .instrument(span)
            .await
        })
    }
}

async fn receive_warmup(
    socket: &mut ResponsesSocket,
    request: &ResponsesAttempt,
) -> Result<WarmupResponse, ResponsesServiceError> {
    loop {
        let received = socket.next_text_or_idle_timeout().await?;
        let raw_event = parse_raw_json(received.text.as_str())?;
        tracing::trace!(
            target: "nanocodex_oai_api",
            direction = "inbound",
            transport = TRANSPORT,
            phase = "warmup",
            api.event = %raw_event.get(),
            "OpenAI Responses API event"
        );
        request.observer.events.emit_with_source_sequence(
            AgentEventKind::ApiEvent,
            ApiEvent {
                direction: "inbound",
                transport: TRANSPORT,
                phase: "warmup",
                model_call_index: None,
                event: raw_event,
            },
            Some(received.received_ns),
        )?;
        match decode_event::<WarmupServerEvent>(raw_event)? {
            WarmupServerEvent::Completed { response } => return Ok(response),
            WarmupServerEvent::Error
            | WarmupServerEvent::Failed
            | WarmupServerEvent::Incomplete => {
                return Err(ResponsesError::api_event(raw_event.get().to_owned()).into());
            }
            WarmupServerEvent::Created { response } => drop(response.id),
            WarmupServerEvent::Other => {}
        }
    }
}

fn required_call_index(request: &ResponsesAttempt) -> Result<u32, ResponsesServiceError> {
    request.call_index.ok_or_else(|| {
        ResponsesServiceError::invalid_attempt_state(
            "generation attempt did not have a model call index",
            FailurePhase::Completion,
            0,
        )
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use web_time::Instant;

    use super::{AttemptGuard, ConnectionPurpose, ConnectionState};

    #[test]
    fn logical_turn_boundaries_reset_but_same_turn_calls_reuse_turn_state() {
        let mut connection = ConnectionState::new();
        connection.turn_state = Some("stale-parent-state".to_owned());

        connection.enter_logical_turn(41);
        assert_eq!(connection.turn_state, None);

        connection.turn_state = Some("turn-41-state".to_owned());
        connection.enter_logical_turn(41);
        assert_eq!(connection.turn_state.as_deref(), Some("turn-41-state"));

        connection.enter_logical_turn(42);
        assert_eq!(connection.turn_state, None);
    }

    #[test]
    fn cancelled_websocket_attempt_discards_the_socket_but_keeps_turn_state() {
        let mut connection = ConnectionState::new();
        connection.turn_state = Some("partial-turn-state".to_owned());
        connection.next_purpose = ConnectionPurpose::Initial;

        let (events, mut receiver) = crate::EventSink::channel("cancelled-attempt".to_owned());
        let stats = Arc::new(crate::TransportStats::default());
        let factory = crate::ResponsesAttemptFactory::new(
            crate::responses::RequestProfile::new(
                "cancelled-attempt",
                "cancelled-attempt",
                Arc::from([]),
            ),
            events,
            Arc::clone(&stats),
        );
        let request = factory.warmup(crate::Model::Sol, crate::Thinking::High, false);

        drop(AttemptGuard::new(
            &mut connection,
            &request,
            crate::ResponsesTransport::WebSocket,
            Instant::now(),
        ));

        assert!(connection.socket.is_none());
        assert_eq!(connection.turn_state.as_deref(), Some("partial-turn-state"));
        assert!(matches!(
            connection.next_purpose,
            ConnectionPurpose::Reconnect
        ));
        let event = receiver
            .try_recv_timed()
            .expect("cancellation must remain observable")
            .event;
        assert_eq!(event.kind, crate::AgentEventKind::ModelAttemptFailed);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(event.payload.get()).unwrap()["error_class"],
            "cancelled"
        );
    }

    #[test]
    fn cancellation_is_observable_for_each_transport() {
        for transport in [
            crate::ResponsesTransport::WebSocket,
            crate::ResponsesTransport::Https,
        ] {
            let mut connection = ConnectionState::new();
            let (events, mut receiver) =
                crate::EventSink::channel(format!("sent-{}-attempt", transport.as_str()));
            let stats = Arc::new(crate::TransportStats::default());
            let factory = crate::ResponsesAttemptFactory::new(
                crate::responses::RequestProfile::new(
                    "sent-attempt",
                    "sent-attempt",
                    Arc::from([]),
                ),
                events,
                Arc::clone(&stats),
            );
            let request = factory.warmup(crate::Model::Sol, crate::Thinking::High, false);

            drop(AttemptGuard::new(
                &mut connection,
                &request,
                transport,
                Instant::now(),
            ));

            let event = receiver
                .try_recv_timed()
                .expect("cancellation must remain observable")
                .event;
            assert_eq!(event.kind, crate::AgentEventKind::ModelAttemptFailed);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(event.payload.get()).unwrap()["error_class"],
                "cancelled"
            );
        }
    }
}
