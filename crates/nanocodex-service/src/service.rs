use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll},
};

#[cfg(not(target_family = "wasm"))]
use nanocodex_core::OpenAiAuthMode;
use nanocodex_core::{
    AgentEventKind, ModelConfig, OpenAiAuthSnapshot, ResponsesHistory, ResponsesTransport,
    responses::{ResponseCreate, WarmupResponse, WarmupServerEvent},
};
use tokio::sync::Mutex;
use tower::{Service, retry::Retry};
use tracing::{Instrument, info_span};
use web_time::Instant;

#[cfg(not(target_family = "wasm"))]
use crate::http::{HttpMetadata, ResponsesHttp, ResponsesHttpStream};
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
};

#[cfg(not(target_family = "wasm"))]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>> + Send>>;
#[cfg(target_family = "wasm")]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>>>>;

struct ConnectionState {
    socket: Option<ResponsesSocket>,
    turn_state: Option<String>,
    generation: u32,
    next_purpose: ConnectionPurpose,
    server_reasoning_included: bool,
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
        }
    }

    fn capture_turn_state(&mut self) {
        if let Some(turn_state) = self.socket.as_ref().and_then(ResponsesSocket::turn_state) {
            self.turn_state = Some(turn_state.to_owned());
        }
    }

    fn invalidate(&mut self, purpose: ConnectionPurpose) {
        self.capture_turn_state();
        self.socket = None;
        self.next_purpose = purpose;
    }
}

/// Stateful transport attempt service at the base of a Responses Tower stack.
#[derive(Clone)]
pub struct ResponsesService {
    config: Arc<ModelConfig>,
    connection: Arc<Mutex<ConnectionState>>,
    #[cfg(not(target_family = "wasm"))]
    http: ResponsesHttp,
}

impl ResponsesService {
    #[must_use]
    pub fn new(config: Arc<ModelConfig>) -> Self {
        Self {
            config,
            connection: Arc::new(Mutex::new(ConnectionState::new())),
            #[cfg(not(target_family = "wasm"))]
            http: ResponsesHttp::new(),
        }
    }

    /// Builds the configured standard Responses service with retry policy.
    #[must_use]
    pub fn standard(config: Arc<ModelConfig>) -> DefaultResponsesService {
        Retry::new(ResponsesRetryPolicy, Self::new(config))
    }

    async fn run(
        &self,
        connection: &mut ConnectionState,
        request: &ResponsesAttempt,
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
        let result = self.run_inner(connection, request, started_at).await;
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

    async fn run_inner(
        &self,
        connection: &mut ConnectionState,
        request: &ResponsesAttempt,
        started_at: Instant,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        match self.config.responses_transport {
            ResponsesTransport::WebSocket => {
                self.run_websocket(connection, request, started_at).await
            }
            ResponsesTransport::Https => {
                #[cfg(not(target_family = "wasm"))]
                {
                    self.run_https(request, started_at).await
                }
                #[cfg(target_family = "wasm")]
                {
                    Err(ResponsesServiceError::invalid_attempt_state(
                        "HTTPS Responses transport is unavailable in browser WASM",
                        FailurePhase::Connect,
                        0,
                    ))
                }
            }
        }
    }

    async fn run_websocket(
        &self,
        connection: &mut ConnectionState,
        request: &ResponsesAttempt,
        started_at: Instant,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        if connection.socket.is_none() {
            self.connect(connection, request).await?;
        }
        let generation = connection.generation;
        let encode_started_at = Instant::now();
        let encoded = self.encode_request(connection, request)?;
        let encode_duration_ns = elapsed_ns(encode_started_at);
        let request_bytes = encoded.raw().get().len();
        let span = tracing::Span::current();
        span.record("request.bytes", request_bytes);
        span.record("request.encode.duration_ns", encode_duration_ns);
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
                    &mut stream::ResponseEventSource::WebSocket(socket),
                    ResponsesTransport::WebSocket.as_str(),
                    &request.observer.events,
                    required_call_index(request)?,
                    started_at,
                )
                .await
                .map_err(|error| error.with_connection_generation(generation))?,
            ),
            ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(
                stream::receive_compaction(
                    &mut stream::ResponseEventSource::WebSocket(socket),
                    ResponsesTransport::WebSocket.as_str(),
                    &request.observer.events,
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

    #[cfg(not(target_family = "wasm"))]
    async fn run_https(
        &self,
        request: &ResponsesAttempt,
        started_at: Instant,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        if matches!(request.kind, ResponsesAttemptKind::Warmup) {
            return Err(ResponsesServiceError::invalid_attempt_state(
                "HTTPS Responses transport does not perform a warmup request",
                FailurePhase::Protocol,
                0,
            ));
        }
        let encode_started_at = Instant::now();
        let encoded = self.encode_request(&ConnectionState::new(), request)?;
        let encode_duration_ns = elapsed_ns(encode_started_at);
        let request_bytes = encoded.raw().get().len();
        let transport = ResponsesTransport::Https.as_str();
        let span = tracing::Span::current();
        span.record("request.bytes", request_bytes);
        span.record("request.encode.duration_ns", encode_duration_ns);
        request.observer.emit(
            AgentEventKind::ApiEvent,
            ApiEvent {
                direction: "outbound",
                transport,
                phase: request.kind.phase(),
                model_call_index: request.call_index,
                event: encoded.raw(),
            },
        )?;
        let send_started_at = Instant::now();
        let (mut response, metadata) = self
            .send_https_with_auth_recovery(request.profile.session_id(), &encoded)
            .await
            .map_err(|error| ResponsesServiceError::responses(error, FailurePhase::Send, 0))?;
        let send_duration_ns = elapsed_ns(send_started_at);
        span.record("request.send.duration_ns", send_duration_ns);
        let mut source = stream::ResponseEventSource::Https(&mut response);
        let output = match request.kind {
            ResponsesAttemptKind::Generation => ResponsesOutput::Generation(
                stream::receive(
                    &mut source,
                    transport,
                    &request.observer.events,
                    required_call_index(request)?,
                    started_at,
                )
                .await?,
            ),
            ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(
                stream::receive_compaction(
                    &mut source,
                    transport,
                    &request.observer.events,
                    required_call_index(request)?,
                    started_at,
                )
                .await?,
            ),
            ResponsesAttemptKind::Warmup => unreachable!("warmup rejected above"),
        };
        let pipeline_stats = match &output {
            ResponsesOutput::Generation(result) => result.pipeline_stats,
            ResponsesOutput::Compaction(result) => result.pipeline_stats,
            ResponsesOutput::Warmup(_) => unreachable!("warmup rejected above"),
        };
        record_pipeline_stats(
            &span,
            request_bytes,
            encode_duration_ns,
            send_duration_ns,
            pipeline_stats,
        );
        Ok(ResponsesServiceResponse {
            output,
            attempt: request.attempt,
            connection_generation: 0,
            server_reasoning_included: metadata.reasoning_included,
        })
    }

    fn encode_request(
        &self,
        connection: &ConnectionState,
        request: &ResponsesAttempt,
    ) -> Result<EncodedRequest, ResponsesServiceError> {
        let encoded = match request.kind {
            ResponsesAttemptKind::Warmup => EncodedRequest::new(&ResponseCreate::warmup(
                &self.config,
                request.thinking(),
                request.fast_mode(),
                &request.profile,
                connection.turn_state.as_deref(),
            )),
            ResponsesAttemptKind::Generation | ResponsesAttemptKind::Compaction => {
                EncodedRequest::new(&ResponseCreate::generation(
                    &self.config,
                    request.thinking(),
                    request.fast_mode(),
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
            .establish_connection(request, purpose, generation)
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
            target: "nanocodex_service",
            "responses.connect",
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            purpose = ?purpose,
            connection.generation = generation,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let result = self
            .connect_with_auth_recovery(request.profile.session_id())
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

    #[cfg(not(target_family = "wasm"))]
    async fn connect_with_auth_recovery(
        &self,
        session_id: &str,
    ) -> Result<(ResponsesSocket, ConnectionMetadata), ResponsesError> {
        let auth = self.auth_snapshot().await?;
        match ResponsesSocket::connect(&self.config.websocket_url, &auth, session_id).await {
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
                ResponsesSocket::connect(&self.config.websocket_url, &refreshed, session_id).await
            }
            result => result,
        }
    }

    #[cfg(target_family = "wasm")]
    async fn connect_with_auth_recovery(
        &self,
        session_id: &str,
    ) -> Result<(ResponsesSocket, ConnectionMetadata), ResponsesError> {
        let auth = self.auth_snapshot().await?;
        ResponsesSocket::connect(&self.config.websocket_url, &auth, session_id).await
    }

    #[cfg(not(target_family = "wasm"))]
    async fn send_https_with_auth_recovery(
        &self,
        session_id: &str,
        request: &EncodedRequest,
    ) -> Result<(ResponsesHttpStream, HttpMetadata), ResponsesError> {
        let auth = self.auth_snapshot().await?;
        match self
            .http
            .send(&self.config.api_base_url, &auth, session_id, request)
            .await
        {
            Err(ResponsesError::HttpRejected { status: 401, .. })
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
                self.http
                    .send(&self.config.api_base_url, &refreshed, session_id, request)
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
        target: "nanocodex_service",
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
        let service = self.clone();
        Box::pin(async move {
            let mut connection = service.connection.lock().await;
            if matches!(
                service.config.responses_history,
                ResponsesHistory::FullReplay
            ) || (matches!(
                service.config.responses_transport,
                ResponsesTransport::Https
            ) && !service.config.store_responses)
                || (matches!(
                    service.config.responses_transport,
                    ResponsesTransport::WebSocket
                ) && !service.config.store_responses
                    && connection.socket.is_none()
                    && request.previous_response_id().is_some())
            {
                request.force_full_replay();
            }
            let input_item_count = request.input_item_count();
            let span = info_span!(
                target: "nanocodex_service",
                "responses.attempt",
                otel.kind = "client",
                otel.status_code = tracing::field::Empty,
                phase = request.kind.phase(),
                model.call_index = request.call_index,
                attempt = request.attempt,
                max_attempts = request.max_attempts,
                replay.mode = request.replay_mode(),
                model.input.item_count = input_item_count,
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
            service
                .run(&mut connection, &request)
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
                return Err(ResponsesError::Api {
                    event: raw_event.get().to_owned(),
                }
                .into());
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
