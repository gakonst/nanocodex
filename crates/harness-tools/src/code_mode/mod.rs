mod description;
mod host;
mod output;

use std::{collections::HashMap, time::Instant};

use harness_core::{CustomToolFormat, ToolDefinition};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::{sync::Mutex, time::Duration};

use super::{ToolContext, ToolHandler, ToolOutputBody, ToolOutputContent, ToolRuntime};
use host::NodeHost;

const INITIAL_YIELD: Duration = if cfg!(test) {
    Duration::from_secs(5)
} else {
    Duration::from_secs(10)
};
const DEFAULT_WAIT_YIELD: Duration = Duration::from_secs(10);
const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const EXEC_PRAGMA_PREFIX: &str = "// @exec:";
const GRAMMAR: &str = r"start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE
PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/";

pub(crate) struct CodeModeRuntime {
    state: Mutex<CodeModeState>,
}

struct CodeModeState {
    host: Option<NodeHost>,
    stored: HashMap<String, Value>,
    next_cell_id: u64,
    live_cell: Option<LiveCell>,
}

struct LiveCell {
    id: u64,
    call_id: String,
}

pub struct CodeModeExecution {
    pub output: ToolOutputBody,
    pub success: bool,
    pub nested_calls: Vec<NestedToolCall>,
    pub notifications: Vec<CodeModeNotification>,
}

pub struct CodeModeNotification {
    pub call_id: String,
    pub text: String,
}

pub struct NestedToolCall {
    pub call_id: String,
    pub name: String,
    pub input: Value,
    pub output: ToolOutputBody,
    pub success: bool,
    pub duration_ns: u64,
    pub metadata: Option<Box<RawValue>>,
}

struct CompletedNestedCall {
    id: u64,
    value: Value,
    call: NestedToolCall,
}

enum CellOutcome {
    Yielded {
        content: Vec<ToolOutputContent>,
        stored: Option<HashMap<String, Value>>,
        nested_calls: Vec<NestedToolCall>,
        notifications: Vec<CodeModeNotification>,
    },
    Completed {
        content: Vec<ToolOutputContent>,
        stored: HashMap<String, Value>,
        nested_calls: Vec<NestedToolCall>,
        notifications: Vec<CodeModeNotification>,
    },
    ScriptFailed {
        message: String,
        content: Vec<ToolOutputContent>,
        stored: HashMap<String, Value>,
        nested_calls: Vec<NestedToolCall>,
        notifications: Vec<CodeModeNotification>,
    },
}

struct HostFailure {
    message: String,
    nested_calls: Vec<NestedToolCall>,
    notifications: Vec<CodeModeNotification>,
}

impl CodeModeRuntime {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(CodeModeState {
                host: None,
                stored: HashMap::new(),
                next_cell_id: 1,
                live_cell: None,
            }),
        }
    }

    pub(super) async fn execute(
        &self,
        source: &str,
        tools: &ToolRuntime,
        context: ToolContext<'_>,
    ) -> CodeModeExecution {
        let started_at = Instant::now();
        let source = match parse_exec_source(source) {
            Ok(source) => source,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        let mut state = self.state.lock().await;
        if let Some(live_cell) = state.live_cell.as_ref() {
            return failed_execution(
                started_at,
                &format!(
                    "exec cell {} is still running; use wait before starting another",
                    live_cell.id
                ),
                Vec::new(),
            );
        }
        let cell_id = state.allocate_cell_id();
        let parent_call_id = context.call_id.to_owned();
        if state.host.is_none() {
            match NodeHost::spawn() {
                Ok(host) => state.host = Some(host),
                Err(message) => return failed_execution(started_at, &message, Vec::new()),
            }
        }

        let stored = state.stored.clone();
        let result = if let Some(host) = state.host.as_mut() {
            match host.start_cell(cell_id, &source.code, stored, tools).await {
                Ok(()) => {
                    let yield_after = source
                        .yield_time_ms
                        .map_or(INITIAL_YIELD, Duration::from_millis);
                    host.drive_cell(cell_id, &parent_call_id, tools, context, yield_after)
                        .await
                }
                Err(error) => Err(error),
            }
        } else {
            return failed_execution(
                started_at,
                "local Node.js code-mode host was unavailable",
                Vec::new(),
            );
        };
        finish_cell(
            &mut state,
            cell_id,
            parent_call_id,
            started_at,
            result,
            source.max_output_tokens,
        )
        .await
    }

    pub(super) async fn wait(
        &self,
        input: &str,
        tools: &ToolRuntime,
        context: ToolContext<'_>,
    ) -> CodeModeExecution {
        let started_at = Instant::now();
        let arguments = match serde_json::from_str::<WaitArguments>(input) {
            Ok(arguments) => arguments,
            Err(error) => {
                return failed_execution(
                    started_at,
                    &format!("failed to parse wait arguments: {error}"),
                    Vec::new(),
                );
            }
        };
        let cell_id = match arguments.cell_id.parse::<u64>() {
            Ok(cell_id) => cell_id,
            Err(error) => {
                return failed_execution(
                    started_at,
                    &format!("invalid exec cell ID `{}`: {error}", arguments.cell_id),
                    Vec::new(),
                );
            }
        };
        let mut state = self.state.lock().await;
        let Some(live_cell) = state.live_cell.as_ref() else {
            return failed_execution(
                started_at,
                &format!("exec cell {cell_id} was not found"),
                Vec::new(),
            );
        };
        if live_cell.id != cell_id {
            return failed_execution(
                started_at,
                &format!("exec cell {cell_id} was not found"),
                Vec::new(),
            );
        }
        let parent_call_id = live_cell.call_id.clone();
        if arguments.terminate {
            terminate_host(&mut state).await;
            return CodeModeExecution {
                output: ToolOutputBody::Text(format!(
                    "Script terminated\nWall time {:.1} seconds\nOutput:\nexec cell {cell_id} was terminated",
                    started_at.elapsed().as_secs_f64()
                )),
                success: true,
                nested_calls: Vec::new(),
                notifications: Vec::new(),
            };
        }
        let yield_time = Duration::from_millis(
            arguments
                .yield_time_ms
                .unwrap_or(u64::try_from(DEFAULT_WAIT_YIELD.as_millis()).unwrap_or(u64::MAX)),
        );
        let result = if let Some(host) = state.host.as_mut() {
            host.drive_cell(cell_id, &parent_call_id, tools, context, yield_time)
                .await
        } else {
            Err(HostFailure::new(
                "local Node.js code-mode host was unavailable".to_owned(),
            ))
        };
        finish_cell(
            &mut state,
            cell_id,
            parent_call_id,
            started_at,
            result,
            arguments.max_tokens,
        )
        .await
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecPragma {
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

struct ParsedExecSource {
    code: String,
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}

fn parse_exec_source(input: &str) -> Result<ParsedExecSource, String> {
    if input.trim().is_empty() {
        return Err(
            "exec expects raw JavaScript source text (non-empty). Provide JS only, optionally with first-line `// @exec: {\"yield_time_ms\": 10000, \"max_output_tokens\": 1000}`."
                .to_owned(),
        );
    }
    let mut source = ParsedExecSource {
        code: input.to_owned(),
        yield_time_ms: None,
        max_output_tokens: None,
    };
    let mut lines = input.splitn(2, '\n');
    let first_line = lines.next().unwrap_or_default();
    let rest = lines.next().unwrap_or_default();
    let Some(pragma) = first_line.trim_start().strip_prefix(EXEC_PRAGMA_PREFIX) else {
        return Ok(source);
    };
    if rest.trim().is_empty() {
        return Err(
            "exec pragma must be followed by JavaScript source on subsequent lines".to_owned(),
        );
    }
    let directive = pragma.trim();
    if directive.is_empty() {
        return Err(
            "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
                .to_owned(),
        );
    }
    let value: Value = serde_json::from_str(directive).map_err(|error| {
        format!(
            "exec pragma must be valid JSON with supported fields `yield_time_ms` and `max_output_tokens`: {error}"
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
            .to_owned()
    })?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "yield_time_ms" | "max_output_tokens"))
    {
        return Err(format!(
            "exec pragma only supports `yield_time_ms` and `max_output_tokens`; got `{key}`"
        ));
    }
    let pragma: ExecPragma = serde_json::from_value(value).map_err(|error| {
        format!(
            "exec pragma fields `yield_time_ms` and `max_output_tokens` must be non-negative safe integers: {error}"
        )
    })?;
    if pragma
        .yield_time_ms
        .is_some_and(|yield_time_ms| yield_time_ms > MAX_JS_SAFE_INTEGER)
    {
        return Err(
            "exec pragma field `yield_time_ms` must be a non-negative safe integer".to_owned(),
        );
    }
    if pragma.max_output_tokens.is_some_and(|max_output_tokens| {
        u64::try_from(max_output_tokens)
            .map(|max_output_tokens| max_output_tokens > MAX_JS_SAFE_INTEGER)
            .unwrap_or(true)
    }) {
        return Err(
            "exec pragma field `max_output_tokens` must be a non-negative safe integer".to_owned(),
        );
    }
    rest.clone_into(&mut source.code);
    source.yield_time_ms = pragma.yield_time_ms;
    source.max_output_tokens = pragma.max_output_tokens;
    Ok(source)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArguments {
    cell_id: String,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_tokens: Option<usize>,
    #[serde(default)]
    terminate: bool,
}

async fn finish_cell(
    state: &mut CodeModeState,
    cell_id: u64,
    parent_call_id: String,
    started_at: Instant,
    result: Result<CellOutcome, HostFailure>,
    max_output_tokens: Option<usize>,
) -> CodeModeExecution {
    let wall_time = started_at.elapsed().as_secs_f64();
    match result {
        Ok(CellOutcome::Yielded {
            content,
            stored,
            nested_calls,
            notifications,
        }) => {
            if let Some(stored) = stored {
                state.stored = stored;
            }
            state.live_cell = Some(LiveCell {
                id: cell_id,
                call_id: parent_call_id,
            });
            let content = output::truncate_content(content, max_output_tokens);
            CodeModeExecution {
                output: with_status(
                    &format!("Script running with cell ID {cell_id}"),
                    wall_time,
                    content,
                ),
                success: true,
                nested_calls,
                notifications,
            }
        }
        Ok(CellOutcome::Completed {
            content,
            stored,
            nested_calls,
            notifications,
        }) => {
            state.live_cell = None;
            state.stored = stored;
            let content = output::truncate_content(content, max_output_tokens);
            CodeModeExecution {
                output: with_status("Script completed", wall_time, content),
                success: true,
                nested_calls,
                notifications,
            }
        }
        Ok(CellOutcome::ScriptFailed {
            message,
            mut content,
            stored,
            nested_calls,
            notifications,
        }) => {
            state.live_cell = None;
            state.stored = stored;
            content.push(ToolOutputContent::InputText {
                text: format!("Script error:\n{message}"),
            });
            let content = output::truncate_content(content, max_output_tokens);
            CodeModeExecution {
                output: with_status("Script failed", wall_time, content),
                success: false,
                nested_calls,
                notifications,
            }
        }
        Err(failure) => {
            terminate_host(state).await;
            let content = output::truncate_content(
                vec![ToolOutputContent::InputText {
                    text: failure.message,
                }],
                max_output_tokens,
            );
            CodeModeExecution {
                output: with_status("Script failed", wall_time, content),
                success: false,
                nested_calls: failure.nested_calls,
                notifications: failure.notifications,
            }
        }
    }
}

impl CodeModeState {
    fn allocate_cell_id(&mut self) -> u64 {
        let cell_id = self.next_cell_id;
        self.next_cell_id += 1;
        cell_id
    }
}

impl CodeModeNotification {
    fn new(call_id: &str, text: String) -> Self {
        Self {
            call_id: call_id.to_owned(),
            text,
        }
    }
}

impl HostFailure {
    fn new(message: String) -> Self {
        Self {
            message,
            nested_calls: Vec::new(),
            notifications: Vec::new(),
        }
    }

    fn with_progress(
        mut self,
        calls: Vec<(u64, NestedToolCall)>,
        notifications: Vec<CodeModeNotification>,
    ) -> Self {
        self.nested_calls = ordered_calls(calls);
        self.notifications = notifications;
        self
    }
}

fn ordered_calls(mut calls: Vec<(u64, NestedToolCall)>) -> Vec<NestedToolCall> {
    calls.sort_unstable_by_key(|(id, _)| *id);
    calls.into_iter().map(|(_, call)| call).collect()
}

async fn execute_nested_call(
    tools: &ToolRuntime,
    id: u64,
    name: String,
    input: Value,
    context: ToolContext<'_>,
) -> CompletedNestedCall {
    let started_at = Instant::now();
    let call_id = format!("code-{id}");
    let execution = tools
        .execute_nested(
            &name,
            input.clone(),
            ToolContext {
                call_id: &call_id,
                ..context
            },
        )
        .await;
    let duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let value = execution.value();
    CompletedNestedCall {
        id,
        value,
        call: NestedToolCall {
            call_id,
            name,
            input,
            output: execution.output,
            success: execution.success,
            duration_ns,
            metadata: execution.metadata,
        },
    }
}

async fn terminate_host(state: &mut CodeModeState) {
    if let Some(mut host) = state.host.take() {
        host.terminate().await;
    }
    state.live_cell = None;
}

fn failed_execution(
    started_at: Instant,
    message: &str,
    nested_calls: Vec<NestedToolCall>,
) -> CodeModeExecution {
    let wall_time = started_at.elapsed().as_secs_f64();
    CodeModeExecution {
        output: ToolOutputBody::Text(format!(
            "Script failed\nWall time {wall_time:.1} seconds\nOutput:\n{message}"
        )),
        success: false,
        nested_calls,
        notifications: Vec::new(),
    }
}

pub(super) fn exec_spec(handlers: &[Box<dyn ToolHandler>]) -> ToolDefinition {
    ToolDefinition::custom(
        "exec",
        description::exec_description(handlers),
        CustomToolFormat::grammar("lark", GRAMMAR),
    )
}

pub(super) fn wait_spec() -> ToolDefinition {
    ToolDefinition::function(
        "wait",
        "Waits on a yielded `exec` cell and returns new output or completion.\n- Use `wait` only after `exec` returns `Script running with cell ID ...`.\n- `cell_id` identifies the running `exec` cell to resume.\n- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.\n- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.\n- `terminate: true` stops the running cell; false or omitted waits for output.\n- `wait` returns only the new output since the last yield, or the final completion or termination result for that cell.\n- If the cell is still running, `wait` may yield again with the same `cell_id`.\n- If the cell has already finished, `wait` returns the completed result and closes the cell.",
        json!({
            "type": "object",
            "properties": {
                "cell_id": {
                    "type": "string",
                    "description": "Identifier of the running exec cell."
                },
                "yield_time_ms": {
                    "type": "number",
                    "description": "Wait before yielding more output. Defaults to 10000 ms."
                },
                "max_tokens": {
                    "type": "number",
                    "description": "Output token budget for this wait call. Defaults to 10000 tokens."
                },
                "terminate": {
                    "type": "boolean",
                    "description": "True stops the running exec cell; false or omitted waits for output."
                }
            },
            "required": ["cell_id"],
            "additionalProperties": false
        }),
    )
}

fn with_status(
    status: &str,
    wall_time: f64,
    mut content: Vec<ToolOutputContent>,
) -> ToolOutputBody {
    let header = format!("{status}\nWall time {wall_time:.1} seconds\nOutput:\n");
    if content.is_empty() {
        return ToolOutputBody::Text(header);
    }
    content.insert(0, ToolOutputContent::InputText { text: header });
    ToolOutputBody::Content(content)
}

#[cfg(test)]
mod tests;
