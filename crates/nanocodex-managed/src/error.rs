use reqwest::StatusCode;

/// Failure returned by the native managed lifecycle transport.
#[derive(Debug, thiserror::Error)]
pub enum ManagedError {
    /// A locally supplied origin, credential, identifier, cursor, or request
    /// violates the managed API contract.
    #[error("{0}")]
    Configuration(String),
    /// The HTTP transport failed before a complete response was available.
    #[error("managed request failed")]
    Transport(#[source] reqwest::Error),
    /// The managed service returned a non-success HTTP response.
    #[error("managed request failed ({status}): {code}: {message}")]
    Http {
        /// HTTP status returned by the service.
        status: StatusCode,
        /// Stable service error code, or an HTTP-derived fallback.
        code: String,
        /// Human-readable service error message.
        message: String,
    },
    /// A successful ordinary response violated its typed JSON contract.
    #[error("managed response is malformed: {0}")]
    InvalidResponse(&'static str),
    /// A durable event-stream frame or envelope violated its protocol.
    #[error("managed event stream is malformed: {0}")]
    InvalidEvent(String),
    /// A VM-host control connection or strict protocol frame was invalid.
    #[error("managed VM host control failed: {0}")]
    VmHost(String),
    /// A managed turn reached an unsuccessful terminal state.
    #[error("managed turn {turn_id} {state}: {message}")]
    Turn {
        /// Stable managed turn identifier.
        turn_id: String,
        /// Terminal state returned by the service.
        state: String,
        /// Failure detail returned by the service.
        message: String,
    },
}
