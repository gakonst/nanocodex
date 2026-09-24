//! Idempotent first-run setup shared by the curl installer and manual repair.
use clap::Args;
use eyre::{Result, WrapErr, bail};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const CHROME_EXTENSION_ID: &str = "hehggadaopoacecdllhhajmbjkdcmajg";
const EDGE_EXTENSION_ID: &str = "odlomjlbamekndcpllcnffbgeohgkmjh";
const CHROME_STORE: &str =
    "https://chromewebstore.google.com/detail/chatgpt/hehggadaopoacecdllhhajmbjkdcmajg";

#[derive(Args)]
pub(crate) struct Setup {
    /// Recheck OpenAI's component feed even when CUA is already installed.
    #[arg(long)]
    refresh: bool,
    /// Do not open the browser extension store automatically.
    #[arg(long)]
    no_open_browser: bool,
    /// Skip managed account sign-in.
    #[arg(long)]
    skip_account: bool,
    /// Skip Computer Use and browser bridge setup.
    #[arg(long)]
    skip_computer: bool,
    /// Skip the persistent local Hand service.
    #[arg(long)]
    skip_hand: bool,
}

fn extension_present(root: &Path, id: &str) -> bool {
    let Ok(profiles) = fs::read_dir(root) else {
        return false;
    };
    profiles.flatten().any(|profile| {
        let path = profile.path().join("Extensions").join(id);
        path.is_dir()
            && fs::read_dir(path)
                .is_ok_and(|versions| versions.flatten().any(|entry| entry.path().is_dir()))
    })
}

fn browser_extension_installed(home: &Path) -> bool {
    [
        (
            "Library/Application Support/Google/Chrome",
            CHROME_EXTENSION_ID,
        ),
        (
            "Library/Application Support/BraveSoftware/Brave-Browser",
            CHROME_EXTENSION_ID,
        ),
        ("Library/Application Support/Vivaldi", CHROME_EXTENSION_ID),
        (
            "Library/Application Support/com.operasoftware.Opera",
            CHROME_EXTENSION_ID,
        ),
        (
            "Library/Application Support/Microsoft Edge",
            EDGE_EXTENSION_ID,
        ),
    ]
    .into_iter()
    .any(|(root, id)| extension_present(&home.join(root), id))
}

fn prompt_open_extension() -> Result<bool> {
    eprint!("Open the official ChatGPT browser extension page now? [Y/n] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(!matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "n" | "no"
    ))
}

impl Setup {
    pub(crate) async fn run(self) -> Result<()> {
        eprintln!("Setting up Nanocodex…");
        if !self.skip_account {
            if nanocodex_cli_auth::has_default_login() {
                eprintln!("✓ Nanocodex account login found");
            } else {
                eprintln!("1/4 Sign in to the account used by nanocodex2 and Hand.");
                nanocodex_cli_auth::login_default().await?;
            }
        }

        if !self.skip_computer {
            eprintln!("2/4 Installing the signed upstream Computer Use components…");
            let receipt = nanocodex_computer::provision::provision_upstream(self.refresh)
                .await
                .map_err(eyre::Report::msg)?;
            if receipt["status"] != "installed" && cfg!(target_os = "macos") {
                bail!("Computer Use setup did not install a runtime");
            }
            if cfg!(target_os = "macos") {
                nanocodex_computer::provision::configure_browser_bridge()
                    .await
                    .map_err(eyre::Report::msg)?;
            }
            eprintln!("✓ Computer Use and its browser bridge are ready");
            if cfg!(target_os = "macos") {
                eprintln!(
                    "  macOS may request Screen Recording and Accessibility on first Computer Use."
                );
            }
        }

        if !self.skip_hand && cfg!(target_os = "macos") {
            if !nanocodex_cli_auth::has_default_login() {
                bail!(
                    "Hand needs an account login; rerun without --skip-account or run `nanocodex account login`"
                );
            }
            eprintln!("3/4 Ensuring the local Hand daemon is current and connected…");
            let _lock = crate::update::lock_service_operation()?;
            crate::hand_service::ensure(None, None).await?;
            eprintln!("✓ Hand is connected");
        }

        if cfg!(target_os = "macos") {
            let home = PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is unset"))?,
            );
            if browser_extension_installed(&home) {
                eprintln!("4/4 ✓ Official browser extension found");
            } else {
                eprintln!(
                    "4/4 Install the official ChatGPT browser extension to control browser tabs."
                );
                let open = !self.no_open_browser && prompt_open_extension()?;
                if open {
                    crate::auth::open_browser(CHROME_STORE)
                        .wrap_err("Could not open the browser extension page")?;
                }
                eprintln!("Extension page: {CHROME_STORE}");
            }
        }
        println!(
            "Nanocodex setup complete. Run `nanocodex setup` again any time to repair or resume it."
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_extension_in_any_profile() {
        let temp = tempfile::tempdir().unwrap();
        let version = temp
            .path()
            .join("Default/Extensions")
            .join(CHROME_EXTENSION_ID)
            .join("1.0.0");
        fs::create_dir_all(version).unwrap();
        assert!(extension_present(temp.path(), CHROME_EXTENSION_ID));
        assert!(!extension_present(temp.path(), EDGE_EXTENSION_ID));
    }
}
