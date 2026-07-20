use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use nanocodex_core::{
    ContentItem, FunctionOutputBody, FunctionOutputContent, ResponseItem, ToolDefinition,
};
use reqwest::header::USER_AGENT;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ImageDetail, ImageGenerationConfig, Tool, ToolContext, ToolExecution, ToolInput,
    ToolOutputBody, ToolOutputContent, image::load_for_prompt_data_url,
};

const DESCRIPTION: &str = include_str!("imagegen_description.md");
const IMAGE_MODEL: &str = "gpt-image-2";
const MAX_EDIT_IMAGES: usize = 5;
const ERROR_BODY_LIMIT: usize = 4_096;
const MAX_OUTPUT_HINT_BYTES: usize = 1_024;

pub(super) struct ImageGenerationHandler {
    client: reqwest::Client,
    generation_endpoint: String,
    edit_endpoint: String,
    api_key: String,
    save_root: PathBuf,
}

impl ImageGenerationHandler {
    pub(super) fn new(config: ImageGenerationConfig) -> Self {
        let api_base_url = config.api_base_url.trim_end_matches('/');
        Self {
            client: reqwest::Client::new(),
            generation_endpoint: format!("{api_base_url}/images/generations"),
            edit_endpoint: format!("{api_base_url}/images/edits"),
            api_key: config.api_key,
            save_root: config.save_root,
        }
    }

    async fn run(&self, input: &str, context: ToolContext<'_>) -> ToolExecution {
        let args = match serde_json::from_str::<ImagegenArgs>(input) {
            Ok(args) => args,
            Err(error) => {
                return ToolExecution::error(format!(
                    "failed to parse image_gen.imagegen arguments: {error}"
                ));
            }
        };
        let request = match request_for_args(&args, context.history).await {
            Ok(request) => request,
            Err(error) => return ToolExecution::error(error),
        };
        let response = match request {
            ImageRequest::Generate(request) => {
                self.post_image_request(&self.generation_endpoint, &request, "image generation")
                    .await
            }
            ImageRequest::Edit(request) => {
                self.post_image_request(&self.edit_endpoint, &request, "image edit")
                    .await
            }
        };
        let result = match response {
            Ok(response) => match response.data.into_iter().next() {
                Some(data) => data.b64_json,
                None => {
                    return ToolExecution::error("image generation returned no image data");
                }
            },
            Err(error) => return ToolExecution::error(format!("image generation failed: {error}")),
        };
        let saved_path = match save_result(
            &self.save_root,
            context.session_id,
            context.call_id,
            &result,
        )
        .await
        {
            Ok(path) => Some(path),
            Err(error) => {
                eprintln!("failed to save generated image: {error}");
                None
            }
        };
        let output_hint = saved_path.as_ref().and_then(|path| image_output_hint(path));
        let image_url = format!("data:image/png;base64,{result}");
        let mut output_items = vec![ToolOutputContent::InputImage {
            image_url: image_url.clone(),
            detail: ImageDetail::High,
        }];
        let mut code_mode_value = json!({ "image_url": image_url });
        if let Some(output_hint) = output_hint {
            output_items.push(ToolOutputContent::InputText {
                text: output_hint.clone(),
            });
            code_mode_value["output_hint"] = Value::String(output_hint);
        }
        ToolExecution {
            output: ToolOutputBody::Content(output_items),
            success: true,
            code_mode_value: Some(code_mode_value),
            metadata: None,
            process_trace: None,
        }
    }

    async fn post_image_request<R: Serialize + ?Sized>(
        &self,
        endpoint: &str,
        request: &R,
        operation: &str,
    ) -> Result<ImageResponse, String> {
        let response = self
            .client
            .post(endpoint)
            .header(USER_AGENT, concat!("nanocodex/", env!("CARGO_PKG_VERSION")))
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|error| format!("{operation} request failed: {error}"))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| format!("failed to read {operation} response: {error}"))?;
        if !status.is_success() {
            return Err(format!(
                "{operation} returned HTTP {status}: {}",
                body_preview(&body)
            ));
        }
        serde_json::from_slice(&body)
            .map_err(|error| format!("failed to decode {operation} response: {error}"))
    }
}

#[async_trait::async_trait]
impl Tool for ImageGenerationHandler {
    fn name(&self) -> &'static str {
        "image_gen__imagegen"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "referenced_image_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "maxItems": MAX_EDIT_IMAGES
                    },
                    "num_last_images_to_include": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_EDIT_IMAGES
                    }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolExecution {
        let input = match input.function_json() {
            Ok(input) => input,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        self.run(input.get(), context).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImagegenArgs {
    prompt: String,
    #[serde(default)]
    referenced_image_paths: Option<Vec<PathBuf>>,
    #[serde(default)]
    num_last_images_to_include: Option<usize>,
}

#[derive(Debug, PartialEq, Serialize)]
struct ImageGenerationRequest {
    prompt: String,
    background: &'static str,
    model: &'static str,
    quality: &'static str,
    size: &'static str,
}

#[derive(Debug, PartialEq, Serialize)]
struct ImageEditRequest {
    images: Vec<ImageUrl>,
    prompt: String,
    background: &'static str,
    model: &'static str,
    quality: &'static str,
    size: &'static str,
}

#[derive(Debug, PartialEq, Serialize)]
struct ImageUrl {
    image_url: String,
}

#[derive(Deserialize)]
struct ImageResponse {
    #[serde(rename = "created")]
    _created: u64,
    data: Vec<ImageData>,
}

#[derive(Deserialize)]
struct ImageData {
    b64_json: String,
}

#[derive(Debug, PartialEq)]
enum ImageRequest {
    Generate(ImageGenerationRequest),
    Edit(ImageEditRequest),
}

async fn request_for_args(
    args: &ImagegenArgs,
    history: &[ResponseItem],
) -> Result<ImageRequest, String> {
    let paths = args.referenced_image_paths.as_deref().unwrap_or_default();
    if paths.len() > MAX_EDIT_IMAGES {
        return Err(format!(
            "`referenced_image_paths` must contain at most {MAX_EDIT_IMAGES} paths"
        ));
    }
    let images = match (paths.is_empty(), args.num_last_images_to_include) {
        (true, None) => {
            return Ok(ImageRequest::Generate(ImageGenerationRequest {
                prompt: args.prompt.clone(),
                background: "auto",
                model: IMAGE_MODEL,
                quality: "auto",
                size: "auto",
            }));
        }
        (false, None) => {
            let mut images = Vec::with_capacity(paths.len());
            for path in paths {
                if !path.is_absolute() {
                    return Err(format!(
                        "referenced image path `{}` must be absolute",
                        path.display()
                    ));
                }
                images.push(ImageUrl {
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

    Ok(ImageRequest::Edit(ImageEditRequest {
        images,
        prompt: args.prompt.clone(),
        background: "auto",
        model: IMAGE_MODEL,
        quality: "auto",
        size: "auto",
    }))
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

fn recent_images(history: &[ResponseItem], count: usize) -> Vec<ImageUrl> {
    let mut function_call_ids = HashSet::new();
    let mut custom_tool_call_ids = HashSet::new();
    for item in history {
        match item {
            ResponseItem::FunctionCall { call_id, .. } => {
                function_call_ids.insert(call_id.as_ref());
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                custom_tool_call_ids.insert(call_id.as_ref());
            }
            _ => {}
        }
    }

    let mut images = Vec::with_capacity(count);
    'history: for item in history.iter().rev() {
        let mut image_urls = Vec::new();
        match item {
            ResponseItem::Message { content, .. } => {
                image_urls.extend(content.iter().rev().filter_map(content_image_url));
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } if function_call_ids.contains(call_id.as_ref()) => {
                image_urls.extend(output_image_urls(output));
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } if custom_tool_call_ids.contains(call_id.as_ref()) => {
                image_urls.extend(output_image_urls(output));
            }
            ResponseItem::ImageGenerationCall { result, .. } if !result.is_empty() => {
                image_urls.push(format!("data:image/png;base64,{result}"));
            }
            _ => {}
        }
        for image_url in image_urls {
            images.push(ImageUrl { image_url });
            if images.len() == count {
                break 'history;
            }
        }
    }
    images.reverse();
    images
}

fn output_image_urls(output: &FunctionOutputBody) -> impl Iterator<Item = String> + '_ {
    let content = match output {
        FunctionOutputBody::Content(content) => Some(content.as_slice()),
        FunctionOutputBody::Text(_) => None,
    };
    content.into_iter().flatten().rev().filter_map(|content| {
        let FunctionOutputContent::InputImage { image_url, .. } = content else {
            return None;
        };
        Some(image_url.to_string())
    })
}

fn content_image_url(item: &ContentItem) -> Option<String> {
    let ContentItem::InputImage { image_url, .. } = item else {
        return None;
    };
    Some(image_url.to_string())
}

async fn save_result(
    save_root: &Path,
    session_id: &str,
    call_id: &str,
    result: &str,
) -> Result<PathBuf, String> {
    let bytes = BASE64_STANDARD
        .decode(result.trim().as_bytes())
        .map_err(|error| format!("generated image was not valid base64: {error}"))?;
    let path = artifact_path(save_root, session_id, call_id);
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

fn artifact_path(save_root: &Path, session_id: &str, call_id: &str) -> PathBuf {
    save_root
        .join("generated_images")
        .join(sanitize_path_component(session_id))
        .join(format!("{}.png", sanitize_path_component(call_id)))
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
        "Generated images are saved to {} as {} by default.\nIf you need to use a generated image at another path, copy it and leave the original in place unless the user explicitly asks you to delete it.",
        output_dir.display(),
        path.display()
    );
    (hint.len() <= MAX_OUTPUT_HINT_BYTES).then_some(hint)
}

fn body_preview(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let mut end = text.len().min(ERROR_BODY_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let suffix = if end < text.len() { "…" } else { "" };
    format!("{}{suffix}", &text[..end])
}

#[cfg(test)]
mod tests;
