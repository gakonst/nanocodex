//! Inline client-tool images, prepared before they join request history.
use crate::{
    ContentBlock, ToolResultContent,
    prompt::{IMAGE_URL_UNSUPPORTED, ImageResolution, prepare_base64},
};
use serde_json::{Value, json};

/// Replaces each base64 image in tool results with its form prepared for the
/// model's native resolution, or with a text omission when it cannot be
/// prepared. Every native resolution is within the 3000 px long edge the
/// direct Messages API enforces once a request carries more than twenty
/// images, so preparing a result when it is first admitted keeps earlier image
/// bytes, and therefore the cached prefix, stable as history grows. The tool
/// result keeps its error status: the tool's effect has already completed. A
/// URL source becomes the same omission as a remote prompt image. File sources
/// are provider-resolved and left unchanged.
pub(super) async fn prepare_tool_images(results: &mut [ContentBlock], resolution: ImageResolution) {
    let images: Vec<&mut Value> = results
        .iter_mut()
        .filter_map(|result| match result {
            ContentBlock::ToolResult {
                content: ToolResultContent::Blocks(blocks),
                ..
            } => Some(blocks),
            _ => None,
        })
        .flatten()
        .filter(|block| {
            block["type"] == "image"
                && matches!(block["source"]["type"].as_str(), Some("base64" | "url"))
        })
        .collect();
    for block in images {
        if block["source"]["type"] == "url" {
            *block = json!({"type": "text", "text": IMAGE_URL_UNSUPPORTED});
            continue;
        }
        let data = match block["source"]["data"].take() {
            Value::String(data) => data,
            _ => String::new(),
        };
        match prepare_base64(&data, resolution).await {
            Ok((data, media_type)) => {
                block["source"]["data"] = data.into();
                block["source"]["media_type"] = media_type.into();
            }
            Err(omission) => *block = json!({"type": "text", "text": omission}),
        }
    }
}
