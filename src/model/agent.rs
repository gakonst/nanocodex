use std::{io::Write, time::Instant};

use futures_util::future::join_all;
use serde::Serialize;
use serde_json::Value;

use super::{
    AssistantMessage, FunctionCall, ModelConfig, ModelResponse, RunError, RunStarted, RunStats,
    TRANSPORT, display_endpoint, elapsed_ns, resolve_workspace, terminal_payload,
    wire::{Caller, InputItem, Usage},
};
use crate::{
    AgentError, Result,
    protocol::{EventWriter, Task},
    responses::ResponsesSocket,
    shell::{self, ExecCommandArgs},
};

struct ToolOutcome {
    status: &'static str,
    result: shell::ExecCommandResult,
}

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
    event: &'a T,
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
    function_calls: usize,
    usage: &'a Usage,
}

#[derive(Serialize)]
struct ModelCallFailed<'a> {
    call_index: u32,
    model: &'a str,
    duration_ns: u64,
    error: &'a str,
}

#[derive(Serialize)]
struct ToolCallEvent<'a> {
    call_id: &'a str,
    tool: &'a str,
    arguments: &'a Value,
    model_call_index: u32,
    caller: &'a Caller,
}

#[derive(Serialize)]
struct ToolResultEvent<'a> {
    call_id: &'a str,
    tool: &'a str,
    status: &'static str,
    duration_ns: u64,
    result: &'a shell::ExecCommandResult,
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
                websocket_url: display_endpoint(&self.config.websocket_url),
                workspace: self.task.workspace.as_deref(),
                instruction_bytes: self.task.instruction.len(),
                max_model_calls: self.config.max_model_calls,
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
        let mut input = super::wire::initial_input(self.task, &workspace);
        let mut previous_response_id: Option<String> = None;

        for call_index in 1..=self.config.max_model_calls {
            let response = self
                .perform_model_call(
                    &mut socket,
                    call_index,
                    &input,
                    previous_response_id.as_deref(),
                )
                .await?;
            previous_response_id = Some(response.id.clone());
            if response.function_calls.is_empty() {
                input.clear();
                if response.has_message {
                    return Ok(if response.text.trim().is_empty() {
                        "The model completed without emitting assistant text.".to_owned()
                    } else {
                        response.text
                    });
                }
                continue;
            }

            input = self
                .execute_function_calls(response.function_calls, &workspace, call_index)
                .await?;
        }

        Err(AgentError::ModelCallLimit {
            limit: self.config.max_model_calls,
        }
        .into())
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
        let (socket, metadata) =
            ResponsesSocket::connect(&self.config.websocket_url, &self.config.api_key).await?;
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
    ) -> Result<ModelResponse> {
        let request = super::wire::response_create(self.config, input, previous_response_id);
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
                event: &request,
            },
        )?;
        let response = match async {
            socket.send(&request).await?;
            super::stream::receive(socket, self.events, call_index, started_at).await
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
                function_calls: response.function_calls.len(),
                usage: &response.usage,
            },
        )?;
        Ok(response)
    }

    async fn execute_function_calls(
        &mut self,
        function_calls: Vec<FunctionCall>,
        workspace: &str,
        call_index: u32,
    ) -> Result<Vec<InputItem>> {
        for function_call in &function_calls {
            let event_arguments = serde_json::from_str::<Value>(&function_call.arguments)
                .unwrap_or_else(|_| Value::String(function_call.arguments.clone()));
            self.stats.tool_calls += 1;
            self.events.emit(
                "tool.call",
                ToolCallEvent {
                    call_id: &function_call.call_id,
                    tool: &function_call.name,
                    arguments: &event_arguments,
                    model_call_index: call_index,
                    caller: &function_call.caller,
                },
            )?;
        }

        let completed = join_all(function_calls.into_iter().map(|function_call| async move {
            let parsed_arguments =
                serde_json::from_str::<ExecCommandArgs>(&function_call.arguments);
            let started_at = Instant::now();
            let outcome = execute_tool(&function_call.name, parsed_arguments, workspace).await;
            (function_call, outcome, elapsed_ns(started_at))
        }))
        .await;

        let mut outputs = Vec::with_capacity(completed.len());
        for (function_call, outcome, duration_ns) in completed {
            self.stats.tool_duration_ns += duration_ns;
            self.events.emit(
                "tool.result",
                ToolResultEvent {
                    call_id: &function_call.call_id,
                    tool: &function_call.name,
                    status: outcome.status,
                    duration_ns,
                    result: &outcome.result,
                },
            )?;
            outputs.push(super::wire::function_call_output(
                function_call.call_id,
                serde_json::to_string(&outcome.result).map_err(AgentError::EncodeToolResult)?,
                function_call.caller,
            ));
        }
        Ok(outputs)
    }
}

async fn execute_tool(
    name: &str,
    arguments: std::result::Result<ExecCommandArgs, serde_json::Error>,
    workspace: &str,
) -> ToolOutcome {
    if name != "exec_command" {
        return tool_error(format!("unknown tool: {name}"));
    }
    let args = match arguments {
        Ok(arguments) => arguments,
        Err(error) => return tool_error(format!("invalid JSON arguments: {error}")),
    };
    let result = shell::execute_command(args, workspace).await;
    let status = if result.succeeded() {
        "completed"
    } else {
        "failed"
    };
    ToolOutcome { status, result }
}

fn tool_error(message: String) -> ToolOutcome {
    ToolOutcome {
        status: "error",
        result: shell::ExecCommandResult::tool_error(message),
    }
}
