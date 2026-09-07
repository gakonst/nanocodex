use std::sync::Arc;
#[cfg(feature = "openai")]
use std::{io, path::PathBuf};

#[cfg(feature = "openai")]
use nanocodex_oai_api::ResponseError;
#[cfg(feature = "openai")]
pub use nanocodex_oai_api::transport::ResponsesError;

/// Recovery action attached by a higher-layer execution policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionPolicyDisposition {
    /// The same live policy owner may safely retry the operation.
    Retry,
    /// This policy owner must stop and be rebuilt from authoritative state.
    Reopen,
    /// The operation cannot be retried automatically.
    Fatal,
}

/// Error returned by the Nanocodex library boundary.
#[derive(Debug, thiserror::Error)]
pub enum NanocodexError {
    /// Caller input or two configured policies are incompatible.
    #[error("invalid task request: {0}")]
    InvalidRequest(String),

    /// The configured workspace could not be resolved.
    #[cfg(feature = "openai")]
    #[error("failed to resolve task workspace {path}: {source}")]
    ResolveWorkspace {
        /// Workspace path supplied by the caller.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },

    /// The resolved workspace exists but is not a directory.
    #[cfg(feature = "openai")]
    #[error("task workspace is not a directory: {path}")]
    WorkspaceNotDirectory {
        /// Resolved workspace path.
        path: PathBuf,
    },

    /// The resolved workspace cannot be represented as UTF-8.
    #[cfg(feature = "openai")]
    #[error("task workspace path is not valid UTF-8: {path}")]
    WorkspaceNotUtf8 {
        /// Resolved workspace path.
        path: PathBuf,
    },

    /// A follow-on prompt attempted to change an owned session's workspace.
    #[cfg(feature = "openai")]
    #[error("an active agent session cannot change workspace from {current} to {requested}")]
    WorkspaceChanged {
        /// Workspace already owned by the session.
        current: String,
        /// Conflicting workspace requested by the caller.
        requested: String,
    },

    /// A completed provider response violated an agent-loop invariant.
    #[cfg(feature = "openai")]
    #[error("malformed Responses API event: {detail}")]
    MalformedResponse {
        /// Stable invariant failure description.
        detail: &'static str,
    },

    /// A service returned an output for the wrong kind of attempt.
    #[cfg(feature = "openai")]
    #[error("invalid Responses attempt state: {detail}")]
    InvalidAttemptState {
        /// Stable invalid-state description.
        detail: &'static str,
    },

    /// The immutable request prefix could not be serialized for fingerprinting.
    #[cfg(feature = "openai")]
    #[error("failed to fingerprint the immutable prompt prefix: {0}")]
    SerializePromptPrefix(#[source] serde_json::Error),

    /// The private driver stopped before accepting a command.
    #[error("the agent stopped before accepting the command")]
    AgentStopped,

    /// An agent with an attached execution policy stopped and must be rebuilt
    /// from that policy's authoritative state before accepting more work.
    #[error("the execution-policy-owned agent stopped and must be reopened")]
    ExecutionPolicyOwnerStopped,

    /// The private driver stopped after accepting a turn but before delivering its result.
    #[error("the agent stopped before the turn completed")]
    TurnStopped,

    /// Shared cleanup failure returned to every caller of an idempotent
    /// shutdown.
    #[error(transparent)]
    Shutdown(Arc<Self>),

    /// Steering targeted a queued or terminal turn.
    #[error("the targeted turn is queued, completed, or otherwise not active for steering")]
    TurnNotSteerable,

    /// The active turn cannot accept more queued steering input.
    #[error("the active turn's steering queue is full")]
    SteerQueueFull,

    /// Cancellation targeted an already terminal turn.
    #[error("the targeted turn has already completed or been cancelled")]
    TurnNotCancellable,

    /// The targeted turn was cancelled after its resources stopped.
    #[error("the turn was cancelled")]
    TurnCancelled,

    /// A fork was requested before any safe committed boundary existed.
    #[cfg(feature = "openai")]
    #[error("the agent has no safe conversation boundary to fork")]
    ForkBeforeCompletedTurn,

    /// A historical result came from a different conversation lineage.
    #[cfg(feature = "openai")]
    #[error("the completed turn belongs to a different conversation lineage")]
    CheckpointLineageMismatch,

    /// A serialized session snapshot failed structural or policy validation.
    #[cfg(feature = "openai")]
    #[error("invalid session snapshot: {0}")]
    InvalidSessionSnapshot(String),

    /// A higher-layer execution policy or its host store failed.
    #[cfg(feature = "openai")]
    #[error("{layer} execution policy failed: {source}")]
    ExecutionPolicy {
        /// Human-readable layer identity.
        layer: &'static str,
        /// Action the lifecycle must preserve while handling the failure.
        disposition: ExecutionPolicyDisposition,
        /// Original extension error.
        #[source]
        source: Arc<dyn std::error::Error + Send + Sync>,
    },

    /// An identified prompt was submitted without an execution policy.
    #[cfg(feature = "openai")]
    #[error("identified prompt submission requires a configured execution policy")]
    ExecutionPolicyNotConfigured,

    /// Saving or recovering a model context window failed.
    #[cfg(feature = "openai")]
    #[error("context storage failed: {0}")]
    ContextStorage(String),

    /// An attached execution policy violated the agent integration contract.
    #[cfg(feature = "openai")]
    #[error("invalid execution policy state: {0}")]
    InvalidExecutionPolicy(String),

    /// An execution policy relied on a fail-closed default for a capability
    /// that must explicitly acknowledge durable authority.
    #[cfg(feature = "openai")]
    #[error("execution policy does not implement required capability `{capability}`")]
    ExecutionPolicyCapabilityUnsupported {
        /// Missing policy capability.
        capability: &'static str,
    },

    /// A context-inheriting branch was requested from an execution-policy-owned session.
    #[cfg(feature = "openai")]
    #[error(
        "cannot {operation} from an agent with an attached execution policy; build the branch with its own execution policy"
    )]
    ExecutionPolicyBranchUnsupported {
        /// Requested child operation.
        operation: &'static str,
    },

    /// A typed execution boundary could not be encoded or decoded.
    #[cfg(feature = "openai")]
    #[error("execution policy payload is invalid: {0}")]
    ExecutionPayload(#[source] serde_json::Error),

    /// A policy-replayed result does not retain an in-process fork checkpoint.
    #[cfg(feature = "openai")]
    #[error("a policy-replayed result cannot be used as an in-process fork checkpoint")]
    ReplayedCheckpointUnavailable,

    /// The selected backend does not implement one lifecycle capability.
    #[error("the selected agent backend does not support {capability}")]
    UnsupportedCapability {
        /// Stable capability name.
        capability: &'static str,
    },

    /// A concrete backend violated the common lifecycle contract.
    #[error("agent backend violated the lifecycle contract: {detail}")]
    BackendContract {
        /// Stable invariant that the backend violated.
        detail: &'static str,
    },

    /// A concrete lifecycle backend reported a transport or protocol failure.
    #[error("{backend} backend failed: {source}")]
    Backend {
        /// Stable backend identity.
        backend: &'static str,
        /// Original backend error.
        #[source]
        source: Arc<dyn std::error::Error + Send + Sync>,
    },

    /// A previously failed identified operation was replayed from its durable terminal record.
    #[cfg(feature = "openai")]
    #[error("durable operation previously failed: {0}")]
    ReplayedExecutionFailed(String),

    /// Agent construction was attempted outside an active Tokio runtime.
    #[cfg(feature = "openai")]
    #[error("building an agent requires an active Tokio runtime")]
    TokioRuntimeUnavailable,

    /// Codex-compatible rollout recording could not be initialized.
    #[cfg(feature = "openai")]
    #[error("failed to initialize a Codex rollout under {codex_home}: {source}")]
    InitializeRollout {
        /// Codex state directory selected by the caller.
        codex_home: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },

    /// A committed rollout could not be durably persisted.
    #[cfg(feature = "openai")]
    #[error("failed to persist Codex rollout at {path}: {source}")]
    PersistRollout {
        /// Rollout file that could not be written.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },

    /// Contractual agent event serialization failed.
    #[error(transparent)]
    Event(#[from] nanocodex_oai_api::events::EventError),

    /// A complete Responses operation failed.
    #[cfg(feature = "openai")]
    #[error(transparent)]
    Response(#[from] ResponseError),

    /// The configured tool registry or runtime could not be built.
    #[cfg(feature = "openai")]
    #[error("failed to build tools for an agent driver: {0}")]
    Tools(#[from] nanocodex_tools::ToolsBuildError),
}

impl NanocodexError {
    /// Wraps an error returned by a concrete lifecycle backend.
    #[doc(hidden)]
    pub fn backend<E>(backend: &'static str, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend {
            backend,
            source: Arc::new(source),
        }
    }

    /// Wraps an error returned by a higher-layer execution policy.
    #[cfg(feature = "openai")]
    #[doc(hidden)]
    pub fn execution_policy<E>(layer: &'static str, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::ExecutionPolicy {
            layer,
            disposition: ExecutionPolicyDisposition::Fatal,
            source: Arc::new(source),
        }
    }

    /// Wraps an execution-policy error with its required recovery action.
    #[cfg(feature = "openai")]
    #[doc(hidden)]
    pub fn execution_policy_with_disposition<E>(
        layer: &'static str,
        disposition: ExecutionPolicyDisposition,
        source: E,
    ) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::ExecutionPolicy {
            layer,
            disposition,
            source: Arc::new(source),
        }
    }

    /// Returns the recovery action supplied by an execution policy.
    #[cfg(feature = "openai")]
    #[must_use]
    pub fn execution_policy_disposition(&self) -> Option<ExecutionPolicyDisposition> {
        match self {
            Self::ExecutionPolicy { disposition, .. } => Some(*disposition),
            Self::ExecutionPolicyOwnerStopped => Some(ExecutionPolicyDisposition::Reopen),
            Self::Shutdown(source) => source.execution_policy_disposition(),
            _ => None,
        }
    }

    /// Returns the underlying Responses transport/API error, including when a
    /// caller-provided Tower middleware boxed the standard service error.
    #[cfg(feature = "openai")]
    #[must_use]
    pub fn responses_error(&self) -> Option<&ResponsesError> {
        match self {
            Self::Response(error) => error.responses_error(),
            Self::Shutdown(error) => error.responses_error(),
            _ => None,
        }
    }
}

/// Result type returned by the owned agent lifecycle.
pub type Result<T> = std::result::Result<T, NanocodexError>;

#[cfg(all(test, feature = "openai"))]
mod tests {
    use super::{NanocodexError, ResponsesError};
    use nanocodex_oai_api::{ResponseError, tower::ResponsesServiceError};

    #[test]
    fn response_error_is_the_single_provider_failure_boundary() {
        let service = NanocodexError::Response(ResponseError::from(ResponsesServiceError::from(
            ResponsesError::UnexpectedEnd,
        )));
        assert!(matches!(
            service.responses_error(),
            Some(ResponsesError::UnexpectedEnd)
        ));

        let service = ResponsesServiceError::from(ResponsesError::UnexpectedEnd);
        let error =
            NanocodexError::Response(ResponseError::from(Box::new(service) as tower::BoxError));
        assert!(matches!(
            error.responses_error(),
            Some(ResponsesError::UnexpectedEnd)
        ));
        assert_eq!(
            error.to_string(),
            "Responses WebSocket closed without a close frame"
        );
    }
}
