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
        // Install the local service before any login prompt or network work.
        // It remains dormant until a verified saved account is available.
        if !self.skip_hand && cfg!(target_os = "macos") {
            crate::hand_setup::prepare_default(None).await?;
            eprintln!("✓ Hand is installed on this Mac");
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
        let mut login = None;
        if !self.skip_account {
            if nanocodex_cli_auth::has_default_login() {
                eprintln!("✓ Nanocodex account login found");
            } else {
                eprintln!("Sign in once to connect Nanocodex and this machine's Hand.");
                login = Some(nanocodex_cli_auth::login_default_with_receipt().await?);
            }
        }

        if !self.skip_hand {
            if cfg!(target_os = "macos") {
                if let Some(login) = login {
                    crate::hand_setup::connect_saved_login(
                        login.account_file,
                        login.origin,
                        login.credentials_changed,
                    )
                    .await?;
                    eprintln!("✓ This machine's Hand is connected");
                } else if nanocodex_cli_auth::has_default_login() {
                    crate::hand_setup::connect_saved_login(
                        nanocodex_cli_auth::saved_enrollment_account_file()?,
                        nanocodex_cli_auth::managed_url_from_environment(None)?,
                        false,
                    )
                    .await?;
                    eprintln!("✓ This machine's Hand is connected");
                } else {
                    println!(
                        "Hand is installed. Run `nanocodex account login` or `nanocodex2 login` to sign in and connect it automatically."
                    );
                    return Ok(());
                }
            } else {
                if !nanocodex_cli_auth::has_default_login() {
                    bail!(
                        "Hand needs an account login; rerun without --skip-account or run `nanocodex account login`"
                    );
                }
                eprintln!("Installing or repairing the Hand on this machine…");
                crate::hand_setup::install_default(None, None, None, None).await?;
                eprintln!("✓ This machine's Hand is connected");
            }
        }
        println!(
            "Nanocodex setup complete. Run `nanocodex setup` again any time to repair or resume it."
        );
        Ok(())
    }
}
