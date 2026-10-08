//! Claude-native Messages protocol and an experimental agent-loop backend.
//!
//! Authentication uses an explicit Console API key, a host header provider, or
//! the Rust subscription manager with private host storage and HTTP capabilities.
//! The crate never reads Claude Code credentials.
//!
//! Agent sessions retry transient model failures before publishing text or risking
//! repeated server effects, with five attempts per model call and three per compaction.
//! Cancellable backoff grows through 1, 2, 4, and 8 seconds with 90–110% jitter.
//! `Retry-After` is a minimum delay; hints over 60 seconds end the call.
//! Direct [`ClaudeClient`] calls leave transient retry policy to their caller.
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

mod auth;
mod hooks;
mod prompt;
pub use hooks::{
    ClaudeHookFuture, ClaudeLifecycleDecision, ClaudeLifecycleEvent, ClaudeLifecycleInvocation,
    ClaudeLifecycleOutcome, ClaudeToolDecision, ClaudeToolHooks,
};
mod subscription_wire;
pub use subscription_wire::SubscriptionIdentity;
pub mod subscription;
pub use auth::{ClaudeAccessToken, ClaudeTokenSource, RefreshingClaudeAuth};

pub const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
/// Messages route observed in Claude Code's subscription client.
pub const ANTHROPIC_SUBSCRIPTION_MESSAGES_URL: &str =
    "https://api.anthropic.com/v1/messages?beta=true";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Research-preview beta required alongside `speed` on Messages requests.
const FAST_MODE_BETA: &str = "fast-mode-2026-02-01";

#[derive(Debug, Error)]
pub enum ClaudeError {
    #[error("Messages HTTP {status}: {body}")]
    Http {
        status: u16,
        body: String,
        /// Provider delay from a valid `Retry-After` header.
        retry_after: Option<Duration>,
    },
    #[error("Messages transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Messages JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Messages stream error {kind}: {message}")]
    StreamError { kind: String, message: String },
    #[error("Messages protocol: {0}")]
    Protocol(String),
    #[error("Messages stream ended before message_stop")]
    IncompleteStream,
    #[error("approved Claude authentication provider unavailable")]
    AuthUnavailable,
}

impl ClaudeError {
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            Self::Http { status, .. } => *status == 429 || (500..600).contains(status),
            Self::Transport(error) => error.is_timeout() || error.is_request() || error.is_body(),
            Self::StreamError { kind, .. } => matches!(
                kind.as_str(),
                "overloaded_error" | "api_error" | "rate_limit_error" | "timeout_error"
            ),
            Self::IncompleteStream => true,
            Self::Json(_) | Self::Protocol(_) | Self::AuthUnavailable => false,
        }
    }
}

/// Parses delta-seconds or an HTTP-date; a past date means no extra delay.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    let deadline = httpdate::parse_http_date(value)
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .ok()?;
    Some(deadline.saturating_sub(now))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Image {
        source: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    Document {
        source: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    Text {
        text: String,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    ServerToolUse {
        id: String,
        name: String,
        #[serde(default = "empty_object")]
        input: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    WebSearchToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    WebFetchToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    ToolSearchToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    CodeExecutionToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    BashCodeExecutionToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    TextEditorCodeExecutionToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    McpToolUse {
        id: String,
        name: String,
        server_name: String,
        #[serde(default = "empty_object")]
        input: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    McpToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    McpToolListing {
        mcp_server_name: String,
        tools: Value,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: String,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    RedactedThinking {
        data: String,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
}

/// Claude accepts a plain string or an array of nested text/image/document blocks.
/// Blocks retain the provider JSON shape for media and future block variants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<Value>),
}

fn empty_object() -> Value {
    serde_json::json!({})
}

fn is_false(value: &bool) -> bool {
    !value
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            extra: BTreeMap::new(),
        }
    }

    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            extra: BTreeMap::new(),
        }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        Self::ToolResult {
            tool_use_id: id.into(),
            content: ToolResultContent::Text(content.into()),
            is_error,
            extra: BTreeMap::new(),
        }
    }

    pub fn tool_result_content(
        id: impl Into<String>,
        content: ToolResultContent,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            tool_use_id: id.into(),
            content,
            is_error,
            extra: BTreeMap::new(),
        }
    }

    pub fn tool_result_blocks(id: impl Into<String>, blocks: Vec<Value>, is_error: bool) -> Self {
        Self::ToolResult {
            tool_use_id: id.into(),
            content: ToolResultContent::Blocks(blocks),
            is_error,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub const fn tool_results(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub defer_loading: bool,
}

/// Anthropic-executed API tools do not have client handlers or tool_result replies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(flatten)]
    pub options: BTreeMap<String, Value>,
}
impl ServerToolDefinition {
    pub fn tool_search_bm25() -> Self {
        Self {
            kind: "tool_search_tool_bm25_20251119".into(),
            name: "tool_search_tool_bm25".into(),
            options: BTreeMap::new(),
        }
    }
    pub fn tool_search_regex() -> Self {
        Self {
            kind: "tool_search_tool_regex_20251119".into(),
            name: "tool_search_tool_regex".into(),
            options: BTreeMap::new(),
        }
    }
    pub fn web_search_basic(max_uses: u32) -> Self {
        Self {
            kind: "web_search_20250305".into(),
            name: "web_search".into(),
            options: BTreeMap::from([("max_uses".into(), Value::from(max_uses))]),
        }
    }
    /// Latest web search with response inclusion control; the provider may
    /// automatically run code execution for dynamic filtering.
    pub fn web_search_current(max_uses: u32) -> Self {
        Self {
            kind: "web_search_20260318".into(),
            name: "web_search".into(),
            options: BTreeMap::from([("max_uses".into(), Value::from(max_uses))]),
        }
    }
    /// Latest web fetch with response inclusion control.
    pub fn web_fetch_current(max_uses: u32) -> Self {
        Self {
            kind: "web_fetch_20260318".into(),
            name: "web_fetch".into(),
            options: BTreeMap::from([("max_uses".into(), Value::from(max_uses))]),
        }
    }
    /// Provider-executed code sandbox, distinct from local Claude Code Bash.
    pub fn code_execution_current() -> Self {
        Self {
            kind: "code_execution_20260521".into(),
            name: "code_execution".into(),
            options: BTreeMap::new(),
        }
    }
    pub fn web_fetch_basic(max_uses: u32) -> Self {
        Self {
            kind: "web_fetch_20250910".into(),
            name: "web_fetch".into(),
            options: BTreeMap::from([("max_uses".into(), Value::from(max_uses))]),
        }
    }
}

/// One entry in the Claude Messages tools array, not an OpenAI tool schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClaudeToolSpec {
    Client(ToolDefinition),
    Server(ServerToolDefinition),
}
impl From<ToolDefinition> for ClaudeToolSpec {
    fn from(value: ToolDefinition) -> Self {
        Self::Client(value)
    }
}
impl From<ServerToolDefinition> for ClaudeToolSpec {
    fn from(value: ServerToolDefinition) -> Self {
        Self::Server(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: CacheType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<CacheTtl>,
}
impl CacheControl {
    pub const fn ephemeral() -> Self {
        Self {
            kind: CacheType::Ephemeral,
            ttl: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
    Ephemeral,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheTtl {
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

/// Adaptive-thinking depth on current Claude models. No legacy thinking budget is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputConfig {
    pub effort: Effort,
}

/// Inference speed for models that offer fast mode. Fast mode runs the same
/// model at premium rates and does not share prompt-cache prefixes with
/// standard-speed requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speed {
    Standard,
    Fast,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Value>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ClaudeToolSpec>,
}

impl CacheControl {
    const fn effective_ttl(&self) -> CacheTtl {
        match self.ttl {
            Some(ttl) => ttl,
            None => CacheTtl::FiveMinutes,
        }
    }
}

#[derive(Default)]
struct CacheLayout {
    explicit: Vec<CacheTtl>,
    // None means no eligible block; Some(None) means an unmarked final block.
    last_eligible: Option<Option<CacheTtl>>,
}

impl CacheLayout {
    fn block(&mut self, control: Option<&Value>, eligible: bool) -> Result<(), ClaudeError> {
        let ttl = control
            .map(|value| {
                if !eligible {
                    return Err(ClaudeError::Protocol(
                        "cache_control cannot target thinking or empty text blocks".into(),
                    ));
                }
                serde_json::from_value::<CacheControl>(value.clone())
                    .map(|control| control.effective_ttl())
                    .map_err(|_| ClaudeError::Protocol("invalid cache_control type or TTL".into()))
            })
            .transpose()?;
        if let Some(ttl) = ttl {
            self.explicit.push(ttl);
        }
        if eligible {
            self.last_eligible = Some(ttl);
        }
        Ok(())
    }

    fn validate(mut self, automatic: Option<&CacheControl>) -> Result<(), ClaudeError> {
        if let (Some(automatic), Some(last)) = (automatic, self.last_eligible) {
            let ttl = automatic.effective_ttl();
            if let Some(explicit) = last {
                if explicit != ttl {
                    return Err(ClaudeError::Protocol(
                        "automatic cache TTL conflicts with the final cache breakpoint".into(),
                    ));
                }
            } else {
                self.explicit.push(ttl);
            }
        }
        if self.explicit.len() > 4 {
            return Err(ClaudeError::Protocol(
                "at most four cache breakpoints are allowed".into(),
            ));
        }
        let mut short_ttl = false;
        for ttl in self.explicit {
            match ttl {
                CacheTtl::FiveMinutes => short_ttl = true,
                CacheTtl::OneHour if short_ttl => {
                    return Err(ClaudeError::Protocol(
                        "1h cache breakpoints must precede 5m cache breakpoints".into(),
                    ));
                }
                CacheTtl::OneHour => {}
            }
        }
        Ok(())
    }
}

impl MessagesRequest {
    /// Validate the documented cache marker limit, TTL order, and targets.
    /// Cache order is tools -> system -> messages, independent of JSON key order.
    /// Does not predict cache hits, token thresholds, or model-specific eviction.
    pub fn validate_cache_control(&self) -> Result<(), ClaudeError> {
        let mut layout = CacheLayout::default();
        for tool in &self.tools {
            let control = match tool {
                ClaudeToolSpec::Server(tool) => tool.options.get("cache_control"),
                ClaudeToolSpec::Client(_) => None,
            };
            layout.block(control, true)?;
        }
        match self.system.as_ref() {
            Some(Value::String(text)) => layout.block(None, !text.is_empty())?,
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    layout.block(block.get("cache_control"), cacheable_json_block(block))?;
                }
            }
            _ => {}
        }
        for message in &self.messages {
            for block in &message.content {
                let (control, eligible) = block.cache_slot();
                layout.block(control, eligible)?;
            }
        }
        layout.validate(self.cache_control.as_ref())
    }

    /// When automatic caching is enabled, also write a stable system prefix so
    /// it can survive replacement of message history during compaction. Existing
    /// system breakpoints are caller policy and are never moved. This optional
    /// optimization is skipped when it would exceed the marker budget or violate
    /// TTL ordering. Only this request's system representation is changed.
    pub fn cache_system_prefix(&mut self) -> Result<(), ClaudeError> {
        self.validate_cache_control()?;
        let Some(control) = self.cache_control.as_ref() else {
            return Ok(());
        };
        let mut blocks = match self.system.as_ref() {
            Some(Value::String(text)) if !text.is_empty() => {
                vec![serde_json::json!({"type":"text","text":text})]
            }
            Some(Value::Array(blocks)) => blocks.clone(),
            _ => return Ok(()),
        };
        if blocks
            .iter()
            .any(|block| block.get("cache_control").is_some())
        {
            return Ok(());
        }
        let Some(block) = blocks.iter_mut().rev().find(|block| {
            block.get("type").and_then(Value::as_str) == Some("text")
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty())
        }) else {
            return Ok(());
        };
        block["cache_control"] = serde_json::to_value(control)?;
        let previous = self.system.replace(Value::Array(blocks));
        if self.validate_cache_control().is_err() {
            self.system = previous;
        }
        Ok(())
    }
    /// Replace top-level automatic caching with the equivalent explicit marker
    /// on the last cacheable block, as Claude Code does. Automatic caching is
    /// defined as one breakpoint on that block, so validated layouts keep the
    /// same breakpoint count, TTL order and target; an equal explicit marker
    /// already there is the documented deduplication. Message blocks are
    /// preferred, then the system array. Thinking and empty text are skipped.
    pub fn explicit_cache_tail(&mut self) -> Result<(), ClaudeError> {
        self.validate_cache_control()?;
        let Some(control) = self.cache_control.take() else {
            return Ok(());
        };
        let marker = serde_json::to_value(&control)?;
        for block in self
            .messages
            .iter_mut()
            .rev()
            .flat_map(|message| message.content.iter_mut().rev())
        {
            if block.cache_slot().1 {
                block
                    .extra_mut()
                    .entry("cache_control".to_owned())
                    .or_insert(marker);
                return Ok(());
            }
        }
        if let Some(Value::Array(blocks)) = self.system.as_mut()
            && let Some(block) = blocks
                .iter_mut()
                .rev()
                .find(|block| cacheable_json_block(block))
            && let Some(block) = block.as_object_mut()
        {
            block.entry("cache_control").or_insert(marker);
        }
        // Without any cacheable target the automatic marker had no effect.
        Ok(())
    }
}

impl ContentBlock {
    /// The block's own cache marker and whether it may carry one.
    fn cache_slot(&self) -> (Option<&Value>, bool) {
        match self {
            Self::Text { text, extra } => (extra.get("cache_control"), !text.is_empty()),
            Self::Thinking { extra, .. } | Self::RedactedThinking { extra, .. } => {
                (extra.get("cache_control"), false)
            }
            Self::Image { extra, .. }
            | Self::Document { extra, .. }
            | Self::ToolResult { extra, .. }
            | Self::ToolUse { extra, .. }
            | Self::ServerToolUse { extra, .. }
            | Self::WebSearchToolResult { extra, .. }
            | Self::WebFetchToolResult { extra, .. }
            | Self::ToolSearchToolResult { extra, .. }
            | Self::CodeExecutionToolResult { extra, .. }
            | Self::BashCodeExecutionToolResult { extra, .. }
            | Self::TextEditorCodeExecutionToolResult { extra, .. }
            | Self::McpToolUse { extra, .. }
            | Self::McpToolResult { extra, .. }
            | Self::McpToolListing { extra, .. } => (extra.get("cache_control"), true),
        }
    }
    const fn extra_mut(&mut self) -> &mut BTreeMap<String, Value> {
        match self {
            Self::Image { extra, .. }
            | Self::Document { extra, .. }
            | Self::Text { extra, .. }
            | Self::ToolUse { extra, .. }
            | Self::ServerToolUse { extra, .. }
            | Self::WebSearchToolResult { extra, .. }
            | Self::WebFetchToolResult { extra, .. }
            | Self::ToolSearchToolResult { extra, .. }
            | Self::CodeExecutionToolResult { extra, .. }
            | Self::BashCodeExecutionToolResult { extra, .. }
            | Self::TextEditorCodeExecutionToolResult { extra, .. }
            | Self::McpToolUse { extra, .. }
            | Self::McpToolResult { extra, .. }
            | Self::McpToolListing { extra, .. }
            | Self::ToolResult { extra, .. }
            | Self::Thinking { extra, .. }
            | Self::RedactedThinking { extra, .. } => extra,
        }
    }
}

fn cacheable_json_block(block: &Value) -> bool {
    match block.get("type").and_then(Value::as_str) {
        Some("thinking" | "redacted_thinking") => false,
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        _ => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    ModelContextWindowExceeded,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageResponse {
    pub id: String,
    pub role: Role,
    pub model: String,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    pub usage: Usage,
    #[serde(default)]
    pub container: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct UsageDelta {
    pub input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    SignatureDelta {
        signature: String,
    },
    CitationsDelta {
        citation: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ApiError {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
}

/// Raw Messages SSE events, without tool execution or orchestration.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: MessageResponse,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: ContentDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageChange,
        usage: UsageDelta,
    },
    MessageStop,
    Ping,
    Error {
        error: ApiError,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MessageChange {
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    #[serde(default)]
    pub container: Option<Value>,
}

/// Authentication future on a native runtime.
#[cfg(not(target_family = "wasm"))]
pub type ClaudeAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Authentication future on an isolate-local runtime, allowing browser fetch.
#[cfg(target_family = "wasm")]
pub type ClaudeAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// An embedding-owned, approved credential broker. It can refresh/rotate OAuth
/// headers before each request without exposing tokens to the agent loop.
/// This trait does not perform OAuth registration or define a subscription grant.
pub trait ClaudeAuthProvider: Send + Sync {
    fn headers(
        &self,
    ) -> ClaudeAuthFuture<'_, Result<reqwest::header::HeaderMap, ClaudeAuthUnavailable>>;

    /// Recover a rejected credential before one bounded retry. The default does
    /// not retry. Implementations must compare the rejected headers with their
    /// current credential so late 401 responses cannot evict a newer token.
    fn recover_unauthorized<'a>(
        &'a self,
        _rejected: &'a reqwest::header::HeaderMap,
    ) -> ClaudeAuthFuture<'a, Result<bool, ClaudeAuthUnavailable>> {
        Box::pin(async { Ok(false) })
    }
}

/// An intentionally detail-free failure; do not put access/refresh tokens in errors.
#[derive(Debug, Error)]
#[error("approved Claude authentication provider unavailable")]
pub struct ClaudeAuthUnavailable;

#[derive(Clone)]
enum ClientAuth {
    ApiKey(String),
    Headers(reqwest::header::HeaderMap),
    Provider(Arc<dyn ClaudeAuthProvider>),
}

/// Public protocol affinity frozen with an admitted operation, never credentials.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenWireProfile {
    enabled: bool,
    identity: SubscriptionIdentity,
    session: String,
    /// Automatic caching is sent as an explicit final-block marker. Absent in
    /// cursors admitted before that conversion, which keep the top-level field.
    #[serde(default, skip_serializing_if = "is_false")]
    cache_tail: bool,
}

#[derive(Clone)]
pub struct ClaudeClient {
    http: reqwest::Client,
    endpoint: String,
    auth: ClientAuth,
    subscription_compatibility: bool,
    subscription_identity: SubscriptionIdentity,
    subscription_session: String,
    // New subscription requests send automatic caching as an explicit final
    // block marker. Legacy cursors restore `false` to keep their exact bytes.
    subscription_cache_tail: bool,
}

impl ClaudeClient {
    /// `endpoint` can be the official Console API URL or an explicitly chosen
    /// compatible endpoint, such as a loopback fixture. The key is never logged.
    pub fn new(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            auth: ClientAuth::ApiKey(api_key.into()),
            subscription_compatibility: false,
            subscription_identity: SubscriptionIdentity::default(),
            subscription_session: uuid::Uuid::new_v4().to_string(),
            subscription_cache_tail: true,
        }
    }

    /// Use authentication headers supplied by the embedding application. This
    /// transport does not infer token schemes, read local credentials, or grant
    /// authority to use a particular endpoint. The caller must select an
    /// approved authentication mechanism and trusted endpoint.
    pub fn with_auth_headers(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        headers: reqwest::header::HeaderMap,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            auth: ClientAuth::Headers(headers),
            subscription_compatibility: false,
            subscription_identity: SubscriptionIdentity::default(),
            subscription_session: uuid::Uuid::new_v4().to_string(),
            subscription_cache_tail: true,
        }
    }

    /// Resolve caller-owned, approved headers separately for every request.
    /// The provider owns OAuth acquisition/refresh and any program-specific
    /// authorization; no Claude Code identity or local credential is borrowed.
    pub fn with_auth_provider(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        provider: Arc<dyn ClaudeAuthProvider>,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            auth: ClientAuth::Provider(provider),
            subscription_compatibility: false,
            subscription_identity: SubscriptionIdentity::default(),
            subscription_session: uuid::Uuid::new_v4().to_string(),
            subscription_cache_tail: true,
        }
    }

    pub fn official(http: reqwest::Client, api_key: impl Into<String>) -> Self {
        Self::new(http, ANTHROPIC_MESSAGES_URL, api_key)
    }

    /// Use a subscription manager on the observed subscription Messages route.
    /// The provider supplies OAuth headers and owns login/refresh state outside
    /// agent checkpoints. No local Claude Code login or API key is discovered.
    pub fn subscription(http: reqwest::Client, provider: Arc<dyn ClaudeAuthProvider>) -> Self {
        Self::with_auth_provider(http, ANTHROPIC_SUBSCRIPTION_MESSAGES_URL, provider)
            .subscription_compatibility()
    }

    /// Enable the observed subscription compatibility profile for an explicitly
    /// selected endpoint. Uses OMP v18.4.4's Claude Code wire fingerprint;
    /// this supplies neither a credential nor authorization.
    ///
    /// Agent requests are prepared before durable request freezing. Raw transport
    /// callers can inspect [`Self::request_body`] before `create` or `stream`.
    /// Final bytes are attested before dispatch and before durable effect identity.
    pub fn subscription_compatibility(mut self) -> Self {
        self.subscription_compatibility = true;
        #[cfg(not(target_family = "wasm"))]
        if let Ok(version) = std::env::var("PI_AI_CLAUDE_CODE_VERSION")
            && !version.is_empty()
        {
            self.subscription_identity.version = Some(version);
        }
        self
    }

    /// Set public OMP wire identity. No OAuth credentials are accepted here.
    /// Reuse installation identity and version when reopening durable sessions.
    pub fn with_subscription_identity(mut self, identity: SubscriptionIdentity) -> Self {
        self.subscription_identity = identity;
        self
    }
    pub(crate) fn bind_subscription_session(mut self, session: &str) -> Self {
        self.subscription_session = session.to_owned();
        self
    }
    /// Prepare subscription protocol blocks before freezing a logical request.
    /// `request_body` performs wire-only names, metadata and CCH over final bytes.
    pub(crate) fn freeze_wire_profile(&self, automatic_cache: bool) -> FrozenWireProfile {
        let mut identity = self.subscription_identity.clone();
        identity.version = Some(identity.version().to_owned());
        identity
            .install_id
            .get_or_insert_with(|| self.subscription_session.clone());
        if identity.platform.is_none() {
            identity.platform = Some(
                if cfg!(target_os = "macos") {
                    "darwin"
                } else if cfg!(target_os = "windows") {
                    "win32"
                } else {
                    "linux"
                }
                .into(),
            );
        }
        if identity.arch.is_none() {
            identity.arch = Some(
                if cfg!(target_arch = "aarch64") {
                    "arm64"
                } else if cfg!(target_arch = "x86") {
                    "ia32"
                } else {
                    "x64"
                }
                .into(),
            );
        }
        FrozenWireProfile {
            enabled: self.subscription_compatibility,
            identity,
            session: self.subscription_session.clone(),
            // Serialized only when it changes bytes, so cursors without
            // automatic caching remain readable by earlier releases.
            cache_tail: self.subscription_compatibility
                && self.subscription_cache_tail
                && automatic_cache,
        }
    }
    pub(crate) fn restore_wire_profile(&self, profile: Option<&FrozenWireProfile>) -> Self {
        let mut client = self.clone();
        // Old cursors predate final-byte attestation: retain their legacy body and
        // effect identity instead of silently adopting new default transformations.
        client.subscription_compatibility = profile.is_some_and(|p| p.enabled);
        client.subscription_cache_tail = profile.is_some_and(|p| p.cache_tail);
        if let Some(profile) = profile {
            client.subscription_identity = profile.identity.clone();
            client.subscription_session = profile.session.clone();
        }
        client
    }

    pub fn prepare_request(&self, request: &mut MessagesRequest) {
        if self.subscription_compatibility {
            subscription_wire::prepare(request, &self.subscription_identity);
        }
    }
    /// Exact final request bytes used both for durable identity and HTTP.
    pub fn request_body(
        &self,
        request: &MessagesRequest,
        streaming: bool,
    ) -> Result<String, ClaudeError> {
        if self.subscription_compatibility {
            subscription_wire::body(
                request,
                streaming,
                &self.subscription_identity,
                &self.subscription_session,
                self.subscription_cache_tail,
            )
        } else {
            #[derive(Serialize)]
            struct Body<'a> {
                #[serde(flatten)]
                request: &'a MessagesRequest,
                stream: bool,
            }
            Ok(serde_json::to_string(&Body {
                request,
                stream: streaming,
            })?)
        }
    }
    pub(crate) fn durable_request(&self, request: &MessagesRequest) -> Result<Value, ClaudeError> {
        if self.subscription_compatibility {
            Ok(serde_json::json!({"subscription_wire_v1":self.request_body(request,true)?}))
        } else {
            Ok(serde_json::to_value(request)?)
        }
    }
    fn decode_block(&self, block: &mut ContentBlock) {
        if self.subscription_compatibility
            && let ContentBlock::ToolUse { name, .. } = block
        {
            subscription_wire::strip(name);
        }
    }

    async fn post(
        &self,
        request: &MessagesRequest,
        streaming: bool,
    ) -> Result<(reqwest::Response, Vec<String>), ClaudeError> {
        // Reject invalid cache policy before resolving credentials or sending HTTP.
        request.validate_cache_control()?;
        let wire_body = self.request_body(request, streaming)?;
        let mut retried = false;
        // Retain both attempted generations only for this request, so an error
        // gateway cannot reflect an earlier rejected credential into a durable
        // failure receipt. Sensitive request headers alone do not scrub bodies.
        let mut credentials = Vec::<String>::new();
        loop {
            let mut provider_headers = None;
            let mut headers = match &self.auth {
                ClientAuth::ApiKey(key) => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    let mut value = reqwest::header::HeaderValue::from_str(key)
                        .map_err(|_| ClaudeError::AuthUnavailable)?;
                    value.set_sensitive(true);
                    headers.insert("x-api-key", value);
                    headers
                }
                ClientAuth::Headers(headers) => headers.clone(),
                ClientAuth::Provider(provider) => {
                    let headers = provider
                        .headers()
                        .await
                        .map_err(|_| ClaudeError::AuthUnavailable)?;
                    provider_headers = Some(headers.clone());
                    headers
                }
            };
            // Authentication and request features share this comma-separated
            // header. RequestBuilder::headers replaces a pre-existing value,
            // which would silently drop context management for OAuth clients.
            let mut betas = Vec::new();
            for value in headers.get_all("anthropic-beta") {
                for beta in value
                    .to_str()
                    .map_err(|_| ClaudeError::AuthUnavailable)?
                    .split(',')
                    .map(str::trim)
                    .filter(|beta| !beta.is_empty())
                {
                    if !betas.contains(&beta) {
                        betas.push(beta);
                    }
                }
            }
            if self.subscription_compatibility {
                let agent = !request.tools.is_empty() || request.thinking.is_some();
                let defaults: &[&str] = if agent {
                    &[
                        "claude-code-20250219",
                        "oauth-2025-04-20",
                        "interleaved-thinking-2025-05-14",
                        "thinking-token-count-2026-05-13",
                        "context-management-2025-06-27",
                        "prompt-caching-scope-2026-01-05",
                        "mid-conversation-system-2026-04-07",
                    ]
                } else {
                    &[
                        "oauth-2025-04-20",
                        "interleaved-thinking-2025-05-14",
                        "thinking-token-count-2026-05-13",
                        "context-management-2025-06-27",
                        "prompt-caching-scope-2026-01-05",
                        "structured-outputs-2025-12-15",
                    ]
                };
                let extras = std::mem::take(&mut betas);
                betas.extend_from_slice(defaults);
                if agent && request.thinking.is_some() && !betas.contains(&"effort-2025-11-24") {
                    betas.push("effort-2025-11-24");
                }
                if agent && !betas.contains(&"fallback-credit-2026-06-01") {
                    betas.push("fallback-credit-2026-06-01");
                }
                for beta in extras {
                    if !betas.contains(&beta) {
                        betas.push(beta);
                    }
                }
            }
            if request.context_management.is_some()
                && !betas.contains(&"context-management-2025-06-27")
            {
                betas.push("context-management-2025-06-27");
            }
            if request.speed.is_some() && !betas.contains(&FAST_MODE_BETA) {
                betas.push(FAST_MODE_BETA);
            }
            if !betas.is_empty() {
                let value = reqwest::header::HeaderValue::from_str(&betas.join(","))
                    .map_err(|_| ClaudeError::AuthUnavailable)?;
                headers.insert("anthropic-beta", value);
            }
            if self.subscription_compatibility {
                let identity = &self.subscription_identity;
                for (name, value) in [
                    (
                        "user-agent",
                        format!("claude-cli/{} (external, cli)", identity.version()),
                    ),
                    ("x-app", "cli".into()),
                    ("x-stainless-lang", "js".into()),
                    ("x-stainless-runtime", "node".into()),
                    ("x-stainless-runtime-version", "v26.3.0".into()),
                    (
                        "x-stainless-package-version",
                        subscription_wire::CLAUDE_CODE_SDK_VERSION.into(),
                    ),
                    ("x-stainless-retry-count", "0".into()),
                    ("x-stainless-timeout", "600".into()),
                    ("x-stainless-os", identity.os()),
                    ("x-stainless-arch", identity.arch()),
                    (
                        "x-claude-code-session-id",
                        self.subscription_session.clone(),
                    ),
                    ("accept", "application/json".into()),
                    ("accept-encoding", "gzip, deflate, br, zstd".into()),
                    ("connection", "keep-alive".into()),
                    ("anthropic-dangerous-direct-browser-access", "true".into()),
                ] {
                    headers.insert(
                        name,
                        reqwest::header::HeaderValue::from_str(&value).map_err(|_| {
                            ClaudeError::Protocol("invalid subscription wire header".into())
                        })?,
                    );
                }
                headers.remove("x-claude-code-request-class");
            }
            for (name, value) in &headers {
                if name == reqwest::header::AUTHORIZATION
                    || name == "x-api-key"
                    || value.is_sensitive()
                {
                    let value = String::from_utf8_lossy(value.as_bytes());
                    if !value.is_empty() && !credentials.iter().any(|item| item == &value) {
                        credentials.push(value.to_string());
                    }
                    if name == reqwest::header::AUTHORIZATION
                        && let Some((_, token)) = value.split_once(' ')
                        && !token.trim().is_empty()
                        && !credentials.iter().any(|item| item == token.trim())
                    {
                        credentials.push(token.trim().to_owned());
                    }
                }
            }
            let builder = self
                .http
                .post(&self.endpoint)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .headers(headers);
            let response = builder
                .header("content-type", "application/json")
                .body(wire_body.clone())
                .send()
                .await?;
            // Only an explicit HTTP authentication rejection is recovered here.
            // Agent sessions own transient retries because they track published
            // text and possible server effects.
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && !retried
                && let (ClientAuth::Provider(provider), Some(headers)) =
                    (&self.auth, provider_headers.as_ref())
                && provider
                    .recover_unauthorized(headers)
                    .await
                    .map_err(|_| ClaudeError::AuthUnavailable)?
            {
                retried = true;
                continue;
            }
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let retry_after = retry_after(response.headers());
                let mut body = response.text().await?;
                credentials.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
                for credential in &credentials {
                    body = body.replace(credential, "[redacted]");
                }
                return Err(ClaudeError::Http {
                    status,
                    body,
                    retry_after,
                });
            }
            credentials.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
            return Ok((response, credentials));
        }
    }

    pub async fn create(&self, request: &MessagesRequest) -> Result<MessageResponse, ClaudeError> {
        let mut response: MessageResponse = self.post(request, false).await?.0.json().await?;
        for block in &mut response.content {
            self.decode_block(block);
        }
        Ok(response)
    }

    pub async fn stream(&self, request: &MessagesRequest) -> Result<ClaudeStream, ClaudeError> {
        let (response, credentials) = self.post(request, true).await?;
        // A successful non-SSE response (for example an upstream JSON gateway
        // response) must not masquerade as a truncated Messages stream.
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        if !is_sse {
            return Err(ClaudeError::Protocol(
                "expected text/event-stream response".into(),
            ));
        }
        let decode_subscription = self.subscription_compatibility;
        Ok(Box::pin(stream::unfold(
            SseState::new(response, credentials),
            move |mut state| async move {
                if state.done {
                    return None;
                }
                loop {
                    let line = match state.pop_line() {
                        Ok(line) => line,
                        Err(error) => {
                            state.done = true;
                            return Some((Err(redact_error(error, &state.credentials)), state));
                        }
                    };
                    if let Some(line) = line {
                        if line.is_empty() {
                            state.frame_bytes = 0;
                            let event_name = state.event_name.take();
                            if let Some(data) = state.take_data() {
                                let parsed = serde_json::from_str::<Value>(&data)
                                    .map_err(ClaudeError::from)
                                    .and_then(|value| {
                                        if event_name.as_deref().is_some_and(|name| {
                                            value.get("type").and_then(Value::as_str) != Some(name)
                                        }) {
                                            return Err(ClaudeError::Protocol(
                                                "SSE event name disagrees with data type".into(),
                                            ));
                                        }
                                        serde_json::from_value::<StreamEvent>(value)
                                            .map_err(ClaudeError::from)
                                    });
                                let event = match parsed {
                                    Ok(StreamEvent::Error { error }) => {
                                        Err(ClaudeError::StreamError {
                                            kind: error.kind,
                                            message: error.message,
                                        })
                                    }
                                    other => other,
                                };
                                match event {
                                    Ok(StreamEvent::Other | StreamEvent::Ping) => continue,
                                    Ok(StreamEvent::MessageStop) => {
                                        state.done = true;
                                        return Some((Ok(StreamEvent::MessageStop), state));
                                    }
                                    Err(error) => {
                                        state.done = true;
                                        return Some((
                                            Err(redact_error(error, &state.credentials)),
                                            state,
                                        ));
                                    }
                                    Ok(mut event) => {
                                        if decode_subscription {
                                            match &mut event {
                                                StreamEvent::ContentBlockStart {
                                                    content_block:
                                                        ContentBlock::ToolUse { name, .. },
                                                    ..
                                                } => subscription_wire::strip(name),
                                                StreamEvent::MessageStart { message } => {
                                                    for block in &mut message.content {
                                                        if let ContentBlock::ToolUse {
                                                            name, ..
                                                        } = block
                                                        {
                                                            subscription_wire::strip(name);
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        return Some((Ok(event), state));
                                    }
                                }
                            }
                        } else {
                            // SSE removes exactly one ASCII space after the colon,
                            // not arbitrary whitespace. A field without a colon has
                            // an empty value; comments and unknown fields are ignored.
                            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
                            let value = value.strip_prefix(' ').unwrap_or(value);
                            match field {
                                "event" => {
                                    state.event_name = (!value.is_empty()).then(|| value.to_owned())
                                }
                                "data" => state.data.push(value.to_owned()),
                                _ => {}
                            }
                        }
                        continue;
                    }
                    match state.response.next().await {
                        Some(Ok(chunk)) => {
                            state.bytes.extend_from_slice(&chunk);
                        }
                        None => {
                            state.done = true;
                            return Some((Err(ClaudeError::IncompleteStream), state));
                        }
                        Some(Err(error)) => {
                            state.done = true;
                            return Some((Err(ClaudeError::Transport(error)), state));
                        }
                    }
                }
            },
        )))
    }
}

#[cfg(not(target_family = "wasm"))]
pub type ClaudeStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ClaudeError>> + Send>>;
#[cfg(target_family = "wasm")]
pub type ClaudeStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ClaudeError>>>>;

/// Future accepted by a registered tool on the current execution target.
#[doc(hidden)]
#[cfg(not(target_family = "wasm"))]
pub trait ToolFuture: Future + Send {}
#[cfg(not(target_family = "wasm"))]
impl<T: Future + Send> ToolFuture for T {}
/// Future accepted by an isolate-local tool.
#[doc(hidden)]
#[cfg(target_family = "wasm")]
pub trait ToolFuture: Future {}
#[cfg(target_family = "wasm")]
impl<T: Future> ToolFuture for T {}

const MAX_SSE_FRAME_BYTES: usize = 32 * 1024 * 1024;

#[cfg(not(target_family = "wasm"))]
type ResponseBytes = Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>;
#[cfg(target_family = "wasm")]
type ResponseBytes = Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>>>>;

// Scrub diagnostics only, never valid signed or opaque provider content.
fn redact_error(error: ClaudeError, credentials: &[String]) -> ClaudeError {
    let scrub = |mut text: String| {
        for credential in credentials {
            text = text.replace(credential, "[redacted]");
        }
        text
    };
    match error {
        ClaudeError::StreamError { kind, message } => ClaudeError::StreamError {
            kind: scrub(kind),
            message: scrub(message),
        },
        ClaudeError::Protocol(message) => ClaudeError::Protocol(scrub(message)),
        ClaudeError::Json(error) => {
            let original = error.to_string();
            let redacted = scrub(original.clone());
            if redacted == original {
                ClaudeError::Json(error)
            } else {
                ClaudeError::Protocol(format!("invalid Messages JSON: {redacted}"))
            }
        }
        other => other,
    }
}

struct SseState {
    credentials: Vec<String>,
    response: ResponseBytes,
    bytes: Vec<u8>,
    data: Vec<String>,
    done: bool,
    frame_bytes: usize,
    event_name: Option<String>,
    first_line: bool,
    skip_lf: bool,
    scan_start: usize,
}

impl SseState {
    fn new(response: reqwest::Response, credentials: Vec<String>) -> Self {
        Self {
            credentials,
            response: Box::pin(
                response
                    .bytes_stream()
                    .map(|chunk| chunk.map(|bytes| bytes.to_vec())),
            ),
            bytes: Vec::new(),
            data: Vec::new(),
            done: false,
            frame_bytes: 0,
            event_name: None,
            first_line: true,
            skip_lf: false,
            scan_start: 0,
        }
    }

    fn pop_line(&mut self) -> Result<Option<String>, ClaudeError> {
        // SSE permits LF, CRLF and bare CR. Consume a CR immediately and skip
        // only its optional following LF, even when those bytes arrive in
        // different HTTP chunks. Waiting for LF loses a valid CR-only terminal.
        if self.skip_lf {
            if self.bytes.is_empty() {
                return Ok(None);
            }
            if self.bytes[0] == b'\n' {
                self.bytes.remove(0);
            }
            self.skip_lf = false;
        }
        let delimiter = self.bytes[self.scan_start..]
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
            .map(|offset| self.scan_start + offset);
        let pending = delimiter.map_or(self.bytes.len(), |index| index + 1);
        if pending + self.frame_bytes > MAX_SSE_FRAME_BYTES {
            return Err(ClaudeError::Protocol("SSE frame exceeds 32 MiB".into()));
        }
        let Some(index) = delimiter else {
            // Do not repeatedly scan the entire unfinished line for each chunk.
            self.scan_start = self.bytes.len();
            return Ok(None);
        };
        self.frame_bytes += index + 1;
        self.skip_lf = self.bytes[index] == b'\r';
        self.scan_start = 0;
        let mut line = self.bytes.drain(..=index).collect::<Vec<_>>();
        line.pop();
        // SSE is UTF-8; malformed lines must not be silently altered. Only one
        // leading BOM is optional, at the start of the stream, not every frame.
        let mut line = String::from_utf8(line)
            .map_err(|_| ClaudeError::Protocol("invalid UTF-8 SSE line".into()))?;
        if self.first_line {
            self.first_line = false;
            if line.starts_with('\u{feff}') {
                line.drain(..'\u{feff}'.len_utf8());
            }
        }
        Ok(Some(line))
    }

    fn take_data(&mut self) -> Option<String> {
        if self.data.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.data).join("\n"))
        }
    }
}

#[derive(Debug)]
enum BlockAccumulator {
    Text {
        text: String,
        extra: BTreeMap<String, Value>,
    },
    ToolUse {
        id: String,
        name: String,
        initial: Value,
        fragments: String,
        extra: BTreeMap<String, Value>,
    },
    Thinking {
        thinking: String,
        signature: String,
        extra: BTreeMap<String, Value>,
    },
    ServerToolUse {
        id: String,
        name: String,
        initial: Value,
        fragments: String,
        extra: BTreeMap<String, Value>,
    },
    McpToolUse {
        id: String,
        name: String,
        server_name: String,
        initial: Value,
        fragments: String,
        extra: BTreeMap<String, Value>,
    },
    Other(ContentBlock),
}

impl BlockAccumulator {
    // Diagnostics must never format payloads or provider-controlled type strings.
    fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::ToolUse { .. } => "tool_use",
            Self::Thinking { .. } => "thinking",
            Self::ServerToolUse { .. } => "server_tool_use",
            Self::McpToolUse { .. } => "mcp_tool_use",
            Self::Other(block) => match block {
                ContentBlock::Image { .. } => "image",
                ContentBlock::Document { .. } => "document",
                ContentBlock::Text { .. } => "text",
                ContentBlock::ToolUse { .. } => "tool_use",
                ContentBlock::ServerToolUse { .. } => "server_tool_use",
                ContentBlock::WebSearchToolResult { .. } => "web_search_tool_result",
                ContentBlock::WebFetchToolResult { .. } => "web_fetch_tool_result",
                ContentBlock::ToolSearchToolResult { .. } => "tool_search_tool_result",
                ContentBlock::CodeExecutionToolResult { .. } => "code_execution_tool_result",
                ContentBlock::BashCodeExecutionToolResult { .. } => {
                    "bash_code_execution_tool_result"
                }
                ContentBlock::TextEditorCodeExecutionToolResult { .. } => {
                    "text_editor_code_execution_tool_result"
                }
                ContentBlock::McpToolUse { .. } => "mcp_tool_use",
                ContentBlock::McpToolResult { .. } => "mcp_tool_result",
                ContentBlock::McpToolListing { .. } => "mcp_tool_listing",
                ContentBlock::ToolResult { .. } => "tool_result",
                ContentBlock::Thinking { .. } => "thinking",
                ContentBlock::RedactedThinking { .. } => "redacted_thinking",
            },
        }
    }

    fn finish(self, truncated: bool) -> Result<ContentBlock, ClaudeError> {
        let block = match self {
            Self::Text { text, extra } => ContentBlock::Text { text, extra },
            Self::ToolUse {
                id,
                name,
                initial,
                fragments,
                extra,
            } => {
                let input = if fragments.is_empty() {
                    Ok(initial)
                } else {
                    serde_json::from_str::<Value>(&fragments)
                };
                let input = match input {
                    Ok(input) if input.is_object() => input,
                    _ if truncated => {
                        return Ok(ContentBlock::text(format!(
                            "Incomplete tool input for {name} ({id}) was not executed because the output token limit was reached. Issue a fresh complete call if needed."
                        )));
                    }
                    Err(error) => return Err(error.into()),
                    _ => {
                        return Err(ClaudeError::Protocol(
                            "tool input must be a JSON object".into(),
                        ));
                    }
                };
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    extra,
                }
            }
            Self::ServerToolUse {
                id,
                name,
                initial,
                fragments,
                extra,
            } => {
                let input = if fragments.is_empty() {
                    initial
                } else {
                    serde_json::from_str(&fragments)?
                };
                if !input.is_object() {
                    return Err(ClaudeError::Protocol(
                        "server tool input must be a JSON object".into(),
                    ));
                }
                ContentBlock::ServerToolUse {
                    id,
                    name,
                    input,
                    extra,
                }
            }
            Self::Thinking {
                thinking,
                signature,
                extra,
            } => ContentBlock::Thinking {
                thinking,
                signature,
                extra,
            },
            Self::McpToolUse {
                id,
                name,
                server_name,
                initial,
                fragments,
                extra,
            } => {
                let input = if fragments.is_empty() {
                    initial
                } else {
                    serde_json::from_str(&fragments)?
                };
                if !input.is_object() {
                    return Err(ClaudeError::Protocol(
                        "MCP tool input must be a JSON object".into(),
                    ));
                }
                ContentBlock::McpToolUse {
                    id,
                    name,
                    server_name,
                    input,
                    extra,
                }
            }
            Self::Other(block) => block,
        };
        Ok(block)
    }
}

/// Assemble streaming deltas into the same typed response as `create`.
/// A partial tool JSON object or a missing terminal event is never returned as
/// a usable tool call.
pub async fn collect_stream<S>(
    first: StreamEvent,
    mut events: S,
) -> Result<MessageResponse, ClaudeError>
where
    S: Stream<Item = Result<StreamEvent, ClaudeError>> + Unpin,
{
    let StreamEvent::MessageStart { mut message } = first else {
        return Err(ClaudeError::Protocol("expected message_start".into()));
    };
    let mut active = BTreeMap::<usize, BlockAccumulator>::new();
    let mut completed = BTreeMap::<usize, BlockAccumulator>::new();
    while let Some(event) = events.next().await {
        match event? {
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                if active.contains_key(&index) || completed.contains_key(&index) {
                    return Err(ClaudeError::Protocol(format!(
                        "duplicate content block {index}"
                    )));
                }
                let block = match content_block {
                    ContentBlock::Text { text, extra } => BlockAccumulator::Text { text, extra },
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        extra,
                    } => BlockAccumulator::ToolUse {
                        id,
                        name,
                        initial: input,
                        fragments: String::new(),
                        extra,
                    },
                    ContentBlock::ServerToolUse {
                        id,
                        name,
                        input,
                        extra,
                    } => BlockAccumulator::ServerToolUse {
                        id,
                        name,
                        initial: input,
                        fragments: String::new(),
                        extra,
                    },
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                        extra,
                    } => BlockAccumulator::Thinking {
                        thinking,
                        signature,
                        extra,
                    },
                    ContentBlock::McpToolUse {
                        id,
                        name,
                        server_name,
                        input,
                        extra,
                    } => BlockAccumulator::McpToolUse {
                        id,
                        name,
                        server_name,
                        initial: input,
                        fragments: String::new(),
                        extra,
                    },
                    other => BlockAccumulator::Other(other),
                };
                active.insert(index, block);
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                match (active.get_mut(&index), delta) {
                    (
                        Some(BlockAccumulator::Text { text, .. }),
                        ContentDelta::TextDelta { text: chunk },
                    ) => text.push_str(&chunk),
                    (
                        Some(BlockAccumulator::Text { extra, .. }),
                        ContentDelta::CitationsDelta { citation },
                    ) => match extra
                        .entry("citations".to_owned())
                        .or_insert_with(|| Value::Array(Vec::new()))
                    {
                        Value::Array(citations) => citations.push(citation),
                        _ => {
                            return Err(ClaudeError::Protocol(
                                "text citations must be an array".into(),
                            ));
                        }
                    },
                    (
                        Some(
                            BlockAccumulator::ToolUse { fragments, .. }
                            | BlockAccumulator::ServerToolUse { fragments, .. }
                            | BlockAccumulator::McpToolUse { fragments, .. },
                        ),
                        ContentDelta::InputJsonDelta { partial_json },
                    ) => fragments.push_str(&partial_json),
                    (
                        Some(BlockAccumulator::Thinking { thinking, .. }),
                        ContentDelta::ThinkingDelta { thinking: chunk },
                    ) => thinking.push_str(&chunk),
                    (
                        Some(BlockAccumulator::Thinking { signature, .. }),
                        ContentDelta::SignatureDelta { signature: chunk },
                    ) => signature.push_str(&chunk),
                    (Some(_), ContentDelta::Other) => {
                        return Err(ClaudeError::Protocol(format!(
                            "unsupported delta for content block {index}"
                        )));
                    }
                    _ => {
                        return Err(ClaudeError::Protocol(format!(
                            "unexpected delta for content block {index}"
                        )));
                    }
                }
            }
            StreamEvent::ContentBlockStop { index } => {
                let block = active.remove(&index).ok_or_else(|| {
                    ClaudeError::Protocol(format!("unknown content block {index}"))
                })?;
                completed.insert(index, block);
            }
            StreamEvent::MessageDelta { delta, usage } => {
                if let Some(container) = delta.container {
                    message.container = Some(container);
                }
                if let Some(reason) = delta.stop_reason {
                    message.stop_reason = Some(reason);
                }
                if let Some(input) = usage.input_tokens {
                    message.usage.input_tokens = input;
                }
                if let Some(cached) = usage.cache_read_input_tokens {
                    message.usage.cache_read_input_tokens = cached;
                }
                if let Some(created) = usage.cache_creation_input_tokens {
                    message.usage.cache_creation_input_tokens = created;
                }
                if let Some(output) = usage.output_tokens {
                    message.usage.output_tokens = output;
                }
            }
            StreamEvent::MessageStop => {
                if message.stop_reason.is_none() {
                    return Err(ClaudeError::Protocol("missing final stop_reason".into()));
                }
                let truncated = message.stop_reason == Some(StopReason::MaxTokens);
                for (index, block) in active {
                    // Only an explicit output cutoff explains an open text/client
                    // block. Never synthesize a signature or a server-tool receipt.
                    let block = match block {
                        BlockAccumulator::Text { .. } if truncated => block,
                        BlockAccumulator::ToolUse { id, name, .. } if truncated => {
                            BlockAccumulator::Other(ContentBlock::text(format!(
                                "Incomplete tool input for {name} ({id}) was not executed because the output token limit was reached. Issue a fresh complete call if needed."
                            )))
                        }
                        _ => {
                            return Err(ClaudeError::Protocol(format!(
                                "message_stop before content_block_stop: stop_reason={}, block_kind={}, block_index={index}",
                                match message.stop_reason {
                                    Some(StopReason::EndTurn) => "end_turn",
                                    Some(StopReason::MaxTokens) => "max_tokens",
                                    Some(StopReason::StopSequence) => "stop_sequence",
                                    Some(StopReason::ToolUse) => "tool_use",
                                    Some(StopReason::PauseTurn) => "pause_turn",
                                    Some(StopReason::Refusal) => "refusal",
                                    Some(StopReason::ModelContextWindowExceeded) =>
                                        "model_context_window_exceeded",
                                    Some(StopReason::Unknown) => "unknown",
                                    None => "missing",
                                },
                                block.diagnostic_kind(),
                            )));
                        }
                    };
                    completed.insert(index, block);
                }
                let count = completed.len();
                if completed.keys().copied().ne(0..count) {
                    return Err(ClaudeError::Protocol(
                        "non-contiguous content block indices".into(),
                    ));
                }
                message.content = completed
                    .into_values()
                    .map(|block| block.finish(truncated))
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok(message);
            }
            StreamEvent::Error { error } => {
                return Err(ClaudeError::StreamError {
                    kind: error.kind,
                    message: error.message,
                });
            }
            StreamEvent::MessageStart { .. } => {
                return Err(ClaudeError::Protocol("duplicate message_start".into()));
            }
            StreamEvent::Ping | StreamEvent::Other => {}
        }
    }
    Err(ClaudeError::IncompleteStream)
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompactedHistory {
    pub messages: Vec<Message>,
    pub summary: String,
    pub dropped_messages: usize,
}

impl CompactedHistory {
    /// Caller-supplied summary, appended to the normal system prompt. This
    /// helper intentionally does not summarize via a model or guess tokens.
    pub fn system_context(&self, system: &str) -> String {
        if self.summary.is_empty() {
            return system.to_owned();
        }
        if system.is_empty() {
            return format!("Conversation summary:\n{}", self.summary);
        }
        format!("{system}\n\nConversation summary:\n{}", self.summary)
    }
}

/// Retain recent messages without severing an assistant tool use and its user
/// tool result. Rewind to the beginning of the containing user turn. Truncating
/// history or supplying a summary invalidates retained thinking's prefix binding,
/// so remove that thinking and any messages left empty by its removal.
pub fn compact_history(
    history: &[Message],
    keep_recent: usize,
    summary: impl Into<String>,
) -> CompactedHistory {
    let mut start = history.len().saturating_sub(keep_recent);
    if start < history.len() && start > 0 {
        while start > 0 && !is_user_turn_start(&history[start]) {
            start -= 1;
        }
    }
    let summary = summary.into();
    let mut messages = history[start..].to_vec();
    if start > 0 || !summary.is_empty() {
        strip_thinking(&mut messages);
    }
    CompactedHistory {
        dropped_messages: history.len() - messages.len(),
        messages,
        summary,
    }
}

fn strip_thinking(messages: &mut Vec<Message>) {
    messages.retain_mut(|message| {
        message.content.retain(|block| {
            !matches!(
                block,
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
            )
        });
        !message.content.is_empty()
    });
}

fn is_user_turn_start(message: &Message) -> bool {
    message.role == Role::User
        && !message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

mod agent;
pub use agent::{
    Claude, ClaudeBuilder, ClaudeToolInvocation, ClaudeToolReply, ClaudeTools, rewind_checkpoint,
};

/// Portable durability integration with provider-native state.
pub mod execution;

#[cfg(test)]
mod sse_framing_tests {
    use super::*;

    fn state() -> SseState {
        SseState {
            credentials: Vec::new(),
            response: Box::pin(stream::empty()),
            bytes: Vec::new(),
            data: Vec::new(),
            done: false,
            frame_bytes: 0,
            event_name: None,
            first_line: true,
            skip_lf: false,
            scan_start: 0,
        }
    }

    #[test]
    fn every_byte_boundary_preserves_crlf_bom_and_unicode() {
        for ending in ["\n", "\r\n", "\r"] {
            let input =
                format!("\u{feff}data: 日本語 😀{ending}{ending}data: done{ending}{ending}");
            let mut state = state();
            let mut lines = Vec::new();
            for byte in input.bytes() {
                state.bytes.push(byte);
                while let Some(line) = state.pop_line().unwrap() {
                    if line.is_empty() {
                        state.frame_bytes = 0;
                    }
                    lines.push(line);
                }
            }
            assert_eq!(lines, ["data: 日本語 😀", "", "data: done", ""]);
            assert!(state.bytes.is_empty());
        }
    }

    #[test]
    fn coalesced_small_frames_are_not_one_oversize_frame() {
        let mut state = state();
        // This whole HTTP chunk is larger than a single allowed SSE frame.
        let frame = format!(": {}\n\n", "x".repeat(1024 * 1024));
        state.bytes = frame.repeat(33).into_bytes();
        let mut frames = 0;
        while let Some(line) = state.pop_line().unwrap() {
            if line.is_empty() {
                state.frame_bytes = 0;
                frames += 1;
            }
        }
        assert_eq!(frames, 33);
    }

    #[test]
    fn unfinished_line_still_has_a_byte_bound() {
        let mut state = state();
        state.bytes = vec![b'x'; MAX_SSE_FRAME_BYTES + 1];
        assert!(matches!(state.pop_line(), Err(ClaudeError::Protocol(_))));
    }

    #[test]
    fn invalid_utf8_is_not_replaced_or_accepted() {
        let mut state = state();
        state.bytes = vec![b'd', b'a', b't', b'a', b':', 0xff, b'\n'];
        assert!(matches!(state.pop_line(), Err(ClaudeError::Protocol(_))));
    }
}

#[cfg(test)]
mod cache_tail_profile_tests {
    use super::*;
    use serde_json::json;

    fn cached_request() -> MessagesRequest {
        MessagesRequest {
            model: "test".into(),
            max_tokens: 16,
            cache_control: Some(CacheControl {
                kind: CacheType::Ephemeral,
                ttl: Some(CacheTtl::OneHour),
            }),
            output_config: None,
            speed: None,
            tool_choice: None,
            thinking: None,
            context_management: None,
            diagnostics: None,
            system: Some(Value::String("stable".into())),
            messages: vec![Message::text(Role::User, "hello")],
            container: None,
            tools: vec![],
        }
    }

    // A cursor admitted before the conversion lacks `cache_tail` and must keep
    // its exact durable bytes; durable steps fail closed on changed input.
    #[test]
    fn legacy_cursor_keeps_top_level_and_new_cursor_uses_explicit_tail() {
        let client = ClaudeClient::new(reqwest::Client::new(), "http://127.0.0.1/v1/messages", "x")
            .subscription_compatibility()
            .bind_subscription_session("session");
        let fresh = client.freeze_wire_profile(true);
        let encoded = serde_json::to_value(&fresh).unwrap();
        assert_eq!(encoded["cache_tail"], true);
        let mut legacy = encoded;
        legacy.as_object_mut().unwrap().remove("cache_tail");
        let legacy: FrozenWireProfile = serde_json::from_value(legacy).unwrap();
        // Profiles without automatic caching serialize exactly as before.
        let uncached = serde_json::to_value(client.freeze_wire_profile(false)).unwrap();
        assert!(uncached.get("cache_tail").is_none());

        let request = cached_request();
        let wire = |profile: &FrozenWireProfile| -> Value {
            serde_json::from_str(
                &client
                    .restore_wire_profile(Some(profile))
                    .request_body(&request, true)
                    .unwrap(),
            )
            .unwrap()
        };
        let old = wire(&legacy);
        assert_eq!(old["cache_control"], json!({"type":"ephemeral","ttl":"1h"}));
        assert!(
            old["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        let new = wire(&fresh);
        assert!(new.get("cache_control").is_none());
        assert_eq!(
            new["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral","ttl":"1h"})
        );
        // Durable identity is the final wire body, so it is deterministic.
        let restored = client.restore_wire_profile(Some(&fresh));
        assert_eq!(
            restored.durable_request(&request).unwrap(),
            restored.durable_request(&request).unwrap()
        );
    }
}
