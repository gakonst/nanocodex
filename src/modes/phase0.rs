use std::{io::Write, time::Instant};

use serde::Serialize;

use super::elapsed_ms;
use crate::{
    Result,
    protocol::{EventWriter, Task},
};

pub(super) fn run<W: Write>(events: &mut EventWriter<W>, task: &Task) -> Result<()> {
    let started_at = Instant::now();
    events.emit(
        "run.started",
        RunStarted {
            mode: "phase0_no_model",
            workspace: task.workspace.as_deref(),
            instruction_bytes: task.instruction.len(),
        },
    )?;
    events.emit(
        "assistant.message",
        AssistantMessage {
            text: "Phase 0 transport probe completed; no model or tools were run.",
        },
    )?;
    events.emit(
        "run.completed",
        TerminalPayload {
            status: "not_attempted",
            model_calls: 0,
            tool_calls: 0,
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
