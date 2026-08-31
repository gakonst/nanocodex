#[cfg(not(target_family = "wasm"))]
use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex},
};

use crate::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, ImageDetail, Model, ResponseItem,
    responses::ResponseHistory,
};
#[cfg(not(target_family = "wasm"))]
use sha2::{Digest as _, Sha256};

use super::context::is_contextual_user_message;
#[cfg(not(target_family = "wasm"))]
use crate::session::image_dimensions::dimensions_from_base64;

const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const RESIZED_IMAGE_BYTES_ESTIMATE: usize = 7_373;
#[cfg(not(target_family = "wasm"))]
const ORIGINAL_IMAGE_PATCH_SIZE: u32 = 32;
#[cfg(not(target_family = "wasm"))]
const ORIGINAL_IMAGE_MAX_PATCHES: usize = 10_000;
#[cfg(not(target_family = "wasm"))]
const ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE: usize = 32;
const CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE: &str =
    "Output exceeded the available model context and was truncated";

/// Model-owned context policy used by the narrow supported model family.
///
/// This keeps continuation behavior attached to typed model metadata rather
/// than string matching on wire model IDs. A routing prefix is intentionally a
/// transport concern and cannot change context behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelContextPolicy {
    default_context_window_tokens: u64,
    max_context_window_tokens: u64,
    auto_compact_percent: u8,
}

impl ModelContextPolicy {
    /// Resolves a caller override against the model's advertised maximum.
    #[must_use]
    pub const fn resolve_context_window(self, configured_tokens: u64) -> u64 {
        if configured_tokens == 0 {
            self.default_context_window_tokens
        } else if configured_tokens > self.max_context_window_tokens {
            self.max_context_window_tokens
        } else {
            configured_tokens
        }
    }

    /// Returns the model-owned automatic compaction threshold.
    #[must_use]
    pub const fn auto_compact_token_limit(self, configured_tokens: u64) -> u64 {
        self.resolve_context_window(configured_tokens)
            .saturating_mul(self.auto_compact_percent as u64)
            / 100
    }
}

/// Returns the context policy for a supported typed model.
#[must_use]
pub const fn model_context_policy(model: Model) -> ModelContextPolicy {
    // Keep the entries distinct even while the currently supported family
    // advertises the same limits. Future model metadata changes stay local to
    // this typed table instead of leaking model-ID conditionals through the
    // continuation runtime.
    match model {
        Model::Sol => ModelContextPolicy {
            default_context_window_tokens: crate::CONTEXT_WINDOW_TOKENS,
            max_context_window_tokens: crate::MAX_CONTEXT_WINDOW_TOKENS,
            auto_compact_percent: 90,
        },
        Model::Terra => ModelContextPolicy {
            default_context_window_tokens: crate::CONTEXT_WINDOW_TOKENS,
            max_context_window_tokens: crate::MAX_CONTEXT_WINDOW_TOKENS,
            auto_compact_percent: 90,
        },
        Model::Luna => ModelContextPolicy {
            default_context_window_tokens: crate::CONTEXT_WINDOW_TOKENS,
            max_context_window_tokens: crate::MAX_CONTEXT_WINDOW_TOKENS,
            auto_compact_percent: 90,
        },
    }
}

#[cfg(not(target_family = "wasm"))]
#[derive(Default)]
struct OriginalImageEstimateCache {
    entries: HashMap<[u8; 32], Option<usize>>,
    order: VecDeque<[u8; 32]>,
}

#[cfg(not(target_family = "wasm"))]
impl OriginalImageEstimateCache {
    fn get_or_insert_with(
        &mut self,
        key: [u8; 32],
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

#[cfg(not(target_family = "wasm"))]
static ORIGINAL_IMAGE_ESTIMATE_CACHE: LazyLock<Mutex<OriginalImageEstimateCache>> =
    LazyLock::new(|| Mutex::new(OriginalImageEstimateCache::default()));

#[must_use]
pub const fn auto_compact_token_limit(model: Model, context_window_tokens: u64) -> u64 {
    model_context_policy(model).auto_compact_token_limit(context_window_tokens)
}

#[must_use]
pub const fn trigger() -> ResponseItem {
    ResponseItem::compaction_trigger()
}

pub fn trim_tool_outputs_to_fit_context_window(
    history: &mut ResponseHistory,
    request_prefix: &[ResponseItem],
    context_window_tokens: u64,
) -> usize {
    let mut estimated_tokens = request_prefix
        .iter()
        .chain(history.iter())
        .map(estimate_item_tokens)
        .fold(0_u64, u64::saturating_add);
    let mut rewritten_outputs = Vec::new();
    for item in history.iter_rev() {
        if estimated_tokens <= context_window_tokens {
            break;
        }
        let tokens_before = estimate_item_tokens(item);
        let Some(rewritten) = rewritten_tool_output(item) else {
            break;
        };
        let tokens_after = estimate_item_tokens(&rewritten);
        estimated_tokens =
            estimated_tokens.saturating_sub(tokens_before.saturating_sub(tokens_after));
        rewritten_outputs.push(rewritten);
    }
    let rewritten_count = rewritten_outputs.len();
    if rewritten_count > 0 {
        rewritten_outputs.reverse();
        history.replace_suffix(history.len() - rewritten_count, rewritten_outputs);
    }
    rewritten_count
}

fn rewritten_tool_output(item: &ResponseItem) -> Option<ResponseItem> {
    let output = FunctionOutputBody::Text(CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE.into());
    match item {
        ResponseItem::FunctionCallOutput {
            id,
            call_id,
            caller,
            status,
            created_by,
            internal_chat_message_metadata_passthrough,
            ..
        } => Some(ResponseItem::FunctionCallOutput {
            id: id.clone(),
            call_id: call_id.clone(),
            output,
            caller: caller.clone(),
            status: *status,
            created_by: created_by.clone(),
            internal_chat_message_metadata_passthrough: internal_chat_message_metadata_passthrough
                .clone(),
        }),
        ResponseItem::CustomToolCallOutput {
            id,
            call_id,
            name,
            caller,
            status,
            created_by,
            internal_chat_message_metadata_passthrough,
            ..
        } => Some(ResponseItem::CustomToolCallOutput {
            id: id.clone(),
            call_id: call_id.clone(),
            name: name.clone(),
            output,
            caller: caller.clone(),
            status: *status,
            created_by: created_by.clone(),
            internal_chat_message_metadata_passthrough: internal_chat_message_metadata_passthrough
                .clone(),
        }),
        ResponseItem::ToolSearchOutput {
            id,
            call_id,
            status,
            execution,
            internal_chat_message_metadata_passthrough,
            ..
        } => Some(ResponseItem::ToolSearchOutput {
            id: id.clone(),
            call_id: call_id.clone(),
            status: status.clone(),
            execution: execution.clone(),
            tools: Vec::new(),
            internal_chat_message_metadata_passthrough: internal_chat_message_metadata_passthrough
                .clone(),
        }),
        _ => None,
    }
}

#[must_use]
pub fn install_history(
    history: &[ResponseItem],
    initial_context: &[ResponseItem],
    compaction: ResponseItem,
) -> Vec<ResponseItem> {
    let retained = history
        .iter()
        .filter(|item| {
            (item.is_user_message() && !is_contextual_user_message(item))
                || is_client_developer_message(item)
        })
        .cloned()
        .collect();
    let mut installed = truncate_retained_messages(retained, RETAINED_MESSAGE_TOKEN_BUDGET);
    let insertion_index = installed.len().saturating_sub(1);
    installed.splice(
        insertion_index..insertion_index,
        initial_context.iter().cloned(),
    );
    installed.push(compaction);
    installed
}

fn is_client_developer_message(item: &ResponseItem) -> bool {
    let ResponseItem::Message {
        role: crate::MessageRole::Developer,
        content,
        ..
    } = item
    else {
        return false;
    };
    !content.iter().any(|content| {
        let ContentItem::InputText { text } = content else {
            return false;
        };
        let text = text.trim();
        text.starts_with("<permissions instructions>")
            && text.ends_with("</permissions instructions>")
    })
}

fn truncate_retained_messages(items: Vec<ResponseItem>, max_tokens: usize) -> Vec<ResponseItem> {
    let mut remaining = max_tokens;
    let mut retained = Vec::with_capacity(items.len());
    for item in items.into_iter().rev() {
        if remaining == 0 {
            continue;
        }
        let tokens = retained_message_token_count(&item).max(1);
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

fn retained_message_token_count(item: &ResponseItem) -> usize {
    let ResponseItem::Message { content, .. } = item else {
        return 0;
    };
    content.iter().fold(0usize, |total, content| {
        let tokens = match content {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                approx_tokens(text.len())
            }
            ContentItem::InputImage { image_url, detail } => image_token_count(image_url, *detail),
            ContentItem::InputAudio { .. } => 0,
        };
        total.saturating_add(tokens)
    })
}

fn truncate_message_text(mut item: ResponseItem, max_tokens: usize) -> Option<ResponseItem> {
    let ResponseItem::Message {
        content,
        internal_chat_message_metadata_passthrough,
        ..
    } = &mut item
    else {
        return None;
    };
    let original_len = content.len();
    let mut kinds = internal_chat_message_metadata_passthrough
        .as_mut()
        .and_then(|metadata| metadata.content_item_kinds.take())
        .map(|mut kinds| {
            kinds.resize_with(original_len, || {
                crate::responses::ContentItemKind("unknown".into())
            });
            kinds.into_iter()
        });
    let mut remaining = max_tokens;
    let mut truncated = Vec::with_capacity(content.len());
    let mut retained_kinds = kinds.as_ref().map(|_| Vec::with_capacity(content.len()));
    for mut content_item in std::mem::take(content) {
        let kind = kinds.as_mut().and_then(Iterator::next);
        let mut retained = false;
        match &mut content_item {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                if remaining == 0 {
                    continue;
                }
                let tokens = approx_tokens(text.len());
                if tokens <= remaining {
                    remaining = remaining.saturating_sub(tokens);
                } else {
                    *text = truncate_middle_with_token_budget(text, remaining).into_boxed_str();
                    remaining = 0;
                }
                truncated.push(content_item);
                retained = true;
            }
            ContentItem::InputImage { image_url, detail } => {
                let tokens = image_token_count(image_url, *detail);
                if tokens <= remaining {
                    remaining = remaining.saturating_sub(tokens);
                    truncated.push(content_item);
                    retained = true;
                } else {
                    // Do not retain an image that alone exceeds the bounded
                    // compacted-message budget; otherwise a single data URL
                    // silently defeats the 64k cap.
                    remaining = 0;
                }
            }
            ContentItem::InputAudio { .. } => {
                truncated.push(content_item);
                retained = true;
            }
        }
        if retained && let (Some(retained_kinds), Some(kind)) = (&mut retained_kinds, kind) {
            retained_kinds.push(kind);
        }
    }
    if truncated.is_empty() {
        return None;
    }
    *content = truncated;
    if let Some(metadata) = internal_chat_message_metadata_passthrough
        && let Some(retained_kinds) = retained_kinds
    {
        metadata.content_item_kinds = Some(retained_kinds);
    }
    Some(item)
}

#[must_use]
pub fn truncate_middle_with_token_budget(text: &str, max_tokens: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
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
    let suffix_start =
        ceil_char_boundary(text, text.len().saturating_sub(right_budget)).max(prefix_end);
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

#[must_use]
pub fn estimate_item_tokens(item: &ResponseItem) -> u64 {
    u64::try_from(approx_tokens(model_visible_len(item))).unwrap_or(u64::MAX)
}

fn model_visible_len(item: &ResponseItem) -> usize {
    let encrypted = match item {
        ResponseItem::Reasoning {
            encrypted_content: Some(encrypted),
            ..
        }
        | ResponseItem::ContextCompaction {
            encrypted_content: Some(encrypted),
            ..
        }
        | ResponseItem::Compaction {
            encrypted_content: encrypted,
            ..
        } => Some(encrypted),
        _ => None,
    };
    if let Some(encrypted) = encrypted {
        return encrypted
            .len()
            .saturating_mul(3)
            .checked_div(4)
            .unwrap_or_default()
            .saturating_sub(650);
    }
    let raw = serde_json::to_vec(item).map_or(0, |encoded| encoded.len());
    let (image_payload, image_replacement) = image_estimate_adjustment(item);
    let (encrypted_payload, encrypted_replacement) =
        encrypted_function_output_estimate_adjustment(item);
    raw.saturating_sub(image_payload)
        .saturating_add(image_replacement)
        .saturating_sub(encrypted_payload)
        .saturating_add(encrypted_replacement)
}

fn image_estimate_adjustment(item: &ResponseItem) -> (usize, usize) {
    let images: Box<dyn Iterator<Item = (&str, Option<ImageDetail>)> + '_> = match item {
        ResponseItem::Message { content, .. } => Box::new(content.iter().filter_map(|content| {
            let ContentItem::InputImage { image_url, detail } = content else {
                return None;
            };
            Some((image_url.as_ref(), *detail))
        })),
        ResponseItem::FunctionCallOutput {
            output: FunctionOutputBody::Content(content),
            ..
        }
        | ResponseItem::CustomToolCallOutput {
            output: FunctionOutputBody::Content(content),
            ..
        } => Box::new(content.iter().filter_map(|content| {
            let FunctionOutputContent::InputImage { image_url, detail } = content else {
                return None;
            };
            Some((image_url.as_ref(), *detail))
        })),
        _ => Box::new(std::iter::empty()),
    };
    images.fold(
        (0usize, 0usize),
        |(payload_bytes, replacement_bytes), (image_url, detail)| {
            let Some(payload) = base64_image_payload(image_url) else {
                return (payload_bytes, replacement_bytes);
            };
            let replacement = if detail == Some(ImageDetail::Original) {
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

fn image_token_count(image_url: &str, detail: Option<ImageDetail>) -> usize {
    let Some(payload) = base64_image_payload(image_url) else {
        return 0;
    };
    let bytes = if detail == Some(ImageDetail::Original) {
        original_image_bytes_estimate(image_url).unwrap_or(RESIZED_IMAGE_BYTES_ESTIMATE)
    } else {
        RESIZED_IMAGE_BYTES_ESTIMATE
    };
    // Base64 itself does not count as model input; image patches do. Keeping
    // the payload check makes remote URLs preserve their existing provider
    // accounting behavior while locally supplied image bytes are bounded.
    let _ = payload;
    approx_tokens(bytes)
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

#[cfg(not(target_family = "wasm"))]
fn original_image_bytes_estimate(image_url: &str) -> Option<usize> {
    let key = Sha256::digest(image_url.as_bytes()).into();
    let estimate = || {
        let payload = base64_image_payload(image_url)?;
        let (width, height) = dimensions_from_base64(payload)?;
        let patches_wide = width.div_ceil(ORIGINAL_IMAGE_PATCH_SIZE);
        let patches_high = height.div_ceil(ORIGINAL_IMAGE_PATCH_SIZE);
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

#[cfg(target_family = "wasm")]
const fn original_image_bytes_estimate(_image_url: &str) -> Option<usize> {
    // The portable runtime uses the same conservative resized-image estimate when dimensions
    // are unavailable.
    None
}

fn encrypted_function_output_estimate_adjustment(item: &ResponseItem) -> (usize, usize) {
    let ResponseItem::FunctionCallOutput {
        output: FunctionOutputBody::Content(content),
        ..
    } = item
    else {
        return (0, 0);
    };
    content
        .iter()
        .filter_map(|content| {
            let FunctionOutputContent::EncryptedContent { encrypted_content } = content else {
                return None;
            };
            Some(encrypted_content.len())
        })
        .fold((0usize, 0usize), |(payload, replacement), len| {
            (
                payload.saturating_add(len),
                replacement.saturating_add(len.saturating_mul(9).div_ceil(16)),
            )
        })
}

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CONTEXT_WINDOW_TOKENS, Model};

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn original_image_estimate_uses_header_dimensions() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&65_u32.to_be_bytes());
        png.extend_from_slice(&33_u32.to_be_bytes());
        let image_url = format!("data:image/png;base64,{}", STANDARD.encode(png));

        assert_eq!(
            original_image_bytes_estimate(&image_url),
            Some(6 * APPROX_BYTES_PER_TOKEN)
        );
    }

    #[test]
    fn model_metadata_owns_auto_compaction_thresholds() {
        assert_eq!(
            auto_compact_token_limit(Model::Sol, CONTEXT_WINDOW_TOKENS),
            244_800
        );
        assert_eq!(
            auto_compact_token_limit(Model::Terra, CONTEXT_WINDOW_TOKENS),
            244_800
        );
        assert_eq!(
            auto_compact_token_limit(Model::Luna, crate::MAX_CONTEXT_WINDOW_TOKENS),
            784_800
        );
    }

    #[test]
    fn retained_image_budget_prevents_a_data_url_from_defeating_the_64k_cap() {
        let image = format!("data:image/png;base64,{}", "A".repeat(32));
        let older = message("older user fact");
        let latest = ResponseItem::message(
            crate::MessageRole::User,
            (0..40).map(|_| ContentItem::InputImage {
                image_url: image.clone().into_boxed_str(),
                detail: Some(ImageDetail::High),
            }),
        );

        let retained =
            truncate_retained_messages(vec![older, latest], RETAINED_MESSAGE_TOKEN_BUDGET);

        assert_eq!(retained.len(), 1);
        let ResponseItem::Message { content, .. } = &retained[0] else {
            panic!("the retained image message must remain a user message");
        };
        assert!(content.len() < 40);
    }

    #[test]
    fn retained_message_truncation_keeps_content_kinds_positional() {
        let mut item = ResponseItem::message(
            crate::MessageRole::User,
            [
                ContentItem::input_text("keep"),
                ContentItem::input_image(format!("data:image/png;base64,{}", "A".repeat(32))),
            ],
        );
        item.set_message_content_item_kinds([
            crate::responses::ContentItemKind("user.text".into()),
            crate::responses::ContentItemKind("user.image".into()),
        ]);

        let retained = truncate_message_text(item, 1).expect("text remains");
        let ResponseItem::Message { content, .. } = &retained else {
            panic!("retained item is a message");
        };
        assert_eq!(content.len(), 1);
        let kinds = retained
            .message_content_item_kinds()
            .expect("retained kinds");
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].as_str(), "user.text");
    }

    #[test]
    fn installed_history_retains_user_inputs_and_reinjects_context() {
        let permissions = ResponseItem::message(
            crate::MessageRole::Developer,
            [ContentItem::InputText {
                text: "<permissions instructions>...</permissions instructions>".into(),
            }],
        );
        let initial =
            message("<environment_context>\n<cwd>/workspace</cwd>\n</environment_context>");
        let first = message("do the task");
        let adapter = ResponseItem::message(
            crate::MessageRole::Developer,
            [ContentItem::InputText {
                text: "adapter session state".into(),
            }],
        );
        let latest = message("and preserve the tests");
        let history = vec![
            initial.clone(),
            first.clone(),
            adapter.clone(),
            ResponseItem::Reasoning {
                id: None,
                summary: Vec::new(),
                content: None,
                encrypted_content: Some("old".into()),
                status: None,
                internal_chat_message_metadata_passthrough: None,
            },
            latest.clone(),
        ];
        let compaction: ResponseItem = serde_json::from_str(
            r#"{"id":"cmp-id","type":"compaction","encrypted_content":"opaque"}"#,
        )
        .unwrap();
        let installed = install_history(
            &history,
            &[permissions.clone(), initial.clone()],
            compaction,
        );
        assert_eq!(installed.len(), 6);
        assert_eq!(
            serde_json::to_value(&installed[0]).unwrap(),
            serde_json::to_value(first).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&installed[1]).unwrap(),
            serde_json::to_value(adapter).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&installed[2]).unwrap(),
            serde_json::to_value(permissions).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&installed[3]).unwrap(),
            serde_json::to_value(initial).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&installed[4]).unwrap(),
            serde_json::to_value(latest).unwrap()
        );
        assert!(matches!(
            &installed[5],
            ResponseItem::Compaction { id: Some(id), .. } if id.as_str() == "cmp-id"
        ));
    }

    #[test]
    fn over_window_history_rewrites_trailing_tool_outputs() {
        let mut history = ResponseHistory::new(vec![ResponseItem::custom_tool_output(
            "call".to_owned(),
            None,
            FunctionOutputBody::Text(
                "x".repeat(272_001 * APPROX_BYTES_PER_TOKEN)
                    .into_boxed_str(),
            ),
        )]);
        assert_eq!(
            trim_tool_outputs_to_fit_context_window(&mut history, &[], CONTEXT_WINDOW_TOKENS,),
            1
        );
        assert!(matches!(
            history.iter().next().unwrap(),
            ResponseItem::CustomToolCallOutput {
                output: FunctionOutputBody::Text(text),
                ..
            } if text.as_ref() == CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE
        ));
    }

    #[test]
    fn over_window_history_rewrites_tool_search_output_without_losing_metadata() {
        let mut history = ResponseHistory::new(vec![ResponseItem::ToolSearchOutput {
            id: Some(crate::ResponseItemId::from("tso_search")),
            call_id: Some("call_search".into()),
            status: "completed".into(),
            execution: "client".into(),
            tools: vec![
                serde_json::json!({
                    "name": "large_tool",
                    "description": "x".repeat(272_001 * APPROX_BYTES_PER_TOKEN),
                })
                .into(),
            ],
            internal_chat_message_metadata_passthrough: Some(
                crate::responses::InternalMessageMetadata {
                    turn_id: Some("turn_search".into()),
                    create_time: None,
                    content_item_kinds: None,
                },
            ),
        }]);

        assert_eq!(
            trim_tool_outputs_to_fit_context_window(&mut history, &[], CONTEXT_WINDOW_TOKENS,),
            1
        );
        assert_eq!(
            serde_json::to_value(history.iter().next().unwrap()).unwrap(),
            serde_json::json!({
                "type": "tool_search_output",
                "id": "tso_search",
                "call_id": "call_search",
                "status": "completed",
                "execution": "client",
                "tools": [],
                "internal_chat_message_metadata_passthrough": {
                    "turn_id": "turn_search",
                },
            })
        );
    }

    #[test]
    fn under_window_history_keeps_its_shared_storage() {
        let mut history = ResponseHistory::new(vec![ResponseItem::custom_tool_output(
            "call".to_owned(),
            None,
            FunctionOutputBody::Text("output".into()),
        )]);
        let shared_tail = history.shared_tail();

        assert_eq!(
            trim_tool_outputs_to_fit_context_window(&mut history, &[], CONTEXT_WINDOW_TOKENS,),
            0
        );
        assert!(std::sync::Arc::ptr_eq(&history.shared_tail(), &shared_tail));
    }

    fn message(text: &str) -> ResponseItem {
        ResponseItem::message(
            crate::MessageRole::User,
            [ContentItem::InputText { text: text.into() }],
        )
    }
}
