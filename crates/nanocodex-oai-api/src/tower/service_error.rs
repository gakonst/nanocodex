use std::time::Duration;

use crate::EventError;
use serde::Serialize;

use crate::{ResponsesError, RetryAdvice};

/// Typed failure returned by the standard Responses Tower service.
#[derive(Debug)]
pub struct ResponsesServiceError {
    pub(crate) source: ResponsesServiceErrorSource,
    pub(crate) phase: FailurePhase,
    class: &'static str,
    pub(crate) retry_advice: Option<RetryAdvice>,
    pub(crate) connection_generation: u32,
}

impl ResponsesServiceError {
    const fn new(
        source: ResponsesServiceErrorSource,
        phase: FailurePhase,
        class: &'static str,
        retry_advice: Option<RetryAdvice>,
        connection_generation: u32,
    ) -> Self {
        Self {
            source,
            phase,
            class,
            retry_advice,
            connection_generation,
        }
    }

    pub(crate) fn responses(
        source: ResponsesError,
        phase: FailurePhase,
        connection_generation: u32,
    ) -> Self {
        let class = source.class();
        let retry_advice = source.retry_advice();
        Self::new(
            ResponsesServiceErrorSource::Responses(source),
            phase,
            class,
            retry_advice,
            connection_generation,
        )
    }

    pub(crate) const fn event(
        source: EventError,
        phase: FailurePhase,
        connection_generation: u32,
    ) -> Self {
        Self::new(
            ResponsesServiceErrorSource::Event(source),
            phase,
            "output",
            None,
            connection_generation,
        )
    }

    pub(crate) const fn invalid_attempt_state(
        detail: &'static str,
        phase: FailurePhase,
        connection_generation: u32,
    ) -> Self {
        Self::new(
            ResponsesServiceErrorSource::InvalidAttemptState { detail },
            phase,
            "protocol",
            None,
            connection_generation,
        )
    }

    pub(crate) const fn invalid_compaction(count: usize) -> Self {
        Self::new(
            ResponsesServiceErrorSource::InvalidCompactionOutput { count },
            FailurePhase::Completion,
            "protocol",
            None,
            0,
        )
    }

    pub(crate) const fn with_connection_generation(mut self, connection_generation: u32) -> Self {
        self.connection_generation = connection_generation;
        self
    }

    /// Returns a stable low-cardinality error class.
    #[must_use]
    pub const fn error_class(&self) -> &'static str {
        self.class
    }

    /// Returns whether the SDK retry policy may safely retry this failure.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.retry_advice.is_some()
    }

    /// Returns whether a private continuation checkpoint disappeared.
    #[must_use]
    pub fn is_checkpoint_missing(&self) -> bool {
        self.responses_error()
            .is_some_and(ResponsesError::is_checkpoint_missing)
    }

    /// Returns whether the request exceeded the model context window.
    #[must_use]
    pub fn is_context_window_exceeded(&self) -> bool {
        self.responses_error()
            .is_some_and(ResponsesError::is_context_window_exceeded)
    }

    /// Returns the server-requested retry delay, if supplied.
    #[must_use]
    pub fn server_retry_after(&self) -> Option<Duration> {
        self.retry_advice.and_then(|advice| advice.server_delay)
    }

    /// Returns the underlying Responses transport error when applicable.
    #[must_use]
    pub const fn responses_error(&self) -> Option<&ResponsesError> {
        match &self.source {
            ResponsesServiceErrorSource::Responses(error) => Some(error),
            ResponsesServiceErrorSource::Event(_)
            | ResponsesServiceErrorSource::InvalidAttemptState { .. }
            | ResponsesServiceErrorSource::InvalidCompactionOutput { .. } => None,
        }
    }
}

impl std::fmt::Display for ResponsesServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for ResponsesServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl From<ResponsesError> for ResponsesServiceError {
    fn from(error: ResponsesError) -> Self {
        let phase = match error {
            ResponsesError::IdleTimeout { .. } => FailurePhase::Idle,
            ResponsesError::UnexpectedEnd
            | ResponsesError::Closed { .. }
            | ResponsesError::Receive { .. } => FailurePhase::Receive,
            ResponsesError::HttpRequest { .. } | ResponsesError::InvalidSseUtf8 { .. } => {
                FailurePhase::Receive
            }
            ResponsesError::Api { .. } | ResponsesError::ContextWindowExceeded { .. } => {
                FailurePhase::Api
            }
            ResponsesError::HttpRejected { .. } => FailurePhase::Api,
            ResponsesError::HostUnavailable
            | ResponsesError::HandshakeTimeout { .. }
            | ResponsesError::Handshake { .. }
            | ResponsesError::HandshakeRejected { .. } => FailurePhase::Connect,
            ResponsesError::Send { .. } | ResponsesError::SendTimeout { .. } => FailurePhase::Send,
            _ => FailurePhase::Protocol,
        };
        Self::responses(error, phase, 0)
    }
}

impl From<EventError> for ResponsesServiceError {
    fn from(error: EventError) -> Self {
        Self::event(error, FailurePhase::Output, 0)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResponsesServiceErrorSource {
    #[error(transparent)]
    Responses(#[from] ResponsesError),
    #[error(transparent)]
    Event(#[from] EventError),
    #[error("invalid Responses attempt state: {detail}")]
    InvalidAttemptState { detail: &'static str },
    #[error("remote compaction returned {count} compaction items; expected exactly one")]
    InvalidCompactionOutput { count: usize },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailurePhase {
    Connect,
    Encode,
    Send,
    Receive,
    Idle,
    Api,
    Protocol,
    Completion,
    Output,
}
