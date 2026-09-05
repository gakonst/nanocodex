use std::sync::Arc;

use nanocodex_oai_api::{
    responses::ResponseItem,
    tools::{ToolContext, ToolOutputBody},
};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};

/// Owned context for a Code Mode cell that may outlive its initiating call.
///
/// Prefer [`ToolContext`] for ordinary synchronous tool handlers. This owned
/// form retains shared history without copying it again when execution crosses
/// an asynchronous embedding boundary.
pub struct OwnedToolContext {
    pub(crate) model: String,
    pub(crate) session_id: String,
    pub(crate) call_id: String,
    pub(crate) history: Arc<Vec<ResponseItem>>,
    pub(crate) output_token_budget: usize,
    pub(crate) host_context: Option<Arc<str>>,
}

impl OwnedToolContext {
    /// Creates an owned context from its complete invocation state.
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
            host_context: None,
        }
    }

    /// Copies a borrowed context into independently owned invocation state.
    #[must_use]
    pub fn from_context(context: ToolContext<'_>) -> Self {
        Self::new(
            context.model(),
            context.session_id(),
            context.call_id(),
            Arc::new(context.history().to_vec()),
            context.output_token_budget(),
        )
        .with_host_context(context.host_context().map(Arc::from))
    }

    /// Borrows this owned state as the standard tool invocation context.
    #[must_use]
    pub fn as_context(&self) -> ToolContext<'_> {
        ToolContext::new(
            &self.model,
            &self.session_id,
            &self.call_id,
            self.history.as_slice(),
            self.output_token_budget,
        )
        .with_host_context(self.host_context.as_deref())
    }

    /// Attaches embedding-owned context to this owned invocation.
    #[doc(hidden)]
    #[must_use]
    pub fn with_host_context(mut self, host_context: Option<Arc<str>>) -> Self {
        self.host_context = host_context;
        self
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) const fn with_output_token_budget(mut self, output_token_budget: usize) -> Self {
        self.output_token_budget = output_token_budget;
        self
    }
}

/// Complete result of one Code Mode cell observation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeExecution {
    /// Ordered model-visible output emitted by the cell.
    pub output: ToolOutputBody,
    /// Whether the JavaScript cell reached a successful terminal state.
    pub success: bool,
    /// Nested tool calls in their original invocation order.
    #[serde(default)]
    pub nested_calls: Vec<NestedToolCall>,
    /// Application notifications emitted by the cell.
    #[serde(default)]
    pub notifications: Vec<CodeModeNotification>,
}

/// One notification emitted by a Code Mode cell.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeModeNotification {
    /// Code Mode call that emitted the notification.
    pub call_id: String,
    /// Complete notification text.
    pub text: String,
}

impl CodeModeNotification {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn new(call_id: &str, text: String) -> Self {
        Self {
            call_id: call_id.to_owned(),
            text,
        }
    }
}

/// Incremental nested-tool update observed while a Code Mode cell runs.
pub enum CodeModeUpdate<'a> {
    /// A nested call was accepted and may now run concurrently.
    NestedCallStarted {
        /// Stable nested call identity.
        call_id: &'a str,
        /// Registered tool name.
        name: &'a str,
        /// Complete JSON input value.
        input: &'a Value,
    },
    /// A nested call reached a terminal result.
    NestedCallCompleted(&'a NestedToolCall),
}

/// Observer for ordered nested-tool lifecycle updates.
///
/// The callback runs inline with Code Mode observation. Implementations should
/// hand off expensive work rather than blocking the cell.
pub trait CodeModeObserver: Send {
    /// Observes one ordered nested-tool update.
    fn update(&mut self, update: CodeModeUpdate<'_>);
}

/// Recorded nested tool call made by one Code Mode cell.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NestedToolCall {
    /// Stable call identity derived from the parent Code Mode invocation.
    pub call_id: String,
    /// Registered tool name.
    pub name: String,
    /// Complete JSON input value.
    pub input: Value,
    /// Complete model-visible output.
    pub output: ToolOutputBody,
    /// Exact machine-readable tool result.
    pub structured_result: Value,
    /// Whether the nested operation succeeded.
    pub success: bool,
    /// Nanoseconds from cell start until this call started.
    pub started_after_ns: u64,
    /// Nanoseconds spent executing this call.
    pub duration_ns: u64,
    /// Optional opaque metadata retained for events and adapters.
    #[serde(default)]
    pub metadata: Option<Box<RawValue>>,
}
