extern crate self as nanocodex;

mod agent;
mod error;
mod model;
mod responses;

pub use agent::{Nanocodex, NanocodexBuilder, Turn, TurnResult};
pub use async_trait::async_trait;
pub use error::{AgentError, NanocodexError, ResponsesError, Result};
pub use nanocodex_core::responses::RequestProfile;
pub use nanocodex_core::{
    AgentEvent, AgentEventKind, AgentEvents, AgentMessageContent, ContentItem, CustomToolFormat,
    FunctionOutputBody, FunctionOutputContent, ImageDetail, InternalMessageMetadata, ItemStatus,
    JsonSchema, JsonValue, LocalShellAction, LocalShellExecAction, LocalShellStatus, MODEL,
    MessagePhase, MessageRole, OutputTextAnnotation, OutputTextLogprob, OutputTextTopLogprob,
    Prompt, PromptInput, ReasoningContent, ReasoningSummary, ResponseItem, Thinking, ToolCaller,
    ToolDefinition, Usage, UserInput, WebSearchAction,
};
pub use nanocodex_macros::tool;
pub use nanocodex_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesAttemptKind, ResponsesClient,
    ResponsesRetryPolicy, ResponsesService, ResponsesServiceError, ResponsesServiceResponse,
};
pub use nanocodex_tools::{
    DEFAULT_TOOL_OUTPUT_TOKENS, Tool, ToolContext, ToolExecution, ToolInput, ToolInputError,
    ToolOutputBody, ToolOutputContent, Tools, ToolsBuildError, ToolsBuilder,
};
#[doc(hidden)]
pub use responses::{LayeredResponses, StandardResponses};
pub use responses::{Responses, ResponsesBuilder};
pub use schemars::JsonSchema as ToolSchema;

#[doc(hidden)]
pub mod __private {
    pub use async_trait::async_trait;
    pub use nanocodex_tools::schema_for;
    pub use schemars;
    pub use serde;
}
