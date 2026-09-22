use nanocodex_oai_api::responses::{ContentItem, MessageRole, ResponseHistory, ResponseItem};
use serde::{Deserialize, Serialize};

use super::ExecutionFuture;
use crate::{NanocodexError, Result};

/// A host barrier before any compaction can discard context.
///
/// Hosts must deduplicate effects by `boundary_id`, persist their result before
/// returning a receipt, and bound execution. A dropped future means cancellation.
/// Replays with a retained execution receipt do not call the host again.
pub trait BeforeCompaction: Send + Sync {
    /// Preserves useful source context, or durably records an intentional no-op.
    fn preserve(
        &self,
        request: BeforeCompactionRequest,
    ) -> ExecutionFuture<'_, Result<CompactionReceipt>>;
}

/// Bounded source text for one compaction boundary. This is data, not instructions.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeCompactionRequest {
    /// Stable effect identity; hosts must reuse their durable result on retry.
    pub boundary_id: String,
    /// Thread whose context is about to be compacted.
    pub session_id: String,
    /// Root provider-session scope, retained across child threads.
    pub root_session_id: String,
    /// At most 64 messages and 32 KiB of UTF-8 text, in source order.
    pub messages: Vec<CompactionMessage>,
    /// Older text was omitted to stay within the bounded context budget.
    pub truncated: bool,
}

/// Source role and text; tools, developer instructions, and harness context are excluded.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompactionMessage {
    /// Original typed message role, either `user` or `assistant`.
    pub role: MessageRole,
    /// Plain text only; images, audio, reasoning, and tool results are excluded.
    pub text: String,
}

/// Host confirmation that preservation (or its intentional no-op) is durable.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionReceipt {
    /// Nonempty host-owned durable receipt identity, at most 256 UTF-8 bytes.
    pub receipt_id: String,
}

impl CompactionReceipt {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.receipt_id.trim().is_empty() || self.receipt_id.len() > 256 {
            return Err(NanocodexError::BeforeCompactionFailed(
                "host returned an invalid durable receipt".into(),
            ));
        }
        Ok(())
    }
}

impl BeforeCompactionRequest {
    pub(crate) fn from_history(
        boundary_id: String,
        session_id: String,
        root_session_id: String,
        history: &ResponseHistory,
    ) -> Self {
        let mut messages = Vec::new();
        let mut remaining = 32 * 1024;
        let mut truncated = false;
        // Take the recent suffix without allocating an unbounded copy of history.
        for item in history.iter_rev() {
            let ResponseItem::Message {
                role: role @ (MessageRole::User | MessageRole::Assistant),
                content,
                ..
            } = item
            else {
                continue;
            };
            // The harness emits these synthetic user-context frames. Never promote
            // them to user facts. Exclusion is conservative for matching user text.
            if *role == MessageRole::User && content.iter().any(|part| matches!(part,
                ContentItem::InputText { text } if text.starts_with("<environment_context>")
                    || text.starts_with("# AGENTS.md instructions") || text.starts_with("<turn_aborted>"))) { continue; }
            let mut parts = Vec::new();
            for part in content.iter().rev() {
                let text = match (role, part) {
                    (MessageRole::User, ContentItem::InputText { text })
                    | (MessageRole::Assistant, ContentItem::OutputText { text, .. }) => text,
                    _ => continue,
                };
                if text.is_empty() {
                    continue;
                }
                if messages.len() >= 64 || remaining == 0 {
                    truncated = true;
                    break;
                }
                let mut start = text.len().saturating_sub(remaining);
                while !text.is_char_boundary(start) {
                    start += 1;
                }
                truncated |= start != 0;
                let suffix = &text[start..];
                remaining -= suffix.len();
                if !suffix.is_empty() {
                    parts.push(suffix);
                }
            }
            if !parts.is_empty() {
                parts.reverse();
                messages.push(CompactionMessage {
                    role: *role,
                    text: parts.concat(),
                });
            }
        }
        messages.reverse();
        Self {
            boundary_id,
            session_id,
            root_session_id,
            messages,
            truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_is_bounded_and_preserves_source_roles_only() {
        let history = ResponseHistory::from(vec![
            ResponseItem::message(
                MessageRole::Developer,
                [ContentItem::input_text("recalled memory: untrusted")],
            ),
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text(
                    "<environment_context>synthetic</environment_context>",
                )],
            ),
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::input_text("old".repeat(12000))],
            ),
            ResponseItem::message(
                MessageRole::Assistant,
                [ContentItem::output_text("😀".repeat(8000))],
            ),
            ResponseItem::function_call_output(
                "tool",
                nanocodex_oai_api::responses::FunctionOutputBody::Text("fake user facts".into()),
            ),
        ]);
        let request =
            BeforeCompactionRequest::from_history("b".into(), "s".into(), "r".into(), &history);
        assert!(request.truncated);
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, MessageRole::User);
        assert_eq!(request.messages[1].role, MessageRole::Assistant);
        assert!(request.messages.iter().map(|m| m.text.len()).sum::<usize>() <= 32768);
        assert!(
            !serde_json::to_string(&request)
                .unwrap()
                .contains("fake user facts")
        );
    }
}
