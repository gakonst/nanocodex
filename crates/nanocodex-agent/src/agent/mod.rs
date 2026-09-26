#[cfg(feature = "openai")]
use std::{collections::VecDeque, path::PathBuf};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use futures_util::Stream;
#[cfg(feature = "openai")]
use nanocodex_oai_api::{
    __private::{EventSink, ModelConfig, ResponsesServiceFactory, into_openai_parts},
    OpenAi, ReasoningMode, ResponseError,
    auth::OpenAiAuthMode,
    session::SessionId,
    tower::{ResponsesAttempt, ResponsesClient, ResponsesServiceResponse, StandardServiceFactory},
    transport::{ResponsesHistory, ResponsesTransport, TransportStats},
};
use nanocodex_oai_api::{
    Model, Prompt, Thinking,
    events::{AgentEvent, AgentEvents},
};
#[cfg(feature = "openai")]
use nanocodex_tools::Tools;
#[cfg(feature = "openai")]
use nanocodex_tools::ToolsBuildError;
#[cfg(feature = "openai")]
use tokio::sync::oneshot;
#[cfg(feature = "openai")]
use tokio::sync::{mpsc, watch};
#[cfg(feature = "openai")]
use tower::Service;
#[cfg(feature = "openai")]
use tracing::{Instrument, info, info_span};

#[cfg(feature = "openai")]
use crate::prompt_cache::{ModelPromptCache, SharedPromptCache};
use crate::{NanocodexError, Result, session::SessionSnapshot, usage::TurnUsage};
#[cfg(feature = "openai")]
use crate::{
    model::run::{
        CompletedModelTurn, HistoryCheckpoint, ModelCheckpoint, ModelCompactOutcome, ModelRun,
        ModelTurnOutcome, PreparedCheckpoint, QueuedBoundaryOutput, TurnSteering,
        prepare_checkpoint, prepare_history_checkpoint, prepare_resumed_checkpoint,
    },
    session::{CommittedSession, SessionResume},
};

#[cfg(feature = "openai")]
const COMMAND_CAPACITY: usize = 8;
#[cfg(feature = "openai")]
const STEER_CAPACITY: usize = 8;

#[cfg(feature = "openai")]
type ToolsFactory =
    Arc<dyn Fn(AgentHandle) -> std::result::Result<Tools, ToolsBuildError> + Send + Sync>;

#[cfg(feature = "openai")]
type SpawnObserver = dyn Fn(&str) + Send + Sync;

#[cfg(feature = "openai")]
enum InitialResume {
    Exact(Box<ModelCheckpoint>),
    History(Box<HistoryCheckpoint>),
}

#[cfg(feature = "openai")]
impl InitialResume {
    fn workspace(&self) -> &str {
        match self {
            Self::Exact(checkpoint) => checkpoint.workspace(),
            Self::History(resume) => &resume.workspace,
        }
    }

    fn history_len(&self) -> usize {
        match self {
            Self::Exact(checkpoint) => checkpoint.history().len(),
            Self::History(resume) => resume.history.len(),
        }
    }
}

#[derive(Clone)]
#[cfg(feature = "openai")]
enum ToolsConfiguration {
    Shared(Tools),
    PerAgent(ToolsFactory),
}

#[cfg(feature = "openai")]
impl ToolsConfiguration {
    fn materialize(&self, agent_handle: AgentHandle) -> Result<Tools> {
        match self {
            Self::Shared(tools) => Ok(tools.clone()),
            Self::PerAgent(factory) => factory(agent_handle).map_err(Into::into),
        }
    }
}

/// Receipt for a trusted, typed late function output. Model continuation is
/// deliberately reported separately from durable checkpoint acceptance.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LateFunctionOutputReceipt {
    /// Caller-owned stable idempotency identity.
    pub operation_id: String,
    /// The exact staged function call completed.
    pub call_id: String,
    /// Whether this identical operation was previously checkpointed.
    pub replayed: bool,
    /// Whether a genuine prompt-less model continuation was started.
    pub continuation_started: bool,
}

/// One typed terminal result within a bounded, trusted idle cohort.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LateFunctionOutput {
    /// Original function-call identity, not a generated surrogate.
    pub call_id: String,
    /// Stable caller-owned identity for the per-job journal.
    pub operation_id: String,
    /// Typed terminal function output.
    pub output: nanocodex_oai_api::responses::FunctionOutputBody,
}

pub mod backend;
#[cfg(feature = "openai")]
mod builder;
#[cfg(feature = "openai")]
mod context_source;
#[cfg(feature = "openai")]
mod driver;
#[cfg(feature = "openai")]
pub mod execution;
#[cfg(feature = "openai")]
mod executor;
mod handle;
mod session_context;
#[cfg(feature = "openai")]
mod spawn;
mod turn;

pub use backend::BuilderBackend;
#[cfg(feature = "openai")]
pub use builder::NanocodexBuilder;
#[cfg(feature = "openai")]
pub use context_source::ExecutionEnvironment;
#[cfg(feature = "openai")]
pub use handle::AgentHandle;
pub use handle::Nanocodex;
pub use session_context::AgentSessionContext;
#[cfg(feature = "openai")]
use turn::TurnCheckpoint;
pub use turn::{
    ChildRuntimeSnapshot, PromptRequest, PromptRoute, SpawnOptions, Turn, TurnControl, TurnResult,
};

#[cfg(feature = "openai")]
use backend::{BackendRuntime, LocalLifecycle};
#[cfg(feature = "openai")]
use builder::{CodexCompatibility, PromptCacheConfig};
#[cfg(feature = "openai")]
pub(crate) use context_source::ContextSource;
#[cfg(feature = "openai")]
use context_source::ContextSourceConfig;
#[cfg(feature = "openai")]
use driver::{AgentDriver, AgentOrigin, BranchSpawner, DriverShutdown};
#[cfg(feature = "openai")]
use execution::{Execution, ExecutionConfig};
#[cfg(feature = "openai")]
pub(crate) use execution::{ExecutionStep, ExecutionSteps};
#[cfg(feature = "openai")]
pub(crate) use executor::{AgentFactory, AgentSend};
#[cfg(feature = "openai")]
use executor::{ServiceFactory, spawn_driver};
#[cfg(feature = "openai")]
use handle::request_command;
#[cfg(feature = "openai")]
use spawn::{build_agent, spawn_agent_driver, validate};
#[cfg(feature = "openai")]
use turn::{Command, ExecutionOperation, PromptRouteKind, QueuedTurn, TurnKey};
