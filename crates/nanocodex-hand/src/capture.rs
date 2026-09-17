//! Shared still-capture sizing and transport policy.
use crate::Error;
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize)]
struct Policy {
    max_dimension: u32,
    max_base64_bytes: usize,
    jpeg_qualities: Vec<u8>,
}
fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(|| {
        serde_json::from_str(include_str!("capture_policy.json"))
            .expect("valid embedded capture policy")
    })
}
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{RgbImage, codecs::jpeg::JpegEncoder};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Error>;

fn error(message: &str) -> Error {
    std::io::Error::other(message.to_owned()).into()
}

/// Fit valid display dimensions into 1280 pixels without increasing the scale.
pub(crate) fn target_dimensions(width: f64, height: f64) -> Result<(u32, u32)> {
    if !width.is_finite()
        || !height.is_finite()
        || !(1.0..=65536.0).contains(&width)
        || !(1.0..=65536.0).contains(&height)
    {
        return Err(error(
            "main display is unavailable or has invalid dimensions",
        ));
    }
    let maximum = f64::from(policy().max_dimension);
    let scale = (maximum / width.max(height)).min(1.0);
    Ok((
        (width * scale).round().clamp(1.0, maximum) as u32,
        (height * scale).round().clamp(1.0, maximum) as u32,
    ))
}

/// Encode a bounded RGB frame using the common JPEG response contract.
pub(crate) fn encode_jpeg(frame: &RgbImage) -> Result<Value> {
    if !(1..=policy().max_dimension).contains(&frame.width())
        || !(1..=policy().max_dimension).contains(&frame.height())
    {
        return Err(error("unexpected screen capture dimensions"));
    }
    // Bound the base64 representation itself, stricter than a 500k JPEG bound.
    for &quality in &policy().jpeg_qualities {
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(frame)
            .map_err(|_| error("could not encode screen JPEG"))?;
        if bytes.len().div_ceil(3) * 4 <= policy().max_base64_bytes {
            return Ok(
                json!({"status":"ok","jpeg":STANDARD.encode(bytes),"width":frame.width(),"height":frame.height()}),
            );
        }
    }
    Err(error("screen JPEG exceeds the 500000-byte transport limit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_fit_landscape_portrait_and_small_displays() {
        for (source, expected) in [
            ((2560.0, 1440.0), (1280, 720)),
            ((1440.0, 2560.0), (720, 1280)),
            ((640.0, 480.0), (640, 480)),
            ((1280.0, 1280.0), (1280, 1280)),
            ((65536.0, 1.0), (1280, 1)),
            ((1.0, 65536.0), (1, 1280)),
            ((1.0, 1.0), (1, 1)),
            ((1920.0, 1081.0), (1280, 721)),
        ] {
            assert_eq!(target_dimensions(source.0, source.1).unwrap(), expected);
        }
    }

    #[test]
    fn invalid_display_dimensions_are_rejected() {
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            0.0,
            0.5,
            65537.0,
        ] {
            assert!(target_dimensions(invalid, 720.0).is_err());
            assert!(target_dimensions(1280.0, invalid).is_err());
        }
    }

    #[test]
    fn invalid_frame_dimensions_are_rejected_before_encoding() {
        for (width, height) in [(0, 1), (1, 0), (1281, 1), (1, 1281)] {
            assert!(encode_jpeg(&RgbImage::new(width, height)).is_err());
        }
    }

    #[test]
    fn noisy_frame_fits_base64_budget_and_decodes_as_jpeg() {
        let mut random = 1u32;
        let frame = RgbImage::from_fn(1280, 1280, |_, _| {
            image::Rgb(std::array::from_fn(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            }))
        });
        let result = encode_jpeg(&frame).unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["width"], 1280);
        assert_eq!(result["height"], 1280);
        let jpeg = result["jpeg"].as_str().unwrap();
        assert!(jpeg.len() <= 500_000);
        let bytes = STANDARD.decode(jpeg).unwrap();
        assert_eq!(STANDARD.encode(&bytes), jpeg);
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1280, 1280));
    }
}
