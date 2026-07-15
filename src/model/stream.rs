use std::{io::Write, time::Instant};

use serde::Serialize;
use serde_json::Value;

use super::{
    FunctionCall, ModelResponse, TRANSPORT, elapsed_ns,
    wire::{CompletedResponse, OutputContent, OutputItem, ServerEvent},
};
use crate::{ResponsesError, Result, protocol::EventWriter, responses::ResponsesSocket};

#[derive(Default)]
struct ResponseAccumulator {
    response_id: Option<String>,
    streamed_text: String,
    completed_text: Option<String>,
    has_message: bool,
    function_calls: Vec<FunctionCall>,
}

#[derive(Serialize)]
struct InboundApiEvent<'a> {
    direction: &'static str,
    transport: &'static str,
    model_call_index: u32,
    event: &'a Value,
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
) -> Result<ModelResponse> {
    let mut accumulator = ResponseAccumulator::default();
    let mut first_event_ns = None;
    let mut first_output_ns = None;

    loop {
        let raw_event = socket.next_json().await?;
        let elapsed = elapsed_ns(started_at);
        first_event_ns.get_or_insert(elapsed);
        events.emit(
            "api.event",
            InboundApiEvent {
                direction: "inbound",
                transport: TRANSPORT,
                model_call_index: call_index,
                event: &raw_event,
            },
        )?;
        let event = decode_event(&raw_event)?;
        if event.is_output_delta() {
            first_output_ns.get_or_insert(elapsed);
        }

        match event {
            ServerEvent::Created { response } => accumulator.response_id = Some(response.id),
            ServerEvent::OutputTextDelta { delta } => {
                accumulator.streamed_text.push_str(&delta);
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
            ServerEvent::OutputItemDone { item } => accumulator.add_output_item(item),
            ServerEvent::Completed { response } => {
                return Ok(accumulator.finish(
                    response,
                    first_event_ns.unwrap_or(elapsed),
                    first_output_ns,
                ));
            }
            ServerEvent::Error | ServerEvent::Failed | ServerEvent::Incomplete => {
                return Err(ResponsesError::Api {
                    event: Box::new(raw_event),
                }
                .into());
            }
            ServerEvent::FunctionCallArgumentsDelta | ServerEvent::Other => {}
        }
    }
}

impl ResponseAccumulator {
    fn finish(
        mut self,
        response: CompletedResponse,
        first_event_ns: u64,
        first_output_ns: Option<u64>,
    ) -> ModelResponse {
        self.response_id = Some(response.id.clone());
        for item in response.output {
            self.add_output_item(item);
        }
        let text = self
            .completed_text
            .filter(|text| !text.is_empty())
            .unwrap_or(self.streamed_text);
        ModelResponse {
            id: response.id,
            status: response.status,
            text,
            has_message: self.has_message,
            function_calls: self.function_calls,
            usage: response.usage,
            time_to_first_event_ns: first_event_ns,
            time_to_first_output_ns: first_output_ns,
        }
    }

    fn add_output_item(&mut self, item: OutputItem) {
        match item {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                caller,
            } => {
                if !self
                    .function_calls
                    .iter()
                    .any(|function_call| function_call.call_id == call_id)
                {
                    self.function_calls.push(FunctionCall {
                        call_id,
                        name,
                        arguments,
                        caller,
                    });
                }
            }
            OutputItem::Message { content } => {
                self.has_message = true;
                let text = content
                    .into_iter()
                    .filter_map(|content| match content {
                        OutputContent::OutputText { text } => Some(text),
                        OutputContent::Other => None,
                    })
                    .collect::<String>();
                if !text.is_empty() {
                    self.completed_text = Some(text);
                }
            }
            OutputItem::Program | OutputItem::ProgramOutput | OutputItem::Other => {}
        }
    }
}

fn decode_event(raw_event: &Value) -> Result<ServerEvent> {
    serde_json::from_value(raw_event.clone())
        .map_err(|source| ResponsesError::InvalidPayload {
            source,
            event: Box::new(raw_event.clone()),
        })
        .map_err(Into::into)
}
