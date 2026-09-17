//! Clipboard access at the terminal boundary.

use arboard::Clipboard;
use base64::{Engine, engine::general_purpose::STANDARD};
use png::{BitDepth, ColorType, Encoder, EncodingError};

#[path = "../../clipboard.rs"]
mod text;
pub(crate) use text::copy_to_clipboard as copy_text;

pub(crate) fn image_data_url() -> Option<String> {
    let mut clipboard = Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;
    encode_png(image.width, image.height, image.bytes.as_ref())
        .ok()
        .map(|png| format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

fn encode_png(width: usize, height: usize, pixels: &[u8]) -> Result<Vec<u8>, EncodingError> {
    let width = u32::try_from(width).map_err(|_| EncodingError::LimitsExceeded)?;
    let height = u32::try_from(height).map_err(|_| EncodingError::LimitsExceeded)?;
    let mut png = Vec::new();
    let mut encoder = Encoder::new(&mut png, width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::encode_png;

    #[test]
    fn clipboard_pixels_are_encoded_as_png() {
        let encoded = encode_png(1, 1, &[255, 0, 0, 255]).unwrap();
        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
    }
}
