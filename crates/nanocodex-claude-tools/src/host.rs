//! Opt-in Claude orchestration adapters for embedding-owned capabilities.
//!
//! These adapters validate a bounded subset of Claude Code inputs. They never
//! create a child, answer a question, grant plan approval, switch directories,
//! or synthesize a task receipt. The host owns those effects and their durable
//! lifecycle. See <https://code.claude.com/docs/en/tools-reference>.
//!
//! Register only [`ClaudeHostTools::definitions`] and dispatch each call with
//! its real [`HostContext`]. A host must persist effect admission and results
//! keyed by session, turn and call identity, authorize resume/task IDs, and
//! reconcile cancellation or restart before accepting another effect. This
//! module has no default host and does not provide full Claude Code parity.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Invocation identity and capture budget supplied by the Claude caller.
/// This is not a Responses context: it carries no provider wire history.
#[derive(Clone, Copy, Debug)]
pub struct HostContext<'a> {
    model: &'a str,
    session_id: &'a str,
    turn_id: Option<&'a str>,
    call_id: &'a str,
    max_output_tokens: usize,
}
impl<'a> HostContext<'a> {
    /// Construct from the actual invocation; the host must enforce capture bounds.
    #[must_use]
    pub const fn new(
        model: &'a str,
        session_id: &'a str,
        call_id: &'a str,
        max_output_tokens: usize,
    ) -> Self {
        Self {
            model,
            session_id,
            turn_id: None,
            call_id,
            max_output_tokens,
        }
    }
    /// Attach the actual turn identity before dispatch.
    #[must_use]
    pub const fn with_turn_id(mut self, turn_id: Option<&'a str>) -> Self {
        self.turn_id = turn_id;
        self
    }
    /// Current Claude model.
    #[must_use]
    pub const fn model(self) -> &'a str {
        self.model
    }
    /// Host session identity.
    #[must_use]
    pub const fn session_id(self) -> &'a str {
        self.session_id
    }
    /// Host turn identity, required for orchestration effects.
    #[must_use]
    pub const fn turn_id(self) -> Option<&'a str> {
        self.turn_id
    }
    /// Admitted tool call identity.
    #[must_use]
    pub const fn call_id(self) -> &'a str {
        self.call_id
    }
    /// Host-enforced output token budget, not permission to buffer unbounded data.
    #[must_use]
    pub const fn max_output_tokens(self) -> usize {
        self.max_output_tokens
    }
}

/// Claude image source, without OpenAI image URLs/detail wire fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64 image bytes; the caller validates supported image MIME types.
    Base64 { media_type: String, data: String },
    /// Public HTTPS image authorized by the host.
    Url { url: String },
}
/// Native result blocks. Unsupported media remains explicit, never silently lost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultBlock {
    /// Claude-native text block.
    Text { text: String },
    /// Claude-native image block.
    Image { source: ImageSource },
    /// Media not representable by the current Claude client result adapter.
    UnsupportedMedia { media_type: String },
}
/// A result body is text or Claude-native blocks, never Responses input_* items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolContent {
    /// Plain text result.
    Text(String),
    /// Ordered native content blocks.
    Blocks(Vec<ToolResultBlock>),
}
/// Caller-owned result with native error semantics and separate event data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Native text/image result body.
    pub content: ToolContent,
    /// True for an actual failed host/remote result.
    pub is_error: bool,
    /// Structured result preserved on the tool event, not invented wire content.
    pub structured_result: Option<Value>,
    /// Host metadata preserved on the tool event.
    pub metadata: Option<Value>,
}
impl ToolOutput {
    /// Successful text result, with the text retained as structured data.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            structured_result: Some(Value::String(text.clone())),
            content: ToolContent::Text(text),
            is_error: false,
            metadata: None,
        }
    }
    /// Failed text result; this remains a failure even when returned through Ok.
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text)
        }
    }
    /// Successful ordered native blocks. Hosts attach structured data explicitly.
    #[must_use]
    pub const fn content(blocks: Vec<ToolResultBlock>) -> Self {
        Self {
            content: ToolContent::Blocks(blocks),
            is_error: false,
            structured_result: None,
            metadata: None,
        }
    }
    /// Attach exact structured data, independently of the text/image wire body.
    #[must_use]
    pub fn with_structured_result(mut self, result: Value) -> Self {
        self.structured_result = Some(result);
        self
    }
    /// Attach exact host metadata without raw JSON or provider DTO conversion.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Maximum serialized arguments admitted at this boundary.
pub const MAX_HOST_INPUT_BYTES: usize = 64 * 1024;

/// Capabilities explicitly installed by an embedding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostTool {
    /// Start or resume a host-owned child agent.
    Agent,
    /// Observe or wait for a host-owned background task.
    TaskOutput,
    /// Stop a host-owned background task.
    TaskStop,
    /// Await structured user answers in the host UI.
    AskUserQuestion,
    /// Enter the host's restricted planning mode.
    EnterPlanMode,
    /// Request the host's approval to leave planning mode.
    ExitPlanMode,
    /// Enter an authorized isolated worktree.
    EnterWorktree,
    /// Leave a host-owned worktree.
    ExitWorktree,
}
impl HostTool {
    /// All adapter capabilities; listing one never supplies its implementation.
    pub const ALL: [Self; 8] = [
        Self::Agent,
        Self::TaskOutput,
        Self::TaskStop,
        Self::AskUserQuestion,
        Self::EnterPlanMode,
        Self::ExitPlanMode,
        Self::EnterWorktree,
        Self::ExitWorktree,
    ];

    /// Claude-visible tool name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::TaskOutput => "TaskOutput",
            Self::TaskStop => "TaskStop",
            Self::AskUserQuestion => "AskUserQuestion",
            Self::EnterPlanMode => "EnterPlanMode",
            Self::ExitPlanMode => "ExitPlanMode",
            Self::EnterWorktree => "EnterWorktree",
            Self::ExitWorktree => "ExitWorktree",
        }
    }
}

/// Validated requests passed to the actual host lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostRequest {
    /// Start/resume a child. Background requests require output and stop capabilities.
    Agent(AgentRequest),
    /// Retrieve actual task output/status, optionally waiting.
    TaskOutput(TaskOutputRequest),
    /// Stop an actual host task; success must mean the host observed the stop.
    TaskStop(TaskStopRequest),
    /// Await a user response without flattening options or selecting defaults.
    AskUserQuestion(QuestionsRequest),
    /// Enforce host planning restrictions before reporting entry.
    EnterPlanMode,
    /// Await real approval; requested permissions do not grant themselves.
    ExitPlanMode(ExitPlanRequest),
    /// Create/enter and update host execution context before reporting success.
    EnterWorktree(EnterWorktreeRequest),
    /// Return to the original execution context; authorize removals separately.
    ExitWorktree(ExitWorktreeRequest),
}

/// Actual embedding capability, with no permissive or synthetic default.
///
/// Implementations must enforce authorization, input-dependent capability
/// support, capture limits, child/task ownership, and cancellation. In
/// particular, unknown subagent types/models/isolation modes must fail before
/// spawning; questions must remain pending until answered or cancelled; plan
/// approval must come from the user/host policy, never model-supplied fields.
/// The host should reuse its existing agent, task, approval and workspace
/// services. Returning an acknowledgement is not proof an effect completed.
pub trait ClaudeHost: Send + Sync {
    /// Execute once through the host's admitted lifecycle and preserve actual
    /// result/error/media using this crate's Claude-native output contract.
    fn execute(
        &self,
        request: HostRequest,
        context: HostContext<'_>,
    ) -> impl std::future::Future<Output = Result<ToolOutput, String>> + Send;
}

/// Validated Claude inputs backed by an explicitly installed host.
pub struct ClaudeHostTools<H: ClaudeHost> {
    host: H,
    enabled: BTreeSet<HostTool>,
}
impl<H: ClaudeHost> ClaudeHostTools<H> {
    /// Install only capabilities the host actually implements and authorizes.
    pub fn new(host: H, enabled: impl IntoIterator<Item = HostTool>) -> Self {
        Self {
            host,
            enabled: enabled.into_iter().collect(),
        }
    }

    /// Definitions for this host's explicit capability subset. No native Codex
    /// tool definitions are loaded. Unsupported fields fail during execution.
    #[must_use]
    pub fn definitions(&self) -> Vec<Value> {
        self.enabled.iter().map(|tool| definition(*tool)).collect()
    }

    /// Validate all fields before host admission and forward the real context.
    /// No retries or lifecycle receipts are generated by this adapter.
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        context: HostContext<'_>,
    ) -> Result<ToolOutput, String> {
        let tool = self
            .enabled
            .iter()
            .find(|tool| tool.name() == name)
            .copied()
            .ok_or_else(|| format!("Claude host tool is unavailable: {name}"))?;
        if context.session_id().is_empty()
            || context.call_id().is_empty()
            || context.turn_id().is_none_or(str::is_empty)
        {
            return Err("host tools require real session, turn and call identities".into());
        }
        if serde_json::to_vec(&input).map_err(|e| e.to_string())?.len() > MAX_HOST_INPUT_BYTES {
            return Err("host tool input exceeds 64 KiB".into());
        }
        let request = match tool {
            HostTool::Agent => {
                let request: AgentRequest = decode(input)?;
                text(&request.prompt, "prompt", 48 * 1024)?;
                text(&request.description, "description", 1024)?;
                text(&request.subagent_type, "subagent_type", 256)?;
                if let Some(resume) = &request.resume {
                    text(resume, "resume", 256)?;
                }
                if request.max_turns.is_some_and(|turns| turns == 0) {
                    return Err("max_turns must be positive".into());
                }
                if request.run_in_background
                    && !(self.enabled.contains(&HostTool::TaskOutput)
                        && self.enabled.contains(&HostTool::TaskStop))
                {
                    return Err(
                        "background Agent requires installed TaskOutput and TaskStop capabilities"
                            .into(),
                    );
                }
                HostRequest::Agent(request)
            }
            HostTool::TaskOutput => {
                let request: TaskOutputRequest = decode(input)?;
                text(&request.task_id, "task_id", 256)?;
                if request.timeout > 600_000 {
                    return Err("timeout must be 0..600000 milliseconds".into());
                }
                HostRequest::TaskOutput(request)
            }
            HostTool::TaskStop => {
                let request: TaskStopRequest = decode(input)?;
                text(&request.task_id, "task_id", 256)?;
                HostRequest::TaskStop(request)
            }
            HostTool::AskUserQuestion => {
                let request: QuestionsRequest = decode(input)?;
                if !(1..=4).contains(&request.questions.len()) {
                    return Err("questions requires 1..4 entries".into());
                }
                let mut questions = BTreeSet::new();
                for question in &request.questions {
                    text(&question.question, "question", 8192)?;
                    text(&question.header, "header", 48)?;
                    if question.header.chars().count() > 12 {
                        return Err("header must contain at most 12 characters".into());
                    }
                    if !questions.insert(&question.question) {
                        return Err("duplicate question".into());
                    }
                    if !(2..=4).contains(&question.options.len()) {
                        return Err("question requires 2..4 options".into());
                    }
                    let mut labels = BTreeSet::new();
                    for option in &question.options {
                        text(&option.label, "label", 1024)?;
                        text(&option.description, "description", 8192)?;
                        if let Some(markdown) = &option.markdown {
                            text(markdown, "markdown", 8192)?;
                        }
                        if !labels.insert(&option.label) {
                            return Err("duplicate option label".into());
                        }
                    }
                }
                HostRequest::AskUserQuestion(request)
            }
            HostTool::EnterPlanMode => {
                let _: EmptyRequest = decode(input)?;
                HostRequest::EnterPlanMode
            }
            HostTool::ExitPlanMode => {
                let request: ExitPlanRequest = decode(input)?;
                if request.allowed_prompts.len() > 32 {
                    return Err("too many allowedPrompts".into());
                }
                for prompt in &request.allowed_prompts {
                    text(&prompt.prompt, "prompt", 8192)?;
                }
                HostRequest::ExitPlanMode(request)
            }
            HostTool::EnterWorktree => {
                let request: EnterWorktreeRequest = decode(input)?;
                if request.name.is_some() && request.path.is_some() {
                    return Err("name and path are mutually exclusive".into());
                }
                if let Some(name) = &request.name {
                    text(name, "name", 256)?;
                }
                if let Some(path) = &request.path {
                    text(path, "path", 4096)?;
                }
                HostRequest::EnterWorktree(request)
            }
            HostTool::ExitWorktree => HostRequest::ExitWorktree(decode(input)?),
        };
        self.host.execute(request, context).await
    }
}
fn decode<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, String> {
    serde_json::from_value(input).map_err(|e| format!("invalid Claude host input: {e}"))
}
fn text(value: &str, field: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(format!(
            "{field} must be nonblank, NUL-free and at most {max} bytes"
        ));
    }
    Ok(())
}
fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("JSON schema serialization")
}
fn definition(tool: HostTool) -> Value {
    let (description, input_schema) = match tool {
        HostTool::Agent => (
            "Start or resume an authorized host-owned agent; return its actual result or background task identity.",
            schema::<AgentRequest>(),
        ),
        HostTool::TaskOutput => (
            "Read a host-owned background task's status and output, optionally waiting up to timeout milliseconds.",
            schema::<TaskOutputRequest>(),
        ),
        HostTool::TaskStop => (
            "Stop an authorized host-owned background task and report the host's observed result.",
            schema::<TaskStopRequest>(),
        ),
        HostTool::AskUserQuestion => (
            "Present structured questions and await the user's answers through the host UI.",
            schema::<QuestionsRequest>(),
        ),
        HostTool::EnterPlanMode => (
            "Enter host-enforced planning mode.",
            schema::<EmptyRequest>(),
        ),
        HostTool::ExitPlanMode => (
            "Present the host's current plan for approval; requested prompts are not grants.",
            schema::<ExitPlanRequest>(),
        ),
        HostTool::EnterWorktree => (
            "Create or enter an authorized git worktree and switch the host execution context.",
            schema::<EnterWorktreeRequest>(),
        ),
        HostTool::ExitWorktree => (
            "Leave the current host worktree, keeping it or requesting authorized removal.",
            schema::<ExitWorktreeRequest>(),
        ),
    };
    json!({"name":tool.name(), "description":description, "input_schema":input_schema})
}

/// Supported Agent request subset. Team/bypass/fork options are rejected.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRequest {
    /// Complete task instructions, preserved as text.
    pub prompt: String,
    /// Short purpose shown by the host.
    pub description: String,
    /// A host-configured agent type; the host validates availability.
    pub subagent_type: String,
    /// Optional model choice; the host must reject unavailable models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
    /// Existing authorized host agent ID to resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// Return a real task identity while execution continues in the host.
    #[serde(default)]
    pub run_in_background: bool,
    /// Optional positive turn limit enforced by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Request isolated worktree execution; never silently ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<AgentIsolation>,
}
/// Recognized Claude model selectors; availability belongs to the host.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentModel {
    /// Host-configured Sonnet model.
    Sonnet,
    /// Host-configured Opus model.
    Opus,
    /// Host-configured Haiku model.
    Haiku,
}
/// Supported child isolation request.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentIsolation {
    /// A separate host-authorized worktree.
    Worktree,
}
/// Background output request; timeout bounds waiting, not the task lifetime.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskOutputRequest {
    /// Host task identity, scoped and authorized by the host.
    pub task_id: String,
    /// Wait for completion if true; poll if false.
    #[serde(default = "default_block")]
    pub block: bool,
    /// Maximum wait in milliseconds, from zero through 600000.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}
const fn default_block() -> bool {
    true
}
const fn default_timeout() -> u64 {
    30_000
}
/// Stop a task by its actual host identity.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskStopRequest {
    /// Task/agent identity accepted by the host; never a local checklist ID.
    pub task_id: String,
}
/// Questions displayed together, with no model-supplied answers accepted.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionsRequest {
    /// One to four distinct questions.
    pub questions: Vec<UserQuestion>,
}
/// A structured host UI question.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserQuestion {
    /// Full question text.
    pub question: String,
    /// Short UI label, at most twelve characters.
    pub header: String,
    /// Two to four distinct answer choices.
    pub options: Vec<QuestionOption>,
    /// Whether the user may select multiple options.
    #[serde(rename = "multiSelect")]
    pub multi_select: bool,
}
/// A question choice, retained without flattening.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    /// Displayed choice label.
    pub label: String,
    /// Explanation of the choice.
    pub description: String,
    /// Optional preview content, untrusted UI data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyRequest {}
/// Exit planning with permissions requested for host review, not preapproved.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExitPlanRequest {
    /// Requested actions to present with the host's stored plan.
    #[serde(default, rename = "allowedPrompts")]
    pub allowed_prompts: Vec<PlanPrompt>,
}
/// One action whose permission the host must decide.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlanPrompt {
    /// Currently supported permission-request target.
    pub tool: PlanTool,
    /// Natural-language description of the requested action.
    pub prompt: String,
}
/// Supported tool named in a plan permission request.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum PlanTool {
    /// Shell action; this adapter never executes the prompt as a command.
    Bash,
}
/// Create a worktree or enter an existing path; authorization is host-owned.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnterWorktreeRequest {
    /// Optional new worktree name; mutually exclusive with path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional existing worktree location, verified by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
/// Leave a host-owned worktree; a remove request is not removal approval.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExitWorktreeRequest {
    /// Explicit preservation/removal choice; no destructive default.
    pub action: WorktreeAction,
}
/// What to do with the worktree after leaving it.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeAction {
    /// Preserve the worktree and its changes.
    Keep,
    /// Ask the host to remove it subject to its own authorization policy.
    Remove,
}

/// Convert an original MCP tools/call result into native Claude result blocks.
/// Unknown media fails explicitly; the complete remote result and metadata are retained.
pub fn mcp_tool_output(result: Value) -> Result<ToolOutput, String> {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or("MCP result omitted its content array")?;
    let mut blocks = Vec::new();
    for item in content {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => blocks.push(ToolResultBlock::Text {
                text: item
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("MCP text content omitted text")?
                    .to_owned(),
            }),
            Some("image") => {
                let media_type = item
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .ok_or("MCP image omitted mimeType")?;
                if !matches!(
                    media_type,
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                ) {
                    return Err(format!(
                        "MCP image type is unsupported by Claude: {media_type}"
                    ));
                }
                blocks.push(ToolResultBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: media_type.to_owned(),
                        data: item
                            .get("data")
                            .and_then(Value::as_str)
                            .ok_or("MCP image omitted data")?
                            .to_owned(),
                    },
                });
            }
            Some("resource") => {
                let resource = item
                    .get("resource")
                    .ok_or("MCP resource omitted resource body")?;
                // Resource URIs are remote data, never authorization to read local paths.
                blocks.push(ToolResultBlock::Text {
                    text: serde_json::to_string(resource).map_err(|e| e.to_string())?,
                });
            }
            Some("resource_link") => blocks.push(ToolResultBlock::Text {
                text: serde_json::to_string(item).map_err(|e| e.to_string())?,
            }),
            kind => {
                return Err(format!(
                    "MCP content cannot be represented in Claude Messages: {}",
                    kind.unwrap_or("missing type")
                ));
            }
        }
    }
    let mut output = ToolOutput::content(blocks);
    output.is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    output.metadata = result.get("_meta").cloned();
    output.structured_result = Some(result);
    Ok(output)
}

/// Exact Claude-visible definition supplied by a caller-owned MCP catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpToolDefinition {
    /// Namespaced MCP tool name (mcp__server__tool).
    pub name: String,
    /// Remote tool description, treated as untrusted data.
    pub description: String,
    /// Exact native input JSON schema; no inferred function schema.
    pub input_schema: Value,
}
impl McpToolDefinition {
    /// Construct a catalog entry; the adapter checks namespace and schema shape.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// Explicit embedding-owned MCP discovery and execution contract.
///
/// The embedding implements this interface against its authorized MCP service.
/// Connections, authentication, remote validation, ownership, cancellation and
/// capture bounds stay in that service; this crate never installs a default.
/// Native results retain errors, structured data, metadata and ordered media.
/// No Responses DynamicToolProvider or tool_search interface is required.
pub trait ClaudeMcpProvider: Send + Sync {
    /// Begin the host's idempotent discovery lifecycle (not a grant of access).
    fn start(&self);
    /// Read the current catalog at every request boundary, including removals.
    fn available_definitions(&self) -> Vec<McpToolDefinition>;
    /// Execute an authorized tool; None means it disappeared after discovery.
    /// An Err remains a provider failure, not an invented successful receipt.
    fn execute(
        &self,
        name: &str,
        input: Value,
        context: HostContext<'_>,
    ) -> impl std::future::Future<Output = Result<Option<ToolOutput>, String>> + Send;
}

/// Claude view over an explicitly supplied, caller-implemented MCP provider.
/// Query definitions at each model request boundary; never freeze a startup
/// catalog. MCP resources and WaitForMcpServers are not implemented here.
pub struct ClaudeMcp<P: ClaudeMcpProvider + ?Sized> {
    provider: std::sync::Arc<P>,
}
impl<P: ClaudeMcpProvider + ?Sized> ClaudeMcp<P> {
    /// Install the caller's authorized provider without starting connections.
    #[must_use]
    pub const fn new(provider: std::sync::Arc<P>) -> Self {
        Self { provider }
    }

    /// Start the existing host's idempotent discovery lifecycle.
    pub fn start(&self) {
        self.provider.start();
    }

    /// Read exact schemas, rejecting duplicate/non-MCP names and malformed schemas.
    pub fn definitions(&self) -> Result<Vec<Value>, String> {
        let mut names = BTreeSet::new();
        self.provider.available_definitions().into_iter().map(|definition| {
            let name = &definition.name;
            if !name.starts_with("mcp__") || !names.insert(name.clone()) {
                return Err(format!("invalid or duplicate MCP tool name: {name}"));
            }
            if !definition.input_schema.is_object() { return Err("MCP input schema must be an object".into()); }
            Ok(json!({"name":name,"description":definition.description,"input_schema":definition.input_schema}))
        }).collect()
    }

    /// Pass intact input to a currently available remote tool. Both removed and
    /// racing removals fail closed; no retry or synthetic lifecycle is performed.
    pub async fn execute(
        &self,
        name: &str,
        input: Value,
        context: HostContext<'_>,
    ) -> Result<ToolOutput, String> {
        if !self
            .definitions()?
            .iter()
            .any(|definition| definition["name"].as_str() == Some(name))
        {
            return Err(format!("MCP tool is no longer available: {name}"));
        }
        if !input.is_object() {
            return Err("MCP input must be an object".into());
        }
        self.provider
            .execute(name, input, context)
            .await?
            .ok_or_else(|| format!("MCP tool became unavailable: {name}"))
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
#[path = "host_tests.rs"]
mod tests;
