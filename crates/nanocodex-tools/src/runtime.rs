use std::{collections::HashMap, fmt, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use nanocodex_core::{ImageDetail, ResponseItem, ToolDefinition};
use schemars::{JsonSchema, r#gen::SchemaSettings};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::value::{RawValue, to_raw_value};
use serde_json::{Map, Value, json};

use crate::{
    apply_patch,
    code_mode::{self, CodeModeExecution},
    image_generation, plan,
    shell::{self, ShellSessions},
    view_image, web_search,
};

pub const DEFAULT_TOOL_OUTPUT_TOKENS: usize = 10_000;

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

    /// Returns a successful multimodal tool result.
    #[must_use]
    pub fn content(output: Vec<ToolOutputContent>) -> Self {
        Self {
            output: ToolOutputBody::Content(output),
            success: true,
            code_mode_value: None,
            metadata: None,
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

#[derive(Clone, Copy)]
pub struct ToolContext<'a> {
    pub model: &'a str,
    pub session_id: &'a str,
    pub call_id: &'a str,
    pub history: &'a [ResponseItem],
    pub output_token_budget: usize,
}

/// Canonical input presented to function and freeform tools.
pub enum ToolInput {
    Function(Box<RawValue>),
    Freeform(String),
}

impl ToolInput {
    /// Borrows raw JSON function arguments without materializing a value tree.
    ///
    /// # Errors
    ///
    /// Returns an error for freeform input.
    pub fn function_json(&self) -> Result<&RawValue, ToolInputError> {
        match self {
            Self::Function(input) => Ok(input),
            Self::Freeform(_) => Err(ToolInputError::ExpectedFunction),
        }
    }

    /// Decodes JSON function arguments into a caller-selected type.
    ///
    /// # Errors
    ///
    /// Returns an error for freeform input or invalid JSON arguments.
    pub fn decode_json<T: DeserializeOwned>(&self) -> Result<T, ToolInputError> {
        serde_json::from_str(self.function_json()?.get()).map_err(ToolInputError::Decode)
    }

    /// Extracts freeform source text.
    ///
    /// # Errors
    ///
    /// Returns an error for JSON function arguments.
    pub fn into_freeform(self) -> Result<String, ToolInputError> {
        match self {
            Self::Freeform(input) => Ok(input),
            Self::Function(_) => Err(ToolInputError::ExpectedFreeform),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolInputError {
    #[error("expected JSON function arguments")]
    ExpectedFunction,

    #[error("expected freeform tool input")]
    ExpectedFreeform,

    #[error("failed to decode tool arguments: {0}")]
    Decode(#[source] serde_json::Error),
}

/// A model-visible tool installed in an agent's heterogeneous tool registry.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the registry and model-visible tool name.
    fn name(&self) -> &'static str;

    /// Returns the model-visible function or freeform definition.
    fn definition(&self) -> ToolDefinition;

    /// Executes one tool call.
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolExecution;
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
    registered: Vec<Arc<dyn Tool>>,
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

    #[error("registered tool name `{registered}` does not match definition name `{definition}`")]
    DefinitionName {
        registered: Box<str>,
        definition: Box<str>,
    },
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

    /// Adds a function or freeform tool to the runtime.
    #[must_use]
    pub fn tool<T: Tool + 'static>(mut self, tool: T) -> Self {
        self.tools.registered.push(Arc::new(tool));
        self
    }

    /// Validates tool names and finishes the runtime configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, inconsistent, duplicate, or enabled built-in
    /// tool names.
    pub fn build(self) -> Result<Tools, ToolsBuildError> {
        let mut names = Vec::with_capacity(self.tools.registered.len());
        for tool in &self.tools.registered {
            let name = tool.name();
            if name.is_empty() {
                return Err(ToolsBuildError::EmptyName);
            }
            let definition = tool.definition();
            if definition.name() != name {
                return Err(ToolsBuildError::DefinitionName {
                    registered: name.into(),
                    definition: definition.name().into(),
                });
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
    registry: Arc<ToolRegistry>,
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
        let registry_workspace = workspace.clone();
        let mut handlers: Vec<Arc<dyn Tool>> = vec![
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
            registry: Arc::new(ToolRegistry::from_ordered(registry_workspace, handlers)),
            code_mode: code_mode::CodeModeRuntime::new(),
            default_shell_name,
        }
    }

    #[must_use]
    pub fn with_tools(mut self, tools: &Tools) -> Self {
        Arc::make_mut(&mut self.registry).extend(tools.registered.iter().cloned());
        self
    }

    pub const fn default_shell_name(&self) -> &'static str {
        self.default_shell_name
    }

    pub fn model_specs(&self) -> Vec<ToolDefinition> {
        vec![
            code_mode::exec_spec(self.registry.definitions()),
            code_mode::wait_spec(),
        ]
    }

    pub async fn execute_code(&self, source: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode
            .execute(source, Arc::clone(&self.registry), context)
            .await
    }

    pub async fn wait_for_code(&self, input: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode.wait(input, context).await
    }
}

#[derive(Clone)]
pub(crate) struct ToolRegistry {
    pub(crate) workspace: PathBuf,
    ordered: Vec<Arc<dyn Tool>>,
    definitions: Vec<ToolDefinition>,
    by_name: HashMap<Box<str>, usize>,
}

impl ToolRegistry {
    pub(crate) async fn execute_nested(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolExecution {
        let Some((handler, definition)) = self.get(name) else {
            return ToolExecution::error(format!("unsupported nested tool call: {name}"));
        };
        let input = match definition {
            ToolDefinition::Function { .. } if !input.is_object() => {
                return ToolExecution::error(format!(
                    "nested function tool {name} requires an object argument"
                ));
            }
            ToolDefinition::Function { .. } => match to_raw_value(&input) {
                Ok(input) => ToolInput::Function(input),
                Err(error) => {
                    return ToolExecution::error(format!("failed to encode {name} input: {error}"));
                }
            },
            ToolDefinition::Custom { .. } => match input.as_str() {
                Some(input) => ToolInput::Freeform(input.to_owned()),
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
        self.entries()
            .map(|(handler, definition)| {
                let kind = match definition {
                    ToolDefinition::Function { .. } => "function",
                    ToolDefinition::Custom { .. } => "freeform",
                };
                json!({
                    "name": handler.name(),
                    "description": definition.description(),
                    "kind": kind,
                })
            })
            .collect()
    }
    fn from_ordered(workspace: PathBuf, ordered: Vec<Arc<dyn Tool>>) -> Self {
        let definitions = ordered.iter().map(|tool| tool.definition()).collect();
        let by_name = ordered
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.name().into(), index))
            .collect();
        Self {
            workspace,
            ordered,
            definitions,
            by_name,
        }
    }

    fn extend(&mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) {
        for tool in tools {
            let index = self.ordered.len();
            self.by_name.insert(tool.name().into(), index);
            self.definitions.push(tool.definition());
            self.ordered.push(tool);
        }
    }

    fn get(&self, name: &str) -> Option<(&Arc<dyn Tool>, &ToolDefinition)> {
        let index = *self.by_name.get(name)?;
        Some((self.ordered.get(index)?, self.definitions.get(index)?))
    }

    pub(crate) fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    fn entries(&self) -> impl Iterator<Item = (&Arc<dyn Tool>, &ToolDefinition)> {
        self.ordered.iter().zip(&self.definitions)
    }
}

/// Produces the compact JSON Schema shape used for macro-generated tools.
#[doc(hidden)]
#[must_use]
pub fn schema_for<T: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2019_09()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<T>();
    let Value::Object(mut schema) =
        serde_json::to_value(schema).expect("a schemars root schema should serialize to an object")
    else {
        unreachable!("a schemars root schema should be an object");
    };
    let mut tool_schema = Map::new();
    for key in [
        "properties",
        "required",
        "type",
        "additionalProperties",
        "$defs",
        "definitions",
        "enum",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
    ] {
        if let Some(value) = schema.remove(key) {
            tool_schema.insert(key.to_owned(), value);
        }
    }
    Value::Object(tool_schema)
}

#[cfg(test)]
mod tests {
    use nanocodex_core::ToolDefinition;
    use serde::Deserialize;
    use serde_json::json;

    use super::{
        DEFAULT_TOOL_OUTPUT_TOKENS, ImageGenerationConfig, Tool, ToolContext, ToolExecution,
        ToolInput, ToolOutputBody, ToolRuntime, Tools, WebSearchConfig,
    };

    struct Double;

    #[derive(Deserialize)]
    struct DoubleInput {
        value: i64,
    }

    #[async_trait::async_trait]
    impl Tool for Double {
        fn name(&self) -> &'static str {
            "double"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition::function(
                self.name(),
                "Doubles an integer.",
                json!({
                    "type": "object",
                    "properties": { "value": { "type": "integer" } },
                    "required": ["value"],
                    "additionalProperties": false
                }),
            )
        }

        async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
            let input = match input.decode_json::<DoubleInput>() {
                Ok(input) => input,
                Err(error) => return ToolExecution::error(error.to_string()),
            };
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
                save_root: std::env::temp_dir().join("nanocodex-test-images"),
            }),
        )
    }

    #[test]
    fn web_search_handler_and_spec_are_absent_when_disabled() {
        let enabled = runtime(true);
        assert!(
            enabled
                .registry
                .entries()
                .any(|(handler, _)| handler.name() == "web__run")
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
                .registry
                .entries()
                .all(|(handler, _)| handler.name() != "web__run")
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

        let (handler, _) = runtime.registry.get("double").unwrap();
        let execution = handler
            .execute(
                ToolInput::Function(
                    serde_json::value::to_raw_value(&json!({ "value": 21 })).unwrap(),
                ),
                ToolContext {
                    model: "test-model",
                    session_id: "test-session",
                    call_id: "test-call",
                    history: &[],
                    output_token_budget: DEFAULT_TOOL_OUTPUT_TOKENS,
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
