use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use js_sys::Promise;
use nanocodex::{
    AgentEvents, AgentSessionContext, DurableAgentExt, Model, Nanocodex as RustNanocodex,
    NanocodexError, OpenAi, PromptRoute, ReasoningMode, Thinking, Turn, TurnControl, TurnResult,
    agent::{
        ExecutionEnvironment, PromptRequest,
        durability::{
            JournalStore, OwnedJournal, OwnerId, OwnerToken, StoreError, StoreFuture, StoredBatch,
            StoredJournal,
        },
        input::{Prompt, UserInput},
        session::{SessionId, SessionSnapshot},
    },
    oai::auth::{
        ChatGptCredentialSeed, ChatGptLoginStatus, ChatGptSubscription, ChatGptSubscriptionHost,
        SubscriptionCommit, SubscriptionFuture, SubscriptionHostError, SubscriptionHttpRequest,
        SubscriptionHttpResponse, SubscriptionStoreValue,
    },
    oai::responses::{ContentItem, MessageRole, ResponseItem},
    tools::{
        ToolContext, ToolDefinition, ToolInput, ToolOutput,
        contract::ToolOutputWire,
        hosted::{
            CodeModeExecution, CodeModeHost, CodeModeHostError, CodeModeObserver, CodeModeUpdate,
            HostFuture, HostedToolMode, HostedTools, NestedToolCall,
        },
        standard::StandardTool,
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use nanocodex_subagents::{
    AgentId as SubagentId, AgentStatus as SubagentStatus, AgentTask, AgentUpdate as SubagentUpdate,
    Registry as SubagentRegistry, ScopedAgentUpdate, SubagentControl, start_agent,
};
use nanocodex_voice_protocol::{
    BrowserVoiceEffects, BrowserVoiceProtocol, REALTIME_END_INSTRUCTIONS,
    REALTIME_START_INSTRUCTIONS, TranscriptEntry, VoiceHistoryEntry, build_browser_startup_context,
    build_chatgpt_realtime_call, decode_chatgpt_realtime_call, preferred_physical_input,
    realtime_delegation, realtime_message_requires_agent_admission, realtime_tail_delegation,
    valid_realtime_call_id,
};

mod transport;

use transport::JavaScriptResponsesHost;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = emitEvent)]
    fn host_emit_event(session_id: &str, event: &str, encoded_bytes: u32);

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = executeCode)]
    fn host_execute_code(source: &str, session_id: &str, call_id: &str)
    -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = nextCodeUpdate)]
    fn host_next_code_update(session_id: &str, call_id: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = executeTool)]
    fn host_execute_tool(
        name: &str,
        input: &str,
        session_id: &str,
        call_id: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = cancelCode)]
    fn host_cancel_code(session_id: &str);

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = toolMode)]
    fn host_tool_mode(definition_host_id: u32, session_id: &str) -> String;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = toolDefinitions)]
    fn host_tool_definitions(definition_host_id: u32, session_id: &str) -> Result<String, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = durabilityAcquire)]
    fn host_durability_acquire(
        route_id: &str,
        journal_id: &str,
        owner_id: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = durabilityAppend)]
    fn host_durability_append(
        route_id: &str,
        journal_id: &str,
        owner_id: &str,
        fence: &str,
        expected_revision: &str,
        payload: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = durabilityCompact)]
    fn host_durability_compact(
        route_id: &str,
        journal_id: &str,
        owner_id: &str,
        fence: &str,
        expected_revision: &str,
        payload: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = readWorkspaceFile)]
    fn host_read_workspace_file(path: &str, session_id: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = listWorkspace)]
    fn host_list_workspace(path: &str, session_id: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = writeWorkspaceFile)]
    fn host_write_workspace_file(
        path: &str,
        contents: &js_sys::Uint8Array,
        session_id: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = removeWorkspaceFile)]
    fn host_remove_workspace_file(path: &str, session_id: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = subscriptionLoad)]
    fn host_subscription_load(subscription_id: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = subscriptionCompareAndSwap)]
    fn host_subscription_compare_and_swap(
        subscription_id: &str,
        expected_revision: &str,
        payload: &str,
    ) -> Result<Promise, JsValue>;

    #[wasm_bindgen(catch, js_namespace = ["globalThis", "nanocodexHost"], js_name = subscriptionRequest)]
    fn host_subscription_request(subscription_id: &str, request: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = bindSubagentSession)]
    fn host_bind_subagent_session(root_session_id: &str, session_id: &str, context_json: &str);

    #[wasm_bindgen(js_namespace = ["globalThis", "nanocodexHost"], js_name = releaseSubagentSession)]
    fn host_release_subagent_session(session_id: &str);
}

struct JavaScriptSubscriptionHost {
    subscription_id: String,
}

#[derive(Deserialize)]
struct JavaScriptSubscriptionValue {
    revision: String,
    #[serde(default)]
    payload: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum JavaScriptSubscriptionCommit {
    Committed { revision: String },
    Conflict { actual_revision: String },
}

#[derive(Deserialize)]
struct JavaScriptSubscriptionResponse {
    status: u16,
    body: String,
}

#[derive(Serialize)]
struct WasmAgentSessionContext<'a> {
    workspace: &'a str,
    history: &'a [nanocodex::oai::responses::ResponseItem],
}

#[derive(Deserialize)]
struct WasmOwnedAgentSessionContext {
    workspace: String,
    history: Vec<ResponseItem>,
}

#[derive(Deserialize)]
struct WasmRealtimeTranscriptEntry {
    role: String,
    text: String,
}

impl ChatGptSubscriptionHost for JavaScriptSubscriptionHost {
    fn load<'a>(
        &'a self,
        _key: &'a str,
    ) -> SubscriptionFuture<'a, Result<SubscriptionStoreValue, SubscriptionHostError>> {
        Box::pin(async move {
            let promise =
                host_subscription_load(&self.subscription_id).map_err(subscription_host_error)?;
            let stored: JavaScriptSubscriptionValue = await_subscription_json(promise).await?;
            Ok(SubscriptionStoreValue {
                revision: parse_subscription_revision(&stored.revision)?,
                payload: stored.payload,
            })
        })
    }

    fn compare_and_swap<'a>(
        &'a self,
        _key: &'a str,
        expected_revision: u64,
        payload: &'a str,
    ) -> SubscriptionFuture<'a, Result<SubscriptionCommit, SubscriptionHostError>> {
        Box::pin(async move {
            let expected = expected_revision.to_string();
            let promise =
                host_subscription_compare_and_swap(&self.subscription_id, &expected, payload)
                    .map_err(subscription_host_error)?;
            match await_subscription_json::<JavaScriptSubscriptionCommit>(promise).await? {
                JavaScriptSubscriptionCommit::Committed { revision } => Ok(
                    SubscriptionCommit::Committed(parse_subscription_revision(&revision)?),
                ),
                JavaScriptSubscriptionCommit::Conflict { actual_revision } => Ok(
                    SubscriptionCommit::Conflict(parse_subscription_revision(&actual_revision)?),
                ),
            }
        })
    }

    fn request<'a>(
        &'a self,
        request: SubscriptionHttpRequest,
    ) -> SubscriptionFuture<'a, Result<SubscriptionHttpResponse, SubscriptionHostError>> {
        Box::pin(async move {
            let encoded = serde_json::json!({
                "method": request.method(),
                "url": request.url(),
                "contentType": request.content_type(),
                "body": request.body(),
                "maxResponseBytes": request.max_response_bytes(),
            })
            .to_string();
            let promise = host_subscription_request(&self.subscription_id, &encoded)
                .map_err(subscription_host_error)?;
            let response: JavaScriptSubscriptionResponse = await_subscription_json(promise).await?;
            Ok(SubscriptionHttpResponse {
                status: response.status,
                body: response.body,
            })
        })
    }
}

async fn await_subscription_json<T: for<'de> Deserialize<'de>>(
    promise: Promise,
) -> Result<T, SubscriptionHostError> {
    let value = JsFuture::from(promise)
        .await
        .map_err(subscription_host_error)?;
    let encoded = value.as_string().ok_or_else(|| {
        SubscriptionHostError::new("JavaScript subscription host returned a non-string")
    })?;
    serde_json::from_str(&encoded).map_err(|error| {
        SubscriptionHostError::new(format!(
            "JavaScript subscription host returned invalid JSON: {error}"
        ))
    })
}

fn parse_subscription_revision(revision: &str) -> Result<u64, SubscriptionHostError> {
    revision.parse().map_err(|error| {
        SubscriptionHostError::new(format!("invalid subscription revision: {error}"))
    })
}

fn subscription_host_error(error: JsValue) -> SubscriptionHostError {
    SubscriptionHostError::new(host_error_message(&error))
}

struct JavaScriptDurabilityStore {
    route_id: String,
}

#[derive(Deserialize)]
struct JavaScriptOwnedJournal {
    owner_id: String,
    fence: String,
    revision: String,
    batches: Vec<JavaScriptStoredBatch>,
}

#[derive(Deserialize)]
struct JavaScriptStoredBatch {
    revision: String,
    payload: String,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum JavaScriptAppendResult {
    Appended { revision: String },
    Conflict { actual_revision: String },
    Fenced,
    NotCommitted { message: String },
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum JavaScriptCompactResult {
    Compacted { revision: String },
    Conflict { actual_revision: String },
    Fenced,
    NotCommitted { message: String },
}

impl JournalStore for JavaScriptDurabilityStore {
    fn acquire_owner<'a>(
        &'a mut self,
        journal_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedJournal, StoreError>> {
        Box::pin(async move {
            let promise = host_durability_acquire(&self.route_id, journal_id, owner_id.as_str())
                .map_err(|error| StoreError::Backend(host_error_message(&error)))?;
            let value = JsFuture::from(promise)
                .await
                .map_err(|error| StoreError::Backend(host_error_message(&error)))?;
            let encoded = value.as_string().ok_or_else(|| {
                StoreError::Backend(
                    "JavaScript durability acquire returned a non-string".to_owned(),
                )
            })?;
            let stored =
                serde_json::from_str::<JavaScriptOwnedJournal>(&encoded).map_err(|error| {
                    StoreError::Backend(format!("invalid durability acquire: {error}"))
                })?;
            if stored.owner_id != owner_id.as_str() {
                return Err(StoreError::Backend(
                    "JavaScript durability acquire returned a different owner ID".to_owned(),
                ));
            }
            Ok(OwnedJournal {
                owner: OwnerToken::new(owner_id, parse_revision(&stored.fence)?),
                journal: StoredJournal {
                    revision: parse_revision(&stored.revision)?,
                    batches: stored
                        .batches
                        .into_iter()
                        .map(|batch| {
                            Ok(StoredBatch {
                                revision: parse_revision(&batch.revision)?,
                                payload: batch.payload,
                            })
                        })
                        .collect::<Result<_, StoreError>>()?,
                },
            })
        })
    }

    fn append<'a>(
        &'a mut self,
        journal_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let fence = owner.fence().to_string();
            let expected = expected_revision.to_string();
            let promise = host_durability_append(
                &self.route_id,
                journal_id,
                owner.owner_id().as_str(),
                &fence,
                &expected,
                payload,
            )
            .map_err(|error| StoreError::Backend(host_error_message(&error)))?;
            let value = JsFuture::from(promise)
                .await
                .map_err(|error| StoreError::Backend(host_error_message(&error)))?;
            let encoded = value.as_string().ok_or_else(|| {
                StoreError::Backend("JavaScript durability append returned a non-string".to_owned())
            })?;
            match serde_json::from_str::<JavaScriptAppendResult>(&encoded).map_err(|error| {
                StoreError::Backend(format!("invalid durability append result: {error}"))
            })? {
                JavaScriptAppendResult::Appended { revision } => parse_revision(&revision),
                JavaScriptAppendResult::Conflict { actual_revision } => Err(StoreError::Conflict {
                    expected: expected_revision,
                    actual: parse_revision(&actual_revision)?,
                }),
                JavaScriptAppendResult::Fenced => Err(StoreError::Fenced),
                JavaScriptAppendResult::NotCommitted { message } => {
                    Err(StoreError::NotCommitted(message))
                }
            }
        })
    }

    fn compact<'a>(
        &'a mut self,
        journal_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let fence = owner.fence().to_string();
            let expected = expected_revision.to_string();
            let promise = host_durability_compact(
                &self.route_id,
                journal_id,
                owner.owner_id().as_str(),
                &fence,
                &expected,
                payload,
            )
            .map_err(|error| StoreError::Backend(host_error_message(&error)))?;
            let value = JsFuture::from(promise)
                .await
                .map_err(|error| StoreError::Backend(host_error_message(&error)))?;
            let encoded = value.as_string().ok_or_else(|| {
                StoreError::Backend(
                    "JavaScript durability compact returned a non-string".to_owned(),
                )
            })?;
            match serde_json::from_str::<JavaScriptCompactResult>(&encoded).map_err(|error| {
                StoreError::Backend(format!("invalid durability compact result: {error}"))
            })? {
                JavaScriptCompactResult::Compacted { revision } => parse_revision(&revision),
                JavaScriptCompactResult::Conflict { actual_revision } => {
                    Err(StoreError::Conflict {
                        expected: expected_revision,
                        actual: parse_revision(&actual_revision)?,
                    })
                }
                JavaScriptCompactResult::Fenced => Err(StoreError::Fenced),
                JavaScriptCompactResult::NotCommitted { message } => {
                    Err(StoreError::NotCommitted(message))
                }
            }
        })
    }
}

struct JavaScriptCodeModeHost {
    definition_host_id: u32,
    mode: HostedToolMode,
}

#[derive(Deserialize)]
struct JavaScriptNestedCallStarted {
    call_id: String,
    name: String,
    input: serde_json::Value,
}

#[derive(Deserialize)]
struct JavaScriptNestedCallCompleted {
    call: NestedToolCall,
}

impl JavaScriptCodeModeHost {
    fn new(definition_host_id: u32) -> Self {
        Self {
            definition_host_id,
            mode: if host_tool_mode(definition_host_id, "") == "direct" {
                HostedToolMode::Direct
            } else {
                HostedToolMode::Code
            },
        }
    }
}

impl CodeModeHost for JavaScriptCodeModeHost {
    fn tool_mode(&self) -> HostedToolMode {
        self.mode
    }

    fn tool_definitions(&self, session_id: &str) -> Result<Vec<ToolDefinition>, CodeModeHostError> {
        let encoded = host_tool_definitions(self.definition_host_id, session_id)
            .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
        let mut definitions =
            serde_json::from_str::<Vec<ToolDefinition>>(&encoded).map_err(|error| {
                CodeModeHostError::new(format!(
                    "JavaScript Code Mode host returned invalid tool definitions: {error}"
                ))
            })?;
        for definition in &mut definitions {
            let standard = match definition.name() {
                name if name == StandardTool::WriteStdin.name() => Some(StandardTool::WriteStdin),
                name if name == StandardTool::UpdatePlan.name() => Some(StandardTool::UpdatePlan),
                name if name == StandardTool::ApplyPatch.name() => Some(StandardTool::ApplyPatch),
                name if name == StandardTool::ViewImage.name() => Some(StandardTool::ViewImage),
                _ => None,
            };
            if let Some(standard) = standard {
                *definition = standard.definition();
            }
        }
        Ok(definitions)
    }

    fn execute<'a>(
        &'a self,
        source: &'a str,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>> {
        Box::pin(execute_javascript_code(source, context, None))
    }

    fn execute_with_updates<'a>(
        &'a self,
        source: &'a str,
        context: ToolContext<'a>,
        observer: &'a mut dyn CodeModeObserver,
    ) -> HostFuture<'a, Result<CodeModeExecution, CodeModeHostError>> {
        Box::pin(execute_javascript_code(source, context, Some(observer)))
    }

    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        input: ToolInput,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, Result<ToolOutput, CodeModeHostError>> {
        Box::pin(async move {
            if name == StandardTool::ApplyPatch.name() {
                return execute_browser_apply_patch(input, context.session_id()).await;
            }
            let input = match input {
                ToolInput::Function(input) => input.get().to_owned(),
                ToolInput::Freeform(input) => serde_json::to_string(&input).map_err(|error| {
                    CodeModeHostError::new(format!("failed to encode hosted tool input: {error}"))
                })?,
            };
            let promise = host_execute_tool(name, &input, context.session_id(), context.call_id())
                .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
            let value = JsFuture::from(promise)
                .await
                .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
            let encoded = value.as_string().ok_or_else(|| {
                CodeModeHostError::new("JavaScript tool host returned a non-string result")
            })?;
            let wire = serde_json::from_str::<ToolOutputWire>(&encoded).map_err(|error| {
                CodeModeHostError::new(format!(
                    "JavaScript tool host returned invalid execution JSON: {error}"
                ))
            })?;
            ToolOutput::from_wire(wire).map_err(|error| {
                CodeModeHostError::new(format!("JavaScript tool result was invalid: {error}"))
            })
        })
    }

    fn cancel<'a>(&'a self, session_id: &'a str) -> HostFuture<'a, Result<(), CodeModeHostError>> {
        Box::pin(async move {
            host_cancel_code(session_id);
            Ok(())
        })
    }
}

async fn execute_javascript_code(
    source: &str,
    context: ToolContext<'_>,
    mut observer: Option<&mut dyn CodeModeObserver>,
) -> Result<CodeModeExecution, CodeModeHostError> {
    let execution = host_execute_code(source, context.session_id(), context.call_id())
        .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
    loop {
        let update = host_next_code_update(context.session_id(), context.call_id())
            .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
        let value = JsFuture::from(update)
            .await
            .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
        if value.is_null() || value.is_undefined() {
            break;
        }
        let encoded = value.as_string().ok_or_else(|| {
            CodeModeHostError::new("JavaScript Code Mode host returned a non-string nested update")
        })?;
        let value = serde_json::from_str::<serde_json::Value>(&encoded).map_err(|error| {
            CodeModeHostError::new(format!(
                "JavaScript Code Mode host returned invalid nested update JSON: {error}"
            ))
        })?;
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("nested_call_started") => {
                let update = serde_json::from_value::<JavaScriptNestedCallStarted>(value).map_err(
                    |error| {
                        CodeModeHostError::new(format!(
                            "JavaScript Code Mode host returned invalid nested start: {error}"
                        ))
                    },
                )?;
                if let Some(observer) = observer.as_deref_mut() {
                    observer.update(CodeModeUpdate::NestedCallStarted {
                        call_id: &update.call_id,
                        name: &update.name,
                        input: &update.input,
                    });
                }
            }
            Some("nested_call_completed") => {
                let update = serde_json::from_value::<JavaScriptNestedCallCompleted>(value)
                    .map_err(|error| {
                        CodeModeHostError::new(format!(
                            "JavaScript Code Mode host returned invalid nested completion: {error}"
                        ))
                    })?;
                if let Some(observer) = observer.as_deref_mut() {
                    observer.update(CodeModeUpdate::NestedCallCompleted(&update.call));
                }
            }
            _ => {
                return Err(CodeModeHostError::new(
                    "JavaScript Code Mode host returned an unknown nested update",
                ));
            }
        }
    }
    let value = JsFuture::from(execution)
        .await
        .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
    decode_code_execution(value)
}

fn decode_code_execution(value: JsValue) -> Result<CodeModeExecution, CodeModeHostError> {
    let encoded = value.as_string().ok_or_else(|| {
        CodeModeHostError::new("JavaScript Code Mode host returned a non-string result")
    })?;
    serde_json::from_str(&encoded).map_err(|error| {
        CodeModeHostError::new(format!(
            "JavaScript Code Mode host returned invalid execution JSON: {error}"
        ))
    })
}

async fn execute_browser_apply_patch(
    input: ToolInput,
    session_id: &str,
) -> Result<ToolOutput, CodeModeHostError> {
    let patch = input
        .into_freeform()
        .map_err(|error| CodeModeHostError::new(format!("invalid apply_patch input: {error}")))?;
    let summary = apply_browser_patch_plan(&patch, session_id).await?;
    Ok(ToolOutput::text(summary).with_structured_result(serde_json::json!({})))
}

async fn apply_browser_patch_plan(
    patch: &str,
    session_id: &str,
) -> Result<String, CodeModeHostError> {
    use nanocodex::tools::apply_patch::{PatchOperation, plan, required_files};

    let mut files = HashMap::new();
    for path in required_files(patch).map_err(CodeModeHostError::new)? {
        let display = path.to_string_lossy().into_owned();
        let promise = host_read_workspace_file(&display, session_id)
            .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
        let value = JsFuture::from(promise)
            .await
            .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
        if !value.is_instance_of::<js_sys::Uint8Array>() {
            return Err(CodeModeHostError::new(format!(
                "browser workspace returned non-byte data for {display}"
            )));
        }
        let contents =
            String::from_utf8(js_sys::Uint8Array::new(&value).to_vec()).map_err(|error| {
                CodeModeHostError::new(format!(
                    "browser workspace returned non-UTF-8 data for {display}: {error}"
                ))
            })?;
        files.insert(PathBuf::from(display), contents);
    }
    let plan = plan(patch, &files).map_err(CodeModeHostError::new)?;
    for operation in plan.operations() {
        let promise = match operation {
            PatchOperation::Write { path, contents } => {
                let bytes = js_sys::Uint8Array::from(contents.as_bytes());
                host_write_workspace_file(&path.to_string_lossy(), &bytes, session_id)
            }
            PatchOperation::Delete { path } => {
                host_remove_workspace_file(&path.to_string_lossy(), session_id)
            }
        }
        .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
        JsFuture::from(promise)
            .await
            .map_err(|error| CodeModeHostError::new(host_error_message(&error)))?;
    }
    Ok(plan.summary().to_owned())
}

/// Applies a browser-workspace patch through the canonical Rust planner.
///
/// The browser host uses this internal binding for nested Code Mode calls so
/// they share the direct `apply_patch` tool's verification and mutation path.
#[wasm_bindgen(js_name = applyBrowserPatch)]
pub async fn apply_browser_patch(patch: &str, session_id: &str) -> Result<String, JsValue> {
    apply_browser_patch_plan(patch, session_id)
        .await
        .map_err(js_error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WasmConfig {
    api_key: String,
    host_definition_id: u32,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default = "default_thinking")]
    thinking: String,
    #[serde(default = "default_reasoning_mode")]
    reasoning_mode: String,
    #[serde(default)]
    fast_mode: bool,
    #[serde(default)]
    websocket_warmup: bool,
    #[serde(default)]
    websocket_url: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    execution_environment: Option<WasmExecutionEnvironment>,
    #[serde(default)]
    resume: Option<SessionSnapshot>,
    #[serde(default)]
    durability_id: Option<String>,
    #[serde(default)]
    durability_host_id: Option<String>,
    #[serde(default)]
    terminal_receipt_retention: Option<usize>,
    #[serde(default)]
    subagents: Option<WasmSubagentsConfig>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct WasmSubagentsConfig {
    #[serde(default = "default_max_subagents")]
    max_concurrency: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WasmExecutionEnvironment {
    current_date: String,
    timezone: String,
    #[serde(default)]
    project_instructions: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WasmSubscriptionConfig {
    id: String,
    #[serde(default)]
    issuer: Option<String>,
    #[serde(default)]
    seed: Option<WasmSubscriptionSeed>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WasmSubscriptionSeed {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    account_id: String,
    #[serde(default)]
    fedramp: bool,
}

/// JavaScript binding over the Rust-owned hosted ChatGPT credential lifecycle.
#[wasm_bindgen(js_name = ChatGptSubscription)]
pub struct WasmChatGptSubscription {
    inner: ChatGptSubscription,
}

#[wasm_bindgen(js_class = ChatGptSubscription)]
impl WasmChatGptSubscription {
    /// Opens a subscription over the currently registered generic host capabilities.
    #[wasm_bindgen(js_name = open)]
    pub async fn open(config_json: &str) -> Result<Self, JsValue> {
        let config = serde_json::from_str::<WasmSubscriptionConfig>(config_json)
            .map_err(|error| js_error(format!("invalid ChatGPT subscription config: {error}")))?;
        let seed = config.seed.map(|seed| {
            ChatGptCredentialSeed::new(
                seed.access_token,
                seed.refresh_token,
                seed.account_id,
                seed.fedramp,
            )
        });
        let host = JavaScriptSubscriptionHost {
            subscription_id: config.id.clone(),
        };
        let inner = if let Some(issuer) = config.issuer {
            ChatGptSubscription::open_with_issuer(host, config.id, seed, issuer).await
        } else {
            ChatGptSubscription::open(host, config.id, seed).await
        }
        .map_err(js_error)?;
        Ok(Self { inner })
    }

    /// Starts a ChatGPT device login and returns public pending state as JSON.
    #[wasm_bindgen(js_name = startLogin)]
    pub async fn start_login(&self) -> Result<String, JsValue> {
        encode_login_status(self.inner.start_login().await)
    }

    /// Polls device login and returns public state as JSON.
    pub async fn status(&self) -> Result<String, JsValue> {
        encode_login_status(self.inner.status().await)
    }

    /// Resolves one credential generation for a host-owned outbound request.
    pub async fn credential(&self) -> Result<String, JsValue> {
        encode_subscription_credential(self.inner.credential().await)
    }

    /// Refreshes a rejected generation and returns the credential now current.
    pub async fn recover(&self, rejected_revision: &str) -> Result<String, JsValue> {
        let revision = rejected_revision
            .parse::<u64>()
            .map_err(|error| js_error(format!("invalid credential revision: {error}")))?;
        encode_subscription_credential(self.inner.recover(revision).await)
    }

    /// Clears the persisted credential and pending login.
    pub async fn logout(&self) -> Result<(), JsValue> {
        self.inner.logout().await.map_err(js_error)
    }
}

fn encode_login_status<E: ToString>(
    status: Result<ChatGptLoginStatus, E>,
) -> Result<String, JsValue> {
    serde_json::to_string(&status.map_err(js_error)?).map_err(js_error)
}

fn encode_subscription_credential(
    credential: Result<nanocodex::oai::auth::ChatGptCredential, impl ToString>,
) -> Result<String, JsValue> {
    let credential = credential.map_err(js_error)?;
    Ok(serde_json::json!({
        "kind": "chatgpt",
        "accessToken": credential.access_token(),
        "accountId": credential.account_id(),
        "fedramp": credential.is_fedramp(),
        "revision": credential.revision().to_string(),
    })
    .to_string())
}

/// JavaScript binding over the shared Rust agent lifecycle.
#[wasm_bindgen(js_name = Nanocodex)]
pub struct WasmNanocodex {
    inner: RustNanocodex,
    subagents: Option<WasmSubagents>,
    event_forwarding: Rc<Cell<bool>>,
}

#[derive(Clone)]
struct WasmSubagents {
    control: SubagentControl,
    registry: Arc<SubagentRegistry>,
    parents: Arc<Mutex<HashMap<String, nanocodex::agent::AgentHandle>>>,
    sessions: Rc<RefCell<HashMap<(String, SubagentId), String>>>,
    event_forwarders: Rc<Cell<usize>>,
}

impl WasmSubagents {
    fn new(
        control: SubagentControl,
        registry: Arc<SubagentRegistry>,
        parents: Arc<Mutex<HashMap<String, nanocodex::agent::AgentHandle>>>,
        updates: tokio::sync::mpsc::UnboundedReceiver<ScopedAgentUpdate>,
    ) -> Self {
        let sessions = Rc::new(RefCell::new(HashMap::new()));
        let event_forwarders = Rc::new(Cell::new(0));
        forward_subagent_updates(updates, Rc::clone(&sessions), Rc::clone(&event_forwarders));
        Self {
            control,
            registry,
            parents,
            sessions,
            event_forwarders,
        }
    }

    fn set_event_forwarding(&self, enabled: bool) {
        let active = self.event_forwarders.get();
        self.event_forwarders.set(if enabled {
            active.saturating_add(1)
        } else {
            active.saturating_sub(1)
        });
    }

    async fn close_all(&self, root_session_id: &str) -> std::io::Result<()> {
        self.control.close_all(root_session_id).await?;
        release_subagent_scope(&self.sessions, root_session_id);
        match self.parents.lock() {
            Ok(mut parents) => {
                parents.remove(root_session_id);
            }
            Err(poisoned) => {
                poisoned.into_inner().remove(root_session_id);
            }
        }
        Ok(())
    }
}

#[wasm_bindgen(js_class = Nanocodex)]
impl WasmNanocodex {
    /// Builds an agent from its JavaScript JSON configuration.
    ///
    /// # Errors
    ///
    /// Throws when the JSON or agent policy is invalid.
    pub async fn create(config_json: &str) -> Result<Self, JsValue> {
        let config = serde_json::from_str::<WasmConfig>(config_json)
            .map_err(|error| js_error(format!("invalid Nanocodex configuration: {error}")))?;
        let auth = nanocodex::oai::auth::OpenAiAuth::api_key(config.api_key.clone());
        Self::create_with_auth(config, auth).await
    }

    /// Builds an agent whose ChatGPT credential lifecycle is owned by Rust.
    #[wasm_bindgen(js_name = createWithChatGpt)]
    pub async fn create_with_chat_gpt(
        config_json: &str,
        subscription: &WasmChatGptSubscription,
    ) -> Result<Self, JsValue> {
        let config = serde_json::from_str::<WasmConfig>(config_json)
            .map_err(|error| js_error(format!("invalid Nanocodex configuration: {error}")))?;
        let auth = subscription.inner.authorization().await.map_err(js_error)?;
        Self::create_with_auth(config, auth).await
    }

    async fn create_with_auth(
        config: WasmConfig,
        auth: nanocodex::oai::auth::OpenAiAuth,
    ) -> Result<Self, JsValue> {
        validate(&config)?;

        let model = config.model.parse::<Model>().map_err(js_error)?;
        let thinking = config.thinking.parse::<Thinking>().map_err(js_error)?;
        let reasoning_mode = config
            .reasoning_mode
            .parse::<ReasoningMode>()
            .map_err(js_error)?;
        let mut openai = OpenAi::builder(auth)
            .model(model)
            .thinking(thinking)
            .reasoning_mode(reasoning_mode)
            .fast_mode(config.fast_mode)
            .websocket_warmup(config.websocket_warmup);
        if let Some(websocket_url) = config.websocket_url {
            openai = openai.websocket_url(websocket_url);
        }
        if let Some(api_base_url) = config.api_base_url {
            openai = openai.api_base_url(api_base_url);
        }
        let openai = openai
            .host_transport(JavaScriptResponsesHost)
            .build()
            .map_err(js_error)?;
        let hosted_tools = HostedTools::new(JavaScriptCodeModeHost::new(config.host_definition_id));
        let (mut builder, subagents) = if let Some(subagents) = config.subagents {
            let (registry, control, updates) =
                nanocodex_subagents::channel(subagents.max_concurrency);
            let parents = Arc::new(Mutex::new(HashMap::new()));
            let tool_registry = Arc::clone(&registry);
            let tool_parents = Arc::clone(&parents);
            (
                RustNanocodex::builder(openai).tools_factory(move |agent| {
                    let mut parents = match tool_parents.lock() {
                        Ok(parents) => parents,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    parents.insert(agent.session_id().to_string(), agent.clone());
                    drop(parents);
                    nanocodex_subagents::install_tools(
                        hosted_tools.clone(),
                        agent,
                        tool_registry.clone(),
                    )
                }),
                Some(WasmSubagents::new(control, registry, parents, updates)),
            )
        } else {
            (RustNanocodex::builder(openai).tools(hosted_tools), None)
        };
        if let Some(instructions) = config.instructions {
            builder = builder.instructions(instructions);
        }
        if let Some(session_id) = config.session_id {
            builder = builder.session_id(session_id.parse::<SessionId>().map_err(js_error)?);
        }
        if let Some(workspace) = config.workspace {
            builder = builder.workspace(workspace);
        }
        if let Some(configured) = config.execution_environment {
            let mut environment =
                ExecutionEnvironment::new(configured.current_date, configured.timezone);
            if let Some(project_instructions) = configured.project_instructions {
                environment = environment.project_instructions(project_instructions);
            }
            builder = builder.execution_environment(environment);
        }
        if let Some(resume) = config.resume {
            builder = builder.resume(resume);
        }
        if let (Some(route_id), Some(journal_id)) =
            (config.durability_host_id, config.durability_id)
        {
            let store = JavaScriptDurabilityStore { route_id };
            let journal = if let Some(limit) = config.terminal_receipt_retention {
                if !(1..=4_096).contains(&limit) {
                    return Err(js_error(
                        "terminal_receipt_retention must be from 1 through 4096",
                    ));
                }
                nanocodex::agent::durability::DurableSession::open_with_terminal_receipt_limit(
                    store, journal_id, limit,
                )
                .await
            } else {
                nanocodex::agent::durability::DurableSession::open(store, journal_id).await
            }
            .map_err(js_error)?;
            builder = builder.durability(journal).await.map_err(js_error)?;
        }
        let (inner, events) = builder.build().map_err(js_error)?;
        Ok(Self::from_parts(inner, events, subagents))
    }

    /// Returns the stable `UUIDv7` session identity.
    #[wasm_bindgen(getter, js_name = sessionId)]
    #[must_use]
    pub fn session_id(&self) -> String {
        self.inner.session_id().to_string()
    }

    /// Enables or disables the optional JavaScript event crossing for this handle.
    #[wasm_bindgen(js_name = setEventForwarding)]
    pub fn set_event_forwarding(&self, enabled: bool) {
        if self.event_forwarding.replace(enabled) != enabled
            && let Some(subagents) = &self.subagents
        {
            subagents.set_event_forwarding(enabled);
        }
    }

    /// Accepts a text prompt and returns its independently awaitable turn.
    ///
    /// # Errors
    ///
    /// Throws when the prompt is empty.
    pub fn prompt(
        &self,
        instruction: &str,
        operation_id: Option<String>,
    ) -> Result<WasmTurn, JsValue> {
        validate_operation_id(operation_id.as_deref())?;
        if instruction.trim().is_empty() {
            return Err(js_error("prompt instruction must not be empty"));
        }
        Ok(WasmTurn::accept(
            self.inner.clone(),
            Prompt::new(instruction),
            operation_id,
        ))
    }

    /// Accepts browser-safe multimodal input encoded as JSON.
    ///
    /// # Errors
    ///
    /// Throws for malformed, empty, or local-filesystem input.
    #[wasm_bindgen(js_name = promptContent)]
    pub fn prompt_content(
        &self,
        content_json: &str,
        operation_id: Option<String>,
    ) -> Result<WasmTurn, JsValue> {
        validate_operation_id(operation_id.as_deref())?;
        Ok(WasmTurn::accept(
            self.inner.clone(),
            parse_browser_prompt(content_json)?,
            operation_id,
        ))
    }

    /// Atomically steers the active turn or starts a new independently awaitable turn.
    ///
    /// Returns `undefined` when the input was steered into an active turn.
    ///
    /// # Errors
    ///
    /// Rejects empty input, a full steering queue, or a stopped driver.
    #[wasm_bindgen(js_name = routePrompt)]
    pub async fn route_prompt(&self, instruction: &str) -> Result<Option<WasmTurn>, JsValue> {
        if instruction.trim().is_empty() {
            return Err(js_error("prompt instruction must not be empty"));
        }
        match self
            .inner
            .route_prompt(Prompt::new(instruction))
            .await
            .map_err(js_error)?
        {
            PromptRoute::Steered => Ok(None),
            PromptRoute::Started(turn) => Ok(Some(WasmTurn::started(turn))),
        }
    }

    /// Forks the latest safe committed model boundary.
    ///
    /// # Errors
    ///
    /// Rejects before the first safe boundary or after the driver stops.
    pub async fn fork(&self) -> Result<Self, JsValue> {
        let (inner, events) = self.inner.fork().await.map_err(js_error)?;
        Ok(Self::from_parts(inner, events, self.subagents.clone()))
    }

    /// Forks from an exact completed historical turn.
    ///
    /// # Errors
    ///
    /// Rejects if the result belongs to another agent or the driver stopped.
    #[wasm_bindgen(js_name = forkFrom)]
    pub async fn fork_from(&self, result: &WasmTurnResult) -> Result<Self, JsValue> {
        let (inner, events) = self
            .inner
            .fork_from(&result.inner)
            .await
            .map_err(js_error)?;
        Ok(Self::from_parts(inner, events, self.subagents.clone()))
    }

    /// Starts a clean sibling with the same private agent policy.
    ///
    /// # Errors
    ///
    /// Rejects after the driver stops.
    pub async fn spawn(&self) -> Result<Self, JsValue> {
        let (inner, events) = self.inner.spawn().await.map_err(js_error)?;
        Ok(Self::from_parts(inner, events, self.subagents.clone()))
    }

    /// Starts one canonical subagent from application code in the same task tree.
    ///
    /// # Errors
    ///
    /// Rejects malformed tasks, agents without subagent tools, or a stopped parent.
    #[wasm_bindgen(js_name = startSubagent)]
    pub async fn start_subagent(&self, task_json: &str) -> Result<String, JsValue> {
        let task = serde_json::from_str::<AgentTask>(task_json)
            .map_err(|error| js_error(format!("invalid subagent task: {error}")))?;
        let subagents = self
            .subagents
            .as_ref()
            .ok_or_else(|| js_error("subagents are not configured for this Agent"))?;
        let parents = match subagents.parents.lock() {
            Ok(parents) => parents,
            Err(poisoned) => poisoned.into_inner(),
        };
        let parent = parents
            .get(&self.inner.session_id().to_string())
            .cloned()
            .ok_or_else(|| js_error("subagent parent is not ready"))?;
        drop(parents);
        let report = start_agent(
            &parent,
            &subagents.registry,
            &self.inner.session_id().to_string(),
            task,
        )
        .await
        .map_err(js_error)?;
        serde_json::to_string(&report).map_err(js_error)
    }

    /// Changes the reasoning effort for subsequently accepted turns.
    ///
    /// # Errors
    ///
    /// Rejects an invalid effort or a stopped driver.
    #[wasm_bindgen(js_name = setThinking)]
    pub async fn set_thinking(&self, thinking: &str) -> Result<(), JsValue> {
        self.inner
            .set_thinking(thinking.parse::<Thinking>().map_err(js_error)?)
            .await
            .map_err(js_error)
    }

    /// Enables or disables priority processing for subsequently accepted turns.
    ///
    /// # Errors
    ///
    /// Rejects after the driver stops.
    #[wasm_bindgen(js_name = setFastMode)]
    pub async fn set_fast_mode(&self, enabled: bool) -> Result<(), JsValue> {
        self.inner.set_fast_mode(enabled).await.map_err(js_error)
    }

    /// Compacts retained history immediately without fabricating a user prompt.
    ///
    /// # Errors
    ///
    /// Throws when compaction or the agent driver fails.
    pub async fn compact(&self) -> Result<(), JsValue> {
        self.inner.compact().await.map_err(js_error)
    }

    /// Appends adapter-owned developer context at the next safe model boundary.
    ///
    /// Returns the complete read-only session context captured at that boundary.
    ///
    /// # Errors
    ///
    /// Rejects empty text or a stopped driver.
    #[wasm_bindgen(js_name = appendDeveloperMessage)]
    pub async fn append_developer_message(&self, text: &str) -> Result<String, JsValue> {
        append_developer_context(&self.inner, text).await
    }

    /// Returns complete read-only session context at the latest safe boundary.
    ///
    /// # Errors
    ///
    /// Rejects after the driver stops or when context serialization fails.
    pub async fn context(&self) -> Result<String, JsValue> {
        serialize_session_context(self.inner.context().await.map_err(js_error)?)
    }

    /// Starts the canonical Codex Realtime adapter lifecycle.
    ///
    /// # Errors
    ///
    /// Rejects when the agent driver has stopped or context serialization fails.
    #[wasm_bindgen(js_name = startRealtimeConversation)]
    pub async fn start_realtime_conversation(&self) -> Result<String, JsValue> {
        append_developer_context(&self.inner, REALTIME_START_INSTRUCTIONS).await
    }

    /// Ends the canonical Codex Realtime adapter lifecycle.
    ///
    /// # Errors
    ///
    /// Rejects when the agent driver has stopped or context serialization fails.
    #[wasm_bindgen(js_name = endRealtimeConversation)]
    pub async fn end_realtime_conversation(&self) -> Result<String, JsValue> {
        append_developer_context(&self.inner, REALTIME_END_INSTRUCTIONS).await
    }

    /// Formats one structured Realtime delegation using canonical Codex markers.
    ///
    /// # Errors
    ///
    /// Rejects malformed transcript JSON.
    #[wasm_bindgen(js_name = realtimeDelegation)]
    pub fn realtime_delegation(&self, input: &str, transcript: &str) -> Result<String, JsValue> {
        let transcript = serde_json::from_str::<Vec<WasmRealtimeTranscriptEntry>>(transcript)
            .map_err(js_error)?;
        let transcript = transcript
            .into_iter()
            .map(|entry| TranscriptEntry::new(entry.role, entry.text))
            .collect::<Vec<_>>();
        Ok(realtime_delegation(input, &transcript))
    }

    /// Formats an unconsumed Realtime transcript tail using canonical Codex markers.
    ///
    /// # Errors
    ///
    /// Rejects malformed transcript JSON.
    #[wasm_bindgen(js_name = realtimeTailDelegation)]
    pub fn realtime_tail_delegation(&self, transcript: &str) -> Result<Option<String>, JsValue> {
        let transcript = serde_json::from_str::<Vec<WasmRealtimeTranscriptEntry>>(transcript)
            .map_err(js_error)?;
        let transcript = transcript
            .into_iter()
            .map(|entry| TranscriptEntry::new(entry.role, entry.text))
            .collect::<Vec<_>>();
        Ok(realtime_tail_delegation(&transcript))
    }

    /// Creates the Rust-owned Codex browser voice controller for this agent.
    ///
    /// # Errors
    ///
    /// Rejects voices outside Codex's ChatGPT V3 catalog.
    #[wasm_bindgen(js_name = browserVoice)]
    pub fn browser_voice(&self, voice: &str) -> Result<WasmBrowserVoice, JsValue> {
        WasmBrowserVoice::new(self.inner.clone(), voice).map_err(js_error)
    }

    /// Gracefully stops the driver and joins every resource owned by this agent.
    ///
    /// # Errors
    ///
    /// Rejects when the driver had already stopped or cleanup fails.
    pub async fn shutdown(&self) -> Result<(), JsValue> {
        if let Some(subagents) = &self.subagents {
            subagents
                .close_all(&self.inner.session_id().to_string())
                .await
                .map_err(js_error)?;
        }
        self.inner.shutdown().await.map_err(js_error)
    }
}

impl WasmNanocodex {
    fn from_parts(
        inner: RustNanocodex,
        events: AgentEvents,
        subagents: Option<WasmSubagents>,
    ) -> Self {
        let event_forwarding = Rc::new(Cell::new(false));
        forward_events(events, Rc::clone(&event_forwarding));
        Self {
            inner,
            subagents,
            event_forwarding,
        }
    }
}

impl Drop for WasmNanocodex {
    fn drop(&mut self) {
        if self.event_forwarding.replace(false)
            && let Some(subagents) = &self.subagents
        {
            subagents.set_event_forwarding(false);
        }
    }
}

/// Rust-owned Codex browser voice protocol and Agent bridge.
#[wasm_bindgen(js_name = BrowserVoice)]
pub struct WasmBrowserVoice {
    agent: RustNanocodex,
    protocol: RefCell<BrowserVoiceProtocol>,
    active_turn: Rc<RefCell<Option<(u64, TurnControl)>>>,
    next_turn: Rc<Cell<u64>>,
    startup_context: RefCell<Option<String>>,
    started: Cell<bool>,
}

#[wasm_bindgen(js_class = BrowserVoice)]
impl WasmBrowserVoice {
    /// Begins Codex's Realtime lifecycle and builds bounded browser startup context in Rust.
    ///
    /// # Errors
    ///
    /// Rejects when the Agent driver has stopped.
    pub async fn start(&self) -> Result<(), JsValue> {
        if self.started.get() {
            return Ok(());
        }
        let context = self
            .agent
            .append_developer_message(REALTIME_START_INSTRUCTIONS)
            .await
            .map_err(js_error)?;
        let tree =
            browser_workspace_tree(context.workspace(), &self.agent.session_id().to_string()).await;
        let history = browser_voice_history(context.history());
        self.startup_context.replace(build_browser_startup_context(
            &history,
            context.workspace(),
            &tree,
        ));
        self.started.set(true);
        Ok(())
    }

    /// Encodes the complete same-origin call request after the browser creates its SDP offer.
    ///
    /// # Errors
    ///
    /// Rejects calls made before [`Self::start`] or an empty SDP offer.
    #[wasm_bindgen(js_name = callBody)]
    pub fn call_body(&self, sdp: &str) -> Result<String, JsValue> {
        if !self.started.get() {
            return Err(js_error("browser voice has not started"));
        }
        if sdp.trim().is_empty() {
            return Err(js_error("browser voice requires an SDP offer"));
        }
        let protocol = self.protocol.borrow();
        let thread_id = self.agent.session_id().to_string();
        let call_body = build_chatgpt_realtime_call(
            sdp,
            protocol.voice(),
            self.startup_context.borrow().as_deref(),
        )
        .map_err(js_error)?;
        serde_json::to_string(&serde_json::json!({
            "openai_alpha": "quicksilver=v2",
            "realtime_session_id": thread_id,
            "session_id": thread_id,
            "thread_id": thread_id,
            "call_body": call_body,
        }))
        .map_err(js_error)
    }

    /// Decodes Codex's provider response body and Location header in Rust.
    ///
    /// # Errors
    ///
    /// Rejects an empty SDP answer or a Location without a Codex call identity.
    #[wasm_bindgen(js_name = completeCall)]
    pub fn complete_call(&self, response_body: &str, location: &str) -> Result<String, JsValue> {
        let result = decode_chatgpt_realtime_call(response_body, location).map_err(js_error)?;
        serde_json::to_string(&serde_json::json!({
            "call_id": result.call_id,
            "sdp": result.sdp,
        }))
        .map_err(js_error)
    }

    /// Builds the same-origin sideband URL with Codex's Rust-owned request identity.
    ///
    /// # Errors
    ///
    /// Rejects a malformed provider call identity.
    #[wasm_bindgen(js_name = sidebandUrl)]
    pub fn sideband_url(&self, call_id: &str) -> Result<String, JsValue> {
        if !valid_realtime_call_id(call_id) {
            return Err(js_error("invalid Realtime call ID"));
        }
        let thread_id = self.agent.session_id();
        Ok(format!(
            "/api/realtime/sideband?call_id={call_id}&realtime_session_id={thread_id}&session_id={thread_id}&thread_id={thread_id}&openai_alpha=quicksilver%3Dv2",
        ))
    }

    /// Replays Rust-retained outbound frames after a sideband connects.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    #[wasm_bindgen(js_name = sidebandOpened)]
    pub fn sideband_opened(&self) -> Result<String, JsValue> {
        encode_voice_effects(&self.protocol.borrow().sideband_opened())
    }

    /// Applies Codex's Rust-owned Frameless reconnect policy after transport loss.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    #[wasm_bindgen(js_name = sidebandClosed)]
    pub fn sideband_closed(&self, connected_ms: u32) -> Result<String, JsValue> {
        encode_voice_effects(
            &self
                .protocol
                .borrow_mut()
                .sideband_closed(u64::from(connected_ms)),
        )
    }

    /// Acknowledges frames written by the browser WebSocket effect executor.
    #[wasm_bindgen(js_name = framesSent)]
    pub fn frames_sent(&self, count: u32) {
        self.protocol.borrow_mut().frames_sent(count as usize);
    }

    /// Reports whether one Rust-decoded sideband event can admit Agent work.
    #[wasm_bindgen(js_name = requiresAgentAdmission)]
    pub fn requires_agent_admission(&self, payload: &str) -> bool {
        realtime_message_requires_agent_admission(payload)
    }

    /// Applies one Frameless sideband event and routes any delegation through the Rust Agent.
    ///
    /// # Errors
    ///
    /// Rejects when delegated Agent work cannot be accepted or steered.
    #[wasm_bindgen(js_name = realtimeMessage)]
    pub async fn realtime_message(&self, payload: &str) -> Result<String, JsValue> {
        let update = self.protocol.borrow_mut().realtime_message(payload);
        if let Some(delegation) = update.delegation {
            let input = realtime_delegation(&delegation.input, &delegation.transcript);
            self.route_agent_input(input).await.map_err(js_error)?;
        }
        encode_voice_effects(&update.effects)
    }

    /// Applies one typed Agent event to the Rust-owned handoff stream.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    #[wasm_bindgen(js_name = agentEvent)]
    pub fn agent_event(&self, envelope: &str) -> Result<String, JsValue> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(envelope) else {
            return encode_voice_effects(&BrowserVoiceEffects::default());
        };
        let target = value.get("target").unwrap_or(&serde_json::Value::Null);
        let session_id = self.agent.session_id().to_string();
        if value.get("type").and_then(serde_json::Value::as_str) != Some("event")
            || target.get("pane").and_then(serde_json::Value::as_str) != Some("main")
            || target.get("branchId").and_then(serde_json::Value::as_str)
                != Some(session_id.as_str())
        {
            return encode_voice_effects(&BrowserVoiceEffects::default());
        }
        let event = value
            .get("event")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let encoded = serde_json::to_string(&event).map_err(js_error)?;
        encode_voice_effects(&self.protocol.borrow_mut().agent_event(&encoded))
    }

    /// Drains one Codex-paced streamed or final Agent handoff chunk.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    pub fn flush(&self, final_chunk: bool) -> Result<String, JsValue> {
        encode_voice_effects(&self.protocol.borrow_mut().flush(final_chunk))
    }

    /// Flushes any final transcript tail and ends Codex's Realtime lifecycle.
    ///
    /// # Errors
    ///
    /// Rejects when the Agent driver stops.
    pub async fn stop(&self) -> Result<String, JsValue> {
        if !self.started.get() {
            return encode_voice_effects(&self.protocol.borrow().close_effects());
        }
        let tail = self.protocol.borrow_mut().take_transcript_tail();
        let routed = if let Some(input) = realtime_tail_delegation(&tail) {
            self.route_agent_input(input).await
        } else {
            Ok(())
        };
        let ended = self
            .agent
            .append_developer_message(REALTIME_END_INSTRUCTIONS)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string());
        self.started.set(false);
        match (routed, ended) {
            (Err(error), _) | (Ok(()), Err(error)) => return Err(js_error(error)),
            (Ok(()), Ok(())) => {}
        }
        encode_voice_effects(&self.protocol.borrow().close_effects())
    }

    /// Cancels only the active coding turn, never merely the voice transport.
    ///
    /// # Errors
    ///
    /// Rejects when the active turn cannot be cancelled.
    pub async fn cancel(&self) -> Result<bool, JsValue> {
        let control = self
            .active_turn
            .borrow()
            .as_ref()
            .map(|(_, control)| control.clone());
        let Some(control) = control else {
            return Ok(false);
        };
        control.cancel().await.map_err(js_error)?;
        Ok(true)
    }

    /// Selects Codex's preferred physical input from browser device labels.
    ///
    /// # Errors
    ///
    /// Rejects malformed label JSON.
    #[wasm_bindgen(js_name = preferredPhysicalInput)]
    pub fn preferred_physical_input(
        &self,
        current_label: &str,
        labels_json: &str,
    ) -> Result<Option<u32>, JsValue> {
        let labels = serde_json::from_str::<Vec<String>>(labels_json).map_err(js_error)?;
        preferred_physical_input(current_label, &labels)
            .map(|index| u32::try_from(index).map_err(js_error))
            .transpose()
    }
}

impl WasmBrowserVoice {
    fn new(agent: RustNanocodex, voice: &str) -> Result<Self, String> {
        Ok(Self {
            agent,
            protocol: RefCell::new(BrowserVoiceProtocol::new(voice)?),
            active_turn: Rc::new(RefCell::new(None)),
            next_turn: Rc::new(Cell::new(0)),
            startup_context: RefCell::new(None),
            started: Cell::new(false),
        })
    }

    async fn route_agent_input(&self, input: String) -> Result<(), String> {
        match self
            .agent
            .route_prompt(Prompt::new(input))
            .await
            .map_err(|error| error.to_string())?
        {
            PromptRoute::Steered => Ok(()),
            PromptRoute::Started(turn) => {
                let ticket = self.next_turn.get().saturating_add(1);
                self.next_turn.set(ticket);
                self.active_turn.replace(Some((ticket, turn.control())));
                let active_turn = Rc::clone(&self.active_turn);
                spawn_local(async move {
                    let _ = turn.await;
                    let mut active = active_turn.borrow_mut();
                    if active
                        .as_ref()
                        .is_some_and(|(active_ticket, _)| *active_ticket == ticket)
                    {
                        active.take();
                    }
                });
                Ok(())
            }
        }
    }
}

#[derive(Serialize)]
struct WasmManagedBrowserVoiceUpdate {
    effects: BrowserVoiceEffects,
    #[serde(skip_serializing_if = "Option::is_none")]
    delegation: Option<String>,
}

/// Standalone Rust-owned browser voice protocol for a remote managed Agent.
///
/// This core owns only Realtime protocol state. The caller owns media,
/// transports, the managed Agent lifecycle, and routing returned delegations.
#[wasm_bindgen(js_name = ManagedBrowserVoice)]
pub struct WasmManagedBrowserVoice {
    protocol: RefCell<BrowserVoiceProtocol>,
    startup_context: RefCell<Option<String>>,
    started: Cell<bool>,
}

#[wasm_bindgen(js_class = ManagedBrowserVoice)]
impl WasmManagedBrowserVoice {
    /// Creates an idle managed browser voice protocol core.
    ///
    /// # Errors
    ///
    /// Rejects voices outside Codex's ChatGPT V3 catalog.
    #[wasm_bindgen(constructor)]
    pub fn new(voice: &str) -> Result<Self, JsValue> {
        Ok(Self {
            protocol: RefCell::new(BrowserVoiceProtocol::new(voice).map_err(js_error)?),
            startup_context: RefCell::new(None),
            started: Cell::new(false),
        })
    }

    /// Starts the protocol from the managed Agent's authoritative serialized context.
    ///
    /// # Errors
    ///
    /// Rejects malformed `AgentSessionContext` JSON.
    pub fn start(&self, context_json: &str) -> Result<(), JsValue> {
        if self.started.get() {
            return Ok(());
        }
        let context = serde_json::from_str::<WasmOwnedAgentSessionContext>(context_json)
            .map_err(|error| js_error(format!("invalid AgentSessionContext: {error}")))?;
        let history = browser_voice_history(&context.history);
        self.startup_context.replace(build_browser_startup_context(
            &history,
            &context.workspace,
            &[],
        ));
        self.started.set(true);
        Ok(())
    }

    /// Encodes the managed same-origin call request after the browser creates its SDP offer.
    ///
    /// # Errors
    ///
    /// Rejects calls before [`Self::start`], invalid session IDs, or empty SDP offers.
    #[wasm_bindgen(js_name = callBody)]
    pub fn call_body(&self, sdp: &str, managed_session_id: &str) -> Result<String, JsValue> {
        if !self.started.get() {
            return Err(js_error("managed browser voice has not started"));
        }
        let session_id = managed_voice_session_id(managed_session_id)?;
        let protocol = self.protocol.borrow();
        let call_body = build_chatgpt_realtime_call(
            sdp,
            protocol.voice(),
            self.startup_context.borrow().as_deref(),
        )
        .map_err(js_error)?;
        serde_json::to_string(&serde_json::json!({
            "openai_alpha": "quicksilver=v2",
            "realtime_session_id": session_id,
            "session_id": session_id,
            "thread_id": session_id,
            "call_body": call_body,
        }))
        .map_err(js_error)
    }

    /// Decodes Codex's provider response body and Location header in Rust.
    ///
    /// # Errors
    ///
    /// Rejects an empty SDP answer or a Location without a Codex call identity.
    #[wasm_bindgen(js_name = completeCall)]
    pub fn complete_call(&self, response_body: &str, location: &str) -> Result<String, JsValue> {
        let result = decode_chatgpt_realtime_call(response_body, location).map_err(js_error)?;
        serde_json::to_string(&serde_json::json!({
            "call_id": result.call_id,
            "sdp": result.sdp,
        }))
        .map_err(js_error)
    }

    /// Builds the managed same-origin sideband URL for a provider call identity.
    ///
    /// # Errors
    ///
    /// Rejects malformed provider call or managed session identities.
    #[wasm_bindgen(js_name = sidebandUrl)]
    pub fn sideband_url(&self, call_id: &str, managed_session_id: &str) -> Result<String, JsValue> {
        if !valid_realtime_call_id(call_id) {
            return Err(js_error("invalid Realtime call ID"));
        }
        let session_id = managed_voice_session_id(managed_session_id)?;
        Ok(format!(
            "/api/realtime/sideband?call_id={call_id}&realtime_session_id={session_id}&session_id={session_id}&thread_id={session_id}&openai_alpha=quicksilver%3Dv2",
        ))
    }

    /// Replays Rust-retained outbound frames after a sideband connects.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    #[wasm_bindgen(js_name = sidebandOpened)]
    pub fn sideband_opened(&self) -> Result<String, JsValue> {
        encode_voice_effects(&self.protocol.borrow().sideband_opened())
    }

    /// Applies Codex's bounded reconnect policy after sideband transport loss.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    #[wasm_bindgen(js_name = sidebandClosed)]
    pub fn sideband_closed(&self, connected_ms: u32) -> Result<String, JsValue> {
        encode_voice_effects(
            &self
                .protocol
                .borrow_mut()
                .sideband_closed(u64::from(connected_ms)),
        )
    }

    /// Acknowledges frames written by the caller's WebSocket effect executor.
    #[wasm_bindgen(js_name = framesSent)]
    pub fn frames_sent(&self, count: u32) {
        self.protocol.borrow_mut().frames_sent(count as usize);
    }

    /// Reports whether one sideband event may produce a managed Agent delegation.
    #[wasm_bindgen(js_name = requiresAgentAdmission)]
    pub fn requires_agent_admission(&self, payload: &str) -> bool {
        realtime_message_requires_agent_admission(payload)
    }

    /// Applies one sideband event and returns effects plus canonical delegation text.
    ///
    /// The caller must route returned delegation text through its remote managed Agent.
    ///
    /// # Errors
    ///
    /// Rejects only when the update cannot be serialized.
    #[wasm_bindgen(js_name = realtimeMessage)]
    pub fn realtime_message(&self, payload: &str) -> Result<String, JsValue> {
        let update = self.protocol.borrow_mut().realtime_message(payload);
        let delegation = update
            .delegation
            .map(|delegation| realtime_delegation(&delegation.input, &delegation.transcript));
        encode_managed_voice_update(update.effects, delegation)
    }

    /// Applies one canonical raw `AgentEvent` JSON value to the handoff stream.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    #[wasm_bindgen(js_name = agentEvent)]
    pub fn agent_event(&self, event_json: &str) -> Result<String, JsValue> {
        encode_voice_effects(&self.protocol.borrow_mut().agent_event(event_json))
    }

    /// Drains one Codex-paced streamed or final managed Agent handoff chunk.
    ///
    /// # Errors
    ///
    /// Rejects only when effects cannot be serialized.
    pub fn flush(&self, final_chunk: bool) -> Result<String, JsValue> {
        encode_voice_effects(&self.protocol.borrow_mut().flush(final_chunk))
    }

    /// Stops the protocol and returns final transcript delegation plus close effects.
    ///
    /// # Errors
    ///
    /// Rejects only when the update cannot be serialized.
    pub fn stop(&self) -> Result<String, JsValue> {
        let tail = self.protocol.borrow_mut().take_transcript_tail();
        let delegation = realtime_tail_delegation(&tail);
        self.started.set(false);
        self.startup_context.replace(None);
        encode_managed_voice_update(self.protocol.borrow().close_effects(), delegation)
    }

    /// Selects Codex's preferred physical input from browser device labels.
    ///
    /// # Errors
    ///
    /// Rejects malformed label JSON.
    #[wasm_bindgen(js_name = preferredPhysicalInput)]
    pub fn preferred_physical_input(
        &self,
        current_label: &str,
        labels_json: &str,
    ) -> Result<Option<u32>, JsValue> {
        let labels = serde_json::from_str::<Vec<String>>(labels_json).map_err(js_error)?;
        preferred_physical_input(current_label, &labels)
            .map(|index| u32::try_from(index).map_err(js_error))
            .transpose()
    }
}

fn managed_voice_session_id(value: &str) -> Result<String, JsValue> {
    value
        .parse::<SessionId>()
        .map(|session_id| session_id.to_string())
        .map_err(|error| js_error(format!("invalid managed session ID: {error}")))
}

fn encode_managed_voice_update(
    effects: BrowserVoiceEffects,
    delegation: Option<String>,
) -> Result<String, JsValue> {
    serde_json::to_string(&WasmManagedBrowserVoiceUpdate {
        effects,
        delegation,
    })
    .map_err(js_error)
}

#[derive(Deserialize)]
struct WasmWorkspaceEntry {
    kind: String,
    path: String,
}

async fn browser_workspace_tree(_workspace: &str, session_id: &str) -> Vec<String> {
    const TREE_DEPTH: usize = 2;
    const TREE_ENTRIES: usize = 20;
    enum Task {
        List(String, usize),
        Render(WasmWorkspaceEntry, usize),
        Omitted(usize, usize),
    }
    let mut output = Vec::new();
    let mut pending = VecDeque::from([Task::List(String::from("."), 0_usize)]);
    while let Some(task) = pending.pop_back() {
        match task {
            Task::List(path, depth) => {
                if depth >= TREE_DEPTH {
                    continue;
                }
                let Ok(promise) = host_list_workspace(&path, session_id) else {
                    continue;
                };
                let Ok(value) = JsFuture::from(promise).await else {
                    continue;
                };
                let Some(encoded) = value.as_string() else {
                    continue;
                };
                let Ok(mut entries) = serde_json::from_str::<Vec<WasmWorkspaceEntry>>(&encoded)
                else {
                    continue;
                };
                entries.retain(|entry| !noisy_workspace_entry(&entry.path));
                entries.sort_by(|left, right| {
                    (left.kind == "file")
                        .cmp(&(right.kind == "file"))
                        .then_with(|| left.path.cmp(&right.path))
                });
                let omitted = entries.len().saturating_sub(TREE_ENTRIES);
                if omitted > 0 {
                    pending.push_back(Task::Omitted(omitted, depth));
                }
                for entry in entries.into_iter().take(TREE_ENTRIES).rev() {
                    if entry.kind == "directory" {
                        pending.push_back(Task::List(entry.path.clone(), depth + 1));
                    }
                    pending.push_back(Task::Render(entry, depth));
                }
            }
            Task::Render(entry, depth) => {
                let name = entry
                    .path
                    .rsplit('/')
                    .find(|part| !part.is_empty())
                    .unwrap_or(&entry.path);
                output.push(format!(
                    "{}- {}{}",
                    "  ".repeat(depth),
                    name,
                    if entry.kind == "directory" { "/" } else { "" }
                ));
            }
            Task::Omitted(omitted, depth) => {
                output.push(format!(
                    "{}- ... {omitted} more entries",
                    "  ".repeat(depth)
                ));
            }
        }
    }
    output
}

fn noisy_workspace_entry(path: &str) -> bool {
    let name = path
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path);
    name.starts_with('.')
        || [
            ".git",
            ".next",
            ".pytest_cache",
            ".ruff_cache",
            "__pycache__",
            "build",
            "dist",
            "node_modules",
            "out",
            "target",
        ]
        .contains(&name)
}

fn browser_voice_history(history: &[ResponseItem]) -> Vec<VoiceHistoryEntry> {
    history
        .iter()
        .filter_map(|item| {
            let ResponseItem::Message { role, content, .. } = item else {
                return None;
            };
            let role = match role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Developer => "developer",
            };
            let text = content
                .iter()
                .filter_map(|part| match part {
                    ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                        Some(text.as_ref())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(VoiceHistoryEntry::new(role, text))
        })
        .collect()
}

fn encode_voice_effects(effects: &BrowserVoiceEffects) -> Result<String, JsValue> {
    serde_json::to_string(effects).map_err(js_error)
}

struct TurnState {
    accepted: Option<Result<Option<String>, TurnFailure>>,
    control: Option<TurnControl>,
    completed: Option<Result<TurnResult, TurnFailure>>,
    waiters: Vec<oneshot::Sender<()>>,
}

#[derive(Clone)]
struct TurnFailure {
    code: &'static str,
    message: String,
}

impl TurnState {
    fn notify(&mut self) {
        for waiter in self.waiters.drain(..) {
            let _ = waiter.send(());
        }
    }
}

/// JavaScript binding over one shared Rust turn.
#[wasm_bindgen(js_name = Turn)]
pub struct WasmTurn {
    state: Rc<RefCell<TurnState>>,
}

#[wasm_bindgen(js_class = Turn)]
impl WasmTurn {
    /// Waits until the Rust driver has durably admitted this turn.
    ///
    /// Returns the durable request identity selected during admission, or
    /// `undefined` when the agent has no execution policy.
    ///
    /// # Errors
    ///
    /// Rejects with a stable `code` describing an admission failure.
    pub async fn accepted(&self) -> Result<Option<String>, JsValue> {
        self.acceptance().await.map_err(js_turn_error)
    }

    /// Injects text input at the active turn's next safe model boundary.
    ///
    /// # Errors
    ///
    /// Rejects if the turn is not active or its driver stopped.
    pub async fn steer(&self, instruction: &str) -> Result<(), JsValue> {
        if instruction.trim().is_empty() {
            return Err(js_error("steer instruction must not be empty"));
        }
        self.control()
            .await
            .map_err(js_error)?
            .steer(Prompt::new(instruction))
            .await
            .map_err(js_error)
    }

    /// Injects browser-safe multimodal input at the active turn's next boundary.
    ///
    /// # Errors
    ///
    /// Rejects malformed input or a turn that is no longer active.
    #[wasm_bindgen(js_name = steerContent)]
    pub async fn steer_content(&self, content_json: &str) -> Result<(), JsValue> {
        let prompt = parse_browser_prompt(content_json)?;
        self.control()
            .await
            .map_err(js_error)?
            .steer(prompt)
            .await
            .map_err(js_error)
    }

    /// Cancels this exact active or queued turn.
    ///
    /// # Errors
    ///
    /// Rejects if the turn is already terminal or its driver stopped.
    pub async fn cancel(&self) -> Result<(), JsValue> {
        self.control()
            .await
            .map_err(js_error)?
            .cancel()
            .await
            .map_err(js_error)
    }

    /// Waits for the final assistant message.
    ///
    /// # Errors
    ///
    /// Rejects with a stable `code` when the model run or driver fails.
    pub async fn result(&self) -> Result<WasmTurnResult, JsValue> {
        self.completion()
            .await
            .map(|inner| WasmTurnResult { inner })
            .map_err(js_turn_error)
    }
}

impl WasmTurn {
    fn accept(agent: RustNanocodex, prompt: Prompt, operation_id: Option<String>) -> Self {
        let state = Rc::new(RefCell::new(TurnState {
            accepted: None,
            control: None,
            completed: None,
            waiters: Vec::new(),
        }));
        let task_state = Rc::clone(&state);
        spawn_local(async move {
            let mut request = PromptRequest::new(prompt);
            if let Some(operation_id) = operation_id {
                request = request.request_id(operation_id);
            }
            let accepted = agent.prompt(request).await;
            match accepted {
                Ok(turn) => Self::complete_started(task_state, turn).await,
                Err(error) => {
                    let failure = turn_failure(&error);
                    let mut state = task_state.borrow_mut();
                    state.accepted = Some(Err(failure.clone()));
                    state.completed = Some(Err(failure));
                    state.notify();
                }
            }
        });
        Self { state }
    }

    fn started(turn: Turn) -> Self {
        let state = Rc::new(RefCell::new(TurnState {
            accepted: None,
            control: None,
            completed: None,
            waiters: Vec::new(),
        }));
        let task_state = Rc::clone(&state);
        spawn_local(async move {
            Self::complete_started(task_state, turn).await;
        });
        Self { state }
    }

    async fn complete_started(state: Rc<RefCell<TurnState>>, turn: Turn) {
        {
            let mut state = state.borrow_mut();
            state.accepted = Some(Ok(turn.request_id().map(str::to_owned)));
            state.control = Some(turn.control());
            state.notify();
        }
        let completed = turn.await.map_err(|error| turn_failure(&error));
        let mut state = state.borrow_mut();
        state.control = None;
        state.completed = Some(completed);
        state.notify();
    }

    async fn acceptance(&self) -> Result<Option<String>, TurnFailure> {
        loop {
            let notified = {
                let mut state = self.state.borrow_mut();
                if let Some(accepted) = &state.accepted {
                    return accepted.clone();
                }
                let (notify, notified) = oneshot::channel();
                state.waiters.push(notify);
                notified
            };
            notified.await.map_err(|_| TurnFailure {
                code: "retryable",
                message: "the turn stopped before it was accepted".to_owned(),
            })?;
        }
    }

    async fn control(&self) -> Result<TurnControl, String> {
        loop {
            let notified = {
                let mut state = self.state.borrow_mut();
                if let Some(control) = &state.control {
                    return Ok(control.clone());
                }
                if let Some(completed) = &state.completed {
                    return Err(completed
                        .as_ref()
                        .err()
                        .map(|failure| failure.message.clone())
                        .unwrap_or_else(|| "the turn is already complete".to_owned()));
                }
                let (notify, notified) = oneshot::channel();
                state.waiters.push(notify);
                notified
            };
            notified
                .await
                .map_err(|_| "the turn stopped before it was accepted".to_owned())?;
        }
    }

    async fn completion(&self) -> Result<TurnResult, TurnFailure> {
        loop {
            let notified = {
                let mut state = self.state.borrow_mut();
                if let Some(completed) = &state.completed {
                    return completed.clone();
                }
                let (notify, notified) = oneshot::channel();
                state.waiters.push(notify);
                notified
            };
            notified.await.map_err(|_| TurnFailure {
                code: "retryable",
                message: "the turn stopped before it completed".to_owned(),
            })?;
        }
    }
}

fn turn_failure(error: &NanocodexError) -> TurnFailure {
    let code = match error {
        NanocodexError::TurnCancelled => "cancelled",
        NanocodexError::InvalidRequest(_) | NanocodexError::ExecutionPolicyNotConfigured => {
            "invalid_request"
        }
        NanocodexError::AgentStopped | NanocodexError::TurnStopped => "retryable",
        NanocodexError::ExecutionPolicyOwnerStopped => "reopen_required",
        NanocodexError::ExecutionPolicy {
            disposition,
            source,
            ..
        } => source
            .as_ref()
            .downcast_ref::<nanocodex::durability::Error>()
            .filter(|error| {
                matches!(
                    error,
                    nanocodex::durability::Error::OperationConflict { .. }
                )
            })
            .map_or(execution_policy_failure_code(*disposition), |_| "conflict"),
        NanocodexError::Response(_)
            if error
                .responses_error()
                .is_some_and(|source| source.retry_advice().is_some()) =>
        {
            "retryable"
        }
        NanocodexError::Shutdown(source) => return turn_failure(source),
        _ => "failed",
    };
    TurnFailure {
        code,
        message: error.to_string(),
    }
}

const fn execution_policy_failure_code(
    disposition: nanocodex::ExecutionPolicyDisposition,
) -> &'static str {
    use nanocodex::ExecutionPolicyDisposition;

    match disposition {
        ExecutionPolicyDisposition::Retry => "retryable",
        ExecutionPolicyDisposition::Blocked => "blocked",
        ExecutionPolicyDisposition::Reopen => "reopen_required",
        ExecutionPolicyDisposition::Fatal => "failed",
    }
}

fn js_turn_error(failure: TurnFailure) -> JsValue {
    let error = js_sys::Error::new(&failure.message);
    let _ = js_sys::Reflect::set(&error, &"code".into(), &failure.code.into());
    error.into()
}

/// JavaScript binding over one completed Rust turn result.
#[wasm_bindgen(js_name = TurnResult)]
pub struct WasmTurnResult {
    inner: TurnResult,
}

#[wasm_bindgen(js_class = TurnResult)]
impl WasmTurnResult {
    /// Returns the final assistant message.
    #[wasm_bindgen(getter, js_name = finalMessage)]
    #[must_use]
    pub fn final_message(&self) -> String {
        self.inner.final_message().to_owned()
    }

    /// Serializes this completed boundary's resumable session snapshot.
    ///
    /// # Errors
    ///
    /// Throws when serialization fails.
    pub fn snapshot(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.inner.snapshot()).map_err(js_error)
    }

    /// Serializes exact aggregate usage for this completed logical turn.
    ///
    /// # Errors
    ///
    /// Throws when serialization fails.
    pub fn usage(&self) -> Result<String, JsValue> {
        serde_json::to_string(self.inner.usage()).map_err(js_error)
    }
}

async fn append_developer_context(agent: &RustNanocodex, text: &str) -> Result<String, JsValue> {
    let context = agent
        .append_developer_message(text)
        .await
        .map_err(js_error)?;
    serialize_session_context(context)
}

fn serialize_session_context(context: AgentSessionContext) -> Result<String, JsValue> {
    serde_json::to_string(&WasmAgentSessionContext {
        workspace: context.workspace(),
        history: context.history(),
    })
    .map_err(js_error)
}

fn forward_events(mut events: AgentEvents, forwarding: Rc<Cell<bool>>) {
    spawn_local(async move {
        while let Some(event) = events.recv().await {
            if !forwarding.get() {
                continue;
            }
            if let Ok(encoded) = serde_json::to_string(&event) {
                host_emit_event(
                    event.request_id.as_ref(),
                    &encoded,
                    u32::try_from(encoded.len()).unwrap_or(u32::MAX),
                );
            }
        }
    });
}

fn forward_subagent_updates(
    mut updates: tokio::sync::mpsc::UnboundedReceiver<ScopedAgentUpdate>,
    sessions: Rc<RefCell<HashMap<(String, SubagentId), String>>>,
    event_forwarders: Rc<Cell<usize>>,
) {
    spawn_local(async move {
        while let Some(scoped) = updates.recv().await {
            let root_session_id = scoped.root_session_id;
            match scoped.update {
                SubagentUpdate::Added(descriptor) => {
                    let context = serde_json::json!({
                        "agentId": descriptor.id.to_string(),
                        "parentAgentId": descriptor.parent.map(|id| id.to_string()),
                        "sessionId": &descriptor.session_id,
                        "role": &descriptor.role,
                        "task": &descriptor.task,
                    });
                    host_bind_subagent_session(
                        &root_session_id,
                        &descriptor.session_id,
                        &context.to_string(),
                    );
                    sessions
                        .borrow_mut()
                        .insert((root_session_id, descriptor.id), descriptor.session_id);
                }
                SubagentUpdate::Event { event, .. } => {
                    if event_forwarders.get() > 0
                        && let Ok(encoded) = serde_json::to_string(&event)
                    {
                        host_emit_event(
                            event.request_id.as_ref(),
                            &encoded,
                            u32::try_from(encoded.len()).unwrap_or(u32::MAX),
                        );
                    }
                }
                SubagentUpdate::Status {
                    id,
                    status: SubagentStatus::Closed,
                } => {
                    let session_id = sessions.borrow_mut().remove(&(root_session_id, id));
                    if let Some(session_id) = session_id {
                        host_release_subagent_session(&session_id);
                    }
                }
                SubagentUpdate::Status { .. } | SubagentUpdate::Message(_) => {}
            }
        }
        let session_ids = sessions
            .borrow_mut()
            .drain()
            .map(|(_, session_id)| session_id)
            .collect::<Vec<_>>();
        for session_id in session_ids {
            host_release_subagent_session(&session_id);
        }
    });
}

fn release_subagent_scope(
    sessions: &Rc<RefCell<HashMap<(String, SubagentId), String>>>,
    root_session_id: &str,
) {
    let session_ids = {
        let mut sessions = sessions.borrow_mut();
        let keys = sessions
            .keys()
            .filter(|(root, _)| root == root_session_id)
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| sessions.remove(&key))
            .collect::<Vec<_>>()
    };
    for session_id in session_ids {
        host_release_subagent_session(&session_id);
    }
}

fn parse_browser_prompt(content_json: &str) -> Result<Prompt, JsValue> {
    let content = serde_json::from_str::<Vec<UserInput>>(content_json)
        .map_err(|error| js_error(format!("invalid prompt content: {error}")))?;
    if content.iter().any(|input| {
        matches!(
            input,
            UserInput::LocalImage { .. } | UserInput::LocalAudio { .. }
        )
    }) {
        return Err(js_error(
            "browser prompt content cannot reference local filesystem paths",
        ));
    }
    let prompt = Prompt::content(content);
    if prompt.instruction.is_empty() {
        return Err(js_error("prompt content must not be empty"));
    }
    Ok(prompt)
}

fn validate(config: &WasmConfig) -> Result<(), JsValue> {
    if config.host_definition_id == 0 {
        return Err(js_error("host_definition_id must be at least 1"));
    }
    if config.api_key.trim().is_empty() {
        return Err(js_error("api_key must not be empty"));
    }
    for (name, value) in [
        ("websocket_url", config.websocket_url.as_deref()),
        ("api_base_url", config.api_base_url.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(js_error(format!("{name} must not be empty")));
        }
    }
    if config
        .session_id
        .as_deref()
        .is_some_and(|session_id| session_id.trim().is_empty())
    {
        return Err(js_error("session_id must not be empty"));
    }
    if config
        .durability_id
        .as_deref()
        .is_some_and(|journal_id| journal_id.trim().is_empty())
    {
        return Err(js_error("durability_id must not be empty"));
    }
    if config
        .durability_host_id
        .as_deref()
        .is_some_and(|route_id| route_id.trim().is_empty())
    {
        return Err(js_error("durability_host_id must not be empty"));
    }
    if config.durability_id.is_some() != config.durability_host_id.is_some() {
        return Err(js_error(
            "durability_id and durability_host_id must be supplied together",
        ));
    }
    if config
        .subagents
        .is_some_and(|subagents| subagents.max_concurrency == 0)
    {
        return Err(js_error("subagents.max_concurrency must be at least 1"));
    }
    Ok(())
}

fn validate_operation_id(operation_id: Option<&str>) -> Result<(), JsValue> {
    if operation_id.is_some_and(|operation_id| operation_id.trim().is_empty()) {
        return Err(js_error("durable operation ID must not be empty"));
    }
    Ok(())
}

fn parse_revision(revision: &str) -> Result<u64, StoreError> {
    revision.parse::<u64>().map_err(|error| {
        StoreError::Backend(format!("invalid JavaScript durability revision: {error}"))
    })
}

fn default_thinking() -> String {
    "high".to_owned()
}

fn default_model() -> String {
    Model::default().to_string()
}

fn default_reasoning_mode() -> String {
    "standard".to_owned()
}

const fn default_max_subagents() -> usize {
    nanocodex_subagents::DEFAULT_MAX_SUBAGENTS
}

fn host_error_message(error: &JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}

#[allow(clippy::needless_pass_by_value)]
fn js_error(error: impl ToString) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
