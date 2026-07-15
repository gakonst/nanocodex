use std::{
    io::{self, BufRead, Write},
    process::ExitCode,
    time::Instant,
};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Parser)]
#[command(version, about = "A Harbor-first OpenAI coding harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read one task request as JSONL from stdin and stream JSONL events to stdout.
    Run,
}

#[derive(Debug, Deserialize)]
struct InputEnvelope {
    // is this necessary?
    protocol_version: u32,
    // is this necessary?
    request_id: String,
    // is this necessary?
    seq: u64,
    // is this necessary?
    #[serde(rename = "type")]
    kind: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct TaskStart {
    instruction: String,
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Debug, Serialize)]
struct OutputEnvelope<'a> {
    // is this necessary?
    protocol_version: u32,
    // is this necessary?
    request_id: &'a str,
    // is this necessary?
    seq: u64,
    // is this necessary?
    #[serde(rename = "type")]
    kind: &'a str,
    payload: Value,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run => run(io::stdin().lock(), io::stdout().lock()),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("harness: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(mut input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    let mut line = String::new();
    loop {
        let bytes_read = input
            .read_line(&mut line)
            .map_err(|error| format!("failed to read stdin: {error}"))?;
        if bytes_read == 0 {
            return Err("stdin ended before task.start".to_owned());
        }
        if !line.trim().is_empty() {
            break;
        }
        line.clear();
    }

    let envelope: InputEnvelope =
        serde_json::from_str(&line).map_err(|error| format!("invalid input JSONL: {error}"))?;
    validate_envelope(&envelope)?;

    let task: TaskStart = serde_json::from_value(envelope.payload)
        .map_err(|error| format!("invalid task.start payload: {error}"))?;
    if task.instruction.trim().is_empty() {
        return Err("task.start instruction must not be empty".to_owned());
    }

    let started_at = Instant::now();

    emit(
        &mut output,
        &OutputEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: &envelope.request_id,
            seq: 1,
            kind: "run.started",
            payload: json!({
                "mode": "phase0_no_model",
                "workspace": task.workspace,
                "instruction_bytes": task.instruction.len(),
            }),
        },
    )?;

    emit(
        &mut output,
        &OutputEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: &envelope.request_id,
            seq: 2,
            kind: "assistant.message",
            payload: json!({
                "text": "Phase 0 transport probe completed; no model or tools were run."
            }),
        },
    )?;

    emit(
        &mut output,
        &OutputEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: &envelope.request_id,
            seq: 3,
            kind: "run.completed",
            payload: json!({
                "status": "not_attempted",
                "model_calls": 0,
                "tool_calls": 0,
                "duration_ms": u64::try_from(started_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
            }),
        },
    )?;

    Ok(())
}

fn validate_envelope(envelope: &InputEnvelope) -> Result<(), String> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "unsupported protocol_version {}; expected {PROTOCOL_VERSION}",
            envelope.protocol_version
        ));
    }
    if envelope.request_id.trim().is_empty() {
        return Err("request_id must not be empty".to_owned());
    }
    if envelope.seq != 1 {
        return Err(format!("task.start seq must be 1; got {}", envelope.seq));
    }
    if envelope.kind != "task.start" {
        return Err(format!(
            "first input message must be task.start; got {}",
            envelope.kind
        ));
    }
    Ok(())
}

fn emit(output: &mut impl Write, envelope: &OutputEnvelope<'_>) -> Result<(), String> {
    serde_json::to_writer(&mut *output, &envelope)
        .map_err(|error| format!("failed to encode stdout event: {error}"))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|error| format!("failed to flush stdout event: {error}"))
}
