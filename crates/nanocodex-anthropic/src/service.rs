use std::{
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use nanocodex_oai_api::{
    ResponseEvent,
    responses::{ContentItem, MessageRole, ResponseItem, ServerEvent, ToolDefinition},
    tower::{
        CodeCall, CodeCallKind, GenerationOutput, ResponsePipelineStats, ResponsesAttempt,
        ResponsesAttemptKind, ResponsesOutput, ResponsesRetryPolicy, ResponsesServiceError,
        ResponsesServiceResponse,
    },
    transport::ResponsesError,
};
use tower::{Service, retry::Retry};
use tracing::{Instrument, info_span};
use web_time::Instant;

use super::{
    AnthropicAuth, AnthropicAuthMode,
    translate::{StreamTranslator, build_request},
    transport::AnthropicHttp,
    wire::SystemBlock,
};

const TRANSPORT: &str = "anthropic_messages_sse";
const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

type ServiceFuture = Pin<
    Box<
        dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>>
            + Send
            + 'static,
    >,
>;

/// In-process OpenAI Responses-compatible wrapper around Anthropic Messages.
#[derive(Clone)]
pub struct AnthropicService {
    auth: AnthropicAuth,
    model: Arc<str>,
    api_base_url: Arc<str>,
    max_tokens: u32,
    http: AnthropicHttp,
}

impl AnthropicService {
    pub(crate) fn new(
        auth: AnthropicAuth,
        model: Arc<str>,
        api_base_url: String,
        max_tokens: u32,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            auth,
            model,
            api_base_url: api_base_url.into(),
            max_tokens,
            http: AnthropicHttp::new(http_client),
        }
    }
}

/// Concrete retrying service installed into [`nanocodex_oai_api::OpenAi`].
pub type AnthropicResponsesService = Retry<ResponsesRetryPolicy, AnthropicService>;

impl Service<ResponsesAttempt> for AnthropicService {
    type Response = ResponsesServiceResponse;
    type Error = ResponsesServiceError;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let service = self.clone();
        let parent = tracing::Span::current();
        Box::pin(async move {
            let span = info_span!(
                target: "nanocodex_anthropic",
                parent: &parent,
                "responses.attempt",
                otel.kind = "client",
                phase = ?request.kind(),
                model.call_index = request.model_call_index(),
                attempt = request.attempt(),
                transport = TRANSPORT,
                model.input.item_count = request.input_item_count(),
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
        if !matches!(request.kind(), ResponsesAttemptKind::Generation) {
            return Err(ResponsesError::Api {
                event: format!(
                    "Anthropic adapter does not support {:?} Responses operations",
                    request.kind()
                ),
            }
            .into());
        }

        let mut snapshot = self.auth.snapshot().await.map_err(auth_error)?;
        let body = self.encode(&request, snapshot.mode())?;
        trace_outbound(&body, request.model_call_index(), false);
        let first = self
            .http
            .send(&self.api_base_url, &snapshot, body.clone())
            .await;
        let mut stream = match first {
            Err(error @ ResponsesError::HttpRejected { status: 401, .. })
                if snapshot.mode() == AnthropicAuthMode::OAuth =>
            {
                tracing::trace!(
                    target: "nanocodex_anthropic",
                    direction = "inbound",
                    transport = TRANSPORT,
                    api.error = %error,
                    "Anthropic Messages API rejected authorization"
                );
                self.auth
                    .recover_unauthorized(&snapshot)
                    .await
                    .map_err(auth_error)?;
                snapshot = self.auth.snapshot().await.map_err(auth_error)?;
                trace_outbound(&body, request.model_call_index(), true);
                self.http.send(&self.api_base_url, &snapshot, body).await
            }
            result => result,
        }?;

        let started_at = Instant::now();
        let mut first_event_ns = None;
        let mut first_output_ns = None;
        let mut pipeline = ResponsePipelineStats::default();
        let mut done_items = Vec::new();
        let mut translator = StreamTranslator::with_custom_tools(custom_tool_names(&request));

        loop {
            let (raw, event) = stream.next().await?;
            let elapsed_ns = elapsed_ns(started_at);
            first_event_ns.get_or_insert(elapsed_ns);
            pipeline.event_count = pipeline.event_count.saturating_add(1);
            pipeline.event_bytes = pipeline
                .event_bytes
                .saturating_add(u64::try_from(raw.len()).unwrap_or(u64::MAX));
            tracing::trace!(
                target: "nanocodex_anthropic",
                direction = "inbound",
                transport = TRANSPORT,
                model.call_index = request.model_call_index(),
                api.event = raw,
                "Anthropic Messages API event"
            );

            for event in translator.push(event) {
                if is_output_event(&event) {
                    first_output_ns.get_or_insert(elapsed_ns);
                }
                if let ServerEvent::Completed { response } = &event
                    && response.status != "completed"
                {
                    return Err(ResponsesError::Api {
                        event: format!("Anthropic response ended with status {}", response.status),
                    }
                    .into());
                }
                emit_normalized(&request, &event).await?;
                match event {
                    ServerEvent::OutputItemDone { item } => done_items.push(item),
                    ServerEvent::Completed { mut response } => {
                        let output_items = if response.output.is_empty() {
                            done_items
                        } else {
                            std::mem::take(&mut response.output)
                        };
                        let output = GenerationOutput {
                            id: response.id,
                            status: response.status,
                            end_turn: response.end_turn,
                            final_message: final_message(&output_items),
                            code_calls: code_calls(&output_items),
                            output_items,
                            usage: response.usage,
                            time_to_first_event_ns: first_event_ns.unwrap_or_default(),
                            time_to_first_output_ns: first_output_ns,
                            pipeline_stats: pipeline,
                        };
                        return Ok(ResponsesServiceResponse::new(ResponsesOutput::Generation(
                            output,
                        ))
                        .with_attempt(request.attempt())
                        .with_server_reasoning_included(true));
                    }
                    ServerEvent::Error | ServerEvent::Failed | ServerEvent::Incomplete => {
                        return Err(ResponsesError::Api { event: raw }.into());
                    }
                    _ => {}
                }
            }
        }
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
            SYSTEM_PROMPT,
            request.full_replay_input_items(),
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
            .map_err(Into::into)
    }
}

fn auth_error(error: super::AnthropicAuthError) -> ResponsesServiceError {
    ResponsesError::Authorization {
        detail: error.to_string(),
    }
    .into()
}

fn trace_outbound(body: &str, call_index: Option<u32>, replay: bool) {
    tracing::trace!(
        target: "nanocodex_anthropic",
        direction = "outbound",
        transport = TRANSPORT,
        model.call_index = call_index,
        auth.replay = replay,
        api.request = body,
        "Anthropic Messages API request"
    );
}

async fn emit_normalized(
    request: &ResponsesAttempt,
    event: &ServerEvent,
) -> Result<(), ResponsesServiceError> {
    let normalized = match event {
        ServerEvent::Created { .. } => Some(ResponseEvent::Created),
        ServerEvent::OutputItemAdded { item, .. } => {
            Some(ResponseEvent::OutputItemAdded(item.clone()))
        }
        ServerEvent::OutputTextDelta { delta, .. } => {
            Some(ResponseEvent::OutputTextDelta(delta.clone()))
        }
        ServerEvent::ReasoningSummaryTextDelta {
            delta,
            summary_index,
        }
        | ServerEvent::ReasoningSummaryDelta {
            delta,
            summary_index: Some(summary_index),
        } => Some(ResponseEvent::ReasoningSummaryDelta {
            delta: delta.clone(),
            summary_index: *summary_index,
        }),
        ServerEvent::OutputItemDone { item } => Some(ResponseEvent::OutputItemDone(item.clone())),
        ServerEvent::Completed { response } => Some(ResponseEvent::Completed {
            usage: response.usage.clone(),
            end_turn: response.end_turn,
        }),
        _ => None,
    };
    if let Some(event) = normalized {
        request.emit(event).await?;
    }
    Ok(())
}

const fn is_output_event(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::OutputTextDelta { .. }
            | ServerEvent::ReasoningSummaryTextDelta { .. }
            | ServerEvent::ReasoningSummaryDelta { .. }
            | ServerEvent::OutputItemDone { .. }
    )
}

fn custom_tool_names(request: &ResponsesAttempt) -> Vec<String> {
    request
        .full_replay_input_items()
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

fn code_calls(items: &[ResponseItem]) -> Vec<CodeCall> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                input,
                ..
            } => Some(CodeCall {
                call_id: call_id.to_string(),
                name: name.to_string(),
                namespace: namespace.as_deref().map(str::to_owned),
                input: input.to_string(),
                kind: CodeCallKind::Custom,
            }),
            ResponseItem::FunctionCall {
                call_id,
                name,
                namespace,
                arguments,
                ..
            } => Some(CodeCall {
                call_id: call_id.to_string(),
                name: name.to_string(),
                namespace: namespace.as_deref().map(str::to_owned),
                input: arguments.to_string(),
                kind: CodeCallKind::Function,
            }),
            _ => None,
        })
        .collect()
}

fn final_message(items: &[ResponseItem]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        let ResponseItem::Message {
            role: MessageRole::Assistant,
            content,
            ..
        } = item
        else {
            return None;
        };
        Some(
            content
                .iter()
                .filter_map(|part| match part {
                    ContentItem::OutputText { text, .. } => Some(text.as_ref()),
                    _ => None,
                })
                .collect(),
        )
    })
}

fn elapsed_ns(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) const fn retrying(
    service: AnthropicService,
    max_attempts: NonZeroU32,
) -> AnthropicResponsesService {
    Retry::new(ResponsesRetryPolicy::new(max_attempts), service)
}
