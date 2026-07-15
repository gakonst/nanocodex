use std::{io::Write, time::Instant};

use serde::Serialize;

use super::elapsed_ms;
use crate::{
    Result,
    protocol::{EventWriter, Task},
    shell,
};

const CALL_ID: &str = "fix-git-positive-control";
const CWD: &str = "/app/personal-site";
const COMMAND: &str = "cp -- /app/resources/patch_files/about.md /app/personal-site/_includes/about.md && cp -- /app/resources/patch_files/default.html /app/personal-site/_layouts/default.html";

pub(super) fn run<W: Write>(events: &mut EventWriter<W>, task: &Task) -> Result<()> {
    let started_at = Instant::now();
    events.emit(
        "run.started",
        RunStarted {
            mode: "fix_git_cheat",
            workspace: task.workspace.as_deref(),
            instruction_bytes: task.instruction.len(),
        },
    )?;
    events.emit(
        "tool.call",
        ToolCall {
            call_id: CALL_ID,
            tool: "shell",
            arguments: ToolArguments {
                command: COMMAND,
                cwd: CWD,
                timeout_sec: 30,
            },
        },
    )?;

    let result = shell::execute(COMMAND, CWD);
    let succeeded = result.succeeded;
    events.emit(
        "tool.result",
        ToolResult {
            call_id: CALL_ID,
            tool: "shell",
            status: result.status,
            return_code: result.return_code,
            stdout: &result.stdout,
            stderr: &result.stderr,
            duration_ns: result.duration_ns,
        },
    )?;

    let (message, terminal_kind, status) = if succeeded {
        (
            "Hard-coded positive control copied both verifier fixtures; no model was called.",
            "run.completed",
            "completed",
        )
    } else {
        (
            "The hard-coded positive-control tool call failed.",
            "run.failed",
            "tool_failed",
        )
    };
    events.emit("assistant.message", AssistantMessage { text: message })?;
    events.emit(
        terminal_kind,
        TerminalPayload {
            status,
            model_calls: 0,
            tool_calls: 1,
            duration_ms: elapsed_ms(started_at),
        },
    )
}

#[derive(Serialize)]
struct RunStarted<'a> {
    mode: &'static str,
    workspace: Option<&'a str>,
    instruction_bytes: usize,
}

#[derive(Serialize)]
struct ToolCall {
    call_id: &'static str,
    tool: &'static str,
    arguments: ToolArguments,
}

#[derive(Serialize)]
struct ToolArguments {
    command: &'static str,
    cwd: &'static str,
    timeout_sec: u64,
}

#[derive(Serialize)]
struct ToolResult<'a> {
    call_id: &'static str,
    tool: &'static str,
    status: &'static str,
    return_code: Option<i32>,
    stdout: &'a str,
    stderr: &'a str,
    duration_ns: u64,
}

#[derive(Serialize)]
struct AssistantMessage {
    text: &'static str,
}

#[derive(Serialize)]
struct TerminalPayload {
    status: &'static str,
    model_calls: u32,
    tool_calls: u32,
    duration_ms: u64,
}
