//! Idempotent first-run setup shared by the curl installer and manual repair.
use clap::Args;
use eyre::{Result, bail};

#[derive(Args)]
pub(crate) struct Setup {
    /// Recheck OpenAI's component feed in the background even when CUA is installed.
    #[arg(long)]
    refresh: bool,
    /// Compatibility option; native computer control needs no browser extension.
    #[arg(long, hide = true)]
    no_open_browser: bool,
    /// Skip managed account sign-in.
    #[arg(long)]
    skip_account: bool,
    /// Skip preparing the optional upstream Computer Use components.
    #[arg(long)]
    skip_computer: bool,
    /// Skip the persistent local Hand service.
    #[arg(long)]
    skip_hand: bool,
}

impl Setup {
    pub(crate) async fn run(self) -> Result<()> {
        eprintln!("Setting up Nanocodex…");
        if !self.skip_account {
            if nanocodex_cli_auth::has_default_login() {
                eprintln!("✓ Nanocodex account login found");
            } else {
                eprintln!("Sign in to the account used by nanocodex2 and Hand.");
                nanocodex_cli_auth::login_default().await?;
            }
        }

        if !self.skip_hand {
            if !nanocodex_cli_auth::has_default_login() {
                bail!(
                    "Hand needs an account login; rerun without --skip-account or run `nanocodex account login`"
                );
            }
            eprintln!("Installing or repairing the Hand on this machine…");
            crate::hand_setup::install_default(None, None, None, None).await?;
            eprintln!("✓ This machine's Hand is connected");
        }

        if !self.skip_computer {
            match crate::computer::setup_in_background(self.refresh) {
                Ok(Some(log)) => {
                    eprintln!(
                        "• Computer Use is preparing in the background. You can use Nanocodex now."
                    );
                    eprintln!("  Progress: {}", log.display());
                    eprintln!("  Run `nanocodex computer setup` to wait for completion or retry.");
                    eprintln!(
                        "  macOS may request Screen Recording and Accessibility on first Computer Use."
                    );
                }
                Ok(None) => {}
                Err(error) => eprintln!(
                    "Computer Use could not start ({error}). Retry with `nanocodex computer setup`."
                ),
            }
        }
        println!(
            "Nanocodex setup complete. Run `nanocodex setup` again any time to repair or resume it."
        );
        Ok(())
    }
}
