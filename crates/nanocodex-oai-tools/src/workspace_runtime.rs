//! Focused retained runtime for the canonical local workspace tools.
//!
//! This runtime is intentionally smaller than the agent-facing
//! `crate::runtime` module available in native builds. It lets constrained
//! process boundaries, including the VM guest, reuse the exact workspace
//! handlers without linking Code Mode, MCP, HTTP clients, or provider
//! transports.

use std::{ffi::OsString, path::PathBuf, sync::Arc};

use crate::{
    SessionEnvironment, StandardTool, Tool, ToolContext, ToolInput, ToolOutput,
    apply_patch::ApplyPatchHandler,
    shell::{ExecCommandHandler, ShellSessions, WriteStdinHandler},
    view_image::ViewImageHandler,
};

/// Retained canonical workspace-tool runtime.
///
/// Shell processes and interactive sessions live for the lifetime of this
/// value, so a later `write_stdin` call can address a session created by
/// `exec_command`.
pub struct WorkspaceToolRuntime {
    apply_patch: ApplyPatchHandler,
    exec_command: ExecCommandHandler,
    view_image: ViewImageHandler,
    write_stdin: WriteStdinHandler,
    sessions: Arc<ShellSessions>,
}

/// Canonical local workspace tools rooted at one directory.
#[derive(Clone, Debug)]
pub struct WorkspaceTools {
    pub(crate) root: PathBuf,
}

impl WorkspaceTools {
    /// Creates canonical workspace tools rooted at `workspace`.
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            root: workspace.into(),
        }
    }

    /// Returns the configured workspace root.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

impl WorkspaceToolRuntime {
    /// Creates a runtime rooted at `workspace` for one agent session.
    ///
    /// Commands run with a sanitized process environment plus the session's
    /// [`SessionEnvironment`] variables.
    #[must_use]
    pub fn new(workspace: PathBuf, session: &SessionEnvironment) -> Self {
        Self::with_optional_view_image_wire_limit(workspace, None, Arc::new(session.os_variables()))
    }

    /// Creates a runtime for a tool selection that no session has bound.
    ///
    /// Commands receive no session variables, not even inherited ones.
    #[cfg(feature = "attachment")]
    pub(crate) fn unbound(workspace: PathBuf) -> Self {
        Self::with_optional_view_image_wire_limit(workspace, None, Arc::default())
    }

    /// Creates a retained runtime whose `view_image` responses must fit one
    /// bounded process-transport frame.
    ///
    /// This is an internal process-boundary policy. Normal in-process callers
    /// should use [`Self::new`].
    #[doc(hidden)]
    #[must_use]
    pub fn with_view_image_wire_limit(workspace: PathBuf, max_wire_bytes: u64) -> Self {
        Self::with_optional_view_image_wire_limit(
            workspace,
            Some(max_wire_bytes),
            Arc::<Vec<(OsString, OsString)>>::default(),
        )
    }

    /// Creates a process-boundary runtime with an explicit environment that
    /// should remain visible to guest shell commands.
    ///
    /// Values whose names look sensitive remain available to the isolated
    /// process while also participating in normal subprocess-output
    /// sanitization. Normal in-process callers should use [`Self::new`].
    #[doc(hidden)]
    #[must_use]
    pub fn with_environment_and_view_image_wire_limit(
        workspace: PathBuf,
        max_wire_bytes: u64,
        environment: Vec<(OsString, OsString)>,
    ) -> Self {
        Self::with_optional_view_image_wire_limit(
            workspace,
            Some(max_wire_bytes),
            Arc::new(environment),
        )
    }

    fn with_optional_view_image_wire_limit(
        workspace: PathBuf,
        max_wire_bytes: Option<u64>,
        environment: Arc<Vec<(OsString, OsString)>>,
    ) -> Self {
        let sessions = Arc::new(ShellSessions::with_environment(environment));
        Self {
            apply_patch: ApplyPatchHandler::new(workspace.clone()),
            exec_command: ExecCommandHandler::new(workspace.clone(), Arc::clone(&sessions)),
            view_image: max_wire_bytes.map_or_else(
                || ViewImageHandler::new(workspace.clone()),
                |max_wire_bytes| {
                    ViewImageHandler::with_wire_limit(workspace.clone(), max_wire_bytes)
                },
            ),
            write_stdin: WriteStdinHandler::new(Arc::clone(&sessions)),
            sessions,
        }
    }

    /// Executes one canonical workspace tool through retained runtime state.
    pub async fn execute_tool(
        &self,
        name: &str,
        input: ToolInput,
        context: ToolContext<'_>,
    ) -> ToolOutput {
        let Some(handler) = self.handler(name) else {
            return ToolOutput::error(format!("unknown workspace tool `{name}`"));
        };
        let result = handler.execute(input, context).await;
        result.unwrap_or_else(|error| ToolOutput::error(error.to_string()))
    }

    pub(crate) fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        self.handler(name)
            .is_some_and(Tool::supports_parallel_tool_calls)
    }

    fn handler(&self, name: &str) -> Option<&dyn Tool> {
        match name {
            name if name == StandardTool::ApplyPatch.name() => Some(&self.apply_patch),
            name if name == StandardTool::ExecCommand.name() => Some(&self.exec_command),
            name if name == StandardTool::ViewImage.name() => Some(&self.view_image),
            name if name == StandardTool::WriteStdin.name() => Some(&self.write_stdin),
            _ => None,
        }
    }

    /// Returns cancellation and shutdown control for retained subprocesses.
    #[must_use]
    pub fn control(&self) -> WorkspaceToolRuntimeControl {
        WorkspaceToolRuntimeControl {
            sessions: Arc::clone(&self.sessions),
        }
    }
}

/// Cancellation and shutdown control for a [`WorkspaceToolRuntime`].
pub struct WorkspaceToolRuntimeControl {
    sessions: Arc<ShellSessions>,
}

impl WorkspaceToolRuntimeControl {
    /// Terminates every retained subprocess and its descendants.
    pub async fn cancel(&self) {
        self.sessions.terminate_all().await;
    }
}

#[cfg(test)]
mod tests {
    use nanocodex_oai_api::tools::ToolInput;
    use serde_json::value::to_raw_value;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn retains_shell_sessions_and_cancels_them() {
        let workspace = tempdir().unwrap();
        let runtime = WorkspaceToolRuntime::new(
            workspace.path().to_path_buf(),
            &SessionEnvironment::root("session"),
        );
        let input = ToolInput::Function(
            to_raw_value(&serde_json::json!({
                "cmd": "printf ready; sleep 30",
                "yield_time_ms": 250
            }))
            .unwrap(),
        );
        let output = runtime
            .execute_tool(
                StandardTool::ExecCommand.name(),
                input,
                ToolContext::new("model", "session", "call", &[], 10_000),
            )
            .await;
        assert!(output.success);
        runtime.control().cancel().await;
    }

    #[tokio::test]
    async fn rejects_non_workspace_tools() {
        let workspace = tempdir().unwrap();
        let runtime = WorkspaceToolRuntime::new(
            workspace.path().to_path_buf(),
            &SessionEnvironment::root("session"),
        );
        let output = runtime
            .execute_tool(
                "web_search",
                ToolInput::Function(to_raw_value(&serde_json::json!({})).unwrap()),
                ToolContext::new("model", "session", "call", &[], 10_000),
            )
            .await;
        assert!(!output.success);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_boundary_environment_reaches_guest_shell_commands() {
        let workspace = tempdir().unwrap();
        let runtime = WorkspaceToolRuntime::with_environment_and_view_image_wire_limit(
            workspace.path().to_path_buf(),
            1024 * 1024,
            vec![(
                OsString::from("NANOCODEX_IMAGE_ENV"),
                OsString::from("from-image"),
            )],
        );
        let input = ToolInput::Function(
            to_raw_value(&serde_json::json!({
                "cmd": "printf %s \"$NANOCODEX_IMAGE_ENV\""
            }))
            .unwrap(),
        );

        let output = runtime
            .execute_tool(
                StandardTool::ExecCommand.name(),
                input,
                ToolContext::new("model", "session", "call", &[], 10_000),
            )
            .await;

        assert!(output.success);
        assert_eq!(output.structured_result()["output"], "from-image");
    }
}
