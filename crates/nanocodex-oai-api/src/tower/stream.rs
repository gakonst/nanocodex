use std::collections::HashMap;

use crate::{
    AgentEventKind, ContentItem, EventSink, MessagePhase, MessageRole, ResponseItem,
    ResponseItemId, monotonic_now_ns,
    responses::{ServerEvent, Usage},
};
use serde::{Deserialize, Serialize};
use web_time::Instant;

use crate::{
    ResponsesError,
    attempt::ResponsesObserver,
    service_error::ResponsesServiceError,
    socket::{ResponsesSocket, decode_event, parse_raw_json},
    telemetry::{ApiEvent, elapsed_ns},
};

/// Complete provider output from one `response.create` operation.
#[derive(Deserialize, Serialize)]
pub struct GenerationOutput {
    /// Provider response ID retained privately by a managed session.
    pub id: String,
    /// Provider terminal status.
    pub status: String,
    /// Whether the model affirmatively ended the logical turn.
    pub end_turn: Option<bool>,
    /// Concatenated final assistant message when one was produced.
    pub final_message: Option<String>,
    /// Complete provider output items in order.
    pub output_items: Vec<ResponseItem>,
    /// Parsed custom and function calls derived from the output items.
    pub code_calls: Vec<CodeCall>,
    /// Provider token usage.
    pub usage: Option<Usage>,
    /// Nanoseconds from attempt start to the first provider event.
    pub time_to_first_event_ns: u64,
    /// Nanoseconds from attempt start to the first model output.
    pub time_to_first_output_ns: Option<u64>,
    /// Detailed stream-pipeline counters.
    pub pipeline_stats: ResponsePipelineStats,
}

/// Complete provider output from one `response.compact` operation.
pub struct CompactionOutput {
    /// Provider response ID retained privately by a managed session.
    pub id: String,
    /// Provider terminal status.
    pub status: String,
    /// Exactly one completed compaction item.
    pub item: ResponseItem,
    /// Provider token usage.
    pub usage: Option<Usage>,
    /// Nanoseconds from attempt start to the first provider event.
    pub time_to_first_event_ns: u64,
    /// Nanoseconds from attempt start to the first compaction output.
    pub time_to_first_output_ns: Option<u64>,
    /// Detailed stream-pipeline counters.
    pub pipeline_stats: ResponsePipelineStats,
}

/// Work and latency counters for one complete streamed response.
#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub struct ResponsePipelineStats {
    /// Provider events received.
    pub event_count: u64,
    /// Encoded provider event bytes received.
    pub event_bytes: u64,
    /// Nanoseconds waiting for the transport to yield events.
    pub receive_wait_duration_ns: u64,
    /// Nanoseconds validating raw JSON.
    pub parse_duration_ns: u64,
    /// Nanoseconds emitting raw and normalized events.
    pub emit_duration_ns: u64,
    /// Nanoseconds decoding typed provider events.
    pub decode_duration_ns: u64,
    /// Nanoseconds events waited in the socket pump queue.
    pub socket_queue_duration_ns: u64,
    /// Displayable text and reasoning delta count.
    pub display_delta_count: u64,
    /// Displayable delta bytes.
    pub display_delta_bytes: u64,
    /// Sum of gaps between displayable deltas.
    pub inter_delta_gap_duration_ns: u64,
    /// Largest gap between displayable deltas.
    pub inter_delta_gap_max_ns: u64,
    /// Gaps of at least 50 milliseconds.
    pub inter_delta_stall_50ms_count: u64,
    /// Gaps of at least 100 milliseconds.
    pub inter_delta_stall_100ms_count: u64,
    /// Gaps of at least 250 milliseconds.
    pub inter_delta_stall_250ms_count: u64,
}

/// Completed callable output derived from a response item.
#[derive(Clone, Deserialize, Serialize)]
pub struct CodeCall {
    /// Provider call identity.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Optional tool namespace.
    pub namespace: Option<String>,
    /// Complete function/search arguments or custom-tool input.
    pub input: String,
    /// Wire-level call representation.
    pub kind: CodeCallKind,
}

/// Wire-level representation used by a completed callable output.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeCallKind {
    /// Custom tool call with free-form input.
    Custom,
    /// Function call with JSON arguments.
    Function,
    /// Provider-native deferred-tool search with JSON arguments.
    ToolSearch,
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

pub(crate) trait ResponseEventSource {
    async fn next_text(&mut self) -> Result<crate::socket::ReceivedText, ResponsesError>;
}

impl ResponseEventSource for ResponsesSocket {
    async fn next_text(&mut self) -> Result<crate::socket::ReceivedText, ResponsesError> {
        Self::next_text(self).await
    }
}

pub(crate) async fn receive<S>(
    source: &mut S,
    transport: &'static str,
    observer: &ResponsesObserver,
    call_index: u32,
    started_at: Instant,
) -> Result<GenerationOutput, ResponsesServiceError>
where
    S: ResponseEventSource,
{
    let mut done_items = Vec::with_capacity(2);
    let mut assistant_items = HashMap::new();
    let item_namespace = uuid::Uuid::new_v4();
    let mut timing = StreamTiming::new(started_at);

    loop {
        let received = next_event(
            source,
            transport,
            observer,
            "generation",
            call_index,
            &mut timing,
        )
        .await?;
        match received.event {
            ServerEvent::OutputItemAdded {
                output_index,
                mut item,
            } => {
                let Some(output_index) = output_index else {
                    continue;
                };
                normalize_stream_item(
                    &mut item,
                    output_index,
                    &item_namespace,
                    &mut assistant_items,
                );
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
                let index = output_index.unwrap_or(0);
                let item = &*assistant_items
                    .entry(index)
                    .or_insert_with(|| AssistantStreamItem {
                        item_id: Some(ResponseItemId::with_suffix(
                            "msg",
                            uuid::Uuid::new_v5(&item_namespace, &index.to_be_bytes()),
                        )),
                        phase: None,
                    });
                emit_display_delta(
                    &observer.events,
                    &mut timing,
                    AgentEventKind::AssistantDelta,
                    AssistantTextDelta {
                        model_call_index: call_index,
                        item_id: item.item_id.as_deref(),
                        phase: item.phase,
                        text: &delta,
                    },
                    received.received_ns,
                    received.api_event_seq,
                    delta.len(),
                )?;
            }
            ServerEvent::ReasoningSummaryTextDelta { delta, .. }
            | ServerEvent::ReasoningSummaryDelta { delta, .. } => {
                emit_display_delta(
                    &observer.events,
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
            ServerEvent::OutputItemDone {
                output_index,
                mut item,
            } => {
                normalize_stream_item(
                    &mut item,
                    output_index.unwrap_or(done_items.len() as u32),
                    &item_namespace,
                    &mut assistant_items,
                );
                emit_assistant_message(&observer.events, call_index, &item)?;
                done_items.push(item);
            }
            ServerEvent::Completed { mut response } => {
                let emitted_ids = done_items
                    .iter()
                    .filter_map(|item| item.id().cloned())
                    .collect::<std::collections::HashSet<_>>();
                let mut output_items = if response.output.is_empty() {
                    done_items
                } else {
                    std::mem::take(&mut response.output)
                };
                for (index, item) in output_items.iter_mut().enumerate() {
                    normalize_stream_item(
                        item,
                        index as u32,
                        &item_namespace,
                        &mut assistant_items,
                    );
                    if item.id().is_some_and(|id| !emitted_ids.contains(id)) {
                        emit_assistant_message(&observer.events, call_index, item)?;
                    }
                }
                let code_calls = code_calls(&output_items);
                let final_message = final_message(&output_items);
                return Ok(GenerationOutput {
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

fn normalize_stream_item(
    item: &mut ResponseItem,
    index: u32,
    namespace: &uuid::Uuid,
    known: &mut HashMap<u32, AssistantStreamItem>,
) {
    if let ResponseItem::Message {
        id,
        role: MessageRole::Assistant,
        phase,
        ..
    } = item
    {
        let stream = known.entry(index).or_insert_with(|| AssistantStreamItem {
            item_id: id.clone().or_else(|| {
                Some(ResponseItemId::with_suffix(
                    "msg",
                    uuid::Uuid::new_v5(namespace, &index.to_be_bytes()),
                ))
            }),
            phase: *phase,
        });
        *id = stream.item_id.clone();
        if phase.is_some() {
            stream.phase = *phase;
        } else {
            *phase = stream.phase;
        }
    }
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

pub(crate) async fn receive_compaction<S>(
    source: &mut S,
    transport: &'static str,
    observer: &ResponsesObserver,
    call_index: u32,
    started_at: Instant,
) -> Result<CompactionOutput, ResponsesServiceError>
where
    S: ResponseEventSource,
{
    let mut done_items = Vec::with_capacity(2);
    let mut timing = StreamTiming::new(started_at);

    loop {
        let received = next_event(
            source,
            transport,
            observer,
            "compaction",
            call_index,
            &mut timing,
        )
        .await?;
        match received.event {
            ServerEvent::OutputItemDone { item, .. } => done_items.push(item),
            ServerEvent::Completed { response } => {
                // Pinned Codex counts streamed output_item.done compactions only.
                // The completion envelope must not replace or manufacture them.
                let mut compactions = done_items
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
                return Ok(CompactionOutput {
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

async fn next_event<S>(
    source: &mut S,
    transport: &'static str,
    observer: &ResponsesObserver,
    phase: &'static str,
    call_index: u32,
    timing: &mut StreamTiming,
) -> Result<ReceivedServerEvent, ResponsesServiceError>
where
    S: ResponseEventSource,
{
    let receive_started_at = Instant::now();
    let received = source.next_text().await?;
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

    tracing::trace!(
        target: "nanocodex_oai_api",
        direction = "inbound",
        transport,
        phase,
        model.call_index = call_index,
        api.event = %raw_event.get(),
        "OpenAI Responses API event"
    );
    let emit_started_at = Instant::now();
    let api_event_seq = observer.events.emit_with_source_sequence(
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
    if let Some(event) = event.normalized() {
        observer.emit_response(event).await;
    }
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
        return Err(ResponsesError::api_event(raw_event.get().to_owned()).into());
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
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                execution,
                arguments,
                ..
            } if execution.as_ref() == "client" => {
                calls.push(CodeCall {
                    call_id: call_id.to_string(),
                    name: "tool_search".to_owned(),
                    namespace: None,
                    input: arguments.as_value().to_string(),
                    kind: CodeCallKind::ToolSearch,
                });
            }
            _ => {}
        }
    }
    calls
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
    use serde_json::json;
    use web_time::Instant;

    use super::{
        CodeCallKind, ContentItem, MessageRole, ResponseItem, StreamTiming, code_calls,
        final_message,
    };

    #[test]
    fn missing_provider_ids_are_stable_through_final_history_and_unique_per_attempt() {
        let namespace = uuid::Uuid::new_v4();
        let mut known = std::collections::HashMap::new();
        let mut added = ResponseItem::message(MessageRole::Assistant, []);
        super::normalize_stream_item(&mut added, 0, &namespace, &mut known);
        let id = known[&0].item_id.clone();
        assert!(id.is_some());
        let mut done =
            ResponseItem::message(MessageRole::Assistant, [ContentItem::output_text("answer")]);
        super::normalize_stream_item(&mut done, 0, &namespace, &mut known);
        assert_eq!(
            serde_json::to_value(&done).unwrap()["id"],
            serde_json::to_value(&added).unwrap()["id"]
        );
        let mut history =
            ResponseItem::message(MessageRole::Assistant, [ContentItem::output_text("answer")]);
        super::normalize_stream_item(&mut history, 0, &namespace, &mut known);
        assert_eq!(
            serde_json::to_value(&history).unwrap()["id"],
            serde_json::to_value(&done).unwrap()["id"]
        );
        let mut retry = std::collections::HashMap::new();
        super::normalize_stream_item(
            &mut ResponseItem::message(MessageRole::Assistant, []),
            0,
            &uuid::Uuid::new_v4(),
            &mut retry,
        );
        assert_ne!(id, retry[&0].item_id);
    }

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

    #[test]
    fn only_client_tool_search_calls_with_call_ids_are_callable() {
        let items = serde_json::from_value::<Vec<ResponseItem>>(json!([
            {
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": { "query": "calendar", "limit": 2 }
            },
            {
                "type": "tool_search_call",
                "call_id": "search-2",
                "execution": "server",
                "arguments": { "query": "ignored" }
            },
            {
                "type": "tool_search_call",
                "execution": "client",
                "arguments": { "query": "missing call id" }
            }
        ]))
        .unwrap();

        let calls = code_calls(&items);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "search-1");
        assert_eq!(calls[0].name, "tool_search");
        assert!(calls[0].namespace.is_none());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&calls[0].input).unwrap(),
            json!({ "query": "calendar", "limit": 2 })
        );
        assert!(matches!(calls[0].kind, CodeCallKind::ToolSearch));
    }

    #[test]
    fn final_message_skips_trailing_non_assistant_messages() {
        let items = [
            ResponseItem::message(MessageRole::Assistant, [ContentItem::output_text("answer")]),
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text("trailing input")],
            ),
        ];

        assert_eq!(final_message(&items).as_deref(), Some("answer"));
    }
}
