use std::collections::HashSet;

use serde_json::{Value, json};

const TOOL_OUTPUT_TOKEN_LIMIT: usize = 12_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

/// The model-visible transcript. Stored items remain unchanged after insertion;
/// prompt-only repairs are applied to a clone when a full history is replayed.
pub(super) struct ContextManager {
    items: Vec<Value>,
}

impl ContextManager {
    pub(super) fn new(items: Vec<Value>) -> Self {
        let mut history = Self { items: Vec::new() };
        history.record_items(items);
        history
    }

    pub(super) fn raw_items(&self) -> &[Value] {
        &self.items
    }

    pub(super) fn items_from(&self, start: usize) -> &[Value] {
        &self.items[start..]
    }

    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    pub(super) fn record_items(&mut self, items: impl IntoIterator<Item = Value>) {
        self.items.extend(
            items
                .into_iter()
                .filter(is_api_item)
                .map(truncate_tool_output),
        );
    }

    pub(super) fn replace(&mut self, items: Vec<Value>) {
        self.items = items;
    }

    pub(super) fn for_prompt(&self) -> Vec<Value> {
        let mut items = self.items.clone();
        ensure_call_outputs_present(&mut items);
        remove_orphan_outputs(&mut items);
        items
    }

    pub(super) fn replace_last_turn_images(&mut self, placeholder: &str) -> bool {
        for item in self.items.iter_mut().rev() {
            if item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
            {
                return false;
            }
            if !matches!(
                item.get("type").and_then(Value::as_str),
                Some("custom_tool_call_output" | "function_call_output")
            ) {
                continue;
            }
            let Some(output) = item.get_mut("output").and_then(Value::as_array_mut) else {
                continue;
            };
            let mut replaced = false;
            for content in output {
                if content.get("type").and_then(Value::as_str) == Some("input_image") {
                    *content = json!({
                        "type": "input_text",
                        "text": placeholder,
                    });
                    replaced = true;
                }
            }
            if replaced {
                return true;
            }
        }
        false
    }
}

fn is_api_item(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => item.get("role").and_then(Value::as_str) != Some("system"),
        Some(
            "additional_tools"
            | "agent_message"
            | "function_call_output"
            | "function_call"
            | "tool_search_call"
            | "tool_search_output"
            | "custom_tool_call"
            | "custom_tool_call_output"
            | "local_shell_call"
            | "reasoning"
            | "web_search_call"
            | "image_generation_call"
            | "compaction"
            | "context_compaction",
        ) => true,
        _ => false,
    }
}

fn truncate_tool_output(mut item: Value) -> Value {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call_output" | "custom_tool_call_output")
    ) {
        return item;
    }
    let Some(output) = item.get_mut("output") else {
        return item;
    };
    match output {
        Value::String(text) => {
            *text = truncate_middle_with_token_budget(text, TOOL_OUTPUT_TOKEN_LIMIT);
        }
        Value::Array(items) => truncate_output_content(items, TOOL_OUTPUT_TOKEN_LIMIT),
        _ => {}
    }
    item
}

fn truncate_output_content(items: &mut Vec<Value>, token_limit: usize) {
    let mut remaining = token_limit;
    let mut omitted_text_items = 0usize;
    let mut output = Vec::with_capacity(items.len());
    for mut item in std::mem::take(items) {
        match item.get("type").and_then(Value::as_str) {
            Some("input_text") => {
                let Some(text) = item.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if remaining == 0 {
                    omitted_text_items += 1;
                    continue;
                }
                let tokens = approx_tokens(text.len());
                if tokens <= remaining {
                    remaining -= tokens;
                    output.push(item);
                } else {
                    let text = truncate_middle_with_token_budget(text, remaining);
                    if text.is_empty() {
                        omitted_text_items += 1;
                    } else if let Some(slot) = item.get_mut("text") {
                        *slot = Value::String(text);
                        output.push(item);
                    }
                    remaining = 0;
                }
            }
            Some("input_audio") => {}
            _ => output.push(item),
        }
    }
    if omitted_text_items > 0 {
        output.push(json!({
            "type": "input_text",
            "text": format!("[omitted {omitted_text_items} text items ...]"),
        }));
    }
    *items = output;
}

fn ensure_call_outputs_present(items: &mut Vec<Value>) {
    let function_outputs = call_ids(items, "function_call_output");
    let custom_outputs = call_ids(items, "custom_tool_call_output");
    let tool_search_outputs = call_ids(items, "tool_search_output");
    let mut missing = Vec::new();

    for (index, item) in items.iter().enumerate() {
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        let output = match item.get("type").and_then(Value::as_str) {
            Some("function_call" | "local_shell_call") if !function_outputs.contains(call_id) => {
                Some(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": "aborted",
                }))
            }
            Some("custom_tool_call") if !custom_outputs.contains(call_id) => Some(json!({
                "type": "custom_tool_call_output",
                "call_id": call_id,
                "output": "aborted",
            })),
            Some("tool_search_call") if !tool_search_outputs.contains(call_id) => Some(json!({
                "type": "tool_search_output",
                "call_id": call_id,
                "status": "completed",
                "execution": "client",
                "tools": [],
            })),
            _ => None,
        };
        if let Some(output) = output {
            missing.push((index, output));
        }
    }

    for (index, output) in missing.into_iter().rev() {
        items.insert(index + 1, output);
    }
}

fn remove_orphan_outputs(items: &mut Vec<Value>) {
    let function_calls = call_ids(items, "function_call");
    let local_shell_calls = call_ids(items, "local_shell_call");
    let custom_calls = call_ids(items, "custom_tool_call");
    let tool_search_calls = call_ids(items, "tool_search_call");

    items.retain(|item| {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return true;
        };
        let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
            return true;
        };
        match item_type {
            "function_call_output" => {
                function_calls.contains(call_id) || local_shell_calls.contains(call_id)
            }
            "custom_tool_call_output" => custom_calls.contains(call_id),
            "tool_search_output"
                if item.get("execution").and_then(Value::as_str) == Some("server") =>
            {
                true
            }
            "tool_search_output" => tool_search_calls.contains(call_id),
            _ => true,
        }
    });
}

fn call_ids(items: &[Value], item_type: &str) -> HashSet<String> {
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some(item_type))
        .filter_map(|item| {
            item.get("call_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn truncate_middle_with_token_budget(text: &str, max_tokens: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if max_tokens > 0 && text.len() <= max_bytes {
        return text.to_owned();
    }
    let left_budget = max_bytes / 2;
    let right_budget = max_bytes.saturating_sub(left_budget);
    let prefix_end = floor_char_boundary(text, left_budget);
    let suffix_start =
        ceil_char_boundary(text, text.len().saturating_sub(right_budget)).max(prefix_end);
    let removed_tokens = approx_tokens(suffix_start.saturating_sub(prefix_end));
    format!(
        "{}…{removed_tokens} tokens truncated…{}",
        &text[..prefix_end],
        &text[suffix_start..]
    )
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ContextManager;

    #[test]
    fn stores_only_codex_api_items() {
        let history = ContextManager::new(vec![
            json!({ "type": "message", "role": "system", "content": [] }),
            json!({ "type": "message", "role": "user", "content": [] }),
            json!({ "type": "compaction_trigger" }),
            json!({ "type": "future_unknown" }),
        ]);
        assert_eq!(history.raw_items().len(), 1);
        assert_eq!(history.raw_items()[0]["role"], "user");
    }

    #[test]
    fn prompt_repairs_do_not_mutate_raw_history() {
        let history = ContextManager::new(vec![
            json!({
                "type": "custom_tool_call",
                "call_id": "call-missing",
                "name": "exec",
                "input": "code",
            }),
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call-orphan",
                "output": "unused",
            }),
        ]);

        let prompt = history.for_prompt();
        assert_eq!(history.raw_items().len(), 2);
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[1]["call_id"], "call-missing");
        assert_eq!(prompt[1]["output"], "aborted");
    }

    #[test]
    fn history_truncates_tool_text_but_preserves_images() {
        let history = ContextManager::new(vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call",
            "output": [
                { "type": "input_text", "text": "x".repeat(48_004) },
                { "type": "input_image", "image_url": "data:image/png;base64,a" },
                { "type": "input_text", "text": "omitted" },
            ],
        })]);
        let output = history.raw_items()[0]["output"].as_array().unwrap();
        assert!(
            output[0]["text"]
                .as_str()
                .unwrap()
                .contains("tokens truncated")
        );
        assert_eq!(output[1]["type"], "input_image");
        assert_eq!(output[2]["text"], "[omitted 1 text items ...]");
    }
}
