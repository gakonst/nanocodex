use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use harness_core::{ImageDetail, ResponseItem, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use serde_json::{Value, json};

use crate::{
    apply_patch,
    code_mode::{self, CodeModeExecution},
    image_generation, plan,
    shell::{self, ShellSessions},
    view_image, web_search,
};

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

pub struct ToolExecution {
    pub output: ToolOutputBody,
    pub success: bool,
    pub(crate) code_mode_value: Option<Value>,
    pub metadata: Option<Box<RawValue>>,
}

impl ToolExecution {
    pub(crate) fn text(output: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(output.into()),
            success: true,
            code_mode_value: None,
            metadata: None,
        }
    }

    pub(crate) fn error(error: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(error.into()),
            success: false,
            code_mode_value: None,
            metadata: None,
        }
    }

    pub(crate) fn json(output: &impl Serialize) -> Self {
        match serde_json::to_string(output) {
            Ok(output) => Self::text(output),
            Err(error) => Self::error(format!("failed to encode tool result: {error}")),
        }
    }

    pub(crate) fn value(&self) -> Value {
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

    pub(crate) fn with_code_mode_value(mut self, value: Value) -> Self {
        self.code_mode_value = Some(value);
        self
    }

    pub(crate) fn with_metadata(mut self, metadata: impl Serialize) -> Self {
        match to_raw_value(&metadata) {
            Ok(metadata) => self.metadata = Some(metadata),
            Err(error) => {
                self.output =
                    ToolOutputBody::Text(format!("failed to encode tool result metadata: {error}"));
                self.success = false;
            }
        }
        self
    }
}

pub(crate) type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolExecution> + Send + 'a>>;

#[derive(Clone, Copy)]
pub struct ToolContext<'a> {
    pub model: &'a str,
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub history: &'a [ResponseItem],
}

pub(crate) trait ToolHandler: Send + Sync {
    fn name(&self) -> &'static str;
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    fn spec(&self) -> ToolDefinition;
    fn execute<'a>(&'a self, input: String, context: ToolContext<'a>) -> ToolFuture<'a>;
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolKind {
    Function,
    Freeform,
}

pub struct WebSearchConfig {
    pub endpoint: String,
    pub api_key: String,
}

pub struct ImageGenerationConfig {
    pub api_base_url: String,
    pub api_key: String,
    pub save_root: PathBuf,
}

pub struct ToolRuntime {
    handlers: Vec<Box<dyn ToolHandler>>,
    code_mode: code_mode::CodeModeRuntime,
    default_shell_name: &'static str,
}

impl ToolRuntime {
    pub fn new(
        workspace: impl Into<PathBuf>,
        web_search: Option<WebSearchConfig>,
        image_generation: ImageGenerationConfig,
    ) -> Self {
        let workspace = workspace.into();
        let sessions = Arc::new(ShellSessions::new());
        let default_shell_name = sessions.default_shell_name();
        let mut handlers: Vec<Box<dyn ToolHandler>> = vec![
            Box::new(shell::ExecCommandHandler::new(
                workspace.clone(),
                Arc::clone(&sessions),
            )),
            Box::new(shell::WriteStdinHandler::new(sessions)),
            Box::new(plan::PlanHandler::new()),
            Box::new(apply_patch::ApplyPatchHandler::new(workspace.clone())),
            Box::new(view_image::ViewImageHandler::new(workspace)),
        ];
        if let Some(web_search) = web_search {
            handlers.push(Box::new(web_search::WebSearchHandler::new(web_search)));
        }
        handlers.push(Box::new(image_generation::ImageGenerationHandler::new(
            image_generation,
        )));
        Self {
            handlers,
            code_mode: code_mode::CodeModeRuntime::new(),
            default_shell_name,
        }
    }

    pub const fn default_shell_name(&self) -> &'static str {
        self.default_shell_name
    }

    pub fn model_specs(&self) -> Vec<ToolDefinition> {
        vec![code_mode::exec_spec(&self.handlers), code_mode::wait_spec()]
    }

    pub async fn execute_code(&self, source: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode.execute(source, self, context).await
    }

    pub async fn wait_for_code(&self, input: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode.wait(input, self, context).await
    }

    pub(crate) async fn execute_nested(
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

    pub(crate) fn nested_tool_metadata(&self) -> Vec<Value> {
        self.handlers
            .iter()
            .map(|handler| {
                let spec = handler.spec();
                json!({
                    "name": handler.name(),
                    "description": spec.description(),
                    "kind": handler.kind(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{ImageGenerationConfig, ToolRuntime, WebSearchConfig};

    fn runtime(web_search: bool) -> ToolRuntime {
        ToolRuntime::new(
            ".",
            web_search.then(|| WebSearchConfig {
                endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
                api_key: "test-key".to_owned(),
            }),
            ImageGenerationConfig {
                api_base_url: "http://127.0.0.1:1/v1".to_owned(),
                api_key: "test-key".to_owned(),
                save_root: std::env::temp_dir().join("harness-test-images"),
            },
        )
    }

    #[test]
    fn web_search_handler_and_spec_are_absent_when_disabled() {
        let enabled = runtime(true);
        assert!(
            enabled
                .handlers
                .iter()
                .any(|handler| handler.name() == "web__run")
        );
        let enabled_specs = serde_json::to_value(enabled.model_specs()).unwrap();
        assert!(
            enabled_specs[0]["description"]
                .as_str()
                .is_some_and(|description| description.contains("`web__run`"))
        );

        let disabled = runtime(false);
        assert!(
            disabled
                .handlers
                .iter()
                .all(|handler| handler.name() != "web__run")
        );
        let disabled_specs = serde_json::to_value(disabled.model_specs()).unwrap();
        assert!(
            disabled_specs[0]["description"]
                .as_str()
                .is_some_and(|description| !description.contains("`web__run`"))
        );
    }
}
