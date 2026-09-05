//! Private preparation and execution engine for attached tools.

pub(crate) mod runtime;

use std::{collections::HashSet, sync::Arc};

use crate::{ToolDefinition, Tools};

use runtime::PreparedToolEntry;
pub(crate) use runtime::{PreparedToolCall, PreparedToolError, PreparedToolRuntime};

pub(crate) struct PreparedTools {
    entries: Vec<PreparedToolEntry>,
    #[cfg(feature = "native")]
    mcps: Vec<Arc<crate::mcp::Mcp>>,
    #[cfg(feature = "workspace-runtime")]
    workspaces: Vec<Arc<crate::workspace_runtime::WorkspaceToolRuntime>>,
}

impl PreparedTools {
    pub(crate) fn prepare(tools: &Tools) -> Result<Self, PreparedToolError> {
        if tools.workspace_enabled() {
            #[cfg(feature = "workspace-runtime")]
            if tools.workspace_tools.is_none() {
                return Err(PreparedToolError::NonAttachable(
                    "enabled workspace tools require a pinned WorkspaceTools source".into(),
                ));
            }
            #[cfg(not(feature = "workspace-runtime"))]
            return Err(PreparedToolError::NonAttachable(
                "enabled workspace tools require the workspace-runtime feature and a pinned WorkspaceTools source".into(),
            ));
        }
        if tools.web_search_enabled() {
            return Err(PreparedToolError::NonAttachable(
                "built-in web search has no attached executor; disable it for this placement"
                    .into(),
            ));
        }
        if tools.image_generation_enabled() {
            return Err(PreparedToolError::NonAttachable(
                "built-in image generation has no attached executor; disable it for this placement"
                    .into(),
            ));
        }
        if tools.has_unattachable_provider() {
            return Err(PreparedToolError::NonAttachable(
                "a generic dynamic provider cannot produce an immutable catalog; add MCP with ToolsBuilder::add".into(),
            ));
        }

        #[allow(unused_mut, reason = "workspace and MCP features append entries")]
        let mut entries = tools
            .registered
            .iter()
            .map(|registered| {
                let definition = registered.handler.definition();
                validate_definition(&definition)?;
                Ok(PreparedToolEntry::fixed(
                    definition,
                    registered.handler.supports_parallel_tool_calls(),
                    Arc::clone(&registered.handler),
                ))
            })
            .collect::<Result<Vec<_>, PreparedToolError>>()?;
        #[cfg(feature = "workspace-runtime")]
        let mut workspaces = Vec::new();
        #[cfg(feature = "workspace-runtime")]
        if tools.workspace_enabled()
            && let Some(workspace) = tools.workspace_tools.as_ref()
        {
            let workspace = Arc::new(crate::workspace_runtime::WorkspaceToolRuntime::new(
                workspace.root.clone(),
            ));
            for tool in [
                crate::StandardTool::ApplyPatch,
                crate::StandardTool::ExecCommand,
                crate::StandardTool::ViewImage,
                crate::StandardTool::WriteStdin,
            ] {
                entries.push(PreparedToolEntry::workspace(
                    tool.definition(),
                    Arc::clone(&workspace),
                ));
            }
            // `update_plan` is agent-relative state, not a remotely executable
            // workspace effect, and is deliberately absent from this catalog.
            workspaces.push(workspace);
        }

        let mut names = HashSet::with_capacity(entries.len());
        for entry in &entries {
            let name = entry.definition().name();
            if !names.insert(name.to_owned()) {
                return Err(PreparedToolError::DuplicateTool(name.into()));
            }
        }

        Ok(Self {
            entries,
            #[cfg(feature = "native")]
            mcps: tools.attachment_mcps.clone(),
            #[cfg(feature = "workspace-runtime")]
            workspaces,
        })
    }
}

fn validate_definition(definition: &ToolDefinition) -> Result<(), PreparedToolError> {
    match definition {
        ToolDefinition::Function { .. } => Ok(()),
        ToolDefinition::Custom { format, .. } if &*format.kind == "grammar" => Ok(()),
        ToolDefinition::Custom { name, .. } | ToolDefinition::Namespace { name, .. } => {
            Err(PreparedToolError::UnsupportedDefinition(name.clone()))
        }
        ToolDefinition::ToolSearch { .. } => Err(PreparedToolError::UnsupportedDefinition(
            "tool_search".into(),
        )),
    }
}
