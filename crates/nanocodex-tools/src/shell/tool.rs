use std::{path::PathBuf, sync::Arc};

use nanocodex_core::ToolDefinition;
use serde::Deserialize;

use crate::{StandardTool, Tool, ToolContext, ToolExecution, ToolInput, ToolResult};

use super::{ExecCommand, ShellSessions, WriteStdin};

pub(crate) struct ExecCommandHandler {
    workspace: PathBuf,
    sessions: Arc<ShellSessions>,
}

impl ExecCommandHandler {
    pub(crate) fn new(workspace: PathBuf, sessions: Arc<ShellSessions>) -> Self {
        Self {
            workspace,
            sessions,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ExecCommandHandler {
    fn name(&self) -> &'static str {
        "exec_command"
    }

    fn definition(&self) -> ToolDefinition {
        StandardTool::ExecCommand.definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let arguments = input.decode_json::<ExecCommandArguments>()?;
        let command = ExecCommand::new(
            arguments.cmd,
            arguments.workdir,
            arguments.shell,
            arguments.login,
            arguments.tty,
            arguments.yield_time_ms,
            arguments.max_output_tokens,
        );
        let result = self.sessions.execute(command, &self.workspace).await;
        Ok(shell_execution(&result))
    }
}

pub(crate) struct WriteStdinHandler {
    sessions: Arc<ShellSessions>,
}

impl WriteStdinHandler {
    pub(crate) fn new(sessions: Arc<ShellSessions>) -> Self {
        Self { sessions }
    }
}

#[async_trait::async_trait]
impl Tool for WriteStdinHandler {
    fn name(&self) -> &'static str {
        "write_stdin"
    }

    fn definition(&self) -> ToolDefinition {
        StandardTool::WriteStdin.definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let arguments = input.decode_json::<WriteStdinArguments>()?;
        let request = WriteStdin::new(
            arguments.session_id,
            arguments.chars,
            arguments.yield_time_ms,
            arguments.max_output_tokens,
        );
        let result = self.sessions.write_stdin(request).await;
        Ok(shell_execution(&result))
    }
}

fn shell_execution(result: &super::ExecCommandResult) -> ToolExecution {
    ToolExecution::json(&result).with_process_trace(
        result.exit_code,
        result.session_id,
        result.original_token_count,
        result.output.len(),
        result.wall_time_seconds,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecCommandArguments {
    cmd: String,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    login: Option<bool>,
    #[serde(default)]
    tty: bool,
    #[serde(default)]
    yield_time_ms: Option<i64>,
    #[serde(default)]
    max_output_tokens: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteStdinArguments {
    session_id: i64,
    #[serde(default)]
    chars: String,
    #[serde(default)]
    yield_time_ms: Option<i64>,
    #[serde(default)]
    max_output_tokens: Option<i64>,
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use super::{ExecCommandHandler, Tool};
    use crate::shell::ShellSessions;

    #[test]
    fn exec_command_exposes_shell_parameter_and_session_lifecycle() {
        let handler = ExecCommandHandler::new(PathBuf::from("/"), Arc::new(ShellSessions::new()));
        let spec = serde_json::to_value(handler.definition()).unwrap();

        assert_eq!(
            spec.pointer("/description")
                .and_then(serde_json::Value::as_str),
            Some(
                "Runs a shell command, returning output or a session ID for ongoing interaction. Live sessions are terminated when the agent ends; detach services that must remain running afterward."
            )
        );
        assert_eq!(
            spec.pointer("/parameters/properties/shell/type")
                .and_then(serde_json::Value::as_str),
            Some("string")
        );
    }
}
