use nanocodex_oai_api::{PromptInput, UserInput, responses::ContentItem, tools::ToolOutputBody};

/// Converts public prompt input to provider-ready content without native file processing.
#[allow(
    clippy::unused_async,
    reason = "matches the native input-preparation contract"
)]
pub async fn prepare_user_input(input: &PromptInput) -> Vec<ContentItem> {
    let items = match input {
        PromptInput::Text(text) => vec![UserInput::Text { text: text.clone() }],
        PromptInput::Content(items) => items.clone(),
    };
    items
        .into_iter()
        .map(|item| match item {
            UserInput::Text { text } => ContentItem::InputText {
                text: text.into_boxed_str(),
            },
            UserInput::Image { image_url, detail } => ContentItem::InputImage {
                image_url: image_url.into_boxed_str(),
                detail,
            },
            UserInput::Audio { audio_url } => ContentItem::InputAudio {
                audio_url: audio_url.into_boxed_str(),
            },
            UserInput::LocalImage { path, .. } => ContentItem::InputText {
                text: format!(
                    "Local image paths are unavailable in browser WASM: {}",
                    path.display()
                )
                .into_boxed_str(),
            },
            UserInput::LocalAudio { path } => ContentItem::InputText {
                text: format!(
                    "Local audio paths are unavailable in browser WASM: {}",
                    path.display()
                )
                .into_boxed_str(),
            },
        })
        .collect()
}

/// Leaves embedding-prepared tool images unchanged.
#[allow(
    clippy::unused_async,
    reason = "matches the native output-preparation contract"
)]
pub async fn prepare_output_images(_output: &mut ToolOutputBody) {}
