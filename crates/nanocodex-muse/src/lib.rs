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
mod service_tier_serde;
pub use nanocodex_agent_reference::{
    ClaudeModel, HarnessFamily, HarnessModel, ModelCapabilities, ModelTransport,
};
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
#[cfg(all(feature = "rollout", not(target_family = "wasm")))]
#[cfg_attr(
    docsrs,
    doc(cfg(all(feature = "rollout", not(target_family = "wasm"))))
)]
/// Codex-compatible durable rollout recording and restoration.
pub mod rollout;
/// Harness-neutral session identity, lineage, checkpoints, forks, and
/// capabilities, plus the Codex-native snapshot payload.
pub mod session;
/// Per-turn token accounting and USD estimates.
pub use nanocodex_agent_reference::usage;

/// Backend implementor surface used by first-party lifecycle crates.
#[doc(hidden)]
pub mod backend {
    pub use crate::agent::backend::*;
    pub use crate::session::TurnBoundary;
}

pub use agent::{
    AgentHandle, AgentSessionContext, BuilderBackend, Nanocodex, PromptRequest, PromptRoute,
    SpawnOptions, Turn, TurnControl, TurnResult,
};
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use agent::{ExecutionEnvironment, NanocodexBuilder};
#[cfg(feature = "openai")]
pub use error::CompactionRecovery;
pub use error::{ExecutionPolicyDisposition, NanocodexError, Result};
pub use nanocodex_oai_api::{Model, ReasoningMode, Thinking, events::AgentEvents};
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use nanocodex_oai_api::{OpenAi, ResponseError, ResponseErrorKind};
#[cfg(all(feature = "openai", not(target_family = "wasm")))]
#[cfg_attr(docsrs, doc(cfg(all(feature = "openai", not(target_family = "wasm")))))]
pub use nanocodex_oai_tools::tool;
#[cfg(feature = "openai")]
#[cfg_attr(docsrs, doc(cfg(feature = "openai")))]
pub use nanocodex_oai_tools::{Tool, Tools};
pub use session::{
    Capabilities, ForkPoint, ForkRequest, Lineage, Mutability, Origin, Persistence,
    SessionCheckpoint, SessionInfo,
};
pub use usage::{
    CostStatus, EstimatedUsdCost, ReportedTurnUsage, ServiceTier, TurnUsage, UsdAmount,
};

/// Complete typed lifecycle events emitted by an agent.
pub mod events {
    #[cfg(feature = "openai")]
    pub use nanocodex_oai_api::events::OpenAiEvent;
    pub use nanocodex_oai_api::events::{
        AcceptedInput, AgentEventData, AssistantDelta, AssistantEvent, AssistantMessage,
        CompactionCompleted, CompactionFailed, CompactionStarted, ContextEvent, EventUsage,
        ModelCallCompleted, ModelCallFailed, ModelCallStarted, ModelEvent, ModelWarmupCompleted,
        ModelWarmupFailed, ModelWarmupStarted, ReasoningEvent, ReasoningSummaryDelta, RunError,
        RunEvent, RunMetrics, RunStarted, RunStatus, RunSteered, RunTerminal, ToolCall, ToolEvent,
        ToolResultEvent, ToolStatus, TransportEvent,
    };
    pub use nanocodex_oai_api::events::{
        AgentEvent, AgentEventKind, AgentEventPublisher, AgentEventTiming, AgentEvents, EventError,
        TimedAgentEvent, monotonic_now_ns,
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
    pub use nanocodex_oai_tools::*;
}

#[cfg(all(feature = "openai", not(target_family = "wasm")))]
#[doc(hidden)]
pub mod __private {
    pub use nanocodex_oai_tools::__private::*;
}

#[cfg(all(feature = "openai", not(target_family = "wasm")))]
pub mod auth;
#[cfg(feature = "openai")]
mod muse;
#[cfg(feature = "openai")]
pub use muse::{Muse, MuseBuilder};
#[cfg(feature = "openai")]
mod responses;
#[cfg(feature = "openai")]
mod service;
#[cfg(feature = "openai")]
#[doc(hidden)]
pub use service::{MuseService, MuseServiceFactory};
#[cfg(feature = "openai")]
mod image;
#[cfg(all(feature = "openai", not(target_family = "wasm")))]
mod image_generation;
#[cfg(feature = "openai")]
/// Protocol version header required by every Meta API request.
const API_VERSION_HEADER: (&str, &str) = ("x-api-version", "1.0.0");
mod muse_model;
pub use muse_model::MuseModel;
