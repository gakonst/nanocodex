use std::{io, path::PathBuf};

use tokio_tungstenite::tungstenite::{Error as WebSocketError, http::header::InvalidHeaderValue};

/// Failures at the Responses API transport and wire boundary.
#[derive(Debug, thiserror::Error)]
pub enum ResponsesError {
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
    HandshakeRejected { status: u16, body: String },

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
    pub(crate) const fn is_reconnectable_send(&self) -> bool {
        matches!(
            self,
            Self::Send(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed)
        )
    }
}

/// Failures in the model/tool orchestration layer.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("failed to resolve task workspace {path}")]
    ResolveWorkspace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("task workspace is not a directory: {path}")]
    WorkspaceNotDirectory { path: PathBuf },

    #[error("task workspace path is not valid UTF-8: {path}")]
    WorkspaceNotUtf8 { path: PathBuf },

    #[error("failed to read project instructions from {path}")]
    ReadProjectInstructions {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("Responses API requested unsupported function {name} in call {call_id}")]
    UnsupportedFunction { name: String, call_id: String },

    #[error("malformed Responses API event: {detail}")]
    MalformedResponse { detail: &'static str },

    #[error("remote compaction returned {count} compaction items; expected exactly one")]
    InvalidCompactionOutput { count: usize },
}

/// Error returned by the harness library boundary.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("failed to read task request")]
    ReadInput(#[source] io::Error),

    #[error("failed to decode task request")]
    DecodeInput(#[source] serde_json::Error),

    #[error("invalid task request: {0}")]
    InvalidRequest(String),

    #[error("failed to encode stdout event")]
    EncodeOutput(#[source] serde_json::Error),

    #[error("failed to write stdout event")]
    WriteOutput(#[source] io::Error),

    #[error(transparent)]
    Responses(#[from] ResponsesError),

    #[error(transparent)]
    Agent(#[from] AgentError),
}

pub type Result<T> = std::result::Result<T, HarnessError>;
