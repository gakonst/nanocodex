//! Stable typed projections of the agent event firehose.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, value::RawValue};

use super::stream::AgentEventKind;
use crate::{
    pricing::{CostStatus, EstimatedUsdCost},
    responses::{MessagePhase, ResponseUsageMetadata, Usage},
    tools::ToolOutputBody,
};

/// A normalized view of one event in the session-wide agent firehose.
///
/// Call [`crate::events::AgentEvent::data`] to obtain this view. The original raw
/// payload remains available on [`crate::events::AgentEvent`] for lossless JSONL
/// adapters and forward-compatible diagnostics.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AgentEventData {
    /// One complete inbound or outbound `OpenAI` protocol frame.
    #[cfg(feature = "client")]
    OpenAi(OpenAiEvent),
    /// Incremental or completed assistant output.
    Assistant(AssistantEvent),
    /// Model reasoning that the API made visible.
    Reasoning(ReasoningEvent),
    /// Agent-turn lifecycle state.
    Run(RunEvent),
    /// Tool invocation lifecycle state.
    Tool(ToolEvent),
    /// Logical model-call lifecycle state.
    Model(ModelEvent),
    /// Context compaction lifecycle state.
    Context(ContextEvent),
    /// Lower-level retry or connection diagnostics.
    Transport(TransportEvent),
}

/// One raw `OpenAI` Responses protocol frame with stable routing metadata.
///
/// `event` deliberately remains raw JSON. Provider wire events are already
/// available as the typed per-operation [`crate::ResponseEvent`] stream, while
/// this firehose variant preserves every frame without forcing unknown
/// provider additions through a JSON value tree.
#[derive(Clone, Debug, Deserialize)]
#[cfg(feature = "client")]
pub struct OpenAiEvent {
    /// Whether the frame was sent or received.
    pub direction: String,
    /// Transport identifier used for the frame.
    pub transport: String,
    /// Logical Responses phase such as `generation` or `compaction`.
    pub phase: String,
    /// Agent model-call index, when the frame belongs to one.
    pub model_call_index: Option<u32>,
    /// Exact provider frame.
    pub event: Box<RawValue>,
}

/// Assistant output emitted by the agent.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AssistantEvent {
    /// Incremental displayable text.
    Delta(AssistantDelta),
    /// One complete assistant message.
    Message(AssistantMessage),
}

/// Incremental displayable assistant text.
#[derive(Clone, Debug, Deserialize)]
pub struct AssistantDelta {
    /// Logical model-call index.
    pub model_call_index: u32,
    /// Provider output-item identity when supplied.
    pub item_id: Option<String>,
    /// Commentary or final-answer phase when supplied.
    pub phase: Option<MessagePhase>,
    /// Newly received text.
    pub text: String,
}

/// One complete assistant message.
#[derive(Clone, Debug, Deserialize)]
pub struct AssistantMessage {
    /// Logical model-call index.
    pub model_call_index: u32,
    /// Provider output-item identity when supplied.
    pub item_id: Option<String>,
    /// Commentary or final-answer phase when supplied.
    pub phase: Option<MessagePhase>,
    /// Complete message text.
    pub text: String,
}

/// API-visible reasoning output.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ReasoningEvent {
    /// Incremental reasoning-summary text.
    SummaryDelta(ReasoningSummaryDelta),
}

/// Incremental API-visible reasoning-summary text.
#[derive(Clone, Debug, Deserialize)]
pub struct ReasoningSummaryDelta {
    /// Logical model-call index.
    pub model_call_index: u32,
    /// Newly received summary text.
    pub text: String,
}

/// Lifecycle state for one accepted agent turn.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum RunEvent {
    /// The driver started executing an accepted turn.
    Started(RunStarted),
    /// Steering input entered the active turn FIFO.
    Steered(RunSteered),
    /// A human-readable error accompanied a failed terminal event.
    Error(RunError),
    /// The turn completed successfully.
    Completed(Box<RunTerminal>),
    /// The turn failed or was cancelled.
    Failed(Box<RunTerminal>),
}

/// Configuration captured when an accepted turn begins.
#[derive(Clone, Debug, Deserialize)]
pub struct RunStarted {
    /// Runtime mode.
    pub mode: String,
    /// Fixed model contract.
    pub model: String,
    /// Reasoning execution mode.
    pub reasoning_mode: String,
    /// Thinking effort.
    pub effort: String,
    /// Responses transport.
    pub transport: String,
    /// Agent orchestration policy.
    pub orchestration: String,
    /// Sanitized Responses endpoint.
    pub websocket_url: String,
    /// Agent workspace when one is configured.
    pub workspace: Option<String>,
    /// Bytes in the accepted user instruction.
    pub instruction_bytes: usize,
}

/// A steering input accepted by the active turn.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct RunSteered {
    /// One-based steering index within this turn.
    pub steer_index: u32,
    /// Bytes in the steering instruction.
    pub instruction_bytes: usize,
}

/// Human-readable error detail emitted before a failed terminal event.
#[derive(Clone, Debug, Deserialize)]
pub struct RunError {
    /// Complete error message.
    pub message: String,
}

/// Terminal status of one accepted turn.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunStatus {
    /// The turn completed successfully.
    Completed,
    /// The turn was explicitly cancelled.
    Cancelled,
    /// The turn failed.
    Failed,
}

/// Exact aggregate token counts for a family of Responses operations.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct EventUsage {
    /// Input tokens.
    pub input_tokens: u64,
    /// Input tokens served from cache.
    pub cached_input_tokens: u64,
    /// Input tokens written to cache.
    pub cache_write_input_tokens: u64,
    /// Output tokens, including reasoning.
    pub output_tokens: u64,
    /// Reasoning subset of output tokens.
    pub reasoning_output_tokens: u64,
    /// Provider-reported total tokens.
    pub total_tokens: u64,
}

/// Runtime and transport measurements accumulated by one agent turn.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RunMetrics {
    /// Logical model calls.
    pub model_calls: u32,
    /// Accepted steering inputs.
    pub steers: u32,
    /// Completed context compactions.
    pub compactions: u32,
    /// Tool calls.
    pub tool_calls: u32,
    /// Physical connection attempts.
    pub connection_attempts: u32,
    /// Successful WebSocket replacements.
    pub websocket_reconnects: u32,
    /// Physical Responses attempts.
    pub response_attempts: u32,
    /// Retried Responses attempts.
    pub response_retries: u32,
    /// Nanoseconds spent establishing connections.
    pub connection_duration_ns: u64,
    /// Nanoseconds spent in SDK retry backoff.
    pub retry_backoff_duration_ns: u64,
    /// Nanoseconds spent awaiting logical model work.
    pub model_duration_ns: u64,
    /// Subset of model time spent awaiting context compaction.
    pub compaction_duration_ns: u64,
    /// Nanoseconds spent warming the persistent connection.
    pub warmup_duration_ns: u64,
    /// Sum of top-level tool-handler execution durations after scheduler admission.
    pub tool_work_duration_ns: u64,
    /// Union of wall-clock intervals spent executing top-level tool batches.
    pub tool_wall_duration_ns: u64,
    /// Token usage from generation and compaction operations.
    pub usage: EventUsage,
    /// Token usage from connection warmup.
    pub warmup_usage: EventUsage,
}

/// Complete terminal projection for one accepted turn.
#[derive(Clone, Debug, Deserialize)]
pub struct RunTerminal {
    /// Terminal outcome.
    pub status: RunStatus,
    /// Fixed model contract.
    pub model: String,
    /// Reasoning execution mode.
    pub reasoning_mode: String,
    /// Thinking effort.
    pub effort: String,
    /// Responses transport.
    pub transport: String,
    /// Agent orchestration policy.
    pub orchestration: String,
    /// Whole-millisecond turn duration.
    pub duration_ms: u64,
    /// Nanosecond turn duration.
    pub duration_ns: u64,
    /// Runtime, transport, and token measurements.
    #[serde(flatten)]
    pub metrics: RunMetrics,
    /// Exact estimate using the built-in model and service-tier rates.
    pub estimated_cost: Option<EstimatedUsdCost>,
    /// Floating-point compatibility projection for existing JSONL consumers.
    pub cost_usd: Option<f64>,
    /// Why an exact local estimate is present or unavailable.
    #[serde(default)]
    pub cost_status: CostStatus,
}

/// Lifecycle state for one model-requested tool call.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ToolEvent {
    /// A tool invocation began.
    Call(ToolCall),
    /// A tool invocation finished.
    Result(ToolResultEvent),
}

/// One model-requested tool invocation.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolCall {
    /// Stable provider tool-call identity.
    pub call_id: String,
    /// Canonical registry tool name.
    pub tool: String,
    /// Exact JSON arguments or a JSON string containing freeform input.
    pub arguments: Box<RawValue>,
    /// Logical model-call index that requested the tool.
    pub model_call_index: u32,
}

impl ToolCall {
    /// Decodes function arguments into an application-selected type.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained argument JSON does not match `T`.
    pub fn decode_arguments<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.arguments.get())
    }
}

/// Outcome status of a tool invocation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolStatus {
    /// The tool completed successfully.
    Completed,
    /// The tool returned or raised a failure.
    Failed,
    /// Cancellation interrupted the tool.
    Cancelled,
}

/// Completed model-visible tool output.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolResultEvent {
    /// Stable provider tool-call identity.
    pub call_id: String,
    /// Canonical registry tool name.
    pub tool: String,
    /// Invocation outcome.
    pub status: ToolStatus,
    /// Wall-clock execution duration.
    pub duration_ns: u64,
    /// Offset from the containing Code Mode cell start for nested calls.
    pub started_after_ns: Option<u64>,
    /// Complete model-visible output.
    pub result: ToolOutputBody,
    /// Exact machine-readable tool result.
    pub structured_result: Value,
    /// Optional application or remote-tool metadata.
    pub metadata: Option<Box<RawValue>>,
}

/// Lifecycle state for one logical model operation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ModelEvent {
    /// Persistent-connection warmup began.
    WarmupStarted(ModelWarmupStarted),
    /// Persistent-connection warmup completed.
    WarmupCompleted(ModelWarmupCompleted),
    /// Persistent-connection warmup failed.
    WarmupFailed(ModelWarmupFailed),
    /// A logical `response.create` call began.
    CallStarted(ModelCallStarted),
    /// A logical `response.create` call completed.
    CallCompleted(ModelCallCompleted),
    /// A logical `response.create` call failed.
    CallFailed(ModelCallFailed),
}

/// Persistent-connection warmup input.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelWarmupStarted {
    /// Fixed model contract.
    pub model: String,
    /// Stable prompt-cache identity.
    pub prompt_cache_key: String,
}

/// Completed persistent-connection warmup.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelWarmupCompleted {
    /// Whether work came from a new response or a shared prefix.
    pub source: String,
    /// Physical attempt when a response was sent.
    pub attempt: Option<u32>,
    /// Connection generation when a response was sent.
    pub connection_generation: Option<u32>,
    /// Warmup duration.
    pub duration_ns: u64,
    /// Provider token usage when supplied.
    pub usage: Option<Usage>,
}

/// Failed persistent-connection warmup.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelWarmupFailed {
    /// Warmup duration.
    pub duration_ns: u64,
    /// Complete failure message.
    pub error: String,
}

/// Input metadata for one logical `response.create` call.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelCallStarted {
    /// One-based model-call index within the agent turn.
    pub call_index: u32,
    /// Fixed model contract.
    pub model: String,
    /// Reasoning execution mode.
    pub reasoning_mode: String,
    /// Thinking effort.
    pub effort: String,
}

/// Completed logical `response.create` call.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelCallCompleted {
    /// One-based model-call index within the agent turn.
    pub call_index: u32,
    /// Fixed model contract.
    pub model: String,
    /// Exact provider response identity.
    pub response_id: String,
    /// Successful physical attempt number.
    pub attempt: u32,
    /// Connection generation used by the attempt.
    pub connection_generation: u32,
    /// Provider terminal status.
    pub status: String,
    /// Whole-call duration.
    pub duration_ns: u64,
    /// Time from attempt start to the first provider event.
    pub time_to_first_event_ns: u64,
    /// Time from attempt start to first model output.
    pub time_to_first_output_ns: Option<u64>,
    /// Completed callable output count.
    pub tool_calls: usize,
    /// Provider token usage when supplied.
    pub usage: Option<Usage>,
    /// Exact non-aggregated upstream usage metadata.
    pub usage_metadata: Option<ResponseUsageMetadata>,
}

/// Failed logical `response.create` call.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelCallFailed {
    /// One-based model-call index within the agent turn.
    pub call_index: u32,
    /// Fixed model contract.
    pub model: String,
    /// Whole-call duration.
    pub duration_ns: u64,
    /// Complete failure message.
    pub error: String,
}

/// Context-management lifecycle state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ContextEvent {
    /// Automatic remote compaction began.
    CompactionStarted(CompactionStarted),
    /// Automatic remote compaction completed.
    CompactionCompleted(CompactionCompleted),
    /// Automatic remote compaction failed.
    CompactionFailed(CompactionFailed),
}

/// Input metadata for automatic context compaction.
#[derive(Clone, Debug, Deserialize)]
pub struct CompactionStarted {
    /// Model-call boundary after which compaction ran.
    pub after_model_call_index: u32,
    /// Estimated active context before compaction.
    pub active_context_tokens: u64,
    /// Configured automatic-compaction threshold.
    pub auto_compact_token_limit: u64,
}

/// Completed automatic context compaction.
#[derive(Clone, Debug, Deserialize)]
pub struct CompactionCompleted {
    /// Model-call boundary after which compaction ran.
    pub after_model_call_index: u32,
    /// Exact provider response identity.
    pub response_id: String,
    /// Successful physical attempt number.
    pub attempt: u32,
    /// Connection generation used by the attempt.
    pub connection_generation: u32,
    /// Provider terminal status.
    pub status: String,
    /// Whole-compaction duration.
    pub duration_ns: u64,
    /// Time from attempt start to the first provider event.
    pub time_to_first_event_ns: u64,
    /// Time from attempt start to first compaction output.
    pub time_to_first_output_ns: Option<u64>,
    /// Provider token usage when supplied.
    pub usage: Option<Usage>,
    /// Exact non-aggregated upstream usage metadata.
    pub usage_metadata: Option<ResponseUsageMetadata>,
}

/// Failed automatic context compaction.
#[derive(Clone, Debug, Deserialize)]
pub struct CompactionFailed {
    /// Model-call boundary after which compaction ran.
    pub after_model_call_index: u32,
    /// Whole-compaction duration.
    pub duration_ns: u64,
    /// Complete failure message.
    pub error: String,
}

/// Exact lower-level retry or connection diagnostic retained by the firehose.
///
/// These mechanics intentionally remain outside the stable agent-domain
/// projection. Use [`Self::decode_payload`] for an application-specific
/// diagnostic view, or serialize the containing [`crate::events::AgentEvent`]
/// unchanged.
#[derive(Clone, Debug)]
pub struct TransportEvent {
    kind: AgentEventKind,
    payload: Arc<RawValue>,
}

impl TransportEvent {
    pub(crate) const fn new(kind: AgentEventKind, payload: Arc<RawValue>) -> Self {
        Self { kind, payload }
    }

    /// Returns the exact diagnostic event category.
    #[must_use]
    pub const fn kind(&self) -> AgentEventKind {
        self.kind
    }

    /// Returns the exact retained diagnostic payload.
    #[must_use]
    pub fn raw_payload(&self) -> &RawValue {
        &self.payload
    }

    /// Decodes the diagnostic payload into an application-selected type.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained payload does not match `T`.
    pub fn decode_payload<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.payload.get())
    }
}
