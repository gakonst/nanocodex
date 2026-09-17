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

/// Validates every embedded tool image envelope, including raw host tool outputs.
/// Pixel decoding and normalization remain the embedding producer's responsibility.
#[allow(
    clippy::unused_async,
    reason = "matches the native output-preparation contract"
)]
pub async fn prepare_output_images(output: &mut ToolOutputBody) {
    output.replace_invalid_image_envelopes();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolOutputContent;

    #[tokio::test]
    async fn embedded_preparation_repairs_raw_host_images() {
        let mut output = ToolOutputBody::Content(vec![
            ToolOutputContent::InputText { text: "retained".into() },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,AAAA\n[output truncated]".into(),
                detail: nanocodex_oai_api::ImageDetail::Auto,
            },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC".into(),
                detail: nanocodex_oai_api::ImageDetail::Original,
            },
        ]);
        #[cfg(not(target_family = "wasm"))]
        {
            use base64::Engine;
            let ToolOutputBody::Content(items) = &output else {
                unreachable!()
            };
            let ToolOutputContent::InputImage { image_url, .. } = &items[2] else {
                unreachable!()
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(image_url.split_once(',').unwrap().1)
                .unwrap();
            let decoded = image::load_from_memory(&bytes).expect("valid PNG fixture");
            assert_eq!((decoded.width(), decoded.height()), (1, 1));
        }
        let expected = output.clone();
        prepare_output_images(&mut output).await;
        let ToolOutputBody::Content(items) = output else {
            panic!("expected content")
        };
        assert!(matches!(&items[0], ToolOutputContent::InputText { text } if text == "retained"));
        assert!(
            matches!(&items[1], ToolOutputContent::InputText { text } if text.contains("malformed"))
        );
        let ToolOutputBody::Content(original) = expected else {
            unreachable!()
        };
        assert_eq!(
            serde_json::to_value(&items[2]).unwrap(),
            serde_json::to_value(&original[2]).unwrap()
        );
        // The real PNG fixture and requested detail survive unchanged.
        assert!(matches!(
            &items[2],
            ToolOutputContent::InputImage {
                detail: nanocodex_oai_api::ImageDetail::Original,
                ..
            }
        ));
    }
}
