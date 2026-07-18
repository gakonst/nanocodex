use std::{io::Write, path::Path, time::Instant};

use serde::Serialize;
use serde_json::Value;

use super::{
    ApiEvent, AssistantMessage, ModelConfig, RunError, RunStarted, RunStats, TRANSPORT,
    agents_md::load_project_instructions,
    compaction,
    context_manager::ContextManager,
    display_endpoint, elapsed_ns, resolve_workspace,
    stream::{CodeCall, CodeCallKind},
    terminal_payload,
    wire::{
        RequestProfile, ResponseCreate, Usage, WarmupServerEvent, custom_tool_notification,
        custom_tool_output, function_tool_output, task_input,
    },
};
use crate::{
    AgentError, HarnessError, ResponsesError, Result,
    protocol::{EventWriter, Task},
    responses::{EncodedRequest, ResponsesSocket, decode_event, parse_raw_json},
    tools::{
        ImageGenerationConfig, NestedToolCall, ToolContext, ToolOutputBody, ToolRuntime,
        WebSearchConfig, prepare_output_images, prepare_user_input,
    },
};

#[derive(Serialize)]
struct ConnectionStarted<'a> {
    transport: &'static str,
    websocket_url: &'a str,
    attempt: u32,
    purpose: ConnectionPurpose,
}

#[derive(Serialize)]
struct ConnectionCompleted<'a> {
    transport: &'static str,
    attempt: u32,
    purpose: ConnectionPurpose,
    duration_ns: u64,
    http_status: u16,
    request_id: Option<&'a str>,
    server_model: Option<&'a str>,
    server_reasoning_included: bool,
}

#[derive(Serialize)]
struct ConnectionFailed<'a> {
    transport: &'static str,
    attempt: u32,
    purpose: ConnectionPurpose,
    duration_ns: u64,
    error: &'a str,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConnectionPurpose {
    Initial,
    WarmupFallback,
    Reconnect,
}

#[derive(Serialize)]
struct ConnectionRetry<'a> {
    call_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    reason: &'a str,
}

#[derive(Serialize)]
struct ModelCallStarted<'a> {
    call_index: u32,
    model: &'a str,
    effort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
}

#[derive(Serialize)]
struct WarmupStarted<'a> {
    model: &'a str,
    prompt_cache_key: &'a str,
}

#[derive(Serialize)]
struct WarmupCompleted<'a> {
    response_id: &'a str,
    duration_ns: u64,
    usage: Option<&'a Usage>,
}

#[derive(Serialize)]
struct WarmupFailed<'a> {
    duration_ns: u64,
    error: &'a str,
}

#[derive(Serialize)]
struct ModelCallCompleted<'a> {
    call_index: u32,
    model: &'a str,
    response_id: &'a str,
    status: &'a str,
    duration_ns: u64,
    time_to_first_event_ns: u64,
    time_to_first_output_ns: Option<u64>,
    tool_calls: usize,
    usage: Option<&'a Usage>,
}

#[derive(Serialize)]
struct ModelCallFailed<'a> {
    call_index: u32,
    model: &'a str,
    duration_ns: u64,
    error: &'a str,
}

#[derive(Serialize)]
struct CompactionStarted<'a> {
    after_model_call_index: u32,
    active_context_tokens: u64,
    auto_compact_token_limit: u64,
    previous_response_id: &'a str,
}

#[derive(Serialize)]
struct CompactionCompleted<'a> {
    after_model_call_index: u32,
    response_id: &'a str,
    status: &'a str,
    duration_ns: u64,
    time_to_first_event_ns: u64,
    time_to_first_output_ns: Option<u64>,
    usage: Option<&'a Usage>,
}

#[derive(Serialize)]
struct CompactionFailed<'a> {
    after_model_call_index: u32,
    duration_ns: u64,
    error: &'a str,
}

#[derive(Serialize)]
struct ToolCallEvent<'a> {
    call_id: &'a str,
    tool: &'a str,
    arguments: &'a Value,
    model_call_index: u32,
}

#[derive(Serialize)]
struct ToolResultEvent<'a> {
    call_id: &'a str,
    tool: &'a str,
    status: &'static str,
    duration_ns: u64,
    result: &'a ToolOutputBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a Value>,
}

pub(super) struct ModelRun<'a, W> {
    events: &'a mut EventWriter<W>,
    task: &'a Task,
    config: &'a ModelConfig,
    started_at: Instant,
    stats: RunStats,
    server_reasoning_included: bool,
    turn_state: Option<String>,
}

struct ConversationState {
    canonical_context: Value,
    history: ContextManager,
    delta_start: Option<usize>,
    previous_response_id: Option<String>,
}

impl ConversationState {
    fn new(history: Vec<Value>) -> Result<Self> {
        let canonical_context = history
            .first()
            .cloned()
            .ok_or(AgentError::MalformedResponse {
                detail: "task input did not include initial context",
            })?;
        Ok(Self {
            canonical_context,
            history: ContextManager::new(history),
            delta_start: Some(0),
            previous_response_id: None,
        })
    }

    fn delta(&self) -> &[Value] {
        self.history.items_from(self.delta_start.unwrap_or(0))
    }

    fn raw_history(&self) -> &[Value] {
        self.history.raw_items()
    }

    fn prompt_history(&self) -> Vec<Value> {
        self.history.for_prompt()
    }

    fn history_len(&self) -> usize {
        self.history.len()
    }

    fn record_items(&mut self, items: impl IntoIterator<Item = Value>) {
        self.history.record_items(items);
    }

    fn update_token_info(&mut self, usage: Option<&Usage>) {
        self.history.update_token_info(usage);
    }

    fn active_context_tokens(&self, server_reasoning_included: bool) -> u64 {
        self.history
            .active_context_tokens(server_reasoning_included)
    }

    fn install_compaction(&mut self, item: Value) {
        let history =
            compaction::install_history(self.history.raw_items(), &self.canonical_context, item);
        self.history.replace(history);
        self.delta_start = None;
        self.previous_response_id = None;
    }

    fn reset_for_full_request(&mut self) {
        self.delta_start = None;
        self.previous_response_id = None;
    }

    fn replace_last_turn_images(&mut self, placeholder: &str) -> bool {
        self.history.replace_last_turn_images(placeholder)
    }
}

impl<'a, W: Write> ModelRun<'a, W> {
    pub(super) fn new(
        events: &'a mut EventWriter<W>,
        task: &'a Task,
        config: &'a ModelConfig,
    ) -> Self {
        Self {
            events,
            task,
            config,
            started_at: Instant::now(),
            stats: RunStats::default(),
            server_reasoning_included: false,
            turn_state: None,
        }
    }

    pub(super) async fn execute(mut self) -> Result<()> {
        self.events.emit(
            "run.started",
            RunStarted {
                mode: "openai_model",
                model: &self.config.model,
                effort: self.config.effort.as_str(),
                transport: TRANSPORT,
                orchestration: ModelConfig::orchestration(),
                websocket_url: display_endpoint(&self.config.websocket_url),
                workspace: self.task.workspace.as_deref(),
                instruction_bytes: self.task.instruction.text_bytes(),
            },
        )?;

        let outcome = self.execute_task().await;
        let elapsed = self.started_at.elapsed();
        match outcome {
            Ok(message) => {
                self.events
                    .emit("assistant.message", AssistantMessage { text: &message })?;
                self.events.emit(
                    "run.completed",
                    terminal_payload("completed", elapsed, self.config, &self.stats),
                )
            }
            Err(error) => {
                let message = error.to_string();
                self.events
                    .emit("run.error", RunError { message: &message })?;
                self.events.emit(
                    "run.failed",
                    terminal_payload("failed", elapsed, self.config, &self.stats),
                )?;
                Err(error)
            }
        }
    }

    async fn execute_task(&mut self) -> Result<String> {
        let workspace = resolve_workspace(self.task.workspace.as_deref())?;
        let project_instructions = load_project_instructions(Path::new(&workspace))?;
        let tools = ToolRuntime::new(
            &workspace,
            WebSearchConfig {
                endpoint: self.config.search_endpoint(),
                api_key: self.config.api_key.clone(),
            },
            ImageGenerationConfig {
                api_base_url: self.config.api_base_url.clone(),
                api_key: self.config.api_key.clone(),
                save_root: std::env::temp_dir().join("harness"),
            },
        );
        let profile = RequestProfile::new(self.events.request_id(), &tools);
        let user_content = prepare_user_input(&self.task.instruction).await;
        let history = task_input(
            &user_content,
            &workspace,
            tools.default_shell_name(),
            project_instructions.as_deref(),
        );
        let mut conversation = ConversationState::new(history)?;
        let mut socket = self
            .connect_with_warmup_fallback(&mut conversation, &profile)
            .await?;

        loop {
            let call_index = self.stats.model_calls + 1;
            let response = match self
                .perform_model_call(&mut socket, call_index, &conversation, &profile)
                .await
            {
                Ok(response) => response,
                Err(HarnessError::Responses(ResponsesError::InvalidImageRequest { .. }))
                    if conversation.replace_last_turn_images("Invalid image") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            conversation.update_token_info(response.usage.as_ref());
            conversation.previous_response_id = Some(response.id.clone());
            let end_turn = response.end_turn;
            let final_message = response.final_message;
            let code_calls = response.code_calls;
            conversation.record_items(response.output_items.into_iter().map(strip_item_id));

            if code_calls.is_empty() {
                if end_turn == Some(false) {
                    conversation.delta_start = Some(conversation.history_len());
                    self.maybe_compact(&mut socket, call_index, &mut conversation, &profile)
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

            conversation.delta_start = Some(conversation.history_len());
            for call in code_calls {
                let output = self
                    .execute_model_tool(&tools, call_index, call, conversation.raw_history())
                    .await?;
                conversation.record_items(output);
            }
            self.maybe_compact(&mut socket, call_index, &mut conversation, &profile)
                .await?;
        }
    }

    async fn execute_model_tool(
        &mut self,
        tools: &ToolRuntime,
        call_index: u32,
        call: CodeCall,
        history: &[Value],
    ) -> Result<Vec<Value>> {
        let arguments = match &call.kind {
            CodeCallKind::Custom => Value::String(call.input.clone()),
            CodeCallKind::Function => serde_json::from_str(&call.input)
                .unwrap_or_else(|_| Value::String(call.input.clone())),
        };
        self.events.emit(
            "tool.call",
            ToolCallEvent {
                call_id: &call.call_id,
                tool: &call.name,
                arguments: &arguments,
                model_call_index: call_index,
            },
        )?;
        self.stats.tool_calls += 1;
        if let Some(message) = unsupported_tool_message(&call) {
            let output = ToolOutputBody::Text(message);
            self.events.emit(
                "tool.result",
                ToolResultEvent {
                    call_id: &call.call_id,
                    tool: &call.name,
                    status: "failed",
                    duration_ns: 0,
                    result: &output,
                    metadata: None,
                },
            )?;
            return Ok(vec![match &call.kind {
                CodeCallKind::Custom => custom_tool_output(&call.call_id, &output),
                CodeCallKind::Function => function_tool_output(&call.call_id, &output),
            }]);
        }
        let started_at = Instant::now();
        let context = ToolContext {
            model: &self.config.model,
            session_id: self.events.request_id(),
            call_id: &call.call_id,
            history,
        };
        let mut execution = if call.name == "exec" {
            tools.execute_code(&call.input, context).await
        } else {
            tools.wait_for_code(&call.input, context).await
        };
        prepare_output_images(&mut execution.output).await;
        let duration_ns = elapsed_ns(started_at);
        self.stats.tool_wall_duration_ns += duration_ns;
        for nested in &execution.nested_calls {
            self.emit_nested_tool(call_index, &call.call_id, nested)?;
        }
        self.events.emit(
            "tool.result",
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
            CodeCallKind::Custom => custom_tool_output(&call.call_id, &execution.output),
            CodeCallKind::Function => function_tool_output(&call.call_id, &execution.output),
        };
        let mut outputs = Vec::with_capacity(execution.notifications.len() + 1);
        outputs.push(output);
        outputs.extend(execution.notifications.into_iter().map(|notification| {
            custom_tool_notification(&notification.call_id, &notification.text)
        }));
        Ok(outputs)
    }

    async fn connect_with_warmup_fallback(
        &mut self,
        conversation: &mut ConversationState,
        profile: &RequestProfile,
    ) -> Result<ResponsesSocket> {
        let socket = match self.connect(ConnectionPurpose::Initial).await {
            Ok(mut socket) => match self.perform_warmup(&mut socket, profile).await {
                Ok(response_id) => {
                    conversation.previous_response_id = Some(response_id);
                    socket
                }
                Err(HarnessError::Responses(_)) => {
                    self.capture_turn_state(&socket);
                    drop(socket);
                    conversation.reset_for_full_request();
                    self.stats.last_response_id = None;
                    self.connect(ConnectionPurpose::WarmupFallback).await?
                }
                Err(error) => return Err(error),
            },
            Err(HarnessError::Responses(_)) => {
                conversation.reset_for_full_request();
                self.stats.last_response_id = None;
                self.connect(ConnectionPurpose::WarmupFallback).await?
            }
            Err(error) => return Err(error),
        };
        Ok(socket)
    }

    async fn maybe_compact(
        &mut self,
        socket: &mut ResponsesSocket,
        after_model_call_index: u32,
        conversation: &mut ConversationState,
        profile: &RequestProfile,
    ) -> Result<()> {
        let Some(auto_compact_token_limit) =
            compaction::auto_compact_token_limit(&self.config.model)
        else {
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
        let (item, usage) = self
            .perform_compaction(
                socket,
                after_model_call_index,
                conversation.delta(),
                &conversation.prompt_history(),
                previous_response_id,
                active_context_tokens,
                auto_compact_token_limit,
                profile,
            )
            .await?;
        conversation.update_token_info(usage.as_ref());
        conversation.install_compaction(item);
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
            "tool.call",
            ToolCallEvent {
                call_id: &call_id,
                tool: &call.name,
                arguments: &call.input,
                model_call_index: call_index,
            },
        )?;
        self.events.emit(
            "tool.result",
            ToolResultEvent {
                call_id: &call_id,
                tool: &call.name,
                status: status(call.success),
                duration_ns: call.duration_ns,
                result: &call.output,
                metadata: call.metadata.as_ref(),
            },
        )?;
        self.stats.tool_calls += 1;
        self.stats.tool_work_duration_ns += call.duration_ns;
        Ok(())
    }

    async fn perform_warmup(
        &mut self,
        socket: &mut ResponsesSocket,
        profile: &RequestProfile,
    ) -> Result<String> {
        self.capture_turn_state(socket);
        let request = EncodedRequest::new(&ResponseCreate::warmup(
            self.config,
            profile,
            self.turn_state.as_deref(),
        ))?;
        let started_at = Instant::now();
        self.events.emit(
            "model.warmup.started",
            WarmupStarted {
                model: &self.config.model,
                prompt_cache_key: profile.prompt_cache_key(),
            },
        )?;
        self.emit_outbound("warmup", None, &request)?;
        if let Err(error) = socket.send(&request).await {
            return self.warmup_failed(started_at, error.into());
        }

        loop {
            let text = match socket.next_text_or_idle_timeout().await {
                Ok(text) => text,
                Err(error) => return self.warmup_failed(started_at, error.into()),
            };
            let raw_event = match parse_raw_json(text.as_str()) {
                Ok(event) => event,
                Err(error) => return self.warmup_failed(started_at, error.into()),
            };
            self.events.emit(
                "api.event",
                ApiEvent {
                    direction: "inbound",
                    transport: TRANSPORT,
                    phase: "warmup",
                    model_call_index: None,
                    event: raw_event,
                },
            )?;
            let event = match decode_event::<WarmupServerEvent>(raw_event) {
                Ok(event) => event,
                Err(error) => return self.warmup_failed(started_at, error.into()),
            };
            match event {
                WarmupServerEvent::Completed { response } => {
                    self.capture_turn_state(socket);
                    let duration_ns = elapsed_ns(started_at);
                    self.stats.warmup_duration_ns += duration_ns;
                    if let Some(usage) = &response.usage {
                        self.stats.warmup_usage.add(usage);
                    }
                    self.stats.last_response_id = Some(response.id.clone());
                    self.events.emit(
                        "model.warmup.completed",
                        WarmupCompleted {
                            response_id: &response.id,
                            duration_ns,
                            usage: response.usage.as_ref(),
                        },
                    )?;
                    return Ok(response.id);
                }
                WarmupServerEvent::Error
                | WarmupServerEvent::Failed
                | WarmupServerEvent::Incomplete => {
                    let error = ResponsesError::Api {
                        event: raw_event.get().to_owned(),
                    };
                    return self.warmup_failed(started_at, error.into());
                }
                WarmupServerEvent::Created { response } => {
                    self.stats.last_response_id = Some(response.id);
                }
                WarmupServerEvent::Other => {}
            }
        }
    }

    fn warmup_failed<T>(&mut self, started_at: Instant, error: HarnessError) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.warmup_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            "model.warmup.failed",
            WarmupFailed {
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }

    async fn connect(&mut self, purpose: ConnectionPurpose) -> Result<ResponsesSocket> {
        let started_at = Instant::now();
        self.stats.connection_attempts += 1;
        let attempt = self.stats.connection_attempts;
        self.events.emit(
            "model.connection.started",
            ConnectionStarted {
                transport: TRANSPORT,
                websocket_url: display_endpoint(&self.config.websocket_url),
                attempt,
                purpose,
            },
        )?;
        let connection = ResponsesSocket::connect(
            &self.config.websocket_url,
            &self.config.api_key,
            self.events.request_id(),
        )
        .await;
        let duration_ns = elapsed_ns(started_at);
        self.stats.connection_duration_ns += duration_ns;
        let (socket, metadata) = match connection {
            Ok(connection) => connection,
            Err(error) => {
                let message = error.to_string();
                self.events.emit(
                    "model.connection.failed",
                    ConnectionFailed {
                        transport: TRANSPORT,
                        attempt,
                        purpose,
                        duration_ns,
                        error: &message,
                    },
                )?;
                return Err(error.into());
            }
        };
        if matches!(
            purpose,
            ConnectionPurpose::WarmupFallback | ConnectionPurpose::Reconnect
        ) {
            self.stats.websocket_reconnects += 1;
        }
        if metadata.turn_state.is_some() {
            self.turn_state.clone_from(&metadata.turn_state);
        }
        self.server_reasoning_included |= metadata.reasoning_included;
        self.events.emit(
            "model.connection.completed",
            ConnectionCompleted {
                transport: TRANSPORT,
                attempt,
                purpose,
                duration_ns,
                http_status: metadata.status,
                request_id: metadata.request_id.as_deref(),
                server_model: metadata.server_model.as_deref(),
                server_reasoning_included: metadata.reasoning_included,
            },
        )?;
        Ok(socket)
    }

    async fn perform_model_call(
        &mut self,
        socket: &mut ResponsesSocket,
        call_index: u32,
        conversation: &ConversationState,
        profile: &RequestProfile,
    ) -> Result<super::stream::TurnResult> {
        self.capture_turn_state(socket);
        let full_input = conversation
            .delta_start
            .is_none()
            .then(|| profile.full_input(&conversation.prompt_history()));
        let input = full_input.as_deref().unwrap_or(conversation.delta());
        let previous_response_id = conversation.previous_response_id.as_deref();
        let request = EncodedRequest::new(&ResponseCreate::generation(
            self.config,
            input,
            previous_response_id,
            profile,
            self.turn_state.as_deref(),
        ))?;
        let started_at = Instant::now();
        self.stats.model_calls += 1;
        self.events.emit(
            "model.call.started",
            ModelCallStarted {
                call_index,
                model: &self.config.model,
                effort: self.config.effort.as_str(),
                previous_response_id,
            },
        )?;
        self.emit_outbound("generation", Some(call_index), &request)?;

        if let Err(error) = socket.send(&request).await {
            if !error.is_reconnectable_send() {
                return self.model_call_failed(call_index, started_at, error.into());
            }
            let reason = error.to_string();
            self.events.emit(
                "model.connection.retrying",
                ConnectionRetry {
                    call_index,
                    previous_response_id,
                    reason: &reason,
                },
            )?;
            self.capture_turn_state(socket);
            *socket = self.connect(ConnectionPurpose::Reconnect).await?;
            let full_input = profile.full_input(&conversation.prompt_history());
            let replay = EncodedRequest::new(&ResponseCreate::generation(
                self.config,
                &full_input,
                None,
                profile,
                self.turn_state.as_deref(),
            ))?;
            self.emit_outbound("generation", Some(call_index), &replay)?;
            if let Err(error) = socket.send(&replay).await {
                return self.model_call_failed(call_index, started_at, error.into());
            }
        }

        let response =
            match super::stream::receive(socket, self.events, call_index, started_at).await {
                Ok(response) => response,
                Err(error) => return self.model_call_failed(call_index, started_at, error),
            };
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        if let Some(usage) = &response.usage {
            self.stats.usage.add(usage);
        }
        self.stats.last_response_id = Some(response.id.clone());
        self.events.emit(
            "model.call.completed",
            ModelCallCompleted {
                call_index,
                model: &self.config.model,
                response_id: &response.id,
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
        socket: &mut ResponsesSocket,
        after_model_call_index: u32,
        delta: &[Value],
        history: &[Value],
        previous_response_id: &str,
        active_context_tokens: u64,
        auto_compact_token_limit: u64,
        profile: &RequestProfile,
    ) -> Result<(Value, Option<Usage>)> {
        self.capture_turn_state(socket);
        let trigger = compaction::trigger();
        let delta_start =
            history
                .len()
                .checked_sub(delta.len())
                .ok_or(AgentError::MalformedResponse {
                    detail: "compaction delta was longer than conversation history",
                })?;
        let mut compaction_history = history.to_vec();
        compaction::trim_tool_outputs_to_fit_context_window(
            &mut compaction_history,
            active_context_tokens,
        );
        let mut incremental_input = Vec::with_capacity(delta.len() + 1);
        incremental_input.extend_from_slice(&compaction_history[delta_start..]);
        incremental_input.push(trigger.clone());
        let request = EncodedRequest::new(&ResponseCreate::generation(
            self.config,
            &incremental_input,
            Some(previous_response_id),
            profile,
            self.turn_state.as_deref(),
        ))?;
        let started_at = Instant::now();
        self.stats.compactions += 1;
        self.events.emit(
            "model.compaction.started",
            CompactionStarted {
                after_model_call_index,
                active_context_tokens,
                auto_compact_token_limit,
                previous_response_id,
            },
        )?;
        self.emit_outbound("compaction", Some(after_model_call_index), &request)?;

        if let Err(error) = socket.send(&request).await {
            if !error.is_reconnectable_send() {
                return self.compaction_failed(after_model_call_index, started_at, error.into());
            }
            let reason = error.to_string();
            self.events.emit(
                "model.connection.retrying",
                ConnectionRetry {
                    call_index: after_model_call_index,
                    previous_response_id: Some(previous_response_id),
                    reason: &reason,
                },
            )?;
            self.capture_turn_state(socket);
            *socket = self.connect(ConnectionPurpose::Reconnect).await?;
            let mut replay_input = profile.full_input(&compaction_history);
            replay_input.push(trigger);
            let replay = EncodedRequest::new(&ResponseCreate::generation(
                self.config,
                &replay_input,
                None,
                profile,
                self.turn_state.as_deref(),
            ))?;
            self.emit_outbound("compaction", Some(after_model_call_index), &replay)?;
            if let Err(error) = socket.send(&replay).await {
                return self.compaction_failed(after_model_call_index, started_at, error.into());
            }
        }

        let response = match super::stream::receive_compaction(
            socket,
            self.events,
            after_model_call_index,
            started_at,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                return self.compaction_failed(after_model_call_index, started_at, error);
            }
        };
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        if let Some(usage) = &response.usage {
            self.stats.usage.add(usage);
        }
        self.stats.last_response_id = Some(response.id.clone());
        self.events.emit(
            "model.compaction.completed",
            CompactionCompleted {
                after_model_call_index,
                response_id: &response.id,
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
        error: crate::HarnessError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            "model.compaction.failed",
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
        error: crate::HarnessError,
    ) -> Result<T> {
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        let message = error.to_string();
        self.events.emit(
            "model.call.failed",
            ModelCallFailed {
                call_index,
                model: &self.config.model,
                duration_ns,
                error: &message,
            },
        )?;
        Err(error)
    }

    fn emit_outbound(
        &mut self,
        phase: &'static str,
        call_index: Option<u32>,
        request: &EncodedRequest,
    ) -> Result<()> {
        self.events.emit(
            "api.event",
            ApiEvent {
                direction: "outbound",
                transport: TRANSPORT,
                phase,
                model_call_index: call_index,
                event: request.raw(),
            },
        )
    }

    fn capture_turn_state(&mut self, socket: &ResponsesSocket) {
        if let Some(turn_state) = socket.turn_state() {
            self.turn_state = Some(turn_state.to_owned());
        }
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

fn strip_item_id(mut item: Value) -> Value {
    if let Some(object) = item.as_object_mut() {
        object.remove("id");
    }
    item
}

const fn status(success: bool) -> &'static str {
    if success { "completed" } else { "failed" }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
