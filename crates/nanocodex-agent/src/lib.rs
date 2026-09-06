#![doc = include_str!("../README.md")]
#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(target_family = "wasm", not(target_os = "unknown")))]
compile_error!(
    "nanocodex-agent supports browser/JavaScript WebAssembly \
     (`wasm32-unknown-unknown`), not WASI targets"
);

extern crate self as nanocodex_agent;

mod agent;
mod error;
#[cfg(feature = "openai")]
mod model;
#[cfg(feature = "openai")]
mod prompt_cache;
/// Neutral interception contract implemented by optional execution layers.
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub mod execution {
    pub use crate::agent::execution::*;
}
#[cfg(all(feature = "openai", not(target_family = "wasm")))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
/// Codex-compatible durable rollout recording and restoration.
pub mod rollout;
/// Serializable local session snapshots returned when a backend supports them.
pub mod session;
/// Per-turn token accounting and USD estimates.
pub mod usage;

/// Backend implementor surface used by first-party lifecycle crates.
#[doc(hidden)]
pub mod backend {
    pub use crate::agent::backend::*;
}

#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use agent::{AgentHandle, ExecutionEnvironment, NanocodexBuilder};
pub use agent::{
    AgentSessionContext, BuilderBackend, ForkTurns, Nanocodex, PromptRequest, PromptRoute,
    SpawnOptions, Turn, TurnControl, TurnResult,
};
pub use error::{ExecutionPolicyDisposition, NanocodexError, Result};
pub use nanocodex_oai_api::{Model, ReasoningMode, Thinking, events::AgentEvents};
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use nanocodex_oai_api::{OpenAi, ResponseError, ResponseErrorKind};
#[cfg(all(feature = "openai", not(target_family = "wasm")))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
pub use nanocodex_tools::tool;
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use nanocodex_tools::{Tool, Tools};
pub use usage::{
    CostStatus, EstimatedUsdCost, ReportedTurnUsage, ServiceTier, TurnUsage, UsdAmount,
};

/// Complete typed lifecycle events emitted by an agent.
pub mod events {
    #[cfg(feature = "openai")]
    pub use nanocodex_oai_api::events::OpenAiEvent;
    pub use nanocodex_oai_api::events::{
        AgentEvent, AgentEventKind, AgentEventPublisher, AgentEventTiming, AgentEvents, EventError,
        TimedAgentEvent, monotonic_now_ns,
    };
    pub use nanocodex_oai_api::events::{
        AgentEventData, AssistantDelta, AssistantEvent, AssistantMessage, CompactionCompleted,
        CompactionFailed, CompactionStarted, ContextEvent, EventUsage, ModelCallCompleted,
        ModelCallFailed, ModelCallStarted, ModelEvent, ModelWarmupCompleted, ModelWarmupFailed,
        ModelWarmupStarted, ReasoningEvent, ReasoningSummaryDelta, RunError, RunEvent, RunMetrics,
        RunStarted, RunStatus, RunSteered, RunTerminal, ToolCall, ToolEvent, ToolResultEvent,
        ToolStatus, TransportEvent,
    };
    pub use nanocodex_oai_api::responses::AgentMessageContent;
}

/// Prompts and multimodal user input accepted by the agent.
pub mod input {
    pub use nanocodex_oai_api::{
        ImageDetail, Prompt, PromptInput, PromptMessage, PromptMessageRole, UserInput,
        responses::{AgentMessageContent, ContentItem},
    };
}

/// Advanced Responses transport and Tower service configuration.
#[cfg(all(feature = "openai", not(target_family = "wasm")))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
pub mod transport {
    pub use crate::error::ResponsesError;
    pub use nanocodex_oai_api::{
        responses::RequestProfile,
        tower::{
            DefaultResponsesService, ResponsesAttempt, ResponsesAttemptKind, ResponsesClient,
            ResponsesRetryPolicy, ResponsesServiceError, ResponsesServiceResponse,
        },
        transport::{ResponsesHistory, ResponsesTransport},
    };
}

/// Complete tool contracts, registry, built-ins, Code Mode, and MCP.
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub mod tools {
    #[doc(inline)]
    pub use nanocodex_tools::*;
}

#[cfg(all(feature = "openai", not(target_family = "wasm")))]
#[doc(hidden)]
pub mod __private {
    pub use nanocodex_tools::__private::*;
}
