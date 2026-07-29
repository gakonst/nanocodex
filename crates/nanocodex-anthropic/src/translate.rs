//! Translation between the Responses item model and the Anthropic Messages API.
//!
//! Nanocodex's agent loop, rollout, and compaction are written against Responses items
//! and [`ServerEvent`]. Rather than teach every layer a second protocol, the Anthropic
//! provider translates at the transport boundary: requests are built from the same
//! [`ResponseItem`] history, and streaming events are rewritten into [`ServerEvent`].

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::wire::{
    ContentBlock, Delta, ImageSource, InputBlock, InputMessage, MessageUsage, MessagesRequest,
    OutputConfig, Role, StreamEvent, SystemBlock, ThinkingConfig, ToolResultBlock,
    ToolResultContent, ToolSpec,
};
use nanocodex_oai_api::{
    Thinking,
    responses::{
        CompletedResponse, ContentItem, FunctionOutputBody, FunctionOutputContent,
        InputTokenDetails, ItemStatus, MessageRole, OutputTokenDetails, ReasoningSummary,
        ResponseItem, ResponseItemId, ServerEvent, ToolDefinition, Usage,
    },
};

/// Default output ceiling. Anthropic requires `max_tokens`; this leaves room for
/// adaptive thinking plus a full answer without approaching the 128K model cap.
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

/// Builds a Messages request from Responses history.
///
/// `system_prompt` becomes the cached system prefix. Signed thinking and redacted
/// thinking emitted by Anthropic are retained as opaque reasoning and restored here.
/// Responses-specific bookkeeping without a Messages representation is dropped.
#[must_use]
pub fn build_request<'a>(
    model: &str,
    max_tokens: u32,
    thinking: Thinking,
    system_prompt: &str,
    items: impl IntoIterator<Item = &'a ResponseItem>,
    tools: &[ToolDefinition],
) -> MessagesRequest {
    let mut system = Vec::new();
    if !system_prompt.trim().is_empty() {
        system.push(SystemBlock::text(system_prompt).cached());
    }

    let mut builder = MessageBuilder::default();
    // Responses Lite carries model-visible tools inside the input stream, so the
    // request's tool set is the union of the explicit list and any declared inline.
    let mut specs: Vec<ToolSpec> = tools.iter().filter_map(tool_spec).collect();
    for item in items {
        if is_embedded_openai_system_prompt(item) {
            continue;
        }
        if let ResponseItem::AdditionalTools { tools, .. } = item {
            for tool in tools {
                if !specs.iter().any(|spec| spec.name == tool.name())
                    && let Some(spec) = tool_spec(tool)
                {
                    specs.push(spec);
                }
            }
            continue;
        }
        append_item(&mut builder, &mut system, item);
    }
    let messages = builder.finish();

    let (thinking_config, output_config) = thinking_settings(thinking);

    MessagesRequest {
        model: model.to_owned(),
        max_tokens,
        system,
        messages,
        tools: specs,
        thinking: Some(thinking_config),
        output_config,
        stream: true,
    }
}

/// Maps the harness thinking level onto Anthropic's thinking and effort controls.
///
/// Disabling thinking is only accepted at effort `high` or below, so the disabled path
/// pins effort rather than sending a combination the API rejects. Adaptive thinking uses
/// Claude Code's omitted display mode so internal reasoning is not printed in the transcript.
const fn thinking_settings(thinking: Thinking) -> (ThinkingConfig, Option<OutputConfig>) {
    match thinking {
        Thinking::None => (
            ThinkingConfig::Disabled,
            Some(OutputConfig { effort: "high" }),
        ),
        Thinking::Low => (adaptive(), Some(OutputConfig { effort: "low" })),
        Thinking::Medium => (adaptive(), Some(OutputConfig { effort: "medium" })),
        Thinking::High => (adaptive(), Some(OutputConfig { effort: "high" })),
        Thinking::Xhigh => (adaptive(), Some(OutputConfig { effort: "xhigh" })),
        Thinking::Max => (adaptive(), Some(OutputConfig { effort: "max" })),
    }
}

const fn adaptive() -> ThinkingConfig {
    ThinkingConfig::Adaptive {
        display: Some("omitted"),
    }
}

fn tool_spec(tool: &ToolDefinition) -> Option<ToolSpec> {
    match tool {
        ToolDefinition::Function {
            name,
            description,
            parameters,
            ..
        } => Some(ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: object_schema(parameters.as_value().clone()),
        }),
        // Anthropic tools take a JSON object; there is no freeform-grammar tool. The
        // grammar is surfaced in the description and the call is carried as one string.
        ToolDefinition::Custom {
            name,
            description,
            format,
        } => Some(ToolSpec {
            name: name.to_string(),
            description: format!(
                "{description}\n\nProvide the call as a single `input` string in {} syntax:\n{}",
                format.syntax, format.definition
            ),
            input_schema: freeform_input_schema(),
        }),
        // Deferred discovery is still available inside Code Mode's `exec` tool.
        // Messages has no equivalent declaration with the same client dispatch contract.
        ToolDefinition::ToolSearch { .. } => None,
    }
}

/// Anthropic requires `input_schema` to be a JSON Schema object.
fn object_schema(value: Value) -> Value {
    if value.get("type").and_then(Value::as_str) == Some("object") {
        return value;
    }
    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("properties".into(), Value::Object(Map::new()));
    Value::Object(schema)
}

fn freeform_input_schema() -> Value {
    let mut input = Map::new();
    input.insert("type".into(), Value::String("string".into()));

    let mut properties = Map::new();
    properties.insert("input".into(), Value::Object(input));

    let mut schema = Map::new();
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("properties".into(), Value::Object(properties));
    schema.insert(
        "required".into(),
        Value::Array(vec![Value::String("input".into())]),
    );
    Value::Object(schema)
}

/// Accumulates blocks into role-contiguous messages.
///
/// Consecutive blocks for the same role are merged into one message. This matters for
/// tool results: every result for a parallel tool-call batch must arrive in a single
/// user message, or the model learns to stop issuing parallel calls.
#[derive(Default)]
struct MessageBuilder {
    messages: Vec<InputMessage>,
}

impl MessageBuilder {
    fn push(&mut self, role: Role, block: InputBlock) {
        match self.messages.last_mut() {
            Some(message) if message.role == role => message.content.push(block),
            _ => self.messages.push(InputMessage {
                role,
                content: vec![block],
            }),
        }
    }

    fn finish(mut self) -> Vec<InputMessage> {
        self.messages.retain(|message| !message.content.is_empty());
        // The API requires the first message to be from the user.
        if self
            .messages
            .first()
            .is_some_and(|message| message.role == Role::Assistant)
        {
            self.messages.insert(
                0,
                InputMessage {
                    role: Role::User,
                    content: vec![InputBlock::Text {
                        text: "Continue from the transcript below.".to_owned(),
                    }],
                },
            );
        }
        self.messages
    }
}

fn append_item(builder: &mut MessageBuilder, system: &mut Vec<SystemBlock>, item: &ResponseItem) {
    match item {
        ResponseItem::Message { role, content, .. } => {
            append_message(builder, system, *role, content);
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => {
            builder.push(
                Role::Assistant,
                InputBlock::ToolUse {
                    id: call_id.to_string(),
                    name: name.to_string(),
                    input: parse_arguments(arguments),
                },
            );
        }
        ResponseItem::FunctionCallOutput {
            call_id,
            output,
            status,
            ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id,
            output,
            status,
            ..
        } => {
            builder.push(
                Role::User,
                InputBlock::ToolResult {
                    tool_use_id: call_id.to_string(),
                    content: tool_result_content(output),
                    is_error: matches!(status, Some(ItemStatus::Failed)),
                },
            );
        }
        ResponseItem::CustomToolCall {
            name,
            input,
            call_id,
            ..
        } => {
            let mut arguments = Map::new();
            arguments.insert("input".into(), Value::String(input.to_string()));
            builder.push(
                Role::Assistant,
                InputBlock::ToolUse {
                    id: call_id.to_string(),
                    name: name.to_string(),
                    input: Value::Object(arguments),
                },
            );
        }
        ResponseItem::Reasoning {
            encrypted_content: Some(encrypted_content),
            ..
        } => {
            if let Some(block) = decode_anthropic_reasoning(encrypted_content) {
                builder.push(Role::Assistant, block);
            }
        }
        // The remaining items are Responses-specific bookkeeping with no wire equivalent.
        ResponseItem::Reasoning { .. }
        | ResponseItem::AdditionalTools { .. }
        | ResponseItem::AgentMessage { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger {}
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other(_) => {}
    }
}

fn append_message(
    builder: &mut MessageBuilder,
    system: &mut Vec<SystemBlock>,
    role: MessageRole,
    content: &[ContentItem],
) {
    // Developer turns carry operator instructions; Anthropic expresses those as system
    // context rather than a conversation role.
    if role == MessageRole::Developer {
        for item in content {
            if let Some(text) = content_text(item)
                && !text.trim().is_empty()
            {
                system.push(SystemBlock::text(text));
            }
        }
        return;
    }

    let wire_role = match role {
        MessageRole::Assistant => Role::Assistant,
        MessageRole::User | MessageRole::Developer => Role::User,
    };

    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                if !text.trim().is_empty() {
                    builder.push(
                        wire_role,
                        InputBlock::Text {
                            text: text.to_string(),
                        },
                    );
                }
            }
            ContentItem::InputImage { image_url, .. } => {
                if let Some(block) = image_block(image_url) {
                    builder.push(wire_role, block);
                }
            }
            // Audio has no Messages representation.
            ContentItem::InputAudio { .. } => {}
        }
    }
}

const fn content_text(item: &ContentItem) -> Option<&str> {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => Some(text),
        ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
    }
}

/// Accepts both remote URLs and `data:` URIs, which Anthropic models differently.
fn image_block(image_url: &str) -> Option<InputBlock> {
    image_source(image_url).map(|source| InputBlock::Image { source })
}

fn image_source(image_url: &str) -> Option<ImageSource> {
    let Some(rest) = image_url.strip_prefix("data:") else {
        return Some(ImageSource::Url {
            url: image_url.to_owned(),
        });
    };
    let (media_type, data) = rest.split_once(";base64,")?;
    Some(ImageSource::Base64 {
        media_type: media_type.to_owned(),
        data: data.to_owned(),
    })
}

/// Tool arguments cross the Responses boundary as a JSON string. Anthropic expects a
/// decoded object; anything unparseable is preserved under a raw key rather than dropped.
fn parse_arguments(arguments: &str) -> Value {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }
    if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let mut raw = Map::new();
    raw.insert("input".into(), Value::String(arguments.to_owned()));
    Value::Object(raw)
}

fn tool_result_content(output: &FunctionOutputBody) -> ToolResultContent {
    match output {
        FunctionOutputBody::Text(text) => ToolResultContent::Text(text.to_string()),
        FunctionOutputBody::Content(items) => ToolResultContent::Blocks(
            items
                .iter()
                .filter_map(|item| match item {
                    FunctionOutputContent::InputText { text } => Some(ToolResultBlock::Text {
                        text: text.to_string(),
                    }),
                    FunctionOutputContent::InputImage { image_url, .. } => {
                        image_source(image_url).map(|source| ToolResultBlock::Image { source })
                    }
                    FunctionOutputContent::EncryptedContent { .. }
                    | FunctionOutputContent::InputAudio { .. } => None,
                })
                .collect(),
        ),
    }
}

fn is_embedded_openai_system_prompt(item: &ResponseItem) -> bool {
    const OPENAI_SYSTEM_PROMPT_PREFIX: &str = "You are Codex, an agent based on GPT-5.";

    matches!(
        item,
        ResponseItem::Message {
            role: MessageRole::Developer,
            content,
            ..
        } if content.iter().any(|item| {
            content_text(item).is_some_and(|text| text.starts_with(OPENAI_SYSTEM_PROMPT_PREFIX))
        })
    )
}

// ---------------------------------------------------------------------------
// Streaming translation
// ---------------------------------------------------------------------------

enum BlockState {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking(String),
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
    Ignored,
}

/// Rewrites an Anthropic streaming response into Responses [`ServerEvent`]s.
///
/// One Anthropic event can produce zero, one, or several server events, so
/// [`push`](Self::push) returns a batch. The translator also accumulates the completed
/// items and usage needed to synthesize the terminal `response.completed` event, which
/// Anthropic does not send in a single payload.
#[derive(Default)]
pub struct StreamTranslator {
    response_id: String,
    blocks: BTreeMap<u32, BlockState>,
    custom_tools: BTreeSet<String>,
    output: Vec<ResponseItem>,
    usage: Usage,
    stop_reason: Option<String>,
}

impl StreamTranslator {
    #[cfg(test)]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures the custom/freeform tool names declared on the request.
    #[must_use]
    pub fn with_custom_tools(tools: impl IntoIterator<Item = String>) -> Self {
        Self {
            custom_tools: tools.into_iter().collect(),
            ..Self::default()
        }
    }

    /// Translates one Anthropic event into zero or more Responses events.
    pub fn push(&mut self, event: StreamEvent) -> Vec<ServerEvent> {
        match event {
            StreamEvent::MessageStart { message } => {
                self.response_id = message.id;
                if let Some(usage) = message.usage {
                    self.merge_usage(&usage);
                }
                vec![ServerEvent::Created { response: None }]
            }
            StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                self.blocks.insert(index, start_block(content_block));
                Vec::new()
            }
            StreamEvent::ContentBlockDelta { index, delta } => self.apply_delta(index, delta),
            StreamEvent::ContentBlockStop { index } => self.finish_block(index),
            StreamEvent::MessageDelta { delta, usage } => {
                if let Some(stop_reason) = delta.stop_reason {
                    self.stop_reason = Some(stop_reason);
                }
                if let Some(usage) = usage {
                    self.merge_usage(&usage);
                }
                Vec::new()
            }
            StreamEvent::MessageStop => vec![ServerEvent::Completed {
                response: self.completed(),
            }],
            StreamEvent::Error { .. } => vec![ServerEvent::Error],
            StreamEvent::Ping | StreamEvent::Other => Vec::new(),
        }
    }

    fn apply_delta(&mut self, index: u32, delta: Delta) -> Vec<ServerEvent> {
        let Some(state) = self.blocks.get_mut(&index) else {
            return Vec::new();
        };
        match (state, delta) {
            (BlockState::Text(buffer), Delta::TextDelta { text }) => {
                buffer.push_str(&text);
                vec![ServerEvent::OutputTextDelta {
                    output_index: Some(index),
                    delta: text,
                }]
            }
            (
                BlockState::Thinking {
                    thinking: buffer, ..
                },
                Delta::ThinkingDelta { thinking },
            ) => {
                buffer.push_str(&thinking);
                vec![ServerEvent::ReasoningSummaryTextDelta {
                    delta: thinking,
                    summary_index: i64::from(index),
                }]
            }
            (
                BlockState::Thinking { signature, .. },
                Delta::SignatureDelta { signature: delta },
            ) => {
                signature.push_str(&delta);
                Vec::new()
            }
            (BlockState::ToolUse { arguments, .. }, Delta::InputJsonDelta { partial_json }) => {
                arguments.push_str(&partial_json);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn finish_block(&mut self, index: u32) -> Vec<ServerEvent> {
        let Some(state) = self.blocks.remove(&index) else {
            return Vec::new();
        };
        let item = match state {
            BlockState::Text(text) if !text.is_empty() => ResponseItem::Message {
                id: self.item_id(index),
                role: MessageRole::Assistant,
                content: vec![ContentItem::output_text(text)],
                status: Some(ItemStatus::Completed),
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            BlockState::Thinking {
                thinking,
                signature,
            } if !thinking.is_empty() || !signature.is_empty() => ResponseItem::Reasoning {
                id: self.item_id(index),
                summary: (!thinking.is_empty())
                    .then(|| ReasoningSummary::SummaryText {
                        text: thinking.clone().into(),
                    })
                    .into_iter()
                    .collect(),
                content: None,
                encrypted_content: (!signature.is_empty())
                    .then(|| encode_thinking(&thinking, &signature)),
                status: Some(ItemStatus::Completed),
                internal_chat_message_metadata_passthrough: None,
            },
            BlockState::RedactedThinking(data) => ResponseItem::Reasoning {
                id: self.item_id(index),
                summary: Vec::new(),
                content: None,
                encrypted_content: Some(encode_redacted_thinking(&data)),
                status: Some(ItemStatus::Completed),
                internal_chat_message_metadata_passthrough: None,
            },
            BlockState::ToolUse {
                id,
                name,
                arguments,
            } => {
                if self.custom_tools.contains(&name) {
                    ResponseItem::CustomToolCall {
                        id: self.item_id(index),
                        status: Some(ItemStatus::Completed),
                        call_id: id.into(),
                        name: name.into(),
                        namespace: None,
                        input: custom_tool_input(arguments).into(),
                        caller: None,
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    }
                } else {
                    ResponseItem::FunctionCall {
                        id: self.item_id(index),
                        name: name.into(),
                        namespace: None,
                        arguments: normalize_arguments(arguments).into(),
                        call_id: id.into(),
                        caller: None,
                        status: Some(ItemStatus::Completed),
                        created_by: None,
                        internal_chat_message_metadata_passthrough: None,
                    }
                }
            }
            BlockState::Text(_) | BlockState::Thinking { .. } | BlockState::Ignored => {
                return Vec::new();
            }
        };
        self.output.push(item.clone());
        vec![ServerEvent::OutputItemDone { item }]
    }

    fn item_id(&self, index: u32) -> Option<ResponseItemId> {
        if self.response_id.is_empty() {
            return None;
        }
        Some(ResponseItemId::with_suffix(&self.response_id, index))
    }

    const fn merge_usage(&mut self, usage: &MessageUsage) {
        let input_tokens = usage
            .input_tokens
            .saturating_add(usage.cache_read_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens);
        if input_tokens > 0 {
            self.usage.input_tokens = input_tokens;
        }
        if usage.output_tokens > 0 {
            self.usage.output_tokens = usage.output_tokens;
        }
        if usage.cache_read_input_tokens > 0 || usage.cache_creation_input_tokens > 0 {
            self.usage.input_tokens_details = Some(InputTokenDetails {
                cached_tokens: usage.cache_read_input_tokens,
                cache_write_tokens: usage.cache_creation_input_tokens,
            });
        }
        self.usage.total_tokens = self.usage.input_tokens + self.usage.output_tokens;
    }

    fn completed(&mut self) -> CompletedResponse {
        if self.usage.output_tokens_details.is_none() {
            self.usage.output_tokens_details = Some(OutputTokenDetails {
                reasoning_tokens: 0,
            });
        }
        let stop_reason = self.stop_reason.as_deref();
        CompletedResponse {
            id: self.response_id.clone(),
            status: match stop_reason {
                Some("max_tokens") => "incomplete".to_owned(),
                Some("refusal") => "failed".to_owned(),
                _ => "completed".to_owned(),
            },
            // `tool_use` means the model is waiting on results, so the turn continues.
            end_turn: match stop_reason {
                Some("tool_use" | "pause_turn") => Some(false),
                Some(_) => Some(true),
                None => None,
            },
            output: std::mem::take(&mut self.output),
            usage: Some(self.usage.clone()),
        }
    }
}

fn start_block(content_block: ContentBlock) -> BlockState {
    match content_block {
        ContentBlock::Text { text } => BlockState::Text(text),
        ContentBlock::Thinking { thinking } => BlockState::Thinking {
            thinking,
            signature: String::new(),
        },
        ContentBlock::ToolUse { id, name, input } => BlockState::ToolUse {
            id,
            name,
            arguments: input
                .filter(|value| !value.is_null())
                .map(|value| value.to_string())
                .filter(|value| value != "{}")
                .unwrap_or_default(),
        },
        ContentBlock::RedactedThinking { data } => BlockState::RedactedThinking(data),
        ContentBlock::Other => BlockState::Ignored,
    }
}

const ANTHROPIC_REASONING_PREFIX: &str = "anthropic-messages:";

fn encode_thinking(thinking: &str, signature: &str) -> Box<str> {
    format!(
        "{ANTHROPIC_REASONING_PREFIX}{}",
        serde_json::json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": signature,
        })
    )
    .into()
}

fn encode_redacted_thinking(data: &str) -> Box<str> {
    format!(
        "{ANTHROPIC_REASONING_PREFIX}{}",
        serde_json::json!({
            "type": "redacted_thinking",
            "data": data,
        })
    )
    .into()
}

fn decode_anthropic_reasoning(encrypted: &str) -> Option<InputBlock> {
    let encoded = encrypted.strip_prefix(ANTHROPIC_REASONING_PREFIX)?;
    match serde_json::from_str::<StoredReasoningBlock>(encoded).ok()? {
        StoredReasoningBlock::Thinking {
            thinking,
            signature,
        } => Some(InputBlock::Thinking {
            thinking,
            signature,
        }),
        StoredReasoningBlock::RedactedThinking { data } => {
            Some(InputBlock::RedactedThinking { data })
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredReasoningBlock {
    Thinking { thinking: String, signature: String },
    RedactedThinking { data: String },
}

/// Tool arguments must reach the harness as a JSON object string, even when the model
/// emitted no arguments at all.
fn normalize_arguments(arguments: String) -> String {
    if arguments.trim().is_empty() {
        "{}".to_owned()
    } else {
        arguments
    }
}

/// Anthropic represents freeform tools as an object with one string field because
/// every Messages API tool must have an object input schema.
fn custom_tool_input(arguments: String) -> String {
    serde_json::from_str::<Value>(&arguments)
        .ok()
        .and_then(|value| value.get("input")?.as_str().map(str::to_owned))
        .unwrap_or(arguments)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_TOKENS, StreamTranslator, build_request};
    use crate::wire::{
        InputBlock, Role, StreamEvent, ThinkingConfig, ToolResultBlock, ToolResultContent,
    };
    use nanocodex_oai_api::{
        Thinking,
        responses::{
            ContentItem, FunctionOutputBody, FunctionOutputContent, ItemStatus, MessageRole,
            ResponseItem, ServerEvent, ToolDefinition,
        },
    };

    fn user(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: MessageRole::User,
            content: vec![ContentItem::InputText { text: text.into() }],
            status: None,
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    fn call(call_id: &str, name: &str, arguments: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: name.into(),
            namespace: None,
            arguments: arguments.into(),
            call_id: call_id.into(),
            caller: None,
            status: None,
            created_by: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    fn output(call_id: &str, text: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: call_id.into(),
            output: FunctionOutputBody::Text(text.into()),
            caller: None,
            status: None,
            created_by: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    fn event(json: &str) -> StreamEvent {
        serde_json::from_str(json).expect("event should deserialize")
    }

    #[test]
    fn developer_turns_become_system_context_not_a_conversation_role() {
        let items = vec![
            ResponseItem::Message {
                id: None,
                role: MessageRole::Developer,
                content: vec![ContentItem::InputText {
                    text: "Operator rule.".into(),
                }],
                status: None,
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            user("hello"),
        ];
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "base prompt",
            &items,
            &[],
        );
        assert_eq!(request.system.len(), 2);
        assert_eq!(request.system[1].text, "Operator rule.");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, Role::User);
    }

    #[test]
    fn the_cacheable_prefix_ends_at_the_base_system_prompt() {
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "base prompt",
            &[user("hi")],
            &[],
        );
        assert!(request.system[0].cache_control.is_some());
    }

    #[test]
    fn parallel_tool_results_are_merged_into_one_user_message() {
        let items = vec![
            user("run both"),
            call("toolu_1", "a", "{}"),
            call("toolu_2", "b", "{}"),
            output("toolu_1", "first"),
            output("toolu_2", "second"),
        ];
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &items,
            &[],
        );
        assert_eq!(request.messages.len(), 3);
        assert_eq!(request.messages[1].role, Role::Assistant);
        assert_eq!(request.messages[1].content.len(), 2);
        assert_eq!(request.messages[2].role, Role::User);
        assert_eq!(
            request.messages[2].content.len(),
            2,
            "both tool results must ride in a single user message"
        );
    }

    #[test]
    fn failed_tool_output_is_flagged_as_an_error_result() {
        let items = vec![
            user("go"),
            call("toolu_1", "a", "{}"),
            ResponseItem::FunctionCallOutput {
                id: None,
                call_id: "toolu_1".into(),
                output: FunctionOutputBody::Text("boom".into()),
                caller: None,
                status: Some(ItemStatus::Failed),
                created_by: None,
                internal_chat_message_metadata_passthrough: None,
            },
        ];
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &items,
            &[],
        );
        let InputBlock::ToolResult { is_error, .. } = &request.messages[2].content[0] else {
            panic!("expected a tool result");
        };
        assert!(is_error);
    }

    #[test]
    fn image_tool_results_remain_multimodal() {
        let items = vec![
            user("inspect it"),
            call("toolu_1", "image", "{}"),
            ResponseItem::FunctionCallOutput {
                id: None,
                call_id: "toolu_1".into(),
                output: FunctionOutputBody::Content(vec![
                    FunctionOutputContent::InputText {
                        text: "screenshot".into(),
                    },
                    FunctionOutputContent::InputImage {
                        image_url: "data:image/png;base64,AAAA".into(),
                        detail: None,
                    },
                ]),
                caller: None,
                status: None,
                created_by: None,
                internal_chat_message_metadata_passthrough: None,
            },
        ];
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &items,
            &[],
        );
        let InputBlock::ToolResult {
            content: ToolResultContent::Blocks(blocks),
            ..
        } = &request.messages[2].content[0]
        else {
            panic!("expected multimodal tool result");
        };
        assert!(matches!(blocks[0], ToolResultBlock::Text { .. }));
        assert!(matches!(blocks[1], ToolResultBlock::Image { .. }));
    }

    #[test]
    fn embedded_openai_prompt_is_replaced_by_the_anthropic_prompt() {
        let items = vec![
            ResponseItem::Message {
                id: None,
                role: MessageRole::Developer,
                content: vec![ContentItem::InputText {
                    text: "You are Codex, an agent based on GPT-5. Internal details.".into(),
                }],
                status: None,
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            user("hello"),
        ];
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "You are Claude Code.",
            &items,
            &[],
        );
        assert_eq!(request.system.len(), 1);
        assert_eq!(request.system[0].text, "You are Claude Code.");
    }

    #[test]
    fn unparseable_tool_arguments_are_preserved_rather_than_dropped() {
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &[user("x"), call("toolu_1", "a", "not json")],
            &[],
        );
        let InputBlock::ToolUse { input, .. } = &request.messages[1].content[0] else {
            panic!("expected a tool use");
        };
        assert_eq!(input["input"], "not json");
    }

    #[test]
    fn disabled_thinking_never_pairs_with_an_effort_the_api_rejects() {
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::None,
            "",
            &[user("hi")],
            &[],
        );
        assert!(matches!(request.thinking, Some(ThinkingConfig::Disabled)));
        assert_eq!(request.output_config.expect("effort").effort, "high");
    }

    #[test]
    fn adaptive_thinking_uses_claude_code_display_default() {
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::Xhigh,
            "",
            &[user("hi")],
            &[],
        );
        let Some(ThinkingConfig::Adaptive { display }) = request.thinking else {
            panic!("expected adaptive thinking");
        };
        assert_eq!(display, Some("omitted"));
        assert_eq!(request.output_config.expect("effort").effort, "xhigh");
    }

    #[test]
    fn function_tools_carry_their_object_schema() {
        let tool = ToolDefinition::function(
            "shell",
            "run a command",
            serde_json::json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        );
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &[user("hi")],
            &[tool],
        );
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "shell");
        assert_eq!(
            request.tools[0].input_schema["properties"]["cmd"]["type"],
            "string"
        );
    }

    #[test]
    fn a_transcript_starting_with_the_assistant_gets_a_leading_user_turn() {
        let items = vec![ResponseItem::Message {
            id: None,
            role: MessageRole::Assistant,
            content: vec![ContentItem::output_text("resumed")],
            status: None,
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];
        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &items,
            &[],
        );
        assert_eq!(request.messages[0].role, Role::User);
        assert_eq!(request.messages[1].role, Role::Assistant);
    }

    #[test]
    fn a_text_turn_streams_deltas_then_a_completed_item() {
        let mut translator = StreamTranslator::new();
        assert!(matches!(
            translator.push(event(
                r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":10}}}"#
            ))[..],
            [ServerEvent::Created { .. }]
        ));
        translator.push(event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ));
        let deltas = translator.push(event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        ));
        assert!(matches!(deltas[..], [ServerEvent::OutputTextDelta { .. }]));

        let done = translator.push(event(r#"{"type":"content_block_stop","index":0}"#));
        let [ServerEvent::OutputItemDone { item }] = &done[..] else {
            panic!("expected a completed item");
        };
        let ResponseItem::Message { content, .. } = item else {
            panic!("expected an assistant message");
        };
        let ContentItem::OutputText { text, .. } = &content[0] else {
            panic!("expected output text");
        };
        assert_eq!(&**text, "Hi");
    }

    #[test]
    fn tool_use_blocks_accumulate_partial_json_into_one_function_call() {
        let mut translator = StreamTranslator::new();
        translator.push(event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"shell","input":{}}}"#,
        ));
        for fragment in [r#"{"cmd""#, r#": "ls""#, "}"] {
            let payload = serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": fragment},
            });
            assert!(
                translator
                    .push(serde_json::from_value(payload).unwrap())
                    .is_empty(),
                "argument fragments must not surface as user-visible deltas"
            );
        }
        let done = translator.push(event(r#"{"type":"content_block_stop","index":0}"#));
        let [ServerEvent::OutputItemDone { item }] = &done[..] else {
            panic!("expected a completed item");
        };
        let ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } = item
        else {
            panic!("expected a function call");
        };
        assert_eq!(&**name, "shell");
        assert_eq!(&**call_id, "toolu_9");
        assert_eq!(&**arguments, r#"{"cmd": "ls"}"#);
    }

    #[test]
    fn custom_tool_use_unwraps_the_anthropic_object_input() {
        let mut translator = StreamTranslator::with_custom_tools(["exec".to_owned()]);
        translator.push(event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_10","name":"exec","input":{}}}"#,
        ));
        translator.push(event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"text(2 * 2);\"}"}}"#,
        ));

        let done = translator.push(event(r#"{"type":"content_block_stop","index":0}"#));
        let [
            ServerEvent::OutputItemDone {
                item:
                    ResponseItem::CustomToolCall {
                        name,
                        input,
                        call_id,
                        ..
                    },
            },
        ] = &done[..]
        else {
            panic!("expected a custom tool call");
        };
        assert_eq!(&**name, "exec");
        assert_eq!(&**call_id, "toolu_10");
        assert_eq!(&**input, "text(2 * 2);");
    }

    #[test]
    fn thinking_deltas_surface_as_reasoning_summaries() {
        let mut translator = StreamTranslator::new();
        translator.push(event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ));
        let events = translator.push(event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"weighing"}}"#,
        ));
        assert!(matches!(
            events[..],
            [ServerEvent::ReasoningSummaryTextDelta { .. }]
        ));
        let done = translator.push(event(r#"{"type":"content_block_stop","index":0}"#));
        assert!(matches!(
            done[..],
            [ServerEvent::OutputItemDone {
                item: ResponseItem::Reasoning { .. }
            }]
        ));
    }

    #[test]
    fn signed_thinking_round_trips_through_typed_history() {
        let mut translator = StreamTranslator::new();
        translator.push(event(
            r#"{"type":"message_start","message":{"id":"msg_thinking"}}"#,
        ));
        translator.push(event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ));
        translator.push(event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"private thought"}}"#,
        ));
        translator.push(event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}"#,
        ));
        let [ServerEvent::OutputItemDone { item }] =
            &translator.push(event(r#"{"type":"content_block_stop","index":0}"#))[..]
        else {
            panic!("expected completed reasoning");
        };

        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &[user("continue"), item.clone()],
            &[],
        );
        let InputBlock::Thinking {
            thinking,
            signature,
        } = &request.messages[1].content[0]
        else {
            panic!("expected replayed thinking block");
        };
        assert_eq!(thinking, "private thought");
        assert_eq!(signature, "opaque-signature");
    }

    #[test]
    fn redacted_thinking_round_trips_through_typed_history() {
        let mut translator = StreamTranslator::new();
        translator.push(event(
            r#"{"type":"message_start","message":{"id":"msg_redacted"}}"#,
        ));
        translator.push(event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"opaque-redacted-data"}}"#,
        ));
        let [ServerEvent::OutputItemDone { item }] =
            &translator.push(event(r#"{"type":"content_block_stop","index":0}"#))[..]
        else {
            panic!("expected completed redacted reasoning");
        };

        let request = build_request(
            "claude-opus-5",
            DEFAULT_MAX_TOKENS,
            Thinking::High,
            "",
            &[user("continue"), item.clone()],
            &[],
        );
        let InputBlock::RedactedThinking { data } = &request.messages[1].content[0] else {
            panic!("expected replayed redacted thinking block");
        };
        assert_eq!(data, "opaque-redacted-data");
    }

    #[test]
    fn a_tool_use_stop_reason_keeps_the_turn_open() {
        let mut translator = StreamTranslator::new();
        translator.push(event(
            r#"{"type":"message_start","message":{"id":"msg_2"}}"#,
        ));
        translator.push(event(
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
        ));
        let completed = translator.push(event(r#"{"type":"message_stop"}"#));
        let [ServerEvent::Completed { response }] = &completed[..] else {
            panic!("expected completion");
        };
        assert_eq!(response.end_turn, Some(false));
        assert_eq!(response.status, "completed");
        assert_eq!(response.usage.as_ref().unwrap().output_tokens, 7);
    }

    #[test]
    fn end_turn_closes_the_turn_and_max_tokens_reports_incomplete() {
        let mut finished = StreamTranslator::new();
        finished.push(event(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        ));
        let [ServerEvent::Completed { response }] =
            &finished.push(event(r#"{"type":"message_stop"}"#))[..]
        else {
            panic!("expected completion");
        };
        assert_eq!(response.end_turn, Some(true));

        let mut truncated = StreamTranslator::new();
        truncated.push(event(
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#,
        ));
        let [ServerEvent::Completed { response }] =
            &truncated.push(event(r#"{"type":"message_stop"}"#))[..]
        else {
            panic!("expected completion");
        };
        assert_eq!(response.status, "incomplete");
    }

    #[test]
    fn cache_usage_maps_onto_responses_token_details() {
        let mut translator = StreamTranslator::new();
        translator.push(event(
            r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":100,"cache_read_input_tokens":80,"cache_creation_input_tokens":20}}}"#,
        ));
        let [ServerEvent::Completed { response }] =
            &translator.push(event(r#"{"type":"message_stop"}"#))[..]
        else {
            panic!("expected completion");
        };
        let details = response
            .usage
            .as_ref()
            .unwrap()
            .input_tokens_details
            .as_ref()
            .unwrap();
        assert_eq!(details.cached_tokens, 80);
        assert_eq!(details.cache_write_tokens, 20);
        assert_eq!(response.usage.as_ref().unwrap().input_tokens, 200);
        assert_eq!(response.usage.as_ref().unwrap().total_tokens, 200);
    }

    #[test]
    fn ping_and_unknown_events_are_inert() {
        let mut translator = StreamTranslator::new();
        assert!(translator.push(event(r#"{"type":"ping"}"#)).is_empty());
        assert!(translator.push(event(r#"{"type":"whatever"}"#)).is_empty());
        assert!(matches!(
            translator.push(event(
                r#"{"type":"error","error":{"type":"overloaded_error"}}"#
            ))[..],
            [ServerEvent::Error]
        ));
    }
}
