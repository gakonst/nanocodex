use nanocodex_oai_api::{PromptInput, responses::ContentItem, tools::ToolOutputBody};

/// Prepares embedded input with the shared image decoder, without local file access.
#[allow(
    clippy::unused_async,
    reason = "embedded image preparation runs inline"
)]
pub async fn prepare_user_input(input: &PromptInput) -> Vec<ContentItem> {
    crate::image::prepare_embedded_user_input(input)
}

/// Validates and normalizes embedded tool images without requiring a Tokio runtime.
#[allow(
    clippy::unused_async,
    reason = "embedded image preparation runs inline"
)]
pub async fn prepare_output_images(output: &mut ToolOutputBody) {
    crate::image::prepare_embedded_output_images(output);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolOutputContent;
    use nanocodex_oai_api::{
        PromptInput, UserInput, responses::ContentItem, tools::ToolOutputBody,
    };

    #[test]
    fn embedded_preparation_is_runtime_independent_and_preserves_host_policy() {
        use futures_util::FutureExt;
        let input = PromptInput::Content(vec![
            UserInput::Audio {
                audio_url: "data:audio/wav;base64,AAAA".into(),
            },
            UserInput::LocalImage {
                path: "/unavailable.png".into(),
                detail: None,
            },
            UserInput::LocalAudio {
                path: "/unavailable.wav".into(),
            },
        ]);
        let content = prepare_user_input(&input)
            .now_or_never()
            .expect("inline preparation");
        assert!(
            matches!(&content[0], ContentItem::InputAudio { audio_url } if audio_url.as_ref() == "data:audio/wav;base64,AAAA")
        );
        assert!(
            matches!(&content[1], ContentItem::InputText { text } if text.contains("Local image paths are unavailable"))
        );
        assert!(
            matches!(&content[2], ContentItem::InputText { text } if text.contains("Local audio paths are unavailable"))
        );
        let mut output = ToolOutputBody::Content(vec![ToolOutputContent::InputImage {
            image_url: "data:image/png;base64,YQ==".into(),
            detail: nanocodex_oai_api::ImageDetail::Auto,
        }]);
        prepare_output_images(&mut output)
            .now_or_never()
            .expect("inline preparation");
        assert!(
            matches!(output, ToolOutputBody::Content(ref items) if matches!(&items[0], ToolOutputContent::InputText { .. }))
        );
    }

    #[tokio::test]
    async fn embedded_preparation_decodes_pixels_for_input_and_output() {
        let corrupt = "data:image/png;base64,YQ==";
        let input = prepare_user_input(&PromptInput::Content(vec![UserInput::Image {
            image_url: corrupt.into(),
            detail: None,
        }]))
        .await;
        assert!(
            matches!(&input[0], ContentItem::InputText { text } if text.contains("could not be processed"))
        );
        let mut output = ToolOutputBody::Content(vec![ToolOutputContent::InputImage {
            image_url: corrupt.into(),
            detail: nanocodex_oai_api::ImageDetail::Original,
        }]);
        prepare_output_images(&mut output).await;
        assert!(
            matches!(output, ToolOutputBody::Content(ref items) if matches!(&items[0], ToolOutputContent::InputText { text } if text.contains("could not be processed")))
        );
    }

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
        let ToolOutputBody::Content(original_items) = &output else {
            unreachable!()
        };
        let ToolOutputContent::InputImage { image_url, .. } = &original_items[2] else {
            unreachable!()
        };
        let prepared_input = prepare_user_input(&PromptInput::Content(vec![UserInput::Image {
            image_url: image_url.clone(),
            detail: Some(nanocodex_oai_api::ImageDetail::Original),
        }]))
        .await;
        assert!(
            matches!(&prepared_input[0], ContentItem::InputImage { image_url: prepared, detail: Some(nanocodex_oai_api::ImageDetail::Original) } if prepared.as_ref() == image_url)
        );
        let expected = output.clone();
        prepare_output_images(&mut output).await;
        let ToolOutputBody::Content(items) = output else {
            panic!("expected content")
        };
        assert!(matches!(&items[0], ToolOutputContent::InputText { text } if text == "retained"));
        assert!(
            matches!(&items[1], ToolOutputContent::InputText { text } if text.contains("could not be processed"))
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
