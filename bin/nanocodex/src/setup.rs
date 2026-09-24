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
    /// Also install or update a Linux Hand over SSH.
    #[arg(
        long,
        value_name = "USER@HOST",
        value_parser = crate::hand_setup::ssh_target,
        conflicts_with = "skip_hand"
    )]
    hand_target: Option<String>,
    /// SSH port for --hand-target; otherwise use normal SSH configuration.
    #[arg(long, requires = "hand_target")]
    hand_port: Option<u16>,
    /// Do not offer optional Linux Hand enrollment during interactive setup.
    #[arg(long)]
    no_remote_hand_prompt: bool,
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

fn remote_hand_answer(answer: &str) -> Result<Option<String>> {
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(None);
    }
    crate::hand_setup::ssh_target(answer)
        .map(Some)
        .map_err(eyre::Report::msg)
}

fn prompt_remote_hand() -> Result<Option<String>> {
    loop {
        eprint!("Optional Linux Hand SSH target (user@host, blank to skip): ");
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        match remote_hand_answer(&answer) {
            Ok(target) => return Ok(target),
            Err(error) => eprintln!("Invalid SSH target: {error}"),
        }
    }
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

        if !self.skip_computer {
            eprintln!("Installing the signed upstream Computer Use components…");
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
            eprintln!("Ensuring the local Hand daemon is current and connected…");
            let _lock = crate::update::lock_service_operation()?;
            crate::hand_service::ensure(None, None).await?;
            eprintln!("✓ Local Hand is connected");
        }

        if !self.skip_hand {
            let target = match self.hand_target {
                Some(target) => Some(target),
                None if self.no_remote_hand_prompt => None,
                None => prompt_remote_hand()?,
            };
            if let Some(target) = target {
                if !nanocodex_cli_auth::has_default_login() {
                    bail!(
                        "Linux Hand enrollment needs an account login; rerun without --skip-account or run `nanocodex account login`"
                    );
                }
                eprintln!("Installing the Linux Hand and VM factory on {target}…");
                crate::hand_setup::add_default(target.clone(), self.hand_port)
                    .await
                    .wrap_err_with(|| format!("Could not set up the Linux Hand on {target}"))?;
                eprintln!("✓ Linux Hand on {target} is connected");
            }
        }

        if cfg!(target_os = "macos") {
            let home = PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is unset"))?,
            );
            if browser_extension_installed(&home) {
                eprintln!("✓ Official browser extension found");
            } else {
                eprintln!(
                    "Install the official ChatGPT browser extension to control browser tabs."
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
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        setup: Setup,
    }

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

    #[test]
    fn optional_remote_hand_target_is_bounded_and_shell_safe() {
        assert_eq!(remote_hand_answer("\n").unwrap(), None);
        assert_eq!(
            remote_hand_answer(" ubuntu@hand.example \n").unwrap(),
            Some("ubuntu@hand.example".into())
        );
        for answer in ["-oProxyCommand=evil", "host;id", "host path", "$(id)"] {
            assert!(remote_hand_answer(answer).is_err(), "{answer}");
        }
    }

    #[test]
    fn remote_hand_flags_are_explicit_and_consistent() {
        let parsed = TestCli::try_parse_from([
            "setup",
            "--hand-target",
            "ubuntu@hand.example",
            "--hand-port",
            "2222",
            "--no-remote-hand-prompt",
        ])
        .unwrap();
        assert_eq!(
            parsed.setup.hand_target.as_deref(),
            Some("ubuntu@hand.example")
        );
        assert_eq!(parsed.setup.hand_port, Some(2222));
        assert!(parsed.setup.no_remote_hand_prompt);
        assert!(TestCli::try_parse_from(["setup", "--hand-port", "2222"]).is_err());
        assert!(
            TestCli::try_parse_from([
                "setup",
                "--skip-hand",
                "--hand-target",
                "ubuntu@hand.example"
            ])
            .is_err()
        );
    }
}
