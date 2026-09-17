//! Retained-image budgeting follows codex-rs remote compaction v2: retain the
//! newest parts of a boundary message and keep image labels with their image.
use super::{
    ContentItem, ImageDetail, RESIZED_IMAGE_BYTES_ESTIMATE, ResponseItem, approx_tokens,
    estimate_item_tokens, original_image_bytes_estimate, truncate_middle_with_token_budget,
};

pub(super) fn content_tokens(item: &ContentItem) -> usize {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
            approx_tokens(text.len())
        }
        ContentItem::InputImage { image_url, detail } => {
            approx_tokens(if *detail == Some(ImageDetail::Original) {
                original_image_bytes_estimate(image_url).unwrap_or(RESIZED_IMAGE_BYTES_ESTIMATE)
            } else {
                RESIZED_IMAGE_BYTES_ESTIMATE
            })
        }
        ContentItem::InputAudio { .. } => 0,
    }
}

pub(super) fn message_content_token_count(item: &ResponseItem) -> usize {
    let ResponseItem::Message { content, .. } = item else {
        return usize::try_from(estimate_item_tokens(item)).unwrap_or(usize::MAX);
    };
    content.iter().map(content_tokens).sum()
}

pub(super) fn truncate_message(mut item: ResponseItem, max_tokens: usize) -> Option<ResponseItem> {
    let ResponseItem::Message { content, .. } = &mut item else {
        return None;
    };
    let mut remaining = max_tokens;
    let mut retained = Vec::with_capacity(content.len());
    while let Some(last) = content.len().checked_sub(1) {
        let image_index = match &content[last] {
            ContentItem::InputImage { .. } => Some(last),
            ContentItem::InputText { text }
                if text.as_ref() == "</image>"
                    && last > 0
                    && matches!(content[last - 1], ContentItem::InputImage { .. }) =>
            {
                Some(last - 1)
            }
            _ => None,
        };
        if let Some(image_index) = image_index {
            let has_open_tag = image_index > 0
                && matches!(&content[image_index - 1], ContentItem::InputText { text }
                    if text.as_ref() == "<image>"
                        || (text.starts_with("<image name=") && text.ends_with('>')));
            let start = image_index - usize::from(has_open_tag);
            let tokens: usize = content[start..].iter().map(content_tokens).sum();
            let fits = tokens <= remaining;
            remaining = if fits { remaining - tokens } else { 0 };
            for part in content.drain(start..).rev() {
                if fits {
                    retained.push(part);
                }
            }
            continue;
        }
        let mut part = content.pop()?;
        match &mut part {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                if remaining == 0 {
                    continue;
                }
                let tokens = approx_tokens(text.len());
                if tokens <= remaining {
                    remaining -= tokens;
                } else {
                    *text = truncate_middle_with_token_budget(text, remaining).into_boxed_str();
                    remaining = 0;
                }
                if !text.is_empty() {
                    retained.push(part);
                }
            }
            ContentItem::InputAudio { .. } => retained.push(part),
            ContentItem::InputImage { .. } => unreachable!("images handled atomically above"),
        }
    }
    if retained.is_empty() {
        return None;
    }
    retained.reverse();
    *content = retained;
    Some(item)
}
