mod apply_patch;
mod code_mode;
mod plan;
mod shell;
mod view_image;

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::shell::ShellSessions;

pub(crate) use code_mode::{CodeModeExecution, NestedToolCall};

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum ToolOutputBody {
    Text(String),
    Content(Vec<ToolOutputContent>),
}

#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Clone, Copy, Deserialize, Serialize)]
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
}

impl ToolExecution {
    pub(super) fn text(output: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(output.into()),
            success: true,
            code_mode_value: None,
        }
    }

    pub(super) fn error(error: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(error.into()),
            success: false,
            code_mode_value: None,
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
}

type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolExecution> + Send + 'a>>;

trait ToolHandler: Send + Sync {
    fn name(&self) -> &'static str;
    fn spec(&self) -> Value;
    fn execute(&self, input: String) -> ToolFuture<'_>;
}

pub(crate) struct ToolRuntime {
    handlers: Vec<Box<dyn ToolHandler>>,
    code_mode: code_mode::CodeModeRuntime,
}

impl ToolRuntime {
    pub(crate) fn new(workspace: impl Into<PathBuf>) -> Arc<Self> {
        let workspace = workspace.into();
        let sessions = Arc::new(ShellSessions::new());
        let handlers: Vec<Box<dyn ToolHandler>> = vec![
            Box::new(shell::ExecCommandHandler::new(
                workspace.clone(),
                Arc::clone(&sessions),
            )),
            Box::new(shell::WriteStdinHandler::new(sessions)),
            Box::new(plan::PlanHandler::new()),
            Box::new(apply_patch::ApplyPatchHandler::new(workspace.clone())),
            Box::new(view_image::ViewImageHandler::new(workspace)),
        ];
        Arc::new(Self {
            handlers,
            code_mode: code_mode::CodeModeRuntime::new(),
        })
    }

    pub(crate) fn model_specs(&self) -> Vec<Value> {
        vec![code_mode::exec_spec(&self.handlers), code_mode::wait_spec()]
    }

    pub(crate) async fn execute_code(&self, source: &str) -> CodeModeExecution {
        self.code_mode.execute(source, self).await
    }

    pub(crate) async fn wait_for_code(&self, input: &str) -> CodeModeExecution {
        self.code_mode.wait(input, self).await
    }

    async fn execute_nested(&self, name: &str, input: Value) -> ToolExecution {
        let Some(handler) = self.handlers.iter().find(|handler| handler.name() == name) else {
            return ToolExecution::error(format!("unsupported nested tool call: {name}"));
        };
        if !input.is_object() {
            return ToolExecution::error(format!(
                "nested function tool {name} requires an object argument"
            ));
        }
        match serde_json::to_string(&input) {
            Ok(input) => handler.execute(input).await,
            Err(error) => ToolExecution::error(format!("failed to encode {name} input: {error}")),
        }
    }

    fn nested_tool_metadata(&self) -> Vec<Value> {
        self.handlers
            .iter()
            .map(|handler| {
                let spec = handler.spec();
                json!({
                    "name": handler.name(),
                    "description": spec.get("description").and_then(Value::as_str).unwrap_or_default(),
                })
            })
            .collect()
    }
}
