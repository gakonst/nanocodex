//! Explicit native registrations; project skill metadata grants no authority.
use nanocodex::{
    agent::Result,
    claude::{ClaudeTools, ToolDefinition},
    claude_tools::ClaudeProjectContext,
};
use std::sync::Arc;

pub(super) fn install(
    mut tools: ClaudeTools,
    workspace: Arc<super::worktree::Workspace>,
) -> Result<ClaudeTools> {
    for schema in ClaudeProjectContext::definitions() {
        let definition: ToolDefinition =
            serde_json::from_value(schema).expect("native context schema");
        let workspace = workspace.clone();
        tools = tools.tool_with_context(definition, move |input, _| {
            let workspace = workspace.clone();
            async move {
                let context = ClaudeProjectContext::new(workspace.current())?;
                context
                    .execute("ProjectContext", input)
                    .await
                    .map(super::text_reply)
            }
        });
    }
    Ok(tools)
}
