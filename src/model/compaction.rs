use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};

use super::wire::Usage;

const SOL_CONTEXT_WINDOW: u64 = 372_000;
const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const RESIZED_IMAGE_BYTES_ESTIMATE: usize = 7_373;
const ORIGINAL_IMAGE_PATCH_SIZE: u32 = 32;
const ORIGINAL_IMAGE_MAX_PATCHES: usize = 10_000;
const ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE: usize = 32;

#[derive(Default)]
struct OriginalImageEstimateCache {
    entries: HashMap<[u8; 20], Option<usize>>,
    order: VecDeque<[u8; 20]>,
}

impl OriginalImageEstimateCache {
    fn get_or_insert_with(
        &mut self,
        key: [u8; 20],
        estimate: impl FnOnce() -> Option<usize>,
    ) -> Option<usize> {
        if let Some(value) = self.entries.get(&key).copied() {
            self.order.retain(|candidate| candidate != &key);
            self.order.push_back(key);
            return value;
        }
        let value = estimate();
        self.entries.insert(key, value);
        self.order.push_back(key);
        while self.entries.len() > ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        value
    }
}

static ORIGINAL_IMAGE_ESTIMATE_CACHE: LazyLock<Mutex<OriginalImageEstimateCache>> =
    LazyLock::new(|| Mutex::new(OriginalImageEstimateCache::default()));

pub(super) fn auto_compact_token_limit(model: &str) -> Option<u64> {
    if model == "gpt-5.6-sol" {
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

    let task = history
        .iter()
        .find(|item| is_real_user_message(item, initial_context))
        .cloned()
        .map(truncate_task);
    let mut installed = Vec::with_capacity(usize::from(task.is_some()) + 2);
    installed.push(initial_context.clone());
    installed.extend(task);
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

fn truncate_task(mut task: Value) -> Value {
    let Some(text) = task.pointer("/content/0/text").and_then(Value::as_str) else {
        return task;
    };
    if approx_tokens(text.len()) <= RETAINED_MESSAGE_TOKEN_BUDGET {
        return task;
    }
    let text = truncate_middle_with_token_budget(text, RETAINED_MESSAGE_TOKEN_BUDGET);
    if let Some(slot) = task.pointer_mut("/content/0/text") {
        *slot = Value::String(text);
    }
    task
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
    let raw = serde_json::to_vec(item).map_or(0, |encoded| encoded.len());
    let (image_payload_bytes, image_replacement_bytes) = image_estimate_adjustment(item);
    raw.saturating_sub(image_payload_bytes)
        .saturating_add(image_replacement_bytes)
}

fn image_estimate_adjustment(item: &Value) -> (usize, usize) {
    let content = match item.get("type").and_then(Value::as_str) {
        Some("message") => item.get("content").and_then(Value::as_array),
        Some("function_call_output" | "custom_tool_call_output") => {
            item.get("output").and_then(Value::as_array)
        }
        _ => None,
    };
    content.into_iter().flatten().fold(
        (0_usize, 0_usize),
        |(payload_bytes, replacement_bytes), content_item| {
            if content_item.get("type").and_then(Value::as_str) != Some("input_image") {
                return (payload_bytes, replacement_bytes);
            }
            let Some(image_url) = content_item.get("image_url").and_then(Value::as_str) else {
                return (payload_bytes, replacement_bytes);
            };
            let Some(payload) = base64_image_payload(image_url) else {
                return (payload_bytes, replacement_bytes);
            };
            let replacement =
                if content_item.get("detail").and_then(Value::as_str) == Some("original") {
                    original_image_bytes_estimate(image_url).unwrap_or(RESIZED_IMAGE_BYTES_ESTIMATE)
                } else {
                    RESIZED_IMAGE_BYTES_ESTIMATE
                };
            (
                payload_bytes.saturating_add(payload.len()),
                replacement_bytes.saturating_add(replacement),
            )
        },
    )
}

fn base64_image_payload(image_url: &str) -> Option<&str> {
    if !image_url
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        return None;
    }
    let (metadata, payload) = image_url.split_once(',')?;
    let mut metadata = metadata["data:".len()..].split(';');
    let mime = metadata.next().unwrap_or_default();
    let base64 = metadata.any(|part| part.eq_ignore_ascii_case("base64"));
    (mime
        .get(.."image/".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
        && base64)
        .then_some(payload)
}

fn original_image_bytes_estimate(image_url: &str) -> Option<usize> {
    let key = Sha1::digest(image_url.as_bytes()).into();
    let estimate = || {
        let payload = base64_image_payload(image_url)?;
        let bytes = BASE64_STANDARD.decode(payload).ok()?;
        let image = image::load_from_memory(&bytes).ok()?;
        let patches_wide = image.width().div_ceil(ORIGINAL_IMAGE_PATCH_SIZE);
        let patches_high = image.height().div_ceil(ORIGINAL_IMAGE_PATCH_SIZE);
        let patches = usize::try_from(u64::from(patches_wide) * u64::from(patches_high))
            .unwrap_or(usize::MAX)
            .min(ORIGINAL_IMAGE_MAX_PATCHES);
        Some(patches.saturating_mul(APPROX_BYTES_PER_TOKEN))
    };
    match ORIGINAL_IMAGE_ESTIMATE_CACHE.lock() {
        Ok(mut cache) => cache.get_or_insert_with(key, estimate),
        Err(poisoned) => poisoned.into_inner().get_or_insert_with(key, estimate),
    }
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
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

    #[test]
    fn image_payload_uses_codex_fixed_cost_in_context_estimate() {
        let payload = "A".repeat(2_500_000);
        let item = json!({
            "type": "custom_tool_call_output",
            "call_id": "call-image",
            "output": [{
                "type": "input_image",
                "image_url": format!("data:image/png;base64,{payload}"),
                "detail": "high"
            }]
        });
        let usage = Usage {
            input_tokens: 9_000,
            output_tokens: 100,
            total_tokens: 9_100,
            ..Usage::default()
        };
        let raw = serde_json::to_vec(&item)
            .expect("serialize image item")
            .len();
        let expected_item_tokens = approx_tokens(
            raw.saturating_sub(payload.len())
                .saturating_add(RESIZED_IMAGE_BYTES_ESTIMATE),
        );

        assert_eq!(
            active_context_tokens(&usage, &[item]),
            9_100 + u64::try_from(expected_item_tokens).expect("estimate fits u64")
        );
        assert!(expected_item_tokens < 2_000);
    }

    fn message(text: &str) -> Value {
        json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
        })
    }
}
