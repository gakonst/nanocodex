//! Private Hand transport for the native Claude filesystem capabilities.
//!
//! The managed harness retains its native tool names and schemas. This adapter
//! is a machine primitive, rooted by the Hand owner, never a model tool alias.

use nanocodex_claude_tools::{ClaudeNotebook, ClaudeWorkspaceFiles, ToolOutput as ClaudeOutput};
use nanocodex_oai_tools::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io, path::PathBuf};

pub(super) struct NativeWorkspace {
    root: PathBuf,
    operations: tokio::sync::Mutex<()>,
}

impl NativeWorkspace {
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            operations: tokio::sync::Mutex::new(()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    tool: String,
    input: Value,
}

#[async_trait::async_trait]
impl Tool for NativeWorkspace {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "workspace_tool",
            "Private native filesystem dispatch scoped to this Hand's published workspace. The harness supplies a native tool name and its unchanged input schema. No arbitrary workspace override, process or provider access.",
            json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "enum": ["Read", "Write", "Edit", "Glob", "Grep", "NotebookEdit"]},
                    "input": {"type": "object"}
                },
                "required": ["tool", "input"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let request = input.decode_json::<Request>()?;
        let _operation = self.operations.lock().await;
        // Revalidate the published root on every invocation; native adapters
        // enforce traversal, symlink, size, search and atomic mutation rules.
        let result = match request.tool.as_str() {
            "Read" | "Write" | "Edit" | "Glob" | "Grep" => {
                let files = ClaudeWorkspaceFiles::new(&self.root).map_err(io::Error::other)?;
                files
                    .execute_output_with_context(&request.tool, request.input, false)
                    .await
            }
            "NotebookEdit" => {
                let notebook = ClaudeNotebook::new(&self.root).map_err(io::Error::other)?;
                notebook
                    .execute(&request.tool, request.input)
                    .await
                    .map(ClaudeOutput::text)
            }
            _ => return Err(io::Error::other("unsupported native workspace tool").into()),
        };
        let output = result.unwrap_or_else(ClaudeOutput::error);
        let success = !output.is_error;
        // Keep ordered native text/media and structured diagnostics intact. The
        // receiving harness unwraps this envelope rather than reserializing it
        // as duplicate model-visible text.
        let value = serde_json::to_value(output)?;
        let summary = if success {
            "Native workspace operation completed"
        } else {
            "Native workspace operation failed"
        };
        let receipt = if success {
            ToolOutput::text(summary)
        } else {
            ToolOutput::error(summary)
        };
        Ok(receipt.with_structured_result(value))
    }
}
