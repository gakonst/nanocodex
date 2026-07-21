use std::{collections::HashMap, time::Duration};

use serde::Deserialize;

use tokio_tungstenite::tungstenite::{
    Error as WebSocketError, error::ProtocolError, http::header::InvalidHeaderValue,
};

/// Errors produced by the `OpenAI` Responses WebSocket transport.
#[derive(Debug, thiserror::Error)]
pub enum ResponsesError {
    #[error("failed to resolve OpenAI authorization: {detail}")]
    Authorization { detail: String },
    #[error("invalid Responses WebSocket URL")]
    InvalidUrl(#[source] WebSocketError),
    #[error("invalid OpenAI authorization header")]
    InvalidAuthorization(#[source] InvalidHeaderValue),
    #[error("invalid Responses session identifier header")]
    InvalidSessionId(#[source] InvalidHeaderValue),
    #[error("Responses WebSocket handshake exceeded {seconds} seconds")]
    HandshakeTimeout { seconds: u64 },
    #[error("Responses WebSocket handshake failed")]
    Handshake(#[source] WebSocketError),
    #[error("Responses WebSocket handshake was rejected with HTTP {status}: {body}")]
    HandshakeRejected {
        status: u16,
        body: String,
        retry_after: Option<Duration>,
    },
    #[error("failed to send a Responses WebSocket frame")]
    Send(#[source] WebSocketError),
    #[error("sending a Responses WebSocket frame exceeded {seconds} seconds")]
    SendTimeout { seconds: u64 },
    #[error("Responses WebSocket produced no event for {seconds} seconds")]
    IdleTimeout { seconds: u64 },
    #[error("Responses WebSocket closed without a close frame")]
    UnexpectedEnd,
    #[error("failed to receive a Responses WebSocket frame")]
    Receive(#[source] WebSocketError),
    #[error("Responses WebSocket event was not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
    #[error("Responses WebSocket returned a binary data frame; expected JSON text")]
    UnexpectedBinary,
    #[error("failed to encode a Responses WebSocket request")]
    EncodeRequest(#[source] serde_json::Error),
    #[error("Responses API event did not match its declared type: {event}")]
    InvalidPayload {
        #[source]
        source: serde_json::Error,
        event: String,
    },
    #[error("Responses WebSocket closed {detail}")]
    Closed { detail: String },
    #[error("Responses API returned an error event: {event}")]
    Api { event: String },
    #[error("Responses API rejected invalid image data: {event}")]
    InvalidImageRequest { event: String },
}

impl ResponsesError {
    #[must_use]
    pub fn retry_advice(&self) -> Option<RetryAdvice> {
        let (class, server_delay) = match self {
            Self::HandshakeTimeout { .. } => ("handshake_timeout", None),
            Self::Handshake(error) if is_transient_websocket(error) => {
                ("handshake_transport", None)
            }
            Self::HandshakeRejected {
                status,
                retry_after,
                ..
            } if *status == 429 => ("handshake_rate_limit", *retry_after),
            Self::HandshakeRejected {
                status,
                retry_after,
                ..
            } if (500..=599).contains(status) => ("handshake_server", *retry_after),
            Self::SendTimeout { .. } => ("send_timeout", None),
            Self::Send(error) if is_transient_websocket(error) => ("send_transport", None),
            Self::IdleTimeout { .. } => ("event_idle_timeout", None),
            Self::UnexpectedEnd | Self::Closed { .. } => ("premature_close", None),
            Self::Receive(error) if is_transient_websocket(error) => ("receive_transport", None),
            Self::Api { event } => retryable_api_error(event)?,
            _ => return None,
        };
        Some(RetryAdvice {
            class,
            server_delay,
        })
    }

    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            Self::Authorization { .. } => "authorization",
            Self::InvalidUrl(_) => "invalid_url",
            Self::InvalidAuthorization(_) => "invalid_authorization",
            Self::InvalidSessionId(_) => "invalid_session_id",
            Self::HandshakeTimeout { .. } => "handshake_timeout",
            Self::Handshake(_) => "handshake",
            Self::HandshakeRejected { .. } => "handshake_rejected",
            Self::Send(_) => "send",
            Self::SendTimeout { .. } => "send_timeout",
            Self::IdleTimeout { .. } => "event_idle_timeout",
            Self::UnexpectedEnd => "premature_close",
            Self::Receive(_) => "receive",
            Self::InvalidJson(_) => "invalid_json",
            Self::UnexpectedBinary => "unexpected_binary",
            Self::EncodeRequest(_) => "encode_request",
            Self::InvalidPayload { .. } => "invalid_payload",
            Self::Closed { .. } => "closed",
            Self::Api { event } if api_error_has_code(event, "previous_response_not_found") => {
                "checkpoint_missing"
            }
            Self::Api { .. } => "api",
            Self::InvalidImageRequest { .. } => "invalid_image_request",
        }
    }

    #[must_use]
    pub fn is_checkpoint_missing(&self) -> bool {
        matches!(self, Self::Api { event } if api_error_has_code(event, "previous_response_not_found"))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RetryAdvice {
    pub class: &'static str,
    pub server_delay: Option<Duration>,
}

fn is_transient_websocket(error: &WebSocketError) -> bool {
    matches!(
        error,
        WebSocketError::ConnectionClosed
            | WebSocketError::AlreadyClosed
            | WebSocketError::Io(_)
            | WebSocketError::Protocol(
                ProtocolError::HandshakeIncomplete
                    | ProtocolError::ResetWithoutClosingHandshake
                    | ProtocolError::SendAfterClosing
            )
    )
}

fn retryable_api_error(event: &str) -> Option<(&'static str, Option<Duration>)> {
    let event: ApiErrorEnvelope = serde_json::from_str(event).ok()?;
    let error = event
        .error
        .as_ref()
        .or_else(|| event.response.as_ref()?.error.as_ref())?;
    let class = match error.code.as_deref() {
        Some(
            "server_is_overloaded"
            | "slow_down"
            | "server_error"
            | "websocket_connection_limit_reached",
        ) => "api_server",
        Some("rate_limit_exceeded") => "api_rate_limit",
        _ => return None,
    };
    let server_delay = error
        .retry_after
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .or_else(|| retry_after_header(&event.headers));
    Some((class, server_delay))
}

fn api_error_has_code(event: &str, expected: &str) -> bool {
    let Ok(event) = serde_json::from_str::<ApiErrorEnvelope>(event) else {
        return false;
    };
    let code = event
        .error
        .as_ref()
        .or_else(|| {
            event
                .response
                .as_ref()
                .and_then(|response| response.error.as_ref())
        })
        .and_then(|error| error.code.as_deref());
    code == Some(expected)
}

fn retry_after_header(headers: &HashMap<String, RetryAfterValue>) -> Option<Duration> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.seconds())
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
}

#[derive(Deserialize)]
struct ApiErrorEnvelope {
    #[serde(default)]
    error: Option<ApiErrorDetail>,
    #[serde(default)]
    response: Option<ApiErrorResponse>,
    #[serde(default)]
    headers: HashMap<String, RetryAfterValue>,
}

#[derive(Deserialize)]
struct ApiErrorResponse {
    #[serde(default)]
    error: Option<ApiErrorDetail>,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    #[serde(default)]
    code: Option<Box<str>>,
    #[serde(default)]
    retry_after: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RetryAfterValue {
    Number(f64),
    String(Box<str>),
}

impl RetryAfterValue {
    fn seconds(&self) -> Option<f64> {
        match self {
            Self::Number(seconds) => Some(*seconds),
            Self::String(seconds) => seconds.parse().ok(),
        }
    }
}
