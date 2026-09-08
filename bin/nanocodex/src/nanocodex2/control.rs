//! Headless controls for the same managed settings and schedules used by the apps.

use clap::{Args, Subcommand, ValueEnum};
use nanocodex_managed::{
    AgentSettings, CronSessionMode, CronTriggerConfig, ManagedClient, ManagedError, Model,
    ReasoningMode, Thinking,
};

#[derive(Args, Default)]
pub(crate) struct InitialSettings {
    /// Initial model (astra, sol, terra, or luna, or its full model ID).
    #[arg(long)]
    model: Option<Model>,
    /// Initial reasoning effort.
    #[arg(long)]
    thinking: Option<Thinking>,
    /// Initial reasoning mode (standard or pro).
    #[arg(long)]
    reasoning_mode: Option<ReasoningMode>,
    /// Enable fast processing for the new agent.
    #[arg(long)]
    fast_mode: bool,
}

impl InitialSettings {
    pub(crate) fn resolve(self) -> AgentSettings {
        let defaults = AgentSettings::default();
        AgentSettings {
            model: self.model.unwrap_or(defaults.model),
            thinking: self.thinking.unwrap_or(defaults.thinking),
            reasoning_mode: self.reasoning_mode.unwrap_or(defaults.reasoning_mode),
            fast_mode: self.fast_mode,
        }
    }
}

#[derive(Args)]
pub(crate) struct Settings {
    /// Account-owned managed agent ID.
    agent_id: String,
    #[command(subcommand)]
    change: Option<SettingsChange>,
}

#[derive(Subcommand)]
enum SettingsChange {
    /// Select the model for subsequently admitted turns.
    Model { model: Model },
    /// Set reasoning effort for subsequently admitted turns.
    Thinking { thinking: Thinking },
    /// Set standard or pro reasoning mode.
    ReasoningMode { mode: ReasoningMode },
    /// Enable or disable fast processing.
    FastMode {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
}

impl Settings {
    pub(crate) async fn run(self, client: &ManagedClient) -> Result<(), ManagedError> {
        let id = &self.agent_id;
        let settings = match self.change {
            None => client.state(id).await?.settings,
            Some(SettingsChange::Model { model }) => client.set_model(id, model).await?,
            Some(SettingsChange::Thinking { thinking }) => {
                client.set_thinking(id, thinking).await?
            }
            Some(SettingsChange::ReasoningMode { mode }) => {
                client.set_reasoning_mode(id, mode).await?
            }
            Some(SettingsChange::FastMode { enabled }) => client.set_fast_mode(id, enabled).await?,
        };
        super::write_json(&settings)
    }
}

#[derive(Args)]
pub(crate) struct Cron {
    #[command(subcommand)]
    command: CronCommand,
}

#[derive(Subcommand)]
enum CronCommand {
    /// List an agent's durable schedules as JSON.
    List { agent_id: String },
    /// Read one named schedule as JSON.
    Get {
        agent_id: String,
        trigger_id: String,
    },
    /// Create or replace one named schedule.
    Put {
        agent_id: String,
        trigger_id: String,
        /// Five-field cron expression, quoted as one argument.
        #[arg(long)]
        cron: String,
        /// IANA timezone used to interpret the schedule.
        #[arg(long, default_value = "UTC")]
        timezone: String,
        /// Prompt submitted on each scheduled occurrence.
        #[arg(long)]
        prompt: String,
        /// Start a fresh agent or continue this agent on each occurrence.
        #[arg(long, value_enum, default_value = "new")]
        session_mode: SessionMode,
        /// Retain the schedule without delivering future occurrences.
        #[arg(long)]
        disabled: bool,
    },
    /// Delete one named schedule.
    Delete {
        agent_id: String,
        trigger_id: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SessionMode {
    New,
    Continue,
}

impl Cron {
    pub(crate) async fn run(self, client: &ManagedClient) -> Result<(), ManagedError> {
        match self.command {
            CronCommand::List { agent_id } => super::write_json(&client.triggers(&agent_id).await?),
            CronCommand::Get {
                agent_id,
                trigger_id,
            } => super::write_json(&client.trigger(&agent_id, &trigger_id).await?),
            CronCommand::Delete {
                agent_id,
                trigger_id,
            } => client.delete_trigger(&agent_id, &trigger_id).await,
            CronCommand::Put {
                agent_id,
                trigger_id,
                cron,
                timezone,
                prompt,
                session_mode,
                disabled,
            } => {
                let config = CronTriggerConfig {
                    cron,
                    timezone,
                    input: prompt,
                    enabled: !disabled,
                    session_mode: match session_mode {
                        SessionMode::New => CronSessionMode::New,
                        SessionMode::Continue => CronSessionMode::Continue,
                    },
                };
                super::write_json(&client.put_trigger(&agent_id, &trigger_id, &config).await?)
            }
        }
    }
}
