//! Model-visible byte accounting from codex-rs 36430b3688 context_manager/history.rs.
//! Transport envelopes, IDs, citations and JSON escaping do not consume model context.
use super::*;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use std::io::Cursor;
use symphonia::core::{
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

pub(super) fn model_visible_len(item: &ResponseItem) -> usize {
    match item {
        ResponseItem::Message { content, .. } => content
            .iter()
            .map(|part| match part {
                ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                    text.len()
                }
                ContentItem::InputImage { image_url, detail } => image_bytes(image_url, *detail),
                ContentItem::InputAudio { audio_url } => audio_bytes(audio_url),
            })
            .fold(0, usize::saturating_add),
        ResponseItem::AgentMessage {
            author,
            recipient,
            content,
            ..
        } => content
            .iter()
            .map(|part| match part {
                crate::responses::AgentMessageContent::InputText { text } => text.len(),
                crate::responses::AgentMessageContent::EncryptedContent { encrypted_content } => {
                    encrypted_content.len().saturating_mul(9).div_ceil(16)
                }
            })
            .fold(
                author.len().saturating_add(recipient.len()),
                usize::saturating_add,
            ),
        ResponseItem::Reasoning {
            encrypted_content: Some(content),
            ..
        }
        | ResponseItem::ContextCompaction {
            encrypted_content: Some(content),
            ..
        }
        | ResponseItem::Compaction {
            encrypted_content: content,
            ..
        } => content
            .len()
            .saturating_mul(3)
            .checked_div(4)
            .unwrap_or(0)
            .saturating_sub(650),
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments: input,
            ..
        }
        | ResponseItem::CustomToolCall {
            name,
            namespace,
            input,
            ..
        } => name
            .len()
            .saturating_add(namespace.as_deref().unwrap_or("functions").len())
            .saturating_add(input.len()),
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        } => output_bytes(output).saturating_add(call_id.len()),
        ResponseItem::CustomToolCallOutput {
            call_id,
            name,
            output,
            ..
        } => output_bytes(output)
            .saturating_add(call_id.len())
            .saturating_add(name.as_deref().unwrap_or_default().len()),
        ResponseItem::AdditionalTools { tools, .. } => json_bytes(tools),
        ResponseItem::ToolSearchCall { arguments, .. } => json_bytes(arguments),
        ResponseItem::ToolSearchOutput { tools, .. } => json_bytes(tools),
        ResponseItem::LocalShellCall { action, .. } => json_bytes(action),
        ResponseItem::WebSearchCall { action, .. } => action.as_ref().map_or(0, json_bytes),
        ResponseItem::ImageGenerationCall {
            revised_prompt,
            result,
            ..
        } => revised_prompt
            .as_deref()
            .unwrap_or_default()
            .len()
            .saturating_add(if result.is_empty() {
                0
            } else {
                RESIZED_IMAGE_BYTES_ESTIMATE
            }),
        ResponseItem::Reasoning {
            encrypted_content: None,
            ..
        }
        | ResponseItem::ContextCompaction {
            encrypted_content: None,
            ..
        }
        | ResponseItem::ConfigurationUpdate { .. }
        | ResponseItem::CompactionTrigger { .. }
        | ResponseItem::Other(_) => 0,
    }
}
fn json_bytes(value: &(impl serde::Serialize + ?Sized)) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}
fn image_bytes(url: &str, detail: Option<ImageDetail>) -> usize {
    if detail == Some(ImageDetail::Original) {
        original_image_bytes_estimate(url).unwrap_or(RESIZED_IMAGE_BYTES_ESTIMATE)
    } else {
        RESIZED_IMAGE_BYTES_ESTIMATE
    }
}
fn output_bytes(output: &FunctionOutputBody) -> usize {
    match output {
        FunctionOutputBody::Text(text) => text.len(),
        FunctionOutputBody::Content(content) => content
            .iter()
            .map(|part| match part {
                FunctionOutputContent::InputText { text } => text.len(),
                FunctionOutputContent::InputImage { image_url, detail } => {
                    image_bytes(image_url, *detail)
                }
                FunctionOutputContent::InputAudio { audio_url } => audio_bytes(audio_url),
                FunctionOutputContent::EncryptedContent { encrypted_content } => {
                    encrypted_content.len().saturating_mul(9).div_ceil(16)
                }
            })
            .fold(0, usize::saturating_add),
    }
}
fn audio_bytes(url: &str) -> usize {
    let tokens = audio_duration_seconds(url).map_or_else(
        || approx_tokens(url.len()),
        |seconds| (seconds * 10.0).ceil() as usize,
    );
    tokens.saturating_mul(APPROX_BYTES_PER_TOKEN)
}

const fn canonical_audio_mime(mime: &str) -> Option<&'static str> {
    if mime.eq_ignore_ascii_case("audio/wav")
        || mime.eq_ignore_ascii_case("audio/x-wav")
        || mime.eq_ignore_ascii_case("audio/wave")
        || mime.eq_ignore_ascii_case("audio/vnd.wave")
    {
        Some("audio/wav")
    } else if mime.eq_ignore_ascii_case("audio/mpeg") || mime.eq_ignore_ascii_case("audio/mp3") {
        Some("audio/mpeg")
    } else if mime.eq_ignore_ascii_case("audio/mp4")
        || mime.eq_ignore_ascii_case("audio/m4a")
        || mime.eq_ignore_ascii_case("audio/x-m4a")
    {
        Some("audio/mp4")
    } else if mime.eq_ignore_ascii_case("audio/webm") {
        Some("audio/webm")
    } else if mime.eq_ignore_ascii_case("audio/ogg") {
        Some("audio/ogg")
    } else {
        None
    }
}

fn audio_duration_seconds(audio_url: &str) -> Option<f64> {
    let (metadata, payload) = audio_url.split_once(',')?;
    let metadata = metadata.get("data:".len()..)?;
    let mut metadata_parts = metadata.split(';');
    let canonical_mime = canonical_audio_mime(metadata_parts.next()?)?;
    if !metadata_parts.any(|part| part.eq_ignore_ascii_case("base64")) {
        return None;
    }

    let bytes = match BASE64_STANDARD.decode(payload) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::trace!(%error, "failed to decode audio payload for token estimation");
            return None;
        }
    };
    let media_source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.mime_type(canonical_mime);
    let format = match symphonia::default::get_probe().probe(
        &hint,
        media_source,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) {
        Ok(format) => format,
        Err(error) => {
            tracing::trace!(%error, "failed to read audio duration for token estimation");
            return None;
        }
    };
    let track = format.default_track(TrackType::Audio)?;
    let timing = track.time_base.zip(track.duration).or_else(|| {
        format
            .media_info()
            .time_base
            .zip(format.media_info().duration)
    });
    let (time_base, duration) = timing?;
    let duration_seconds =
        duration.get() as f64 * f64::from(time_base.numer.get()) / f64::from(time_base.denom.get());
    duration_seconds.is_finite().then_some(duration_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageRole;

    #[test]
    fn text_estimates_ignore_transport_framing_ids_and_json_escaping() {
        let mut item =
            ResponseItem::message(MessageRole::User, [ContentItem::input_text("\"\\\n🙂")]);
        assert_eq!(model_visible_len(&item), 7);
        item.set_id(Some("id-that-does-not-consume-context".into()));
        assert_eq!(estimate_item_tokens(&item), 2);
        assert_eq!(model_visible_len(&ResponseItem::compaction_trigger()), 0);
    }

    #[test]
    fn tool_accounting_includes_model_visible_names_and_arguments() {
        let item: ResponseItem = serde_json::from_value(serde_json::json!({
            "type": "function_call", "call_id": "excluded", "name": "exec", "arguments": "{}"
        }))
        .unwrap();
        assert_eq!(model_visible_len(&item), "execfunctions{}".len());
        let output = ResponseItem::custom_tool_output(
            "call".to_owned(),
            Some("exec".to_owned()),
            FunctionOutputBody::Text("result".into()),
        );
        assert_eq!(model_visible_len(&output), "callexecresult".len());
    }

    #[test]
    fn original_images_require_decoding_and_other_images_have_fixed_cost() {
        assert_eq!(
            image_bytes("https://example.test/image", None),
            RESIZED_IMAGE_BYTES_ESTIMATE
        );
        assert_eq!(
            image_bytes("data:image/png;base64,YQ==", Some(ImageDetail::Original)),
            RESIZED_IMAGE_BYTES_ESTIMATE
        );
    }

    #[test]
    fn wav_duration_and_invalid_audio_fallback_match_pinned_estimator() {
        let samples = 8_000u32;
        let data_len = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&samples.to_le_bytes());
        wav.extend_from_slice(&(samples * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.resize(wav.len() + data_len as usize, 0);
        let url = format!("data:audio/wav;base64,{}", BASE64_STANDARD.encode(wav));
        assert_eq!(audio_duration_seconds(&url), Some(1.0));
        assert_eq!(audio_bytes(&url), 40);
        let invalid = "data:audio/wav;base64,invalid";
        assert_eq!(audio_bytes(invalid), approx_tokens(invalid.len()) * 4);
    }
}
