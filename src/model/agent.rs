use std::{io::Write, time::Instant};

use futures_util::future::join_all;
use serde::Serialize;
use serde_json::Value;

use super::{
    FunctionCall, ModelConfig, ModelResponse, RunStats, TRANSPORT, display_endpoint, elapsed_ns,
    wire::{Caller, InputItem, Usage},
};
use crate::{
    AgentError, HarnessError, Result,
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

pub(super) async fn run<W: Write>(
    events: &mut EventWriter<W>,
    task: &Task,
    workspace: &str,
    config: &ModelConfig,
    run_stats: &mut RunStats,
) -> Result<String> {
    validate_config(config)?;
    let mut socket = connect(events, config).await?;
    let mut input = super::wire::initial_input(task, workspace);
    let mut previous_response_id: Option<String> = None;

    for call_index in 1..=config.max_model_calls {
        let response = perform_model_call(
            events,
            &mut socket,
            config,
            run_stats,
            call_index,
            &input,
            previous_response_id.as_deref(),
        )
        .await?;
        if response.function_calls.is_empty() {
            previous_response_id = Some(response.id);
            input = Vec::new();
            if response.has_message {
                return Ok(if response.text.trim().is_empty() {
                    "The model completed without emitting assistant text.".to_owned()
                } else {
                    response.text
                });
            }
            continue;
        }

        input = execute_function_calls(
            events,
            &response.function_calls,
            workspace,
            call_index,
            run_stats,
        )
        .await?;
        previous_response_id = Some(response.id);
    }

    Err(AgentError::ModelCallLimit {
        limit: config.max_model_calls,
    }
    .into())
}

fn validate_config(config: &ModelConfig) -> Result<()> {
    if config
        .api_key
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(HarnessError::Configuration(
            "OPENAI_API_KEY or --api-key is required in model mode".to_owned(),
        ));
    }
    if config.model.trim().is_empty() {
        return Err(HarnessError::Configuration(
            "model must not be empty".to_owned(),
        ));
    }
    if config.max_model_calls == 0 {
        return Err(HarnessError::Configuration(
            "max_model_calls must be at least 1".to_owned(),
        ));
    }
    Ok(())
}

async fn connect<W: Write>(
    events: &mut EventWriter<W>,
    config: &ModelConfig,
) -> Result<ResponsesSocket> {
    let started_at = Instant::now();
    events.emit(
        "model.connection.started",
        ConnectionStarted {
            transport: TRANSPORT,
            websocket_url: display_endpoint(&config.websocket_url),
        },
    )?;
    let api_key = config.api_key.as_deref().ok_or_else(|| {
        HarnessError::Configuration("OPENAI_API_KEY or --api-key is required".to_owned())
    })?;
    let (socket, metadata) = ResponsesSocket::connect(&config.websocket_url, api_key).await?;
    events.emit(
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

#[allow(clippy::too_many_arguments)]
async fn perform_model_call<W: Write>(
    events: &mut EventWriter<W>,
    socket: &mut ResponsesSocket,
    config: &ModelConfig,
    run_stats: &mut RunStats,
    call_index: u32,
    input: &[InputItem],
    previous_response_id: Option<&str>,
) -> Result<ModelResponse> {
    let request = super::wire::response_create(config, input, previous_response_id);
    let started_at = Instant::now();
    run_stats.model_calls = run_stats.model_calls.saturating_add(1);
    events.emit(
        "model.call.started",
        ModelCallStarted {
            call_index,
            model: &config.model,
            effort: config.effort.as_str(),
            previous_response_id,
        },
    )?;
    events.emit(
        "api.event",
        OutboundApiEvent {
            direction: "outbound",
            transport: TRANSPORT,
            event: &request,
        },
    )?;
    let response = match async {
        socket.send(&request).await?;
        super::stream::receive(socket, events, call_index, started_at).await
    }
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let duration_ns = elapsed_ns(started_at);
            run_stats.model_duration_ns = run_stats.model_duration_ns.saturating_add(duration_ns);
            let message = error.to_string();
            events.emit(
                "model.call.failed",
                ModelCallFailed {
                    call_index,
                    model: &config.model,
                    duration_ns,
                    error: &message,
                },
            )?;
            return Err(error);
        }
    };
    let duration_ns = elapsed_ns(started_at);
    run_stats.model_duration_ns = run_stats.model_duration_ns.saturating_add(duration_ns);
    run_stats.usage.add(&response.usage);
    run_stats.last_response_id = Some(response.id.clone());
    events.emit(
        "model.call.completed",
        ModelCallCompleted {
            call_index,
            model: &config.model,
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

async fn execute_function_calls<W: Write>(
    events: &mut EventWriter<W>,
    function_calls: &[FunctionCall],
    workspace: &str,
    call_index: u32,
    run_stats: &mut RunStats,
) -> Result<Vec<InputItem>> {
    for function_call in function_calls {
        let event_arguments = serde_json::from_str::<Value>(&function_call.arguments);
        let event_arguments = event_arguments.as_ref().map_or_else(
            |_| Value::String(function_call.arguments.clone()),
            Clone::clone,
        );
        run_stats.tool_calls = run_stats.tool_calls.saturating_add(1);
        events.emit(
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

    let completed = join_all(function_calls.iter().map(|function_call| async move {
        let parsed_arguments = serde_json::from_str::<ExecCommandArgs>(&function_call.arguments);
        let started_at = Instant::now();
        let outcome = execute_tool(&function_call.name, parsed_arguments, workspace).await?;
        Ok::<_, HarnessError>((function_call, outcome, elapsed_ns(started_at)))
    }))
    .await;

    let mut outputs = Vec::with_capacity(completed.len());
    for completed_call in completed {
        let (function_call, outcome, duration_ns) = completed_call?;
        run_stats.tool_duration_ns = run_stats.tool_duration_ns.saturating_add(duration_ns);
        events.emit(
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
            function_call.call_id.clone(),
            serde_json::to_string(&outcome.result).map_err(AgentError::EncodeToolResult)?,
            function_call.caller.clone(),
        ));
    }
    Ok(outputs)
}

async fn execute_tool(
    name: &str,
    arguments: std::result::Result<ExecCommandArgs, serde_json::Error>,
    workspace: &str,
) -> Result<ToolOutcome> {
    if name != "exec_command" {
        return Ok(tool_error(format!("unknown tool: {name}")));
    }
    let args = match arguments {
        Ok(arguments) => arguments,
        Err(error) => return Ok(tool_error(format!("invalid JSON arguments: {error}"))),
    };
    let result = shell::execute_command(args, workspace).await;
    let status = if result.succeeded() {
        "completed"
    } else {
        "failed"
    };
    Ok(ToolOutcome { status, result })
}

fn tool_error(message: String) -> ToolOutcome {
    ToolOutcome {
        status: "error",
        result: shell::ExecCommandResult::tool_error(message),
    }
}
