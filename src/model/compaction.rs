use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};

use super::context_manager::is_contextual_user_message;

const SOL_CONTEXT_WINDOW: u64 = 272_000;
const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const RESIZED_IMAGE_BYTES_ESTIMATE: usize = 7_373;
const ORIGINAL_IMAGE_PATCH_SIZE: u32 = 32;
const ORIGINAL_IMAGE_MAX_PATCHES: usize = 10_000;
const ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE: usize = 32;
const CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE: &str =
    "Output exceeded the available model context and was truncated";

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

pub(super) fn trigger() -> Value {
    json!({ "type": "compaction_trigger" })
}

pub(super) fn trim_tool_outputs_to_fit_context_window(
    history: &mut [Value],
    active_context_tokens: u64,
) -> usize {
    let mut estimated_tokens = active_context_tokens;
    let mut rewritten_outputs = 0;
    for item in history.iter_mut().rev() {
        if estimated_tokens <= SOL_CONTEXT_WINDOW {
            break;
        }
        let tokens_before = estimate_item_tokens(item);
        if !rewrite_tool_output(item) {
            break;
        }
        let tokens_after = estimate_item_tokens(item);
        estimated_tokens =
            estimated_tokens.saturating_sub(tokens_before.saturating_sub(tokens_after));
        rewritten_outputs += 1;
    }
    rewritten_outputs
}

fn rewrite_tool_output(item: &mut Value) -> bool {
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call_output" | "custom_tool_call_output")
    ) {
        return false;
    }
    let Some(output) = item.get_mut("output") else {
        return false;
    };
    *output = Value::String(CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE.to_owned());
    true
}

pub(super) fn install_history(
    history: &[Value],
    canonical_context: &Value,
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
        .filter(|item| is_real_user_message(item))
        .cloned()
        .collect();
    let mut installed = truncate_retained_messages(retained, RETAINED_MESSAGE_TOKEN_BUDGET);
    let insertion_index = installed.len().saturating_sub(1);
    installed.insert(insertion_index, canonical_context.clone());
    installed.push(compaction);
    installed
}

fn is_real_user_message(item: &Value) -> bool {
    is_user_message(item) && !is_contextual_user_message(item)
}

fn is_user_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("user")
}

fn truncate_retained_messages(items: Vec<Value>, max_tokens: usize) -> Vec<Value> {
    let mut remaining = max_tokens;
    let mut retained = Vec::with_capacity(items.len());
    for item in items.into_iter().rev() {
        if remaining == 0 {
            continue;
        }
        let tokens = message_text_token_count(&item).max(1);
        if tokens <= remaining {
            retained.push(item);
            remaining = remaining.saturating_sub(tokens);
        } else if let Some(item) = truncate_message_text(item, remaining) {
            retained.push(item);
            remaining = 0;
        }
    }
    retained.reverse();
    retained
}

fn message_text_token_count(item: &Value) -> usize {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(
            |content| match content.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text") => {
                    content.get("text").and_then(Value::as_str).map(str::len)
                }
                _ => None,
            },
        )
        .map(approx_tokens)
        .sum()
}

fn truncate_message_text(mut item: Value, max_tokens: usize) -> Option<Value> {
    let content = item.get_mut("content")?.as_array_mut()?;
    let mut remaining = max_tokens;
    let mut truncated = Vec::with_capacity(content.len());
    for mut content_item in std::mem::take(content) {
        match content_item.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text") => {
                if remaining == 0 {
                    continue;
                }
                let Some(text) = content_item.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let tokens = approx_tokens(text.len());
                if tokens <= remaining {
                    remaining = remaining.saturating_sub(tokens);
                } else {
                    let text = truncate_middle_with_token_budget(text, remaining);
                    let Some(slot) = content_item.get_mut("text") else {
                        continue;
                    };
                    *slot = Value::String(text);
                    remaining = 0;
                }
                truncated.push(content_item);
            }
            Some("input_image") => truncated.push(content_item),
            _ => {}
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

pub(super) fn estimate_item_tokens(item: &Value) -> u64 {
    u64::try_from(approx_tokens(model_visible_len(item))).unwrap_or(u64::MAX)
}

fn model_visible_len(item: &Value) -> usize {
    if matches!(
        item.get("type").and_then(Value::as_str),
        Some("reasoning" | "compaction" | "context_compaction")
    ) {
        if let Some(encrypted) = item.get("encrypted_content").and_then(Value::as_str) {
            return encrypted
                .len()
                .saturating_mul(3)
                .checked_div(4)
                .unwrap_or_default()
                .saturating_sub(650);
        }
    }
    let raw = serde_json::to_vec(item).map_or(0, |encoded| encoded.len());
    let (image_payload_bytes, image_replacement_bytes) = image_estimate_adjustment(item);
    let (encrypted_payload_bytes, encrypted_replacement_bytes) =
        encrypted_function_output_estimate_adjustment(item);
    raw.saturating_sub(image_payload_bytes)
        .saturating_add(image_replacement_bytes)
        .saturating_sub(encrypted_payload_bytes)
        .saturating_add(encrypted_replacement_bytes)
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

fn encrypted_function_output_estimate_adjustment(item: &Value) -> (usize, usize) {
    if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
        return (0, 0);
    }
    item.get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("encrypted_content"))
        .filter_map(|content| content.get("encrypted_content").and_then(Value::as_str))
        .fold((0usize, 0usize), |(payload, replacement), encrypted| {
            (
                payload.saturating_add(encrypted.len()),
                replacement.saturating_add(encrypted.len().saturating_mul(9).div_ceil(16)),
            )
        })
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sol_compacts_at_ninety_percent_of_its_context_window() {
        assert_eq!(auto_compact_token_limit("gpt-5.6-sol"), Some(244_800));
        assert_eq!(auto_compact_token_limit("unknown-model"), None);
    }

    #[test]
    fn installed_history_retains_user_inputs_and_reinjects_initial_context() {
        let initial = json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "<environment_context>\n<cwd>/workspace</cwd>\n</environment_context>",
            }],
        });
        let first = message("do the task");
        let latest = message("and preserve the tests");
        let replaced_context = json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "# AGENTS.md instructions\n\n<INSTRUCTIONS>\nThese instructions replace the old ones.\n</INSTRUCTIONS>",
            }],
        });
        let history = vec![
            initial.clone(),
            first.clone(),
            json!({ "type": "reasoning", "encrypted_content": "old" }),
            json!({ "type": "custom_tool_call", "call_id": "call-1", "name": "exec", "input": "text(1)" }),
            json!({ "type": "custom_tool_call_output", "call_id": "call-1", "output": "done" }),
            replaced_context,
            latest.clone(),
        ];

        assert_eq!(
            install_history(
                &history,
                &initial,
                json!({ "id": "cmp-id", "type": "compaction", "encrypted_content": "opaque" }),
            ),
            vec![
                first,
                initial,
                latest,
                json!({ "type": "compaction", "encrypted_content": "opaque" }),
            ]
        );
    }

    #[test]
    fn retained_history_truncation_keeps_newest_messages_first() {
        let retained = vec![message("old-old"), message("middle1234"), message("new")];

        assert_eq!(
            truncate_retained_messages(retained, 3),
            vec![message("midd…1 tokens truncated…1234"), message("new")]
        );
    }

    #[test]
    fn retained_history_truncation_preserves_images() {
        let item = json!({
            "type": "message",
            "role": "user",
            "content": [
                { "type": "input_text", "text": "abcdef" },
                { "type": "input_image", "image_url": "data:image/png;base64,abc" },
                { "type": "output_text", "text": "uvwxyz" },
            ],
        });

        assert_eq!(
            truncate_retained_messages(vec![item], 3),
            vec![json!({
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "abcdef" },
                    { "type": "input_image", "image_url": "data:image/png;base64,abc" },
                    { "type": "output_text", "text": "uv…1 tokens truncated…yz" },
                ],
            })]
        );
    }

    #[test]
    fn over_window_history_rewrites_trailing_tool_outputs() {
        let history = vec![
            json!({
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "a".repeat(200_000),
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call-2",
                "output": "b".repeat(200_000),
            }),
        ];
        let mut request_history = history.clone();

        assert_eq!(
            trim_tool_outputs_to_fit_context_window(
                &mut request_history,
                SOL_CONTEXT_WINDOW + 75_000,
            ),
            2
        );
        assert_eq!(
            request_history[0]["output"],
            CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE
        );
        assert_eq!(
            request_history[1]["output"],
            CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE
        );
        assert_ne!(history, request_history);
        assert!(
            history[0]["output"]
                .as_str()
                .is_some_and(|text| text.len() == 200_000)
        );
    }

    #[test]
    fn auto_compaction_below_hard_window_preserves_tool_output() {
        let mut history = vec![json!({
            "type": "custom_tool_call_output",
            "call_id": "call-1",
            "output": "unchanged",
        })];

        assert_eq!(
            trim_tool_outputs_to_fit_context_window(
                &mut history,
                auto_compact_token_limit("gpt-5.6-sol").expect("Sol has a compact limit"),
            ),
            0
        );
        assert_eq!(history[0]["output"], "unchanged");
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
        let raw = serde_json::to_vec(&item)
            .expect("serialize image item")
            .len();
        let expected_item_tokens = approx_tokens(
            raw.saturating_sub(payload.len())
                .saturating_add(RESIZED_IMAGE_BYTES_ESTIMATE),
        );

        assert_eq!(
            estimate_item_tokens(&item),
            u64::try_from(expected_item_tokens).expect("estimate fits u64")
        );
        assert!(expected_item_tokens < 2_000);
    }

    #[test]
    fn encrypted_payloads_use_codex_decoded_size_estimates() {
        let reasoning = json!({
            "type": "reasoning",
            "encrypted_content": "r".repeat(4_000),
        });
        assert_eq!(estimate_item_tokens(&reasoning), 588);

        let encrypted = "e".repeat(1_600);
        let output = json!({
            "type": "function_call_output",
            "call_id": "call",
            "output": [{
                "type": "encrypted_content",
                "encrypted_content": encrypted,
            }],
        });
        let raw = serde_json::to_vec(&output).unwrap().len();
        let expected = approx_tokens(raw - 1_600 + 900);
        assert_eq!(estimate_item_tokens(&output), expected as u64);
    }

    fn message(text: &str) -> Value {
        json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }],
        })
    }
}
