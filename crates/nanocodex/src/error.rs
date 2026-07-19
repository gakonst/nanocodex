use std::{io, path::PathBuf};

pub use nanocodex_service::ResponsesError;

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

    #[error("an active agent session cannot change workspace from {current} to {requested}")]
    WorkspaceChanged { current: String, requested: String },

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

    #[error("invalid Responses attempt state: {detail}")]
    InvalidAttemptState { detail: &'static str },

    #[error("the agent driver stopped before accepting the prompt")]
    DriverStopped,

    #[error("the agent driver stopped before the turn completed")]
    TurnStopped,

    #[error("building an agent requires an active Tokio runtime")]
    TokioRuntimeUnavailable,
}

/// Error returned by the nanocodex library boundary.
#[derive(Debug, thiserror::Error)]
pub enum NanocodexError {
    #[error("invalid task request: {0}")]
    InvalidRequest(String),

    #[error(transparent)]
    Event(#[from] nanocodex_core::EventError),

    #[error(transparent)]
    Responses(#[from] ResponsesError),

    #[error(transparent)]
    ResponsesService(#[from] nanocodex_service::ResponsesServiceError),

    #[error(transparent)]
    Agent(#[from] AgentError),

    #[error("Responses service middleware failed")]
    ResponsesMiddleware(#[from] tower::BoxError),
}

impl NanocodexError {
    pub(crate) fn responses_error(&self) -> Option<&ResponsesError> {
        match self {
            Self::Responses(error) => Some(error),
            Self::ResponsesService(error) => error.responses_error(),
            Self::InvalidRequest(_)
            | Self::Event(_)
            | Self::Agent(_)
            | Self::ResponsesMiddleware(_) => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, NanocodexError>;
