use std::collections::HashMap;

use nanocodex_core::{
    AgentEventKind, ContentItem, EventSink, MessagePhase, MessageRole, ResponseItem,
    responses::{ServerEvent, Usage},
};
use serde::Serialize;
use web_time::Instant;

use crate::{
    ResponsesError,
    service_error::ResponsesServiceError,
    socket::{ResponsesSocket, decode_event, parse_raw_json},
    telemetry::{ApiEvent, TRANSPORT, elapsed_ns},
};

const INVALID_IMAGE_ERROR: &str = "The image data you provided does not represent a valid image";

pub struct TurnResult {
    pub id: String,
    pub status: String,
    pub end_turn: Option<bool>,
    pub final_message: Option<String>,
    pub output_items: Vec<ResponseItem>,
    pub code_calls: Vec<CodeCall>,
    pub usage: Option<Usage>,
    pub time_to_first_event_ns: u64,
    pub time_to_first_output_ns: Option<u64>,
}

pub struct CompactionResult {
    pub id: String,
    pub status: String,
    pub item: ResponseItem,
    pub usage: Option<Usage>,
    pub time_to_first_event_ns: u64,
    pub time_to_first_output_ns: Option<u64>,
}

pub struct CodeCall {
    pub call_id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub input: String,
    pub kind: CodeCallKind,
}

pub enum CodeCallKind {
    Custom,
    Function,
}

#[derive(Serialize)]
struct AssistantTextDelta<'a> {
    model_call_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<MessagePhase>,
    text: &'a str,
}

struct AssistantStreamItem {
    item_id: Option<Box<str>>,
    phase: Option<MessagePhase>,
}

#[derive(Serialize)]
struct TextDelta<'a> {
    model_call_index: u32,
    text: &'a str,
}

struct StreamTiming {
    started_at: Instant,
    first_event_ns: Option<u64>,
    first_output_ns: Option<u64>,
}

impl StreamTiming {
    const fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            first_event_ns: None,
            first_output_ns: None,
        }
    }
}

pub(crate) async fn receive(
    socket: &mut ResponsesSocket,
    events: &EventSink,
    call_index: u32,
    started_at: Instant,
) -> Result<TurnResult, ResponsesServiceError> {
    let mut done_items = Vec::with_capacity(2);
    let mut assistant_items = HashMap::new();
    let mut timing = StreamTiming::new(started_at);

    loop {
        match next_event(socket, events, "generation", call_index, &mut timing).await? {
            ServerEvent::OutputItemAdded { output_index, item } => {
                let Some(output_index) = output_index else {
                    continue;
                };
                let ResponseItem::Message {
                    id,
                    role: MessageRole::Assistant,
                    phase,
                    ..
                } = item
                else {
                    continue;
                };
                assistant_items.insert(output_index, AssistantStreamItem { item_id: id, phase });
            }
            ServerEvent::OutputTextDelta {
                output_index,
                delta,
            } => {
                let payload_bytes = delta.len();
                let item = output_index.and_then(|index| assistant_items.get(&index));
                let seq = events.emit_with_sequence(
                    AgentEventKind::AssistantDelta,
                    AssistantTextDelta {
                        model_call_index: call_index,
                        item_id: item.and_then(|item| item.item_id.as_deref()),
                        phase: item.and_then(|item| item.phase),
                        text: &delta,
                    },
                )?;
                tracing::trace!(
                    target: "nanocodex_stream_timing",
                    stage = "api_delta_emitted",
                    request.id = events.request_id(),
                    event.seq = seq,
                    event.kind = "assistant.delta",
                    model.call.index = call_index,
                    payload.bytes = payload_bytes,
                    "Responses text delta entered the agent event stream"
                );
            }
            ServerEvent::ReasoningSummaryTextDelta { delta }
            | ServerEvent::ReasoningSummaryDelta { delta } => {
                let payload_bytes = delta.len();
                let seq = events.emit_with_sequence(
                    AgentEventKind::ReasoningSummaryDelta,
                    TextDelta {
                        model_call_index: call_index,
                        text: &delta,
                    },
                )?;
                tracing::trace!(
                    target: "nanocodex_stream_timing",
                    stage = "api_delta_emitted",
                    request.id = events.request_id(),
                    event.seq = seq,
                    event.kind = "reasoning.summary.delta",
                    model.call.index = call_index,
                    payload.bytes = payload_bytes,
                    "Responses reasoning delta entered the agent event stream"
                );
            }
            ServerEvent::OutputItemDone { item } => done_items.push(item),
            ServerEvent::Completed { mut response } => {
                let output_items = if response.output.is_empty() {
                    done_items
                } else {
                    std::mem::take(&mut response.output)
                };
                let code_calls = code_calls(&output_items);
                let final_message = final_message(&output_items);
                return Ok(TurnResult {
                    id: response.id,
                    status: response.status,
                    end_turn: response.end_turn,
                    final_message,
                    output_items,
                    code_calls,
                    usage: response.usage,
                    time_to_first_event_ns: timing.first_event_ns.unwrap_or_default(),
                    time_to_first_output_ns: timing.first_output_ns,
                });
            }
            _ => {}
        }
    }
}

pub(crate) async fn receive_compaction(
    socket: &mut ResponsesSocket,
    events: &EventSink,
    call_index: u32,
    started_at: Instant,
) -> Result<CompactionResult, ResponsesServiceError> {
    let mut done_items = Vec::with_capacity(2);
    let mut timing = StreamTiming::new(started_at);

    loop {
        match next_event(socket, events, "compaction", call_index, &mut timing).await? {
            ServerEvent::OutputItemDone { item } => done_items.push(item),
            ServerEvent::Completed { mut response } => {
                let output_items = if response.output.is_empty() {
                    done_items
                } else {
                    std::mem::take(&mut response.output)
                };
                let mut compactions = output_items
                    .into_iter()
                    .filter(|item| matches!(item, ResponseItem::Compaction { .. }));
                let item = compactions.next();
                let count = usize::from(item.is_some()) + compactions.count();
                if count != 1 {
                    return Err(ResponsesServiceError::invalid_compaction(count));
                }
                let Some(item) = item else {
                    return Err(ResponsesServiceError::invalid_compaction(0));
                };
                return Ok(CompactionResult {
                    id: response.id,
                    status: response.status,
                    item,
                    usage: response.usage,
                    time_to_first_event_ns: timing.first_event_ns.unwrap_or_default(),
                    time_to_first_output_ns: timing.first_output_ns,
                });
            }
            _ => {}
        }
    }
}

async fn next_event(
    socket: &mut ResponsesSocket,
    events: &EventSink,
    phase: &'static str,
    call_index: u32,
    timing: &mut StreamTiming,
) -> Result<ServerEvent, ResponsesServiceError> {
    let text = socket.next_text_or_idle_timeout().await?;
    let raw_event = parse_raw_json(text.as_str())?;
    let elapsed = elapsed_ns(timing.started_at);
    timing.first_event_ns.get_or_insert(elapsed);
    events.emit(
        AgentEventKind::ApiEvent,
        ApiEvent {
            direction: "inbound",
            transport: TRANSPORT,
            phase,
            model_call_index: Some(call_index),
            event: raw_event,
        },
    )?;
    let event = decode_event::<ServerEvent>(raw_event)?;
    if matches!(
        event,
        ServerEvent::OutputTextDelta { .. }
            | ServerEvent::ReasoningSummaryTextDelta { .. }
            | ServerEvent::ReasoningSummaryDelta { .. }
            | ServerEvent::OutputItemAdded { .. }
            | ServerEvent::OutputItemDone { .. }
    ) {
        timing.first_output_ns.get_or_insert(elapsed);
    }
    if matches!(
        event,
        ServerEvent::Error | ServerEvent::Failed | ServerEvent::Incomplete
    ) {
        if raw_event.get().contains(INVALID_IMAGE_ERROR) {
            return Err(ResponsesError::InvalidImageRequest {
                event: raw_event.get().to_owned(),
            }
            .into());
        }
        return Err(ResponsesError::Api {
            event: raw_event.get().to_owned(),
        }
        .into());
    }
    Ok(event)
}

fn code_calls(items: &[ResponseItem]) -> Vec<CodeCall> {
    let mut calls = Vec::with_capacity(items.len().min(4));
    for item in items {
        match item {
            ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                input,
                ..
            } => {
                calls.push(CodeCall {
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    namespace: namespace.as_deref().map(str::to_owned),
                    input: input.to_string(),
                    kind: CodeCallKind::Custom,
                });
            }
            ResponseItem::FunctionCall {
                call_id,
                name,
                namespace,
                arguments,
                ..
            } => {
                calls.push(CodeCall {
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    namespace: namespace.as_deref().map(str::to_owned),
                    input: arguments.to_string(),
                    kind: CodeCallKind::Function,
                });
            }
            _ => {}
        }
    }
    calls
}

fn final_message(items: &[ResponseItem]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        let ResponseItem::Message { content, .. } = item else {
            return None;
        };
        Some(
            content
                .iter()
                .filter_map(|part| match part {
                    ContentItem::OutputText { text, .. } => Some(text.as_ref()),
                    ContentItem::InputText { .. }
                    | ContentItem::InputImage { .. }
                    | ContentItem::InputAudio { .. } => None,
                })
                .collect(),
        )
    })
}
