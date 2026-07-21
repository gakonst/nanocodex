use std::{collections::HashMap, fmt, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use nanocodex_core::{ImageDetail, OpenAiAuth, ResponseItem, ToolDefinition};
use schemars::{JsonSchema, r#gen::SchemaSettings};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::value::{RawValue, to_raw_value};
use serde_json::{Map, Value, json};
use tracing::{Instrument, info, info_span};

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
    pub(crate) process_trace: Option<ProcessTrace>,
}

/// Owned context used when a Code Mode cell may outlive the model tool call
/// that started it.
#[doc(hidden)]
pub struct OwnedToolContext {
    pub(crate) model: String,
    pub(crate) session_id: String,
    pub(crate) call_id: String,
    pub(crate) history: Arc<Vec<ResponseItem>>,
    pub(crate) output_token_budget: usize,
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

    pub(crate) fn from_borrowed(context: ToolContext<'_>) -> Self {
        Self::new(
            context.model,
            context.session_id,
            context.call_id,
            Arc::new(context.history.to_vec()),
            context.output_token_budget,
        )
    }

    pub(crate) fn borrowed(&self) -> ToolContext<'_> {
        ToolContext {
            model: &self.model,
            session_id: &self.session_id,
            call_id: &self.call_id,
            history: self.history.as_slice(),
            output_token_budget: self.output_token_budget,
        }
    }

    pub(crate) fn with_output_token_budget(mut self, output_token_budget: usize) -> Self {
        self.output_token_budget = output_token_budget;
        self
    }
}

pub(crate) struct ProcessTrace {
    exit_code: Option<i32>,
    session_id: Option<i64>,
    original_token_count: Option<usize>,
    output_bytes: usize,
    wall_time_seconds: f64,
}

/// Error returned by an application-defined tool handler.
pub type ToolError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result returned by [`Tool::execute`].
///
/// The runtime converts an error into a failed model-visible tool result so the
/// model can recover. Return `Ok(ToolExecution)` with `success: false` only when
/// preserving a structured failure payload from a remote tool protocol.
pub type ToolResult = std::result::Result<ToolExecution, ToolError>;

impl ToolExecution {
    #[must_use]
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(output.into()),
            success: true,
            code_mode_value: None,
            metadata: None,
            process_trace: None,
        }
    }

    #[must_use]
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(error.into()),
            success: false,
            code_mode_value: None,
            metadata: None,
            process_trace: None,
        }
    }

    #[must_use]
    pub fn json(output: &impl Serialize) -> Self {
        match serde_json::to_string(output) {
            Ok(output) => Self::text(output),
            Err(error) => Self::error(format!("failed to encode tool result: {error}")),
        }
    }

    /// Returns a JSON value to Code Mode while retaining a serialized form for
    /// the model-visible tool result and event stream.
    #[must_use]
    pub fn from_json(output: Value, success: bool) -> Self {
        match serde_json::to_string(&output) {
            Ok(encoded) => Self {
                output: ToolOutputBody::Text(encoded),
                success,
                code_mode_value: Some(output),
                metadata: None,
                process_trace: None,
            },
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
            process_trace: None,
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

    pub(crate) fn with_process_trace(
        mut self,
        exit_code: Option<i32>,
        session_id: Option<i64>,
        original_token_count: Option<usize>,
        output_bytes: usize,
        wall_time_seconds: f64,
    ) -> Self {
        self.process_trace = Some(ProcessTrace {
            exit_code,
            session_id,
            original_token_count,
            output_bytes,
            wall_time_seconds,
        });
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
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult;
}

/// A lazily populated family of Code Mode tools.
///
/// Providers start with the agent driver, advertise only their small direct
/// tool surface initially, and may make additional tools callable at runtime.
#[async_trait]
pub trait DynamicToolProvider: Send + Sync {
    /// Starts background discovery or connection work. Implementations must be idempotent.
    fn start(&self);

    /// Returns the provider's always-visible tools, such as `tool_search`.
    fn direct_tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Returns deferred tools currently activated for new Code Mode cells.
    fn available_definitions(&self) -> Vec<ToolDefinition>;

    /// Executes an activated deferred tool, or returns `None` when this provider
    /// does not currently expose `name`.
    async fn execute(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> Option<ToolExecution>;
}

pub struct WebSearchConfig {
    pub endpoint: String,
    pub auth: OpenAiAuth,
}

pub struct ImageGenerationConfig {
    pub api_base_url: String,
    pub auth: OpenAiAuth,
    pub save_root: PathBuf,
}

/// Declarative selection of the built-in tools installed for an agent.
#[derive(Clone)]
pub struct Tools {
    web_search: bool,
    image_generation: bool,
    registered: Vec<Arc<dyn Tool>>,
    providers: Vec<Arc<dyn DynamicToolProvider>>,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            web_search: true,
            image_generation: true,
            registered: Vec::new(),
            providers: Vec::new(),
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
            .field("provider_count", &self.providers.len())
            .finish()
    }
}

impl Tools {
    #[must_use]
    pub fn builder() -> ToolsBuilder {
        ToolsBuilder::default()
    }

    /// Resumes configuring this tool selection while preserving its built-ins,
    /// registered tools, and dynamic providers.
    #[must_use]
    pub fn into_builder(self) -> ToolsBuilder {
        ToolsBuilder { tools: self }
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

    /// Starts all dynamic providers without waiting for their handshakes.
    pub fn start_providers(&self) {
        for provider in &self.providers {
            provider.start();
        }
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

    /// Adds a dynamic family of Code Mode tools.
    #[must_use]
    pub fn provider<P: DynamicToolProvider + 'static>(mut self, provider: P) -> Self {
        let provider: Arc<dyn DynamicToolProvider> = Arc::new(provider);
        self.tools.registered.extend(provider.direct_tools());
        self.tools.providers.push(provider);
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
    sessions: Arc<ShellSessions>,
    default_shell_name: &'static str,
}

#[doc(hidden)]
#[derive(Clone)]
pub struct ToolRuntimeControl {
    code_mode: code_mode::CodeModeControl,
    sessions: Arc<ShellSessions>,
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
        let code_mode_workspace = workspace.clone();
        let mut handlers: Vec<Arc<dyn Tool>> = vec![
            Arc::new(shell::ExecCommandHandler::new(
                workspace.clone(),
                Arc::clone(&sessions),
            )),
            Arc::new(shell::WriteStdinHandler::new(Arc::clone(&sessions))),
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
            registry: Arc::new(ToolRegistry::from_ordered(handlers)),
            code_mode: code_mode::CodeModeRuntime::new(code_mode_workspace),
            sessions,
            default_shell_name,
        }
    }

    #[must_use]
    pub fn with_tools(mut self, tools: &Tools) -> Self {
        let registry = Arc::make_mut(&mut self.registry);
        registry.extend(tools.registered.iter().cloned());
        registry.providers.extend(tools.providers.iter().cloned());
        self
    }

    #[must_use]
    pub const fn default_shell_name(&self) -> &'static str {
        self.default_shell_name
    }

    #[doc(hidden)]
    #[must_use]
    pub fn control(&self) -> ToolRuntimeControl {
        ToolRuntimeControl {
            code_mode: self.code_mode.control(),
            sessions: Arc::clone(&self.sessions),
        }
    }

    #[must_use]
    pub fn model_specs(&self) -> Vec<ToolDefinition> {
        vec![
            code_mode::exec_spec(self.registry.definitions()),
            code_mode::wait_spec(),
        ]
    }

    pub async fn execute_code(&self, source: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode
            .execute(
                source,
                Arc::clone(&self.registry),
                OwnedToolContext::from_borrowed(context),
            )
            .await
    }

    /// Executes Code Mode without copying an already-owned history snapshot.
    #[doc(hidden)]
    pub async fn execute_code_owned(
        &self,
        source: &str,
        context: OwnedToolContext,
    ) -> CodeModeExecution {
        self.code_mode
            .execute(source, Arc::clone(&self.registry), context)
            .await
    }

    pub async fn wait_for_code(&self, input: &str, context: ToolContext<'_>) -> CodeModeExecution {
        self.code_mode.wait(input, context).await
    }
}

impl ToolRuntimeControl {
    #[doc(hidden)]
    pub async fn cancel(&self) {
        tokio::join!(
            self.code_mode.terminate_all(),
            self.sessions.terminate_all()
        );
    }
}

#[derive(Clone)]
pub(crate) struct ToolRegistry {
    ordered: Vec<Arc<dyn Tool>>,
    definitions: Vec<ToolDefinition>,
    by_name: HashMap<Box<str>, usize>,
    providers: Vec<Arc<dyn DynamicToolProvider>>,
}

impl ToolRegistry {
    pub(crate) async fn execute_nested(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolExecution {
        let arguments_content = serde_json::to_string(&input).ok();
        let arguments_bytes = serde_json::to_vec(&input).map_or(0, |encoded| encoded.len());
        let arguments_kind = match &input {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        let arguments_count = input.as_object().map_or_else(
            || input.as_array().map_or(1, Vec::len),
            serde_json::Map::len,
        );
        let argument_keys = input
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let span = info_span!(
            target: "nanocodex_tools",
            "tool.execute",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            tool.name = name,
            session.id = context.session_id,
            tool.call_id = context.call_id,
            tool.arguments.bytes = arguments_bytes,
            tool.arguments.kind = arguments_kind,
            tool.arguments.count = arguments_count,
            tool.arguments.keys = argument_keys,
            process.exit.code = tracing::field::Empty,
            process.running = tracing::field::Empty,
            process.wall_time_ms = tracing::field::Empty,
            shell.session.id = tracing::field::Empty,
            tool.output.bytes = tracing::field::Empty,
            tool.output.original_tokens = tracing::field::Empty,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        if let Some(arguments_content) = &arguments_content {
            record_tool_content(&span, "tool.arguments", arguments_content);
        }
        let started_at = std::time::Instant::now();
        let execution = self
            .execute_nested_inner(name, input, context)
            .instrument(span.clone())
            .await;
        if let Ok(content) = serde_json::to_string(&execution.output) {
            record_tool_content(&span, "tool.output", &content);
        }
        span.record(
            "status",
            if execution.success {
                "completed"
            } else {
                "failed"
            },
        );
        span.record(
            "otel.status_code",
            if execution.success { "OK" } else { "ERROR" },
        );
        span.record(
            "duration_ns",
            u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        if let Some(process) = &execution.process_trace {
            if let Some(exit_code) = process.exit_code {
                span.record("process.exit.code", exit_code);
            }
            span.record("process.running", process.session_id.is_some());
            span.record("process.wall_time_ms", process.wall_time_seconds * 1_000.0);
            if let Some(session_id) = process.session_id {
                span.record("shell.session.id", session_id);
            }
            span.record("tool.output.bytes", process.output_bytes);
            if let Some(original_token_count) = process.original_token_count {
                span.record("tool.output.original_tokens", original_token_count);
            }
        }
        execution
    }

    async fn execute_nested_inner(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolExecution {
        let Some((handler, definition)) = self.get(name) else {
            for provider in &self.providers {
                if let Some(execution) = provider.execute(name, input.clone(), context).await {
                    return execution;
                }
            }
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
        match handler.execute(input, context).await {
            Ok(execution) => execution,
            Err(error) => ToolExecution::error(error.to_string()),
        }
    }

    pub(crate) fn nested_tool_metadata(&self) -> Vec<Value> {
        let mut metadata = self
            .entries()
            .map(|(handler, definition)| definition_metadata(handler.name(), definition))
            .collect::<Vec<_>>();
        for definition in self
            .providers
            .iter()
            .flat_map(|provider| provider.available_definitions())
        {
            metadata.push(definition_metadata(definition.name(), &definition));
        }
        metadata
    }
    fn from_ordered(ordered: Vec<Arc<dyn Tool>>) -> Self {
        let definitions = ordered.iter().map(|tool| tool.definition()).collect();
        let by_name = ordered
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.name().into(), index))
            .collect();
        Self {
            ordered,
            definitions,
            by_name,
            providers: Vec::new(),
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

fn record_tool_content(span: &tracing::Span, kind: &'static str, content: &str) {
    span.in_scope(|| {
        info!(
            target: "nanocodex_tools",
            content_kind = kind,
            content,
            "tool content"
        );
    });
}

fn definition_metadata(name: &str, definition: &ToolDefinition) -> Value {
    let kind = match definition {
        ToolDefinition::Function { .. } => "function",
        ToolDefinition::Custom { .. } => "freeform",
    };
    json!({
        "name": name,
        "description": definition.description(),
        "kind": kind,
    })
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use nanocodex_core::{OpenAiAuth, ToolDefinition};
    use serde::Deserialize;
    use serde_json::json;

    use super::{
        DEFAULT_TOOL_OUTPUT_TOKENS, DynamicToolProvider, ImageGenerationConfig, Tool, ToolContext,
        ToolExecution, ToolInput, ToolOutputBody, ToolResult, ToolRuntime, Tools, WebSearchConfig,
    };

    struct Double;

    struct Fails;

    struct Search {
        activated: Arc<AtomicBool>,
    }

    struct DeferredProvider {
        activated: Arc<AtomicBool>,
        started: AtomicBool,
    }

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

        async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
            let input = input.decode_json::<DoubleInput>()?;
            Ok(ToolExecution::text((input.value * 2).to_string()))
        }
    }

    #[async_trait::async_trait]
    impl Tool for Fails {
        fn name(&self) -> &'static str {
            "fails"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition::function(
                self.name(),
                "Always fails.",
                json!({ "type": "object", "properties": {} }),
            )
        }

        async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
            Err(std::io::Error::other("intentional handler failure").into())
        }
    }

    #[async_trait::async_trait]
    impl Tool for Search {
        fn name(&self) -> &'static str {
            "tool_search"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition::function(
                self.name(),
                "Activates a matching deferred tool.",
                json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            )
        }

        async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
            self.activated.store(true, Ordering::Release);
            Ok(ToolExecution::from_json(
                json!({ "name": "deferred_echo" }),
                true,
            ))
        }
    }

    #[async_trait::async_trait]
    impl DynamicToolProvider for DeferredProvider {
        fn start(&self) {
            self.started.store(true, Ordering::Release);
        }

        fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![Arc::new(Search {
                activated: Arc::clone(&self.activated),
            })]
        }

        fn available_definitions(&self) -> Vec<ToolDefinition> {
            self.activated
                .load(Ordering::Acquire)
                .then(|| {
                    ToolDefinition::function(
                        "deferred_echo",
                        "Returns its input.",
                        json!({ "type": "object", "properties": {} }),
                    )
                })
                .into_iter()
                .collect()
        }

        async fn execute(
            &self,
            name: &str,
            input: serde_json::Value,
            _context: ToolContext<'_>,
        ) -> Option<ToolExecution> {
            (name == "deferred_echo" && self.activated.load(Ordering::Acquire))
                .then(|| ToolExecution::from_json(input, true))
        }
    }

    fn runtime(web_search: bool) -> ToolRuntime {
        ToolRuntime::new(
            ".",
            web_search.then(|| WebSearchConfig {
                endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
                auth: OpenAiAuth::api_key("test-key"),
            }),
            Some(ImageGenerationConfig {
                api_base_url: "http://127.0.0.1:1/v1".to_owned(),
                auth: OpenAiAuth::api_key("test-key"),
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
    async fn registered_tool_is_described_and_callable_from_code_mode() {
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

        let execution = runtime
            .execute_code(
                r"
const result = await tools.double({ value: 21 });
text(result);
",
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
        assert_eq!(execution.nested_calls.len(), 1);
        assert_eq!(execution.nested_calls[0].name, "double");
        assert_eq!(execution.nested_calls[0].input, json!({ "value": 21 }));
        let ToolOutputBody::Content(content) = execution.output else {
            panic!("expected content output");
        };
        assert_eq!(
            serde_json::to_value(content)
                .unwrap()
                .as_array()
                .unwrap()
                .last(),
            Some(&json!({ "type": "input_text", "text": "42" }))
        );
    }

    #[tokio::test]
    async fn handler_errors_become_failed_model_visible_results() {
        let tools = Tools::builder()
            .without_defaults()
            .tool(Fails)
            .build()
            .unwrap();
        let runtime = ToolRuntime::new(".", None, None).with_tools(&tools);
        let execution = runtime
            .registry
            .execute_nested(
                "fails",
                json!({}),
                ToolContext {
                    model: "test-model",
                    session_id: "test-session",
                    call_id: "test-call",
                    history: &[],
                    output_token_budget: DEFAULT_TOOL_OUTPUT_TOKENS,
                },
            )
            .await;

        assert!(!execution.success);
        assert!(matches!(
            execution.output,
            ToolOutputBody::Text(output) if output == "intentional handler failure"
        ));
    }

    #[tokio::test]
    async fn code_mode_can_search_and_call_a_deferred_tool_in_one_cell() {
        let tools = Tools::builder()
            .without_defaults()
            .provider(DeferredProvider {
                activated: Arc::new(AtomicBool::new(false)),
                started: AtomicBool::new(false),
            })
            .build()
            .unwrap();
        tools.start_providers();
        let runtime = ToolRuntime::new(".", None, None).with_tools(&tools);
        let execution = runtime
            .execute_code(
                r#"
const found = await tools.tool_search({ query: "echo" });
const result = await tools[found.name]({ value: 21 });
text(result.value);
"#,
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
        assert_eq!(execution.nested_calls.len(), 2);
        assert_eq!(execution.nested_calls[0].name, "tool_search");
        assert_eq!(execution.nested_calls[1].name, "deferred_echo");
        let ToolOutputBody::Content(content) = execution.output else {
            panic!("expected content output");
        };
        assert_eq!(
            serde_json::to_value(content)
                .unwrap()
                .as_array()
                .unwrap()
                .last(),
            Some(&json!({ "type": "input_text", "text": "21" }))
        );
    }
}
