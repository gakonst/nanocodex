use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    path::Path,
    sync::{Arc, LazyLock, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use image::{
    ColorType, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat,
    ImageReader,
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
    imageops::FilterType,
};
use nanocodex_core::{ContentItem, ImageDetail, PromptInput, UserInput};
use sha1::{Digest as _, Sha1};

use super::{ToolOutputBody, ToolOutputContent};

pub(super) const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const UNSUPPORTED_LOW_DETAIL_PLACEHOLDER: &str = "image content omitted because detail 'low' is not supported; use 'high', 'original', or 'auto'";
const REMOTE_IMAGE_URL_PLACEHOLDER: &str =
    "image content omitted because remote image URLs are not supported";

const DATA_URL_PREFIX: &str = "data:";
const PROMPT_IMAGE_PATCH_SIZE: u32 = 32;
const MAX_PROMPT_IMAGE_INPUT_BYTES: usize = 1024 * 1024 * 1024;
const MAX_IMAGE_CACHE_ENTRIES: usize = 32;
const MAX_IMAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;

const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2_500,
};
const ORIGINAL_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct PromptImageResizeLimits {
    max_dimension: u32,
    max_patches: u32,
}

#[derive(Clone)]
struct EncodedImage {
    bytes: Arc<[u8]>,
    mime: &'static str,
}

impl EncodedImage {
    fn into_data_url(self) -> String {
        format!(
            "data:{};base64,{}",
            self.mime,
            BASE64_STANDARD.encode(self.bytes)
        )
    }
}

struct ImageMetadata {
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ImageCacheKey {
    digest: [u8; 20],
    limits: PromptImageResizeLimits,
}

#[derive(Default)]
struct ImageCache {
    entries: HashMap<ImageCacheKey, EncodedImage>,
    order: VecDeque<ImageCacheKey>,
    bytes: usize,
}

impl ImageCache {
    fn get(&mut self, key: &ImageCacheKey) -> Option<EncodedImage> {
        let image = self.entries.get(key)?.clone();
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(*key);
        Some(image)
    }

    fn insert(&mut self, key: ImageCacheKey, image: EncodedImage) {
        self.insert_with_limits(key, image, MAX_IMAGE_CACHE_ENTRIES, MAX_IMAGE_CACHE_BYTES);
    }

    fn insert_with_limits(
        &mut self,
        key: ImageCacheKey,
        image: EncodedImage,
        entry_capacity: usize,
        byte_capacity: usize,
    ) {
        if image.bytes.len() > byte_capacity {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(previous.bytes.len());
            self.order.retain(|candidate| *candidate != key);
        }
        self.bytes = self.bytes.saturating_add(image.bytes.len());
        self.entries.insert(key, image);
        self.order.push_back(key);
        while self.entries.len() > entry_capacity || self.bytes > byte_capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.bytes.len());
            }
        }
    }
}

static IMAGE_CACHE: LazyLock<Mutex<ImageCache>> =
    LazyLock::new(|| Mutex::new(ImageCache::default()));

#[derive(Debug, thiserror::Error)]
enum ImagePreparationError {
    #[error("remote image URLs are not supported")]
    RemoteUrlUnsupported,
    #[error("image detail `low` is not supported")]
    UnsupportedLowDetail,
    #[error("image {representation} is too large ({size} bytes; max {max} bytes)")]
    ImageTooLarge {
        representation: &'static str,
        size: usize,
        max: usize,
    },
    #[error("{0}")]
    Processing(String),
}

impl ImagePreparationError {
    const fn placeholder(&self) -> &'static str {
        match self {
            Self::RemoteUrlUnsupported => REMOTE_IMAGE_URL_PLACEHOLDER,
            Self::UnsupportedLowDetail => UNSUPPORTED_LOW_DETAIL_PLACEHOLDER,
            Self::ImageTooLarge { .. } => IMAGE_TOO_LARGE_PLACEHOLDER,
            Self::Processing(_) => IMAGE_PROCESSING_ERROR_PLACEHOLDER,
        }
    }
}

pub async fn prepare_output_images(output: &mut ToolOutputBody) {
    let ToolOutputBody::Content(content) = output else {
        return;
    };
    if !content
        .iter()
        .any(|item| matches!(item, ToolOutputContent::InputImage { .. }))
    {
        return;
    }
    let content = std::mem::take(content);
    match tokio::task::spawn_blocking(move || prepare_content(content)).await {
        Ok(prepared) => {
            let ToolOutputBody::Content(output) = output else {
                return;
            };
            *output = prepared;
        }
        Err(error) => {
            eprintln!("failed to join image preparation task: {error}");
            *output = ToolOutputBody::Content(vec![ToolOutputContent::InputText {
                text: IMAGE_PROCESSING_ERROR_PLACEHOLDER.to_owned(),
            }]);
        }
    }
}

pub async fn prepare_user_input(input: &PromptInput) -> Vec<ContentItem> {
    let input = match input {
        PromptInput::Text(text) => vec![UserInput::Text { text: text.clone() }],
        PromptInput::Content(items) => items.clone(),
    };
    match tokio::task::spawn_blocking(move || prepare_user_content(input)).await {
        Ok(content) => content,
        Err(error) => {
            eprintln!("failed to join user image preparation task: {error}");
            vec![input_text(IMAGE_PROCESSING_ERROR_PLACEHOLDER)]
        }
    }
}

fn prepare_user_content(input: Vec<UserInput>) -> Vec<ContentItem> {
    let mut content = Vec::with_capacity(input.len());
    let mut image_index = 0;
    for item in input {
        match item {
            UserInput::Text { text } => content.push(input_text(text)),
            UserInput::Image { image_url, detail } => {
                image_index += 1;
                content.push(prepare_user_image(
                    image_url,
                    detail.unwrap_or(ImageDetail::High),
                ));
            }
            UserInput::LocalImage { path, detail } => {
                image_index += 1;
                let detail = detail.unwrap_or(ImageDetail::High);
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        content.push(input_text(format!(
                            "<image name=[Image #{image_index}] path=\"{}\">",
                            path.display()
                        )));
                        content.push(prepare_user_image(
                            format!(
                                "data:application/octet-stream;base64,{}",
                                BASE64_STANDARD.encode(bytes)
                            ),
                            detail,
                        ));
                        content.push(input_text("</image>"));
                    }
                    Err(error) => content.push(input_text(format!(
                        "Codex could not read the local image at `{}`: {error}",
                        path.display()
                    ))),
                }
            }
            UserInput::Audio { .. } => {
                content.push(input_text("Codex does not support audio input yet."));
            }
            UserInput::LocalAudio { .. } => {
                content.push(input_text("Codex does not support local audio input yet."));
            }
        }
    }
    content
}

fn prepare_user_image(mut image_url: String, detail: ImageDetail) -> ContentItem {
    match prepare_image(&mut image_url, detail) {
        Ok(()) => ContentItem::InputImage {
            image_url: image_url.into_boxed_str(),
            detail: Some(detail),
        },
        Err(error) => {
            eprintln!("failed to prepare message image: {error}");
            input_text(error.placeholder())
        }
    }
}

fn input_text(text: impl Into<String>) -> ContentItem {
    ContentItem::InputText {
        text: text.into().into_boxed_str(),
    }
}

fn prepare_content(mut content: Vec<ToolOutputContent>) -> Vec<ToolOutputContent> {
    for item in &mut content {
        let ToolOutputContent::InputImage { image_url, detail } = item else {
            continue;
        };
        if let Err(error) = prepare_image(image_url, *detail) {
            eprintln!("failed to prepare tool output image: {error}");
            *item = ToolOutputContent::InputText {
                text: error.placeholder().to_owned(),
            };
        }
    }
    content
}

fn prepare_image(image_url: &mut String, detail: ImageDetail) -> Result<(), ImagePreparationError> {
    if is_remote_image_url(image_url) {
        return Err(ImagePreparationError::RemoteUrlUnsupported);
    }
    if !is_data_url(image_url) {
        return Ok(());
    }
    let limits = match detail {
        ImageDetail::Auto | ImageDetail::High => HIGH_DETAIL_LIMITS,
        ImageDetail::Original => ORIGINAL_DETAIL_LIMITS,
        ImageDetail::Low => return Err(ImagePreparationError::UnsupportedLowDetail),
    };
    let bytes = decode_data_url(image_url, MAX_PROMPT_IMAGE_INPUT_BYTES)?;
    *image_url =
        load_for_prompt_bytes(Path::new("<data-url-image>"), bytes, limits)?.into_data_url();
    Ok(())
}

fn is_remote_image_url(image_url: &str) -> bool {
    image_url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

fn is_data_url(image_url: &str) -> bool {
    image_url
        .get(..DATA_URL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(DATA_URL_PREFIX))
}

fn decode_data_url(
    image_url: &str,
    max_input_bytes: usize,
) -> Result<Vec<u8>, ImagePreparationError> {
    let rest = image_url
        .get(..DATA_URL_PREFIX.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(DATA_URL_PREFIX))
        .and_then(|_| image_url.get(DATA_URL_PREFIX.len()..))
        .ok_or_else(|| ImagePreparationError::Processing("missing data: prefix".to_owned()))?;
    let (metadata, encoded) = rest.split_once(',').ok_or_else(|| {
        ImagePreparationError::Processing("data URL is missing a comma separator".to_owned())
    })?;
    if !metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        return Err(ImagePreparationError::Processing(
            "only base64 data URLs are supported".to_owned(),
        ));
    }
    if encoded.len() > max_input_bytes {
        return Err(ImagePreparationError::ImageTooLarge {
            representation: "base64 payload",
            size: encoded.len(),
            max: max_input_bytes,
        });
    }
    let bytes = BASE64_STANDARD.decode(encoded).map_err(|error| {
        ImagePreparationError::Processing(format!("invalid base64 payload: {error}"))
    })?;
    if bytes.len() > max_input_bytes {
        return Err(ImagePreparationError::ImageTooLarge {
            representation: "decoded input",
            size: bytes.len(),
            max: max_input_bytes,
        });
    }
    Ok(bytes)
}

fn load_for_prompt_bytes(
    path: &Path,
    file_bytes: Vec<u8>,
    limits: PromptImageResizeLimits,
) -> Result<EncodedImage, ImagePreparationError> {
    let key = ImageCacheKey {
        digest: Sha1::digest(&file_bytes).into(),
        limits,
    };
    let cached = match IMAGE_CACHE.lock() {
        Ok(mut cache) => cache.get(&key),
        Err(poisoned) => poisoned.into_inner().get(&key),
    };
    if let Some(image) = cached {
        return Ok(image);
    }

    let guessed_format = image::guess_format(&file_bytes).map_err(|error| {
        ImagePreparationError::Processing(format!(
            "unable to identify image at `{}`: {error}",
            path.display()
        ))
    })?;
    let preserved_format = match guessed_format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP => Some(guessed_format),
        _ => None,
    };
    let mut decoder = ImageReader::with_format(Cursor::new(&file_bytes), guessed_format)
        .into_decoder()
        .map_err(|error| {
            ImagePreparationError::Processing(format!(
                "unable to decode image at `{}`: {error}",
                path.display()
            ))
        })?;
    let metadata = ImageMetadata {
        icc_profile: decoder
            .icc_profile()
            .ok()
            .flatten()
            .filter(|profile| profile.get(16..20) == Some(b"RGB ")),
        exif: decoder.exif_metadata().ok().flatten(),
    };
    let dynamic = DynamicImage::from_decoder(decoder).map_err(|error| {
        ImagePreparationError::Processing(format!(
            "unable to decode image at `{}`: {error}",
            path.display()
        ))
    })?;
    let (width, height) = dynamic.dimensions();
    let (target_width, target_height) =
        prompt_image_output_dimensions_for_limits(width, height, limits);

    let image = if (target_width, target_height) == (width, height) {
        if let Some(format) = preserved_format {
            EncodedImage {
                bytes: file_bytes.into(),
                mime: format_to_mime(format),
            }
        } else {
            encode_image(&dynamic, ImageFormat::Png, metadata)?
        }
    } else {
        let resized = dynamic.resize_exact(target_width, target_height, FilterType::Triangle);
        encode_image(
            &resized,
            preserved_format.unwrap_or(ImageFormat::Png),
            metadata,
        )?
    };

    match IMAGE_CACHE.lock() {
        Ok(mut cache) => cache.insert(key, image.clone()),
        Err(poisoned) => poisoned.into_inner().insert(key, image.clone()),
    }
    Ok(image)
}

pub(super) fn load_for_prompt_data_url(
    path: &Path,
    file_bytes: Vec<u8>,
    detail: ImageDetail,
) -> Result<String, String> {
    let limits = match detail {
        ImageDetail::Auto | ImageDetail::High => HIGH_DETAIL_LIMITS,
        ImageDetail::Original => ORIGINAL_DETAIL_LIMITS,
        ImageDetail::Low => return Err("image detail `low` is not supported".to_owned()),
    };
    load_for_prompt_bytes(path, file_bytes, limits)
        .map(EncodedImage::into_data_url)
        .map_err(|error| error.to_string())
}

fn prompt_image_output_dimensions_for_limits(
    width: u32,
    height: u32,
    limits: PromptImageResizeLimits,
) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    if prompt_image_dimensions_fit(width, height, limits) {
        return (width, height);
    }

    let max_dimension_scale =
        (f64::from(limits.max_dimension) / f64::from(width.max(height))).min(1.0);
    let width = rounded_dimension(f64::from(width) * max_dimension_scale);
    let height = rounded_dimension(f64::from(height) * max_dimension_scale);
    if prompt_image_dimensions_fit(width, height, limits) {
        return (width, height);
    }

    let width_f64 = f64::from(width);
    let height_f64 = f64::from(height);
    let patch_size = f64::from(PROMPT_IMAGE_PATCH_SIZE);
    let mut scale =
        (patch_size * patch_size * f64::from(limits.max_patches) / width_f64 / height_f64).sqrt();
    let scaled_patches_wide = width_f64 * scale / patch_size;
    let scaled_patches_high = height_f64 * scale / patch_size;
    scale *= (scaled_patches_wide.floor() / scaled_patches_wide)
        .min(scaled_patches_high.floor() / scaled_patches_high);

    (
        floored_dimension(width_f64 * scale),
        floored_dimension(height_f64 * scale),
    )
}

fn prompt_image_dimensions_fit(width: u32, height: u32, limits: PromptImageResizeLimits) -> bool {
    let patches_wide = width.div_ceil(PROMPT_IMAGE_PATCH_SIZE);
    let patches_high = height.div_ceil(PROMPT_IMAGE_PATCH_SIZE);
    let patch_count = u64::from(patches_wide) * u64::from(patches_high);
    width <= limits.max_dimension
        && height <= limits.max_dimension
        && patch_count <= u64::from(limits.max_patches)
}

// Both callers pass a positive u32 dimension multiplied by a scale in 0..=1,
// so these conversions are bounded and cannot lose a sign or overflow u32.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rounded_dimension(value: f64) -> u32 {
    (value.round() as u32).max(1)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn floored_dimension(value: f64) -> u32 {
    (value.floor() as u32).max(1)
}

fn encode_image(
    image: &DynamicImage,
    preferred_format: ImageFormat,
    metadata: ImageMetadata,
) -> Result<EncodedImage, ImagePreparationError> {
    let target_format = match preferred_format {
        ImageFormat::Jpeg => ImageFormat::Jpeg,
        ImageFormat::WebP => ImageFormat::WebP,
        _ => ImageFormat::Png,
    };
    let mut bytes = Vec::new();
    let ImageMetadata { icc_profile, exif } = metadata;
    match target_format {
        ImageFormat::Png => {
            let rgba = image.to_rgba8();
            let mut encoder = PngEncoder::new(&mut bytes);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(|error| encode_error(target_format, &error))?;
        }
        ImageFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 85);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .encode_image(image)
                .map_err(|error| encode_error(target_format, &error))?;
        }
        ImageFormat::WebP => {
            let rgba = image.to_rgba8();
            let mut encoder = WebPEncoder::new_lossless(&mut bytes);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .write_image(
                    rgba.as_raw(),
                    image.width(),
                    image.height(),
                    ColorType::Rgba8.into(),
                )
                .map_err(|error| encode_error(target_format, &error))?;
        }
        _ => unreachable!("target format is normalized above"),
    }
    Ok(EncodedImage {
        bytes: bytes.into(),
        mime: format_to_mime(target_format),
    })
}

fn apply_image_metadata(
    encoder: &mut impl ImageEncoder,
    icc_profile: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
    format: ImageFormat,
) -> Result<(), ImagePreparationError> {
    if let Some(icc_profile) = icc_profile {
        encoder
            .set_icc_profile(icc_profile)
            .map_err(|error| encode_error(format, &image::ImageError::Unsupported(error)))?;
    }
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif)
            .map_err(|error| encode_error(format, &image::ImageError::Unsupported(error)))?;
    }
    Ok(())
}

fn encode_error(format: ImageFormat, error: &image::ImageError) -> ImagePreparationError {
    ImagePreparationError::Processing(format!("unable to encode image as {format:?}: {error}"))
}

const fn format_to_mime(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};

    use super::*;

    #[test]
    fn detail_policies_match_codex_patch_budgets() {
        for (limits, input, expected) in [
            (HIGH_DETAIL_LIMITS, (2048, 2048), (1600, 1600)),
            (ORIGINAL_DETAIL_LIMITS, (6401, 100), (6000, 94)),
            (ORIGINAL_DETAIL_LIMITS, (3201, 3201), (3200, 3200)),
        ] {
            assert_eq!(
                prompt_image_output_dimensions_for_limits(input.0, input.1, limits),
                expected
            );
        }
    }

    #[test]
    fn preparation_resizes_high_detail_images() {
        let image = RgbaImage::from_pixel(2048, 2048, Rgba([10, 20, 30, 255]));
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode fixture");
        let mut image_url = format!(
            "data:image/png;base64,{}",
            BASE64_STANDARD.encode(encoded.into_inner())
        );

        prepare_image(&mut image_url, ImageDetail::High).expect("prepare image");

        let bytes = decode_data_url(&image_url, MAX_PROMPT_IMAGE_INPUT_BYTES)
            .expect("decode prepared data URL");
        let prepared = image::load_from_memory(&bytes).expect("decode prepared image");
        assert_eq!(prepared.dimensions(), (1600, 1600));
    }

    #[test]
    fn user_input_uses_local_image_labels_and_audio_placeholders() {
        let image = RgbaImage::from_pixel(1, 1, Rgba([10, 20, 30, 255]));
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode fixture");
        let bytes = encoded.into_inner();
        let local_path = std::env::temp_dir().join(format!(
            "nanocodex-user-input-image-{}.png",
            std::process::id()
        ));
        std::fs::write(&local_path, &bytes).expect("write local image fixture");

        let content = prepare_user_content(vec![
            UserInput::Image {
                image_url: format!("data:image/png;base64,{}", BASE64_STANDARD.encode(&bytes)),
                detail: None,
            },
            UserInput::LocalImage {
                path: local_path.clone(),
                detail: Some(ImageDetail::Original),
            },
            UserInput::Audio {
                audio_url: "data:audio/wav;base64,AAAA".to_owned(),
            },
            UserInput::LocalAudio {
                path: local_path.with_extension("wav"),
            },
        ]);
        std::fs::remove_file(&local_path).expect("remove local image fixture");

        assert!(matches!(
            &content[0],
            ContentItem::InputImage {
                detail: Some(ImageDetail::High),
                ..
            }
        ));
        assert!(matches!(
            &content[1],
            ContentItem::InputText { text }
                if text.as_ref() == format!("<image name=[Image #2] path=\"{}\">", local_path.display())
        ));
        assert!(matches!(
            &content[2],
            ContentItem::InputImage {
                detail: Some(ImageDetail::Original),
                ..
            }
        ));
        assert!(
            matches!(&content[3], ContentItem::InputText { text } if text.as_ref() == "</image>")
        );
        assert!(
            matches!(&content[4], ContentItem::InputText { text } if text.as_ref() == "Codex does not support audio input yet.")
        );
        assert!(
            matches!(&content[5], ContentItem::InputText { text } if text.as_ref() == "Codex does not support local audio input yet.")
        );
    }

    #[test]
    fn converts_portable_pixmap_to_supported_png() {
        let ppm = b"P6\n1 1\n255\n\xff\x00\x00".to_vec();
        let image = load_for_prompt_bytes(Path::new("screen.ppm"), ppm, HIGH_DETAIL_LIMITS)
            .expect("decode portable pixmap");
        assert_eq!(image.mime, "image/png");
        assert!(image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn data_url_input_guard_precedes_base64_decoding() {
        let error = decode_data_url("data:image/png;base64,AAAAA", 4)
            .expect_err("oversized representation should fail");
        assert!(matches!(
            error,
            ImagePreparationError::ImageTooLarge {
                representation: "base64 payload",
                ..
            }
        ));
    }

    #[test]
    fn failed_images_become_bounded_placeholders() {
        let content = prepare_content(vec![
            ToolOutputContent::InputText {
                text: "before".to_owned(),
            },
            ToolOutputContent::InputImage {
                image_url: "https://example.com/image.png".to_owned(),
                detail: ImageDetail::High,
            },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,not-an-image".to_owned(),
                detail: ImageDetail::High,
            },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,ignored".to_owned(),
                detail: ImageDetail::Low,
            },
        ]);
        assert!(matches!(
            &content[0],
            ToolOutputContent::InputText { text } if text == "before"
        ));
        assert!(matches!(
            &content[1],
            ToolOutputContent::InputText { text } if text == REMOTE_IMAGE_URL_PLACEHOLDER
        ));
        assert!(matches!(
            &content[2],
            ToolOutputContent::InputText { text } if text == IMAGE_PROCESSING_ERROR_PLACEHOLDER
        ));
        assert!(matches!(
            &content[3],
            ToolOutputContent::InputText { text } if text == UNSUPPORTED_LOW_DETAIL_PLACEHOLDER
        ));
    }

    #[test]
    fn cache_enforces_count_and_byte_limits() {
        let mut cache = ImageCache::default();
        let entry_capacity =
            u64::try_from(MAX_IMAGE_CACHE_ENTRIES).expect("cache capacity fits in u64");
        for index in 0_u64..=entry_capacity {
            let mut digest = [0; 20];
            digest[..8].copy_from_slice(&index.to_le_bytes());
            cache.insert(
                ImageCacheKey {
                    digest,
                    limits: HIGH_DETAIL_LIMITS,
                },
                EncodedImage {
                    bytes: Arc::from([0_u8]),
                    mime: "image/png",
                },
            );
        }
        assert_eq!(cache.entries.len(), MAX_IMAGE_CACHE_ENTRIES);
        assert_eq!(cache.bytes, MAX_IMAGE_CACHE_ENTRIES);
        let first_key = ImageCacheKey {
            digest: [0; 20],
            limits: HIGH_DETAIL_LIMITS,
        };
        assert!(!cache.entries.contains_key(&first_key));

        let mut byte_bounded = ImageCache::default();
        for index in 0_u64..2 {
            let mut digest = [0; 20];
            digest[..8].copy_from_slice(&index.to_le_bytes());
            byte_bounded.insert_with_limits(
                ImageCacheKey {
                    digest,
                    limits: HIGH_DETAIL_LIMITS,
                },
                EncodedImage {
                    bytes: Arc::from([0_u8, 1]),
                    mime: "image/png",
                },
                2,
                3,
            );
        }
        assert_eq!(byte_bounded.entries.len(), 1);
        assert_eq!(byte_bounded.bytes, 2);
    }
}
