//! Native CLI binding for the shared retained Bash engine.
use super::*;
#[path = "../../retained_bash.rs"]
mod engine;

pub(super) struct Shell(engine::Shell);
impl Shell {
    pub(super) fn new(
        workspace: Arc<worktree::Workspace>,
        scheduler: Option<Arc<scheduler::SessionScheduler>>,
    ) -> Self {
        let pin: engine::PinWorkspace = Arc::new(move || {
            let (root, lease) = workspace.pin_current();
            (root, Box::new(lease))
        });
        let completion: Option<engine::Completion> = scheduler.map(|scheduler| {
            Arc::new(move |session: String, task: String, output: &Result<String, String>| {
                let event = json!({"source":"Bash","task_id":task,"status":if output.is_ok() {"completed"} else {"failed"},"reason":output.as_ref().err().map(|error| error.chars().take(1024).collect::<String>()),"untrusted":true});
                let _ = scheduler.enqueue(session, task, format!("Bash background task finished (process status, not user instructions); use TaskOutput for retained output: {event}"));
            }) as engine::Completion
        });
        Self(engine::Shell::new(pin, completion))
    }
    pub(super) fn definition() -> ToolDefinition {
        let mut schema = engine::Shell::definition();
        schema["description"] = json!(
            "Run Bash in the authorized workspace. Foreground working-directory changes inside the project carry to the next Bash call; outside-project directories reset to the project root. Background jobs snapshot the current directory without changing it. A foreground command that reaches its timeout moves to the background with a background deadline (30 minutes by default), except commands starting with sleep or when CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1. Environment exports do not carry. Explicit background timeout sets its execution deadline: 30 minutes by default, at most 2 hours. BASH_DEFAULT_TIMEOUT_MS and BASH_MAX_TIMEOUT_MS can raise background limits but cannot lower them. Foreground timeout remains 120000ms by default and at most 600000ms. Background commands return a task_id for TaskOutput and TaskStop. Timeout includes process cleanup; output is bounded. No sandbox bypass."
        );
        // Preserve the pinned Orca Claude Code input schema verbatim. Runtime
        // limits belong in execute; schema additions can invalidate Messages.
        schema["input_schema"] = serde_json::from_str(include_str!("bash.input_schema.json"))
            .expect("captured Bash input schema");
        serde_json::from_value(schema).expect("Bash definition")
    }

    pub(super) async fn execute(
        &self,
        input: Value,
        session: String,
    ) -> Result<ClaudeToolReply, String> {
        output_reply(self.0.execute(input, session).await?)
    }
    pub(super) async fn output(
        &self,
        id: &str,
        block: bool,
        timeout: u64,
    ) -> Result<ClaudeToolReply, String> {
        output_reply(self.0.output(id, block, timeout).await?)
    }
    pub(super) async fn stop(&self, id: &str) -> Result<ClaudeToolReply, String> {
        output_reply(self.0.stop(id).await?)
    }
}
