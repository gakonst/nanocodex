use std::{fmt, future::Future, path::PathBuf, pin::Pin, sync::Arc};

use harness_core::{ImageDetail, ResponseItem, ToolDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
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
    #[must_use]
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(output.into()),
            success: true,
            code_mode_value: None,
            metadata: None,
        }
    }

    #[must_use]
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(error.into()),
            success: false,
            code_mode_value: None,
            metadata: None,
        }
    }

    #[must_use]
    pub fn json(output: &impl Serialize) -> Self {
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

    #[must_use]
    pub fn with_metadata(mut self, metadata: impl Serialize) -> Self {
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

pub(crate) type ErasedToolFuture<'a> = Pin<Box<dyn Future<Output = ToolExecution> + Send + 'a>>;

#[derive(Clone, Copy)]
pub struct ToolContext<'a> {
    pub model: &'a str,
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub history: &'a [ResponseItem],
}

/// A typed function tool that can be installed in an agent's tool runtime.
pub trait Tool: Send + Sync {
    /// Arguments decoded from the model's JSON function call.
    type Input: DeserializeOwned + Send + 'static;

    /// Returns the model-visible function definition.
    fn definition(&self) -> ToolDefinition;

    /// Executes one decoded tool call.
    fn execute<'a>(
        &'a self,
        input: Self::Input,
        context: ToolContext<'a>,
    ) -> impl Future<Output = ToolExecution> + Send + 'a;
}

pub(crate) trait ErasedTool: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }
    fn spec(&self) -> ToolDefinition;
    fn execute<'a>(&'a self, input: String, context: ToolContext<'a>) -> ErasedToolFuture<'a>;
}

struct RegisteredTool<T> {
    tool: T,
    definition: ToolDefinition,
}

impl<T: Tool> RegisteredTool<T> {
    fn new(tool: T) -> Self {
        let definition = tool.definition();
        Self { tool, definition }
    }
}

impl<T: Tool> ErasedTool for RegisteredTool<T> {
    fn name(&self) -> &str {
        self.definition.name()
    }

    fn spec(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn execute<'a>(&'a self, input: String, context: ToolContext<'a>) -> ErasedToolFuture<'a> {
        Box::pin(async move {
            let input = match serde_json::from_str(&input) {
                Ok(input) => input,
                Err(error) => {
                    return ToolExecution::error(format!(
                        "failed to decode {} arguments: {error}",
                        self.name()
                    ));
                }
            };
            self.tool.execute(input, context).await
        })
    }
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

/// Declarative selection of the built-in tools installed for an agent.
#[derive(Clone)]
pub struct Tools {
    web_search: bool,
    image_generation: bool,
    registered: Vec<Arc<dyn ErasedTool>>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            web_search: true,
            image_generation: true,
            registered: Vec::new(),
        }
    }
}

impl fmt::Debug for Tools {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tools")
            .field("web_search", &self.web_search)
            .field("image_generation", &self.image_generation)
            .field(
                "registered",
                &self
                    .registered
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Tools {
    #[must_use]
    pub fn builder() -> ToolsBuilder {
        ToolsBuilder::default()
    }

    /// Returns whether the standard web-search tool is enabled.
    #[must_use]
    pub const fn web_search_enabled(&self) -> bool {
        self.web_search
    }

    /// Returns whether the standard image-generation tool is enabled.
    #[must_use]
    pub const fn image_generation_enabled(&self) -> bool {
        self.image_generation
    }
}

/// Builder for the built-in tool selection.
#[derive(Default)]
pub struct ToolsBuilder {
    tools: Tools,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolsBuildError {
    #[error("tool name must not be empty")]
    EmptyName,

    #[error("tool name `{0}` is registered more than once")]
    DuplicateName(Box<str>),

    #[error("tool name `{0}` conflicts with an enabled built-in tool")]
    BuiltInName(Box<str>),

    #[error("registered tool `{0}` must use a function definition")]
    NonFunctionDefinition(Box<str>),
}

impl ToolsBuilder {
    /// Starts from an empty built-in tool set.
    #[must_use]
    pub fn without_defaults(mut self) -> Self {
        self.tools.web_search = false;
        self.tools.image_generation = false;
        self
    }

    #[must_use]
    pub fn web_search(mut self, enabled: bool) -> Self {
        self.tools.web_search = enabled;
        self
    }

    #[must_use]
    pub fn image_generation(mut self, enabled: bool) -> Self {
        self.tools.image_generation = enabled;
        self
    }

    /// Adds a typed function tool to the runtime.
    #[must_use]
    pub fn tool<T: Tool + 'static>(mut self, tool: T) -> Self {
        self.tools
            .registered
            .push(Arc::new(RegisteredTool::new(tool)));
        self
    }

    /// Validates tool names and finishes the runtime configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, duplicate, non-function, or enabled
    /// built-in tool definitions.
    pub fn build(self) -> Result<Tools, ToolsBuildError> {
        let mut names = Vec::with_capacity(self.tools.registered.len());
        for tool in &self.tools.registered {
            let name = tool.name();
            if name.is_empty() {
                return Err(ToolsBuildError::EmptyName);
            }
            if !matches!(tool.spec(), ToolDefinition::Function { .. }) {
                return Err(ToolsBuildError::NonFunctionDefinition(name.into()));
            }
            if built_in_name(&self.tools, name) {
                return Err(ToolsBuildError::BuiltInName(name.into()));
            }
            if names.contains(&name) {
                return Err(ToolsBuildError::DuplicateName(name.into()));
            }
            names.push(name);
        }
        Ok(self.tools)
    }
}

fn built_in_name(tools: &Tools, name: &str) -> bool {
    matches!(
        name,
        "exec_command" | "write_stdin" | "update_plan" | "apply_patch" | "view_image"
    ) || (tools.web_search && name == "web__run")
        || (tools.image_generation && name == "image_gen__imagegen")
}

pub struct ToolRuntime {
    handlers: Vec<Arc<dyn ErasedTool>>,
    code_mode: code_mode::CodeModeRuntime,
    default_shell_name: &'static str,
}

impl ToolRuntime {
    pub fn new(
        workspace: impl Into<PathBuf>,
        web_search: Option<WebSearchConfig>,
        image_generation: Option<ImageGenerationConfig>,
    ) -> Self {
        let workspace = workspace.into();
        let sessions = Arc::new(ShellSessions::new());
        let default_shell_name = sessions.default_shell_name();
        let mut handlers: Vec<Arc<dyn ErasedTool>> = vec![
            Arc::new(shell::ExecCommandHandler::new(
                workspace.clone(),
                Arc::clone(&sessions),
            )),
            Arc::new(shell::WriteStdinHandler::new(sessions)),
            Arc::new(plan::PlanHandler::new()),
            Arc::new(apply_patch::ApplyPatchHandler::new(workspace.clone())),
            Arc::new(view_image::ViewImageHandler::new(workspace)),
        ];
        if let Some(web_search) = web_search {
            handlers.push(Arc::new(web_search::WebSearchHandler::new(web_search)));
        }
        if let Some(image_generation) = image_generation {
            handlers.push(Arc::new(image_generation::ImageGenerationHandler::new(
                image_generation,
            )));
        }
        Self {
            handlers,
            code_mode: code_mode::CodeModeRuntime::new(),
            default_shell_name,
        }
    }

    #[must_use]
    pub fn with_tools(mut self, tools: &Tools) -> Self {
        self.handlers.extend(tools.registered.iter().cloned());
        self
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
    use harness_core::ToolDefinition;
    use serde::Deserialize;
    use serde_json::json;

    use super::{
        ImageGenerationConfig, Tool, ToolContext, ToolExecution, ToolOutputBody, ToolRuntime,
        Tools, WebSearchConfig,
    };

    struct Double;

    #[derive(Deserialize)]
    struct DoubleInput {
        value: i64,
    }

    impl Tool for Double {
        type Input = DoubleInput;

        fn definition(&self) -> ToolDefinition {
            ToolDefinition::function(
                "double",
                "Doubles an integer.",
                json!({
                    "type": "object",
                    "properties": { "value": { "type": "integer" } },
                    "required": ["value"],
                    "additionalProperties": false
                }),
            )
        }

        async fn execute(&self, input: DoubleInput, _context: ToolContext<'_>) -> ToolExecution {
            ToolExecution::text((input.value * 2).to_string())
        }
    }

    fn runtime(web_search: bool) -> ToolRuntime {
        ToolRuntime::new(
            ".",
            web_search.then(|| WebSearchConfig {
                endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
                api_key: "test-key".to_owned(),
            }),
            Some(ImageGenerationConfig {
                api_base_url: "http://127.0.0.1:1/v1".to_owned(),
                api_key: "test-key".to_owned(),
                save_root: std::env::temp_dir().join("harness-test-images"),
            }),
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

    #[tokio::test]
    async fn registered_tool_is_described_and_receives_typed_input() {
        let tools = Tools::builder()
            .without_defaults()
            .tool(Double)
            .build()
            .unwrap();
        let runtime = ToolRuntime::new(".", None, None).with_tools(&tools);
        let description = serde_json::to_value(runtime.model_specs()).unwrap();
        assert!(
            description[0]["description"]
                .as_str()
                .is_some_and(|description| description.contains(
                    "declare const tools: { double(args: { value: number; }): Promise<unknown>; };"
                ))
        );

        let handler = runtime
            .handlers
            .iter()
            .find(|handler| handler.name() == "double")
            .unwrap();
        let execution = handler
            .execute(
                r#"{"value":21}"#.to_owned(),
                ToolContext {
                    model: "test-model",
                    session_id: "test-session",
                    call_id: "test-call",
                    history: &[],
                },
            )
            .await;
        assert!(execution.success);
        let ToolOutputBody::Text(output) = execution.output else {
            panic!("expected text output");
        };
        assert_eq!(output, "42");
    }
}
