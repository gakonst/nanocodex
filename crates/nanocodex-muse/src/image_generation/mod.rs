use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use nanocodex_oai_api::{
    auth::{OpenAiAuth, OpenAiAuthSnapshot},
    responses::{
        ContentItem, FunctionOutputBody, FunctionOutputContent, ImageReference, ResponseItem,
        valid_image_file_id,
    },
    tools::ToolDefinition,
};
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::image::load_for_prompt_data_url;
use nanocodex_oai_api::ImageDetail;
use nanocodex_oai_tools::{
    contract::{Tool, ToolContext, ToolInput, ToolOutput, ToolOutputContent, ToolResult},
    runtime::ImageGenerationConfig,
};

const DESCRIPTION: &str = include_str!("imagegen_description.md");
const IMAGE_MODEL: &str = "muse-image-1.0";
const MAX_EDIT_IMAGES: usize = 5;
const MAX_OUTPUT_HINT_BYTES: usize = 1_024;

pub(super) struct ImageGenerationHandler {
    client: reqwest::Client,
    responses_endpoint: String,
    auth: OpenAiAuth,
    save_root: PathBuf,
}

impl ImageGenerationHandler {
    pub(super) fn with_client(config: ImageGenerationConfig, client: reqwest::Client) -> Self {
        let api_base_url = config.api_base_url.trim_end_matches('/');
        Self {
            client,
            responses_endpoint: format!("{api_base_url}/responses"),
            auth: config.auth,
            save_root: config.save_root,
        }
    }

    async fn run(&self, input: &str, context: ToolContext<'_>) -> ToolOutput {
        let args = match serde_json::from_str::<ImagegenArgs>(input) {
            Ok(args) => args,
            Err(error) => {
                return ToolOutput::error(format!(
                    "failed to parse image_gen.imagegen arguments: {error}"
                ));
            }
        };
        if args.transparent_background {
            return ToolOutput::error(
                "Muse Image only produces opaque images; transparent_background is unsupported",
            );
        }
        let images = match selected_images(&args, context.history()).await {
            Ok(images) => images,
            Err(error) => return ToolOutput::error(error),
        };
        let (image, imagegen_request_id) =
            match self.post_image_request(&args.prompt, &images).await {
                Ok(response) => response,
                Err(error) => return error.output(),
            };
        let metadata = image_ids(imagegen_request_id, image.id);
        let mime = match image.format.as_str() {
            "png" => "image/png",
            "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            _ => {
                return ToolOutput::error("image generation returned an unsupported output format");
            }
        };
        let saved_path = save_result(
            &self.save_root,
            context.session_id(),
            context.call_id(),
            &image.b64,
            &image.format,
        )
        .await
        .ok();
        let image_url = format!("data:{mime};base64,{}", image.b64);
        let mut structured_result = metadata.clone();
        structured_result["image_url"] = Value::String(image_url.clone());
        let mut output_items = vec![ToolOutputContent::InputImage {
            image_url,
            detail: ImageDetail::High,
        }];
        if let Some(output_hint) = saved_path.as_ref().and_then(|path| image_output_hint(path)) {
            output_items.push(ToolOutputContent::InputText {
                text: output_hint.clone(),
            });
            structured_result["output_hint"] = Value::String(output_hint);
        }
        ToolOutput::content(output_items)
            .with_structured_result(structured_result)
            .with_metadata(metadata)
    }

    async fn post_image_request(
        &self,
        prompt: &str,
        images: &[ImageReference],
    ) -> Result<(MuseImage, Option<String>), ImageRequestFailure> {
        let operation = if images.is_empty() {
            "image generation"
        } else {
            "image edit"
        };
        let mut content = vec![json!({"type":"input_text","text":prompt})];
        for image in images {
            let mut image = serde_json::to_value(image)
                .map_err(|_| "failed to encode image request".to_owned())?;
            image["type"] = json!("input_image");
            content.push(image);
        }
        // Subscription credentials support Responses, but reject the Images API.
        // Muse Image accepts only its image_generation tool, never Spark's tools.
        let body = json!({
            "model":IMAGE_MODEL,
            "store":false,
            "input":[{"role":"user","content":content}],
            "tools":[{"type":"image_generation","output_format":"png","size":"auto"}]
        });
        let endpoint = &self.responses_endpoint;
        let auth = self
            .auth
            .snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let response = self.send_authorized(endpoint, &body, &auth).await?;
        let response = if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.auth
                .recover_unauthorized(&auth)
                .await
                .map_err(|error| error.to_string())?;
            let refreshed = self
                .auth
                .snapshot()
                .await
                .map_err(|error| error.to_string())?;
            self.send_authorized(endpoint, &body, &refreshed).await?
        } else {
            response
        };
        let status = response.status();
        let imagegen_request_id = response
            .headers()
            .get("x-codex-imagegen-request-id")
            .and_then(|value| value.to_str().ok())
            .and_then(safe_image_id);
        let body = response.bytes().await.map_err(|_| ImageRequestFailure {
            message: format!("failed to read {operation} response"),
            imagegen_request_id: imagegen_request_id.clone(),
            generation_id: None,
        })?;
        let body = serde_json::from_slice::<Value>(&body).ok();
        let generation_id = body
            .as_ref()
            .and_then(|value| value.get("generation_id"))
            .and_then(Value::as_str)
            .and_then(safe_image_id);
        if !status.is_success() {
            return Err(ImageRequestFailure {
                message: format!("{operation} returned HTTP {status}"),
                imagegen_request_id,
                generation_id,
            });
        }
        let image = muse_image_response(body).map_err(|message| ImageRequestFailure {
            message,
            imagegen_request_id: imagegen_request_id.clone(),
            generation_id,
        })?;
        Ok((image, imagegen_request_id))
    }

    async fn send_authorized(
        &self,
        endpoint: &str,
        body: &Value,
        auth: &OpenAiAuthSnapshot,
    ) -> Result<reqwest::Response, String> {
        let request = self
            .client
            .post(endpoint)
            .header(USER_AGENT, concat!("nanocodex/", env!("CARGO_PKG_VERSION")))
            .header(AUTHORIZATION, format!("Bearer {}", auth.bearer()))
            .header(crate::API_VERSION_HEADER.0, crate::API_VERSION_HEADER.1);
        request
            .json(body)
            .send()
            .await
            .map_err(|error| format!("image request failed: {error}"))
    }
}

#[async_trait::async_trait]
impl Tool for ImageGenerationHandler {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "image_gen__imagegen",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "transparent_background": { "type": "boolean", "default": false },
                    "referenced_image_paths": {
                        "type": ["array", "null"],
                        "items": {
                            "type": "string",
                            "description": "A path that is guaranteed to be absolute and normalized (though it is not guaranteed to be canonicalized or exist on the filesystem).\n\nIMPORTANT: When deserializing an `AbsolutePathBuf`, a base path must be set using [AbsolutePathBufGuard::new]. If no base path is set, the deserialization will fail unless the path being deserialized is already absolute."
                        }
                    },
                    "num_last_images_to_include": {
                        "type": ["integer", "null"]
                    }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let input = input.function_json()?;
        Ok(self.run(input.get(), context).await)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImagegenArgs {
    prompt: String,
    #[serde(default)]
    transparent_background: bool,
    #[serde(default)]
    referenced_image_paths: Option<Vec<PathBuf>>,
    #[serde(default)]
    num_last_images_to_include: Option<usize>,
}

struct MuseImage {
    id: Option<String>,
    b64: String,
    format: String,
}

fn muse_image_response(response: Option<Value>) -> Result<MuseImage, String> {
    let mut response = response.ok_or_else(|| "failed to decode Muse Image response".to_owned())?;
    if response["status"] != "completed" {
        return Err("Muse Image response did not complete".to_owned());
    }
    let image = response["output"]
        .as_array_mut()
        .and_then(|items| {
            items.iter_mut().find(|item| {
                item["type"] == "image_generation_call" && item["status"] == "completed"
            })
        })
        .ok_or_else(|| "Muse Image response contained no completed image".to_owned())?;
    let Value::String(b64) = image["result"].take() else {
        return Err("Muse Image response contained no image bytes".to_owned());
    };
    if b64.is_empty() {
        return Err("Muse Image response contained no image bytes".to_owned());
    }
    Ok(MuseImage {
        id: image["id"].as_str().and_then(safe_image_id),
        b64,
        format: image["output_format"].as_str().unwrap_or("png").to_owned(),
    })
}

struct ImageRequestFailure {
    message: String,
    imagegen_request_id: Option<String>,
    generation_id: Option<String>,
}
impl From<String> for ImageRequestFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            imagegen_request_id: None,
            generation_id: None,
        }
    }
}
impl ImageRequestFailure {
    fn output(self) -> ToolOutput {
        let metadata = image_ids(self.imagegen_request_id, self.generation_id);
        let mut result = metadata.clone();
        result["error"] = Value::String(self.message.clone());
        ToolOutput::error(self.message)
            .with_structured_result(result)
            .with_metadata(metadata)
    }
}
fn safe_image_id(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')))
    .then(|| value.to_owned())
}
fn image_ids(request_id: Option<String>, generation_id: Option<String>) -> Value {
    let mut result = json!({});
    if let Some(id) = request_id {
        result["imagegen_request_id"] = Value::String(id);
    }
    if let Some(id) = generation_id {
        result["generation_id"] = Value::String(id);
    }
    result
}

async fn selected_images(
    args: &ImagegenArgs,
    history: &[ResponseItem],
) -> Result<Vec<ImageReference>, String> {
    let paths = args.referenced_image_paths.as_deref().unwrap_or_default();
    if paths.len() > MAX_EDIT_IMAGES {
        return Err(format!(
            "`referenced_image_paths` must contain at most {MAX_EDIT_IMAGES} paths"
        ));
    }
    let images = match (paths.is_empty(), args.num_last_images_to_include) {
        (true, None) => return Ok(Vec::new()),
        (false, None) => {
            let mut images = Vec::with_capacity(paths.len());
            for path in paths {
                if !path.is_absolute() {
                    return Err(format!(
                        "referenced image path `{}` must be absolute",
                        path.display()
                    ));
                }
                images.push(ImageReference::Inline {
                    image_url: local_image_url(path.clone()).await?,
                });
            }
            images
        }
        (true, Some(count)) => {
            if !(1..=MAX_EDIT_IMAGES).contains(&count) {
                return Err(format!(
                    "`num_last_images_to_include` must be between 1 and {MAX_EDIT_IMAGES}"
                ));
            }
            let images = recent_images(history, count);
            if images.len() != count {
                return Err(format!(
                    "requested the last {count} conversation images, but only {} were available",
                    images.len()
                ));
            }
            images
        }
        (false, Some(_)) => {
            return Err(
                "provide only one of `referenced_image_paths` or `num_last_images_to_include`"
                    .to_owned(),
            );
        }
    };

    for image in &images {
        match image {
            ImageReference::File { file_id } if valid_image_file_id(file_id) => {}
            ImageReference::Inline { image_url }
                if image_url.starts_with("data:image/")
                    || image_url.starts_with("https://")
                    || image_url.starts_with("http://") => {}
            _ => {
                return Err(
                    "selected conversation image has an invalid or unsupported reference"
                        .to_owned(),
                );
            }
        }
    }
    Ok(images)
}

async fn local_image_url(path: PathBuf) -> Result<String, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|error| {
        format!(
            "unable to read referenced image at `{}`: {error}",
            path.display()
        )
    })?;
    let display_path = path.clone();
    tokio::task::spawn_blocking(move || {
        load_for_prompt_data_url(&path, bytes, ImageDetail::Original)
    })
    .await
    .map_err(|error| {
        format!(
            "unable to process referenced image at `{}`: {error}",
            display_path.display()
        )
    })?
    .map_err(|error| {
        format!(
            "unable to process referenced image at `{}`: {error}",
            display_path.display()
        )
    })
}

fn recent_images(history: &[ResponseItem], count: usize) -> Vec<ImageReference> {
    let mut images = Vec::with_capacity(count);
    'history: for item in history.iter().rev() {
        let references: Vec<ImageReference> = match item {
            ResponseItem::Message { content, .. } => {
                content.iter().rev().filter_map(content_image).collect()
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => output_images(output).collect(),
            ResponseItem::ImageGenerationCall { result, .. } if !result.is_empty() => {
                vec![ImageReference::Inline {
                    image_url: format!("data:image/png;base64,{result}"),
                }]
            }
            _ => Vec::new(),
        };
        for reference in references {
            images.push(reference);
            if images.len() == count {
                break 'history;
            }
        }
    }
    images.reverse();
    images
}
fn output_images(output: &FunctionOutputBody) -> impl Iterator<Item = ImageReference> + '_ {
    let content = match output {
        FunctionOutputBody::Content(content) => Some(content.as_slice()),
        FunctionOutputBody::Text(_) => None,
    };
    content
        .into_iter()
        .flatten()
        .rev()
        .filter_map(|content| match content {
            FunctionOutputContent::InputImage { image_url, .. } => Some(ImageReference::Inline {
                image_url: image_url.to_string(),
            }),
            FunctionOutputContent::InputImageFile { file_id, .. } => Some(ImageReference::File {
                file_id: file_id.to_string(),
            }),
            _ => None,
        })
}
fn content_image(item: &ContentItem) -> Option<ImageReference> {
    match item {
        ContentItem::InputImage { image_url, .. } => Some(ImageReference::Inline {
            image_url: image_url.to_string(),
        }),
        ContentItem::InputImageFile { file_id, .. } => Some(ImageReference::File {
            file_id: file_id.to_string(),
        }),
        _ => None,
    }
}

async fn save_result(
    save_root: &Path,
    session_id: &str,
    call_id: &str,
    result: &str,
    format: &str,
) -> Result<PathBuf, String> {
    let bytes = BASE64_STANDARD
        .decode(result.trim().as_bytes())
        .map_err(|error| format!("generated image was not valid base64: {error}"))?;
    let path = artifact_path(save_root, session_id, call_id, format);
    let parent = path
        .parent()
        .ok_or_else(|| format!("generated image path `{}` has no parent", path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("unable to create `{}`: {error}", parent.display()))?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|error| format!("unable to write `{}`: {error}", path.display()))?;
    Ok(path)
}

fn artifact_path(save_root: &Path, session_id: &str, call_id: &str, format: &str) -> PathBuf {
    save_root
        .join("generated_images")
        .join(sanitize_path_component(session_id))
        .join(format!("{}.{format}", sanitize_path_component(call_id)))
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "generated_image".to_owned()
    } else {
        sanitized
    }
}

fn image_output_hint(path: &Path) -> Option<String> {
    let output_dir = path.parent()?;
    let hint = format!(
        "Generated images are saved to {} as {} by default.\nIf you need to use a generated image at another path, copy it and leave the original in place unless the user explicitly asks you to delete it.\nThe generated image is already displayed to the user. There is no need to render it in the final response as a Markdown image or file link.",
        output_dir.display(),
        path.display()
    );
    (hint.len() <= MAX_OUTPUT_HINT_BYTES).then_some(hint)
}
