use std::time::Duration;

use super::api_error::{api_error_has_code, api_error_is_checkpoint_missing, retryable_api_error};

/// Errors produced by the standard `OpenAI` Responses transports.
///
/// The API is identical across native and hosted targets. Platform adapters
/// retain their complete error detail while reducing retry behavior to explicit
/// typed fields before the error reaches the shared state machine.
#[derive(Debug, thiserror::Error)]
pub enum ResponsesError {
    /// Authorization could not be resolved.
    #[error("failed to resolve OpenAI authorization: {detail}")]
    Authorization {
        /// Credential-resolution detail without the credential value.
        detail: String,
    },
    /// The embedding did not install the transport required by this target.
    #[error("the Responses host transport is not configured")]
    HostUnavailable,
    /// The configured WebSocket URL was invalid.
    #[error("invalid Responses WebSocket URL: {detail}")]
    InvalidUrl {
        /// Complete parser failure detail.
        detail: String,
    },
    /// An authorization value could not be encoded for the handshake.
    #[error("invalid OpenAI authorization header: {detail}")]
    InvalidAuthorization {
        /// Complete header encoding failure detail.
        detail: String,
    },
    /// A session identity could not be encoded for the handshake.
    #[error("invalid Responses session identifier header: {detail}")]
    InvalidSessionId {
        /// Complete header encoding failure detail.
        detail: String,
    },
    /// The WebSocket handshake exceeded its deadline.
    #[error("Responses WebSocket handshake exceeded {seconds} seconds")]
    HandshakeTimeout {
        /// Configured timeout in seconds.
        seconds: u64,
    },
    /// The WebSocket handshake failed at the transport layer.
    #[error("Responses WebSocket handshake failed: {detail}")]
    Handshake {
        /// Complete platform failure detail.
        detail: String,
        /// Whether opening a replacement socket may safely recover.
        reconnectable: bool,
    },
    /// The server rejected the WebSocket handshake.
    #[error("Responses WebSocket handshake was rejected with HTTP {status}: {body}")]
    HandshakeRejected {
        /// HTTP response status.
        status: u16,
        /// Retained response body.
        body: String,
        /// Server-requested retry delay when present.
        retry_after: Option<Duration>,
    },
    /// Sending a WebSocket frame failed.
    #[error("failed to send a Responses WebSocket frame: {detail}")]
    Send {
        /// Complete platform failure detail.
        detail: String,
        /// Whether opening a replacement socket may safely recover.
        reconnectable: bool,
    },
    /// Sending a WebSocket frame exceeded its deadline.
    #[error("sending a Responses WebSocket frame exceeded {seconds} seconds")]
    SendTimeout {
        /// Configured timeout in seconds.
        seconds: u64,
    },
    /// No response event arrived before the idle deadline.
    #[error("Responses WebSocket produced no event for {seconds} seconds")]
    IdleTimeout {
        /// Configured idle timeout in seconds.
        seconds: u64,
    },
    /// The WebSocket stream ended without a close frame.
    #[error("Responses WebSocket closed without a close frame")]
    UnexpectedEnd,
    /// Receiving a WebSocket frame failed.
    #[error("failed to receive a Responses WebSocket frame: {detail}")]
    Receive {
        /// Complete platform failure detail.
        detail: String,
        /// Whether opening a replacement socket may safely recover.
        reconnectable: bool,
    },
    /// A received WebSocket event was not valid JSON.
    #[error("Responses WebSocket event was not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
    /// The endpoint returned a binary frame where text JSON was required.
    #[error("Responses WebSocket returned a binary data frame; expected JSON text")]
    UnexpectedBinary,
    /// A typed request could not be serialized.
    #[error("failed to encode a Responses WebSocket request")]
    EncodeRequest(#[source] serde_json::Error),
    /// An event's payload did not match the shape declared by its type.
    #[error("Responses API event did not match its declared type: {event}")]
    InvalidPayload {
        /// Typed payload decode failure.
        #[source]
        source: serde_json::Error,
        /// Complete retained provider event.
        event: String,
    },
    /// The WebSocket closed with provider-supplied detail.
    #[error("Responses WebSocket closed {detail}")]
    Closed {
        /// Close code and reason.
        detail: String,
    },
    /// The Responses API returned a typed error event.
    #[error("Responses API returned an error event: {event}")]
    Api {
        /// Complete retained provider event.
        event: String,
    },
    /// The request exceeded the model context window.
    #[error("Responses input exceeded the model context window")]
    ContextWindowExceeded {
        /// Complete retained provider event.
        event: String,
    },
    /// The provider rejected malformed or unsupported image data.
    #[error("Responses API rejected invalid image data: {event}")]
    InvalidImageRequest {
        /// Complete retained provider event.
        event: String,
    },
    /// Sending or reading an HTTPS request failed.
    #[error("Responses HTTPS request failed: {detail}")]
    HttpRequest {
        /// Complete platform failure detail.
        detail: String,
        /// Whether replaying the request may recover.
        retryable: bool,
        /// Whether the request exceeded a configured deadline.
        timeout: bool,
    },
    /// The server rejected an HTTPS request.
    #[error("Responses HTTPS request was rejected with HTTP {status}: {body}")]
    HttpRejected {
        /// HTTP response status.
        status: u16,
        /// Retained response body.
        body: String,
        /// Server-requested retry delay when present.
        retry_after: Option<Duration>,
    },
    /// An SSE response body contained invalid UTF-8.
    #[error("Responses HTTPS stream contained invalid UTF-8: {detail}")]
    InvalidSseUtf8 {
        /// Complete UTF-8 failure detail.
        detail: String,
    },
}

impl ResponsesError {
    /// Returns the SDK-owned retry classification, if retrying is safe.
    #[must_use]
    pub fn retry_advice(&self) -> Option<RetryAdvice> {
        let (class, server_delay) = match self {
            Self::Handshake {
                reconnectable: true,
                ..
            } => ("handshake_transport", None),
            Self::HandshakeTimeout { .. } => ("handshake_timeout", None),
            // ChatGPT's edge can transiently reject an otherwise valid upgrade. Treating the
            // rejection as bounded recovery also unlocks the standard HTTPS fallback.
            Self::HandshakeRejected { status: 403, .. } => ("handshake_forbidden", None),
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
            Self::Send {
                reconnectable: true,
                ..
            } => ("send_transport", None),
            Self::IdleTimeout { .. } => ("event_idle_timeout", None),
            Self::UnexpectedEnd | Self::Closed { .. } => ("premature_close", None),
            Self::Receive {
                reconnectable: true,
                ..
            } => ("receive_transport", None),
            Self::Api { event } => retryable_api_error(event)?,
            Self::HttpRequest { timeout: true, .. } => ("https_timeout", None),
            Self::HttpRequest {
                retryable: true, ..
            } => ("https_transport", None),
            Self::HttpRejected {
                status,
                retry_after,
                ..
            } if *status == 429 => ("https_rate_limit", *retry_after),
            Self::HttpRejected {
                status,
                retry_after,
                ..
            } if (500..=599).contains(status) => ("https_server", *retry_after),
            _ => return None,
        };
        Some(RetryAdvice {
            class,
            server_delay,
        })
    }

    /// Returns a stable low-cardinality error class for telemetry.
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            Self::Authorization { .. } => "authorization",
            Self::HostUnavailable => "host_unavailable",
            Self::InvalidUrl { .. } => "invalid_url",
            Self::InvalidAuthorization { .. } => "invalid_authorization",
            Self::InvalidSessionId { .. } => "invalid_session_id",
            Self::HandshakeTimeout { .. } => "handshake_timeout",
            Self::Handshake { .. } => "handshake",
            Self::HandshakeRejected { .. } => "handshake_rejected",
            Self::Send { .. } => "send",
            Self::SendTimeout { .. } => "send_timeout",
            Self::IdleTimeout { .. } => "event_idle_timeout",
            Self::UnexpectedEnd => "premature_close",
            Self::Receive { .. } => "receive",
            Self::InvalidJson(_) => "invalid_json",
            Self::UnexpectedBinary => "unexpected_binary",
            Self::EncodeRequest(_) => "encode_request",
            Self::InvalidPayload { .. } => "invalid_payload",
            Self::Closed { .. } => "closed",
            Self::Api { event } if api_error_is_checkpoint_missing(event) => "checkpoint_missing",
            Self::Api { event } if api_error_has_code(event, "misalignment_policy_violation") => {
                "misalignment_policy_violation"
            }
            Self::Api { .. } => "api",
            Self::ContextWindowExceeded { .. } => "context_window_exceeded",
            Self::InvalidImageRequest { .. } => "invalid_image_request",
            Self::HttpRequest { timeout: true, .. } => "https_timeout",
            Self::HttpRequest { .. } => "https_transport",
            Self::HttpRejected { body, .. }
                if api_error_has_code(body, "misalignment_policy_violation") =>
            {
                "misalignment_policy_violation"
            }
            Self::HttpRejected { status: 429, .. } => "https_rate_limit",
            Self::HttpRejected { status, .. } if (500..=599).contains(status) => "https_server",
            Self::HttpRejected { .. } => "https_rejected",
            Self::InvalidSseUtf8 { .. } => "invalid_sse_utf8",
        }
    }

    /// Returns whether the provider no longer recognizes a continuation ID.
    #[must_use]
    pub fn is_checkpoint_missing(&self) -> bool {
        matches!(self, Self::Api { event } if api_error_is_checkpoint_missing(event))
    }

    /// Returns whether the provider rejected the request for context exhaustion.
    #[must_use]
    pub const fn is_context_window_exceeded(&self) -> bool {
        matches!(self, Self::ContextWindowExceeded { .. })
    }

    /// Returns whether Astra's misalignment monitor stopped the conversation.
    #[must_use]
    pub fn is_misalignment_policy_violation(&self) -> bool {
        match self {
            Self::Api { event } => api_error_has_code(event, "misalignment_policy_violation"),
            Self::HttpRejected { body, .. } => {
                api_error_has_code(body, "misalignment_policy_violation")
            }
            _ => false,
        }
    }

    pub(crate) fn api_event(event: String) -> Self {
        if api_error_has_code(&event, "context_length_exceeded") {
            Self::ContextWindowExceeded { event }
        } else {
            Self::Api { event }
        }
    }
}

/// Retry metadata derived from one typed transport or API error.
#[derive(Clone, Copy, Debug)]
pub struct RetryAdvice {
    /// Stable low-cardinality retry class.
    pub class: &'static str,
    /// Server-supplied minimum delay, if any.
    pub server_delay: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::ResponsesError;
    use crate::transport::api_error::retryable_api_error;

    #[test]
    fn handshake_rejection_retains_provider_retry_delay() {
        let delay = Duration::from_secs(3);
        let error = ResponsesError::HandshakeRejected {
            status: 429,
            body: r#"{"error":"slow down"}"#.to_owned(),
            retry_after: Some(delay),
        };

        let advice = error
            .retry_advice()
            .expect("HTTP 429 handshake rejection must remain retryable");
        assert_eq!(advice.class, "handshake_rate_limit");
        assert_eq!(advice.server_delay, Some(delay));
    }

    #[test]
    fn forbidden_handshake_rejection_is_retryable() {
        let error = ResponsesError::HandshakeRejected {
            status: 403,
            body: "empty response body".to_owned(),
            retry_after: None,
        };

        let advice = error
            .retry_advice()
            .expect("HTTP 403 handshake rejection must allow bounded recovery");
        assert_eq!(advice.class, "handshake_forbidden");
        assert_eq!(advice.server_delay, None);
    }

    #[test]
    fn transport_retryability_is_explicit() {
        let retryable = ResponsesError::Send {
            detail: "socket was replaced".to_owned(),
            reconnectable: true,
        };
        let terminal = ResponsesError::Send {
            detail: "host rejected the request".to_owned(),
            reconnectable: false,
        };

        assert_eq!(
            retryable.retry_advice().map(|advice| advice.class),
            Some("send_transport")
        );
        assert!(terminal.retry_advice().is_none());
    }

    #[test]
    fn retries_server_error_reported_as_error_type() {
        let event = r#"{
            "type":"error",
            "error":{
                "type":"server_error",
                "code":null,
                "message":"An error occurred while processing the request."
            }
        }"#;

        assert_eq!(
            retryable_api_error(event).map(|(class, _)| class),
            Some("api_server")
        );
    }

    #[test]
    fn classifies_context_window_failures_from_nested_response_errors() {
        let error = ResponsesError::api_event(
            r#"{
                "type": "response.failed",
                "response": {
                    "error": {
                        "code": "context_length_exceeded",
                        "message": "maximum context length exceeded"
                    }
                }
            }"#
            .to_owned(),
        );

        assert!(error.is_context_window_exceeded());
        assert_eq!(error.class(), "context_window_exceeded");
        assert!(error.retry_advice().is_none());
    }

    #[test]
    fn error_code_takes_precedence_over_error_type() {
        let event = r#"{
            "type":"error",
            "error":{
                "type":"server_error",
                "code":"invalid_prompt"
            }
        }"#;

        assert!(retryable_api_error(event).is_none());
    }

    #[test]
    fn classifies_misalignment_stops_without_retrying() {
        let event = r#"{
            "type": "response.failed",
            "response": {
                "error": {
                    "type": "invalid_request_error",
                    "code": "misalignment_policy_violation",
                    "message": "conversation stopped"
                }
            }
        }"#;
        let socket = ResponsesError::api_event(event.to_owned());
        let http = ResponsesError::HttpRejected {
            status: 403,
            body: event.to_owned(),
            retry_after: None,
        };

        for error in [socket, http] {
            assert!(error.is_misalignment_policy_violation());
            assert_eq!(error.class(), "misalignment_policy_violation");
            assert!(error.retry_advice().is_none());
        }
    }
}
