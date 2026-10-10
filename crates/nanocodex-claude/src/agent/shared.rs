//! Native Claude callbacks for tools written against the shared nanocodex
//! [`Tool`] contract, so one implementation serves Responses and Claude agents.

use std::sync::Arc;

use nanocodex_agent::{NanocodexError, Result};
use nanocodex_oai_tools::{Tool, ToolContext, ToolInput, contract::ToolOutputBody};
use serde_json::Value;

use super::{ClaudeToolInvocation, ClaudeToolReply, Handler};
use crate::{ToolDefinition, ToolResultContent};

/// Converts a shared function tool into a Claude definition and callback.
pub(super) fn bridge<T: Tool>(tool: T) -> Result<(ToolDefinition, Handler)> {
    let shared = tool.definition();
    let parameters = shared.parameters().ok_or_else(|| {
        NanocodexError::InvalidRequest(format!(
            "Claude can only call function tools; `{}` is not one",
            shared.name()
        ))
    })?;
    // Claude definitions have no output-schema field, and Code Mode shows
    // nested tools only through their description. Without the result shape,
    // models guess field types.
    let mut description = shared.description().to_owned();
    if let Some(schema) = shared.output_schema() {
        description.push_str("\nOutput schema: ");
        description.push_str(&schema.as_value().to_string());
    }
    let definition = ToolDefinition {
        name: shared.name().to_owned(),
        description,
        input_schema: parameters.as_value().clone(),
        strict: None,
        defer_loading: false,
    };
    let tool = Arc::new(tool);
    let handler: Handler = Arc::new(move |input, invocation| {
        let tool = Arc::clone(&tool);
        Box::pin(async move { execute(&*tool, input, &invocation).await })
    });
    Ok((definition, handler))
}

/// Runs one invocation with the Claude call's identities. Claude keeps its own
/// transcript, so the tool sees no Responses history and no output budget.
async fn execute<T: Tool>(
    tool: &T,
    input: Value,
    invocation: &ClaudeToolInvocation,
) -> std::result::Result<ClaudeToolReply, String> {
    let raw = serde_json::value::to_raw_value(&input).map_err(|error| error.to_string())?;
    let context = ToolContext::new(
        &invocation.model,
        &invocation.session_id,
        &invocation.call_id,
        &[],
        usize::MAX,
    )
    .with_turn_id(Some(&invocation.turn_id))
    .with_host_context(invocation.host_context.as_deref())
    .with_instruction_revision(invocation.instruction_revision);
    let output = tool
        .execute(ToolInput::Function(raw), context)
        .await
        .map_err(|error| error.to_string())?
        .into_wire()
        .map_err(|error| error.to_string())?;
    let text = match output.output {
        ToolOutputBody::Text(text) => text,
        ToolOutputBody::Content(content) => {
            serde_json::to_string(&content).map_err(|error| error.to_string())?
        }
    };
    let decode = |value: Option<Box<serde_json::value::RawValue>>| {
        value
            .map(|value| serde_json::from_str(value.get()))
            .transpose()
            .map_err(|error| error.to_string())
    };
    Ok(ClaudeToolReply {
        content: ToolResultContent::Text(text),
        is_error: !output.success,
        metadata: decode(output.metadata)?,
        structured_result: decode(output.structured_result)?,
    })
}
