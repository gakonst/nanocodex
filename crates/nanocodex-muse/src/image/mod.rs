//! Prompt image preparation and model-output image normalization.

use std::{
    borrow::Cow,
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
};
pub use nanocodex_oai_api::ImageDetail;
use nanocodex_oai_api::{PromptInput, UserInput, responses::ContentItem};
use sha1::{Digest as _, Sha1};

use nanocodex_oai_tools::contract::{ToolOutputBody, ToolOutputContent};

pub(super) const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";

const DATA_URL_PREFIX: &str = "data:";
const PROMPT_IMAGE_PATCH_SIZE: u32 = 32;
const MAX_PROMPT_IMAGE_INPUT_BYTES: usize = 1024 * 1024 * 1024;
// Pixel buffers expand independently of compressed file size. Keep ordinary
// 12 MP RGB photos usable in Workers, but reject larger decodes before
// allocating their pixels. RGBA needs more headroom for source/destination
// buffers and serialized tool results. Native hosts retain the usual budget.
#[cfg(target_family = "wasm")]
const MAX_PROMPT_IMAGE_DECODE_BYTES: u64 = 40 * 1024 * 1024;
#[cfg(not(target_family = "wasm"))]
const MAX_PROMPT_IMAGE_DECODE_BYTES: u64 = 512 * 1024 * 1024;
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
            Self::ImageTooLarge { .. } => IMAGE_TOO_LARGE_PLACEHOLDER,
            Self::Processing(_) => IMAGE_PROCESSING_ERROR_PLACEHOLDER,
        }
    }
}

/// Validates, normalizes, and bounds images returned by a tool.
///
/// Unsupported or failed images become model-visible text placeholders. CPU
/// image work runs on the blocking pool on native targets and inline on WASM.
#[allow(
    clippy::unused_async,
    reason = "WASM prepares images inline without a blocking pool"
)]
pub async fn prepare_output_images(output: &mut ToolOutputBody) {
    let ToolOutputBody::Content(content) = output else {
        return;
    };
    for item in content.iter_mut() {
        if let ToolOutputContent::InputImageFile { file_id, .. } = item
            && !nanocodex_oai_api::responses::valid_image_file_id(file_id)
        {
            *item = ToolOutputContent::InputText {
                text: "Image file reference is malformed".to_owned(),
            };
        }
    }
    if !content
        .iter()
        .any(|item| matches!(item, ToolOutputContent::InputImage { .. }))
    {
        return;
    }
    let content = std::mem::take(content);
    #[cfg(target_family = "wasm")]
    {
        *output = ToolOutputBody::Content(prepare_content(content));
        output.replace_invalid_image_envelopes();
    }
    #[cfg(not(target_family = "wasm"))]
    match tokio::task::spawn_blocking(move || prepare_content(content)).await {
        Ok(prepared) => {
            let ToolOutputBody::Content(output) = output else {
                return;
            };
            *output = prepared;
        }
        Err(error) => {
            tracing::warn!(%error, "failed to join image preparation task");
            *output = ToolOutputBody::Content(vec![ToolOutputContent::InputText {
                text: IMAGE_PROCESSING_ERROR_PLACEHOLDER.to_owned(),
            }]);
        }
    }
}

/// Prepares reconstructed history with the same decoder and limits as fresh images.
/// Returns whether any image was replaced or normalized; item order is preserved.
pub fn prepare_history_images(items: &mut [nanocodex_oai_api::responses::ResponseItem]) -> bool {
    use nanocodex_oai_api::responses::{FunctionOutputBody, FunctionOutputContent, ResponseItem};
    let mut changed = false;
    for item in items {
        match item {
            ResponseItem::Message { content, .. } => {
                for part in content {
                    if let ContentItem::InputImage { image_url, detail } = part {
                        let mut url = image_url.to_string();
                        match prepare_image(&mut url, detail.unwrap_or(ImageDetail::Auto)) {
                            Ok(()) => {
                                changed |= url != image_url.as_ref();
                                *image_url = url.into_boxed_str();
                            }
                            Err(error) => {
                                *part = ContentItem::input_text(error.placeholder());
                                changed = true;
                            }
                        }
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                let FunctionOutputBody::Content(content) = output else {
                    continue;
                };
                for part in content {
                    if let FunctionOutputContent::InputImage { image_url, detail } = part {
                        let mut url = image_url.to_string();
                        match prepare_image(&mut url, detail.unwrap_or(ImageDetail::Auto)) {
                            Ok(()) => {
                                changed |= url != image_url.as_ref();
                                *image_url = url.into_boxed_str();
                            }
                            Err(error) => {
                                *part = FunctionOutputContent::InputText {
                                    text: error.placeholder().into(),
                                };
                                changed = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

/// Converts public prompt input into provider-ready typed content.
///
/// Local or data-URL images are validated and resized according to their detail
/// policy. Audio input is retained as an explicit placeholder until supported.
#[allow(
    clippy::unused_async,
    reason = "WASM prepares images inline without a blocking pool"
)]
pub async fn prepare_user_input(input: &PromptInput) -> Vec<ContentItem> {
    let input = match input {
        PromptInput::Text(text) => vec![UserInput::Text { text: text.clone() }],
        PromptInput::Content(items) => items.clone(),
    };
    #[cfg(target_family = "wasm")]
    {
        prepare_user_content(input)
    }
    #[cfg(not(target_family = "wasm"))]
    match tokio::task::spawn_blocking(move || prepare_user_content(input)).await {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!(%error, "failed to join user image preparation task");
            vec![input_text(IMAGE_PROCESSING_ERROR_PLACEHOLDER)]
        }
    }
}

fn prepare_user_content(input: Vec<UserInput>) -> Vec<ContentItem> {
    prepare_user_content_for_host(input, cfg!(target_family = "wasm"))
}

fn prepare_user_content_for_host(input: Vec<UserInput>, embedded: bool) -> Vec<ContentItem> {
    let mut content = Vec::with_capacity(input.len());
    #[cfg(not(target_family = "wasm"))]
    let mut image_index = 0;
    for item in input {
        match item {
            UserInput::Text { text } => content.push(input_text(text)),
            UserInput::ImageFile { file_id, detail } => {
                if nanocodex_oai_api::responses::valid_image_file_id(&file_id) {
                    content.push(ContentItem::InputImageFile {
                        file_id: file_id.into_boxed_str(),
                        detail,
                    });
                } else {
                    content.push(input_text("Image file reference is malformed"));
                }
            }
            UserInput::Image { image_url, detail } => {
                #[cfg(not(target_family = "wasm"))]
                {
                    image_index += 1;
                }
                content.push(prepare_user_image(
                    image_url,
                    detail.unwrap_or(ImageDetail::High),
                ));
            }
            #[cfg(target_family = "wasm")]
            UserInput::LocalImage { path, .. } => {
                content.push(input_text(format!(
                    "Local image paths are unavailable in browser WASM: {}",
                    path.display()
                )));
            }
            #[cfg(not(target_family = "wasm"))]
            UserInput::LocalImage { path, detail } => {
                if embedded {
                    content.push(input_text(format!(
                        "Local image paths are unavailable in browser WASM: {}",
                        path.display()
                    )));
                    continue;
                }
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
            UserInput::Audio { audio_url } => {
                if embedded {
                    content.push(ContentItem::InputAudio {
                        audio_url: audio_url.into_boxed_str(),
                    });
                } else {
                    content.push(input_text("Codex does not support audio input yet."));
                }
            }
            UserInput::LocalAudio { path } => {
                if embedded {
                    content.push(input_text(format!(
                        "Local audio paths are unavailable in browser WASM: {}",
                        path.display()
                    )));
                } else {
                    content.push(input_text("Codex does not support local audio input yet."));
                }
            }
            UserInput::File { filename, .. } => {
                content.push(input_text(format!(
                    "Codex does not support inline document input yet; document {} was not sent to the model.",
                    filename.as_deref().unwrap_or("attachment")
                )));
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
            tracing::warn!(%error, "failed to prepare message image");
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
            tracing::warn!(%error, "failed to prepare tool output image");
            *item = ToolOutputContent::InputText {
                text: error.placeholder().to_owned(),
            };
        }
    }
    content
}

fn prepare_image(image_url: &mut String, detail: ImageDetail) -> Result<(), ImagePreparationError> {
    if is_remote_image_url(image_url) {
        return Ok(());
    }
    if !is_data_url(image_url) {
        return Ok(());
    }
    let limits = match detail {
        ImageDetail::Auto | ImageDetail::High | ImageDetail::Low => HIGH_DETAIL_LIMITS,
        ImageDetail::Original => ORIGINAL_DETAIL_LIMITS,
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
    let mut reader = ImageReader::with_format(Cursor::new(&file_bytes), guessed_format);
    let mut decode_limits = image::Limits::default();
    decode_limits.max_alloc = Some(MAX_PROMPT_IMAGE_DECODE_BYTES);
    reader.limits(decode_limits);
    let mut decoder = reader.into_decoder().map_err(|error| {
        ImagePreparationError::Processing(format!(
            "unable to decode image at `{}`: {error}",
            path.display()
        ))
    })?;
    let pixel_bytes = decoder.total_bytes();
    if pixel_bytes > MAX_PROMPT_IMAGE_DECODE_BYTES {
        return Err(ImagePreparationError::ImageTooLarge {
            representation: "decoded pixels",
            size: usize::try_from(pixel_bytes).unwrap_or(usize::MAX),
            max: usize::try_from(MAX_PROMPT_IMAGE_DECODE_BYTES).unwrap_or(usize::MAX),
        });
    }
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
        // Triangle resize constructs an input-width x output-height RGBA-f32
        // intermediate: about 170 MiB for an ordinary 12 MP original-detail
        // photo. Area downsampling needs only source and destination pixels.
        let resized = dynamic.thumbnail_exact(target_width, target_height);
        drop(dynamic);
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

#[cfg(not(target_family = "wasm"))]
pub(super) fn load_for_prompt_data_url(
    path: &Path,
    file_bytes: Vec<u8>,
    detail: ImageDetail,
) -> Result<String, String> {
    let limits = match detail {
        ImageDetail::Auto | ImageDetail::High | ImageDetail::Low => HIGH_DETAIL_LIMITS,
        ImageDetail::Original => ORIGINAL_DETAIL_LIMITS,
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
            let (pixels, color) = rgb_or_rgba8_bytes(image);
            let mut encoder = PngEncoder::new(&mut bytes);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .write_image(pixels.as_ref(), image.width(), image.height(), color.into())
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
            let (pixels, color) = rgb_or_rgba8_bytes(image);
            let mut encoder = WebPEncoder::new_lossless(&mut bytes);
            apply_image_metadata(&mut encoder, icc_profile, exif, target_format)?;
            encoder
                .write_image(pixels.as_ref(), image.width(), image.height(), color.into())
                .map_err(|error| encode_error(target_format, &error))?;
        }
        _ => unreachable!("target format is normalized above"),
    }
    Ok(EncodedImage {
        bytes: bytes.into(),
        mime: format_to_mime(target_format),
    })
}

// Keep already-supported 8-bit buffers borrowed. Converting a 10 MP RGB
// result to owned RGBA adds a 40 MiB allocation at the encoder boundary.
fn rgb_or_rgba8_bytes(image: &DynamicImage) -> (Cow<'_, [u8]>, ColorType) {
    match image.color() {
        color @ (ColorType::Rgb8 | ColorType::Rgba8) => (Cow::Borrowed(image.as_bytes()), color),
        _ => (Cow::Owned(image.to_rgba8().into_raw()), ColorType::Rgba8),
    }
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
