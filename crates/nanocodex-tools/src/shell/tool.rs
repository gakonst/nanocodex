use std::{path::PathBuf, sync::Arc};

use nanocodex_core::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Tool, ToolContext, ToolExecution, ToolInput};

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
        ToolDefinition::function(
            self.name(),
            "Runs a shell command, returning output or a session ID for ongoing interaction. Live sessions are terminated when the agent ends; detach services that must remain running afterward.",
            json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "Shell command to execute." },
                    "workdir": {
                        "type": "string",
                        "description": "Working directory for the command. Defaults to the task workspace."
                    },
                    "shell": {
                        "type": "string",
                        "description": "Shell binary to launch. Defaults to the user's default shell."
                    },
                    "login": {
                        "type": "boolean",
                        "description": "True runs with login-shell semantics; false disables them. Defaults to true."
                    },
                    "tty": {
                        "type": "boolean",
                        "description": "True allocates a PTY for the command; false or omitted uses plain pipes."
                    },
                    "yield_time_ms": {
                        "type": "integer",
                        "description": "Wait before yielding output. Defaults to 10000 ms; effective range is 250-30000 ms."
                    },
                    "max_output_tokens": {
                        "type": "integer",
                        "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
                    }
                },
                "required": ["cmd"],
                "additionalProperties": false
            }),
        )
        .with_output_schema(unified_exec_output_schema())
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
        let arguments = match input.decode_json::<ExecCommandArguments>() {
            Ok(arguments) => arguments,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
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
        shell_execution(&result)
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
        ToolDefinition::function(
            self.name(),
            "Writes characters to an existing exec session and returns recent output.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "integer",
                        "description": "Identifier of the running exec session."
                    },
                    "chars": {
                        "type": "string",
                        "description": "Bytes to write to stdin. Defaults to empty, which polls without writing."
                    },
                    "yield_time_ms": {
                        "type": "integer",
                        "description": "Wait before yielding output. Non-empty writes default to 250 ms and cap at 30000 ms; empty polls wait 5000-300000 ms by default."
                    },
                    "max_output_tokens": {
                        "type": "integer",
                        "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy."
                    }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }),
        )
        .with_output_schema(unified_exec_output_schema())
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
        let arguments = match input.decode_json::<WriteStdinArguments>() {
            Ok(arguments) => arguments,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        let request = WriteStdin::new(
            arguments.session_id,
            arguments.chars,
            arguments.yield_time_ms,
            arguments.max_output_tokens,
        );
        let result = self.sessions.write_stdin(request).await;
        shell_execution(&result)
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

fn unified_exec_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": {
                "type": "string",
                "description": "Chunk identifier included when the response reports one."
            },
            "wall_time_seconds": {
                "type": "number",
                "description": "Elapsed wall time spent waiting for output in seconds."
            },
            "exit_code": {
                "type": "number",
                "description": "Process exit code when the command finished during this call."
            },
            "session_id": {
                "type": "number",
                "description": "Session identifier to pass to write_stdin when the process is still running."
            },
            "original_token_count": {
                "type": "number",
                "description": "Approximate token count before output truncation."
            },
            "output": {
                "type": "string",
                "description": "Command output text, possibly truncated."
            }
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    })
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
