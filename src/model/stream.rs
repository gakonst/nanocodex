use std::{io::Write, time::Instant};

use serde::Serialize;
use serde_json::Value;

use super::{
    ApiEvent, TRANSPORT, elapsed_ns,
    wire::{ServerEvent, Usage},
};
use crate::{
    AgentError, ResponsesError, Result,
    protocol::EventWriter,
    responses::{ResponsesSocket, decode_event, parse_raw_json},
};

pub(super) struct TurnResult {
    pub(super) id: String,
    pub(super) status: String,
    pub(super) final_message: Option<String>,
    pub(super) output_items: Vec<Value>,
    pub(super) code_calls: Vec<CodeCall>,
    pub(super) usage: Usage,
    pub(super) time_to_first_event_ns: u64,
    pub(super) time_to_first_output_ns: Option<u64>,
}

pub(super) struct CompactionResult {
    pub(super) id: String,
    pub(super) status: String,
    pub(super) item: Value,
    pub(super) usage: Usage,
    pub(super) time_to_first_event_ns: u64,
    pub(super) time_to_first_output_ns: Option<u64>,
}

pub(super) struct CodeCall {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) input: String,
    pub(super) kind: CodeCallKind,
}

pub(super) enum CodeCallKind {
    Custom,
    Function,
}

#[derive(Serialize)]
struct TextDelta<'a> {
    model_call_index: u32,
    text: &'a str,
}

pub(super) async fn receive<W: Write>(
    socket: &mut ResponsesSocket,
    events: &mut EventWriter<W>,
    call_index: u32,
    started_at: Instant,
) -> Result<TurnResult> {
    let mut done_items = Vec::new();
    let mut first_event_ns = None;
    let mut first_output_ns = None;

    loop {
        let text = socket.next_text_or_idle_timeout().await?;
        let raw_event = parse_raw_json(text.as_str())?;
        let elapsed = elapsed_ns(started_at);
        first_event_ns.get_or_insert(elapsed);
        events.emit(
            "api.event",
            ApiEvent {
                direction: "inbound",
                transport: TRANSPORT,
                phase: "generation",
                model_call_index: Some(call_index),
                event: raw_event,
            },
        )?;
        let event = decode_event::<ServerEvent>(raw_event)?;
        if event.is_output() {
            first_output_ns.get_or_insert(elapsed);
        }

        match event {
            ServerEvent::OutputTextDelta { delta } => {
                events.emit(
                    "assistant.delta",
                    TextDelta {
                        model_call_index: call_index,
                        text: &delta,
                    },
                )?;
            }
            ServerEvent::ReasoningSummaryTextDelta { delta }
            | ServerEvent::ReasoningSummaryDelta { delta } => {
                events.emit(
                    "reasoning.summary.delta",
                    TextDelta {
                        model_call_index: call_index,
                        text: &delta,
                    },
                )?;
            }
            ServerEvent::OutputItemDone { item } => done_items.push(item),
            ServerEvent::Completed { mut response } => {
                let output_items = if response.output.is_empty() {
                    done_items
                } else {
                    std::mem::take(&mut response.output)
                };
                let code_calls = code_calls(&output_items)?;
                let final_message = final_message(&output_items);
                return Ok(TurnResult {
                    id: response.id,
                    status: response.status,
                    final_message,
                    output_items,
                    code_calls,
                    usage: response.usage,
                    time_to_first_event_ns: first_event_ns.unwrap_or_default(),
                    time_to_first_output_ns: first_output_ns,
                });
            }
            ServerEvent::Error | ServerEvent::Failed | ServerEvent::Incomplete => {
                return Err(ResponsesError::Api {
                    event: raw_event.get().to_owned(),
                }
                .into());
            }
            ServerEvent::Created | ServerEvent::Other => {}
        }
    }
}

pub(super) async fn receive_compaction<W: Write>(
    socket: &mut ResponsesSocket,
    events: &mut EventWriter<W>,
    call_index: u32,
    started_at: Instant,
) -> Result<CompactionResult> {
    let mut done_items = Vec::new();
    let mut first_event_ns = None;
    let mut first_output_ns = None;

    loop {
        let text = socket.next_text_or_idle_timeout().await?;
        let raw_event = parse_raw_json(text.as_str())?;
        let elapsed = elapsed_ns(started_at);
        first_event_ns.get_or_insert(elapsed);
        events.emit(
            "api.event",
            ApiEvent {
                direction: "inbound",
                transport: TRANSPORT,
                phase: "compaction",
                model_call_index: Some(call_index),
                event: raw_event,
            },
        )?;
        let event = decode_event::<ServerEvent>(raw_event)?;
        if event.is_output() {
            first_output_ns.get_or_insert(elapsed);
        }

        match event {
            ServerEvent::OutputItemDone { item } => done_items.push(item),
            ServerEvent::Completed { mut response } => {
                let output_items = if response.output.is_empty() {
                    done_items
                } else {
                    std::mem::take(&mut response.output)
                };
                let mut compactions = output_items.into_iter().filter(|item| {
                    matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("compaction" | "compaction_summary")
                    )
                });
                let item = compactions.next();
                let count = usize::from(item.is_some()) + compactions.count();
                if count != 1 {
                    return Err(AgentError::InvalidCompactionOutput { count }.into());
                }
                let Some(item) = item else {
                    return Err(AgentError::InvalidCompactionOutput { count: 0 }.into());
                };
                return Ok(CompactionResult {
                    id: response.id,
                    status: response.status,
                    item,
                    usage: response.usage,
                    time_to_first_event_ns: first_event_ns.unwrap_or_default(),
                    time_to_first_output_ns: first_output_ns,
                });
            }
            ServerEvent::Error | ServerEvent::Failed | ServerEvent::Incomplete => {
                return Err(ResponsesError::Api {
                    event: raw_event.get().to_owned(),
                }
                .into());
            }
            ServerEvent::Created
            | ServerEvent::OutputTextDelta { .. }
            | ServerEvent::ReasoningSummaryTextDelta { .. }
            | ServerEvent::ReasoningSummaryDelta { .. }
            | ServerEvent::Other => {}
        }
    }
}

fn code_calls(items: &[Value]) -> Result<Vec<CodeCall>> {
    let mut calls = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("custom_tool_call") => {
                let call_id = required_string(item, "call_id")?;
                let name = required_string(item, "name")?;
                let input = required_string(item, "input")?;
                calls.push(CodeCall {
                    call_id,
                    name,
                    input,
                    kind: CodeCallKind::Custom,
                });
            }
            Some("function_call") => {
                let call_id = required_string(item, "call_id")?;
                let name = required_string(item, "name")?;
                let input = required_string(item, "arguments")?;
                calls.push(CodeCall {
                    call_id,
                    name,
                    input,
                    kind: CodeCallKind::Function,
                });
            }
            _ => {}
        }
    }
    Ok(calls)
}

fn required_string(item: &Value, field: &'static str) -> Result<String> {
    item.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(AgentError::MalformedResponse { detail: field })
        .map_err(Into::into)
}

fn final_message(items: &[Value]) -> Option<String> {
    items.iter().rev().find_map(|item| {
        (item.get("type").and_then(Value::as_str) == Some("message"))
            .then(|| item.get("content").and_then(Value::as_array))
            .flatten()
            .map(|content| {
                content
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
    })
}
