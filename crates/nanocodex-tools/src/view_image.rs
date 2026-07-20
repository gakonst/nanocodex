use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use nanocodex_core::ToolDefinition;
use serde::Deserialize;
use serde_json::json;

use super::{
    ImageDetail, Tool, ToolContext, ToolExecution, ToolInput, ToolOutputBody, ToolOutputContent,
};

pub(super) struct ViewImageHandler {
    workspace: PathBuf,
}

impl ViewImageHandler {
    pub(super) fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for ViewImageHandler {
    fn name(&self) -> &'static str {
        "view_image"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
            json!({
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
            }),
        )
        .with_output_schema(json!({
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
            }))
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
        let arguments = match input.decode_json::<ViewImageArguments>() {
            Ok(arguments) => arguments,
            Err(error) => return ToolExecution::error(error.to_string()),
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
        let detail = arguments.detail.unwrap_or(ImageDetailArgument::High).into();
        // The model-history boundary owns image validation, resizing, and caching.
        let image_url = format!(
            "data:application/octet-stream;base64,{}",
            STANDARD.encode(bytes)
        );
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
            process_trace: None,
        }
    }
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
