use std::io;

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::Result;

use crate::config::AgentArgs;

const COMPLETION_AUDIT: &str = r"Perform the required final audit now. Do not merely summarize previous checks.

Re-read the original task below verbatim and compare every literal path, field name, version, port, boundary, representation convention, lifecycle requirement, and caller-facing command against the actual final state. Validation derived from your own implementation is not independent: use the original contract, installed library behavior, or a separately written caller. Run the exact external workflow or the closest possible equivalent, fix every discrepancy you find, and leave the verified passing state intact.

<original_task>
{original_task}
</original_task>";

#[derive(Args)]
pub(crate) struct Run {
    /// Prompt submitted to the agent.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,

    /// Submit the same prompt as sequential follow-on turns on one owned session.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    repeat: u16,

    /// Audit and repair the completed task in one same-session follow-on turn.
    #[arg(long)]
    completion_audit: bool,
}

impl Run {
    pub(crate) async fn run(self, config: AgentArgs) -> Result<()> {
        let configured = config.build()?;
        let handle = configured.handle;
        let mut events = configured.events;
        let result = async {
            for _ in 0..self.repeat {
                run_turn(&handle, &mut events, self.prompt.clone()).await?;
                if self.completion_audit {
                    run_turn(&handle, &mut events, completion_audit_prompt(&self.prompt)).await?;
                }
            }
            Ok(())
        }
        .await;
        drop(handle);
        if let Some(child_agents) = configured.child_agents {
            child_agents.shutdown().await;
        }
        result
    }
}

async fn run_turn(
    handle: &nanocodex::Nanocodex,
    events: &mut nanocodex::AgentEvents,
    prompt: String,
) -> Result<()> {
    let turn = handle.prompt(prompt).await?;
    events.write_turn_jsonl(io::stdout()).await?;
    turn.result().await?;
    Ok(())
}

fn completion_audit_prompt(original_task: &str) -> String {
    COMPLETION_AUDIT.replace("{original_task}", original_task)
}

#[cfg(test)]
mod tests {
    use super::completion_audit_prompt;

    #[test]
    fn audit_replays_the_original_task_verbatim_without_task_specific_hints() {
        let original = "write value at /app/result.txt\nthen keep port 8080 alive";
        let audit = completion_audit_prompt(original);

        assert!(audit.contains(original));
        assert!(audit.contains("Do not merely summarize previous checks"));
        assert!(audit.contains("leave the verified passing state intact"));
        assert!(!audit.contains("T2Retrieval"));
        assert!(!audit.contains("sshd"));
    }
}
