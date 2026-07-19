extern crate self as harness_agent;

mod agent;
mod error;
mod model;
mod responses;

pub use agent::{Agent, AgentBuilder, Turn, TurnResult};
pub use async_trait::async_trait;
pub use error::{AgentError, HarnessError, ResponsesError, Result};
pub use harness_core::responses::RequestProfile;
pub use harness_core::{
    AgentEvent, AgentEventKind, AgentEvents, AgentMessageContent, ContentItem, CustomToolFormat,
    FunctionOutputBody, FunctionOutputContent, ImageDetail, InternalMessageMetadata, ItemStatus,
    JsonSchema, JsonValue, LocalShellAction, LocalShellExecAction, LocalShellStatus, MODEL,
    MessagePhase, MessageRole, OutputTextAnnotation, OutputTextLogprob, OutputTextTopLogprob,
    Prompt, PromptInput, ReasoningContent, ReasoningSummary, ResponseItem, Thinking, ToolCaller,
    ToolDefinition, Usage, UserInput, WebSearchAction,
};
pub use harness_macros::tool;
pub use harness_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesAttemptKind, ResponsesClient,
    ResponsesRetryPolicy, ResponsesService, ResponsesServiceError, ResponsesServiceResponse,
};
pub use harness_tools::{
    Tool, ToolContext, ToolExecution, ToolInput, ToolInputError, ToolOutputBody, ToolOutputContent,
    Tools, ToolsBuildError, ToolsBuilder,
};
#[doc(hidden)]
pub use responses::{LayeredResponses, StandardResponses};
pub use responses::{Responses, ResponsesBuilder};
pub use schemars::JsonSchema as ToolSchema;

#[doc(hidden)]
pub mod __private {
    pub use async_trait::async_trait;
    pub use harness_tools::schema_for;
    pub use schemars;
    pub use serde;
}
