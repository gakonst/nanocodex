use std::time::Duration;

use serde::{Serialize, Serializer};
use serde_json::{Value, value::RawValue};
use web_time::Instant;

use crate::{attempt::ResponsesAttemptKind, service_error::FailurePhase};

/// Stable transport identifier emitted in agent telemetry.
pub const TRANSPORT: &str = "responses_websocket_v2";

pub(crate) fn serialize_compact_raw_json<S>(
    event: &&RawValue,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if event
        .get()
        .bytes()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        let value =
            serde_json::from_str::<Value>(event.get()).map_err(serde::ser::Error::custom)?;
        value.serialize(serializer)
    } else {
        event.serialize(serializer)
    }
}

#[derive(Serialize)]
pub(crate) struct ApiEvent<'a> {
    pub(crate) direction: &'static str,
    pub(crate) transport: &'static str,
    pub(crate) phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_call_index: Option<u32>,
    #[serde(serialize_with = "serialize_compact_raw_json")]
    pub(crate) event: &'a RawValue,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionPurpose {
    Initial,
    WarmupFallback,
    Reconnect,
}

#[derive(Serialize)]
pub(crate) struct AttemptStarted<'a> {
    pub(crate) phase: ResponsesAttemptKind,
    pub(crate) model_call_index: Option<u32>,
    pub(crate) attempt: u32,
    pub(crate) max_attempts: u32,
    pub(crate) replay_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) previous_response_id: Option<&'a str>,
    pub(crate) connection_generation: u32,
}

#[derive(Serialize)]
pub(crate) struct AttemptFailed<'a> {
    pub(crate) phase: ResponsesAttemptKind,
    pub(crate) model_call_index: Option<u32>,
    pub(crate) attempt: u32,
    pub(crate) max_attempts: u32,
    pub(crate) duration_ns: u64,
    pub(crate) failure_phase: FailurePhase,
    pub(crate) error_class: &'static str,
    pub(crate) retryable: bool,
    pub(crate) connection_generation: u32,
    pub(crate) error: &'a str,
}

#[derive(Serialize)]
pub(crate) struct AttemptRetrying<'a> {
    pub(crate) phase: ResponsesAttemptKind,
    pub(crate) model_call_index: Option<u32>,
    pub(crate) attempt: u32,
    pub(crate) next_attempt: u32,
    pub(crate) max_attempts: u32,
    pub(crate) failure_phase: FailurePhase,
    pub(crate) error_class: &'static str,
    pub(crate) delay_ns: u64,
    pub(crate) server_requested_delay: bool,
    pub(crate) opens_new_socket: bool,
    pub(crate) replay_mode: &'static str,
    pub(crate) connection_generation: u32,
    pub(crate) error: &'a str,
}

#[derive(Serialize)]
pub(crate) struct ConnectionStarted<'a> {
    pub(crate) transport: &'static str,
    pub(crate) websocket_url: &'a str,
    pub(crate) attempt: u32,
    pub(crate) purpose: ConnectionPurpose,
    pub(crate) connection_generation: u32,
}

#[derive(Serialize)]
pub(crate) struct ConnectionCompleted<'a> {
    pub(crate) transport: &'static str,
    pub(crate) attempt: u32,
    pub(crate) purpose: ConnectionPurpose,
    pub(crate) duration_ns: u64,
    pub(crate) http_status: u16,
    pub(crate) request_id: Option<&'a str>,
    pub(crate) server_model: Option<&'a str>,
    pub(crate) server_reasoning_included: bool,
    pub(crate) connection_generation: u32,
}

#[derive(Serialize)]
pub(crate) struct ConnectionFailed<'a> {
    pub(crate) transport: &'static str,
    pub(crate) attempt: u32,
    pub(crate) purpose: ConnectionPurpose,
    pub(crate) duration_ns: u64,
    pub(crate) error: &'a str,
    pub(crate) connection_generation: u32,
}

pub(crate) fn elapsed_ns(started_at: Instant) -> u64 {
    duration_ns(started_at.elapsed())
}

pub(crate) fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) fn display_endpoint(endpoint: &str) -> &str {
    endpoint.split_once('?').map_or(endpoint, |(base, _)| base)
}

#[cfg(test)]
mod tests {
    use nanocodex_core::{AgentEventKind, EventSink};
    use serde_json::value::RawValue;

    use super::{ApiEvent, TRANSPORT};

    #[tokio::test]
    async fn pretty_api_events_remain_one_jsonl_record() {
        let raw = RawValue::from_string("{\n  \"type\": \"error\"\n}".to_owned())
            .expect("the regression fixture should be valid JSON");
        let mut output = Vec::new();
        let (events, receiver) = EventSink::channel("test".to_owned());
        events
            .emit(
                AgentEventKind::ApiEvent,
                ApiEvent {
                    direction: "inbound",
                    transport: TRANSPORT,
                    phase: "generation",
                    model_call_index: Some(1),
                    event: &raw,
                },
            )
            .expect("the event should serialize");
        drop(events);
        receiver
            .write_jsonl(&mut output)
            .await
            .expect("the event should write");

        let text = String::from_utf8(output).expect("JSONL should be UTF-8");
        assert_eq!(text.lines().count(), 1);
        serde_json::from_str::<serde_json::Value>(&text).expect("JSONL should parse");
    }
}
