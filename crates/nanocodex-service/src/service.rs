use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context, Poll},
};

use nanocodex_core::{
    AgentEventKind, ModelConfig,
    responses::{ResponseCreate, WarmupResponse, WarmupServerEvent},
};
use tokio::sync::Mutex;
use tower::{Service, retry::Retry};
use tracing::{Instrument, info_span};
use web_time::Instant;

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

/// Stateful WebSocket attempt service at the base of a Responses Tower stack.
#[derive(Clone)]
pub struct ResponsesService {
    config: Arc<ModelConfig>,
    connection: Arc<Mutex<ConnectionState>>,
}

impl ResponsesService {
    #[must_use]
    pub fn new(config: Arc<ModelConfig>) -> Self {
        Self {
            config,
            connection: Arc::new(Mutex::new(ConnectionState::new())),
        }
    }

    /// Builds the standard persistent-WebSocket service with retry policy.
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
                    retryable: failure.retry_advice.is_some(),
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
        if connection.socket.is_none() {
            self.connect(connection, request).await?;
        }
        let generation = connection.generation;
        let encoded = self.encode_request(connection, request)?;
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
        socket.send(encoded).await.map_err(|error| {
            ResponsesServiceError::responses(error, FailurePhase::Send, generation)
        })?;
        let output = match request.kind {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(
                receive_warmup(socket, request)
                    .await
                    .map_err(|error| error.with_connection_generation(generation))?,
            ),
            ResponsesAttemptKind::Generation => ResponsesOutput::Generation(
                stream::receive(
                    socket,
                    &request.observer.events,
                    required_call_index(request)?,
                    started_at,
                )
                .await
                .map_err(|error| error.with_connection_generation(generation))?,
            ),
            ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(
                stream::receive_compaction(
                    socket,
                    &request.observer.events,
                    required_call_index(request)?,
                    started_at,
                )
                .await
                .map_err(|error| error.with_connection_generation(generation))?,
            ),
        };
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
    ) -> Result<EncodedRequest, ResponsesServiceError> {
        let encoded = match request.kind {
            ResponsesAttemptKind::Warmup => EncodedRequest::new(&ResponseCreate::warmup(
                &self.config,
                &request.profile,
                connection.turn_state.as_deref(),
            )),
            ResponsesAttemptKind::Generation | ResponsesAttemptKind::Compaction => {
                EncodedRequest::new(&ResponseCreate::generation(
                    &self.config,
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
        let result = ResponsesSocket::connect(
            &self.config.websocket_url,
            &self.config.api_key,
            request.profile.prompt_cache_key(),
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
}

impl Service<ResponsesAttempt> for ResponsesService {
    type Response = ResponsesServiceResponse;
    type Error = ResponsesServiceError;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let service = self.clone();
        let input_item_count = request.input_item_count();
        let input_bytes = serde_json::to_vec(&request.input()).map_or(0, |encoded| encoded.len());
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
            model.input.bytes = input_bytes,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        Box::pin(
            async move {
                let mut connection = service.connection.lock().await;
                service.run(&mut connection, &request).await
            }
            .instrument(span),
        )
    }
}

async fn receive_warmup(
    socket: &mut ResponsesSocket,
    request: &ResponsesAttempt,
) -> Result<WarmupResponse, ResponsesServiceError> {
    loop {
        let text = socket.next_text_or_idle_timeout().await?;
        let raw_event = parse_raw_json(text.as_str())?;
        request.observer.emit(
            AgentEventKind::ApiEvent,
            ApiEvent {
                direction: "inbound",
                transport: TRANSPORT,
                phase: "warmup",
                model_call_index: None,
                event: raw_event,
            },
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
