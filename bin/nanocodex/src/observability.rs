use std::path::Path;

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::Result;
use nanocodex_observability::{LogOutput, ObservabilityGuard, ObservabilityOutputArgs};

const DEFAULT_FILTER: &str = "warn,nanocodex=info,nanocodex_agent=info,nanocodex_eval=info,nanocodex_oai_api=info,nanocodex_tools=info,nanocodex_vm=info,mpp_egress=info";

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
    pub(crate) fn install(self, interactive: bool, workspace: &Path) -> Result<ObservabilityGuard> {
        let default_output = if interactive {
            LogOutput::File(workspace.join(".nanocodex/logs/tui.log"))
        } else {
            LogOutput::Stderr
        };
        self.output
            .install(
                "nanocodex",
                env!("CARGO_PKG_VERSION"),
                self.log_filter,
                self.otel_filter,
                default_output,
            )
            .map_err(Into::into)
    }
}
