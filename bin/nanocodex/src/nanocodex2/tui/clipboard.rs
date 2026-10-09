//! Clipboard access at the terminal boundary.

use arboard::Clipboard;
use base64::{Engine, engine::general_purpose::STANDARD};
use png::{BitDepth, ColorType, Encoder, EncodingError};

pub(crate) use crate::clipboard::copy_to_clipboard as copy_text;

pub(crate) fn image_data_url() -> Option<String> {
    let mut clipboard = Clipboard::new().ok()?;
    // Finder and other file managers copy file URLs, not bitmap pixels.
    if let Some(data) = clipboard
        .get()
        .file_list()
        .unwrap_or_default()
        .iter()
        .find_map(|path| file_image_data_url(path))
    {
        return Some(data);
    }
    let image = clipboard.get_image().ok()?;
    encode_png(image.width, image.height, image.bytes.as_ref())
        .ok()
        .map(|png| format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

fn encode_png(width: usize, height: usize, pixels: &[u8]) -> Result<Vec<u8>, EncodingError> {
    let width = u32::try_from(width).map_err(|_| EncodingError::LimitsExceeded)?;
    let height = u32::try_from(height).map_err(|_| EncodingError::LimitsExceeded)?;
    // Workers allow only 40 MiB for decoded prompt pixels. A full-resolution
    // RGBA screenshot can exceed that even when the compressed PNG is tiny.
    // Match high-detail prompt sizing before the image crosses the wire, and
    // bound encoded bytes for noisy photographs as well.
    let mut image = image::RgbaImage::from_raw(width, height, pixels.to_vec())
        .ok_or(EncodingError::LimitsExceeded)?;
    if width.max(height) > 2048 {
        let longest = u64::from(width.max(height));
        image = image::imageops::thumbnail(
            &image,
            ((u64::from(width) * 2048 / longest) as u32).max(1),
            ((u64::from(height) * 2048 / longest) as u32).max(1),
        );
    }
    loop {
        let mut png = Vec::new();
        let mut encoder = Encoder::new(&mut png, image.width(), image.height());
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(image.as_raw())?;
        writer.finish()?;
        if png.len() <= 4 * 1024 * 1024 {
            return Ok(png);
        }
        image = image::imageops::thumbnail(
            &image,
            (image.width() * 3 / 4).max(1),
            (image.height() * 3 / 4).max(1),
        );
    }
}

// Terminals commonly paste a local filename for copied/dropped images. Only
// consume an existing image path at the start, preserving any caption exactly.
// Prose, missing files and other files retain text-paste behavior. Never fetch URLs.
pub(crate) fn pasted_image_data_url(text: &str) -> Option<(String, &str)> {
    // Prefer the whole path (including unescaped spaces) before splitting a
    // caption. Try longest prefixes first so existing filenames win.
    if let Some(data) = image_path_data_url(text) {
        return Some((data, ""));
    }
    for (offset, _) in text
        .char_indices()
        .rev()
        .filter(|(_, ch)| ch.is_whitespace())
    {
        if let Some(data) = image_path_data_url(&text[..offset]) {
            return Some((data, &text[offset..]));
        }
    }
    None
}

fn image_path_data_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(data) = file_image_data_url(std::path::Path::new(text)) {
        return Some(data);
    }
    if let Ok(url) = url::Url::parse(text)
        && url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        return file_image_data_url(&path);
    }
    let paths = shlex::split(text)?;
    if paths.len() != 1 {
        return None;
    }
    file_image_data_url(std::path::Path::new(&paths[0]))
}

fn file_image_data_url(path: &std::path::Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let image = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .to_rgba8();
    encode_png(
        image.width() as usize,
        image.height() as usize,
        image.as_raw(),
    )
    .ok()
    .map(|png| format!("data:image/png;base64,{}", STANDARD.encode(png)))
}
