//! Private Hand binding for the same retained Bash engine used by the native CLI.
//! Identity is supplied by the transport, never by the tool arguments.
#[path = "../retained_bash.rs"]
mod engine;

use nanocodex_claude_tools::ToolOutput as ClaudeOutput;
use nanocodex_oai_tools::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub(super) struct NativeBash {
    root: PathBuf,
    sessions: Mutex<BTreeMap<String, Arc<engine::Shell>>>,
}
impl NativeBash {
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    async fn dispatch(&self, request: Request, session: &str) -> Result<ClaudeOutput, String> {
        if session.is_empty() {
            return Err("native Bash requires a transport session identity".into());
        }
        // Validate before creating session state. Poll/stop never create shells.
        enum Operation {
            Bash(Value),
            Output(OutputInput),
            Stop(StopInput),
        }
        let operation = match request.tool.as_str() {
            "Bash" => Operation::Bash(request.input),
            "TaskOutput" => {
                let args: OutputInput =
                    serde_json::from_value(request.input).map_err(|e| e.to_string())?;
                if args.timeout > 600000 {
                    return Err("TaskOutput timeout must be at most 600000 milliseconds".into());
                }
                Operation::Output(args)
            }
            "TaskStop" => {
                Operation::Stop(serde_json::from_value(request.input).map_err(|e| e.to_string())?)
            }
            _ => return Err("unsupported native Bash tool".into()),
        };
        let shell = {
            let mut sessions = self.sessions.lock().await;
            if matches!(operation, Operation::Bash(_)) && !sessions.contains_key(session) {
                let root = self
                    .root
                    .canonicalize()
                    .map_err(|e| format!("published workspace unavailable: {e}"))?;
                if !root.is_dir() {
                    return Err("published workspace is not a directory".into());
                }
                let pin: engine::PinWorkspace = Arc::new(move || (root.clone(), Box::new(())));
                sessions.insert(session.to_owned(), Arc::new(engine::Shell::new(pin, None)));
            }
            sessions
                .get(session)
                .cloned()
                .ok_or("unknown Bash task_id in this session (jobs do not survive Hand restart)")?
        };
        match operation {
            Operation::Bash(input) => shell.execute(input, session.to_owned()).await,
            Operation::Output(args) => shell.output(&args.task_id, args.block, args.timeout).await,
            Operation::Stop(args) => shell.stop(&args.task_id).await,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    tool: String,
    input: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputInput {
    task_id: String,
    #[serde(default = "yes")]
    block: bool,
    #[serde(default = "wait_ms")]
    timeout: u64,
}
const fn yes() -> bool {
    true
}
const fn wait_ms() -> u64 {
    30000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopInput {
    task_id: String,
}

#[async_trait::async_trait]
impl Tool for NativeBash {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "claude_bash",
            "Private native Claude Bash execution and retained job control in this Hand's published workspace. Session identity is transport-owned; jobs and cwd are not durable across restart.",
            json!({
                "type":"object", "properties": {
                    "tool":{"type":"string","enum":["Bash","TaskOutput","TaskStop"]},
                    "input":{"type":"object"}
                }, "required":["tool","input"], "additionalProperties":false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let result = match input.decode_json::<Request>() {
            Ok(request) => self.dispatch(request, context.session_id()).await,
            Err(error) => Err(error.to_string()),
        };
        let output = result.unwrap_or_else(ClaudeOutput::error);
        let receipt = if output.is_error {
            ToolOutput::error("Native Bash operation failed")
        } else {
            ToolOutput::text("Native Bash operation completed")
        };
        Ok(receipt.with_structured_result(serde_json::to_value(output)?))
    }
}
