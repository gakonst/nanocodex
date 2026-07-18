mod agent;
mod error;
mod model;
mod protocol;

use std::io::{BufRead, Write};

pub use agent::{
    Agent, AgentBuilder, AgentDriver, AgentParts, CustomAgentBuilder, PromptDisposition,
    PromptReceipt, Turn, TurnOutcome,
};
pub use error::{AgentError, HarnessError, ResponsesError, Result};
pub use harness_core::responses::RequestProfile;
pub use harness_core::{
    AgentEvent, AgentEventKind, AgentEvents, AgentMessageContent, ContentItem, CustomToolFormat,
    FunctionOutputBody, FunctionOutputContent, ImageDetail, InternalMessageMetadata, ItemStatus,
    JsonSchema, JsonValue, LocalShellAction, LocalShellExecAction, LocalShellStatus, MessagePhase,
    MessageRole, ModelConfig, OutputTextAnnotation, OutputTextLogprob, OutputTextTopLogprob,
    Prompt, PromptInput, ReasoningContent, ReasoningEffort, ReasoningSummary, ResponseItem,
    ToolCaller, ToolDefinition, Usage, UserInput, WebSearchAction,
};
pub use harness_service::{
    DefaultResponsesService, ResponsesAttempt, ResponsesAttemptKind, ResponsesClient,
    ResponsesRetryPolicy, ResponsesService, ResponsesServiceError, ResponsesServiceResponse,
};
use protocol::read_task_start;

/// Run one harness request from JSONL input to JSONL output.
///
/// # Errors
///
/// Returns an error when the input envelope is invalid, a mode fails, or an
/// output event cannot be written.
pub async fn run(input: impl BufRead, output: impl Write, config: ModelConfig) -> Result<()> {
    let request = read_task_start(input)?;
    let AgentParts {
        agent,
        driver,
        events,
    } = Agent::builder(config)
        .request_id(request.request_id)
        .build_parts()?;
    let receipt = agent.prompt(request.task).await?;
    drop(agent);
    let (driver_result, events_result, turn_result) = tokio::join!(
        driver.run(),
        events.write_jsonl(output),
        receipt.turn.completed(),
    );
    driver_result?;
    events_result?;
    turn_result.map(|_| ())
}
