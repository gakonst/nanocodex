use std::collections::BTreeSet;
use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex},
};

use crate::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, ImageDetail, ResponseItem,
    responses::ResponseHistory,
};
use sha2::{Digest as _, Sha256};

use super::context::is_contextual_user_message;
use base64::Engine as _;

#[path = "compaction_estimate.rs"]
mod estimate;
#[path = "compaction_images.rs"]
mod images;

const RETAINED_MESSAGE_TOKEN_BUDGET: usize = 64_000;
const MAX_RETAINED_AGENT_MESSAGE_TOKENS: u64 = 10_000;
const APPROX_BYTES_PER_TOKEN: usize = 4;
const RESIZED_IMAGE_BYTES_ESTIMATE: usize = 7_373;
const ORIGINAL_IMAGE_PATCH_SIZE: u32 = 32;
const ORIGINAL_IMAGE_MAX_PATCHES: usize = 10_000;
const ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE: usize = 32;
const CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE: &str =
    "Output exceeded the available model context and was truncated";

#[derive(Default)]
struct OriginalImageEstimateCache {
    entries: HashMap<[u8; 32], Option<usize>>,
    order: VecDeque<[u8; 32]>,
}

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

static ORIGINAL_IMAGE_ESTIMATE_CACHE: LazyLock<Mutex<OriginalImageEstimateCache>> =
    LazyLock::new(|| Mutex::new(OriginalImageEstimateCache::default()));

#[must_use]
pub fn auto_compact_token_limit(model: &str, context_window_tokens: u64) -> Option<u64> {
    matches!(model, "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna")
        .then_some(context_window_tokens.saturating_mul(9) / 10)
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
    // Codex model_context_window uses 95% of the raw model/configured window.
    let usable_context_tokens = context_window_tokens.saturating_mul(95) / 100;
    let mut estimated_tokens = request_prefix
        .iter()
        .chain(history.iter())
        .map(|item| u128::from(estimate_item_tokens(item)))
        .sum::<u128>();
    let mut rewritten_outputs = Vec::new();
    let mut consumed = 0;
    for (item, notice) in history_item_groups(history.iter()).rev() {
        if estimated_tokens <= u128::from(usable_context_tokens) {
            break;
        }
        let Some(rewritten) = rewritten_tool_output(item) else {
            break;
        };
        let tokens_before = u128::from(estimate_item_tokens(item))
            + notice.map_or(0, |notice| u128::from(estimate_item_tokens(notice)));
        estimated_tokens = estimated_tokens
            .saturating_sub(tokens_before)
            .saturating_add(u128::from(estimate_item_tokens(&rewritten)));
        consumed += 1 + usize::from(notice.is_some());
        rewritten_outputs.push(rewritten);
    }
    let rewritten_count = rewritten_outputs.len();
    if rewritten_count > 0 {
        rewritten_outputs.reverse();
        history.replace_suffix(history.len() - consumed, rewritten_outputs);
    }
    rewritten_count
}

fn history_item_groups<T: std::borrow::Borrow<ResponseItem>>(
    items: impl IntoIterator<Item = T>,
) -> impl DoubleEndedIterator<Item = (T, Option<T>)> {
    let mut items = items.into_iter().peekable();
    let mut groups = Vec::new();
    while let Some(source) = items.next() {
        let notice = items.next_if(|item| is_image_resize_notice(item.borrow()));
        groups.push((source, notice));
    }
    groups.into_iter()
}

fn is_image_resize_notice(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { role: crate::MessageRole::Developer, content, .. }
        if matches!(content.as_slice(), [ContentItem::InputText { text }]
            if text.trim().starts_with("<image_resize_notice>")
                && text.trim().ends_with("</image_resize_notice>")))
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
    install_history_with_provenance(history, initial_context, compaction, &BTreeSet::new())
}

#[must_use]
pub fn install_history_with_provenance(
    history: &[ResponseItem],
    initial_context: &[ResponseItem],
    compaction: ResponseItem,
    client_authored: &BTreeSet<String>,
) -> Vec<ResponseItem> {
    let retained = retained_item_groups(history.iter(), client_authored)
        .filter(|(item, _)| {
            (item.is_user_message() && !is_contextual_user_message(item))
                || is_client_developer_message(item, client_authored)
                || is_retained_agent_message(item)
        })
        .flat_map(|(source, notice)| std::iter::once(source).chain(notice))
        .cloned()
        .collect();
    let mut installed = truncate_retained_messages_with_provenance(
        retained,
        RETAINED_MESSAGE_TOKEN_BUDGET,
        client_authored,
    );
    // A retained developer message can follow the last user input. Context belongs
    // before the latest real input, or immediately before the summary if none remains.
    let insertion_index = installed
        .iter()
        .rposition(|item| item.is_user_message() || is_retained_agent_message(item))
        .unwrap_or(installed.len());
    installed.splice(
        insertion_index..insertion_index,
        initial_context.iter().cloned(),
    );
    installed.push(compaction);
    installed
}

fn is_retained_agent_message(item: &ResponseItem) -> bool {
    let ResponseItem::AgentMessage {
        author,
        recipient,
        content,
        ..
    } = item
    else {
        return false;
    };
    let first_text = match content.first() {
        Some(crate::responses::AgentMessageContent::InputText { text }) => text.as_ref(),
        _ => "",
    };
    let descendant_progress = author
        .strip_prefix(recipient.as_ref())
        .is_some_and(|suffix| suffix.starts_with('/'))
        && first_text.starts_with("Message Type: MESSAGE\n");
    !descendant_progress
        && !first_text.starts_with("Message Type: FINAL_ANSWER\n")
        && estimate_item_tokens(item) <= MAX_RETAINED_AGENT_MESSAGE_TOKENS
}

fn is_client_developer_message(item: &ResponseItem, client_authored: &BTreeSet<String>) -> bool {
    matches!(
        item,
        ResponseItem::Message {
            role: crate::MessageRole::Developer,
            ..
        }
    ) && item
        .id()
        .is_some_and(|id| client_authored.contains(id.as_str()))
}

// A client-authored notice-shaped message is independent, never attached to the
// preceding source. Match upstream v2_history_item_groups before retention.
fn retained_item_groups<T: std::borrow::Borrow<ResponseItem>>(
    items: impl IntoIterator<Item = T>,
    client_authored: &BTreeSet<String>,
) -> impl DoubleEndedIterator<Item = (T, Option<T>)> {
    history_item_groups(items)
        .flat_map(|(source, mut notice)| {
            let independent =
                notice.take_if(|item| is_client_developer_message(item.borrow(), client_authored));
            std::iter::once((source, notice)).chain(independent.map(|item| (item, None)))
        })
        .collect::<Vec<_>>()
        .into_iter()
}

#[cfg(test)]
fn truncate_retained_messages(items: Vec<ResponseItem>, max_tokens: usize) -> Vec<ResponseItem> {
    truncate_retained_messages_with_provenance(items, max_tokens, &BTreeSet::new())
}

fn truncate_retained_messages_with_provenance(
    items: Vec<ResponseItem>,
    max_tokens: usize,
    client_authored: &BTreeSet<String>,
) -> Vec<ResponseItem> {
    let mut remaining = max_tokens;
    let mut retained = Vec::with_capacity(items.len());
    for (item, notice) in retained_item_groups(items, client_authored).rev() {
        if remaining == 0 {
            continue;
        }
        let notice_tokens = notice
            .as_ref()
            .map_or(0, |item| message_text_token_count(item).max(1));
        let available = remaining.saturating_sub(notice_tokens);
        let developer = is_client_developer_message(&item, client_authored);
        let content_tokens = if developer {
            message_text_token_count(&item)
        } else {
            images::message_content_token_count(&item)
        };
        let tokens = if developer {
            usize::try_from(estimate_item_tokens(&item)).unwrap_or(usize::MAX)
        } else {
            content_tokens.max(1)
        };
        if tokens.saturating_add(notice_tokens) <= remaining {
            if let Some(notice) = notice {
                retained.push(notice);
            }
            retained.push(item);
            remaining = remaining
                .saturating_sub(tokens)
                .saturating_sub(notice_tokens);
            continue;
        }
        let content_budget = if developer {
            available.saturating_sub(tokens.saturating_sub(content_tokens))
        } else {
            available
        };
        let has_images = !developer
            && matches!(&item, ResponseItem::Message { content, .. }
            if content.iter().any(|part| matches!(part, ContentItem::InputImage { .. })));
        // Do not backfill with older history when an oversized image consumes the boundary.
        if has_images {
            remaining = 0;
        }
        if available == 0 {
            continue;
        }
        let truncated = if has_images {
            images::truncate_message(item, content_budget)
        } else {
            truncate_message_text(item, content_budget)
        };
        let Some(mut item) = truncated else {
            continue;
        };
        if developer {
            let item_tokens = usize::try_from(estimate_item_tokens(&item)).unwrap_or(usize::MAX);
            if item_tokens > available {
                let adjusted = content_budget
                    .saturating_sub(item_tokens - available)
                    .saturating_sub(1);
                let Some(corrected) = truncate_message_text(item, adjusted) else {
                    continue;
                };
                if usize::try_from(estimate_item_tokens(&corrected)).unwrap_or(usize::MAX)
                    > available
                {
                    continue;
                }
                item = corrected;
            }
        }
        if let Some(notice) = notice {
            retained.push(notice);
        }
        retained.push(item);
        remaining = 0;
    }
    retained.reverse();
    retained
}

fn message_text_token_count(item: &ResponseItem) -> usize {
    let ResponseItem::Message { content, .. } = item else {
        return usize::try_from(estimate_item_tokens(item)).unwrap_or(usize::MAX);
    };
    content
        .iter()
        .filter_map(content_text)
        .map(|text| approx_tokens(text.len()))
        .sum()
}

fn content_text(content: &ContentItem) -> Option<&str> {
    match content {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => Some(text),
        ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
    }
}

fn truncate_message_text(mut item: ResponseItem, max_tokens: usize) -> Option<ResponseItem> {
    let ResponseItem::Message { content, .. } = &mut item else {
        return None;
    };
    let mut remaining = max_tokens;
    let mut truncated = Vec::with_capacity(content.len());
    for mut content_item in std::mem::take(content) {
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
                if !text.is_empty() {
                    truncated.push(content_item);
                }
            }
            ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => {
                truncated.push(content_item);
            }
        }
    }
    if truncated.is_empty() {
        return None;
    }
    *content = truncated;
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
    u64::try_from(approx_tokens(estimate::model_visible_len(item))).unwrap_or(u64::MAX)
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
    let key = Sha256::digest(image_url.as_bytes()).into();
    let estimate = || {
        let payload = base64_image_payload(image_url)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()?;
        // Estimation needs header dimensions, never a full decoded pixel buffer.
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
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

const fn approx_tokens(bytes: usize) -> usize {
    bytes.saturating_add(APPROX_BYTES_PER_TOKEN - 1) / APPROX_BYTES_PER_TOKEN
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CONTEXT_WINDOW_TOKENS;

    #[test]
    fn original_image_estimate_decodes_dimensions() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(65, 33)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let image_url = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(png.into_inner())
        );

        assert_eq!(
            original_image_bytes_estimate(&image_url),
            Some(6 * APPROX_BYTES_PER_TOKEN)
        );
    }

    #[test]
    fn original_image_estimate_reads_dimensions_without_decoding_pixels() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(65, 33)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let mut header = encoded.into_inner();
        let data = header
            .windows(4)
            .position(|bytes| bytes == b"IDAT")
            .unwrap();
        header.truncate(data + 4);
        assert!(image::load_from_memory(&header).is_err());
        let url = format!("data:image/png;base64,{}", STANDARD.encode(header));
        assert_eq!(
            original_image_bytes_estimate(&url),
            Some(6 * APPROX_BYTES_PER_TOKEN)
        );
    }

    #[test]
    fn supported_models_compact_at_ninety_percent_of_the_policy_budget() {
        assert_eq!(
            auto_compact_token_limit("gpt-6-sol", CONTEXT_WINDOW_TOKENS),
            Some(244_800)
        );
        assert_eq!(
            auto_compact_token_limit("gpt-6-luna", crate::MAX_CONTEXT_WINDOW_TOKENS),
            Some(784_800)
        );
        assert_eq!(
            auto_compact_token_limit("gpt-6-astra", crate::MAX_CONTEXT_WINDOW_TOKENS),
            Some(784_800)
        );
        assert_eq!(
            auto_compact_token_limit("unknown-model", CONTEXT_WINDOW_TOKENS),
            None
        );
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
        let mut adapter = ResponseItem::message(
            crate::MessageRole::Developer,
            [ContentItem::InputText {
                text: "adapter session state".into(),
            }],
        );
        adapter.set_id(Some("client-adapter".into()));
        let provenance = BTreeSet::from(["client-adapter".to_owned()]);
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
        let installed = install_history_with_provenance(
            &history,
            &[permissions.clone(), initial.clone()],
            compaction,
            &provenance,
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

#[cfg(test)]
#[path = "compaction_parity_tests.rs"]
mod parity_tests;
