use std::{collections::HashMap, process::Stdio};

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::Duration,
};

use super::{
    CellOutcome, CodeModeNotification, CompletedNestedCall, HostFailure, NestedToolCall,
    ToolContext, ToolOutputContent, ToolRuntime, execute_nested_call, ordered_calls,
};

const RUNTIME: &str = include_str!("runtime.js");
const MAX_PROTOCOL_LINE_BYTES: u64 = 8 * 1024 * 1024;

pub(super) struct NodeHost {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
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
    Notify {
        cell_id: u64,
        text: String,
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
        content: Vec<ToolOutputContent>,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
}

impl RuntimeEvent {
    fn cell_id(&self) -> u64 {
        match self {
            Self::ToolCall { cell_id, .. }
            | Self::Notify { cell_id, .. }
            | Self::Yielded { cell_id, .. }
            | Self::Done { cell_id, .. }
            | Self::Error { cell_id, .. } => *cell_id,
        }
    }
}

impl NodeHost {
    pub(super) fn spawn() -> Result<Self, String> {
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

    pub(super) async fn start_cell(
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

    pub(super) async fn drive_cell(
        &mut self,
        cell_id: u64,
        parent_call_id: &str,
        tools: &ToolRuntime,
        context: ToolContext<'_>,
        yield_after: Duration,
    ) -> Result<CellOutcome, HostFailure> {
        let mut completed_calls = Vec::new();
        let mut notifications = Vec::new();
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
                    let call = match self.send_completed_call(cell_id, completed).await {
                        Ok(call) => call,
                        Err(failure) => {
                            return Err(failure.with_progress(completed_calls, notifications));
                        }
                    };
                    completed_calls.push((id, call));
                }
                () = &mut yield_timer, if pending_calls.is_empty() => {
                    return Ok(CellOutcome::Yielded {
                        content: Vec::new(),
                        stored: None,
                        nested_calls: ordered_calls(completed_calls),
                        notifications,
                    });
                }
                event = self.read_event() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(failure) => {
                            return Err(failure.with_progress(completed_calls, notifications));
                        }
                    };
                    let event_cell_id = event.cell_id();
                    if event_cell_id != cell_id {
                        return Err(HostFailure::new(format!(
                            "local code-mode host returned cell {event_cell_id} while executing cell {cell_id}"
                        )).with_progress(completed_calls, notifications));
                    }
                    match event {
                        RuntimeEvent::ToolCall {
                            id, name, input, ..
                        } => {
                            pending_calls
                                .push(execute_nested_call(tools, id, name, input, context).boxed());
                        }
                        RuntimeEvent::Notify { text, .. } => notifications
                            .push(CodeModeNotification::new(parent_call_id, text)),
                        RuntimeEvent::Yielded {
                            content,
                            stored,
                            ..
                        } => {
                            if !pending_calls.is_empty() {
                                return Err(HostFailure::new(
                                    "exec cell yielded while nested tool calls were pending".to_owned(),
                                ).with_progress(completed_calls, notifications));
                            }
                            return Ok(CellOutcome::Yielded {
                                content,
                                stored: Some(stored),
                                nested_calls: ordered_calls(completed_calls),
                                notifications,
                            });
                        }
                        RuntimeEvent::Done {
                            content,
                            stored,
                            ..
                        } => {
                            return Ok(CellOutcome::Completed {
                                content,
                                stored,
                                nested_calls: ordered_calls(completed_calls),
                                notifications,
                            });
                        }
                        RuntimeEvent::Error {
                            message,
                            content,
                            stored,
                            ..
                        } => {
                            return Ok(CellOutcome::ScriptFailed {
                                message,
                                content,
                                stored,
                                nested_calls: ordered_calls(completed_calls),
                                notifications,
                            });
                        }
                    }
                }
            }
        }
    }

    async fn read_event(&mut self) -> Result<RuntimeEvent, HostFailure> {
        let line = match read_protocol_line(&mut self.stdout).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                let status = self.child.wait().await;
                return Err(HostFailure::new(format!(
                    "local code-mode host ended before a result: {status:?}"
                )));
            }
            Err(error) => {
                return Err(HostFailure::new(format!(
                    "failed to read local code-mode host: {error}"
                )));
            }
        };
        serde_json::from_slice::<RuntimeEvent>(&line).map_err(|error| {
            HostFailure::new(format!(
                "local code-mode host emitted invalid JSON: {error}"
            ))
        })
    }

    async fn send_completed_call(
        &mut self,
        cell_id: u64,
        completed: CompletedNestedCall,
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
                HostFailure::new(format!("failed to return a nested tool result: {error}"))
            })?;
        Ok(completed.call)
    }

    pub(super) async fn terminate(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

async fn write_json_line(stdin: &mut ChildStdin, value: &impl Serialize) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    stdin.write_all(&encoded).await?;
    stdin.flush().await
}

async fn read_protocol_line(
    stdout: &mut BufReader<ChildStdout>,
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
