//! Codex's model-managed context-window controls and authenticated history/notes tools.
//!
//! Protocol pinned to openai/codex ac192cd793. Unsupported providers keep remote compaction.
mod backend;
mod spec;

pub use backend::{BackendFuture, HistoryNotesHost, HistoryNotesRequest};

use crate::{
    Tool, ToolContext, ToolDefinition, ToolExposure, ToolInput, ToolOutput, ToolResult,
    runtime::ToolRuntime,
};
use async_trait::async_trait;
use backend::Backend;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use nanocodex_oai_api::{
    Model,
    auth::{OpenAiAuth, OpenAiAuthMode},
    responses::{ContentItem, MessageRole, ResponseItem},
    tools::ToolOutputContent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spec::HistoryNotesAction;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Deserialize)]
pub struct TokenBudget {
    pub reminder_threshold_tokens: u64,
    pub reminder_message_template: String,
    pub guidance_message: String,
    pub auto_compact_fallback_prompt: String,
    pub auto_compact_fallback_buffer_tokens: u64,
}

/// Persistent identity of a model context window. Stored in the conversation context.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContextWindow {
    pub first_window_id: String,
    pub previous_window_id: Option<String>,
    pub context_window_id: String,
    pub window_number: u64,
}

impl ContextWindow {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            first_window_id: id.clone(),
            previous_window_id: None,
            context_window_id: id,
            window_number: 0,
        }
    }

    fn from_history(history: &[ResponseItem]) -> Option<Self> {
        history.iter().rev().find_map(|item| {
            let ResponseItem::Message {
                role: MessageRole::Developer,
                content,
                ..
            } = item
            else {
                return None;
            };
            content.iter().find_map(|part| {
                let ContentItem::InputText { text } = part else {
                    return None;
                };
                let text = text
                    .strip_prefix("<context_window>\n")?
                    .strip_suffix("\n</context_window>")?;
                let field = |name: &str| {
                    text.lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(str::to_owned)
                };
                Some(Self {
                    first_window_id: field("First context window id: ")?,
                    previous_window_id: field("Previous context window id: "),
                    context_window_id: field("Current context window id: ")?,
                    window_number: field("Context window number: ")?.parse().ok()?,
                })
            })
        })
    }
}

struct State {
    window: Mutex<ContextWindow>,
    requested: AtomicBool,
    remaining: AtomicU64,
}

/// Session-owned context controls; cloning preserves live tools across a context reset.
#[derive(Clone)]
pub struct ContextManagement {
    backend: Backend,
    state: Arc<State>,
    budget: Arc<TokenBudget>,
}

impl ContextManagement {
    /// Resolves the exact upstream model/provider/subscription eligibility gate.
    pub async fn eligible(model: Model, auth: &OpenAiAuth, base_url: &str) -> bool {
        if model != Model::Astra
            || auth.mode() != OpenAiAuthMode::ChatGpt
            || base_url.trim_end_matches('/') != OpenAiAuthMode::ChatGpt.default_api_base_url()
        {
            return false;
        }
        auth.snapshot()
            .await
            .is_ok_and(|snapshot| eligible_plan(snapshot.bearer()))
    }

    /// Creates controls, restoring context identity from durable history when present.
    pub fn new(
        auth: OpenAiAuth,
        base_url: String,
        session_id: String,
        agent_name: String,
        history: &[ResponseItem],
    ) -> Self {
        #[cfg(not(target_family = "wasm"))]
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        Self {
            backend: Backend {
                #[cfg(not(target_family = "wasm"))]
                client: reqwest::Client::new(),
                host: None,
                auth,
                base_url,
                thread_id: session_id.clone(),
                session_id,
                agent_name,
            },
            state: Arc::new(State {
                window: Mutex::new(
                    ContextWindow::from_history(history).unwrap_or_else(ContextWindow::new),
                ),
                requested: AtomicBool::new(false),
                remaining: AtomicU64::new(u64::MAX),
            }),
            budget: Arc::new(
                serde_json::from_str(include_str!("token_budget.json"))
                    .expect("pinned Codex token budget"),
            ),
        }
    }

    /// Installs the embedding's authenticated operation boundary.
    pub fn with_host(mut self, host: Arc<dyn HistoryNotesHost>, thread_id: String) -> Self {
        self.backend.host = Some(host);
        self.backend.thread_id = thread_id;
        self
    }

    /// Installs control tools and the direct-only history/notes namespace contracts.
    pub fn install(&self, runtime: &mut ToolRuntime) -> Result<(), String> {
        let names = HistoryNotesAction::ALL
            .into_iter()
            .map(|action| format!("{}__{}", action.namespace(), action.name()))
            .chain(["new_context".to_owned(), "get_context_remaining".to_owned()]);
        for name in names {
            if runtime.contains(&name) {
                return Err(format!(
                    "experimental context tool {name} conflicts with a registered tool"
                ));
            }
        }
        runtime.add_context_tool(
            Arc::new(ControlTool {
                context: self.clone(),
                reset: true,
            }),
            ToolExposure::DirectOnly,
        );
        runtime.add_context_tool(
            Arc::new(ControlTool {
                context: self.clone(),
                reset: false,
            }),
            runtime.default_exposure(),
        );
        for action in HistoryNotesAction::ALL {
            runtime.add_context_tool(
                Arc::new(HistoryTool {
                    backend: self.backend.clone(),
                    action,
                }),
                ToolExposure::DirectOnly,
            );
        }
        Ok(())
    }

    /// Returns model-owned thresholds and guidance from the pinned Codex catalog.
    pub fn budget(&self) -> &TokenBudget {
        &self.budget
    }
    /// Returns the current persisted window identity.
    pub fn window(&self) -> ContextWindow {
        self.state
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Returns the canonical agent name used by history, notes, and request metadata.
    pub fn agent_name(&self) -> &str {
        &self.backend.agent_name
    }
    /// Updates the token count visible to get_context_remaining.
    pub fn set_remaining(&self, remaining: u64) {
        self.state.remaining.store(remaining, Ordering::Release);
    }
    /// Consumes a model request to begin a fresh window.
    pub fn take_request(&self) -> bool {
        self.state.requested.swap(false, Ordering::AcqRel)
    }
    /// Advances identity without resetting tools, environment, or server-side notes.
    pub fn advance(&self) {
        let mut window = self
            .state
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        window.previous_window_id = Some(window.context_window_id.clone());
        window.context_window_id = uuid::Uuid::new_v4().to_string();
        window.window_number += 1;
    }

    /// Prepares the next identity without mutating the live window before its
    /// replacement has been durably recorded.
    pub fn successor(&self) -> Self {
        let next = Self {
            backend: self.backend.clone(),
            budget: Arc::clone(&self.budget),
            state: Arc::new(State {
                window: Mutex::new(self.window()),
                requested: AtomicBool::new(false),
                remaining: AtomicU64::new(u64::MAX),
            }),
        };
        next.advance();
        next
    }
    /// Restores a request's retained identity during durable replay.
    pub fn restore(&self, history: &[ResponseItem]) {
        if let Some(window) = ContextWindow::from_history(history) {
            *self
                .state
                .window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = window;
        }
    }
    /// Produces the canonical context-window and model guidance messages.
    pub async fn initial_context(&self) -> Vec<ResponseItem> {
        let window = self.window();
        let mut body = format!(
            "Agent name: {}\nFirst context window id: {}\nCurrent context window id: {}\nContext window number: {}",
            self.agent_name(),
            window.first_window_id,
            window.context_window_id,
            window.window_number
        );
        if let Some(previous) = window.previous_window_id {
            body.push_str(&format!("\nPrevious context window id: {previous}"));
        }
        if let Ok(hint) = self
            .backend
            .call(
                "alpha/notes/v2/thread_hint",
                json!({}),
                json!({"mode": "bytes", "limit": 4000}),
            )
            .await
            && let Some(hint) = hint
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| text.len() <= 4000 && !text.is_empty())
        {
            body.push('\n');
            body.push_str(hint);
        }
        vec![
            developer(format!("<context_window>\n{body}\n</context_window>")),
            developer(format!(
                "<context_window_guidance>\n{}\n</context_window_guidance>",
                self.budget.guidance_message
            )),
        ]
    }
}

fn eligible_plan(bearer: &str) -> bool {
    let Some(payload) = bearer
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    matches!(
        payload["https://api.openai.com/auth"]["chatgpt_plan_type"].as_str(),
        Some("plus" | "pro" | "prolite")
    )
}

pub(crate) fn namespace_description(namespace: &str) -> Option<&'static str> {
    HistoryNotesAction::ALL
        .into_iter()
        .find(|action| action.namespace() == namespace)
        .map(|action| action.namespace_description())
}

pub(crate) fn group_definitions(definitions: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let mut grouped = Vec::new();
    for mut definition in definitions {
        let canonical = definition.name().to_owned();
        let Some((namespace, name)) = canonical.rsplit_once("__") else {
            grouped.push(definition);
            continue;
        };
        let Some(description) = namespace_description(namespace) else {
            grouped.push(definition);
            continue;
        };
        let ToolDefinition::Function {
            name: direct_name, ..
        } = &mut definition
        else {
            grouped.push(definition);
            continue;
        };
        *direct_name = name.into();
        if let Some(ToolDefinition::Namespace { tools, .. }) = grouped.iter_mut().find(
            |group| matches!(group, ToolDefinition::Namespace { name, .. } if &**name == namespace),
        ) {
            tools.push(definition);
        } else {
            grouped.push(ToolDefinition::namespace(
                namespace,
                description,
                [definition],
            ));
        }
    }
    grouped
}

fn developer(text: String) -> ResponseItem {
    ResponseItem::message(
        MessageRole::Developer,
        [ContentItem::InputText { text: text.into() }],
    )
}

struct ControlTool {
    context: ContextManagement,
    reset: bool,
}
#[async_trait]
impl Tool for ControlTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description) = if self.reset {
            (
                "new_context",
                "Start a new context window. Does not clear, reset, or otherwise affect environment state.",
            )
        } else {
            (
                "get_context_remaining",
                "Get the remaining tokens in the current context window.",
            )
        };
        let definition = ToolDefinition::function(
            name,
            description,
            json!({"type":"object", "properties":{}, "additionalProperties":false}),
        );
        if self.reset {
            definition
        } else {
            definition.with_output_schema(json!({"type":"object","properties":{"tokens_left":{"anyOf":[{"type":"integer"},{"type":"null"}],"description":"Remaining tokens in the current context window, or null when unavailable."}},"required":["tokens_left"],"additionalProperties":false}))
        }
    }
    async fn execute(&self, input: ToolInput, _: ToolContext<'_>) -> ToolResult {
        let _: Value = input.decode_json()?;
        if self.reset {
            self.context.state.requested.store(true, Ordering::Release);
            Ok(ToolOutput::text(
                "A new context window will start without summarizing conversation history.",
            ))
        } else {
            let left = self.context.state.remaining.load(Ordering::Acquire);
            let text = if left == u64::MAX {
                "You have unknown tokens left in this context window.".to_owned()
            } else {
                format!("You have {left} tokens left in this context window.")
            };
            Ok(ToolOutput::text(text)
                .with_structured_result(json!({"tokens_left": (left != u64::MAX).then_some(left)})))
        }
    }
}

struct HistoryTool {
    backend: Backend,
    action: HistoryNotesAction,
}
#[async_trait]
impl Tool for HistoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            format!("{}__{}", self.action.namespace(), self.action.name()),
            self.action.description(),
            self.action.parameters(),
        )
    }
    fn supports_parallel_tool_calls(&self) -> bool {
        self.action.supports_parallel_tool_calls()
    }
    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let arguments = input.decode_json()?;
        Ok(
            match self
                .backend
                .call(
                    self.action.endpoint(),
                    arguments,
                    json!({"mode": "tokens", "limit": context.output_token_budget()}),
                )
                .await
            {
                Ok(result) => output(result),
                Err(error) => ToolOutput::error(error),
            },
        )
    }
}

fn output(mut result: Value) -> ToolOutput {
    let images = result
        .as_object_mut()
        .and_then(|object| object.remove("images"));
    let mut content = match result.get("encrypted_output").and_then(Value::as_str) {
        Some(encrypted) => vec![ToolOutputContent::EncryptedContent {
            encrypted_content: encrypted.to_owned(),
        }],
        None => vec![ToolOutputContent::InputText {
            text: result.to_string(),
        }],
    };
    if let Some(images) = images {
        let Some(images) = images.as_array() else {
            return ToolOutput::error("History backend returned invalid image content.");
        };
        for image in images {
            let (Some(data), Some(mime), Some(detail)) = (
                image["data"].as_str(),
                image["mime_type"].as_str(),
                serde_json::from_value(image["detail"].clone()).ok(),
            ) else {
                return ToolOutput::error("History backend returned invalid image content.");
            };
            content.push(ToolOutputContent::InputImage {
                image_url: format!("data:{mime};base64,{data}"),
                detail,
            });
        }
    }
    ToolOutput::content(content)
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn context_controls_are_direct_and_do_not_leak_into_code_mode() {
        let context = ContextManagement::new(
            OpenAiAuth::api_key("test-key"),
            "http://127.0.0.1:1".into(),
            "session".into(),
            "/root".into(),
            &[],
        );
        let tools = crate::Tools::builder().without_defaults().build().unwrap();
        let mut runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
        context.install(&mut runtime).unwrap();
        let specs = serde_json::to_value(runtime.model_specs("session")).unwrap();
        let specs = specs.as_array().unwrap();
        assert!(specs.iter().any(|spec| spec["name"] == "new_context"));
        let history = specs.iter().find(|spec| spec["name"] == "history").unwrap();
        assert_eq!(history["tools"].as_array().unwrap().len(), 4);
        let notes = specs.iter().find(|spec| spec["name"] == "notes").unwrap();
        assert_eq!(notes["tools"].as_array().unwrap().len(), 5);
        let exec = specs.iter().find(|spec| spec["name"] == "exec").unwrap();
        assert!(
            !exec["description"]
                .as_str()
                .unwrap()
                .contains("new_context")
        );
        let call_context = ToolContext::new("gpt-6-astra", "session", "call", &[], 10_000);
        let output = runtime
            .execute_code(
                "text(typeof tools.new_context); text(typeof tools.history__read_item);",
                call_context,
            )
            .await;
        assert!(output.success);
        assert!(!context.take_request());
        let control = ControlTool {
            context: context.clone(),
            reset: false,
        };
        context.set_remaining(1234);
        let output = control
            .execute(
                ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
                call_context,
            )
            .await
            .unwrap();
        assert_eq!(output.structured_result(), json!({"tokens_left":1234}));
        let reset = ControlTool {
            context: context.clone(),
            reset: true,
        };
        reset
            .execute(
                ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
                call_context,
            )
            .await
            .unwrap();
        assert!(context.take_request());
        assert!(!context.take_request());
    }
    #[test]
    fn only_supported_subscriptions_are_eligible() {
        for (plan, expected) in [
            ("plus", true),
            ("pro", true),
            ("prolite", true),
            ("free", false),
            ("enterprise", false),
            ("team", false),
        ] {
            let token = format!(
                "header.{}.signature",
                URL_SAFE_NO_PAD.encode(
                    json!({"https://api.openai.com/auth":{"chatgpt_plan_type":plan}}).to_string()
                )
            );
            assert_eq!(eligible_plan(&token), expected, "{plan}");
        }
        assert!(!eligible_plan("invalid"));
    }
    #[test]
    fn history_images_and_encrypted_output_are_preserved() {
        let result = output(
            json!({"encrypted_output":"opaque", "images":[{"data":"abc", "mime_type":"image/png", "detail":"original"}]}),
        );
        assert!(result.success);
        let crate::ToolOutputBody::Content(content) = result.output else {
            panic!("content");
        };
        assert!(
            matches!(&content[0], ToolOutputContent::EncryptedContent { encrypted_content } if encrypted_content == "opaque")
        );
        assert!(
            matches!(&content[1], ToolOutputContent::InputImage { image_url, .. } if image_url == "data:image/png;base64,abc")
        );
    }
}
