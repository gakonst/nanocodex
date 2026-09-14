//! Dependency-light contract for caller-defined and runtime-provided tools.

use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{
    Value,
    value::{RawValue, to_raw_value},
};

pub use crate::responses::ToolDefinition;

use crate::{ImageDetail, ResponseItem};

/// Default maximum model-visible output budget for one tool call.
pub const DEFAULT_TOOL_OUTPUT_TOKENS: usize = 10_000;

/// Model-visible body returned by a tool.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ToolOutputBody {
    /// Plain text, including serialized JSON returned by function tools.
    Text(String),
    /// Ordered multimodal output.
    Content(Vec<ToolOutputContent>),
}

impl ToolOutputBody {
    /// Returns the machine-readable value represented by this output.
    #[must_use]
    pub fn structured_result(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Content(content) => serde_json::to_value(content).unwrap_or(Value::Null),
        }
    }
}

/// One model-visible item in a multimodal tool output.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutputContent {
    /// Text input returned to the model.
    InputText {
        /// Complete text.
        text: String,
    },
    /// Image input returned to the model.
    InputImage {
        /// Data URL or provider-supported image URL.
        image_url: String,
        /// Requested model image detail.
        detail: ImageDetail,
    },
    /// Audio input returned to the model.
    InputAudio {
        /// Data URL or provider-supported audio URL.
        audio_url: String,
    },
    /// Opaque encrypted provider content returned without exposing plaintext.
    EncryptedContent {
        /// Provider-generated encrypted payload.
        encrypted_content: String,
    },
}

/// Complete output of one tool invocation.
///
/// Use [`Self::text`], [`Self::json`], or [`Self::content`] for successful
/// results. [`Self::error`] creates a structured model-visible failure without
/// turning the handler invocation itself into an error.
pub struct ToolOutput {
    /// Model-visible output body.
    pub output: ToolOutputBody,
    /// Whether the remote or local operation succeeded.
    pub success: bool,
    /// Optional validated opaque metadata for events and adapters.
    pub metadata: Option<Box<RawValue>>,
    structured_result: Option<Value>,
    process_trace: Option<ToolProcessTrace>,
}

/// Lossless process-boundary representation of a tool output.
#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Deserialize, Serialize)]
pub struct ToolOutputWire {
    pub output: ToolOutputBody,
    pub success: bool,
    pub structured_result: Option<Box<RawValue>>,
    pub metadata: Option<Box<RawValue>>,
    pub process_trace: Option<ToolProcessTraceWire>,
}

/// Process measurements attached by process-backed tool implementations.
#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct ToolProcessTrace {
    pub exit_code: Option<i32>,
    pub session_id: Option<i64>,
    pub original_token_count: Option<usize>,
    pub output_bytes: usize,
    pub wall_time_seconds: f64,
}

/// Serialized process measurements.
#[doc(hidden)]
#[allow(missing_docs)]
#[derive(Deserialize, Serialize)]
pub struct ToolProcessTraceWire {
    pub exit_code: Option<i32>,
    pub session_id: Option<i64>,
    pub original_token_count: Option<usize>,
    pub output_bytes: usize,
    pub wall_time_seconds: f64,
}

/// Error returned by an application-defined tool handler.
pub type ToolError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result returned by [`Tool::execute`].
///
/// The owning runtime converts an error into a failed model-visible tool
/// result so the model can recover. Return `Ok(ToolOutput::error(...))` only
/// when preserving a structured failure from a remote tool protocol.
pub type ToolResult = std::result::Result<ToolOutput, ToolError>;

impl ToolOutput {
    /// Creates a successful plain-text output.
    #[must_use]
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(output.into()),
            success: true,
            metadata: None,
            structured_result: None,
            process_trace: None,
        }
    }

    /// Creates a model-visible failed output.
    #[must_use]
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            output: ToolOutputBody::Text(error.into()),
            success: false,
            metadata: None,
            structured_result: None,
            process_trace: None,
        }
    }

    /// Serializes one successful function result as JSON text.
    #[must_use]
    pub fn json(output: &impl Serialize) -> Self {
        match serde_json::to_value(output) {
            Ok(output) => Self::from_json(output, true),
            Err(error) => Self::error(format!("failed to encode tool result: {error}")),
        }
    }

    /// Creates a JSON result with an explicit success state.
    ///
    /// Structured consumers receive the typed JSON value while the Responses
    /// API receives its serialized text representation.
    #[must_use]
    pub fn from_json(output: Value, success: bool) -> Self {
        match serde_json::to_string(&output) {
            Ok(encoded) => Self {
                output: ToolOutputBody::Text(encoded),
                success,
                metadata: None,
                structured_result: Some(output),
                process_trace: None,
            },
            Err(error) => Self::error(format!("failed to encode tool result: {error}")),
        }
    }

    /// Creates a successful multimodal output.
    #[must_use]
    pub const fn content(output: Vec<ToolOutputContent>) -> Self {
        Self {
            output: ToolOutputBody::Content(output),
            success: true,
            metadata: None,
            structured_result: None,
            process_trace: None,
        }
    }

    /// Attaches validated opaque metadata.
    ///
    /// An encoding failure converts this output into a model-visible failure.
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

    /// Returns the exact machine-readable tool result.
    ///
    /// An explicit structured result takes precedence. Otherwise plain text
    /// remains a string and multimodal content becomes an array.
    #[must_use]
    pub fn structured_result(&self) -> Value {
        if let Some(value) = &self.structured_result {
            return value.clone();
        }
        self.output.structured_result()
    }

    /// Sets the exact machine-readable result independently of model-visible output.
    #[must_use]
    pub fn with_structured_result(mut self, value: Value) -> Self {
        self.structured_result = Some(value);
        self
    }

    /// Attaches process measurements from a process-backed implementation.
    #[doc(hidden)]
    #[must_use]
    pub const fn with_process_trace(
        mut self,
        exit_code: Option<i32>,
        session_id: Option<i64>,
        original_token_count: Option<usize>,
        output_bytes: usize,
        wall_time_seconds: f64,
    ) -> Self {
        self.process_trace = Some(ToolProcessTrace {
            exit_code,
            session_id,
            original_token_count,
            output_bytes,
            wall_time_seconds,
        });
        self
    }

    /// Returns attached process measurements.
    #[doc(hidden)]
    #[must_use]
    pub const fn process_trace(&self) -> Option<&ToolProcessTrace> {
        self.process_trace.as_ref()
    }

    /// Converts this output into its lossless process-boundary form.
    ///
    /// # Errors
    ///
    /// Returns an error if the internal structured result cannot be encoded.
    #[doc(hidden)]
    pub fn into_wire(self) -> Result<ToolOutputWire, serde_json::Error> {
        Ok(ToolOutputWire {
            output: self.output,
            success: self.success,
            structured_result: self
                .structured_result
                .map(|value| to_raw_value(&value))
                .transpose()?,
            metadata: self.metadata,
            process_trace: self.process_trace.map(Into::into),
        })
    }

    /// Restores an output received from a process boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if an opaque structured result cannot be decoded.
    #[doc(hidden)]
    pub fn from_wire(wire: ToolOutputWire) -> Result<Self, serde_json::Error> {
        Ok(Self {
            output: wire.output,
            success: wire.success,
            metadata: wire.metadata,
            structured_result: wire
                .structured_result
                .map(|value| serde_json::from_str(value.get()))
                .transpose()?,
            process_trace: wire.process_trace.map(Into::into),
        })
    }
}

impl From<ToolProcessTrace> for ToolProcessTraceWire {
    fn from(trace: ToolProcessTrace) -> Self {
        Self {
            exit_code: trace.exit_code,
            session_id: trace.session_id,
            original_token_count: trace.original_token_count,
            output_bytes: trace.output_bytes,
            wall_time_seconds: trace.wall_time_seconds,
        }
    }
}

impl From<ToolProcessTraceWire> for ToolProcessTrace {
    fn from(trace: ToolProcessTraceWire) -> Self {
        Self {
            exit_code: trace.exit_code,
            session_id: trace.session_id,
            original_token_count: trace.original_token_count,
            output_bytes: trace.output_bytes,
            wall_time_seconds: trace.wall_time_seconds,
        }
    }
}

/// Read-only context for one tool invocation.
#[derive(Clone, Copy)]
pub struct ToolContext<'a> {
    model: &'a str,
    session_id: &'a str,
    call_id: &'a str,
    history: &'a [ResponseItem],
    output_token_budget: usize,
    host_context: Option<&'a str>,
}

impl<'a> ToolContext<'a> {
    /// Creates the complete read-only context for one tool invocation.
    #[must_use]
    pub const fn new(
        model: &'a str,
        session_id: &'a str,
        call_id: &'a str,
        history: &'a [ResponseItem],
        output_token_budget: usize,
    ) -> Self {
        Self {
            model,
            session_id,
            call_id,
            history,
            output_token_budget,
            host_context: None,
        }
    }

    /// Attaches embedding-owned invocation context without exposing it to tool
    /// schemas or serialized model-visible values.
    #[doc(hidden)]
    #[must_use]
    pub const fn with_host_context(mut self, host_context: Option<&'a str>) -> Self {
        self.host_context = host_context;
        self
    }

    /// Returns embedding-owned invocation context, when one was supplied.
    #[doc(hidden)]
    #[must_use]
    pub const fn host_context(self) -> Option<&'a str> {
        self.host_context
    }

    /// Returns the fixed model contract for this invocation.
    #[must_use]
    pub const fn model(self) -> &'a str {
        self.model
    }

    /// Returns the stable client-owned session identity.
    #[must_use]
    pub const fn session_id(self) -> &'a str {
        self.session_id
    }

    /// Returns the provider tool-call identity.
    #[must_use]
    pub const fn call_id(self) -> &'a str {
        self.call_id
    }

    /// Returns committed authoritative history visible at this call boundary.
    #[must_use]
    pub const fn history(self) -> &'a [ResponseItem] {
        self.history
    }

    /// Returns the maximum model-visible tool-output budget.
    #[must_use]
    pub const fn output_token_budget(self) -> usize {
        self.output_token_budget
    }
}

/// Canonical input presented to function and freeform tools.
pub enum ToolInput {
    /// Validated raw JSON arguments from a function call.
    Function(Box<RawValue>),
    /// Complete freeform custom-tool input.
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

/// Invalid access or decoding of typed tool input.
#[derive(Debug, thiserror::Error)]
pub enum ToolInputError {
    /// A function tool received freeform input.
    #[error("expected JSON function arguments")]
    ExpectedFunction,
    /// A custom tool received function arguments.
    #[error("expected freeform tool input")]
    ExpectedFreeform,
    /// Function arguments did not decode into the requested type.
    #[error("failed to parse function arguments: {0}")]
    Decode(#[source] serde_json::Error),
}

/// A caller-defined model-visible tool.
///
/// ```
/// use async_trait::async_trait;
/// use nanocodex_oai_api::{
///     responses::JsonSchema,
///     tools::{
///         Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult,
///     },
/// };
/// use serde_json::json;
///
/// struct DeploymentRegion;
///
/// #[async_trait]
/// impl Tool for DeploymentRegion {
///     fn definition(&self) -> ToolDefinition {
///         ToolDefinition::function(
///             "deployment_region",
///             "Return the production deployment region.",
///             JsonSchema::from(json!({
///                 "type": "object",
///                 "properties": {},
///                 "additionalProperties": false
///             })),
///         )
///     }
///
///     async fn execute(
///         &self,
///         input: ToolInput,
///         _context: ToolContext<'_>,
///     ) -> ToolResult {
///         let _: serde_json::Value = input.decode_json()?;
///         Ok(ToolOutput::text("us-west-2"))
///     }
/// }
/// ```
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Returns the complete model-visible definition and registry name.
    fn definition(&self) -> ToolDefinition;

    /// Returns whether this handler is safe to execute alongside sibling tool calls.
    ///
    /// Execution is serial by default. Opt in only when the handler's state and
    /// effects are safe to overlap with other parallel-capable tools.
    fn supports_parallel_tool_calls(&self) -> bool {
        false
    }

    /// Executes one invocation.
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ToolContext, ToolOutput};

    #[test]
    fn host_context_is_private_opt_in_invocation_state() {
        let context = ToolContext::new("gpt-test", "session", "call", &[], 1_024);
        assert_eq!(context.host_context(), None);

        let context = context.with_host_context(Some("opaque-root-turn"));
        assert_eq!(context.host_context(), Some("opaque-root-turn"));
        assert_eq!(context.model(), "gpt-test");
        assert_eq!(context.session_id(), "session");
        assert_eq!(context.call_id(), "call");
        assert!(context.history().is_empty());
        assert_eq!(context.output_token_budget(), 1_024);
    }

    #[test]
    fn structured_result_preserves_text_and_json_types() {
        assert_eq!(ToolOutput::text("42").structured_result(), json!("42"));
        assert_eq!(ToolOutput::json(&42).structured_result(), json!(42));
    }
}
