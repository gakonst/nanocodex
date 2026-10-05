//! Upstream task callbacks. The host owns child execution, task IDs, ownership,
//! resumability and checkpoints. No synthetic success receipts or default agent
//! spawner are installed. Checkpoints are opaque host state, never model text.
use crate::host::*;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskCapability {
    Task,
    GetTaskOutput,
    WaitTasks,
    KillTask,
    SendSubagentMessage,
}
pub trait XaiTaskProvider: Send + Sync + 'static {
    fn capabilities(&self) -> Vec<TaskCapability>;
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>>;
    fn checkpoint(&self, session_id: String) -> HostFuture<Result<Value, String>>;
    /// Must reconcile running/uncertain effects before permitting retries.
    fn restore(&self, session_id: String, checkpoint: Value) -> HostFuture<Result<(), String>>;
}
pub struct XaiTasks<P: XaiTaskProvider + ?Sized> {
    provider: Arc<P>,
}
impl<P: XaiTaskProvider + ?Sized> XaiTasks<P> {
    pub const fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }
    pub fn checkpoint(&self, session_id: String) -> HostFuture<Result<Value, String>> {
        self.provider.checkpoint(session_id)
    }
    pub fn restore(&self, session_id: String, checkpoint: Value) -> HostFuture<Result<(), String>> {
        self.provider.restore(session_id, checkpoint)
    }
}
impl TaskCapability {
    pub fn definition(self) -> ToolDefinition {
        match self {
            Self::Task => definition(
                "task",
                "Delegate work to a host-owned subagent. Resume only completed agents owned by this session.",
                json!({"prompt":{"type":"string"},"description":{"type":"string"},"run_in_background":{"type":"boolean","default":true},"isolation":{"type":"string","enum":["none","worktree"]},"resume_from":{"type":"string"},"cwd":{"type":"string"},"model":{"type":"string"}}),
                &["prompt", "description"],
            ),
            Self::GetTaskOutput => definition(
                "get_task_output",
                "Read host-owned task states; positive timeout_ms waits for all selected tasks.",
                json!({"task_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":64},"timeout_ms":{"type":"integer","minimum":0,"maximum":3600000}}),
                &["task_ids"],
            ),
            Self::WaitTasks => definition(
                "wait_tasks",
                "Wait for selected host-owned tasks; preserve running tasks after a wait expires.",
                json!({"task_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":64},"mode":{"type":"string","enum":["wait_any","wait_all"]},"timeout_ms":{"type":"integer","minimum":0,"maximum":3600000}}),
                &["task_ids", "mode"],
            ),
            Self::KillTask => definition(
                "kill_task",
                "Cancel a task owned by the current session.",
                json!({"task_id":{"type":"string"}}),
                &["task_id"],
            ),
            Self::SendSubagentMessage => definition(
                "send_subagent_message",
                "Send text to a host-owned subagent by its durable ID.",
                json!({"subagent_id":{"type":"string"},"text":{"type":"string"},"delivery":{"type":"string","enum":["steer","queue","interject"]}}),
                &["subagent_id", "text"],
            ),
        }
    }
}
impl<P: XaiTaskProvider + ?Sized> XaiHost for XaiTasks<P> {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.provider
            .capabilities()
            .into_iter()
            .map(TaskCapability::definition)
            .collect()
    }
    fn call(&self, mut request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        let provider = self.provider.clone();
        Box::pin(async move {
            validate_request(&request)?;
            if !provider
                .capabilities()
                .iter()
                .any(|c| c.definition().name == request.tool)
            {
                return Err("task capability is not installed".into());
            }
            match request.tool.as_str() {
                "task" => {
                    fields(
                        &request.input,
                        &[
                            "prompt",
                            "description",
                            "run_in_background",
                            "isolation",
                            "resume_from",
                            "cwd",
                            "model",
                        ],
                    )?;
                    for key in ["prompt", "description"] {
                        if string(&request.input, key)?.trim().is_empty() {
                            return Err(format!("{key} cannot be empty"));
                        }
                    }
                    let background = boolean(&request.input, "run_in_background", true)?;
                    request.input["run_in_background"] = json!(background);
                    if let Some(isolation) = request.input.get("isolation")
                        && isolation != "none"
                        && isolation != "worktree"
                    {
                        return Err("invalid isolation".into());
                    }
                    for key in ["resume_from", "cwd", "model"] {
                        if request.input.get(key).is_some() {
                            string(&request.input, key)?;
                        }
                    }
                    if request.input["isolation"] == "worktree"
                        && request.input.get("cwd").is_some()
                    {
                        return Err("cwd and worktree isolation are mutually exclusive".into());
                    }
                    if request.input.get("resume_from").is_some()
                        && request.input.get("model").is_some()
                    {
                        return Err("resumed tasks inherit their model".into());
                    }
                }
                "get_task_output" | "wait_tasks" => {
                    fields(
                        &request.input,
                        &["task_ids", "task_id", "timeout_ms", "mode"],
                    )?;
                    if request.input.get("task_ids").is_none()
                        && let Some(id) = request.input.get("task_id").cloned()
                    {
                        request.input["task_ids"] = json!([id]);
                    }
                    let ids = request.input["task_ids"]
                        .as_array()
                        .ok_or("task_ids must be an array")?;
                    if ids.is_empty()
                        || ids.len() > 64
                        || ids
                            .iter()
                            .any(|id| id.as_str().is_none_or(|s| s.trim().is_empty()))
                    {
                        return Err("invalid task_ids".into());
                    }
                    number(&request.input, "timeout_ms", 0, 3600000)?;
                    if request.tool == "wait_tasks"
                        && !matches!(
                            request.input["mode"].as_str(),
                            Some("wait_any" | "wait_all")
                        )
                    {
                        return Err("invalid wait mode".into());
                    }
                }
                "kill_task" => {
                    fields(&request.input, &["task_id"])?;
                    if string(&request.input, "task_id")?.is_empty() {
                        return Err("empty task_id".into());
                    }
                }
                "send_subagent_message" => {
                    fields(&request.input, &["subagent_id", "text", "delivery"])?;
                    string(&request.input, "subagent_id")?;
                    string(&request.input, "text")?;
                    if request.input.get("delivery").is_some()
                        && !matches!(
                            request.input["delivery"].as_str(),
                            Some("steer" | "queue" | "interject")
                        )
                    {
                        return Err("invalid delivery".into());
                    }
                }
                _ => return Err("task capability not installed".into()),
            }
            provider.call(request).await
        })
    }
}
