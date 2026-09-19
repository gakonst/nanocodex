//! Audio handling ported from openai/codex 36430b36881cf5c289cb48e671cfc9e8b542ae7b.
//! Measures tool-generated PCM WAV clips using the audio bytes actually present.
//! Unknown formats retain the existing audio output behavior.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use std::io::Cursor;
use symphonia::core::{
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};
const MAX_PROMPT_AUDIO_INPUT_BYTES: usize = 50 * 1024 * 1024;

pub(super) fn wav_duration_seconds(audio_url: &str) -> Option<f64> {
    let (metadata, payload) = audio_url.split_once(',')?;
    if !metadata
        .split(';')
        .skip(1)
        .any(|part| part.eq_ignore_ascii_case("base64"))
        || payload.len() > MAX_PROMPT_AUDIO_INPUT_BYTES.div_ceil(3) * 4
    {
        return None;
    }
    let bytes = BASE64_STANDARD.decode(payload).ok()?;
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }

    let mut chunks = bytes.get(12..)?;
    let mut format = None;
    while chunks.len() >= 8 {
        let chunk_id = &chunks[..4];
        let size = u32::from_le_bytes(chunks[4..8].try_into().ok()?) as usize;
        let remaining = &chunks[8..];
        // Streaming WAV headers can declare more data than the file contains.
        let chunk = &remaining[..size.min(remaining.len())];
        match chunk_id {
            b"fmt " => {
                let mut encoding = u16::from_le_bytes(chunk.get(..2)?.try_into().ok()?);
                if encoding == 0xfffe {
                    // WAVE_FORMAT_EXTENSIBLE stores the encoding in a subtype GUID.
                    if chunk.get(26..40)?
                        != [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]
                    {
                        return None;
                    }
                    encoding = u16::from_le_bytes(chunk.get(24..26)?.try_into().ok()?);
                }
                if !matches!(encoding, 1 | 3) {
                    return None;
                }
                let sample_rate = u32::from_le_bytes(chunk.get(4..8)?.try_into().ok()?);
                let block_align = u16::from_le_bytes(chunk.get(12..14)?.try_into().ok()?);
                if sample_rate == 0 || block_align == 0 {
                    return None;
                }
                format = Some((sample_rate, block_align));
            }
            b"data" => {
                let (sample_rate, block_align) = format?;
                let frames = chunk.len() / usize::from(block_align);
                return Some(frames as f64 / f64::from(sample_rate));
            }
            _ => {}
        }
        chunks = remaining.get(size.checked_add(size % 2)?..)?;
    }
    None
}

fn canonical_audio_mime(mime: &str) -> Option<&'static str> {
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

pub(super) fn estimate_audio_token_count(audio_url: &str) -> usize {
    match audio_duration_seconds(audio_url) {
        Some(duration) => (duration * 10.0).ceil() as usize,
        None => audio_url.len().div_ceil(4),
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
