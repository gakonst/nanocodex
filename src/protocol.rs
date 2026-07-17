use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::{HarnessError, Result};

const VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputEnvelope {
    protocol_version: u32,
    request_id: String,
    seq: u64,
    #[serde(rename = "type")]
    kind: String,
    payload: Task,
}

#[derive(Serialize)]
struct OutputEnvelope<'a, P> {
    // is this neeedd?
    protocol_version: u32,
    request_id: &'a str,
    seq: u64,
    #[serde(rename = "type")]
    kind: &'a str,
    payload: P,
}

pub(crate) struct TaskRequest {
    pub(crate) request_id: String,
    pub(crate) task: Task,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Task {
    pub(crate) instruction: String,
    #[serde(default)]
    pub(crate) workspace: Option<String>,
}

pub(crate) struct EventWriter<W> {
    output: W,
    request_id: String,
    next_seq: u64,
}

impl<W: Write> EventWriter<W> {
    pub(crate) fn new(output: W, request_id: String) -> Self {
        Self {
            output,
            request_id,
            next_seq: 1,
        }
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn emit<P: Serialize>(&mut self, kind: &str, payload: P) -> Result<()> {
        let envelope = OutputEnvelope {
            protocol_version: VERSION,
            request_id: &self.request_id,
            seq: self.next_seq,
            kind,
            payload,
        };
        serde_json::to_writer(&mut self.output, &envelope).map_err(HarnessError::EncodeOutput)?;
        self.output
            .write_all(b"\n")
            .and_then(|()| self.output.flush())
            .map_err(HarnessError::WriteOutput)?;
        self.next_seq += 1;
        Ok(())
    }
}

pub(crate) fn read_task_start(mut input: impl BufRead) -> Result<TaskRequest> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = input
            .read_line(&mut line)
            .map_err(HarnessError::ReadInput)?;
        if bytes_read == 0 {
            return Err(HarnessError::InvalidRequest(
                "stdin ended before task.start".to_owned(),
            ));
        }
        if !line.trim().is_empty() {
            break;
        }
    }

    let request: InputEnvelope = serde_json::from_str(&line).map_err(HarnessError::DecodeInput)?;
    if request.protocol_version != VERSION {
        return Err(HarnessError::InvalidRequest(format!(
            "unsupported protocol_version {}; expected {VERSION}",
            request.protocol_version
        )));
    }
    if request.request_id.trim().is_empty() {
        return Err(HarnessError::InvalidRequest(
            "request_id must not be empty".to_owned(),
        ));
    }
    if request.seq != 1 || request.kind != "task.start" {
        return Err(HarnessError::InvalidRequest(
            "first input event must be task.start with seq 1".to_owned(),
        ));
    }
    if request.payload.instruction.trim().is_empty() {
        return Err(HarnessError::InvalidRequest(
            "task.start instruction must not be empty".to_owned(),
        ));
    }
    Ok(TaskRequest {
        request_id: request.request_id,
        task: request.payload,
    })
}
