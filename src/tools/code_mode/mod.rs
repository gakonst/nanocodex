mod description;
mod output;

use std::{collections::HashMap, process::Stdio, time::Instant};

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::Duration,
};

use super::{ToolContext, ToolHandler, ToolOutputBody, ToolOutputContent, ToolRuntime};

const RUNTIME: &str = include_str!("runtime.js");
const MAX_PROTOCOL_LINE_BYTES: u64 = 8 * 1024 * 1024;
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
    live_cell: Option<u64>,
}

struct NodeHost {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub(crate) struct CodeModeExecution {
    pub(crate) output: ToolOutputBody,
    pub(crate) success: bool,
    pub(crate) nested_calls: Vec<NestedToolCall>,
}

#[derive(Clone)]
pub(crate) struct NestedToolCall {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) input: Value,
    pub(crate) output: ToolOutputBody,
    pub(crate) success: bool,
    pub(crate) duration_ns: u64,
    pub(crate) metadata: Option<Value>,
}

#[derive(Serialize)]
struct ExecuteMessage<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    cell_id: u64,
    source: &'a str,
    tools: Vec<Value>,
    stored: HashMap<String, Value>,
}

#[derive(Serialize)]
struct ToolResultMessage {
    #[serde(rename = "type")]
    kind: &'static str,
    cell_id: u64,
    id: u64,
    value: Value,
    success: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RuntimeEvent {
    ToolCall {
        cell_id: u64,
        id: u64,
        name: String,
        input: Value,
    },
    Yielded {
        cell_id: u64,
        #[serde(default)]
        content: Vec<ToolOutputContent>,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
    Done {
        cell_id: u64,
        #[serde(default)]
        content: Vec<ToolOutputContent>,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
    Error {
        cell_id: u64,
        message: String,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
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
    },
    Completed {
        content: Vec<ToolOutputContent>,
        stored: HashMap<String, Value>,
        nested_calls: Vec<NestedToolCall>,
    },
    ScriptFailed {
        message: String,
        stored: HashMap<String, Value>,
        nested_calls: Vec<NestedToolCall>,
    },
}

struct HostFailure {
    message: String,
    nested_calls: Vec<NestedToolCall>,
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
        if let Some(cell_id) = state.live_cell {
            return failed_execution(
                started_at,
                &format!("exec cell {cell_id} is still running; use wait before starting another"),
                Vec::new(),
            );
        }
        let cell_id = state.allocate_cell_id();
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
                    host.drive_cell(cell_id, tools, context, yield_after).await
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
        if state.live_cell != Some(cell_id) {
            return failed_execution(
                started_at,
                &format!("exec cell {cell_id} was not found"),
                Vec::new(),
            );
        }
        if arguments.terminate {
            terminate_host(&mut state).await;
            return CodeModeExecution {
                output: ToolOutputBody::Text(format!(
                    "Script terminated\nWall time {:.1} seconds\nOutput:\nexec cell {cell_id} was terminated",
                    started_at.elapsed().as_secs_f64()
                )),
                success: true,
                nested_calls: Vec::new(),
            };
        }
        let yield_time = Duration::from_millis(
            arguments
                .yield_time_ms
                .unwrap_or(u64::try_from(DEFAULT_WAIT_YIELD.as_millis()).unwrap_or(u64::MAX)),
        );
        let result = if let Some(host) = state.host.as_mut() {
            host.drive_cell(cell_id, tools, context, yield_time).await
        } else {
            Err(HostFailure::new(
                "local Node.js code-mode host was unavailable".to_owned(),
            ))
        };
        finish_cell(
            &mut state,
            cell_id,
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
        }) => {
            if let Some(stored) = stored {
                state.stored = stored;
            }
            state.live_cell = Some(cell_id);
            let content = output::truncate_content(content, max_output_tokens);
            CodeModeExecution {
                output: with_status(
                    &format!("Script running with cell ID {cell_id}"),
                    wall_time,
                    content,
                ),
                success: true,
                nested_calls,
            }
        }
        Ok(CellOutcome::Completed {
            content,
            stored,
            nested_calls,
        }) => {
            state.live_cell = None;
            state.stored = stored;
            let content = output::truncate_content(content, max_output_tokens);
            CodeModeExecution {
                output: with_status("Script completed", wall_time, content),
                success: true,
                nested_calls,
            }
        }
        Ok(CellOutcome::ScriptFailed {
            message,
            stored,
            nested_calls,
        }) => {
            state.live_cell = None;
            state.stored = stored;
            let content = output::truncate_content(
                vec![ToolOutputContent::InputText { text: message }],
                max_output_tokens,
            );
            CodeModeExecution {
                output: with_status("Script failed", wall_time, content),
                success: false,
                nested_calls,
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

impl NodeHost {
    fn spawn() -> Result<Self, String> {
        let mut child = Command::new("node")
            .args(["--input-type=module", "--eval", RUNTIME])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("failed to start local Node.js code-mode host: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Node code-mode host stdin was unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Node code-mode host stdout was unavailable".to_owned())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    async fn start_cell(
        &mut self,
        cell_id: u64,
        source: &str,
        stored: HashMap<String, Value>,
        tools: &ToolRuntime,
    ) -> Result<(), HostFailure> {
        let request = ExecuteMessage {
            kind: "execute",
            cell_id,
            source,
            tools: tools.nested_tool_metadata(),
            stored,
        };
        write_json_line(&mut self.stdin, &request)
            .await
            .map_err(|error| {
                HostFailure::new(format!(
                    "failed to initialize local code-mode cell: {error}"
                ))
            })?;
        Ok(())
    }

    async fn drive_cell(
        &mut self,
        cell_id: u64,
        tools: &ToolRuntime,
        context: ToolContext<'_>,
        yield_after: Duration,
    ) -> Result<CellOutcome, HostFailure> {
        let mut completed_calls = Vec::new();
        let mut pending_calls: FuturesUnordered<BoxFuture<'_, CompletedNestedCall>> =
            FuturesUnordered::new();
        loop {
            let yield_timer = tokio::time::sleep(yield_after);
            tokio::pin!(yield_timer);
            tokio::select! {
                completed = pending_calls.next(), if !pending_calls.is_empty() => {
                    let Some(completed) = completed else {
                        continue;
                    };
                    let id = completed.id;
                    let call = self
                        .send_completed_call(cell_id, completed, &completed_calls)
                        .await?;
                    completed_calls.push((id, call));
                }
                () = &mut yield_timer, if pending_calls.is_empty() => {
                    return Ok(CellOutcome::Yielded {
                        content: Vec::new(),
                        stored: None,
                        nested_calls: ordered_calls(completed_calls),
                    });
                }
                event = self.read_event(&completed_calls) => {
                    let event = event?;
                    match event {
                        RuntimeEvent::ToolCall {
                            cell_id: event_cell_id,
                            id,
                            name,
                            input,
                        } => {
                            validate_cell_id(cell_id, event_cell_id, &completed_calls)?;
                            pending_calls.push(
                                execute_nested_call(tools, id, name, input, context).boxed(),
                            );
                        }
                        RuntimeEvent::Yielded {
                            cell_id: event_cell_id,
                            content,
                            stored,
                        } => {
                            validate_cell_id(cell_id, event_cell_id, &completed_calls)?;
                            if !pending_calls.is_empty() {
                                return Err(HostFailure::with_calls(
                                    "exec cell yielded while nested tool calls were pending".to_owned(),
                                    &completed_calls,
                                ));
                            }
                            return Ok(CellOutcome::Yielded {
                                content,
                                stored: Some(stored),
                                nested_calls: ordered_calls(completed_calls),
                            });
                        }
                        RuntimeEvent::Done {
                            cell_id: event_cell_id,
                            content,
                            stored,
                        } => {
                            validate_cell_id(cell_id, event_cell_id, &completed_calls)?;
                            return Ok(CellOutcome::Completed {
                                content,
                                stored,
                                nested_calls: ordered_calls(completed_calls),
                            });
                        }
                        RuntimeEvent::Error {
                            cell_id: event_cell_id,
                            message,
                            stored,
                        } => {
                            validate_cell_id(cell_id, event_cell_id, &completed_calls)?;
                            return Ok(CellOutcome::ScriptFailed {
                                message,
                                stored,
                                nested_calls: ordered_calls(completed_calls),
                            });
                        }
                    }
                }
            }
        }
    }

    async fn read_event(
        &mut self,
        completed_calls: &[(u64, NestedToolCall)],
    ) -> Result<RuntimeEvent, HostFailure> {
        let line = match read_protocol_line(&mut self.stdout).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                let status = self.child.wait().await;
                return Err(HostFailure::with_calls(
                    format!("local code-mode host ended before a result: {status:?}"),
                    completed_calls,
                ));
            }
            Err(error) => {
                return Err(HostFailure::with_calls(
                    format!("failed to read local code-mode host: {error}"),
                    completed_calls,
                ));
            }
        };
        serde_json::from_slice::<RuntimeEvent>(&line).map_err(|error| {
            HostFailure::with_calls(
                format!("local code-mode host emitted invalid JSON: {error}"),
                completed_calls,
            )
        })
    }

    async fn send_completed_call(
        &mut self,
        cell_id: u64,
        completed: CompletedNestedCall,
        prior_calls: &[(u64, NestedToolCall)],
    ) -> Result<NestedToolCall, HostFailure> {
        let response = ToolResultMessage {
            kind: "tool_result",
            cell_id,
            id: completed.id,
            value: completed.value,
            success: completed.call.success,
        };
        write_json_line(&mut self.stdin, &response)
            .await
            .map_err(|error| {
                HostFailure::with_calls(
                    format!("failed to return a nested tool result: {error}"),
                    prior_calls,
                )
            })?;
        Ok(completed.call)
    }

    async fn terminate(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

impl HostFailure {
    fn new(message: String) -> Self {
        Self {
            message,
            nested_calls: Vec::new(),
        }
    }

    fn with_calls(message: String, calls: &[(u64, NestedToolCall)]) -> Self {
        Self {
            message,
            nested_calls: ordered_calls(calls.to_vec()),
        }
    }
}

fn validate_cell_id(
    expected: u64,
    actual: u64,
    calls: &[(u64, NestedToolCall)],
) -> Result<(), HostFailure> {
    if expected == actual {
        return Ok(());
    }
    Err(HostFailure {
        message: format!(
            "local code-mode host returned cell {actual} while executing cell {expected}"
        ),
        nested_calls: ordered_calls(calls.to_vec()),
    })
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
    let execution = tools.execute_nested(&name, input.clone(), context).await;
    let duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let value = execution.value();
    CompletedNestedCall {
        id,
        value,
        call: NestedToolCall {
            call_id: format!("code-{id}"),
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
    }
}

pub(super) fn exec_spec(handlers: &[Box<dyn ToolHandler>]) -> Value {
    json!({
        "type": "custom",
        "name": "exec",
        "description": description::exec_description(handlers),
        "format": {
            "type": "grammar",
            "syntax": "lark",
            "definition": GRAMMAR,
        },
    })
}

pub(super) fn wait_spec() -> Value {
    json!({
        "type": "function",
        "name": "wait",
        "description": "Waits on a yielded `exec` cell and returns new output or completion.\n- Use `wait` only after `exec` returns `Script running with cell ID ...`.\n- `cell_id` identifies the running `exec` cell to resume.\n- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.\n- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.\n- `terminate: true` stops the running cell; false or omitted waits for output.\n- `wait` returns only the new output since the last yield, or the final completion or termination result for that cell.\n- If the cell is still running, `wait` may yield again with the same `cell_id`.\n- If the cell has already finished, `wait` returns the completed result and closes the cell.",
        "strict": false,
        "parameters": {
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
        }
    })
}

async fn write_json_line(
    stdin: &mut tokio::process::ChildStdin,
    value: &impl Serialize,
) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    stdin.write_all(&encoded).await?;
    stdin.flush().await
}

async fn read_protocol_line(
    stdout: &mut BufReader<tokio::process::ChildStdout>,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = stdout
        .take(MAX_PROTOCOL_LINE_BYTES + 1)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if u64::try_from(read).unwrap_or(u64::MAX) > MAX_PROTOCOL_LINE_BYTES || !line.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "local code-mode protocol line exceeded 8 MiB",
        ));
    }
    line.pop();
    Ok(Some(line))
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
