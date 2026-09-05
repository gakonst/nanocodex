use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use nanocodex_oai_api::tools::ToolDefinition;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{
    ImageDetail, StandardTool, Tool, ToolContext, ToolInput, ToolOutput, ToolOutputContent,
    ToolResult,
};

pub(crate) struct ViewImageHandler {
    workspace: PathBuf,
    max_wire_bytes: Option<u64>,
}

impl ViewImageHandler {
    pub(crate) const fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            max_wire_bytes: None,
        }
    }

    pub(crate) const fn with_wire_limit(workspace: PathBuf, max_wire_bytes: u64) -> Self {
        Self {
            workspace,
            max_wire_bytes: Some(max_wire_bytes),
        }
    }
}

#[async_trait::async_trait]
impl Tool for ViewImageHandler {
    fn definition(&self) -> ToolDefinition {
        StandardTool::ViewImage.definition()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let arguments = input.decode_json::<ViewImageArguments>()?;
        let detail = match arguments.detail.as_deref() {
            None | Some("high") => ImageDetail::High,
            Some("original") => ImageDetail::Original,
            Some(detail) => {
                return Ok(ToolOutput::error(format!(
                    "view_image.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `{detail}`"
                )));
            }
        };
        let path = resolve(&self.workspace, Path::new(&arguments.path));
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "unable to locate image at `{}`: {error}",
                    path.display()
                )));
            }
        };
        let metadata = match file.metadata().await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                return Ok(ToolOutput::error(format!(
                    "image path `{}` is not a file",
                    path.display()
                )));
            }
            Err(error) => {
                return Ok(ToolOutput::error(format!(
                    "unable to locate image at `{}`: {error}",
                    path.display()
                )));
            }
        };
        let bytes = match self.max_wire_bytes {
            Some(max_wire_bytes) => {
                let max_raw_bytes = max_raw_bytes(max_wire_bytes);
                if metadata.len() > max_raw_bytes {
                    return Ok(oversized_image_error(&path, metadata.len(), max_wire_bytes));
                }
                match read_bounded(&mut file, metadata.len(), max_raw_bytes).await {
                    Ok(BoundedRead::Complete(bytes)) => bytes,
                    Ok(BoundedRead::TooLarge) => {
                        return Ok(growing_image_error(&path, max_raw_bytes, max_wire_bytes));
                    }
                    Err(error) => return Ok(read_error(&path, error)),
                }
            }
            None => {
                let mut bytes = Vec::new();
                match file.read_to_end(&mut bytes).await {
                    Ok(_) => bytes,
                    Err(error) => return Ok(read_error(&path, error)),
                }
            }
        };
        // The model-history boundary owns image validation, resizing, and caching.
        let image_url = format!(
            "data:application/octet-stream;base64,{}",
            STANDARD.encode(bytes)
        );
        Ok(ToolOutput::content(vec![ToolOutputContent::InputImage {
            image_url: image_url.clone(),
            detail,
        }])
        .with_structured_result(json!({
            "image_url": image_url,
            "detail": detail,
        })))
    }
}

const DATA_URL_PREFIX_BYTES: u64 = 37;
const VIEW_IMAGE_WIRE_COPIES: u64 = 2;
const VIEW_IMAGE_WIRE_HEADROOM_BYTES: u64 = 4 * 1024;
const INITIAL_IMAGE_READ_CAPACITY_BYTES: u64 = 64 * 1024;

fn max_raw_bytes(max_wire_bytes: u64) -> u64 {
    let fixed_bytes = DATA_URL_PREFIX_BYTES
        .saturating_mul(VIEW_IMAGE_WIRE_COPIES)
        .saturating_add(VIEW_IMAGE_WIRE_HEADROOM_BYTES);
    max_wire_bytes
        .saturating_sub(fixed_bytes)
        .checked_div(VIEW_IMAGE_WIRE_COPIES)
        .unwrap_or(0)
        .checked_div(4)
        .unwrap_or(0)
        .saturating_mul(3)
}

fn estimated_wire_bytes(raw_bytes: u64) -> u64 {
    raw_bytes
        .div_ceil(3)
        .checked_mul(4)
        .and_then(|encoded| encoded.checked_add(DATA_URL_PREFIX_BYTES))
        .and_then(|data_url| data_url.checked_mul(VIEW_IMAGE_WIRE_COPIES))
        .and_then(|duplicated| duplicated.checked_add(VIEW_IMAGE_WIRE_HEADROOM_BYTES))
        .unwrap_or(u64::MAX)
}

enum BoundedRead {
    Complete(Vec<u8>),
    TooLarge,
}

async fn read_bounded(
    reader: &mut (impl AsyncRead + Unpin),
    expected_bytes: u64,
    max_raw_bytes: u64,
) -> std::io::Result<BoundedRead> {
    let read_limit = max_raw_bytes.saturating_add(1);
    let initial_capacity = usize::try_from(
        expected_bytes
            .min(read_limit)
            .min(INITIAL_IMAGE_READ_CAPACITY_BYTES),
    )
    .unwrap_or(0);
    let mut bytes = Vec::with_capacity(initial_capacity);
    reader.take(read_limit).read_to_end(&mut bytes).await?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_raw_bytes {
        Ok(BoundedRead::TooLarge)
    } else {
        Ok(BoundedRead::Complete(bytes))
    }
}

fn oversized_image_error(path: &Path, raw_bytes: u64, max_wire_bytes: u64) -> ToolOutput {
    let estimated_wire_bytes = estimated_wire_bytes(raw_bytes);
    ToolOutput::error(format!(
        "image at `{}` is {raw_bytes} bytes and its VM transfer would require at least \
         {estimated_wire_bytes} bytes, exceeding the {max_wire_bytes}-byte VM tool frame limit; \
         resize or convert it to a compact PNG, JPEG, or WebP inside the VM (for example with \
         ffmpeg or ImageMagick), then call view_image on the smaller file",
        path.display(),
    ))
}

fn growing_image_error(path: &Path, max_raw_bytes: u64, max_wire_bytes: u64) -> ToolOutput {
    ToolOutput::error(format!(
        "image at `{}` exceeded the {max_raw_bytes}-byte VM raw-image limit while it was read \
         (the file may have grown), so its response cannot fit the {max_wire_bytes}-byte VM tool \
         frame; resize or convert it to a compact PNG, JPEG, or WebP inside the VM (for example \
         with ffmpeg or ImageMagick), then call view_image on the smaller file",
        path.display(),
    ))
}

fn read_error(path: &Path, error: std::io::Error) -> ToolOutput {
    ToolOutput::error(format!(
        "unable to read image at `{}`: {error}",
        path.display()
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewImageArguments {
    path: String,
    #[serde(default)]
    detail: Option<String>,
}

fn resolve(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        workspace.join(path)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{File, write},
        io::Cursor,
    };

    use nanocodex_oai_api::tools::{ToolInput, ToolOutputBody};
    use serde_json::{json, value::to_raw_value};
    use tempfile::tempdir;

    use super::*;

    const VM_FRAME_BYTES: u64 = 64 * 1024 * 1024;
    const PATH_TRACING_IMAGE_BYTES: u64 = 48_262_737;

    #[tokio::test]
    async fn vm_wire_limit_rejects_the_path_tracing_image_before_reading_it() {
        let workspace = tempdir().unwrap();
        let image = workspace.path().join("image.ppm");
        File::create(&image)
            .unwrap()
            .set_len(PATH_TRACING_IMAGE_BYTES)
            .unwrap();
        let handler =
            ViewImageHandler::with_wire_limit(workspace.path().to_owned(), VM_FRAME_BYTES);
        let output = handler
            .execute(
                ToolInput::Function(
                    to_raw_value(&json!({
                        "path": image,
                        "detail": "original",
                    }))
                    .unwrap(),
                ),
                ToolContext::new("model", "session", "call", &[], 10_000),
            )
            .await
            .unwrap();

        assert!(!output.success);
        let ToolOutputBody::Text(error) = output.output else {
            panic!("oversized VM image should return a bounded text error");
        };
        assert!(error.contains("48262737 bytes"));
        assert!(error.contains("VM tool frame limit"));
        assert!(error.contains("resize or convert"));
        assert!(error.contains("PNG, JPEG, or WebP"));
    }

    #[tokio::test]
    async fn bounded_reader_stops_at_raw_cap_plus_one_when_file_grows() {
        const EXPECTED_BYTES: u64 = 3;
        const MAX_RAW_BYTES: u64 = 1_024;

        let mut grown_file = Cursor::new(vec![b'x'; 4_096]);
        let result = read_bounded(&mut grown_file, EXPECTED_BYTES, MAX_RAW_BYTES)
            .await
            .unwrap();

        assert!(matches!(result, BoundedRead::TooLarge));
        assert_eq!(grown_file.position(), MAX_RAW_BYTES + 1);
    }

    #[tokio::test]
    async fn bounded_vm_read_preserves_image_payload_and_detail() {
        let workspace = tempdir().unwrap();
        let image = workspace.path().join("small.png");
        write(&image, [0, 1, 2]).unwrap();
        let handler =
            ViewImageHandler::with_wire_limit(workspace.path().to_owned(), VM_FRAME_BYTES);

        let output = handler
            .execute(
                ToolInput::Function(
                    to_raw_value(&json!({
                        "path": image,
                        "detail": "original",
                    }))
                    .unwrap(),
                ),
                ToolContext::new("model", "session", "call", &[], 10_000),
            )
            .await
            .unwrap();

        assert!(output.success);
        let ToolOutputBody::Content(content) = output.output else {
            panic!("view_image should preserve its multimodal output");
        };
        let [ToolOutputContent::InputImage { image_url, detail }] = content.as_slice() else {
            panic!("view_image should return exactly one image");
        };
        assert_eq!(image_url, "data:application/octet-stream;base64,AAEC");
        assert_eq!(*detail, ImageDetail::Original);
    }

    #[test]
    fn wire_estimate_accounts_for_both_data_url_copies() {
        assert_eq!(estimated_wire_bytes(PATH_TRACING_IMAGE_BYTES), 128_704_802);
        assert!(estimated_wire_bytes(PATH_TRACING_IMAGE_BYTES) > VM_FRAME_BYTES);
        let max_raw_bytes = max_raw_bytes(VM_FRAME_BYTES);
        assert!(estimated_wire_bytes(max_raw_bytes) <= VM_FRAME_BYTES);
        assert!(estimated_wire_bytes(max_raw_bytes + 1) > VM_FRAME_BYTES);
    }
}
