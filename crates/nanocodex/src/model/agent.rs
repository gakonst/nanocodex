use std::{path::Path, sync::Arc};

use nanocodex_core::{
    AgentEventKind, EventSink, MODEL, ModelConfig, Prompt, ReasoningSummary, ResponseItem,
    ToolDefinition, Usage, responses::RequestProfile,
};
use nanocodex_service::{
    CodeCall, CodeCallKind, ResponsesAttempt, ResponsesAttemptFactory, ResponsesClient,
    ResponsesOutput, ResponsesServiceResponse, TRANSPORT, TransportStats, TurnResult,
};
use serde_json::value::RawValue;
use tower::Service;
use tracing::{Instrument, info_span};
use web_time::Instant;

use super::{
    AssistantMessage, CompactionCompleted, CompactionFailed, CompactionStarted, ModelCallCompleted,
    ModelCallFailed, ModelCallStarted, RunError, RunStarted, RunStats, ToolCallArguments,
    ToolCallEvent, ToolResultEvent, WarmupCompleted, WarmupFailed, WarmupStarted,
    agents_md::load_project_instructions,
    compaction,
    context_manager::ContextManager,
    display_endpoint, elapsed_ns,
    input::{
        custom_tool_notification, custom_tool_output, function_tool_output, task_context,
        task_input,
    },
    resolve_workspace, terminal_payload,
};
use crate::{AgentError, NanocodexError, ResponsesError, Result};
use nanocodex_tools::{
    ImageGenerationConfig, NestedToolCall, ToolContext, ToolOutputBody, ToolRuntime, Tools,
    WebSearchConfig, prepare_output_images, prepare_user_input,
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
    tools: Tools,
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
}

struct ConversationState {
    canonical_context: ResponseItem,
    context: ContextManager,
    delta_start: usize,
    previous_response_id: Option<String>,
}

impl ConversationState {
    fn new(history: Vec<ResponseItem>) -> Result<Self> {
        let canonical_context = history
            .first()
            .cloned()
            .ok_or(AgentError::MalformedResponse {
                detail: "task input did not include initial context",
            })?;
        Ok(Self {
            canonical_context,
            context: ContextManager::new(history),
            delta_start: 0,
            previous_response_id: None,
        })
    }

    fn history(&self) -> &[ResponseItem] {
        self.context.raw_items()
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

    fn shared_history(&self) -> Arc<Vec<ResponseItem>> {
        self.context.shared_items()
    }

    fn prompt_history(&self) -> Arc<Vec<ResponseItem>> {
        self.context.prompt_items()
    }

    fn replace_last_turn_images(&mut self, placeholder: &str) -> bool {
        self.context.replace_last_turn_images(placeholder)
    }

    fn install_compaction(
        &mut self,
        item: ResponseItem,
        canonical_context: ResponseItem,
        request_prefix: &[ResponseItem],
    ) {
        let history =
            compaction::install_history(self.context.raw_items(), &canonical_context, item);
        self.canonical_context = canonical_context;
        self.context.replace_and_recompute(history, request_prefix);
        self.delta_start = 0;
        self.previous_response_id = None;
    }

    fn reset_for_full_request(&mut self) {
        self.delta_start = 0;
        self.previous_response_id = None;
    }
}

impl<S> ModelRun<S> {
    pub(crate) fn new(
        events: EventSink,
        config: Arc<ModelConfig>,
        client: ResponsesClient<S>,
        transport_stats: Arc<TransportStats>,
        tools: Tools,
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
            tools,
        }
    }
}

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<NanocodexError>,
    S::Future: AgentSend,
{
    pub(crate) async fn execute(&mut self, task: Prompt) -> Result<String> {
        self.started_at = Instant::now();
        self.stats = RunStats::default();
        let transport_before = self.transport_stats.snapshot();
        self.events.emit(
            AgentEventKind::RunStarted,
            RunStarted {
                mode: "openai_model",
                model: MODEL,
                effort: self.config.thinking.as_str(),
                transport: TRANSPORT,
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(&self.config.websocket_url),
                workspace: task.workspace.as_deref(),
                instruction_bytes: task.instruction.text_bytes(),
            },
        )?;

        let outcome = self.execute_task(task).await;
        let elapsed = self.started_at.elapsed();
        match outcome {
            Ok(message) => {
                self.events.emit(
                    AgentEventKind::AssistantMessage,
                    AssistantMessage { text: &message },
                )?;
                self.stats
                    .apply_transport(self.transport_stats.since(transport_before));
                self.events.emit(
                    AgentEventKind::RunCompleted,
                    terminal_payload("completed", elapsed, &self.config, &self.stats),
                )?;
                Ok(message)
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

    async fn execute_task(&mut self, task: Prompt) -> Result<String> {
        let mut session = if let Some(mut session) = self.session.take() {
            if let Some(requested) = task.workspace.as_deref() {
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
                    return Err(AgentError::WorkspaceChanged {
                        current,
                        requested: resolved,
                    }
                    .into());
                }
            }
            let user_content = prepare_user_input(&task.instruction).await;
            session.conversation.clear_delta();
            session.conversation.append([ResponseItem::message(
                nanocodex_core::MessageRole::User,
                user_content,
            )]);
            session
        } else {
            let workspace = resolve_workspace(task.workspace.as_deref())?;
            let project_instructions = load_project_instructions(Path::new(&workspace))?;
            let tools = ToolRuntime::new(
                &workspace,
                self.tools.web_search_enabled().then(|| WebSearchConfig {
                    endpoint: self.config.search_endpoint(),
                    api_key: self.config.api_key.clone(),
                }),
                self.tools
                    .image_generation_enabled()
                    .then(|| ImageGenerationConfig {
                        api_base_url: self.config.api_base_url.clone(),
                        api_key: self.config.api_key.clone(),
                        save_root: Path::new(&workspace).to_path_buf(),
                    }),
            )
            .with_tools(&self.tools);
            let factory = ResponsesAttemptFactory::new(
                request_profile(
                    self.events.request_id(),
                    tools.model_specs(),
                    self.config.system_prompt(),
                ),
                self.events.clone(),
                Arc::clone(&self.transport_stats),
            );
            let user_content = prepare_user_input(&task.instruction).await;
            let history = task_input(
                user_content,
                &workspace,
                tools.default_shell_name(),
                project_instructions.as_deref(),
            );
            let mut conversation = ConversationState::new(history)?;
            match self.perform_warmup(&factory).await {
                Ok(response_id) => conversation.previous_response_id = Some(response_id),
                Err(error) if error.responses_error().is_some() => {
                    conversation.reset_for_full_request();
                    self.stats.last_response_id = None;
                }
                Err(error) => return Err(error),
            }
            ModelSessionState {
                workspace,
                tools,
                factory,
                conversation,
            }
        };

        let outcome = self.drive_session(&mut session).await;
        self.session = Some(session);
        outcome
    }

    async fn drive_session(&mut self, session: &mut ModelSessionState) -> Result<String> {
        loop {
            let call_index = self.stats.model_calls + 1;
            let response = match self
                .perform_model_call(call_index, &session.conversation, &session.factory)
                .await
            {
                Ok(response) => response,
                Err(error)
                    if matches!(
                        error.responses_error(),
                        Some(ResponsesError::InvalidImageRequest { .. })
                    ) && session
                        .conversation
                        .replace_last_turn_images("Invalid image") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
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

            if code_calls.is_empty() {
                if end_turn == Some(false) {
                    session.conversation.clear_delta();
                    self.maybe_compact(
                        call_index,
                        &mut session.conversation,
                        &session.factory,
                        &session.workspace,
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
                return Err(AgentError::MalformedResponse {
                    detail: "model completed without a final message or exec call",
                }
                .into());
            }

            session.conversation.clear_delta();
            for call in code_calls {
                let output = self
                    .execute_model_tool(
                        &session.tools,
                        call_index,
                        call,
                        session.conversation.history(),
                    )
                    .await?;
                session.conversation.append(output);
            }
            self.maybe_compact(
                call_index,
                &mut session.conversation,
                &session.factory,
                &session.workspace,
                session.tools.default_shell_name(),
            )
            .await?;
        }
    }

    async fn execute_model_tool(
        &mut self,
        tools: &ToolRuntime,
        call_index: u32,
        call: CodeCall,
        history: &[ResponseItem],
    ) -> Result<Vec<ResponseItem>> {
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
        let started_at = Instant::now();
        let context = ToolContext {
            model: MODEL,
            session_id: self.events.request_id(),
            call_id: &call.call_id,
            history,
            output_token_budget: nanocodex_tools::DEFAULT_TOOL_OUTPUT_TOKENS,
        };
        let tool_span = info_span!(
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
        );
        let mut execution = async {
            if call.name == "exec" {
                tools.execute_code(&call.input, context).await
            } else {
                tools.wait_for_code(&call.input, context).await
            }
        }
        .instrument(tool_span.clone())
        .await;
        prepare_output_images(&mut execution.output).await;
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

    async fn maybe_compact(
        &mut self,
        after_model_call_index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
        workspace: &str,
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
        let previous_response_id =
            conversation
                .previous_response_id
                .as_deref()
                .ok_or(AgentError::MalformedResponse {
                    detail: "compaction did not have a previous response ID",
                })?;
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
        let project_instructions = load_project_instructions(Path::new(workspace))?;
        let canonical_context = task_context(workspace, shell, project_instructions.as_deref());
        conversation.install_compaction(item, canonical_context, factory.profile().prefix());
        Ok(())
    }

    fn emit_nested_tool(
        &mut self,
        call_index: u32,
        parent_call_id: &str,
        call: &NestedToolCall,
    ) -> Result<()> {
        let call_id = format!("{parent_call_id}/{}", call.call_id);
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

    async fn perform_warmup(&mut self, factory: &ResponsesAttemptFactory) -> Result<String> {
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
            status = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let success = match self
            .client
            .execute(factory.warmup())
            .instrument(span.clone())
            .await
        {
            Ok(success) => success,
            Err(error) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                span.record("duration_ns", elapsed_ns(started_at));
                return self.warmup_failed(started_at, error.into());
            }
        };
        let attempt = success.attempt();
        let connection_generation = success.connection_generation();
        self.server_reasoning_included |= success.server_reasoning_included();
        let ResponsesOutput::Warmup(response) = success.into_output() else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            return Err(AgentError::InvalidAttemptState {
                detail: "warmup returned a non-warmup response",
            }
            .into());
        };
        let duration_ns = elapsed_ns(started_at);
        span.record("status", "completed");
        span.record("otel.status_code", "OK");
        span.record("duration_ns", duration_ns);
        self.stats.warmup_duration_ns += duration_ns;
        if let Some(usage) = &response.usage {
            self.stats.warmup_usage.add(usage);
        }
        self.stats.last_response_id = Some(response.id.clone());
        self.events.emit(
            AgentEventKind::ModelWarmupCompleted,
            WarmupCompleted {
                response_id: &response.id,
                attempt,
                connection_generation,
                duration_ns,
                usage: response.usage.as_ref(),
            },
        )?;
        Ok(response.id)
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
        let (input_item_count, input_bytes) = trace_model_input(&request);
        let span = model_call_span(
            call_index,
            previous_response_id.is_some(),
            input_item_count,
            input_bytes,
        );
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
            return Err(AgentError::InvalidAttemptState {
                detail: "generation returned a non-generation response",
            }
            .into());
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
        history: Arc<Vec<ResponseItem>>,
        incremental_start: usize,
        previous_response_id: &str,
        active_context_tokens: u64,
        auto_compact_token_limit: u64,
        factory: &ResponsesAttemptFactory,
    ) -> Result<(ResponseItem, Option<Usage>)> {
        let trigger = compaction::trigger();
        let mut history = Arc::unwrap_or_clone(history);
        compaction::trim_tool_outputs_to_fit_context_window(&mut history, active_context_tokens);
        let history = Arc::new(history);
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
            Arc::clone(&history),
            history,
            incremental_start,
            previous_response_id,
            trigger,
        );
        let success = match self.client.execute(request).await {
            Ok(success) => success,
            Err(error) => {
                return self.compaction_failed(after_model_call_index, started_at, error.into());
            }
        };
        let attempt = success.attempt();
        let connection_generation = success.connection_generation();
        self.server_reasoning_included |= success.server_reasoning_included();
        let ResponsesOutput::Compaction(response) = success.into_output() else {
            return Err(AgentError::InvalidAttemptState {
                detail: "compaction returned a non-compaction response",
            }
            .into());
        };
        let duration_ns = elapsed_ns(started_at);
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

fn trace_model_input(request: &ResponsesAttempt) -> (usize, usize) {
    let item_count = request.input_item_count();
    let items = request.input_items().collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&items).map_or(0, |encoded| encoded.len());
    (item_count, bytes)
}

fn model_call_span(
    call_index: u32,
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
        reasoning.summary = tracing::field::Empty,
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
    let output_bytes =
        serde_json::to_vec(&response.output_items).map_or(0, |encoded| encoded.len());
    span.record("model.output.bytes", output_bytes);
    if let Some(message) = &response.final_message {
        span.record("assistant.output.bytes", message.len());
    }
    record_response_reasoning(span, &response.output_items);
}

fn record_response_reasoning(span: &tracing::Span, items: &[ResponseItem]) {
    let mut summaries = Vec::new();
    for item in items {
        let ResponseItem::Reasoning { summary, .. } = item
        else {
            continue;
        };
        summaries.extend(summary.iter().map(|summary| match summary {
            ReasoningSummary::SummaryText { text } => text.as_ref(),
        }));
    }
    span.record("reasoning.summary_count", summaries.len());
    if !summaries.is_empty() {
        span.record("reasoning.summary", summaries.join("\n\n"));
    }
}

fn strip_item_id(mut item: ResponseItem) -> ResponseItem {
    item.strip_id();
    item
}

fn request_profile(
    session_id: &str,
    tool_specs: Vec<ToolDefinition>,
    system_prompt: &str,
) -> RequestProfile {
    RequestProfile::new(
        session_id,
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

const fn status(success: bool) -> &'static str {
    if success { "completed" } else { "failed" }
}

const fn otel_status(success: bool) -> &'static str {
    if success { "OK" } else { "ERROR" }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
