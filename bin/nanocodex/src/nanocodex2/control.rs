//! Headless controls for the same managed settings and schedules used by the apps.

use clap::{Args, Subcommand, ValueEnum};
use nanocodex_managed::{
    AgentSettings, CronSessionMode, CronTriggerConfig, ManagedClient, ManagedError, ManagedModel,
    Model, ReasoningMode, Thinking,
};

fn parse_managed_model(value: &str) -> Result<ManagedModel, &'static str> {
    value.parse::<ManagedModel>().or_else(|_| {
        value
            .parse::<Model>()
            .map(ManagedModel::from)
            .map_err(|_| "Expected a supported managed model ID or native model alias")
    })
}

#[derive(Args, Default)]
pub(crate) struct InitialSettings {
    /// Initial managed model ID (Claude supported; native aliases astra/sol/luna accepted).
    #[arg(long, value_parser = parse_managed_model)]
    model: Option<ManagedModel>,
    /// Initial reasoning effort.
    #[arg(long)]
    thinking: Option<Thinking>,
    /// Initial reasoning mode (standard or pro).
    #[arg(long)]
    reasoning_mode: Option<ReasoningMode>,
    /// Request fast processing (default follows the selected model).
    #[arg(long, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
    fast_mode: Option<bool>,
    /// Pin the new session to this connected ChatGPT account (disables failover).
    #[arg(long)]
    pub(crate) chatgpt_account: Option<String>,
}

impl InitialSettings {
    pub(crate) fn is_explicit(&self) -> bool {
        self.model.is_some()
            || self.thinking.is_some()
            || self.reasoning_mode.is_some()
            || self.fast_mode.is_some()
            || self.chatgpt_account.is_some()
    }

    pub(crate) fn resolve(self) -> AgentSettings {
        let model = self.model.unwrap_or_else(|| Model::Sol.into());
        let defaults = AgentSettings::new(model);
        AgentSettings {
            model,
            thinking: self.thinking.unwrap_or(if model.oai().is_some() {
                Thinking::Xhigh
            } else {
                defaults.thinking
            }),
            reasoning_mode: self.reasoning_mode.unwrap_or(defaults.reasoning_mode),
            fast_mode: self.fast_mode.unwrap_or(model.supports_fast_mode()),
        }
    }

    /// Validates explicit models locally; default selection uses the live catalog.
    /// Provider availability is checked when the provider is used.
    /// Reopening an existing conversation must preserve its retained settings.
    pub(crate) async fn resolve_for_account(
        mut self,
        client: &ManagedClient,
    ) -> Result<AgentSettings, ManagedError> {
        if let Some(model) = self.model {
            if self.chatgpt_account.is_some() && model.oai().is_none() {
                return Err(ManagedError::Configuration(
                    "The requested model cannot be pinned to a ChatGPT account".to_owned(),
                ));
            }
            let settings = self.resolve();
            if !model.supports_thinking(settings.thinking)
                || !model.supports_reasoning_mode(settings.reasoning_mode)
                || (settings.fast_mode && !model.supports_fast_mode())
            {
                return Err(ManagedError::Configuration(
                    "The requested effort, reasoning mode, or fast mode is not offered for this model"
                        .to_owned(),
                ));
            }
            return Ok(settings);
        }
        let catalog = client.models().await?;
        let model = match self.model {
            Some(model) => model,
            None if self.chatgpt_account.is_some() => catalog
                .data
                .iter()
                .find(|entry| entry.provider == "openai" && entry.id == Model::Sol)
                .or_else(|| catalog.data.iter().find(|entry| entry.provider == "openai"))
                .map(|entry| entry.id)
                .ok_or_else(|| {
                    ManagedError::Configuration(
                        "No ChatGPT model is available for the requested account pin".to_owned(),
                    )
                })?,
            None => catalog.default_model.ok_or_else(|| {
                ManagedError::Configuration(
                    "No managed model is available; connect a provider subscription first"
                        .to_owned(),
                )
            })?,
        };
        let entry = catalog.data.iter().find(|entry| entry.id == model)
            .ok_or_else(|| ManagedError::Configuration("The requested model is not available to this account; inspect the managed model catalog".to_owned()))?;
        if self.chatgpt_account.is_some() && (entry.provider != "openai" || model.oai().is_none()) {
            return Err(ManagedError::Configuration(
                "The requested model cannot be pinned to a ChatGPT account".to_owned(),
            ));
        }
        if self.thinking.is_none() {
            let preferred = if model.oai().is_some() {
                Thinking::Xhigh
            } else {
                model.default_thinking()
            };
            self.thinking = Some(if entry.thinking.contains(&preferred) {
                preferred
            } else if entry.thinking.contains(&model.default_thinking()) {
                model.default_thinking()
            } else {
                entry.thinking[0]
            });
        }
        if self.fast_mode.is_none() {
            self.fast_mode = Some(entry.fast_mode);
        }
        self.model = Some(model);
        let settings = self.resolve();
        if !entry.thinking.contains(&settings.thinking)
            || !entry.reasoning_modes.contains(&settings.reasoning_mode)
            || (settings.fast_mode && !entry.fast_mode)
        {
            return Err(ManagedError::Configuration(
                "The requested effort, reasoning mode, or fast mode is not offered for this model"
                    .to_owned(),
            ));
        }
        Ok(settings)
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
    Model {
        #[arg(value_parser = parse_managed_model)]
        model: ManagedModel,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        settings: InitialSettings,
    }

    #[test]
    fn initial_settings_default_to_sol_xhigh_fast() {
        for settings in [
            InitialSettings::default(),
            Cli::parse_from(["test"]).settings,
        ] {
            let settings = settings.resolve();
            assert_eq!(settings.model, Model::Sol);
            assert_eq!(settings.thinking, Thinking::Xhigh);
            assert!(settings.fast_mode);
        }
    }

    #[test]
    fn initial_settings_preserve_overrides() {
        let settings = Cli::parse_from([
            "test",
            "--model",
            "astra",
            "--thinking",
            "high",
            "--fast-mode",
            "false",
        ])
        .settings
        .resolve();
        assert_eq!(settings.model, Model::Astra);
        assert_eq!(settings.thinking, Thinking::High);
        assert!(!settings.fast_mode);
        assert!(
            Cli::parse_from(["test", "--fast-mode"])
                .settings
                .resolve()
                .fast_mode
        );
    }
}
