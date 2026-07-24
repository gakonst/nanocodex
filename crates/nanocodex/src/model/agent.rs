use std::{collections::HashMap, path::Path, sync::Arc};

use nanocodex_core::{
    AgentEventKind, EventSink, MODEL, ModelConfig, Prompt, ResponseItem, ResponseItemId,
    ResponsesTransport, Thinking, ToolDefinition, Usage, responses::RequestProfile,
};
use nanocodex_service::{
    CodeCall, CodeCallKind, ResponsesAttempt, ResponsesAttemptFactory, ResponsesClient,
    ResponsesOutput, ResponsesServiceResponse, TransportStats, TurnResult,
};
use serde::Serialize;
use serde_json::value::RawValue;
use tokio::sync::watch;
use tower::Service;
use tracing::{Instrument, info, info_span};
use web_time::Instant;

use super::context_manager::{
    assign_missing_response_item_id, assign_missing_response_item_ids, has_well_formed_tool_calls,
};
use super::{
    CompactionCompleted, CompactionFailed, CompactionStarted, ModelCallCompleted, ModelCallFailed,
    ModelCallStarted, RunError, RunStarted, RunStats, RunSteered, ToolCallArguments, ToolCallEvent,
    ToolResultEvent, WarmupCompleted, WarmupFailed, WarmupStarted,
    agents_md::load_instructions,
    compaction,
    context_manager::ContextManager,
    display_endpoint, elapsed_ns,
    input::{
        custom_tool_notification, custom_tool_output, developer_context, function_tool_output,
        task_context, task_input, turn_aborted,
    },
    resolve_workspace, terminal_payload,
};
use crate::{NanocodexError, Result, prompt_cache::ModelPromptCache};
use nanocodex_tools::{
    CodeModeExecution, CodeModeObserver, CodeModeUpdate, ImageGenerationConfig, OwnedToolContext,
    ToolContext, ToolOutputBody, ToolRuntime, ToolRuntimeControl, Tools, WebSearchConfig,
    prepare_output_images, prepare_user_input,
};

pub(crate) struct ModelRun<S> {
    events: EventSink,
    config: Arc<ModelConfig>,
    thinking: Thinking,
    fast_mode: bool,
    client: ResponsesClient<S>,
    transport_stats: Arc<TransportStats>,
    started_at: Instant,
    stats: RunStats,
    server_reasoning_included: bool,
    session: Option<ModelSessionState>,
    active_tools: Option<ToolRuntimeControl>,
    active_tool_call: Option<ActiveToolCall>,
    tool_call_indices: HashMap<Box<str>, u32>,
    tools: Tools,
    prompt_cache: ModelPromptCache,
    global_instructions: Option<Arc<str>>,
}

pub(crate) enum ModelTurnOutcome {
    Completed(CompletedModelTurn),
    Cancelled(ModelCheckpoint),
}

pub(crate) struct CompletedModelTurn {
    pub(crate) final_message: String,
    pub(crate) checkpoint: ModelCheckpoint,
}

#[derive(Clone)]
pub(crate) struct ModelCheckpoint {
    workspace: String,
    conversation: ConversationState,
    request_prefix: Arc<[ResponseItem]>,
    prompt_cache_key: Arc<str>,
    preserve_inherited_delta: bool,
    global_instructions: Option<Arc<str>>,
}

pub(crate) struct PreparedCheckpoint {
    pub(crate) checkpoint: ModelCheckpoint,
    pub(crate) runtime: ToolRuntime,
}

impl ModelCheckpoint {
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn history(&self) -> nanocodex_core::responses::ResponseHistory {
        self.conversation.shared_history()
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) const fn history_revision(&self) -> u64 {
        self.conversation.history_revision
    }

    pub(crate) fn request_prefix(&self) -> &[ResponseItem] {
        &self.request_prefix
    }

    pub(crate) fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    pub(crate) fn canonical_context(&self) -> &ResponseItem {
        &self.conversation.canonical_context
    }

    pub(crate) fn snapshot_history(&self) -> Vec<ResponseItem> {
        self.conversation.flattened_history()
    }

    pub(crate) fn resume(
        workspace: String,
        mut request_prefix: Vec<ResponseItem>,
        prompt_cache_key: Arc<str>,
        canonical_context: ResponseItem,
        history: Vec<ResponseItem>,
    ) -> Result<Self> {
        assign_request_prefix_ids(&mut request_prefix);
        Ok(Self {
            workspace,
            conversation: ConversationState::resume(canonical_context, history)?,
            request_prefix: Arc::from(request_prefix),
            prompt_cache_key,
            preserve_inherited_delta: false,
            global_instructions: None,
        })
    }
}

#[cfg(not(target_family = "wasm"))]
pub(crate) trait AgentSend: Send {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send> AgentSend for T {}

#[cfg(target_family = "wasm")]
pub(crate) trait AgentSend {}
#[cfg(target_family = "wasm")]
impl<T> AgentSend for T {}

struct ModelSessionState {
    workspace: String,
    tools: ToolRuntime,
    factory: ResponsesAttemptFactory,
    conversation: ConversationState,
    preserve_inherited_delta: bool,
}

impl ModelSessionState {
    fn validate_workspace(&self, requested: Option<&str>) -> Result<()> {
        let Some(requested) = requested else {
            return Ok(());
        };
        let requested = resolve_workspace(Some(requested))?;
        if requested != self.workspace {
            return Err(NanocodexError::WorkspaceChanged {
                current: self.workspace.clone(),
                requested,
            });
        }
        Ok(())
    }
}

struct ActiveToolCall {
    call_id: String,
    name: String,
    kind: CodeCallKind,
    started_at: Instant,
}

struct NestedToolEventObserver<'a> {
    events: &'a EventSink,
    tool_call_indices: &'a HashMap<Box<str>, u32>,
    stats: &'a mut RunStats,
    fallback_call_index: u32,
    parent_call_id: &'a str,
    error: Option<NanocodexError>,
}

impl CodeModeObserver for NestedToolEventObserver<'_> {
    fn update(&mut self, update: CodeModeUpdate<'_>) {
        if self.error.is_some() {
            return;
        }
        let result = match update {
            CodeModeUpdate::NestedCallStarted {
                call_id,
                name,
                input,
            } => {
                let (call_id, call_index) = self.event_context(call_id);
                let result = self.events.emit(
                    AgentEventKind::ToolCall,
                    ToolCallEvent {
                        call_id: &call_id,
                        tool: name,
                        arguments: input,
                        model_call_index: call_index,
                    },
                );
                if result.is_ok() {
                    self.stats.tool_calls += 1;
                }
                result
            }
            CodeModeUpdate::NestedCallCompleted(call) => {
                let (call_id, _) = self.event_context(&call.call_id);
                let result = self.events.emit(
                    AgentEventKind::ToolResult,
                    ToolResultEvent {
                        call_id: &call_id,
                        tool: &call.name,
                        status: status(call.success),
                        duration_ns: call.duration_ns,
                        started_after_ns: Some(call.started_after_ns),
                        result: &call.output,
                        metadata: call.metadata.as_deref(),
                    },
                );
                if result.is_ok() {
                    self.stats.tool_work_duration_ns += call.duration_ns;
                }
                result
            }
        };
        if let Err(error) = result {
            self.error = Some(error.into());
        }
    }
}

impl NestedToolEventObserver<'_> {
    fn event_context(&self, nested_call_id: &str) -> (String, u32) {
        let embedded_parent = nested_call_id
            .rsplit_once("/code-")
            .map(|(parent, _)| parent);
        let original_parent = embedded_parent.unwrap_or(self.parent_call_id);
        let call_id = embedded_parent.map_or_else(
            || format!("{}/{nested_call_id}", self.parent_call_id),
            |_| nested_call_id.to_owned(),
        );
        let call_index = self
            .tool_call_indices
            .get(original_parent)
            .copied()
            .unwrap_or(self.fallback_call_index);
        (call_id, call_index)
    }
}

async fn execute_code_call(
    tools: &ToolRuntime,
    call: &CodeCall,
    owned_context: Option<OwnedToolContext>,
    session_id: &str,
    observer: &mut dyn CodeModeObserver,
    tool_span: &tracing::Span,
) -> CodeModeExecution {
    if let Some(context) = owned_context {
        tools
            .execute_code_owned_with_updates(&call.input, context, observer)
            .instrument(tool_span.clone())
            .await
    } else {
        let context = ToolContext {
            model: MODEL,
            session_id,
            call_id: &call.call_id,
            history: &[],
            output_token_budget: nanocodex_tools::DEFAULT_TOOL_OUTPUT_TOKENS,
        };
        tools
            .wait_for_code_with_updates(&call.input, context, observer)
            .instrument(tool_span.clone())
            .await
    }
}

struct WarmupExecution {
    response_id: String,
    attempt: u32,
    connection_generation: u32,
    usage: Option<Usage>,
    server_reasoning_included: bool,
}

enum ModelTaskOutcome {
    Completed(String),
    Cancelled,
}

#[derive(Clone)]
struct ConversationState {
    canonical_context: Arc<ResponseItem>,
    context: ContextManager,
    delta_start: usize,
    previous_response_id: Option<String>,
    history_revision: u64,
}

impl ConversationState {
    fn new(mut history: Vec<ResponseItem>) -> Result<Self> {
        assign_missing_response_item_ids(&mut history);
        let canonical_context = history
            .iter()
            .find(|item| item.is_user_message())
            .cloned()
            .ok_or(NanocodexError::MalformedResponse {
                detail: "task input did not include initial context",
            })?;
        Ok(Self {
            canonical_context: Arc::new(canonical_context),
            context: ContextManager::new(history),
            delta_start: 0,
            previous_response_id: None,
            history_revision: 0,
        })
    }

    fn resume(mut canonical_context: ResponseItem, mut history: Vec<ResponseItem>) -> Result<Self> {
        if history.is_empty() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "conversation history must not be empty".to_owned(),
            ));
        }
        if !canonical_context.is_user_message() {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "canonical context must be a user message".to_owned(),
            ));
        }
        assign_missing_response_item_id(&mut canonical_context);
        assign_missing_response_item_ids(&mut history);
        if !has_well_formed_tool_calls(&history) {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "conversation history contains an unmatched or misordered tool call".to_owned(),
            ));
        }
        let history_len = history.len();
        let mut context = ContextManager::new(history);
        if context.len() != history_len {
            return Err(NanocodexError::InvalidSessionSnapshot(
                "conversation history contains an unsupported item".to_owned(),
            ));
        }
        context.commit_tail();
        let delta_start = context.len();
        Ok(Self {
            canonical_context: Arc::new(canonical_context),
            context,
            delta_start,
            previous_response_id: None,
            history_revision: 0,
        })
    }

    fn flattened_history(&self) -> Vec<ResponseItem> {
        self.context.flattened_items()
    }

    fn clear_delta(&mut self) {
        self.delta_start = self.context.len();
    }

    fn append(&mut self, items: impl IntoIterator<Item = ResponseItem>) {
        self.context.record_items(items);
    }

    fn update_token_info(&mut self, usage: Option<&Usage>) {
        self.context.update_token_info(usage);
    }

    fn active_context_tokens(&self, server_reasoning_included: bool) -> u64 {
        self.context
            .active_context_tokens(server_reasoning_included)
    }

    fn prompt_history(&self) -> nanocodex_core::responses::ResponseHistory {
        self.context.prompt_items()
    }

    fn shared_history(&self) -> nanocodex_core::responses::ResponseHistory {
        self.context.shared_items()
    }

    fn install_compaction(
        &mut self,
        item: ResponseItem,
        canonical_developer_context: ResponseItem,
        canonical_context: ResponseItem,
        request_prefix: &[ResponseItem],
    ) {
        let initial_context = [canonical_developer_context, canonical_context.clone()];
        let history =
            compaction::install_history(&self.context.flattened_items(), &initial_context, item);
        self.canonical_context = Arc::new(canonical_context);
        self.context.replace_and_recompute(history, request_prefix);
        self.delta_start = 0;
        self.previous_response_id = None;
        self.history_revision = self.history_revision.saturating_add(1);
    }

    fn reset_for_full_request(&mut self) {
        self.delta_start = 0;
        self.previous_response_id = None;
    }

    fn commit(&mut self) -> Result<()> {
        if self.previous_response_id.is_none() {
            return Err(NanocodexError::MalformedResponse {
                detail: "completed turn did not have a response ID",
            });
        }
        self.context.commit_tail();
        self.delta_start = self.context.len();
        Ok(())
    }

    fn commit_interrupted(&mut self) {
        self.reset_for_full_request();
        self.context.commit_tail();
    }
}

impl<S> ModelRun<S> {
    pub(crate) fn new(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        global_instructions: Option<Arc<str>>,
    ) -> Self {
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        Self {
            events,
            config,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            server_reasoning_included: false,
            session: None,
            active_tools: None,
            active_tool_call: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            global_instructions,
        }
    }

    pub(crate) fn from_checkpoint(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        prepared: PreparedCheckpoint,
    ) -> Self {
        let PreparedCheckpoint {
            checkpoint,
            runtime,
        } = prepared;
        let active_tools = runtime.control();
        let factory = ResponsesAttemptFactory::new(
            RequestProfile::new(
                events.request_id(),
                checkpoint.prompt_cache_key.to_string(),
                Arc::clone(&checkpoint.request_prefix),
            ),
            events.clone(),
            Arc::clone(&transport_stats),
        );
        let thinking = config.thinking;
        let fast_mode = config.fast_mode;
        Self {
            events,
            config,
            thinking,
            fast_mode,
            client,
            transport_stats,
            started_at: Instant::now(),
            stats: RunStats::default(),
            server_reasoning_included: false,
            session: Some(ModelSessionState {
                workspace: checkpoint.workspace,
                tools: runtime,
                factory,
                conversation: checkpoint.conversation,
                preserve_inherited_delta: checkpoint.preserve_inherited_delta,
            }),
            active_tools: Some(active_tools),
            active_tool_call: None,
            tool_call_indices: HashMap::new(),
            tools,
            prompt_cache,
            global_instructions: checkpoint.global_instructions,
        }
    }

    fn attempt_factory(&self, tools: &ToolRuntime) -> ResponsesAttemptFactory {
        attempt_factory(
            &self.events,
            &self.transport_stats,
            self.prompt_cache.key(),
            tools,
            self.config.system_prompt(),
        )
    }

    fn responses_endpoint(&self) -> &str {
        match self.config.responses_transport {
            ResponsesTransport::WebSocket => &self.config.websocket_url,
            ResponsesTransport::Https => &self.config.api_base_url,
        }
    }

    fn load_agent_instructions(&self, workspace: &str) -> Result<Option<String>> {
        load_instructions(Path::new(workspace), self.global_instructions.as_deref())
    }
}

pub(crate) fn prepare_checkpoint(
    checkpoint: ModelCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
) -> PreparedCheckpoint {
    let runtime = tool_runtime(checkpoint.workspace(), config, tools);
    PreparedCheckpoint {
        checkpoint,
        runtime,
    }
}

pub(crate) fn prepare_resumed_checkpoint(
    checkpoint: ModelCheckpoint,
    config: &ModelConfig,
    tools: &Tools,
    session_id: &str,
) -> Result<PreparedCheckpoint> {
    let prepared = prepare_checkpoint(checkpoint, config, tools);
    #[cfg(not(target_family = "wasm"))]
    let tool_specs = {
        let _ = session_id;
        prepared.runtime.model_specs()
    };
    #[cfg(target_family = "wasm")]
    let tool_specs = prepared.runtime.model_specs(session_id);
    let expected = request_profile(
        "resume-validation",
        "resume-validation",
        tool_specs,
        config.system_prompt(),
    );
    let expected =
        serde_json::to_vec(&without_response_item_ids(expected.prefix())).map_err(|error| {
            NanocodexError::InvalidSessionSnapshot(format!(
                "failed to validate the request prefix: {error}"
            ))
        })?;
    let stored = serde_json::to_vec(&without_response_item_ids(
        prepared.checkpoint.request_prefix(),
    ))
    .map_err(|error| {
        NanocodexError::InvalidSessionSnapshot(format!(
            "failed to validate the stored request prefix: {error}"
        ))
    })?;
    if expected != stored {
        return Err(NanocodexError::InvalidSessionSnapshot(
            "instructions or tool definitions do not match the resumed session".to_owned(),
        ));
    }
    Ok(prepared)
}

fn without_response_item_ids(items: &[ResponseItem]) -> Vec<ResponseItem> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            item.strip_id();
            item
        })
        .collect()
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<NanocodexError>,
    S::Future: AgentSend,
{
    pub(crate) fn emit_cancelled_before_start(
        &mut self,
        task: &Prompt,
        workspace: Option<&str>,
        thinking: Thinking,
        fast_mode: bool,
    ) -> Result<()> {
        self.thinking = thinking;
        self.fast_mode = fast_mode;
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: MODEL,
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                transport: self.config.responses_transport.as_str(),
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(self.responses_endpoint()),
                workspace,
                instruction_bytes: task.instruction.text_bytes(),
            },
        )?;
        let error = NanocodexError::TurnCancelled;
        let message = error.to_string();
        self.events
            .emit(AgentEventKind::RunError, RunError { message: &message })?;
        self.events.emit(
            AgentEventKind::RunFailed,
            terminal_payload(
                "cancelled",
                self.started_at.elapsed(),
                &self.config,
                self.thinking,
                &self.stats,
            ),
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute(
        &mut self,
        task: Prompt,
        workspace: Option<Arc<str>>,
        thinking: Thinking,
        fast_mode: bool,
        steers: tokio::sync::mpsc::Receiver<Prompt>,
        mut cancel: tokio::sync::oneshot::Receiver<()>,
        fork_snapshots: watch::Sender<Option<ModelCheckpoint>>,
    ) -> Result<ModelTurnOutcome> {
        self.thinking = thinking;
        self.fast_mode = fast_mode;
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        let transport_before = self.transport_stats.snapshot();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: MODEL,
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                transport: self.config.responses_transport.as_str(),
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(self.responses_endpoint()),
                workspace: workspace.as_deref(),
                instruction_bytes: task.instruction.text_bytes(),
            },
        )?;

        let outcome = self
            .execute_task(task, workspace, steers, &mut cancel, &fork_snapshots)
            .await;
        let elapsed = self.started_at.elapsed();
        match outcome {
            Ok(ModelTaskOutcome::Completed(message)) => {
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                self.events.emit(
                    AgentEventKind::RunCompleted,
                    terminal_payload(
                        "completed",
                        elapsed,
                        &self.config,
                        self.thinking,
                        &self.stats,
                    ),
                )?;
                let checkpoint = self.commit_checkpoint()?;
                Ok(ModelTurnOutcome::Completed(CompletedModelTurn {
                    final_message: message,
                    checkpoint,
                }))
            }
            Ok(ModelTaskOutcome::Cancelled) => {
                if let Some(tools) = &self.active_tools {
                    tools.cancel().await;
                }
                let checkpoint = self.commit_interrupted_checkpoint()?;
                let elapsed = self.started_at.elapsed();
                let error = NanocodexError::TurnCancelled;
                let message = error.to_string();
                self.events
                    .emit(AgentEventKind::RunError, RunError { message: &message })?;
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                self.events.emit(
                    AgentEventKind::RunFailed,
                    terminal_payload(
                        "cancelled",
                        elapsed,
                        &self.config,
                        self.thinking,
                        &self.stats,
                    ),
                )?;
                Ok(ModelTurnOutcome::Cancelled(checkpoint))
            }
            Err(error) => {
                let message = error.to_string();
                self.events
                    .emit(AgentEventKind::RunError, RunError { message: &message })?;
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                self.events.emit(
                    AgentEventKind::RunFailed,
                    terminal_payload("failed", elapsed, &self.config, self.thinking, &self.stats),
                )?;
                Err(error)
            }
        }
    }

    async fn prepare_follow_on_turn(
        &mut self,
        session: &mut ModelSessionState,
        task: &Prompt,
        cancel: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<bool> {
        let compacted = {
            let compaction = self.maybe_compact(
                self.stats.model_calls,
                &mut session.conversation,
                &session.factory,
                &session.workspace,
                session.tools.working_directory(),
                session.tools.default_shell_name(),
            );
            tokio::pin!(compaction);
            tokio::select! {
                biased;
                _ = &mut *cancel => return Ok(false),
                outcome = &mut compaction => outcome?,
            }
        };
        if compacted || session.preserve_inherited_delta {
            session.preserve_inherited_delta = false;
        } else {
            session.conversation.clear_delta();
        }
        let user_content = prepare_user_input(&task.instruction).await;
        session.conversation.append([ResponseItem::message(
            nanocodex_core::MessageRole::User,
            user_content,
        )]);
        Ok(true)
    }

    async fn execute_task(
        &mut self,
        task: Prompt,
        requested_workspace: Option<Arc<str>>,
        steers: tokio::sync::mpsc::Receiver<Prompt>,
        cancel: &mut tokio::sync::oneshot::Receiver<()>,
        fork_snapshots: &watch::Sender<Option<ModelCheckpoint>>,
    ) -> Result<ModelTaskOutcome> {
        let mut session = if let Some(mut session) = self.session.take() {
            if let Err(error) = session.validate_workspace(requested_workspace.as_deref()) {
                self.session = Some(session);
                return Err(error);
            }
            match self
                .prepare_follow_on_turn(&mut session, &task, cancel)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    self.session = Some(session);
                    return Ok(ModelTaskOutcome::Cancelled);
                }
                Err(error) => {
                    self.session = Some(session);
                    return Err(error);
                }
            }
            session
        } else {
            let workspace = resolve_workspace(requested_workspace.as_deref())?;
            let project_instructions = self.load_agent_instructions(&workspace)?;
            let tools = tool_runtime(&workspace, &self.config, &self.tools);
            self.active_tools = Some(tools.control());
            let factory = self.attempt_factory(&tools);
            let user_content = prepare_user_input(&task.instruction).await;
            let history = task_input(
                user_content,
                tools.working_directory(),
                tools.default_shell_name(),
                project_instructions.as_deref(),
            );
            let conversation = ConversationState::new(history)?;
            let mut session = ModelSessionState {
                workspace,
                tools,
                factory,
                conversation,
                preserve_inherited_delta: false,
            };
            Self::publish_fork_snapshot(
                &mut session,
                fork_snapshots,
                self.global_instructions.as_ref(),
            );
            let warmup = {
                let warmup = self.perform_warmup(&session.factory);
                tokio::pin!(warmup);
                tokio::select! {
                    biased;
                    _ = &mut *cancel => None,
                    outcome = &mut warmup => Some(outcome),
                }
            };
            let Some(warmup) = warmup else {
                self.session = Some(session);
                return Ok(ModelTaskOutcome::Cancelled);
            };
            match warmup {
                Ok(Some(response_id)) => {
                    session.conversation.previous_response_id = Some(response_id);
                }
                Ok(None) => {
                    session.conversation.reset_for_full_request();
                    self.stats.last_response_id = None;
                }
                Err(error) if error.responses_error().is_some() => {
                    session.conversation.reset_for_full_request();
                    self.stats.last_response_id = None;
                }
                Err(error) => return Err(error),
            }
            session
        };

        let outcome = {
            let task = self.drive_session(&mut session, steers, fork_snapshots);
            tokio::pin!(task);
            tokio::select! {
                biased;
                _ = &mut *cancel => None,
                outcome = &mut task => Some(outcome),
            }
        };
        self.session = Some(session);
        match outcome {
            Some(outcome) => outcome.map(ModelTaskOutcome::Completed),
            None => Ok(ModelTaskOutcome::Cancelled),
        }
    }

    fn commit_checkpoint(&mut self) -> Result<ModelCheckpoint> {
        let session = self
            .session
            .as_mut()
            .ok_or(NanocodexError::InvalidAttemptState {
                detail: "completed turn did not have a model session",
            })?;
        session.conversation.commit()?;
        Ok(ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta: false,
            global_instructions: self.global_instructions.clone(),
        })
    }

    fn commit_interrupted_checkpoint(&mut self) -> Result<ModelCheckpoint> {
        let aborted_output = if let Some(call) = self.active_tool_call.take() {
            let duration_ns = elapsed_ns(call.started_at);
            let output = ToolOutputBody::Text(format!(
                "Wall time: {:.3} seconds\naborted by user",
                call.started_at.elapsed().as_secs_f64()
            ));
            self.stats.tool_wall_duration_ns += duration_ns;
            self.events.emit(
                AgentEventKind::ToolResult,
                ToolResultEvent {
                    call_id: &call.call_id,
                    tool: &call.name,
                    status: "cancelled",
                    duration_ns,
                    started_after_ns: None,
                    result: &output,
                    metadata: None,
                },
            )?;
            Some(match call.kind {
                CodeCallKind::Custom => custom_tool_output(call.call_id, output),
                CodeCallKind::Function => function_tool_output(call.call_id, output),
            })
        } else {
            None
        };
        let session = self
            .session
            .as_mut()
            .ok_or(NanocodexError::InvalidAttemptState {
                detail: "cancelled turn did not have a model session",
            })?;
        session.conversation.append(aborted_output);
        session.conversation.append([turn_aborted()]);
        session.conversation.commit_interrupted();
        Ok(ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta: false,
            global_instructions: self.global_instructions.clone(),
        })
    }

    fn publish_fork_snapshot(
        session: &mut ModelSessionState,
        snapshots: &watch::Sender<Option<ModelCheckpoint>>,
        global_instructions: Option<&Arc<str>>,
    ) {
        session.conversation.context.commit_tail();
        snapshots.send_replace(Some(ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            request_prefix: session.factory.profile().shared_prefix(),
            prompt_cache_key: Arc::from(session.factory.profile().prompt_cache_key()),
            preserve_inherited_delta: true,
            global_instructions: global_instructions.cloned(),
        }));
    }

    async fn drive_session(
        &mut self,
        session: &mut ModelSessionState,
        mut steers: tokio::sync::mpsc::Receiver<Prompt>,
        fork_snapshots: &watch::Sender<Option<ModelCheckpoint>>,
    ) -> Result<String> {
        // Match Codex's ordering: always sample the turn's initial prompt once
        // before injecting input that arrived while that first request ran.
        let mut can_drain_steers = false;
        loop {
            if can_drain_steers {
                self.drain_steers(&mut session.conversation, &mut steers)
                    .await?;
            }
            Self::publish_fork_snapshot(session, fork_snapshots, self.global_instructions.as_ref());
            let call_index = self.stats.model_calls + 1;
            let response = self
                .perform_model_call(call_index, &session.conversation, &session.factory)
                .await?;
            session
                .conversation
                .update_token_info(response.usage.as_ref());
            session.conversation.previous_response_id = Some(response.id.clone());
            let end_turn = response.end_turn;
            let final_message = response.final_message;
            let code_calls = response.code_calls;
            session.conversation.append(response.output_items);
            can_drain_steers = true;

            if code_calls.is_empty() {
                if end_turn == Some(false) {
                    session.conversation.clear_delta();
                    self.maybe_compact(
                        call_index,
                        &mut session.conversation,
                        &session.factory,
                        &session.workspace,
                        session.tools.working_directory(),
                        session.tools.default_shell_name(),
                    )
                    .await?;
                    continue;
                }
                if !steers.is_empty() {
                    // The completed response is retained by previous_response_id;
                    // the next delta contains only newly drained steer messages.
                    session.conversation.clear_delta();
                    self.maybe_compact(
                        call_index,
                        &mut session.conversation,
                        &session.factory,
                        &session.workspace,
                        session.tools.working_directory(),
                        session.tools.default_shell_name(),
                    )
                    .await?;
                    continue;
                }
                if let Some(message) = final_message {
                    return Ok(if message.trim().is_empty() {
                        "The model completed without emitting assistant text.".to_owned()
                    } else {
                        message
                    });
                }
                return Err(NanocodexError::MalformedResponse {
                    detail: "model completed without a final message or exec call",
                });
            }

            session.conversation.clear_delta();
            for call in code_calls {
                let history = (call.name == "exec")
                    .then(|| Arc::new(session.conversation.flattened_history()));
                let output = self
                    .execute_model_tool(&session.tools, call_index, call, history)
                    .await?;
                session.conversation.append(output);
            }
            self.maybe_compact(
                call_index,
                &mut session.conversation,
                &session.factory,
                &session.workspace,
                session.tools.working_directory(),
                session.tools.default_shell_name(),
            )
            .await?;
        }
    }

    async fn drain_steers(
        &mut self,
        conversation: &mut ConversationState,
        steers: &mut tokio::sync::mpsc::Receiver<Prompt>,
    ) -> Result<()> {
        while let Ok(steer) = steers.try_recv() {
            if trace_content_enabled()
                && let Ok(content) = serde_json::to_string(&steer)
            {
                info!(
                    target: "nanocodex",
                    content_kind = "steer",
                    content = content.as_str(),
                    "turn content"
                );
            }
            let instruction_bytes = steer.instruction.text_bytes();
            let user_content = prepare_user_input(&steer.instruction).await;
            conversation.append([ResponseItem::message(
                nanocodex_core::MessageRole::User,
                user_content,
            )]);
            self.stats.steers += 1;
            self.events.emit(
                AgentEventKind::RunSteered,
                RunSteered {
                    steer_index: self.stats.steers,
                    instruction_bytes,
                },
            )?;
        }
        Ok(())
    }

    async fn execute_model_tool(
        &mut self,
        tools: &ToolRuntime,
        call_index: u32,
        call: CodeCall,
        history: Option<Arc<Vec<ResponseItem>>>,
    ) -> Result<Vec<ResponseItem>> {
        self.tool_call_indices
            .insert(call.call_id.clone().into_boxed_str(), call_index);
        let arguments = if call.name == "exec" {
            ToolCallArguments::Text(&call.input)
        } else {
            serde_json::from_str::<&RawValue>(&call.input)
                .map_or(ToolCallArguments::Text(&call.input), ToolCallArguments::Raw)
        };
        self.events.emit(
            AgentEventKind::ToolCall,
            ToolCallEvent {
                call_id: &call.call_id,
                tool: &call.name,
                arguments,
                model_call_index: call_index,
            },
        )?;
        self.stats.tool_calls += 1;
        if let Some(message) = unsupported_tool_message(&call) {
            let output = ToolOutputBody::Text(message);
            self.events.emit(
                AgentEventKind::ToolResult,
                ToolResultEvent {
                    call_id: &call.call_id,
                    tool: &call.name,
                    status: "failed",
                    duration_ns: 0,
                    started_after_ns: None,
                    result: &output,
                    metadata: None,
                },
            )?;
            return Ok(vec![match call.kind {
                CodeCallKind::Custom => custom_tool_output(call.call_id, output),
                CodeCallKind::Function => function_tool_output(call.call_id, output),
            }]);
        }
        let owned_context = owned_code_context(&call, history, self.events.request_id())?;
        let started_at = self.track_active_tool_call(&call);
        let tool_span = model_tool_span(&call, call_index);
        record_span_content(&tool_span, "tool.arguments", &call.input);
        let session_id = self.events.request_id().to_owned();
        let mut observer = NestedToolEventObserver {
            events: &self.events,
            tool_call_indices: &self.tool_call_indices,
            stats: &mut self.stats,
            fallback_call_index: call_index,
            parent_call_id: &call.call_id,
            error: None,
        };
        let mut execution = execute_code_call(
            tools,
            &call,
            owned_context,
            &session_id,
            &mut observer,
            &tool_span,
        )
        .await;
        let update_error = observer.error.take();
        drop(observer);
        if let Some(error) = update_error {
            return Err(error);
        }
        prepare_output_images(&mut execution.output).await;
        if let Some(content) = serialize_trace_content(&execution.output) {
            record_span_content(&tool_span, "tool.output", &content);
        }
        self.active_tool_call = None;
        let duration_ns = elapsed_ns(started_at);
        tool_span.record("status", status(execution.success));
        tool_span.record("otel.status_code", otel_status(execution.success));
        tool_span.record("duration_ns", duration_ns);
        self.stats.tool_wall_duration_ns += duration_ns;
        self.events.emit(
            AgentEventKind::ToolResult,
            ToolResultEvent {
                call_id: &call.call_id,
                tool: &call.name,
                status: status(execution.success),
                duration_ns,
                started_after_ns: None,
                result: &execution.output,
                metadata: None,
            },
        )?;
        let output = match call.kind {
            CodeCallKind::Custom => custom_tool_output(call.call_id.clone(), execution.output),
            CodeCallKind::Function => function_tool_output(call.call_id.clone(), execution.output),
        };
        let mut outputs = Vec::with_capacity(execution.notifications.len() + 1);
        outputs.push(output);
        outputs.extend(
            execution.notifications.into_iter().map(|notification| {
                custom_tool_notification(notification.call_id, notification.text)
            }),
        );
        Ok(outputs)
    }

    fn track_active_tool_call(&mut self, call: &CodeCall) -> Instant {
        let started_at = Instant::now();
        self.active_tool_call = Some(ActiveToolCall {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            kind: call.kind,
            started_at,
        });
        started_at
    }

    async fn maybe_compact(
        &mut self,
        after_model_call_index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
        project_workspace: &str,
        working_directory: &str,
        shell: &str,
    ) -> Result<bool> {
        let Some(auto_compact_token_limit) = compaction::auto_compact_token_limit(MODEL) else {
            return Ok(false);
        };
        let active_context_tokens =
            conversation.active_context_tokens(self.server_reasoning_included);
        if active_context_tokens < auto_compact_token_limit {
            return Ok(false);
        }
        let previous_response_id = conversation.previous_response_id.as_deref();
        let (item, _usage) = self
            .perform_compaction(
                after_model_call_index,
                conversation.prompt_history(),
                conversation.delta_start,
                previous_response_id,
                active_context_tokens,
                auto_compact_token_limit,
                factory,
            )
            .await?;
        let project_instructions = self.load_agent_instructions(project_workspace)?;
        let canonical_context =
            task_context(working_directory, shell, project_instructions.as_deref());
        conversation.install_compaction(
            item,
            developer_context(),
            canonical_context,
            factory.profile().prefix(),
        );
        Ok(true)
    }

    async fn perform_warmup(
        &mut self,
        factory: &ResponsesAttemptFactory,
    ) -> Result<Option<String>> {
        if matches!(self.config.responses_transport, ResponsesTransport::Https) {
            return Ok(None);
        }
        let started_at = Instant::now();
        self.events.emit(
            AgentEventKind::ModelWarmupStarted,
            WarmupStarted {
                model: MODEL,
                prompt_cache_key: factory.profile().prompt_cache_key(),
            },
        )?;
        let span = info_span!(
            target: "nanocodex",
            "model.warmup",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            model = MODEL,
            system_prompt.bytes = self.config.system_prompt().len(),
            warmup.source = tracing::field::Empty,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        if let Some(content) = serialize_trace_content(factory.profile().prefix()) {
            record_span_content(&span, "model.input", &content);
        }
        let shared_prompt_cache = self.prompt_cache.shared().cloned();
        let outcome = if let Some(cache) = shared_prompt_cache {
            match cache.entry(factory.profile()).await {
                Ok(entry) => {
                    let mut execution = None;
                    let initialized = entry
                        .get_or_try_init(|| async {
                            let completed = self.execute_warmup(factory, &span).await?;
                            execution = Some(completed);
                            Ok(())
                        })
                        .await;
                    initialized.map(|()| execution)
                }
                Err(error) => Err(error),
            }
        } else {
            self.execute_warmup(factory, &span).await.map(Some)
        };
        let execution = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                span.record("duration_ns", elapsed_ns(started_at));
                return self.warmup_failed(started_at, error);
            }
        };
        let duration_ns = elapsed_ns(started_at);
        let (response_id, source, attempt, connection_generation, usage) =
            if let Some(execution) = execution {
                self.server_reasoning_included |= execution.server_reasoning_included;
                if let Some(usage) = &execution.usage {
                    self.stats.warmup_usage.add(usage);
                }
                (
                    Some(execution.response_id),
                    "response",
                    Some(execution.attempt),
                    Some(execution.connection_generation),
                    execution.usage,
                )
            } else {
                (None, "shared_prefix", None, None, None)
            };
        span.record("warmup.source", source);
        span.record("status", "completed");
        span.record("otel.status_code", "OK");
        span.record("duration_ns", duration_ns);
        self.stats.warmup_duration_ns += duration_ns;
        self.stats.last_response_id.clone_from(&response_id);
        self.events.emit(
            AgentEventKind::ModelWarmupCompleted,
            WarmupCompleted {
                response_id: response_id.as_deref(),
                source,
                attempt,
                connection_generation,
                duration_ns,
                usage: usage.as_ref(),
            },
        )?;
        Ok(response_id)
    }

    async fn execute_warmup(
        &mut self,
        factory: &ResponsesAttemptFactory,
        span: &tracing::Span,
    ) -> Result<WarmupExecution> {
        let success = self
            .client
            .execute(factory.warmup(self.thinking, self.fast_mode))
            .instrument(span.clone())
            .await
            .map_err(Into::into)?;
        let attempt = success.attempt();
        let connection_generation = success.connection_generation();
        let server_reasoning_included = success.server_reasoning_included();
        let ResponsesOutput::Warmup(response) = success.into_output() else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            return Err(NanocodexError::InvalidAttemptState {
                detail: "warmup returned a non-warmup response",
            });
        };
        Ok(WarmupExecution {
            response_id: response.id,
            attempt,
            connection_generation,
            usage: response.usage,
            server_reasoning_included,
        })
    }

    fn warmup_failed<T>(&mut self, started_at: Instant, error: NanocodexError) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.warmup_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            AgentEventKind::ModelWarmupFailed,
            WarmupFailed {
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }

    async fn perform_model_call(
        &mut self,
        call_index: u32,
        conversation: &ConversationState,
        factory: &ResponsesAttemptFactory,
    ) -> Result<TurnResult> {
        let previous_response_id = conversation.previous_response_id.as_deref();
        let started_at = Instant::now();
        self.stats.model_calls += 1;
        self.events.emit(
            AgentEventKind::ModelCallStarted,
            ModelCallStarted {
                call_index,
                model: MODEL,
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.thinking.as_str(),
                previous_response_id,
            },
        )?;
        let request = factory.generation(
            call_index,
            conversation.prompt_history(),
            conversation.shared_history(),
            conversation.delta_start,
            previous_response_id,
            self.thinking,
            self.fast_mode,
        );
        let (input_item_count, input_bytes, input_content) = trace_model_input(&request);
        let span = model_call_span(
            call_index,
            self.config.reasoning_mode.as_str(),
            self.thinking.as_str(),
            previous_response_id.is_some(),
            input_item_count,
            input_bytes,
        );
        if let Some(input_content) = &input_content {
            record_span_content(&span, "model.input", input_content);
        }
        let success = match self.client.execute(request).instrument(span.clone()).await {
            Ok(success) => success,
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                span.record("duration_ns", elapsed_ns(started_at));
                return self.model_call_failed(call_index, started_at, error.into());
            }
        };
        let attempt = success.attempt();
        let connection_generation = success.connection_generation();
        self.server_reasoning_included |= success.server_reasoning_included();
        let ResponsesOutput::Generation(response) = success.into_output() else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            return Err(NanocodexError::InvalidAttemptState {
                detail: "generation returned a non-generation response",
            });
        };
        let duration_ns = elapsed_ns(started_at);
        record_model_response(&span, &response);
        span.record("status", "completed");
        span.record("otel.status_code", "OK");
        span.record("duration_ns", duration_ns);
        if let Some(usage) = &response.usage {
            span.record("input_tokens", usage.input_tokens);
            span.record(
                "cached_input_tokens",
                usage
                    .input_tokens_details
                    .as_ref()
                    .map_or(0, |details| details.cached_tokens),
            );
            span.record("output_tokens", usage.output_tokens);
        }
        self.stats.model_duration_ns += duration_ns;
        if let Some(usage) = &response.usage {
            self.stats.usage.add(usage);
        }
        self.stats.last_response_id = Some(response.id.clone());
        self.events.emit(
            AgentEventKind::ModelCallCompleted,
            ModelCallCompleted {
                call_index,
                model: MODEL,
                response_id: &response.id,
                attempt,
                connection_generation,
                status: &response.status,
                duration_ns,
                time_to_first_event_ns: response.time_to_first_event_ns,
                time_to_first_output_ns: response.time_to_first_output_ns,
                tool_calls: response.code_calls.len(),
                usage: response.usage.as_ref(),
            },
        )?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn perform_compaction(
        &mut self,
        after_model_call_index: u32,
        history: nanocodex_core::responses::ResponseHistory,
        incremental_start: usize,
        previous_response_id: Option<&str>,
        active_context_tokens: u64,
        auto_compact_token_limit: u64,
        factory: &ResponsesAttemptFactory,
    ) -> Result<(ResponseItem, Option<Usage>)> {
        let trigger = compaction::trigger();
        let mut history: Vec<_> = history.iter().cloned().collect();
        compaction::trim_tool_outputs_to_fit_context_window(&mut history, active_context_tokens);
        let history = nanocodex_core::responses::ResponseHistory::new(history);
        let started_at = Instant::now();
        self.stats.compactions += 1;
        self.events.emit(
            AgentEventKind::ModelCompactionStarted,
            CompactionStarted {
                after_model_call_index,
                active_context_tokens,
                auto_compact_token_limit,
                previous_response_id,
            },
        )?;
        let request = factory.compaction(
            after_model_call_index,
            history.clone(),
            history,
            incremental_start,
            previous_response_id,
            trigger,
            self.thinking,
            self.fast_mode,
        );
        let (input_item_count, input_bytes, input_content) = trace_model_input(&request);
        let span = info_span!(
            target: "nanocodex",
            "model.compaction",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            after_model_call_index,
            model.input.item_count = input_item_count,
            model.input.bytes = input_bytes,
            model.response.id = tracing::field::Empty,
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        if let Some(input_content) = &input_content {
            record_span_content(&span, "model.input", input_content);
        }
        let success = match self.client.execute(request).instrument(span.clone()).await {
            Ok(success) => success,
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                span.record("duration_ns", elapsed_ns(started_at));
                return self.compaction_failed(after_model_call_index, started_at, error.into());
            }
        };
        let attempt = success.attempt();
        let connection_generation = success.connection_generation();
        self.server_reasoning_included |= success.server_reasoning_included();
        let ResponsesOutput::Compaction(response) = success.into_output() else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            return Err(NanocodexError::InvalidAttemptState {
                detail: "compaction returned a non-compaction response",
            });
        };
        let duration_ns = elapsed_ns(started_at);
        span.record("model.response.id", response.id.as_str());
        if let Some(content) = serialize_trace_content(&response.item) {
            record_span_content(&span, "model.output_item", &content);
        }
        span.record("status", "completed");
        span.record("otel.status_code", "OK");
        span.record("duration_ns", duration_ns);
        self.stats.model_duration_ns += duration_ns;
        if let Some(usage) = &response.usage {
            self.stats.usage.add(usage);
        }
        self.stats.last_response_id = Some(response.id.clone());
        self.events.emit(
            AgentEventKind::ModelCompactionCompleted,
            CompactionCompleted {
                after_model_call_index,
                response_id: &response.id,
                attempt,
                connection_generation,
                status: &response.status,
                duration_ns,
                time_to_first_event_ns: response.time_to_first_event_ns,
                time_to_first_output_ns: response.time_to_first_output_ns,
                usage: response.usage.as_ref(),
            },
        )?;
        Ok((response.item, response.usage))
    }

    fn compaction_failed<T>(
        &mut self,
        after_model_call_index: u32,
        started_at: Instant,
        error: crate::NanocodexError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            AgentEventKind::ModelCompactionFailed,
            CompactionFailed {
                after_model_call_index,
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }

    fn model_call_failed<T>(
        &mut self,
        call_index: u32,
        started_at: Instant,
        error: crate::NanocodexError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            AgentEventKind::ModelCallFailed,
            ModelCallFailed {
                call_index,
                model: MODEL,
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }
}

fn unsupported_tool_message(call: &CodeCall) -> Option<String> {
    if call.namespace.is_none() && matches!(call.name.as_str(), "exec" | "wait") {
        return None;
    }
    let qualified_name = format!("{}{}", call.namespace.as_deref().unwrap_or(""), call.name);
    Some(match &call.kind {
        CodeCallKind::Custom => format!("unsupported custom tool call: {qualified_name}"),
        CodeCallKind::Function => format!("unsupported call: {qualified_name}"),
    })
}

fn trace_model_input(request: &ResponsesAttempt) -> (usize, usize, Option<String>) {
    let item_count = request.input_item_count();
    if !trace_content_enabled() {
        return (item_count, 0, None);
    }
    let items = request.input_items().collect::<Vec<_>>();
    let content = serde_json::to_string(&items).ok();
    let bytes = content.as_ref().map_or(0, String::len);
    (item_count, bytes, content)
}

fn trace_content_enabled() -> bool {
    tracing::enabled!(target: "nanocodex", tracing::Level::INFO)
}

fn serialize_trace_content<T: Serialize + ?Sized>(value: &T) -> Option<String> {
    trace_content_enabled()
        .then(|| serde_json::to_string(value).ok())
        .flatten()
}

fn model_tool_span(call: &CodeCall, call_index: u32) -> tracing::Span {
    info_span!(
        target: "nanocodex",
        "tool.call",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        tool.name = %call.name,
        tool.call_id = %call.call_id,
        tool.arguments.bytes = call.input.len(),
        model.call_index = call_index,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    )
}

fn owned_code_context(
    call: &CodeCall,
    history: Option<Arc<Vec<ResponseItem>>>,
    session_id: &str,
) -> Result<Option<OwnedToolContext>> {
    if call.name != "exec" {
        return Ok(None);
    }
    let history = history.ok_or(NanocodexError::MalformedResponse {
        detail: "exec call did not have an owned history snapshot",
    })?;
    Ok(Some(OwnedToolContext::new(
        MODEL,
        session_id,
        &call.call_id,
        history,
        nanocodex_tools::DEFAULT_TOOL_OUTPUT_TOKENS,
    )))
}

fn record_span_content(span: &tracing::Span, kind: &'static str, content: &str) {
    span.in_scope(|| {
        info!(
            target: "nanocodex",
            content_kind = kind,
            content,
            "trace content"
        );
    });
}

fn record_indexed_span_content(
    span: &tracing::Span,
    kind: &'static str,
    index: usize,
    content: &str,
) {
    span.in_scope(|| {
        info!(
            target: "nanocodex",
            content_kind = kind,
            output.index = index,
            content,
            "trace content"
        );
    });
}

fn model_call_span(
    call_index: u32,
    reasoning_mode: &str,
    reasoning_effort: &str,
    previous_response: bool,
    input_item_count: usize,
    input_bytes: usize,
) -> tracing::Span {
    info_span!(
        target: "nanocodex",
        "model.call",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        model = MODEL,
        reasoning.mode = reasoning_mode,
        reasoning.effort = reasoning_effort,
        model.call_index = call_index,
        previous_response,
        model.input.item_count = input_item_count,
        model.input.bytes = input_bytes,
        model.response.id = tracing::field::Empty,
        model.response.status = tracing::field::Empty,
        model.response.end_turn = tracing::field::Empty,
        model.output.item_count = tracing::field::Empty,
        model.output.bytes = tracing::field::Empty,
        model.tool_call_count = tracing::field::Empty,
        assistant.output.bytes = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
        input_tokens = tracing::field::Empty,
        cached_input_tokens = tracing::field::Empty,
        output_tokens = tracing::field::Empty,
        reasoning.summary_count = tracing::field::Empty,
        time_to_first_event_ns = tracing::field::Empty,
        time_to_first_output_ns = tracing::field::Empty,
        stream.display_delta.count = tracing::field::Empty,
        stream.display_delta.bytes = tracing::field::Empty,
        stream.inter_delta_gap.max_ns = tracing::field::Empty,
        stream.inter_delta_stall_100ms.count = tracing::field::Empty,
    )
}

fn record_model_response(span: &tracing::Span, response: &TurnResult) {
    span.record("model.response.id", response.id.as_str());
    span.record("model.response.status", response.status.as_str());
    if let Some(end_turn) = response.end_turn {
        span.record("model.response.end_turn", end_turn);
    }
    span.record("model.output.item_count", response.output_items.len());
    span.record("model.tool_call_count", response.code_calls.len());
    let trace_content = trace_content_enabled();
    let mut output_bytes = usize::from(trace_content).saturating_mul(2);
    let mut serialized_items = 0_usize;
    let mut summary_count = 0_usize;
    for (index, item) in response.output_items.iter().enumerate() {
        let kind = if let ResponseItem::Reasoning { summary, .. } = item {
            summary_count = summary_count.saturating_add(summary.len());
            "reasoning"
        } else {
            "model.output_item"
        };
        if trace_content && let Ok(content) = serde_json::to_string(item) {
            output_bytes = output_bytes
                .saturating_add(usize::from(serialized_items != 0))
                .saturating_add(content.len());
            serialized_items = serialized_items.saturating_add(1);
            record_indexed_span_content(span, kind, index, &content);
        }
    }
    span.record("model.output.bytes", output_bytes);
    if let Some(message) = &response.final_message {
        span.record("assistant.output.bytes", message.len());
    }
    span.record("reasoning.summary_count", summary_count);
    span.record("time_to_first_event_ns", response.time_to_first_event_ns);
    if let Some(time_to_first_output_ns) = response.time_to_first_output_ns {
        span.record("time_to_first_output_ns", time_to_first_output_ns);
    }
    span.record(
        "stream.display_delta.count",
        response.pipeline_stats.display_delta_count,
    );
    span.record(
        "stream.display_delta.bytes",
        response.pipeline_stats.display_delta_bytes,
    );
    span.record(
        "stream.inter_delta_gap.max_ns",
        response.pipeline_stats.inter_delta_gap_max_ns,
    );
    span.record(
        "stream.inter_delta_stall_100ms.count",
        response.pipeline_stats.inter_delta_stall_100ms_count,
    );
}

fn request_profile(
    session_id: &str,
    prompt_cache_key: &str,
    tool_specs: Vec<ToolDefinition>,
    system_prompt: &str,
) -> RequestProfile {
    let mut prefix = [
        ResponseItem::additional_tools(tool_specs),
        ResponseItem::message(
            nanocodex_core::MessageRole::Developer,
            [nanocodex_core::ContentItem::InputText {
                text: system_prompt.into(),
            }],
        ),
    ];
    assign_request_prefix_ids(&mut prefix);
    RequestProfile::new(session_id, prompt_cache_key, Arc::from(prefix))
}

fn assign_request_prefix_ids(prefix: &mut [ResponseItem]) {
    for item in prefix {
        if item.id().is_some_and(|id| !id.is_empty()) {
            continue;
        }
        let Some((item_prefix, suffix)) = (match item {
            ResponseItem::AdditionalTools { .. } => Some(("at", "nanocodex-tools")),
            ResponseItem::Message {
                role: nanocodex_core::MessageRole::Developer,
                ..
            } => Some(("msg", "nanocodex-instructions")),
            _ => None,
        }) else {
            assign_missing_response_item_id(item);
            continue;
        };
        item.set_id(Some(ResponseItemId::with_suffix(item_prefix, suffix)));
    }
}

fn attempt_factory(
    events: &EventSink,
    transport_stats: &Arc<TransportStats>,
    prompt_cache_key: &str,
    tools: &ToolRuntime,
    system_prompt: &str,
) -> ResponsesAttemptFactory {
    #[cfg(not(target_family = "wasm"))]
    let tool_specs = tools.model_specs();
    #[cfg(target_family = "wasm")]
    let tool_specs = tools.model_specs(events.request_id());
    ResponsesAttemptFactory::new(
        request_profile(
            events.request_id(),
            prompt_cache_key,
            tool_specs,
            system_prompt,
        ),
        events.clone(),
        Arc::clone(transport_stats),
    )
}

fn tool_runtime(workspace: &str, config: &ModelConfig, tools: &Tools) -> ToolRuntime {
    ToolRuntime::new_with_tools(
        workspace,
        tools.web_search_enabled().then(|| WebSearchConfig {
            endpoint: config.search_endpoint(),
            auth: config.auth.clone(),
        }),
        tools
            .image_generation_enabled()
            .then(|| ImageGenerationConfig {
                api_base_url: config.api_base_url.clone(),
                auth: config.auth.clone(),
                save_root: Path::new(workspace).to_path_buf(),
            }),
        tools,
    )
}

const fn status(success: bool) -> &'static str {
    if success { "completed" } else { "failed" }
}

const fn otel_status(success: bool) -> &'static str {
    if success { "OK" } else { "ERROR" }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
