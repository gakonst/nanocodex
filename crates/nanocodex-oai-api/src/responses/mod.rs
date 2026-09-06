//! Typed request, event, and item model for the Responses protocol.

mod content;
mod event;
mod item;
#[cfg(feature = "client")]
mod request;
mod tool;

pub use content::{
    AgentMessageContent, ContentItem, FunctionOutputBody, FunctionOutputContent,
    InternalMessageMetadata, ItemStatus, LocalShellAction, LocalShellExecAction, LocalShellStatus,
    MessagePhase, MessageRole, OutputTextAnnotation, OutputTextLogprob, OutputTextTopLogprob,
    ReasoningContent, ReasoningSummary, ToolCaller, WebSearchAction,
};
pub use event::{
    CompletedResponse, InputTokenDetails, OutputTokenDetails, ResponseEvent, ServerEvent, Usage,
    WarmupResponse, WarmupServerEvent,
};
pub use item::{ConfigurationUpdateReasoning, ResponseItem, ResponseItemId};
#[cfg(feature = "client")]
pub(crate) use request::{CreatePolicy, ResponseCreate};
#[cfg(feature = "client")]
pub use request::{RequestProfile, ResponseHistory, ResponsesInput};
pub use tool::{CustomToolFormat, JsonSchema, JsonValue, ToolDefinition};
