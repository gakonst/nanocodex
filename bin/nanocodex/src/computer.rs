//! Installation management shared by both native CLIs.
use clap::{Args, Subcommand};

#[derive(Args)]
pub(crate) struct Computer {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install and select OpenAI's signed headless CUA components.
    Setup {
        /// Check OpenAI's component feed and update when its signed build changed.
        #[arg(long)]
        refresh: bool,
    },
}

impl Computer {
    pub(crate) async fn run(self) -> Result<(), String> {
        let Command::Setup { refresh } = self.command;
        let receipt = nanocodex_computer::provision::provision_upstream(refresh).await?;
        println!("{receipt}");
        Ok(())
    }
}
