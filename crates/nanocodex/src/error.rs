use std::{error::Error, io, path::PathBuf};

pub use nanocodex_service::ResponsesError;
use nanocodex_service::ResponsesServiceError;

/// Error returned by the Nanocodex library boundary.
#[derive(Debug, thiserror::Error)]
pub enum NanocodexError {
    #[error("invalid task request: {0}")]
    InvalidRequest(String),

    #[error("failed to resolve task workspace {path}: {source}")]
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

    #[error("failed to read project instructions from {path}: {source}")]
    ReadProjectInstructions {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("malformed Responses API event: {detail}")]
    MalformedResponse { detail: &'static str },

    #[error("invalid Responses attempt state: {detail}")]
    InvalidAttemptState { detail: &'static str },

    #[error("the agent stopped before accepting the command")]
    AgentStopped,

    #[error("the agent stopped before the turn completed")]
    TurnStopped,

    #[error("the targeted turn is queued, completed, or otherwise not active for steering")]
    TurnNotSteerable,

    #[error("the active turn's steering queue is full")]
    SteerQueueFull,

    #[error("the targeted turn has already completed or been cancelled")]
    TurnNotCancellable,

    #[error("the turn was cancelled")]
    TurnCancelled,

    #[error("the agent has no completed turn to fork")]
    ForkBeforeCompletedTurn,

    #[error("the completed turn belongs to a different conversation lineage")]
    CheckpointLineageMismatch,

    #[error("building an agent requires an active Tokio runtime")]
    TokioRuntimeUnavailable,

    #[error(transparent)]
    Event(#[from] nanocodex_core::EventError),

    #[error(transparent)]
    Responses(#[from] ResponsesError),

    #[error(transparent)]
    ResponsesService(#[from] nanocodex_service::ResponsesServiceError),

    #[cfg(not(target_family = "wasm"))]
    #[error("failed to build tools for an agent driver: {0}")]
    Tools(#[from] nanocodex_tools::ToolsBuildError),

    #[error("Responses service middleware failed: {0}")]
    ResponsesMiddleware(#[from] tower::BoxError),
}

impl NanocodexError {
    /// Returns the underlying Responses transport/API error, including when a
    /// caller-provided Tower middleware boxed the standard service error.
    #[must_use]
    pub fn responses_error(&self) -> Option<&ResponsesError> {
        match self {
            Self::Responses(error) => return Some(error),
            Self::ResponsesService(error) => return error.responses_error(),
            _ => {}
        }

        let mut current = self.source();
        while let Some(error) = current {
            if let Some(service) = error.downcast_ref::<ResponsesServiceError>() {
                return service.responses_error();
            }
            if let Some(responses) = error.downcast_ref::<ResponsesError>() {
                return Some(responses);
            }
            current = error.source();
        }
        None
    }
}

pub type Result<T> = std::result::Result<T, NanocodexError>;

#[cfg(test)]
mod tests {
    use super::{NanocodexError, ResponsesError};
    use nanocodex_service::ResponsesServiceError;

    #[test]
    fn responses_classification_covers_every_service_boundary() {
        let direct = NanocodexError::Responses(ResponsesError::UnexpectedEnd);
        assert!(matches!(
            direct.responses_error(),
            Some(ResponsesError::UnexpectedEnd)
        ));

        let service = NanocodexError::ResponsesService(ResponsesServiceError::from(
            ResponsesError::UnexpectedEnd,
        ));
        assert!(matches!(
            service.responses_error(),
            Some(ResponsesError::UnexpectedEnd)
        ));

        let service = ResponsesServiceError::from(ResponsesError::UnexpectedEnd);
        let error = NanocodexError::ResponsesMiddleware(Box::new(service));
        assert!(matches!(
            error.responses_error(),
            Some(ResponsesError::UnexpectedEnd)
        ));
        assert_eq!(
            error.to_string(),
            "Responses service middleware failed: Responses WebSocket closed without a close frame"
        );
    }
}
