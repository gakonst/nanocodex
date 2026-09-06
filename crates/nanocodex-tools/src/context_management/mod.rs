//! Codex-style context-window controls with Nanocodex-owned history and notes.
//!
//! Workspace files survive model context resets independently of the provider.
mod backend;
#[cfg(not(target_family = "wasm"))]
pub mod files;
mod spec;

pub use backend::{BackendFuture, HistoryNotesHost, StorageOperation};

use crate::{
    Tool, ToolContext, ToolDefinition, ToolExposure, ToolInput, ToolOutput, ToolResult,
    runtime::ToolRuntime,
};
use async_trait::async_trait;
use backend::Backend;
use nanocodex_oai_api::{
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
    #[serde(default)]
    pub archives: std::collections::BTreeMap<String, String>,
}

impl ContextWindow {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            first_window_id: id.clone(),
            previous_window_id: None,
            context_window_id: id,
            window_number: 0,
            archives: Default::default(),
        }
    }

    fn from_history(history: &[ResponseItem], agent_name: &str) -> Option<Self> {
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
                if field("Agent name: ")?.as_str() != agent_name {
                    return None;
                }
                Some(Self {
                    first_window_id: field("First context window id: ")?,
                    previous_window_id: field("Previous context window id: "),
                    context_window_id: field("Current context window id: ")?,
                    window_number: field("Context window number: ")?.parse().ok()?,
                    archives: field("Archived windows: ")
                        .and_then(|text| serde_json::from_str(&text).ok())
                        .unwrap_or_default(),
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
    /// Creates controls, restoring context identity from durable history when present.
    pub fn new(
        host: Arc<dyn HistoryNotesHost>,
        session_id: String,
        thread_id: String,
        agent_name: String,
        history: &[ResponseItem],
    ) -> Self {
        let window =
            ContextWindow::from_history(history, &agent_name).unwrap_or_else(ContextWindow::new);
        Self {
            backend: Backend {
                host,
                session_id,
                agent_name,
                thread_id,
            },
            state: Arc::new(State {
                window: Mutex::new(window),
                requested: AtomicBool::new(false),
                remaining: AtomicU64::new(u64::MAX),
            }),
            budget: Arc::new(
                serde_json::from_str(include_str!("token_budget.json"))
                    .expect("pinned Codex token budget"),
            ),
        }
    }

    /// Commits the exact old window before the live conversation is reset.
    pub async fn archive(&self, history: &[ResponseItem]) -> Result<String, String> {
        self.backend.archive(&self.window(), history).await
    }

    /// Associates only a committed successor with its immutable archived predecessor.
    pub fn record_archive(&self, window_id: String, archive_id: String) {
        self.state
            .window
            .lock()
            .expect("context window lock")
            .archives
            .insert(window_id, archive_id);
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
                    context: self.clone(),
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
    /// Advances identity without resetting tools, environment, or saved notes.
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
        if let Some(window) = ContextWindow::from_history(history, self.agent_name()) {
            *self
                .state
                .window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = window;
        }
    }
    /// Whether history contains an identity belonging to this agent.
    pub fn has_saved_window(&self, history: &[ResponseItem]) -> bool {
        ContextWindow::from_history(history, self.agent_name()).is_some()
    }
    /// Produces the canonical context-window and model guidance messages.
    pub async fn initial_context(&self) -> Result<Vec<ResponseItem>, String> {
        let window = self.window();
        let mut body = format!(
            "Agent name: {}\nFirst context window id: {}\nCurrent context window id: {}\nContext window number: {}",
            self.agent_name(),
            window.first_window_id,
            window.context_window_id,
            window.window_number
        );
        body.push_str(&format!(
            "\nArchived windows: {}",
            serde_json::to_string(&window.archives).expect("archive index")
        ));
        if let Some(previous) = window.previous_window_id {
            body.push_str(&format!("\nPrevious context window id: {previous}"));
        }
        let mut hint = self.backend.hint().await?;
        hint.truncate(hint.floor_char_boundary(4000));
        if !hint.is_empty() {
            body.push('\n');
            body.push_str(&hint);
        }
        Ok(vec![
            developer(format!("<context_window>\n{body}\n</context_window>")),
            developer(format!(
                "<context_window_guidance>\n{}\n</context_window_guidance>",
                self.budget.guidance_message
            )),
        ])
    }
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
    context: ContextManagement,
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
                .context
                .backend
                .call(
                    self.action,
                    arguments,
                    &self.context.window(),
                    context.history(),
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
    let media = result
        .as_object_mut()
        .and_then(|object| object.remove("media"));
    let mut content = vec![ToolOutputContent::InputText {
        text: result.to_string(),
    }];
    if let Some(Value::Array(parts)) = media {
        for mut part in parts {
            if part["type"] == "input_image" && part["detail"].is_null() {
                part["detail"] = json!("auto");
            }
            match serde_json::from_value(part) {
                Ok(part) => content.push(part),
                Err(error) => return ToolOutput::error(error.to_string()),
            }
        }
    }
    ToolOutput::content(content)
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn a_fork_cannot_restore_its_parents_context_window() {
        let history = [ResponseItem::message(MessageRole::Developer, [ContentItem::InputText {
            text: "<context_window>\nAgent name: /root\nFirst context window id: first\nCurrent context window id: current\nContext window number: 2\n</context_window>".into(),
        }])];
        assert_eq!(
            ContextWindow::from_history(&history, "/root")
                .unwrap()
                .window_number,
            2
        );
        assert!(ContextWindow::from_history(&history, "/root/reviewer").is_none());
        let workspace = tempfile::tempdir().unwrap();
        let child = ContextManagement::new(
            files::host(workspace.path()),
            "session".into(),
            "child-thread".into(),
            "/root/reviewer".into(),
            &history,
        );
        assert_ne!(child.window().context_window_id, "current");
        assert!(!child.has_saved_window(&history));
        child.restore(&history);
        assert_eq!(child.window().window_number, 0);
    }
}
