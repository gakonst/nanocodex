use std::collections::HashMap;

use nanocodex_core::{
    AgentEventKind, ContentItem, EventSink, MessagePhase, MessageRole, ResponseItem,
    ResponseItemId, monotonic_now_ns,
    responses::{ServerEvent, Usage},
};
use serde::Serialize;
use web_time::Instant;

#[cfg(not(target_family = "wasm"))]
use crate::http::ResponsesHttpStream;
use crate::{
    ResponsesError,
    service_error::ResponsesServiceError,
    socket::{ResponsesSocket, decode_event, parse_raw_json},
    telemetry::{ApiEvent, elapsed_ns},
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
    pub pipeline_stats: ResponsePipelineStats,
}

pub struct CompactionResult {
    pub id: String,
    pub status: String,
    pub item: ResponseItem,
    pub usage: Option<Usage>,
    pub time_to_first_event_ns: u64,
    pub time_to_first_output_ns: Option<u64>,
    pub pipeline_stats: ResponsePipelineStats,
}

#[derive(Clone, Copy, Default)]
pub struct ResponsePipelineStats {
    pub event_count: u64,
    pub event_bytes: u64,
    pub receive_wait_duration_ns: u64,
    pub parse_duration_ns: u64,
    pub emit_duration_ns: u64,
    pub decode_duration_ns: u64,
    pub socket_queue_duration_ns: u64,
    pub display_delta_count: u64,
    pub display_delta_bytes: u64,
    pub inter_delta_gap_duration_ns: u64,
    pub inter_delta_gap_max_ns: u64,
    pub inter_delta_stall_50ms_count: u64,
    pub inter_delta_stall_100ms_count: u64,
    pub inter_delta_stall_250ms_count: u64,
}

pub struct CodeCall {
    pub call_id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub input: String,
    pub kind: CodeCallKind,
}

#[derive(Clone, Copy)]
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

#[derive(Serialize)]
struct AssistantMessage<'a> {
    model_call_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<MessagePhase>,
    text: String,
}

struct AssistantStreamItem {
    item_id: Option<ResponseItemId>,
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
    pipeline: ResponsePipelineStats,
    last_display_delta_received_ns: Option<u64>,
}

impl StreamTiming {
    const fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            first_event_ns: None,
            first_output_ns: None,
            pipeline: ResponsePipelineStats {
                event_count: 0,
                event_bytes: 0,
                receive_wait_duration_ns: 0,
                parse_duration_ns: 0,
                emit_duration_ns: 0,
                decode_duration_ns: 0,
                socket_queue_duration_ns: 0,
                display_delta_count: 0,
                display_delta_bytes: 0,
                inter_delta_gap_duration_ns: 0,
                inter_delta_gap_max_ns: 0,
                inter_delta_stall_50ms_count: 0,
                inter_delta_stall_100ms_count: 0,
                inter_delta_stall_250ms_count: 0,
            },
            last_display_delta_received_ns: None,
        }
    }

    fn record_display_delta(&mut self, received_ns: u64, bytes: usize) {
        self.pipeline.display_delta_count = self.pipeline.display_delta_count.saturating_add(1);
        self.pipeline.display_delta_bytes = self
            .pipeline
            .display_delta_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if let Some(previous_ns) = self.last_display_delta_received_ns {
            let gap_ns = received_ns.saturating_sub(previous_ns);
            self.pipeline.inter_delta_gap_duration_ns = self
                .pipeline
                .inter_delta_gap_duration_ns
                .saturating_add(gap_ns);
            self.pipeline.inter_delta_gap_max_ns = self.pipeline.inter_delta_gap_max_ns.max(gap_ns);
            self.pipeline.inter_delta_stall_50ms_count = self
                .pipeline
                .inter_delta_stall_50ms_count
                .saturating_add(u64::from(gap_ns >= 50_000_000));
            self.pipeline.inter_delta_stall_100ms_count = self
                .pipeline
                .inter_delta_stall_100ms_count
                .saturating_add(u64::from(gap_ns >= 100_000_000));
            self.pipeline.inter_delta_stall_250ms_count = self
                .pipeline
                .inter_delta_stall_250ms_count
                .saturating_add(u64::from(gap_ns >= 250_000_000));
        }
        self.last_display_delta_received_ns = Some(received_ns);
    }
}

struct ReceivedServerEvent {
    event: ServerEvent,
    received_ns: u64,
    api_event_seq: u64,
}

pub(crate) enum ResponseEventSource<'a> {
    WebSocket(&'a mut ResponsesSocket),
    #[cfg(not(target_family = "wasm"))]
    Https(&'a mut ResponsesHttpStream),
}

impl ResponseEventSource<'_> {
    async fn next_text_or_idle_timeout(
        &mut self,
    ) -> Result<crate::socket::ReceivedText, ResponsesError> {
        match self {
            Self::WebSocket(socket) => socket.next_text_or_idle_timeout().await,
            #[cfg(not(target_family = "wasm"))]
            Self::Https(stream) => stream.next_text_or_idle_timeout().await,
        }
    }
}

pub(crate) async fn receive(
    source: &mut ResponseEventSource<'_>,
    transport: &'static str,
    events: &EventSink,
    call_index: u32,
    started_at: Instant,
) -> Result<TurnResult, ResponsesServiceError> {
    let mut done_items = Vec::with_capacity(2);
    let mut assistant_items = HashMap::new();
    let mut timing = StreamTiming::new(started_at);

    loop {
        let received = next_event(
            source,
            transport,
            events,
            "generation",
            call_index,
            &mut timing,
        )
        .await?;
        match received.event {
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
                let item = output_index.and_then(|index| assistant_items.get(&index));
                emit_display_delta(
                    events,
                    &mut timing,
                    AgentEventKind::AssistantDelta,
                    AssistantTextDelta {
                        model_call_index: call_index,
                        item_id: item.and_then(|item| item.item_id.as_deref()),
                        phase: item.and_then(|item| item.phase),
                        text: &delta,
                    },
                    received.received_ns,
                    received.api_event_seq,
                    delta.len(),
                )?;
            }
            ServerEvent::ReasoningSummaryTextDelta { delta }
            | ServerEvent::ReasoningSummaryDelta { delta } => {
                emit_display_delta(
                    events,
                    &mut timing,
                    AgentEventKind::ReasoningSummaryDelta,
                    TextDelta {
                        model_call_index: call_index,
                        text: &delta,
                    },
                    received.received_ns,
                    received.api_event_seq,
                    delta.len(),
                )?;
            }
            ServerEvent::OutputItemDone { item } => {
                emit_assistant_message(events, call_index, &item)?;
                done_items.push(item);
            }
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
                    pipeline_stats: timing.pipeline,
                });
            }
            _ => {}
        }
    }
}

fn emit_display_delta<P: Serialize>(
    events: &EventSink,
    timing: &mut StreamTiming,
    kind: AgentEventKind,
    payload: P,
    received_ns: u64,
    api_event_seq: u64,
    payload_bytes: usize,
) -> Result<(), ResponsesServiceError> {
    timing.record_display_delta(received_ns, payload_bytes);
    let seq = events.emit_with_source_sequence(kind, payload, Some(received_ns))?;
    tracing::trace!(
        target: "nanocodex_stream_timing",
        stage = "api_delta_emitted",
        request.id = events.request_id(),
        event.seq = seq,
        event.kind = ?kind,
        source.api.event.seq = api_event_seq,
        payload.bytes = payload_bytes,
        socket_to_agent_emit_ns = monotonic_now_ns().saturating_sub(received_ns),
        "Responses display delta entered the agent event stream"
    );
    Ok(())
}

fn emit_assistant_message(
    events: &EventSink,
    call_index: u32,
    item: &ResponseItem,
) -> Result<(), ResponsesServiceError> {
    let ResponseItem::Message {
        id,
        role: MessageRole::Assistant,
        content,
        phase,
        ..
    } = item
    else {
        return Ok(());
    };
    events.emit(
        AgentEventKind::AssistantMessage,
        AssistantMessage {
            model_call_index: call_index,
            item_id: id.as_deref(),
            phase: *phase,
            text: output_text(content),
        },
    )?;
    Ok(())
}

pub(crate) async fn receive_compaction(
    source: &mut ResponseEventSource<'_>,
    transport: &'static str,
    events: &EventSink,
    call_index: u32,
    started_at: Instant,
) -> Result<CompactionResult, ResponsesServiceError> {
    let mut done_items = Vec::with_capacity(2);
    let mut timing = StreamTiming::new(started_at);

    loop {
        let received = next_event(
            source,
            transport,
            events,
            "compaction",
            call_index,
            &mut timing,
        )
        .await?;
        match received.event {
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
                    pipeline_stats: timing.pipeline,
                });
            }
            _ => {}
        }
    }
}

async fn next_event(
    source: &mut ResponseEventSource<'_>,
    transport: &'static str,
    events: &EventSink,
    phase: &'static str,
    call_index: u32,
    timing: &mut StreamTiming,
) -> Result<ReceivedServerEvent, ResponsesServiceError> {
    let receive_started_at = Instant::now();
    let received = source.next_text_or_idle_timeout().await?;
    timing.pipeline.receive_wait_duration_ns = timing
        .pipeline
        .receive_wait_duration_ns
        .saturating_add(elapsed_ns(receive_started_at));
    timing.pipeline.event_count = timing.pipeline.event_count.saturating_add(1);
    timing.pipeline.socket_queue_duration_ns = timing
        .pipeline
        .socket_queue_duration_ns
        .saturating_add(monotonic_now_ns().saturating_sub(received.received_ns));
    timing.pipeline.event_bytes = timing
        .pipeline
        .event_bytes
        .saturating_add(u64::try_from(received.text.len()).unwrap_or(u64::MAX));

    let parse_started_at = Instant::now();
    let raw_event = parse_raw_json(received.text.as_str())?;
    timing.pipeline.parse_duration_ns = timing
        .pipeline
        .parse_duration_ns
        .saturating_add(elapsed_ns(parse_started_at));
    let elapsed = elapsed_ns(timing.started_at);
    timing.first_event_ns.get_or_insert(elapsed);

    let emit_started_at = Instant::now();
    let api_event_seq = events.emit_with_source_sequence(
        AgentEventKind::ApiEvent,
        ApiEvent {
            direction: "inbound",
            transport,
            phase,
            model_call_index: Some(call_index),
            event: raw_event,
        },
        Some(received.received_ns),
    )?;
    timing.pipeline.emit_duration_ns = timing
        .pipeline
        .emit_duration_ns
        .saturating_add(elapsed_ns(emit_started_at));

    let decode_started_at = Instant::now();
    let event = decode_event::<ServerEvent>(raw_event)?;
    timing.pipeline.decode_duration_ns = timing
        .pipeline
        .decode_duration_ns
        .saturating_add(elapsed_ns(decode_started_at));
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
    Ok(ReceivedServerEvent {
        event,
        received_ns: received.received_ns,
        api_event_seq,
    })
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
        Some(output_text(content))
    })
}

fn output_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentItem::OutputText { text, .. } => Some(text.as_ref()),
            ContentItem::InputText { .. }
            | ContentItem::InputImage { .. }
            | ContentItem::InputAudio { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use web_time::Instant;

    use super::StreamTiming;

    #[test]
    fn display_delta_cadence_records_gaps_and_stalls() {
        let mut timing = StreamTiming::new(Instant::now());

        timing.record_display_delta(1_000_000, 3);
        timing.record_display_delta(61_000_000, 5);
        timing.record_display_delta(311_000_000, 7);

        assert_eq!(timing.pipeline.display_delta_count, 3);
        assert_eq!(timing.pipeline.display_delta_bytes, 15);
        assert_eq!(timing.pipeline.inter_delta_gap_duration_ns, 310_000_000);
        assert_eq!(timing.pipeline.inter_delta_gap_max_ns, 250_000_000);
        assert_eq!(timing.pipeline.inter_delta_stall_50ms_count, 2);
        assert_eq!(timing.pipeline.inter_delta_stall_100ms_count, 1);
        assert_eq!(timing.pipeline.inter_delta_stall_250ms_count, 1);
    }
}
