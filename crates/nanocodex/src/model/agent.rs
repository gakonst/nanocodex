use std::{collections::HashMap, path::Path, sync::Arc};

use nanocodex_core::{
    AgentEventKind, EventSink, MODEL, ModelConfig, Prompt, ResponseItem, ToolDefinition, Usage,
    responses::RequestProfile,
};
use nanocodex_service::{
    CodeCall, CodeCallKind, ResponsesAttempt, ResponsesAttemptFactory, ResponsesClient,
    ResponsesOutput, ResponsesServiceResponse, TRANSPORT, TransportStats, TurnResult,
};
use serde_json::value::RawValue;
use tokio::sync::watch;
use tower::Service;
use tracing::{Instrument, info, info_span};
use web_time::Instant;

use super::{
    CompactionCompleted, CompactionFailed, CompactionStarted, ModelCallCompleted, ModelCallFailed,
    ModelCallStarted, RunError, RunStarted, RunStats, RunSteered, ToolCallArguments, ToolCallEvent,
    ToolResultEvent, WarmupCompleted, WarmupFailed, WarmupStarted,
    agents_md::load_project_instructions,
    compaction,
    context_manager::ContextManager,
    display_endpoint, elapsed_ns,
    input::{
        custom_tool_notification, custom_tool_output, function_tool_output, task_context,
        task_input, turn_aborted,
    },
    resolve_workspace, terminal_payload,
};
use crate::{NanocodexError, Result, prompt_cache::ModelPromptCache};
use nanocodex_tools::{
    ImageGenerationConfig, NestedToolCall, OwnedToolContext, ToolContext, ToolOutputBody,
    ToolRuntime, ToolRuntimeControl, Tools, WebSearchConfig, prepare_output_images,
    prepare_user_input,
};

pub(crate) struct ModelRun<S> {
    events: EventSink,
    config: Arc<ModelConfig>,
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
    preserve_inherited_delta: bool,
}

impl ModelCheckpoint {
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn history(&self) -> nanocodex_core::responses::ResponseHistory {
        self.conversation.shared_history()
    }

    pub(crate) const fn history_revision(&self) -> u64 {
        self.conversation.history_revision
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

struct ActiveToolCall {
    call_id: String,
    name: String,
    kind: CodeCallKind,
    started_at: Instant,
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
    fn new(history: Vec<ResponseItem>) -> Result<Self> {
        let canonical_context =
            history
                .first()
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
        canonical_context: ResponseItem,
        request_prefix: &[ResponseItem],
    ) {
        let history =
            compaction::install_history(&self.context.flattened_items(), &canonical_context, item);
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
    ) -> Self {
        Self {
            events,
            config,
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
        }
    }

    pub(crate) fn from_checkpoint(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
        prompt_cache: ModelPromptCache,
        checkpoint: ModelCheckpoint,
    ) -> Self {
        let runtime = tool_runtime(&checkpoint.workspace, &config, &tools);
        let active_tools = runtime.control();
        let factory = attempt_factory(
            &events,
            &transport_stats,
            prompt_cache.key(),
            &runtime,
            config.system_prompt(),
        );
        Self {
            events,
            config,
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
    ) -> Result<()> {
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: MODEL,
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.config.thinking.as_str(),
                transport: TRANSPORT,
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(&self.config.websocket_url),
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
                &self.stats,
            ),
        )?;
        Ok(())
    }

    pub(crate) async fn execute(
        &mut self,
        task: Prompt,
        workspace: Option<Arc<str>>,
        steers: tokio::sync::mpsc::Receiver<Prompt>,
        mut cancel: tokio::sync::oneshot::Receiver<()>,
        fork_snapshots: watch::Sender<Option<ModelCheckpoint>>,
    ) -> Result<ModelTurnOutcome> {
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        let transport_before = self.transport_stats.snapshot();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: MODEL,
                reasoning_mode: self.config.reasoning_mode.as_str(),
                effort: self.config.thinking.as_str(),
                transport: TRANSPORT,
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(&self.config.websocket_url),
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
                    terminal_payload("completed", elapsed, &self.config, &self.stats),
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
                    terminal_payload("cancelled", elapsed, &self.config, &self.stats),
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
                    terminal_payload("failed", elapsed, &self.config, &self.stats),
                )?;
                Err(error)
            }
        }
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
            if let Some(requested) = requested_workspace.as_deref() {
                let resolved = match resolve_workspace(Some(requested)) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        self.session = Some(session);
                        return Err(error);
                    }
                };
                if resolved != session.workspace {
                    let current = session.workspace.clone();
                    self.session = Some(session);
                    return Err(NanocodexError::WorkspaceChanged {
                        current,
                        requested: resolved,
                    });
                }
            }
            let user_content = prepare_user_input(&task.instruction).await;
            if session.preserve_inherited_delta {
                session.preserve_inherited_delta = false;
            } else {
                session.conversation.clear_delta();
            }
            session.conversation.append([ResponseItem::message(
                nanocodex_core::MessageRole::User,
                user_content,
            )]);
            session
        } else {
            let workspace = resolve_workspace(requested_workspace.as_deref())?;
            let project_instructions = load_project_instructions(Path::new(&workspace))?;
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
            Self::publish_fork_snapshot(&mut session, fork_snapshots);
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
            preserve_inherited_delta: false,
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
            preserve_inherited_delta: false,
        })
    }

    fn publish_fork_snapshot(
        session: &mut ModelSessionState,
        snapshots: &watch::Sender<Option<ModelCheckpoint>>,
    ) {
        session.conversation.context.commit_tail();
        snapshots.send_replace(Some(ModelCheckpoint {
            workspace: session.workspace.clone(),
            conversation: session.conversation.clone(),
            preserve_inherited_delta: true,
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
            Self::publish_fork_snapshot(session, fork_snapshots);
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
            session
                .conversation
                .append(response.output_items.into_iter().map(strip_item_id));
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
            if let Ok(content) = serde_json::to_string(&steer) {
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
        let mut execution = if let Some(context) = owned_context {
            tools
                .execute_code_owned(&call.input, context)
                .instrument(tool_span.clone())
                .await
        } else {
            let context = ToolContext {
                model: MODEL,
                session_id: self.events.request_id(),
                call_id: &call.call_id,
                history: &[],
                output_token_budget: nanocodex_tools::DEFAULT_TOOL_OUTPUT_TOKENS,
            };
            tools
                .wait_for_code(&call.input, context)
                .instrument(tool_span.clone())
                .await
        };
        prepare_output_images(&mut execution.output).await;
        if let Ok(content) = serde_json::to_string(&execution.output) {
            record_span_content(&tool_span, "tool.output", &content);
        }
        self.active_tool_call = None;
        let duration_ns = elapsed_ns(started_at);
        tool_span.record("status", status(execution.success));
        tool_span.record("otel.status_code", otel_status(execution.success));
        tool_span.record("duration_ns", duration_ns);
        self.stats.tool_wall_duration_ns += duration_ns;
        for nested in &execution.nested_calls {
            self.emit_nested_tool(call_index, &call.call_id, nested)?;
        }
        self.events.emit(
            AgentEventKind::ToolResult,
            ToolResultEvent {
                call_id: &call.call_id,
                tool: &call.name,
                status: status(execution.success),
                duration_ns,
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
    ) -> Result<()> {
        let Some(auto_compact_token_limit) = compaction::auto_compact_token_limit(MODEL) else {
            return Ok(());
        };
        let active_context_tokens =
            conversation.active_context_tokens(self.server_reasoning_included);
        if active_context_tokens < auto_compact_token_limit {
            return Ok(());
        }
        let previous_response_id = conversation.previous_response_id.as_deref().ok_or(
            NanocodexError::MalformedResponse {
                detail: "compaction did not have a previous response ID",
            },
        )?;
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
        let project_instructions = load_project_instructions(Path::new(project_workspace))?;
        let canonical_context =
            task_context(working_directory, shell, project_instructions.as_deref());
        conversation.install_compaction(item, canonical_context, factory.profile().prefix());
        Ok(())
    }

    fn emit_nested_tool(
        &mut self,
        call_index: u32,
        parent_call_id: &str,
        call: &NestedToolCall,
    ) -> Result<()> {
        let embedded_parent = call.call_id.rsplit_once("/code-").map(|(parent, _)| parent);
        let original_parent = embedded_parent.unwrap_or(parent_call_id);
        let call_id = embedded_parent.map_or_else(
            || format!("{parent_call_id}/{}", call.call_id),
            |_| call.call_id.clone(),
        );
        let call_index = self
            .tool_call_indices
            .get(original_parent)
            .copied()
            .unwrap_or(call_index);
        self.events.emit(
            AgentEventKind::ToolCall,
            ToolCallEvent {
                call_id: &call_id,
                tool: &call.name,
                arguments: &call.input,
                model_call_index: call_index,
            },
        )?;
        self.events.emit(
            AgentEventKind::ToolResult,
            ToolResultEvent {
                call_id: &call_id,
                tool: &call.name,
                status: status(call.success),
                duration_ns: call.duration_ns,
                result: &call.output,
                metadata: call.metadata.as_deref(),
            },
        )?;
        self.stats.tool_calls += 1;
        self.stats.tool_work_duration_ns += call.duration_ns;
        Ok(())
    }

    async fn perform_warmup(
        &mut self,
        factory: &ResponsesAttemptFactory,
    ) -> Result<Option<String>> {
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
        if let Ok(content) = serde_json::to_string(factory.profile().prefix()) {
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
            .execute(factory.warmup())
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
                effort: self.config.thinking.as_str(),
                previous_response_id,
            },
        )?;
        let request = factory.generation(
            call_index,
            conversation.prompt_history(),
            conversation.shared_history(),
            conversation.delta_start,
            previous_response_id,
        );
        let (input_item_count, input_bytes, input_content) = trace_model_input(&request);
        let span = model_call_span(
            call_index,
            self.config.reasoning_mode.as_str(),
            self.config.thinking.as_str(),
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
        previous_response_id: &str,
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
        if let Ok(content) = serde_json::to_string(&response.item) {
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
    let items = request.input_items().collect::<Vec<_>>();
    let content = serde_json::to_string(&items).ok();
    let bytes = content.as_ref().map_or(0, String::len);
    (item_count, bytes, content)
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
    let output_content = serde_json::to_string(&response.output_items).ok();
    let output_bytes = output_content.as_ref().map_or(0, String::len);
    span.record("model.output.bytes", output_bytes);
    let mut summary_count = 0_usize;
    for (index, item) in response.output_items.iter().enumerate() {
        let kind = if let ResponseItem::Reasoning { summary, .. } = item {
            summary_count = summary_count.saturating_add(summary.len());
            "reasoning"
        } else {
            "model.output_item"
        };
        if let Ok(content) = serde_json::to_string(item) {
            record_indexed_span_content(span, kind, index, &content);
        }
    }
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

fn strip_item_id(mut item: ResponseItem) -> ResponseItem {
    item.strip_id();
    item
}

fn request_profile(
    session_id: &str,
    prompt_cache_key: &str,
    tool_specs: Vec<ToolDefinition>,
    system_prompt: &str,
) -> RequestProfile {
    RequestProfile::new(
        session_id,
        prompt_cache_key,
        Arc::from([
            ResponseItem::additional_tools(tool_specs),
            ResponseItem::message(
                nanocodex_core::MessageRole::Developer,
                [nanocodex_core::ContentItem::InputText {
                    text: system_prompt.into(),
                }],
            ),
        ]),
    )
}

fn attempt_factory(
    events: &EventSink,
    transport_stats: &Arc<TransportStats>,
    prompt_cache_key: &str,
    tools: &ToolRuntime,
    system_prompt: &str,
) -> ResponsesAttemptFactory {
    ResponsesAttemptFactory::new(
        request_profile(
            events.request_id(),
            prompt_cache_key,
            tools.model_specs(),
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
