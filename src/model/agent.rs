use std::{io::Write, time::Instant};

use serde::Serialize;

use super::{
    AssistantMessage, MAX_CONCURRENT_SUBAGENTS, ModelConfig, RunError, RunStarted, RunStats,
    TRANSPORT, display_endpoint, elapsed_ns, resolve_workspace, terminal_payload,
    wire::{InputItem, RequestProfile, ResponseCreate, Usage, WarmupServerEvent},
};
use crate::{
    AgentError, ResponsesError, Result,
    protocol::{EventWriter, Task},
    responses::ResponsesSocket,
};

#[derive(Serialize)]
struct ConnectionStarted<'a> {
    transport: &'static str,
    websocket_url: &'a str,
}

#[derive(Serialize)]
struct ConnectionCompleted<'a> {
    transport: &'static str,
    duration_ns: u64,
    http_status: u16,
    request_id: Option<&'a str>,
    server_model: Option<&'a str>,
    server_reasoning_included: bool,
}

#[derive(Serialize)]
struct ModelCallStarted<'a> {
    call_index: u32,
    model: &'a str,
    effort: &'static str,
    previous_response_id: Option<&'a str>,
}

#[derive(Serialize)]
struct OutboundApiEvent<'a, T: Serialize + ?Sized> {
    direction: &'static str,
    transport: &'static str,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_call_index: Option<u32>,
    event: &'a T,
}

#[derive(Serialize)]
struct WarmupStarted<'a> {
    model: &'a str,
    prompt_cache_key: &'a str,
    compact_threshold: u64,
}

#[derive(Serialize)]
struct WarmupCompleted<'a> {
    response_id: &'a str,
    duration_ns: u64,
    usage: Option<&'a Usage>,
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
    usage: &'a Usage,
}

#[derive(Serialize)]
struct ModelCallFailed<'a> {
    call_index: u32,
    model: &'a str,
    duration_ns: u64,
    error: &'a str,
}

pub(super) struct ModelRun<'a, W> {
    events: &'a mut EventWriter<W>,
    task: &'a Task,
    config: &'a ModelConfig,
    started_at: Instant,
    stats: RunStats,
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
                orchestration: self.config.orchestration(),
                websocket_url: display_endpoint(&self.config.websocket_url),
                workspace: self.task.workspace.as_deref(),
                instruction_bytes: self.task.instruction.len(),
                max_model_calls: self.config.max_model_calls,
                compact_threshold: self.config.compact_threshold,
                multi_agent: self.config.multi_agent,
                max_concurrent_subagents: self
                    .config
                    .multi_agent
                    .then_some(MAX_CONCURRENT_SUBAGENTS),
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
        let mut socket = self.connect().await?;
        let profile = RequestProfile::new(self.config);
        let initial_input = InputItem::for_task(self.task, &workspace, self.config);
        let warmup_response_id = self
            .perform_warmup(&mut socket, &initial_input, &profile)
            .await?;
        let mut previous_response_id = Some(warmup_response_id);
        let mut input = Vec::new();

        for call_index in 1..=self.config.max_model_calls {
            let response = self
                .perform_model_call(
                    &mut socket,
                    call_index,
                    &input,
                    previous_response_id.as_deref(),
                    &profile,
                    &workspace,
                )
                .await?;
            previous_response_id = Some(response.id);
            input = response.next_input;
            if input.is_empty() {
                if let Some(message) = response.final_message {
                    return Ok(if message.trim().is_empty() {
                        "The model completed without emitting assistant text.".to_owned()
                    } else {
                        message
                    });
                }
            }
        }

        Err(AgentError::ModelCallLimit {
            limit: self.config.max_model_calls,
        }
        .into())
    }

    async fn perform_warmup(
        &mut self,
        socket: &mut ResponsesSocket,
        input: &[InputItem],
        profile: &RequestProfile,
    ) -> Result<String> {
        let request = ResponseCreate::warmup(self.config, input, profile);
        let started_at = Instant::now();
        self.events.emit(
            "model.warmup.started",
            WarmupStarted {
                model: &self.config.model,
                prompt_cache_key: profile.prompt_cache_key(),
                compact_threshold: self.config.compact_threshold,
            },
        )?;
        self.events.emit(
            "api.event",
            OutboundApiEvent {
                direction: "outbound",
                transport: TRANSPORT,
                phase: "warmup",
                model_call_index: None,
                event: &request,
            },
        )?;
        socket.send(&request).await?;

        loop {
            let raw_event = socket.next_json().await?;
            self.events.emit(
                "api.event",
                OutboundApiEvent {
                    direction: "inbound",
                    transport: TRANSPORT,
                    phase: "warmup",
                    model_call_index: None,
                    event: &raw_event,
                },
            )?;
            let event = serde_json::from_value::<WarmupServerEvent>(raw_event.clone()).map_err(
                |source| ResponsesError::InvalidPayload {
                    source,
                    event: Box::new(raw_event.clone()),
                },
            )?;
            match event {
                WarmupServerEvent::Completed { response } => {
                    let duration_ns = elapsed_ns(started_at);
                    self.stats.warmup_duration_ns += duration_ns;
                    if let Some(usage) = &response.usage {
                        self.stats.warmup_usage.add(usage);
                    }
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
                    return Err(ResponsesError::Api {
                        event: Box::new(raw_event),
                    }
                    .into());
                }
                WarmupServerEvent::Created { response } => {
                    self.stats.last_response_id = Some(response.id);
                }
                WarmupServerEvent::Other => {}
            }
        }
    }

    async fn connect(&mut self) -> Result<ResponsesSocket> {
        let started_at = Instant::now();
        self.events.emit(
            "model.connection.started",
            ConnectionStarted {
                transport: TRANSPORT,
                websocket_url: display_endpoint(&self.config.websocket_url),
            },
        )?;
        let (socket, metadata) = ResponsesSocket::connect(
            &self.config.websocket_url,
            &self.config.api_key,
            self.config.multi_agent,
        )
        .await?;
        self.events.emit(
            "model.connection.completed",
            ConnectionCompleted {
                transport: TRANSPORT,
                duration_ns: elapsed_ns(started_at),
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
        input: &[InputItem],
        previous_response_id: Option<&str>,
        profile: &RequestProfile,
        workspace: &str,
    ) -> Result<super::stream::TurnResult> {
        let request = previous_response_id.map_or_else(
            || ResponseCreate::initial(self.config, input, profile),
            |previous_response_id| {
                ResponseCreate::continued(self.config, input, previous_response_id, profile)
            },
        );
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
        self.events.emit(
            "api.event",
            OutboundApiEvent {
                direction: "outbound",
                transport: TRANSPORT,
                phase: "generation",
                model_call_index: Some(call_index),
                event: &request,
            },
        )?;
        let response = match async {
            socket.send(&request).await?;
            super::stream::receive(
                socket,
                self.events,
                &mut self.stats,
                workspace,
                call_index,
                started_at,
                self.config.multi_agent,
            )
            .await
        }
        .await
        {
            Ok(response) => response,
            Err(error) => {
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
                return Err(error);
            }
        };
        let duration_ns = elapsed_ns(started_at);
        self.stats.model_duration_ns += duration_ns;
        self.stats.usage.add(&response.usage);
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
                tool_calls: response.tool_calls,
                usage: &response.usage,
            },
        )?;
        Ok(response)
    }
}
