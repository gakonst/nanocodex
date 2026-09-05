use clap::Args;
use eyre::{Result, eyre};

#[derive(Args)]
pub(crate) struct Eval {}

impl Eval {
    pub(crate) async fn run(self) -> Result<()> {
        Err(unsupported_target())
    }
}

fn unsupported_target() -> eyre::Report {
    eyre!("VM evaluation requires glibc Linux or Apple Silicon macOS")
}
