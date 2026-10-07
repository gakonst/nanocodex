use std::path::PathBuf;

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::{Result, eyre};
use nanocodex_observability::{LogOutput, ObservabilityGuard, ObservabilityOutputArgs};

const DEFAULT_FILTER: &str = "warn,nanocodex=info,nanocodex_agent=info,nanocodex_eval=info,nanocodex_oai_api=info,nanocodex_oai_tools=info,nanocodex_vm=info,mpp_egress=info";

#[derive(Args)]
pub(crate) struct ObservabilityArgs {
    /// Tracing filter directive. Defaults to Nanocodex lifecycle spans at info.
    #[arg(
        long,
        env = "RUST_LOG",
        default_value = DEFAULT_FILTER,
        value_parser = NonEmptyStringValueParser::new()
    )]
    log_filter: String,

    /// Tracing filter applied only to exported OpenTelemetry spans.
    #[arg(
        long,
        env = "OTEL_LEVEL",
        default_value = DEFAULT_FILTER,
        value_parser = NonEmptyStringValueParser::new()
    )]
    otel_filter: String,

    #[command(flatten)]
    output: ObservabilityOutputArgs,
}

impl ObservabilityArgs {
    pub(crate) fn install(self, interactive: bool) -> Result<ObservabilityGuard> {
        let log_file = if let Some(path) = self.output.log_file() {
            Some(path.to_owned())
        } else if interactive {
            Some(default_tui_log_file()?)
        } else {
            None
        };
        let default_output = log_file.clone().map_or(LogOutput::Stderr, LogOutput::File);
        let guard = self.output.install(
            "nanocodex",
            env!("CARGO_PKG_VERSION"),
            self.log_filter,
            self.otel_filter,
            default_output,
        )?;
        if interactive && let Some(path) = log_file {
            tracing::info!(
                pid = std::process::id(),
                log_file = %path.display(),
                "TUI logging initialized"
            );
        }
        Ok(guard)
    }
}

// Logs are persistent user state, independent of the launch/resume workspace.
fn default_tui_log_file() -> Result<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let state = match state {
        Some(state) => state,
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| {
                    std::env::var_os("USERPROFILE")
                        .map(PathBuf::from)
                        .filter(|path| path.is_absolute())
                })
                .ok_or_else(|| eyre!("cannot locate the TUI log directory; set XDG_STATE_HOME or HOME to an absolute path, or pass --log-file"))?;
            home.join(".local/state")
        }
    };
    // A conversation can be opened by multiple processes, so identify the
    // launch rather than naming the file only after the conversation or PID.
    let name = format!("tui-{}-{}.log", std::process::id(), uuid::Uuid::now_v7());
    Ok(state.join("nanocodex/logs").join(name))
}
