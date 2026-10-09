//! Portable, bounded Claude prompt media, prepared for the model's native image
//! resolution. Native files are frozen before acceptance.
use crate::{ContentBlock, Message, Role};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageReader;
use nanocodex_agent::{
    NanocodexError, Result,
    input::{Prompt, PromptInput, PromptMessageRole, UserInput},
};
use nanocodex_oai_tools::image::prepare_base64_images;
use serde_json::json;
use std::io::Cursor;

const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Bounds an inline or local image before it is decoded and prepared within
/// [`MAX_IMAGE_BYTES`].
const MAX_SOURCE_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Claude views images in square patches of this many pixels, one visual token each.
const IMAGE_PATCH: u32 = 28;
const MAX_IMAGES: usize = 20;
const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;
/// Anthropic accepts PDFs up to 32 MB per request; bound each decoded document
/// and every prompt's combined media below that after base64 expansion.
const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 5;
const MAX_FILENAME_BYTES: usize = 255;

fn invalid(message: impl Into<String>) -> NanocodexError {
    NanocodexError::InvalidRequest(message.into())
}

/// The largest image a model processes without the Messages API reducing it:
/// a long-edge limit and a visual-token budget. See
/// <https://platform.claude.com/docs/en/build-with-claude/vision#resolution-and-token-cost>.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ImageResolution {
    max_edge: u32,
    max_tokens: u32,
}

impl ImageResolution {
    /// Claude 4.7 and later models use the high-resolution tier; all other
    /// models, including unknown ones, use the standard tier.
    pub(crate) fn of(model: &str) -> Self {
        match model {
            "claude-opus-5-5" | "claude-fable-5-1" | "claude-sonnet-5-5" | "claude-haiku-5-5"
            | "claude-opus-5" | "claude-sonnet-5" => Self {
                max_edge: 2576,
                max_tokens: 4784,
            },
            _ => Self {
                max_edge: 1568,
                max_tokens: 1568,
            },
        }
    }

    fn fits(self, width: u32, height: u32) -> bool {
        let (columns, rows) = (width.div_ceil(IMAGE_PATCH), height.div_ceil(IMAGE_PATCH));
        columns.max(rows) <= self.max_edge / IMAGE_PATCH
            && u64::from(columns) * u64::from(rows) <= u64::from(self.max_tokens)
    }

    /// The size the Messages API reduces an image to: the largest
    /// aspect-preserving size within both limits, following Anthropic's
    /// reference implementation, including its rounding. See
    /// <https://platform.claude.com/docs/en/build-with-claude/vision-coordinates#resize-your-image-before-uploading>.
    fn fit(self, width: u32, height: u32) -> (u32, u32) {
        if self.fits(width, height) {
            return (width, height);
        }
        if height > width {
            let (height, width) = self.fit(height, width);
            return (width, height);
        }
        let aspect_ratio = f64::from(width) / f64::from(height);
        let short_edge = |long_edge: u32| {
            ((f64::from(long_edge) / aspect_ratio).round_ties_even() as u32).max(1)
        };
        // The lower bound always fits and the upper bound never does.
        let (mut low, mut high) = (1, width);
        while low + 1 < high {
            let middle = low.midpoint(high);
            if self.fits(middle, short_edge(middle)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        (low, short_edge(low))
    }
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

// Model-visible notes that replace an image Claude cannot use. The first three
// match the notes of the shared image preparation and the OpenAI driver.
const IMAGE_UNPROCESSABLE: &str = "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
pub(crate) const IMAGE_URL_UNSUPPORTED: &str =
    "image content omitted because remote image URLs are not supported";
const IMAGE_FILE_UNSUPPORTED: &str =
    "image content omitted because Claude cannot use OpenAI image file references";

/// Prepares base64 image data for the model's native resolution and the
/// per-image byte limit, returning the prepared data and its media type, or
/// the note that replaces an image that cannot be prepared. The shared image
/// preparation decodes the image and re-encodes it when it is resized or is
/// not PNG, JPEG, or WebP. It rounds sizes its own way, so its output is
/// checked against both limits, and an encoding over the byte limit is shrunk
/// until it fits.
pub(crate) async fn prepare_base64(
    payload: &str,
    resolution: ImageResolution,
) -> std::result::Result<(String, &'static str), &'static str> {
    let mut long_edge = STANDARD
        .decode(payload)
        .ok()
        .and_then(|bytes| image_dimensions(&bytes))
        .map_or(resolution.max_edge, |(width, height)| {
            let (width, height) = resolution.fit(width, height);
            width.max(height)
        });
    let mut previous_len = usize::MAX;
    loop {
        let (data, media_type) = prepare_base64_images(vec![payload.to_owned()], long_edge)
            .await
            .pop()
            .expect("each image has a preparation result")?;
        let bytes = STANDARD
            .decode(&data)
            .expect("prepared image data is base64");
        let next = match image_dimensions(&bytes) {
            Some((width, height)) if !resolution.fits(width, height) => long_edge - 1,
            _ if bytes.len() <= MAX_IMAGE_BYTES => return Ok((data, media_type)),
            // A step drops nearly half of the pixels, so an encoding that barely
            // shrinks is dominated by data that resizing keeps, such as metadata.
            _ if bytes.len() > previous_len / 10 * 9 => return Err(IMAGE_TOO_LARGE),
            _ => {
                previous_len = bytes.len();
                long_edge * 3 / 4
            }
        };
        if next == 0 {
            return Err(IMAGE_TOO_LARGE);
        }
        long_edge = next;
    }
}

/// Prepares a prompt image the way tool-result images are prepared: an inline
/// image is decoded, converted to a format Claude accepts, and reduced to the
/// model's native resolution and the per-image byte limit. An image Claude
/// cannot use, including any remote URL, yields the note that replaces it.
async fn prepare_image(
    image_url: String,
    resolution: ImageResolution,
) -> std::result::Result<String, &'static str> {
    let Some(data) = image_url
        .get(..5)
        .filter(|scheme| scheme.eq_ignore_ascii_case("data:"))
        .map(|_| &image_url[5..])
    else {
        return Err(IMAGE_URL_UNSUPPORTED);
    };
    let Some((header, payload)) = data.split_once(',') else {
        return Err(IMAGE_UNPROCESSABLE);
    };
    if !header
        .split(';')
        .any(|parameter| parameter.eq_ignore_ascii_case("base64"))
    {
        return Err(IMAGE_UNPROCESSABLE);
    }
    if payload.len() > MAX_SOURCE_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(IMAGE_TOO_LARGE);
    }
    let (data, media_type) = prepare_base64(payload, resolution).await?;
    Ok(format!("data:{media_type};base64,{data}"))
}

fn image_type(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Ok("image/webp")
    } else {
        Err(invalid(
            "Claude images require PNG, JPEG, GIF, or WebP bytes",
        ))
    }
}

pub(crate) fn image_source(value: &str) -> Result<(serde_json::Value, usize)> {
    if let Some(data) = value.strip_prefix("data:") {
        if value.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 + 64 {
            return Err(invalid("Claude image exceeds 5 MiB"));
        }
        let (header, data) = data
            .split_once(',')
            .ok_or_else(|| invalid("invalid Claude image data URL"))?;
        let media_type = header
            .strip_suffix(";base64")
            .ok_or_else(|| invalid("Claude image data URL must use base64"))?;
        if !matches!(
            media_type,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return Err(invalid("unsupported Claude image media type"));
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| invalid("invalid Claude image base64"))?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(invalid("Claude image must contain 1 byte through 5 MiB"));
        }
        if image_type(&bytes)? != media_type {
            return Err(invalid("Claude image media type does not match its bytes"));
        }
        Ok((
            json!({"type":"base64","media_type":media_type,"data":data}),
            bytes.len(),
        ))
    } else {
        if value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(invalid(
                "Claude image URL must not contain whitespace or control characters",
            ));
        }
        if value.len() > 8192 {
            return Err(invalid("Claude image URL exceeds 8192 bytes"));
        }
        let url = url::Url::parse(value).map_err(|_| invalid("invalid Claude image URL"))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "Claude image URL must use HTTPS without credentials or a fragment",
            ));
        }
        Ok((json!({"type":"url","url":value}), value.len()))
    }
}

/// Maps an inline document data URL to a native Claude document source.
/// PDFs stay base64; plain text is decoded into a text source.
pub(crate) fn document_block(
    file_data: &str,
    filename: Option<&str>,
) -> Result<(ContentBlock, usize)> {
    if file_data.len() > MAX_DOCUMENT_BYTES.div_ceil(3) * 4 + 64 {
        return Err(invalid("Claude document exceeds 10 MiB"));
    }
    let (header, data) = file_data
        .strip_prefix("data:")
        .and_then(|value| value.split_once(','))
        .ok_or_else(|| invalid("Claude documents require a base64 data URL"))?;
    let media_type = header
        .strip_suffix(";base64")
        .ok_or_else(|| invalid("Claude document data URL must use base64"))?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| invalid("invalid Claude document base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid(
            "Claude document must contain 1 byte through 10 MiB",
        ));
    }
    let source = match media_type {
        "application/pdf" => {
            if !bytes.starts_with(b"%PDF-") {
                return Err(invalid(
                    "Claude document media type does not match its bytes",
                ));
            }
            json!({"type":"base64","media_type":"application/pdf","data":data})
        }
        "text/plain" => {
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| invalid("Claude text document must be UTF-8"))?;
            json!({"type":"text","media_type":"text/plain","data":text})
        }
        _ => {
            return Err(invalid(
                "Claude documents support application/pdf and text/plain",
            ));
        }
    };
    let mut extra = std::collections::BTreeMap::new();
    if let Some(name) = filename {
        if name.trim().is_empty()
            || name.len() > MAX_FILENAME_BYTES
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err(invalid(
                "Claude document filename must be 1-255 bytes without paths or control characters",
            ));
        }
        extra.insert("title".to_owned(), json!(name));
    }
    Ok((ContentBlock::Document { source, extra }, bytes.len()))
}

/// Reads a local image as a data URL for preparation, which identifies its
/// format from the bytes, or returns why it cannot be read.
#[cfg(not(target_family = "wasm"))]
fn local_image(path: &std::path::Path) -> std::result::Result<String, &'static str> {
    use std::io::Read as _;
    const NOT_A_FILE: &str = "is not a regular file of at most 64 MiB";
    const UNREADABLE: &str = "could not be read";
    // Reject nonregular paths before opening (in particular FIFOs). Check again
    // on the opened handle, and bound the actual read even if the file grows.
    let metadata = std::fs::metadata(path).map_err(|_| UNREADABLE)?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_IMAGE_BYTES as u64 {
        return Err(NOT_A_FILE);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| UNREADABLE)?;
    let metadata = file.metadata().map_err(|_| UNREADABLE)?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_IMAGE_BYTES as u64 {
        return Err(NOT_A_FILE);
    }
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNREADABLE)?;
    if bytes.len() > MAX_SOURCE_IMAGE_BYTES {
        return Err(NOT_A_FILE);
    }
    Ok(format!(
        "data:application/octet-stream;base64,{}",
        STANDARD.encode(bytes)
    ))
}

/// Local files need a native filesystem.
#[cfg(target_family = "wasm")]
fn local_image(_path: &std::path::Path) -> std::result::Result<String, &'static str> {
    Err("requires a native filesystem")
}

/// Freeze local paths exactly once and prepare inline images for the model's
/// native resolution; portable images need no host capability. An image Claude
/// cannot use is replaced by a note that tells the model why, and the rest of
/// the prompt is kept. Claude has no OpenAI image-detail field, so detail
/// hints are not forwarded.
pub(crate) async fn freeze(mut prompt: Prompt, resolution: ImageResolution) -> Result<Prompt> {
    if let PromptInput::Content(items) = &mut prompt.instruction {
        if items.len() > 100 {
            return Err(invalid("Claude prompt exceeds 100 content items"));
        }
        let images = items
            .iter()
            .filter(|item| matches!(item, UserInput::Image { .. } | UserInput::LocalImage { .. }))
            .count();
        if images > MAX_IMAGES {
            return Err(invalid("Claude prompt exceeds 20 images"));
        }
        let mut documents = 0;
        let mut total = 0;
        for item in items {
            let (image_url, detail, local) = match item {
                UserInput::File {
                    file_data,
                    filename,
                } => {
                    documents += 1;
                    if documents > MAX_DOCUMENTS {
                        return Err(invalid("Claude prompt exceeds 5 documents"));
                    }
                    total += document_block(file_data, filename.as_deref())?.1;
                    if total > MAX_TOTAL_BYTES {
                        return Err(invalid("Claude prompt exceeds 20 MiB of media data"));
                    }
                    continue;
                }
                UserInput::ImageFile { .. } => {
                    *item = UserInput::Text {
                        text: IMAGE_FILE_UNSUPPORTED.into(),
                    };
                    continue;
                }
                UserInput::Image { image_url, detail } => {
                    (std::mem::take(image_url), *detail, None)
                }
                UserInput::LocalImage { path, detail } => match local_image(path) {
                    Ok(image_url) => (image_url, *detail, Some(path.display().to_string())),
                    Err(reason) => {
                        *item = UserInput::Text {
                            text: format!(
                                "image content omitted because the local image at `{}` {reason}",
                                path.display()
                            ),
                        };
                        continue;
                    }
                },
                _ => continue,
            };
            *item = match prepare_image(image_url, resolution).await {
                Ok(image_url) => {
                    total += image_source(&image_url)?.1;
                    if total > MAX_TOTAL_BYTES {
                        return Err(invalid("Claude prompt exceeds 20 MiB of media data"));
                    }
                    UserInput::Image { image_url, detail }
                }
                Err(note) => UserInput::Text {
                    text: match local {
                        Some(path) => format!("{note} (local image at `{path}`)"),
                        None => note.into(),
                    },
                },
            };
        }
    }
    messages(&prompt)?;
    Ok(prompt)
}

pub(crate) fn messages(prompt: &Prompt) -> Result<Vec<Message>> {
    let mut messages = prompt
        .transcript()
        .iter()
        .map(|item| {
            Message::text(
                match item.role() {
                    PromptMessageRole::User => Role::User,
                    PromptMessageRole::Assistant => Role::Assistant,
                },
                item.content(),
            )
        })
        .collect::<Vec<_>>();
    let content = match &prompt.instruction {
        PromptInput::Text(text) => vec![ContentBlock::text(text)],
        PromptInput::Content(items) => {
            if items.len() > 100 {
                return Err(invalid("Claude prompt exceeds 100 content items"));
            }
            let mut images = 0;
            let mut documents = 0;
            let mut bytes = 0;
            let mut content = Vec::with_capacity(items.len());
            for item in items {
                content.push(match item {
                    UserInput::Text { text } => ContentBlock::text(text),
                    UserInput::Image { image_url, .. } => {
                        images += 1;
                        let (source, size) = image_source(image_url)?;
                        bytes += size;
                        if images > MAX_IMAGES || bytes > MAX_TOTAL_BYTES { return Err(invalid("Claude prompt exceeds 20 images or 20 MiB of media data")); }
                        ContentBlock::Image { source, extra: Default::default() }
                    }
                    UserInput::File { file_data, filename } => {
                        documents += 1;
                        let (block, size) = document_block(file_data, filename.as_deref())?;
                        bytes += size;
                        if documents > MAX_DOCUMENTS || bytes > MAX_TOTAL_BYTES { return Err(invalid("Claude prompt exceeds 5 documents or 20 MiB of media data")); }
                        block
                    }
                    UserInput::LocalImage { .. } => return Err(invalid("Claude local image was not frozen before execution")),
                    UserInput::ImageFile { .. } => return Err(invalid("Claude cannot use opaque OpenAI image file IDs; supply a data URL or local image")),
                    UserInput::Audio { .. } | UserInput::LocalAudio { .. } => return Err(invalid("Claude audio prompts are unsupported")),
                });
            }
            content
        }
    };
    messages.push(Message {
        role: Role::User,
        content,
    });
    Ok(messages)
}

/// Admission identity retains paths, while its settled media receipt retains bytes.
/// A terminal replay never calls this function, and a resume never reopens a
/// local image after its receipt has committed.
///
/// Returns the prompt with the image resolution of the model the operation's
/// requests name, `resolution` unless a continuation names another. An agent
/// reopened with another model resumes a continued operation on the model its
/// continuation names, while an operation without one starts on the agent's.
pub(crate) async fn freeze_admitted(
    prompt: Prompt,
    policy: &dyn crate::execution::ClaudeExecutionPolicy,
    id: &str,
    resolution: ImageResolution,
) -> Result<(Prompt, ImageResolution)> {
    let continuation = policy.continuation(id.into()).await?;
    let resolution = continuation
        .as_ref()
        .and_then(|cursor| cursor["template"]["model"].as_str())
        .map_or(resolution, ImageResolution::of);
    let has_local = matches!(&prompt.instruction, PromptInput::Content(items) if items.iter().any(|item| matches!(item, UserInput::LocalImage { .. })));
    if !has_local {
        return Ok((freeze(prompt, resolution).await?, resolution));
    }
    // A cursor advance incorporates and retires settled step receipts. Its
    // admitted media must therefore survive in the continuation itself.
    if let Some(cursor) = continuation {
        let frozen: Prompt = serde_json::from_value(
            cursor.get("frozen_prompt").filter(|value| !value.is_null()).cloned()
                .ok_or_else(|| invalid("Claude media recovery is missing its frozen prompt; local files were not reopened"))?
        ).map_err(|_| invalid("invalid frozen Claude prompt continuation"))?;
        messages(&frozen)?;
        return Ok((frozen, resolution));
    }
    let input =
        serde_json::to_value(&prompt).map_err(|_| invalid("cannot encode Claude prompt"))?;
    let frozen = match policy
        .begin_step(
            id.into(),
            "prompt-media".into(),
            "claude_prompt_media".into(),
            input,
        )
        .await?
    {
        // The receipt was prepared for the model at its first admission, which
        // a reopened agent may have changed before any request was sent.
        crate::execution::Step::Replay(value) => {
            let receipt = serde_json::from_value(value)
                .map_err(|_| invalid("invalid frozen Claude prompt receipt"))?;
            freeze(receipt, resolution).await?
        }
        crate::execution::Step::Execute => {
            let frozen = freeze(prompt, resolution).await?;
            let value = serde_json::to_value(&frozen)
                .map_err(|_| invalid("cannot encode frozen Claude prompt"))?;
            policy
                .complete_step(id.into(), "prompt-media".into(), value)
                .await?;
            frozen
        }
    };
    Ok((frozen, resolution))
}
