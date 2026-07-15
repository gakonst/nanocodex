use std::{io, path::PathBuf};

use serde_json::Value;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, http::header::InvalidHeaderValue};

/// Failures at the Responses API transport and wire boundary.
#[derive(Debug, thiserror::Error)]
pub enum ResponsesError {
    #[error("invalid Responses WebSocket URL")]
    InvalidUrl(#[source] WebSocketError),

    #[error("invalid OpenAI authorization header")]
    InvalidAuthorization(#[source] InvalidHeaderValue),

    #[error("Responses WebSocket handshake exceeded {seconds} seconds")]
    HandshakeTimeout { seconds: u64 },

    #[error("Responses WebSocket handshake failed")]
    Handshake(#[source] WebSocketError),

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

    #[error("failed to encode a Responses WebSocket request")]
    EncodeRequest(#[source] serde_json::Error),

    #[error("Responses API event did not match its declared type: {event}")]
    InvalidPayload {
        #[source]
        source: serde_json::Error,
        event: Box<Value>,
    },

    #[error("failed to answer a Responses WebSocket ping")]
    Pong(#[source] WebSocketError),

    #[error("answering a Responses WebSocket ping exceeded {seconds} seconds")]
    PongTimeout { seconds: u64 },

    #[error("Responses WebSocket closed {detail}")]
    Closed { detail: String },

    #[error("Responses API returned an error event: {event}")]
    Api { event: Box<Value> },
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

    #[error("model call limit ({limit}) reached before the task completed")]
    ModelCallLimit { limit: u32 },

    #[error("malformed Responses API event ({detail}): {event}")]
    MalformedResponse {
        detail: &'static str,
        event: Box<Value>,
    },

    #[error("failed to encode a tool result")]
    EncodeToolResult(#[source] serde_json::Error),
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
