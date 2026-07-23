use std::{path::PathBuf, sync::Arc};

use js_sys::Promise;
use nanocodex_core::{
    ContentItem, CustomToolFormat, ImageDetail, PromptInput, ResponseItem, ToolDefinition,
    UserInput,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

pub const DEFAULT_TOOL_OUTPUT_TOKENS: usize = 10_000;

const EXEC_GRAMMAR: &str = r"start: /[\s\S]+/";
const EXEC_DESCRIPTION: &str = r"Run JavaScript in the embedded host.
- `tools` contains the application-defined async tools listed below.
- `text(value)` and `image(value)` append output for the model.
- `generatedImage(result)` appends an image-generation result for the model.
- `store(key, value)` and `load(key)` retain serializable values across calls.
- JavaScript runs inside the Node or browser host supplied by the embedding application.";

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = executeCode)]
    fn host_execute_code(source: &str, session_id: &str, call_id: &str)
    -> Result<Promise, JsValue>;

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = toolDefinitions)]
    fn host_tool_definitions(session_id: &str) -> String;
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum ToolOutputBody {
    Text(String),
    Content(Vec<ToolOutputContent>),
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutputContent {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        detail: ImageDetail,
    },
}

#[derive(Clone, Copy)]
pub struct ToolContext<'a> {
    pub model: &'a str,
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub history: &'a [ResponseItem],
    pub output_token_budget: usize,
}

#[doc(hidden)]
pub struct OwnedToolContext {
    model: String,
    session_id: String,
    call_id: String,
    history: Arc<Vec<ResponseItem>>,
    output_token_budget: usize,
}

impl OwnedToolContext {
    #[must_use]
    pub fn new(
        model: impl Into<String>,
        session_id: impl Into<String>,
        call_id: impl Into<String>,
        history: Arc<Vec<ResponseItem>>,
        output_token_budget: usize,
    ) -> Self {
        Self {
            model: model.into(),
            session_id: session_id.into(),
            call_id: call_id.into(),
            history,
            output_token_budget,
        }
    }

    fn borrowed(&self) -> ToolContext<'_> {
        ToolContext {
            model: &self.model,
            session_id: &self.session_id,
            call_id: &self.call_id,
            history: self.history.as_slice(),
            output_token_budget: self.output_token_budget,
        }
    }
}

pub struct CodeModeExecution {
    pub output: ToolOutputBody,
    pub success: bool,
    pub nested_calls: Vec<NestedToolCall>,
    pub notifications: Vec<CodeModeNotification>,
}

pub struct CodeModeNotification {
    pub call_id: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct NestedToolCall {
    pub call_id: String,
    pub name: String,
    pub input: Value,
    pub output: ToolOutputBody,
    pub success: bool,
    pub duration_ns: u64,
    #[serde(default)]
    pub metadata: Option<Box<RawValue>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostCodeExecution {
    output: ToolOutputBody,
    success: bool,
    #[serde(default)]
    nested_calls: Vec<NestedToolCall>,
    #[serde(default)]
    notifications: Vec<HostNotification>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostNotification {
    call_id: String,
    text: String,
}

pub struct WebSearchConfig {
    pub endpoint: String,
    pub auth: nanocodex_core::OpenAiAuth,
}

pub struct ImageGenerationConfig {
    pub api_base_url: String,
    pub auth: nanocodex_core::OpenAiAuth,
    pub save_root: PathBuf,
}

#[derive(Clone, Default)]
pub struct Tools;

impl Tools {
    #[must_use]
    pub const fn web_search_enabled(&self) -> bool {
        false
    }

    #[must_use]
    pub const fn image_generation_enabled(&self) -> bool {
        false
    }
}

pub struct ToolRuntime {
    working_directory: Arc<str>,
}

#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct ToolRuntimeControl;

impl ToolRuntime {
    pub fn new(
        workspace: impl Into<PathBuf>,
        _web_search: Option<WebSearchConfig>,
        _image_generation: Option<ImageGenerationConfig>,
    ) -> Self {
        let workspace = workspace.into();
        Self {
            working_directory: Arc::from(workspace.to_string_lossy().into_owned()),
        }
    }

    /// Builds the browser runtime from the complete declarative tool selection.
    #[must_use]
    pub fn new_with_tools(
        workspace: impl Into<PathBuf>,
        web_search: Option<WebSearchConfig>,
        image_generation: Option<ImageGenerationConfig>,
        tools: &Tools,
    ) -> Self {
        Self::new(workspace, web_search, image_generation).with_tools(tools)
    }

    #[must_use]
    pub const fn with_tools(self, _tools: &Tools) -> Self {
        self
    }

    #[must_use]
    pub const fn default_shell_name(&self) -> &'static str {
        "javascript"
    }

    #[must_use]
    pub fn working_directory(&self) -> &str {
        &self.working_directory
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn control(&self) -> ToolRuntimeControl {
        ToolRuntimeControl
    }

    #[must_use]
    pub fn model_specs(&self, session_id: &str) -> Vec<ToolDefinition> {
        let definitions =
            serde_json::from_str::<Vec<ToolDefinition>>(&host_tool_definitions(session_id))
                .unwrap_or_default();
        let mut description = EXEC_DESCRIPTION.to_owned();
        for definition in definitions {
            description.push_str("\n\n- `tools.");
            description.push_str(definition.name());
            description.push_str("`: ");
            description.push_str(definition.description().trim());
        }
        vec![ToolDefinition::custom(
            "exec",
            description,
            CustomToolFormat::grammar("lark", EXEC_GRAMMAR),
        )]
    }

    pub async fn execute_code(&self, source: &str, context: ToolContext<'_>) -> CodeModeExecution {
        let promise = match host_execute_code(source, context.session_id, context.call_id) {
            Ok(promise) => promise,
            Err(error) => return failed(&js_error(&error)),
        };
        let value = match JsFuture::from(promise).await {
            Ok(value) => value,
            Err(error) => return failed(&js_error(&error)),
        };
        let Some(encoded) = value.as_string() else {
            return failed("JavaScript code-mode host returned a non-string result");
        };
        match serde_json::from_str::<HostCodeExecution>(&encoded) {
            Ok(execution) => CodeModeExecution {
                output: execution.output,
                success: execution.success,
                nested_calls: execution.nested_calls,
                notifications: execution
                    .notifications
                    .into_iter()
                    .map(|notification| CodeModeNotification {
                        call_id: notification.call_id,
                        text: notification.text,
                    })
                    .collect(),
            },
            Err(error) => failed(&format!(
                "JavaScript code-mode host returned invalid JSON: {error}"
            )),
        }
    }

    #[doc(hidden)]
    pub async fn execute_code_owned(
        &self,
        source: &str,
        context: OwnedToolContext,
    ) -> CodeModeExecution {
        self.execute_code(source, context.borrowed()).await
    }

    #[expect(
        clippy::unused_async,
        reason = "matches the native tool-runtime contract"
    )]
    pub async fn wait_for_code(
        &self,
        _input: &str,
        _context: ToolContext<'_>,
    ) -> CodeModeExecution {
        failed("background code-mode cells are unavailable in the WASM runtime")
    }
}

impl ToolRuntimeControl {
    #[doc(hidden)]
    #[expect(
        clippy::unused_async,
        reason = "matches the native tool-runtime control contract"
    )]
    pub async fn cancel(&self) {}
}

#[expect(
    clippy::unused_async,
    reason = "matches the native input-preparation contract"
)]
pub async fn prepare_user_input(input: &PromptInput) -> Vec<ContentItem> {
    let items = match input {
        PromptInput::Text(text) => vec![UserInput::Text { text: text.clone() }],
        PromptInput::Content(items) => items.clone(),
    };
    items
        .into_iter()
        .map(|item| match item {
            UserInput::Text { text } => ContentItem::InputText {
                text: text.into_boxed_str(),
            },
            UserInput::Image { image_url, detail } => ContentItem::InputImage {
                image_url: image_url.into_boxed_str(),
                detail,
            },
            UserInput::Audio { audio_url } => ContentItem::InputAudio {
                audio_url: audio_url.into_boxed_str(),
            },
            UserInput::LocalImage { path, .. } => ContentItem::InputText {
                text: format!(
                    "Local image paths are unavailable in browser WASM: {}",
                    path.display()
                )
                .into_boxed_str(),
            },
            UserInput::LocalAudio { path } => ContentItem::InputText {
                text: format!(
                    "Local audio paths are unavailable in browser WASM: {}",
                    path.display()
                )
                .into_boxed_str(),
            },
        })
        .collect()
}

#[expect(
    clippy::unused_async,
    reason = "matches the native output-preparation contract"
)]
pub async fn prepare_output_images(_output: &mut ToolOutputBody) {}

fn failed(message: &str) -> CodeModeExecution {
    CodeModeExecution {
        output: ToolOutputBody::Text(format!("Script failed\nOutput:\n{message}")),
        success: false,
        nested_calls: Vec::new(),
        notifications: Vec::new(),
    }
}

fn js_error(error: &JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}
