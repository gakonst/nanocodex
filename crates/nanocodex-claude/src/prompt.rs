//! Portable, bounded Claude prompt media. Native files are frozen before acceptance.
use crate::{ContentBlock, Message, Role};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_agent::{
    NanocodexError, Result,
    input::{Prompt, PromptInput, PromptMessageRole, UserInput},
};
use serde_json::json;

const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
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

fn image_source(value: &str) -> Result<(serde_json::Value, usize)> {
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
fn document_block(file_data: &str, filename: Option<&str>) -> Result<(ContentBlock, usize)> {
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

#[cfg(not(target_family = "wasm"))]
fn local_image(path: &std::path::Path) -> Result<String> {
    use std::io::Read as _;
    // Reject nonregular paths before opening (in particular FIFOs). Check again
    // on the opened handle, and bound the actual read even if the file grows.
    let metadata =
        std::fs::metadata(path).map_err(|_| invalid("cannot inspect Claude local image"))?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(invalid(
            "Claude local image must be a regular file no larger than 5 MiB",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| invalid("cannot open Claude local image"))?;
    let metadata = file
        .metadata()
        .map_err(|_| invalid("cannot inspect opened Claude local image"))?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(invalid(
            "Claude local image must be a regular file no larger than 5 MiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read Claude local image"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid("Claude local image exceeds 5 MiB"));
    }
    let media_type = image_type(&bytes)?;
    Ok(format!(
        "data:{media_type};base64,{}",
        STANDARD.encode(bytes)
    ))
}

/// Freeze local paths exactly once; portable images need no host capability.
/// Claude has no OpenAI image-detail field, so detail hints are not forwarded.
pub(crate) fn freeze(mut prompt: Prompt) -> Result<Prompt> {
    if let PromptInput::Content(items) = &mut prompt.instruction {
        if items.len() > 100 {
            return Err(invalid("Claude prompt exceeds 100 content items"));
        }
        let mut images = 0;
        let mut documents = 0;
        let mut total = 0;
        for item in items {
            if let UserInput::File {
                file_data,
                filename,
            } = item
            {
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
            if matches!(item, UserInput::Image { .. } | UserInput::LocalImage { .. }) {
                images += 1;
                if images > MAX_IMAGES {
                    return Err(invalid("Claude prompt exceeds 20 images"));
                }
            }
            if let UserInput::LocalImage { path, detail } = item {
                #[cfg(not(target_family = "wasm"))]
                {
                    *item = UserInput::Image {
                        image_url: local_image(path)?,
                        detail: *detail,
                    };
                }
                #[cfg(target_family = "wasm")]
                {
                    let _ = (path, detail);
                    return Err(invalid(
                        "Claude local images require a native filesystem; use an image URL or data URL",
                    ));
                }
            }
            if let UserInput::Image { image_url, .. } = item {
                total += image_source(image_url)?.1;
                if total > MAX_TOTAL_BYTES {
                    return Err(invalid("Claude prompt exceeds 20 MiB of media data"));
                }
            }
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
                    UserInput::ImageFile { .. } => return Err(invalid("Claude cannot use opaque OpenAI image file IDs; supply an image URL or local image")),
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
pub(crate) async fn freeze_admitted(
    prompt: Prompt,
    policy: &dyn crate::execution::ClaudeExecutionPolicy,
    id: &str,
) -> Result<Prompt> {
    let has_local = matches!(&prompt.instruction, PromptInput::Content(items) if items.iter().any(|item| matches!(item, UserInput::LocalImage { .. })));
    if !has_local {
        return freeze(prompt);
    }
    // A cursor advance incorporates and retires settled step receipts. Its
    // admitted media must therefore survive in the continuation itself.
    if let Some(cursor) = policy.continuation(id.into()).await? {
        let frozen: Prompt = serde_json::from_value(
            cursor.get("frozen_prompt").filter(|value| !value.is_null()).cloned()
                .ok_or_else(|| invalid("Claude media recovery is missing its frozen prompt; local files were not reopened"))?
        ).map_err(|_| invalid("invalid frozen Claude prompt continuation"))?;
        messages(&frozen)?;
        return Ok(frozen);
    }
    let input =
        serde_json::to_value(&prompt).map_err(|_| invalid("cannot encode Claude prompt"))?;
    match policy
        .begin_step(
            id.into(),
            "prompt-media".into(),
            "claude_prompt_media".into(),
            input,
        )
        .await?
    {
        crate::execution::Step::Replay(value) => serde_json::from_value(value)
            .map_err(|_| invalid("invalid frozen Claude prompt receipt")),
        crate::execution::Step::Execute => {
            let frozen = freeze(prompt)?;
            let value = serde_json::to_value(&frozen)
                .map_err(|_| invalid("cannot encode frozen Claude prompt"))?;
            policy
                .complete_step(id.into(), "prompt-media".into(), value)
                .await?;
            Ok(frozen)
        }
    }
}
