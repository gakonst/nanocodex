//! Bounded native Read media. Images and PDF rasters remain Claude image blocks.
//!
//! PDF helpers are trusted host configuration, run without a shell or network
//! arguments. The embedding must authorize and OS-isolate their filesystem and
//! memory access, just as it isolates the workspace's other native operations.
use crate::{ImageSource, SessionEnvironment, ToolOutput, ToolResultBlock};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_SOURCE: usize = 16 * 1024 * 1024;
const MAX_IMAGE: usize = 5 * 1024 * 1024;
const MAX_MEDIA: usize = 20 * 1024 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_BLOCKS: usize = 256;

/// Trusted executable locations for PDF reading. Install Poppler to use defaults.
#[derive(Clone, Debug)]
pub struct MediaReadOptions {
    /// Poppler's pdfinfo executable, never supplied by model tool arguments.
    pub pdfinfo: PathBuf,
    /// Poppler's pdftoppm executable, never supplied by model tool arguments.
    pub pdftoppm: PathBuf,
}
impl Default for MediaReadOptions {
    fn default() -> Self {
        Self {
            pdfinfo: "pdfinfo".into(),
            pdftoppm: "pdftoppm".into(),
        }
    }
}

pub(crate) fn read(
    path: &Path,
    input: &Value,
    options: &MediaReadOptions,
    session: Option<&SessionEnvironment>,
) -> Result<Option<ToolOutput>, String> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Check the signature as well as the extension, so extensionless screenshots
    // and PDFs still use the media route instead of UTF-8 decoding.
    let mut prefix = [0; 16];
    let count = File::open(path)
        .map_err(|e| format!("read: {e}"))?
        .read(&mut prefix)
        .map_err(|e| format!("read: {e}"))?;
    let signature = image::guess_format(&prefix[..count]).ok();
    let pdf = prefix[..count].starts_with(b"%PDF-") || extension == "pdf";
    let is_image = signature.is_some()
        || matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp");
    if !pdf && !is_image && extension != "ipynb" {
        return Ok(None);
    }
    if !pdf && input.get("pages").is_some() {
        return Err("pages is only applicable to PDF files".into());
    }
    if (pdf || is_image) && (input.get("offset").is_some() || input.get("limit").is_some()) {
        return Err(
            "offset and limit apply to text lines or notebook cells; use pages for PDF files"
                .into(),
        );
    }
    let bytes = read_bytes(path, if is_image { MAX_IMAGE } else { MAX_SOURCE })?;
    if pdf {
        return read_pdf(&bytes, input, options, session).map(Some);
    }
    if is_image {
        let block = image_block(&bytes)?;
        return Ok(Some(
            ToolOutput::content(vec![block])
                .with_metadata(json!({"kind":"image","bytes":bytes.len()})),
        ));
    }
    read_notebook(&bytes, input).map(Some)
}

fn read_bytes(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("read media: {e}"))?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read media: {e}"))?;
    if bytes.len() > max {
        return Err(format!("media exceeds {} MiB limit", max / (1024 * 1024)));
    }
    Ok(bytes)
}

fn image_block(bytes: &[u8]) -> Result<ToolResultBlock, String> {
    if bytes.len() > MAX_IMAGE {
        return Err("image exceeds 5 MiB limit".into());
    }
    let format = image::guess_format(bytes).map_err(|e| format!("invalid image: {e}"))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err("unsupported image format; use PNG, JPEG, GIF or WebP".into()),
    };
    // Validate the real image, limiting decoded allocations as well as input bytes.
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8000);
    limits.max_image_height = Some(8000);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let _decoded = reader
        .decode()
        .map_err(|e| format!("invalid or oversized image: {e}"))?;
    Ok(ToolResultBlock::Image {
        source: ImageSource::Base64 {
            media_type: mime.into(),
            data: STANDARD.encode(bytes),
        },
    })
}

fn page_range(input: &Value, total: u32) -> Result<(u32, u32), String> {
    let Some(value) = input.get("pages") else {
        if total > 10 {
            return Err(format!(
                "PDF has {total} pages; specify pages (maximum 20 per request)"
            ));
        }
        return Ok((1, total));
    };
    let value = value
        .as_str()
        .ok_or("invalid pages; use a page or inclusive range such as 1-5")?;
    if value.len() > 32 {
        return Err("invalid pages".into());
    }
    let mut parts = value.split('-');
    let start = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("invalid pages")?;
    let end = match parts.next() {
        Some(s) => s.parse::<u32>().map_err(|_| "invalid pages")?,
        None => start,
    };
    if parts.next().is_some() || start == 0 || end < start || end > total {
        return Err(format!("pages must be within 1-{total}"));
    }
    if end - start >= 20 {
        return Err("PDF reads are limited to 20 pages per request".into());
    }
    Ok((start, end))
}

fn read_pdf(
    bytes: &[u8],
    input: &Value,
    options: &MediaReadOptions,
    session: Option<&SessionEnvironment>,
) -> Result<ToolOutput, String> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("invalid PDF header".into());
    }
    // A private snapshot makes every helper see the same authorized bytes, even
    // if the source is replaced while pages are rendering.
    let mut snapshot = tempfile::NamedTempFile::new().map_err(|e| format!("PDF snapshot: {e}"))?;
    snapshot
        .write_all(bytes)
        .map_err(|e| format!("PDF snapshot: {e}"))?;
    snapshot.flush().map_err(|e| format!("PDF snapshot: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut info = Command::new(&options.pdfinfo);
    info.arg(snapshot.path()).env("LC_ALL", "C");
    SessionEnvironment::apply_or_clear(session, &mut info);
    let info = helper(&mut info, 16 * 1024, deadline)?;
    let info = String::from_utf8(info).map_err(|_| "pdfinfo returned invalid text")?;
    if info
        .lines()
        .any(|l| l.starts_with("Encrypted:") && l.split_whitespace().nth(1) != Some("no"))
    {
        return Err("encrypted PDFs are not supported".into());
    }
    let pages = info
        .lines()
        .find_map(|l| {
            l.strip_prefix("Pages:")
                .and_then(|n| n.trim().parse::<u32>().ok())
        })
        .filter(|&n| n > 0)
        .ok_or("PDF has no readable pages")?;
    let (first, last) = page_range(input, pages)?;
    let mut blocks = Vec::new();
    let mut size = 0;
    for page in first..=last {
        let mut render = Command::new(&options.pdftoppm);
        render
            .args([
                "-f",
                &page.to_string(),
                "-l",
                &page.to_string(),
                "-singlefile",
                "-scale-to",
                "1600",
                "-png",
            ])
            .arg(snapshot.path())
            .env("LC_ALL", "C");
        SessionEnvironment::apply_or_clear(session, &mut render);
        let raster = helper(&mut render, MAX_IMAGE, deadline)?;
        size += raster.len();
        if size > MAX_MEDIA {
            return Err("PDF rendered output exceeds 20 MiB; request fewer pages".into());
        }
        blocks.push(ToolResultBlock::Text {
            text: format!("PDF page {page} of {pages}\n"),
        });
        blocks.push(image_block(&raster)?);
    }
    Ok(ToolOutput::content(blocks).with_metadata(
        json!({"kind":"pdf","total_pages":pages,"first_page":first,"last_page":last}),
    ))
}

// Pipes are drained concurrently in bounded reader threads. This avoids pipe
// deadlocks and accumulating unbounded stdout/stderr in memory or temporary files.
fn helper(command: &mut Command, max: usize, deadline: Instant) -> Result<Vec<u8>, String> {
    if Instant::now() >= deadline {
        return Err("PDF rendering exceeded 30 seconds".into());
    }
    // A dedicated process group lets timeout/output-limit cleanup close pipes
    // inherited by helper subprocesses as well as terminate the direct child.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("PDF helper unavailable: {e}; install Poppler pdfinfo and pdftoppm or configure MediaReadOptions"))?;
    let stdout = child.stdout.take().ok_or("PDF helper stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("PDF helper stderr unavailable")?;
    let (send, recv) = std::sync::mpsc::channel();
    let send_err = send.clone();
    let out_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((max + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("PDF helper output: {e}"))
            .and_then(|_| {
                if bytes.len() > max {
                    Err("PDF helper output exceeds limit".into())
                } else {
                    Ok(bytes)
                }
            });
        let _ = send.send((true, result));
    });
    let err_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("PDF helper diagnostic: {e}"))
            .and_then(|_| {
                if bytes.len() > 16 * 1024 {
                    Err("PDF helper diagnostics exceed limit".into())
                } else {
                    Ok(bytes)
                }
            });
        let _ = send_err.send((false, result));
    });
    let result = (|| {
        let mut out = None;
        let mut err = None;
        while out.is_none() || err.is_none() {
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10));
            if wait.is_zero() {
                return Err("PDF rendering exceeded 30 seconds".into());
            }
            match recv.recv_timeout(wait) {
                Ok((true, bytes)) => out = Some(bytes?),
                Ok((false, bytes)) => err = Some(bytes?),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("PDF helper output disconnected".into()),
            }
        }
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("PDF helper wait: {e}"))?
            {
                if !status.success() {
                    return Err(format!(
                        "PDF helper failed: {}",
                        String::from_utf8_lossy(&err.unwrap_or_default()).trim()
                    ));
                }
                return Ok(out.unwrap_or_default());
            }
            if Instant::now() >= deadline {
                return Err("PDF rendering exceeded 30 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{Signal, killpg},
            unistd::Pid,
        };
        let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    }
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    // Never extend the deadline waiting for a pipe held by an escaped process.
    // Completed readers can be joined; dropping an unfinished handle detaches it.
    // Hosts still must isolate trusted helpers, particularly on non-Unix systems.
    if out_thread.is_finished() {
        let _ = out_thread.join();
    }
    if err_thread.is_finished() {
        let _ = err_thread.join();
    }
    result
}

fn notebook_text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|p| {
                p.as_str()
                    .ok_or_else(|| "invalid notebook text array".to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|p| p.concat()),
        _ => Err("invalid notebook text".into()),
    }
}

fn push_text(
    blocks: &mut Vec<ToolResultBlock>,
    used: &mut usize,
    text: String,
) -> Result<(), String> {
    *used += text.len();
    if *used > MAX_TEXT || blocks.len() >= MAX_BLOCKS {
        return Err(
            "notebook output exceeds limit; use offset and limit to select fewer cells".into(),
        );
    }
    blocks.push(ToolResultBlock::Text { text });
    Ok(())
}

fn read_notebook(bytes: &[u8], input: &Value) -> Result<ToolOutput, String> {
    let notebook: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid notebook JSON: {e}"))?;
    if notebook.get("nbformat").and_then(Value::as_u64) != Some(4) {
        return Err("Read supports nbformat 4 notebooks".into());
    }
    let cells = notebook
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("notebook has no cells array")?;
    let offset = input
        .get("offset")
        .map_or(Some(1), Value::as_u64)
        .filter(|&n| n > 0)
        .ok_or("invalid offset")?;
    let limit = input
        .get("limit")
        .map_or(Some(2000), Value::as_u64)
        .filter(|&n| n > 0)
        .ok_or("invalid limit")?;
    let mut blocks = Vec::new();
    let mut text_bytes = 0;
    let mut media_bytes = 0;
    for (index, cell) in cells
        .iter()
        .enumerate()
        .skip(offset.saturating_sub(1).min(usize::MAX as u64) as usize)
        .take(limit.min(2000) as usize)
    {
        let kind = cell
            .get("cell_type")
            .and_then(Value::as_str)
            .ok_or("notebook cell has no cell_type")?;
        let id = cell
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| index.to_string());
        let source = notebook_text(cell.get("source").ok_or("notebook cell has no source")?)?;
        push_text(
            &mut blocks,
            &mut text_bytes,
            format!("Cell {id} ({kind}, index {index})\n{source}\n"),
        )?;
        if let Some(outputs) = cell.get("outputs") {
            for output in outputs
                .as_array()
                .ok_or("notebook outputs must be an array")?
            {
                if let Some(text) = output.get("text") {
                    push_text(&mut blocks, &mut text_bytes, notebook_text(text)?)?;
                }
                if output.get("output_type").and_then(Value::as_str) == Some("error") {
                    push_text(
                        &mut blocks,
                        &mut text_bytes,
                        format!(
                            "{}: {}\n{}\n",
                            output
                                .get("ename")
                                .and_then(Value::as_str)
                                .unwrap_or("Error"),
                            output.get("evalue").and_then(Value::as_str).unwrap_or(""),
                            output
                                .get("traceback")
                                .map(notebook_text)
                                .transpose()?
                                .unwrap_or_default()
                        ),
                    )?;
                }
                if let Some(data) = output.get("data").and_then(Value::as_object) {
                    let mut image = false;
                    for mime in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
                        if let Some(value) = data.get(mime) {
                            let encoded = notebook_text(value)?;
                            if encoded.len() > MAX_IMAGE * 4 / 3 + 1024 {
                                return Err("notebook image exceeds 5 MiB limit".into());
                            }
                            let encoded: String = encoded
                                .chars()
                                .filter(|c| !c.is_ascii_whitespace())
                                .collect();
                            let bytes = STANDARD
                                .decode(encoded)
                                .map_err(|e| format!("invalid notebook image base64: {e}"))?;
                            media_bytes += bytes.len();
                            if media_bytes > MAX_MEDIA || blocks.len() >= MAX_BLOCKS {
                                return Err(
                                    "notebook media output exceeds limit; select fewer cells"
                                        .into(),
                                );
                            }
                            let block = image_block(&bytes)?;
                            // Reject a declared type inconsistent with the actual payload.
                            if let ToolResultBlock::Image {
                                source: ImageSource::Base64 { media_type, .. },
                            } = &block
                                && media_type != mime
                            {
                                return Err(
                                    "notebook image MIME type does not match its bytes".into()
                                );
                            }
                            blocks.push(block);
                            image = true;
                            break;
                        }
                    }
                    if let Some(text) = data.get("text/plain") {
                        push_text(&mut blocks, &mut text_bytes, notebook_text(text)?)?;
                    } else if !image {
                        // Preserve HTML/JSON/SVG as labeled source, never pretend it is raster media.
                        for (mime, value) in data {
                            let text = if mime == "application/json" {
                                value.to_string()
                            } else if value.is_string() || value.is_array() {
                                notebook_text(value)?
                            } else {
                                value.to_string()
                            };
                            push_text(
                                &mut blocks,
                                &mut text_bytes,
                                format!("Output ({mime})\n{text}\n"),
                            )?;
                        }
                    }
                }
            }
        }
    }
    if blocks.is_empty() {
        blocks.push(ToolResultBlock::Text {
            text: "No notebook cells in the requested range.\n".into(),
        });
    }
    Ok(ToolOutput::content(blocks)
        .with_metadata(json!({"kind":"notebook","total_cells":cells.len(),"offset":offset})))
}
