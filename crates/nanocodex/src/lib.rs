extern crate self as nanocodex;

#[cfg(not(target_family = "wasm"))]
mod agent;
#[cfg(not(target_family = "wasm"))]
mod auth;
mod error;
mod model;
#[cfg(not(target_family = "wasm"))]
mod responses;
#[cfg(target_family = "wasm")]
mod wasm;

#[cfg(not(target_family = "wasm"))]
pub use agent::{AgentHandle, Nanocodex, NanocodexBuilder, Turn, TurnControl, TurnResult};
#[cfg(not(target_family = "wasm"))]
pub use async_trait::async_trait;
#[cfg(not(target_family = "wasm"))]
pub use auth::{
    ChatGptAuthError, ChatGptAuthStatus, ChatGptLogin, chatgpt_auth_status, load_chatgpt_auth,
    logout_chatgpt,
};
pub use error::{NanocodexError, ResponsesError, Result};
pub use nanocodex_core::responses::RequestProfile;
pub use nanocodex_core::{
    AgentEvent, AgentEventKind, AgentEventTiming, AgentEvents, AgentMessageContent, ContentItem,
    CustomToolFormat, FunctionOutputBody, FunctionOutputContent, ImageDetail,
    InternalMessageMetadata, ItemStatus, JsonSchema, JsonValue, LocalShellAction,
    LocalShellExecAction, LocalShellStatus, MODEL, MessagePhase, MessageRole, OpenAiAuth,
    OpenAiAuthError, OpenAiAuthMode, OutputTextAnnotation, OutputTextLogprob, OutputTextTopLogprob,
    Prompt, PromptInput, ReasoningContent, ReasoningSummary, ResponseItem, Thinking,
    TimedAgentEvent, ToolCaller, ToolDefinition, Usage, UserInput, WebSearchAction,
    monotonic_now_ns,
};
#[cfg(not(target_family = "wasm"))]
pub use nanocodex_macros::tool;
#[cfg(not(target_family = "wasm"))]
pub use nanocodex_mcp::{Mcp, McpBuildError, McpBuilder, McpServer};
pub use nanocodex_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesAttemptKind, ResponsesClient,
    ResponsesRetryPolicy, ResponsesService, ResponsesServiceError, ResponsesServiceResponse,
};
#[cfg(not(target_family = "wasm"))]
pub use nanocodex_tools::{
    DEFAULT_TOOL_OUTPUT_TOKENS, StandardTool, Tool, ToolContext, ToolError, ToolExecution,
    ToolInput, ToolInputError, ToolOutputBody, ToolOutputContent, ToolResult, Tools,
    ToolsBuildError, ToolsBuilder, UpdatePlanTool,
};
#[cfg(not(target_family = "wasm"))]
#[doc(hidden)]
pub use responses::{FactoryResponses, LayeredResponses, StandardResponses};
#[cfg(not(target_family = "wasm"))]
pub use responses::{Responses, ResponsesBuilder};
#[cfg(not(target_family = "wasm"))]
pub use schemars::JsonSchema as ToolSchema;
#[cfg(target_family = "wasm")]
pub use wasm::{WasmNanocodex, WasmTurn};

#[cfg(not(target_family = "wasm"))]
#[doc(hidden)]
pub mod __private {
    pub use async_trait::async_trait;
    pub use nanocodex_tools::schema_for;
    pub use schemars;
    pub use serde;
}
