mod apply_patch;
mod code_mode;
mod image;
mod image_generation;
mod plan;
mod shell;
mod view_image;
mod web_search;

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::shell::ShellSessions;

pub(crate) use code_mode::{CodeModeExecution, NestedToolCall};
pub(crate) use image::prepare_output_images;

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum ToolOutputBody {
    Text(String),
    Content(Vec<ToolOutputContent>),
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToolOutputContent {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        detail: ImageDetail,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

pub(crate) struct ToolExecution {
    pub(crate) output: ToolOutputBody,
    pub(crate) success: bool,
    code_mode_value: Option<Value>,
    pub(crate) metadata: Option<Value>,
}

impl ToolExecution {
    pub(super) fn text(output: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(output.into()),
            success: true,
            code_mode_value: None,
            metadata: None,
        }
    }

    pub(super) fn error(error: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(error.into()),
            success: false,
            code_mode_value: None,
            metadata: None,
        }
    }

    pub(super) fn json(output: &impl Serialize) -> Self {
        match serde_json::to_string(output) {
            Ok(output) => Self::text(output),
            Err(error) => Self::error(format!("failed to encode tool result: {error}")),
        }
    }

    fn value(&self) -> Value {
        if let Some(value) = &self.code_mode_value {
            return value.clone();
        }
        match &self.output {
            ToolOutputBody::Text(text) => {
                serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
            }
            ToolOutputBody::Content(content) => {
                serde_json::to_value(content).unwrap_or(Value::Null)
            }
        }
    }

    fn with_code_mode_value(mut self, value: Value) -> Self {
        self.code_mode_value = Some(value);
        self
    }

    fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolExecution> + Send + 'a>>;

#[derive(Clone, Copy)]
pub(crate) struct ToolContext<'a> {
    pub(crate) model: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) call_id: &'a str,
    pub(crate) history: &'a [Value],
}

trait ToolHandler: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    fn spec(&self) -> Value;
    fn execute<'a>(&'a self, input: String, context: ToolContext<'a>) -> ToolFuture<'a>;
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolKind {
    Function,
    Freeform,
}

pub(crate) struct WebSearchConfig {
    pub(crate) endpoint: String,
    pub(crate) api_key: String,
}

pub(crate) struct ImageGenerationConfig {
    pub(crate) api_base_url: String,
    pub(crate) api_key: String,
    pub(crate) save_root: PathBuf,
}

pub(crate) struct ToolRuntime {
    handlers: Vec<Box<dyn ToolHandler>>,
    code_mode: code_mode::CodeModeRuntime,
    default_shell_name: &'static str,
}

impl ToolRuntime {
    pub(crate) fn new(
        workspace: impl Into<PathBuf>,
        web_search: WebSearchConfig,
        image_generation: ImageGenerationConfig,
    ) -> Self {
        let workspace = workspace.into();
        let sessions = Arc::new(ShellSessions::new());
        let default_shell_name = sessions.default_shell_name();
        let handlers: Vec<Box<dyn ToolHandler>> = vec![
            Box::new(shell::ExecCommandHandler::new(
                workspace.clone(),
                Arc::clone(&sessions),
            )),
            Box::new(shell::WriteStdinHandler::new(sessions)),
            Box::new(plan::PlanHandler::new()),
            Box::new(apply_patch::ApplyPatchHandler::new(workspace.clone())),
            Box::new(view_image::ViewImageHandler::new(workspace)),
            Box::new(web_search::WebSearchHandler::new(web_search)),
            Box::new(image_generation::ImageGenerationHandler::new(
                image_generation,
            )),
        ];
        Self {
            handlers,
            code_mode: code_mode::CodeModeRuntime::new(),
            default_shell_name,
        }
    }

    pub(crate) const fn default_shell_name(&self) -> &'static str {
        self.default_shell_name
    }

    pub(crate) fn model_specs(&self) -> Vec<Value> {
        vec![code_mode::exec_spec(&self.handlers), code_mode::wait_spec()]
    }

    pub(crate) async fn execute_code(
        &self,
        source: &str,
        context: ToolContext<'_>,
    ) -> CodeModeExecution {
        self.code_mode.execute(source, self, context).await
    }

    pub(crate) async fn wait_for_code(
        &self,
        input: &str,
        context: ToolContext<'_>,
    ) -> CodeModeExecution {
        self.code_mode.wait(input, self, context).await
    }

    async fn execute_nested(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolExecution {
        let Some(handler) = self.handlers.iter().find(|handler| handler.name() == name) else {
            return ToolExecution::error(format!("unsupported nested tool call: {name}"));
        };
        let input = match handler.kind() {
            ToolKind::Function if !input.is_object() => {
                return ToolExecution::error(format!(
                    "nested function tool {name} requires an object argument"
                ));
            }
            ToolKind::Function => match serde_json::to_string(&input) {
                Ok(input) => input,
                Err(error) => {
                    return ToolExecution::error(format!("failed to encode {name} input: {error}"));
                }
            },
            ToolKind::Freeform => match input.as_str() {
                Some(input) => input.to_owned(),
                None => {
                    return ToolExecution::error(format!(
                        "nested freeform tool {name} requires a string argument"
                    ));
                }
            },
        };
        handler.execute(input, context).await
    }

    fn nested_tool_metadata(&self) -> Vec<Value> {
        self.handlers
            .iter()
            .map(|handler| {
                let spec = handler.spec();
                json!({
                    "name": handler.name(),
                    "description": spec.get("description").and_then(Value::as_str).unwrap_or_default(),
                    "kind": handler.kind(),
                })
            })
            .collect()
    }
}
