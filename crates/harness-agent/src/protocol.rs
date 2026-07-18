use std::io::BufRead;

use serde::Deserialize;

use crate::{HarnessError, Prompt, Result};

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

pub(crate) struct TaskRequest {
    pub(crate) request_id: String,
    pub(crate) task: Prompt,
}

type Task = Prompt;

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

    use harness_core::{ImageDetail, PromptInput, UserInput};

    use super::*;

    #[test]
    fn task_start_accepts_ordered_multimodal_input() {
        let input = concat!(
            r#"{"protocol_version":1,"request_id":"multimodal","seq":1,"type":"task.start","payload":{"instruction":[{"type":"local_image","path":"/tmp/image.png","detail":"original"},{"type":"text","text":"describe it"},{"type":"audio","audio_url":"data:audio/wav;base64,AAAA"}],"workspace":"/tmp"}}"#,
            "\n",
        );
        let request = read_task_start(Cursor::new(input)).expect("decode structured task input");

        let PromptInput::Content(input) = request.task.instruction else {
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
