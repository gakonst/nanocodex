use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use crate::{HarnessError, Result};

const VERSION: u32 = 1;
const MAX_USER_INPUT_TEXT_CHARS: usize = 1 << 20;

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
    pub(crate) instruction: TaskInstruction,
    #[serde(default)]
    pub(crate) workspace: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum TaskInstruction {
    Text(String),
    Content(Vec<UserInput>),
}

impl TaskInstruction {
    pub(crate) fn text_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Content(items) => items.iter().map(UserInput::text_bytes).sum(),
        }
    }

    fn text_chars(&self) -> usize {
        match self {
            Self::Text(text) => text.chars().count(),
            Self::Content(items) => items.iter().map(UserInput::text_chars).sum(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Content(items) => items.is_empty() || items.iter().all(UserInput::is_empty),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum UserInput {
    Text {
        text: String,
    },
    Image {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    LocalImage {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    Audio {
        audio_url: String,
    },
    LocalAudio {
        path: PathBuf,
    },
}

impl UserInput {
    fn text_bytes(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => 0,
        }
    }

    fn text_chars(&self) -> usize {
        match self {
            Self::Text { text } => text.chars().count(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => 0,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Image { .. }
            | Self::LocalImage { .. }
            | Self::Audio { .. }
            | Self::LocalAudio { .. } => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
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
    if request.payload.instruction.is_empty() {
        return Err(HarnessError::InvalidRequest(
            "task.start instruction must not be empty".to_owned(),
        ));
    }
    let text_chars = request.payload.instruction.text_chars();
    if text_chars > MAX_USER_INPUT_TEXT_CHARS {
        return Err(HarnessError::InvalidRequest(format!(
            "task.start instruction exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters ({text_chars} provided)"
        )));
    }
    Ok(TaskRequest {
        request_id: request.request_id,
        task: request.payload,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn task_start_accepts_ordered_codex_user_input() {
        let input = concat!(
            r#"{"protocol_version":1,"request_id":"multimodal","seq":1,"type":"task.start","payload":{"instruction":[{"type":"local_image","path":"/tmp/image.png","detail":"original"},{"type":"text","text":"describe it"},{"type":"audio","audio_url":"data:audio/wav;base64,AAAA"}],"workspace":"/tmp"}}"#,
            "\n",
        );
        let request = read_task_start(Cursor::new(input)).expect("decode structured task input");

        let TaskInstruction::Content(input) = request.task.instruction else {
            panic!("expected structured content");
        };
        assert!(matches!(
            input.as_slice(),
            [
                UserInput::LocalImage {
                    detail: Some(ImageDetail::Original),
                    ..
                },
                UserInput::Text { text },
                UserInput::Audio { .. },
            ] if text == "describe it"
        ));
    }
}
