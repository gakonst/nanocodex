//! Idempotent first-run setup shared by the curl installer and manual repair.
use clap::Args;
use eyre::{Result, bail};

#[derive(Args)]
pub(crate) struct Setup {
    /// Recheck OpenAI's component feed in the background even when CUA is installed.
    #[arg(long)]
    refresh: bool,
    /// Compatibility option; setup never opens a browser or installs an extension.
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
    /// Preview the shared Codex/Claude home links without creating them.
    #[arg(long)]
    dry_run_homes: bool,
}

impl Setup {
    pub(crate) async fn run(self) -> Result<()> {
        eprintln!("Setting up Nanocodex…");
        crate::homes::link_for_setup(self.dry_run_homes);
        // Install the local service before any login prompt or network work.
        // It remains dormant until a verified saved account is available.
        if !self.skip_hand && cfg!(any(target_os = "macos", target_os = "linux")) {
            crate::hand_setup::prepare_default(None).await?;
            eprintln!("✓ Hand daemon is installed on this computer");
        }

        if !self.skip_computer {
            match crate::computer::setup_in_background(self.refresh) {
                Ok(Some(log)) => {
                    eprintln!(
                        "• Computer Use is preparing in the background. You can use Nanocodex now."
                    );
                    eprintln!("  Progress: {}", log.display());
                    eprintln!("  Run `nanocodex computer setup` to wait for completion or retry.");
                    if cfg!(target_os = "macos") {
                        eprintln!(
                            "  The optional Computer Use app is a separate macOS app; macOS asks for its own permissions when it is first used."
                        );
                    }
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

        // The Hand's own macOS permissions are requested as soon as it connects.
        let mut permissions_ready = true;
        if !self.skip_hand {
            if cfg!(any(target_os = "macos", target_os = "linux")) {
                permissions_ready = if let Some(login) = login {
                    crate::hand_setup::connect_saved_login(
                        login.account_file,
                        login.origin,
                        login.credentials_changed,
                    )
                    .await?
                } else if nanocodex_cli_auth::has_default_login() {
                    crate::hand_setup::connect_saved_login(
                        nanocodex_cli_auth::saved_enrollment_account_file()?,
                        nanocodex_cli_auth::managed_url_from_environment(None)?,
                        false,
                    )
                    .await?
                } else {
                    println!(
                        "Hand is installed. Run `nanocodex account login` or `nanocodex2 login` to sign in and connect it automatically."
                    );
                    return Ok(());
                };
                eprintln!("✓ This machine's Hand is connected");
            } else {
                if !nanocodex_cli_auth::has_default_login() {
                    bail!(
                        "Hand needs an account login; rerun without --skip-account or run `nanocodex account login`"
                    );
                }
                eprintln!("Installing or repairing the Hand on this machine…");
                permissions_ready =
                    crate::hand_setup::install_default(None, None, None, None).await?;
                eprintln!("✓ This machine's Hand is connected");
            }
        }
        if permissions_ready {
            println!(
                "Nanocodex setup complete. Run `nanocodex setup` again any time to repair or resume it."
            );
        } else {
            println!(
                "Nanocodex is set up, but the Hand is waiting for the macOS permissions above. Live screen and input start after you allow them and run `nanocodex hand restart`."
            );
        }
        Ok(())
    }
}
