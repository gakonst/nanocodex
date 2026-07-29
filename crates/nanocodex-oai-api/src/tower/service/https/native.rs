use web_time::Instant;

use crate::{
    AgentEventKind, EncodedRequest, OpenAiAuthMode, ResponsesError, ResponsesTransport,
    http::{HttpMetadata, ResponsesHttpStream},
    telemetry::{ApiEvent, elapsed_ns},
    tower::{
        ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesServiceError,
        ResponsesServiceResponse,
        service_error::FailurePhase,
        stream::{self, ResponseEventSource},
    },
};

use super::super::{ConnectionState, ResponsesService, record_pipeline_stats, required_call_index};

pub(crate) async fn run(
    service: &ResponsesService,
    connection: &mut ConnectionState,
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
    let encoded = service.encode_request(connection, request, ResponsesTransport::Https)?;
    let encode_duration_ns = elapsed_ns(encode_started_at);
    let request_bytes = encoded.raw().get().len();
    let transport = ResponsesTransport::Https.as_str();
    let span = tracing::Span::current();
    span.record("request.bytes", request_bytes);
    span.record("request.encode.duration_ns", encode_duration_ns);
    tracing::trace!(
        target: "nanocodex_oai_api",
        direction = "outbound",
        transport,
        phase = request.kind.phase(),
        model.call_index = request.call_index,
        api.request = %encoded.raw().get(),
        "OpenAI Responses API request"
    );
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
    let (mut response, metadata) = send_with_auth_recovery(
        service,
        request.profile.session_id(),
        connection.turn_state.as_deref(),
        &encoded,
    )
    .await
    .map_err(|error| ResponsesServiceError::responses(error, FailurePhase::Send, 0))?;
    connection.observe_turn_state(metadata.turn_state.as_deref());
    let send_duration_ns = elapsed_ns(send_started_at);
    span.record("request.send.duration_ns", send_duration_ns);
    let output = match request.kind {
        ResponsesAttemptKind::Generation => ResponsesOutput::Generation(
            stream::receive(
                &mut response,
                transport,
                &request.observer,
                required_call_index(request)?,
                started_at,
            )
            .await?,
        ),
        ResponsesAttemptKind::Compaction => ResponsesOutput::Compaction(
            stream::receive_compaction(
                &mut response,
                transport,
                &request.observer,
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

impl ResponseEventSource for ResponsesHttpStream {
    async fn next_text_or_idle_timeout(
        &mut self,
    ) -> Result<crate::socket::ReceivedText, ResponsesError> {
        Self::next_text_or_idle_timeout(self).await
    }
}

async fn send_with_auth_recovery(
    service: &ResponsesService,
    session_id: &str,
    turn_state: Option<&str>,
    request: &EncodedRequest,
) -> Result<(ResponsesHttpStream, HttpMetadata), ResponsesError> {
    let auth = service.auth_snapshot().await?;
    match service
        .platform
        .http()
        .send(
            &service.config.api_base_url,
            &auth,
            session_id,
            turn_state,
            request,
        )
        .await
    {
        Err(ResponsesError::HttpRejected { status: 401, .. })
            if auth.mode() == OpenAiAuthMode::ChatGpt =>
        {
            service
                .config
                .auth
                .recover_unauthorized(&auth)
                .await
                .map_err(|error| ResponsesError::Authorization {
                    detail: error.to_string(),
                })?;
            let refreshed = service.auth_snapshot().await?;
            service
                .platform
                .http()
                .send(
                    &service.config.api_base_url,
                    &refreshed,
                    session_id,
                    turn_state,
                    request,
                )
                .await
        }
        result => result,
    }
}
