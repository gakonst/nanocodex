use serde_json::{Value, json};

use super::wire::Usage;

const SOL_CONTEXT_WINDOW: u64 = 372_000;
const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

pub(super) const fn auto_compact_token_limit(model: &str) -> Option<u64> {
    if const_str_eq(model, "gpt-5.6-sol") {
        Some((SOL_CONTEXT_WINDOW * 9) / 10)
    } else {
        None
    }
}

pub(super) fn active_context_tokens(usage: &Usage, local_items: &[Value]) -> u64 {
    let reported = usage
        .total_tokens
        .max(usage.input_tokens.saturating_add(usage.output_tokens));
    local_items.iter().fold(reported, |total, item| {
        total.saturating_add(u64::try_from(approx_tokens(serialized_len(item))).unwrap_or(u64::MAX))
    })
}

pub(super) fn trigger() -> Value {
    json!({ "type": "compaction_trigger" })
}

pub(super) fn install_history(
    history: &[Value],
    initial_context: &Value,
    mut compaction: Value,
) -> Vec<Value> {
    if compaction.get("type").and_then(Value::as_str) == Some("compaction_summary") {
        if let Some(object) = compaction.as_object_mut() {
            object.insert("type".to_owned(), Value::String("compaction".to_owned()));
        }
    }
    if let Some(object) = compaction.as_object_mut() {
        object.remove("id");
    }

    let retained = history
        .iter()
        .filter(|item| is_real_user_message(item, initial_context))
        .cloned()
        .collect();
    let mut installed = truncate_retained_messages(retained, RETAINED_MESSAGE_TOKEN_BUDGET);
    let insertion = installed
        .iter()
        .rposition(is_user_message)
        .unwrap_or(installed.len());
    installed.insert(insertion, initial_context.clone());
    installed.push(compaction);
    installed
}

fn is_real_user_message(item: &Value, initial_context: &Value) -> bool {
    item != initial_context && is_user_message(item)
}

fn is_user_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("user")
}

fn truncate_retained_messages(items: Vec<Value>, max_tokens: usize) -> Vec<Value> {
    let mut remaining = max_tokens;
    let mut reversed = Vec::with_capacity(items.len());
    for item in items.into_iter().rev() {
        if remaining == 0 {
            continue;
        }
        let cost = message_text_token_count(&item).max(1);
        if cost <= remaining {
            reversed.push(item);
            remaining = remaining.saturating_sub(cost);
        } else if let Some(item) = truncate_message(item, remaining) {
            reversed.push(item);
            remaining = 0;
        }
    }
    reversed.reverse();
    reversed
}

fn message_text_token_count(item: &Value) -> usize {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text") => part.get("text").and_then(Value::as_str),
            _ => None,
        })
        .map(|text| approx_tokens(text.len()))
        .sum()
}

fn truncate_message(mut item: Value, max_tokens: usize) -> Option<Value> {
    let content = item.get_mut("content")?.as_array_mut()?;
    let mut remaining = max_tokens;
    let mut truncated = Vec::with_capacity(content.len());
    for mut part in std::mem::take(content) {
        match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text") => {
                if remaining == 0 {
                    continue;
                }
                let Some(text) = part.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let cost = approx_tokens(text.len());
                if cost <= remaining {
                    remaining = remaining.saturating_sub(cost);
                } else {
                    let text = truncate_middle_with_token_budget(text, remaining);
                    let Some(object) = part.as_object_mut() else {
                        continue;
                    };
                    object.insert("text".to_owned(), Value::String(text));
                    remaining = 0;
                }
                if part
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
                {
                    truncated.push(part);
                }
            }
            _ => truncated.push(part),
        }
    }
    if truncated.is_empty() {
        return None;
    }
    *content = truncated;
    Some(item)
}

fn truncate_middle_with_token_budget(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if max_tokens > 0 && text.len() <= max_bytes {
        return text.to_owned();
    }
    if max_bytes == 0 {
        return format!("…{} tokens truncated…", approx_tokens(text.len()));
    }

    let left_budget = max_bytes / 2;
    let right_budget = max_bytes - left_budget;
    let prefix_end = floor_char_boundary(text, left_budget);
    let suffix_target = text.len().saturating_sub(right_budget);
    let suffix_start = ceil_char_boundary(text, suffix_target).max(prefix_end);
    let removed = approx_tokens(text.len().saturating_sub(max_bytes));
    format!(
        "{}…{removed} tokens truncated…{}",
        &text[..prefix_end],
        &text[suffix_start..]
    )
}

fn floor_char_boundary(text: &str, target: usize) -> usize {
    let mut boundary = target.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    boundary
}

fn ceil_char_boundary(text: &str, target: usize) -> usize {
    let mut boundary = target.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary = boundary.saturating_add(1);
    }
    boundary
}

fn serialized_len(item: &Value) -> usize {
    serde_json::to_vec(item).map_or(0, |encoded| encoded.len())
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

const fn const_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sol_compacts_at_ninety_percent_of_its_context_window() {
        assert_eq!(auto_compact_token_limit("gpt-5.6-sol"), Some(334_800));
        assert_eq!(auto_compact_token_limit("unknown-model"), None);
    }

    #[test]
    fn installed_history_retains_user_input_and_reinjects_initial_context() {
        let initial = message("initial context");
        let task = message("do the task");
        let history = vec![
            initial.clone(),
            task.clone(),
            json!({ "type": "reasoning", "encrypted_content": "old" }),
            json!({ "type": "custom_tool_call", "call_id": "call-1", "name": "exec", "input": "text(1)" }),
            json!({ "type": "custom_tool_call_output", "call_id": "call-1", "output": "done" }),
        ];

        assert_eq!(
            install_history(
                &history,
                &initial,
                json!({ "id": "cmp-id", "type": "compaction", "encrypted_content": "opaque" }),
            ),
            vec![
                initial,
                task,
                json!({ "type": "compaction", "encrypted_content": "opaque" }),
            ]
        );
    }

    fn message(text: &str) -> Value {
        json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
        })
    }
}
