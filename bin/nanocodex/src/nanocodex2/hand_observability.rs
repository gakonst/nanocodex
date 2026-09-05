use clap::{Args, builder::NonEmptyStringValueParser};
use nanocodex_observability::{
    LogOutput, ObservabilityError, ObservabilityGuard, ObservabilityOutputArgs,
};

const DEFAULT_FILTER: &str = "warn,nanocodex2=info,nanocodex_tools::attachment=info";

#[derive(Args)]
pub(crate) struct HandObservabilityArgs {
    /// Tracing filter directive. Full tool payload tracing remains opt-in.
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

impl HandObservabilityArgs {
    pub(crate) fn install(&self) -> Result<ObservabilityGuard, ObservabilityError> {
        self.output.install(
            "nanocodex2-hand",
            env!("CARGO_PKG_VERSION"),
            self.log_filter.clone(),
            self.otel_filter.clone(),
            LogOutput::Stderr,
        )
    }
}
