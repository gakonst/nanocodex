//! Installation management shared by both native CLIs.
use clap::{Args, Subcommand};

#[derive(Args)]
pub(crate) struct Computer {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install and select OpenAI's signed CUA runtime without starting its GUI.
    Setup {
        /// Download the current upstream release instead of reusing a cached runtime.
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
