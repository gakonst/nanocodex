// Shared pending UI request; independent of CLI configuration and provider setup.
use nanocodex::claude_tools::host::UserQuestion;
use serde_json::{Value, json};
use tokio::sync::oneshot;

pub(crate) struct PendingInteraction {
    pub(crate) id: String,
    pub(crate) question: Option<UserQuestion>,
    pub(crate) prompt: String,
    pub(crate) reply: Option<oneshot::Sender<Result<Value, String>>>,
}
impl PendingInteraction {
    pub(crate) fn prompt(&self) -> &str {
        &self.prompt
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(oneshot::Sender::is_closed)
    }
    /// Validation leaves the request pending, including empty input. There is
    /// no default answer and only the literal approval action grants exit.
    pub(crate) fn respond(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        let response = if text == "/cancel" {
            Err("user cancelled the request".into())
        } else if let Some(question) = &self.question {
            let answers = if let Some(other) = text.strip_prefix("other:") {
                let other = other.trim();
                if other.is_empty() || other.len() > 8192 {
                    return Err("Enter a nonempty answer of at most 8192 bytes after other:".into());
                }
                vec![other.to_owned()]
            } else {
                let mut answers = Vec::new();
                for part in text.split(',') {
                    let index: usize = part.trim().parse().map_err(
                        |_| "Choose an option number, or use other: followed by your answer",
                    )?;
                    let answer = question
                        .options
                        .get(index.checked_sub(1).ok_or("Options start at 1")?)
                        .ok_or("Option number out of range")?
                        .label
                        .clone();
                    if !answers.contains(&answer) {
                        answers.push(answer);
                    }
                }
                if !question.multi_select && answers.len() != 1 {
                    return Err("Choose exactly one option".into());
                }
                answers
            };
            Ok(json!(answers))
        } else {
            match text {
                "approve" => Ok(json!(true)),
                "deny" => Ok(json!(false)),
                _ => return Err("Type approve or deny; blank input does not approve".into()),
            }
        };
        self.reply
            .take()
            .ok_or("request already answered")?
            .send(response)
            .map_err(|_| "request was cancelled before the answer arrived".to_owned())
    }
}
