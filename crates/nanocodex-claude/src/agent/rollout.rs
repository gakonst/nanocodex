//! Codex-compatible JSONL mirror of a Claude session.
//!
//! Durable state remains the source of truth; this mirror records every
//! committed turn as Responses-shaped items (user and assistant text,
//! reasoning summaries, client and server tool calls and their outputs) beneath `CODEX_HOME/sessions`, with session
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

/// Model-visible Claude history as Responses-shaped rollout items.
///
/// Visible thinking becomes a reasoning summary; provider server tools
/// (web search/fetch, code execution, tool search, MCP connector) become
/// function calls paired with a textual output, so Codex-compatible readers
/// see the same call/result structure as for client tools. Thinking
/// signatures, redacted thinking, encrypted provider payloads and binary
/// media are model-bound and never mirrored.
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
                ContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                    items.push(json!({
                        "type": "reasoning",
                        "summary": [{"type": "summary_text", "text": thinking}],
                    }));
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                }
                | ContentBlock::ServerToolUse {
                    id, name, input, ..
                } => items.push(function_call(id, name, input)),
                ContentBlock::McpToolUse {
                    id,
                    name,
                    server_name,
                    input,
                    ..
                } => items.push(function_call(
                    id,
                    &format!("mcp__{server_name}__{name}"),
                    input,
                )),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => items.push(function_output(tool_use_id, &tool_output(content))),
                ContentBlock::WebSearchToolResult {
                    tool_use_id,
                    content,
                    ..
                }
                | ContentBlock::WebFetchToolResult {
                    tool_use_id,
                    content,
                    ..
                }
                | ContentBlock::ToolSearchToolResult {
                    tool_use_id,
                    content,
                    ..
                }
                | ContentBlock::CodeExecutionToolResult {
                    tool_use_id,
                    content,
                    ..
                }
                | ContentBlock::BashCodeExecutionToolResult {
                    tool_use_id,
                    content,
                    ..
                }
                | ContentBlock::TextEditorCodeExecutionToolResult {
                    tool_use_id,
                    content,
                    ..
                }
                | ContentBlock::McpToolResult {
                    tool_use_id,
                    content,
                    ..
                } => items.push(function_output(tool_use_id, &server_output(content))),
                ContentBlock::Thinking { .. }
                | ContentBlock::RedactedThinking { .. }
                | ContentBlock::McpToolListing { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Document { .. } => {}
            }
        }
    }
    items
        .into_iter()
        .filter_map(|item| serde_json::from_value(item).ok())
        .collect()
}

fn function_call(id: &str, name: &str, input: &Value) -> Value {
    json!({
        "type": "function_call",
        "call_id": id,
        "name": name,
        "arguments": input.to_string(),
    })
}

fn function_output(call_id: &str, output: &str) -> Value {
    json!({"type": "function_call_output", "call_id": call_id, "output": output})
}

/// Readable text of a server-tool result. Encrypted search content, fetched
/// document bodies and other opaque payloads are summarized, not copied.
fn server_output(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(server_output)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(part) => {
            let field = |key: &str| part.get(key).and_then(Value::as_str).unwrap_or_default();
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                return text.to_owned();
            }
            let kind = field("type");
            if kind == "web_search_result" {
                return format!("{} <{}>", field("title"), field("url"));
            }
            if kind.ends_with("_error") {
                return format!("[{kind}: {}]", field("error_code"));
            }
            let mut lines = Vec::new();
            for key in ["url", "title", "stdout", "stderr"] {
                if !field(key).is_empty() {
                    lines.push(field(key).to_owned());
                }
            }
            if let Some(code) = part.get("return_code").and_then(Value::as_i64) {
                lines.push(format!("exit code {code}"));
            }
            if let Some(references) = part.get("tool_references").and_then(Value::as_array) {
                let names = references
                    .iter()
                    .filter_map(|reference| reference.get("tool_name").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                lines.push(format!("tools: {}", names.join(", ")));
            }
            if let Some(inner) = part.get("content").filter(|_| kind != "document") {
                let inner = server_output(inner);
                if !inner.is_empty() {
                    lines.push(inner);
                }
            }
            if lines.is_empty() && !kind.is_empty() {
                lines.push(format!("[{kind}]"));
            }
            lines.join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
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
