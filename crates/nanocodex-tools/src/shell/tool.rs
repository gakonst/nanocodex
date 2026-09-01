use std::{path::PathBuf, sync::Arc};

use nanocodex_oai_api::tools::ToolDefinition;
use serde::Deserialize;

use crate::{StandardTool, Tool, ToolContext, ToolInput, ToolOutput, ToolResult};

use super::{ExecCommand, ShellSessions, WriteStdin};

pub(crate) struct ExecCommandHandler {
    workspace: PathBuf,
    sessions: Arc<ShellSessions>,
}

impl ExecCommandHandler {
    pub(crate) const fn new(workspace: PathBuf, sessions: Arc<ShellSessions>) -> Self {
        Self {
            workspace,
            sessions,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ExecCommandHandler {
    fn definition(&self) -> ToolDefinition {
        StandardTool::ExecCommand.definition()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
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
    pub(crate) const fn new(sessions: Arc<ShellSessions>) -> Self {
        Self { sessions }
    }
}

#[async_trait::async_trait]
impl Tool for WriteStdinHandler {
    fn definition(&self) -> ToolDefinition {
        StandardTool::WriteStdin.definition()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
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

fn shell_execution(result: &super::ExecCommandResult) -> ToolOutput {
    if let Some(error) = &result.error {
        return ToolOutput::error(error);
    }
    let structured_result = match serde_json::to_value(result) {
        Ok(value) => value,
        Err(error) => {
            return ToolOutput::error(format!("failed to encode tool result: {error}"));
        }
    };
    ToolOutput::text(shell_response_text(result))
        .with_structured_result(structured_result)
        .with_process_trace(
            result.exit_code,
            result.session_id,
            result.original_token_count,
            result.output.len(),
            result.wall_time_seconds,
        )
}

fn shell_response_text(result: &super::ExecCommandResult) -> String {
    let mut sections = Vec::new();
    if let Some(chunk_id) = result
        .chunk_id
        .as_deref()
        .filter(|chunk_id| !chunk_id.is_empty())
    {
        sections.push(format!("Chunk ID: {chunk_id}"));
    }
    sections.push(format!(
        "Wall time: {:.4} seconds",
        result.wall_time_seconds
    ));
    if let Some(exit_code) = result.exit_code {
        sections.push(format!("Process exited with code {exit_code}"));
    }
    if let Some(session_id) = result.session_id {
        sections.push(format!("Process running with session ID {session_id}"));
    }
    if let Some(original_token_count) = result.original_token_count {
        sections.push(format!("Original token count: {original_token_count}"));
    }
    sections.push("Output:".to_owned());
    sections.push(result.output.clone());
    sections.join("\n")
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
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteStdinArguments {
    session_id: i32,
    #[serde(default)]
    chars: String,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use nanocodex_oai_api::tools::DEFAULT_TOOL_OUTPUT_TOKENS;
    use serde_json::{json, value::to_raw_value};

    use super::{ExecCommandHandler, Tool, shell_execution};
    use crate::{
        ToolContext, ToolInput, ToolOutputBody,
        shell::{ExecCommandResult, ShellSessions},
    };

    #[test]
    fn exec_command_exposes_codex_description_and_shell_parameter() {
        let handler = ExecCommandHandler::new(PathBuf::from("/"), Arc::new(ShellSessions::new()));
        let spec = serde_json::to_value(handler.definition()).unwrap();

        assert_eq!(
            spec.pointer("/description")
                .and_then(serde_json::Value::as_str),
            Some(
                "Runs a command in a PTY, returning output or a session ID for ongoing interaction."
            )
        );
        assert_eq!(
            spec.pointer("/parameters/properties/shell/type")
                .and_then(serde_json::Value::as_str),
            Some("string")
        );
        for unsupported in ["justification", "prefix_rule", "sandbox_permissions"] {
            assert!(
                spec.pointer(&format!("/parameters/properties/{unsupported}"))
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn exec_command_rejects_unsupported_approval_and_sandbox_fields() {
        let handler = ExecCommandHandler::new(PathBuf::from("/"), Arc::new(ShellSessions::new()));
        for (field, value) in [
            ("justification", json!("approve this")),
            ("prefix_rule", json!(["git", "pull"])),
            ("sandbox_permissions", json!("require_escalated")),
        ] {
            let mut arguments = json!({ "cmd": "true" });
            arguments[field] = value;
            let result = handler
                .execute(
                    ToolInput::Function(to_raw_value(&arguments).unwrap()),
                    ToolContext::new(
                        "test-model",
                        "test-session",
                        "test-call",
                        &[],
                        DEFAULT_TOOL_OUTPUT_TOKENS,
                    ),
                )
                .await;
            let Err(error) = result else {
                panic!("exec_command accepted unsupported field {field}");
            };
            assert!(
                error
                    .to_string()
                    .contains(&format!("unknown field `{field}`")),
                "{error}"
            );
        }
    }

    #[test]
    fn shell_results_use_codex_text_directly_and_json_in_code_mode() {
        let result = ExecCommandResult {
            error: None,
            chunk_id: Some("a1b2c3".to_owned()),
            wall_time_seconds: 0.68754,
            exit_code: Some(0),
            session_id: None,
            original_token_count: Some(7),
            output: "hello\n".to_owned(),
        };

        let output = shell_execution(&result);
        let ToolOutputBody::Text(direct) = &output.output else {
            panic!("shell output should be plain text");
        };
        assert_eq!(
            direct,
            "Chunk ID: a1b2c3\n\
             Wall time: 0.6875 seconds\n\
             Process exited with code 0\n\
             Original token count: 7\n\
             Output:\n\
             hello\n"
        );
        assert_eq!(
            output.structured_result(),
            serde_json::json!({
                "chunk_id": "a1b2c3",
                "wall_time_seconds": 0.68754,
                "exit_code": 0,
                "original_token_count": 7,
                "output": "hello\n",
            })
        );
    }

    #[test]
    fn running_shell_result_names_the_session() {
        let result = ExecCommandResult {
            error: None,
            chunk_id: None,
            wall_time_seconds: 10.0,
            exit_code: None,
            session_id: Some(42),
            original_token_count: None,
            output: String::new(),
        };

        let output = shell_execution(&result);
        let ToolOutputBody::Text(direct) = output.output else {
            panic!("shell output should be plain text");
        };
        assert_eq!(
            direct,
            "Wall time: 10.0000 seconds\n\
             Process running with session ID 42\n\
             Output:\n"
        );
    }
}
