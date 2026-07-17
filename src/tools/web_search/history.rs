use serde_json::Value;

const ASSISTANT_CONTEXT_TOKEN_LIMIT: usize = 1_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;

pub(super) fn recent_input(history: &[Value]) -> Option<Vec<Value>> {
    let mut messages = history
        .iter()
        .filter_map(visible_message)
        .collect::<Vec<_>>();
    let latest_user = messages.iter().rposition(is_user_message)?;
    messages.truncate(latest_user + 1);
    let first_retained = messages
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, item)| is_user_message(item))
        .take(2)
        .last()
        .map_or(latest_user, |(index, _)| index);
    messages.drain(..first_retained);
    truncate_assistant_text(&mut messages, ASSISTANT_CONTEXT_TOKEN_LIMIT);
    (!messages.is_empty()).then_some(messages)
}

fn visible_message(item: &Value) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    match item.get("role").and_then(Value::as_str) {
        Some("assistant") => {
            let mut message = item.clone();
            message.as_object_mut()?.remove("id");
            Some(message)
        }
        Some("user") => visible_user_message(item),
        _ => None,
    }
}

fn visible_user_message(item: &Value) -> Option<Value> {
    let content = item
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
        .cloned()
        .collect::<Vec<_>>();
    if content.is_empty() || content.iter().all(is_context_item) {
        return None;
    }
    let mut message = item.clone();
    let object = message.as_object_mut()?;
    object.remove("id");
    object.insert("content".to_owned(), Value::Array(content));
    Some(message)
}

fn is_context_item(item: &Value) -> bool {
    item.get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| {
            let text = text.trim_start();
            text.starts_with("# AGENTS.md instructions for ")
                || text.starts_with("<environment_context>")
        })
}

fn is_user_message(item: &Value) -> bool {
    item.get("role").and_then(Value::as_str) == Some("user")
}

fn truncate_assistant_text(messages: &mut Vec<Value>, max_tokens: usize) {
    let mut remaining = max_tokens;
    messages.retain_mut(|message| {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return true;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            return false;
        };
        content.retain_mut(|part| {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                return true;
            }
            if remaining == 0 {
                return false;
            }
            let Some(text) = part.get("text").and_then(Value::as_str) else {
                return false;
            };
            let tokens = approx_tokens(text.len());
            if tokens <= remaining {
                remaining -= tokens;
                return true;
            }
            let truncated = truncate_middle(text, remaining);
            let Some(object) = part.as_object_mut() else {
                return false;
            };
            object.insert("text".to_owned(), Value::String(truncated));
            remaining = 0;
            true
        });
        !content.is_empty()
    });
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

fn truncate_middle(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(APPROX_BYTES_PER_TOKEN);
    if max_tokens > 0 && text.len() <= max_bytes {
        return text.to_owned();
    }
    if max_bytes == 0 {
        return format!("…{} tokens truncated…", approx_tokens(text.len()));
    }
    let left_end = floor_char_boundary(text, max_bytes / 2);
    let right_start =
        ceil_char_boundary(text, text.len().saturating_sub(max_bytes - max_bytes / 2))
            .max(left_end);
    let removed = approx_tokens(right_start.saturating_sub(left_end));
    format!(
        "{}…{removed} tokens truncated…{}",
        &text[..left_end],
        &text[right_start..]
    )
}

fn floor_char_boundary(text: &str, target: usize) -> usize {
    let mut boundary = target.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn ceil_char_boundary(text: &str, target: usize) -> usize {
    let mut boundary = target.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary += 1;
    }
    boundary
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::recent_input;

    #[test]
    fn keeps_two_visible_user_turns_and_intervening_assistant_text() {
        let history = vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>ignored</environment_context>"}]}),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"previous"}]}),
            json!({"type":"message","id":"server-id","role":"assistant","content":[{"type":"output_text","text":"answer"}]}),
            json!({"type":"function_call","name":"other"}),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"current"},{"type":"input_image","image_url":"data:image/png;base64,a"}]}),
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"current commentary"}]}),
        ];

        assert_eq!(
            recent_input(&history),
            Some(vec![
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"previous"}]}),
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}),
                json!({"type":"message","role":"user","content":[{"type":"input_text","text":"current"}]}),
            ])
        );
    }
}
