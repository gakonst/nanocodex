use std::io;

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::Result;

use crate::config::AgentArgs;

#[derive(Args)]
pub(crate) struct Run {
    /// Prompt submitted to the agent.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,

    /// Submit the same prompt as sequential follow-on turns on one owned session.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    repeat: u16,
}

impl Run {
    pub(crate) async fn run(self, config: AgentArgs) -> Result<()> {
        let configured = config.build()?;
        let handle = configured.handle;
        let mut events = configured.events;
        let result = async {
            for _ in 0..self.repeat {
                let turn = handle.prompt(self.prompt.clone()).await?;
                events.write_turn_jsonl(io::stdout()).await?;
                turn.result().await?;
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
