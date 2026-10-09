//! Codex-compatible JSONL mirror of a Claude session.
//!
//! Durable state remains the source of truth; this mirror records every
//! committed turn as Responses-shaped items (user and assistant text, tool
//! calls and their outputs) beneath `CODEX_HOME/sessions`, with session
//! metadata carrying the session's origin, parent and root.
use super::*;
use nanocodex_agent::rollout::{
    RolloutConfig, RolloutInfo, RolloutSession, RolloutTurnRecord, RolloutWriter,
};
use nanocodex_oai_api::responses::ResponseItem;

pub(super) struct Mirror {
    writer: RolloutWriter,
    // Summary recorded by the last commit; a different summary means local
    // compaction replaced history, which the rollout records as such.
    summary: std::sync::Mutex<String>,
}

impl Mirror {
    /// Creates the rollout, or reopens it when continuing a recorded session.
    pub(super) fn open(
        config: &RolloutConfig,
        session: &RolloutSession,
        conversation: &Conversation,
    ) -> std::io::Result<Self> {
        let continuing = !conversation.messages.is_empty() || !conversation.summary.is_empty();
        let recorded = continuing
            .then(|| config.load_session(&session.session_id).ok())
            .flatten();
        let (writer, summary) = match recorded {
            Some(recorded) => {
                let (_, _, resumed) = recorded.into_parts();
                let history = history(conversation).len();
                (
                    RolloutWriter::resume(&resumed, session, history)?,
                    conversation.summary.clone(),
                )
            }
            None => (RolloutWriter::create(config, session)?, String::new()),
        };
        Ok(Self {
            writer,
            summary: std::sync::Mutex::new(summary),
        })
    }

    pub(super) fn info(&self) -> RolloutInfo {
        self.writer.info().clone()
    }

    /// Records one settled turn. Failures are retried by the next commit or
    /// by [`Self::flush`], which reports them.
    pub(super) async fn commit(
        &self,
        turn: RolloutTurnRecord,
        model: HarnessModel,
        conversation: &Conversation,
    ) -> std::io::Result<()> {
        let items = history(conversation);
        let compacted = {
            let mut summary = self.summary.lock().expect("rollout summary lock");
            let changed = *summary != conversation.summary;
            if changed {
                summary.clone_from(&conversation.summary);
            }
            changed
        };
        if compacted {
            self.writer.commit_compaction(turn, model, items).await
        } else {
            self.writer.commit(turn, model, items).await
        }
    }

    pub(super) async fn flush(&self) -> std::io::Result<()> {
        self.writer.flush().await
    }

    pub(super) async fn shutdown(&self) -> std::io::Result<()> {
        self.writer.shutdown().await
    }
}

/// Model-visible Claude history as Responses-shaped rollout items. Signed
/// thinking, server-tool payloads and binary media are not mirrored.
fn history(conversation: &Conversation) -> Vec<ResponseItem> {
    let mut items = Vec::new();
    if !conversation.summary.is_empty() {
        items.push(message(
            Role::Assistant,
            &format!("Retained conversation summary:\n{}", conversation.summary),
        ));
    }
    for entry in &conversation.messages {
        for block in &entry.content {
            match block {
                ContentBlock::Text { text, .. } => items.push(message(entry.role, text)),
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => items.push(json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": input.to_string(),
                })),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => items.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": tool_output(content),
                })),
                _ => {}
            }
        }
    }
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

fn message(role: Role, text: &str) -> Value {
    let (role, kind) = match role {
        Role::User => ("user", "input_text"),
        Role::Assistant => ("assistant", "output_text"),
    };
    json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]})
}

fn tool_output(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                Some(kind) => format!("[{kind}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
