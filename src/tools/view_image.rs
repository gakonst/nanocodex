use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    ImageDetail, ToolContext, ToolExecution, ToolFuture, ToolHandler, ToolOutputBody,
    ToolOutputContent,
};

pub(super) struct ViewImageHandler {
    workspace: PathBuf,
}

impl ViewImageHandler {
    pub(super) fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

impl ToolHandler for ViewImageHandler {
    fn name(&self) -> &'static str {
        "view_image"
    }

    fn spec(&self) -> Value {
        json!({
            "type": "function",
            "name": self.name(),
            "description": "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
            "strict": false,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Local filesystem path to an image file."
                    },
                    "detail": {
                        "type": "string",
                        "enum": ["high", "original"],
                        "description": "Image detail level. Defaults to `high`; use `original` to preserve exact resolution."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            },
            "output_schema": {
                "type": "object",
                "properties": {
                    "image_url": {
                        "type": "string",
                        "description": "Data URL for the loaded image."
                    },
                    "detail": {
                        "type": "string",
                        "enum": ["high", "original"],
                        "description": "Image detail hint returned by view_image."
                    }
                },
                "required": ["image_url", "detail"],
                "additionalProperties": false
            }
        })
    }

    fn execute<'a>(&'a self, input: String, _context: ToolContext<'a>) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments = match serde_json::from_str::<ViewImageArguments>(&input) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return ToolExecution::error(format!(
                        "failed to parse view_image arguments: {error}"
                    ));
                }
            };
            let path = resolve(&self.workspace, Path::new(&arguments.path));
            match tokio::fs::metadata(&path).await {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => {
                    return ToolExecution::error(format!(
                        "image path `{}` is not a file",
                        path.display()
                    ));
                }
                Err(error) => {
                    return ToolExecution::error(format!(
                        "unable to locate image at `{}`: {error}",
                        path.display()
                    ));
                }
            }
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return ToolExecution::error(format!(
                        "unable to read image at `{}`: {error}",
                        path.display()
                    ));
                }
            };
            let (mime, bytes) = match prompt_image_bytes(&path, bytes) {
                Ok(image) => image,
                Err(error) => return ToolExecution::error(error),
            };
            let detail = arguments.detail.unwrap_or(ImageDetailArgument::High).into();
            let image_url = format!("data:{mime};base64,{}", STANDARD.encode(bytes));
            ToolExecution {
                output: ToolOutputBody::Content(vec![ToolOutputContent::InputImage {
                    image_url: image_url.clone(),
                    detail,
                }]),
                success: true,
                code_mode_value: Some(json!({
                    "image_url": image_url,
                    "detail": detail,
                })),
                metadata: None,
            }
        })
    }
}

fn prompt_image_bytes(path: &Path, bytes: Vec<u8>) -> Result<(&'static str, Vec<u8>), String> {
    let format = image::guess_format(&bytes)
        .map_err(|error| format!("unable to decode image at `{}`: {error}", path.display()))?;
    let mime = match format {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::WebP => Some("image/webp"),
        _ => None,
    };
    if let Some(mime) = mime {
        return Ok((mime, bytes));
    }

    let decoded = image::load_from_memory_with_format(&bytes, format)
        .map_err(|error| format!("unable to decode image at `{}`: {error}", path.display()))?;
    let mut encoded = Cursor::new(Vec::new());
    decoded
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|error| format!("unable to encode image at `{}`: {error}", path.display()))?;
    Ok(("image/png", encoded.into_inner()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewImageArguments {
    path: String,
    #[serde(default)]
    detail: Option<ImageDetailArgument>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ImageDetailArgument {
    High,
    Original,
}

impl From<ImageDetailArgument> for ImageDetail {
    fn from(detail: ImageDetailArgument) -> Self {
        match detail {
            ImageDetailArgument::High => Self::High,
            ImageDetailArgument::Original => Self::Original,
        }
    }
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
    use super::prompt_image_bytes;
    use std::path::Path;

    #[test]
    fn converts_portable_pixmap_to_supported_png() {
        let ppm = b"P6\n1 1\n255\n\xff\x00\x00".to_vec();
        let (mime, bytes) = prompt_image_bytes(Path::new("screen.ppm"), ppm)
            .expect("the regression fixture should decode");

        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
