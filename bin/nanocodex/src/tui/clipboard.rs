//! Image clipboard access and shared text clipboard routing.

use std::{io::Cursor, path::PathBuf};

#[path = "../clipboard.rs"]
mod text;
pub(super) use text::copy_to_clipboard;

pub(super) fn paste_image_to_temp_png() -> Result<PathBuf, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    let files = clipboard.get().file_list().unwrap_or_default();
    let image = if let Some(image) = files.into_iter().find_map(|path| image::open(path).ok()) {
        image
    } else {
        let image = clipboard
            .get_image()
            .map_err(|error| format!("no image on clipboard: {error}"))?;
        let width = u32::try_from(image.width)
            .map_err(|error| format!("clipboard image width is invalid: {error}"))?;
        let height = u32::try_from(image.height)
            .map_err(|error| format!("clipboard image height is invalid: {error}"))?;
        let rgba = image::RgbaImage::from_raw(width, height, image.bytes.into_owned())
            .ok_or_else(|| "clipboard returned an invalid RGBA image".to_owned())?;
        image::DynamicImage::ImageRgba8(rgba)
    };

    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("failed to encode clipboard image: {error}"))?;
    let temporary = tempfile::Builder::new()
        .prefix("nanocodex-clipboard-")
        .suffix(".png")
        .tempfile()
        .map_err(|error| format!("failed to create clipboard image file: {error}"))?;
    std::fs::write(temporary.path(), png)
        .map_err(|error| format!("failed to write clipboard image: {error}"))?;
    let (_, path) = temporary
        .keep()
        .map_err(|error| format!("failed to retain clipboard image: {}", error.error))?;
    Ok(path)
}
