use std::{collections::HashMap, fmt::Write as _, process::Stdio, time::Instant};

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::{Duration, timeout},
};

use super::{ToolHandler, ToolOutputBody, ToolOutputContent, ToolRuntime};

const RUNTIME: &str = include_str!("runtime.js");
const MAX_PROTOCOL_LINE_BYTES: u64 = 8 * 1024 * 1024;
const EXECUTION_TIMEOUT: Duration = Duration::from_secs(120);
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
            }),
        }
    }

    pub(super) async fn execute(&self, source: &str, tools: &ToolRuntime) -> CodeModeExecution {
        let started_at = Instant::now();
        let mut state = self.state.lock().await;
        let cell_id = match state.allocate_cell_id() {
            Ok(cell_id) => cell_id,
            Err(message) => return failed_execution(started_at, &message, Vec::new()),
        };
        if state.host.is_none() {
            match NodeHost::spawn() {
                Ok(host) => state.host = Some(host),
                Err(message) => return failed_execution(started_at, &message, Vec::new()),
            }
        }

        let stored = state.stored.clone();
        let result = if let Some(host) = state.host.as_mut() {
            timeout(
                EXECUTION_TIMEOUT,
                host.execute_cell(cell_id, source, stored, tools),
            )
            .await
        } else {
            return failed_execution(
                started_at,
                "local Node.js code-mode host was unavailable",
                Vec::new(),
            );
        };
        let wall_time = started_at.elapsed().as_secs_f64();
        match result {
            Ok(Ok(CellOutcome::Completed {
                content,
                stored,
                nested_calls,
            })) => {
                state.stored = stored;
                CodeModeExecution {
                    output: with_status("Script completed", wall_time, content),
                    success: true,
                    nested_calls,
                }
            }
            Ok(Ok(CellOutcome::ScriptFailed {
                message,
                stored,
                nested_calls,
            })) => {
                state.stored = stored;
                CodeModeExecution {
                    output: ToolOutputBody::Text(format!(
                        "Script failed\nWall time {wall_time:.1} seconds\nOutput:\n{message}"
                    )),
                    success: false,
                    nested_calls,
                }
            }
            Ok(Err(failure)) => {
                terminate_host(&mut state).await;
                CodeModeExecution {
                    output: ToolOutputBody::Text(format!(
                        "Script failed\nWall time {wall_time:.1} seconds\nOutput:\n{}",
                        failure.message
                    )),
                    success: false,
                    nested_calls: failure.nested_calls,
                }
            }
            Err(_) => {
                terminate_host(&mut state).await;
                CodeModeExecution {
                    output: ToolOutputBody::Text(format!(
                        "Script terminated\nWall time {wall_time:.1} seconds\nOutput:\nexecution exceeded {} seconds",
                        EXECUTION_TIMEOUT.as_secs()
                    )),
                    success: false,
                    nested_calls: Vec::new(),
                }
            }
        }
    }
}

impl CodeModeState {
    fn allocate_cell_id(&mut self) -> Result<u64, String> {
        let cell_id = self.next_cell_id;
        self.next_cell_id = cell_id
            .checked_add(1)
            .ok_or_else(|| "local code mode exhausted its cell ID space".to_owned())?;
        Ok(cell_id)
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

    async fn execute_cell(
        &mut self,
        cell_id: u64,
        source: &str,
        stored: HashMap<String, Value>,
        tools: &ToolRuntime,
    ) -> Result<CellOutcome, HostFailure> {
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

        let mut completed_calls = Vec::new();
        let mut pending_calls: FuturesUnordered<BoxFuture<'_, CompletedNestedCall>> =
            FuturesUnordered::new();
        loop {
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
                            pending_calls.push(execute_nested_call(tools, id, name, input).boxed());
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
) -> CompletedNestedCall {
    let started_at = Instant::now();
    let execution = tools.execute_nested(&name, input.clone()).await;
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
        },
    }
}

async fn terminate_host(state: &mut CodeModeState) {
    if let Some(mut host) = state.host.take() {
        host.terminate().await;
    }
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
    let mut description = String::from(
        "Run JavaScript code to orchestrate and compose tool calls.\n\
- Evaluates raw JavaScript as a cell in one local Node.js host reused for the session.\n\
- Nested tools are available on the global `tools` object, for example `await tools.exec_command({cmd: \"pwd\"})`.\n\
- Nested function tools take one object argument and return an object or string.\n\
- Independent nested calls made with `Promise.all` execute concurrently.\n\
- Normal Node.js capabilities are available, including `process`, `require`, dynamic `import()`, the file system, and the network.\n\
- Use `text(value)` or `image(value)` to append output for the model.\n\
- `store(key, value)` and `load(key)` persist serializable values between exec calls.\n\
- `ALL_TOOLS` lists the enabled nested tools.\n\
- Runs raw JavaScript, not JSON, quoted strings, or Markdown code fences.\n\nNested tools:\n",
    );
    for handler in handlers {
        let spec = handler.spec();
        let parameters = spec
            .get("parameters")
            .map_or_else(|| "{}".to_owned(), Value::to_string);
        let _ = write!(
            description,
            "\n- `{}`: {}\n  Input schema: `{parameters}`\n",
            handler.name(),
            spec.get("description")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    }
    json!({
        "type": "custom",
        "name": "exec",
        "description": description,
        "format": {
            "type": "grammar",
            "syntax": "lark",
            "definition": GRAMMAR,
        },
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
