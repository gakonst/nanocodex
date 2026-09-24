//! Native local and SSH Hand enrollment.

use clap::{Args, Subcommand};
use eyre::{Result, WrapErr, bail};
use serde_json::json;
use std::{fs, path::PathBuf, process::Stdio};
use tokio::{io::AsyncWriteExt, process::Command};

const LINUX_SERVICE: &str = "nanocodex-hand.service";

#[derive(Args)]
pub(crate) struct Hand {
    #[command(subcommand)]
    command: HandCommand,
}

#[derive(Subcommand)]
enum HandCommand {
    /// Install or repair the Hand on this machine or a remote Linux host.
    Install {
        /// SSH alias, hostname, IP, or user@host. Omit for this machine.
        #[arg(long, value_parser = ssh_target)]
        target: Option<String>,
        /// SSH port for --target; otherwise use normal SSH configuration.
        #[arg(short, long, requires = "target")]
        port: Option<u16>,
        /// macOS nanocodex2 executable override for local development.
        #[arg(long, conflicts_with = "target")]
        executable: Option<PathBuf>,
        /// macOS account file override for local development.
        #[arg(long, conflicts_with = "target")]
        account_file: Option<PathBuf>,
        /// Directory containing a development Linux nanocodex2 binary.
        #[arg(long, value_name = "DIRECTORY", hide = true)]
        artifacts: Option<PathBuf>,
    },
    /// Show local Hand service status as JSON.
    Status,
    /// Start the local Hand service.
    Start,
    /// Stop the local Hand service.
    Stop,
    /// Restart the local Hand service.
    Restart,
    /// Restore the LaunchAgent saved by an interrupted update.
    Recover,
}

pub(crate) fn ssh_target(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-@:%[]".contains(&byte))
    {
        return Err("Expected an SSH alias, IP, hostname, or user@host".into());
    }
    Ok(value.into())
}

/// One idempotent install entry point for guided setup and direct commands.
pub(crate) async fn install_default(
    target: Option<String>,
    port: Option<u16>,
    executable: Option<PathBuf>,
    account_file: Option<PathBuf>,
) -> Result<()> {
    install_with(target, port, executable, account_file, None).await
}

async fn install_with(
    target: Option<String>,
    port: Option<u16>,
    executable: Option<PathBuf>,
    account_file: Option<PathBuf>,
    artifacts: Option<PathBuf>,
) -> Result<()> {
    if target.is_none() && cfg!(target_os = "macos") {
        if artifacts.is_some() {
            bail!("--artifacts is only for a Linux Hand");
        }
        let _lock = crate::update::lock_service_operation()?;
        return crate::hand_service::ensure(executable, account_file).await;
    }
    if executable.is_some() || account_file.is_some() {
        bail!("--executable and --account-file apply only to a local macOS Hand");
    }
    if target.is_none() && !cfg!(target_os = "linux") {
        bail!(
            "Local Hand installation is not available on {}; use --target for a Linux host",
            std::env::consts::OS
        );
    }
    let destination = match target {
        Some(target) => Destination::Ssh {
            target: ssh_target(&target).map_err(eyre::Report::msg)?,
            port,
        },
        None => Destination::Local,
    };
    install_linux(destination, artifacts).await
}

enum Destination {
    Local,
    Ssh { target: String, port: Option<u16> },
}

impl Destination {
    fn label(&self) -> &str {
        match self {
            Self::Local => "this device",
            Self::Ssh { target, .. } => target,
        }
    }

    fn command(&self, program: &str, arguments: &[&str]) -> Command {
        let mut command = match self {
            Self::Local => {
                let mut command = Command::new(program);
                command.args(arguments);
                command
            }
            Self::Ssh { target, port } => {
                let mut command = Command::new("ssh");
                command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]);
                if let Some(port) = port {
                    command.args(["-p", &port.to_string()]);
                }
                command.arg("--").arg(target).arg(program).args(arguments);
                command
            }
        };
        command.kill_on_drop(true);
        command
    }

    async fn authorize_sudo(&self) -> Result<()> {
        let mut command = self.command("sudo", &["-n", "true"]);
        if matches!(self, Self::Local) {
            command = self.command("sudo", &["-v"]);
        }
        if !command.status().await?.success() {
            bail!(
                "{} needs {}sudo access to install the Hand service",
                self.label(),
                if matches!(self, Self::Ssh { .. }) {
                    "passwordless "
                } else {
                    ""
                }
            );
        }
        Ok(())
    }

    async fn upload(&self, local: &std::path::Path, remote: &str) -> Result<()> {
        match self {
            Self::Local => fs::copy(local, remote).map(|_| ()).map_err(Into::into),
            Self::Ssh { target, port } => {
                let mut command = Command::new("scp");
                command.args(["-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]);
                if let Some(port) = port {
                    command.args(["-P", &port.to_string()]);
                }
                let status = command
                    .arg("--")
                    .arg(local)
                    .arg(format!("{target}:{remote}"))
                    .status()
                    .await
                    .wrap_err("Could not start scp")?;
                if !status.success() {
                    bail!("Could not upload the native Hand installer");
                }
                Ok(())
            }
        }
    }

    async fn cleanup(&self, remote: &str) {
        let _ = self.command("rm", &["-f", "--", remote]).status().await;
    }
}

async fn install_linux(destination: Destination, artifacts: Option<PathBuf>) -> Result<()> {
    let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client
        .get(format!("{origin}/v1/me"))
        .bearer_auth(key.as_str())
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Account verification failed: {}", response.status());
    }
    let identity: serde_json::Value = response.json().await?;
    let owner = identity["user"]["id"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("Invalid account identity"))?;

    destination.authorize_sudo().await?;
    eprintln!(
        "Preparing the native Rust Hand for {}…",
        destination.label()
    );
    let binary = match artifacts {
        Some(directory) => fs::read(directory.join("nanocodex2"))
            .wrap_err_with(|| format!("Missing nanocodex2 in {}", directory.display()))?,
        None => crate::update::linux_hand_binary().await?,
    };
    if binary.get(..6) != Some(b"\x7fELF\x02\x01") || binary.get(18..20) != Some(b"\x3e\x00") {
        bail!("the Hand installer is not an x86_64 Linux executable");
    }
    let mut staged = tempfile::NamedTempFile::new()?;
    use std::io::Write as _;
    staged.write_all(&binary)?;
    staged.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    let remote = format!("/tmp/nanocodex-hand-{}", uuid::Uuid::new_v4());
    destination.upload(staged.path(), &remote).await?;
    let request = json!({"origin": origin, "credential": key.as_str(), "owner": owner});
    eprintln!(
        "Installing or repairing the Hand on {}…",
        destination.label()
    );
    let mut install = destination
        .command("sudo", &["-n", "--", &remote, "__install-hand"])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .wrap_err("Could not start the native Hand installer")?;
    install
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(&serde_json::to_vec(&request)?)
        .await?;
    let result = install.wait().await;
    destination.cleanup(&remote).await;
    if !result?.success() {
        bail!(
            "Hand setup did not become ready. Its private state was retained; rerun the command after correcting the reported error."
        );
    }
    Ok(())
}

async fn linux_service_status() -> Result<()> {
    let output = Command::new("systemctl")
        .args([
            "show",
            LINUX_SERVICE,
            "--no-pager",
            "--property=LoadState,ActiveState,SubState,MainPID,FragmentPath",
        ])
        .output()
        .await
        .wrap_err("Could not inspect the Linux Hand service")?;
    if !output.status.success() {
        bail!(
            "Could not inspect the Linux Hand service: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut values = std::collections::BTreeMap::new();
    let properties = String::from_utf8(output.stdout)?;
    for line in properties.lines() {
        if let Some((name, value)) = line.split_once('=') {
            values.insert(name, value);
        }
    }
    let load = values.get("LoadState").copied().unwrap_or("unknown");
    let active = values.get("ActiveState").copied().unwrap_or("unknown");
    let pid = values
        .get("MainPID")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid != 0);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "installed": load == "loaded",
            "loaded": active == "active",
            "pid": pid,
            "executable": "/opt/nanocodex/current/nanocodex2",
            "unit": LINUX_SERVICE,
            "load_state": load,
            "active_state": active,
            "sub_state": values.get("SubState").copied().unwrap_or("unknown"),
            "fragment_path": values.get("FragmentPath").copied().unwrap_or(""),
        }))?
    );
    Ok(())
}

async fn linux_service_action(action: &str) -> Result<()> {
    Destination::Local.authorize_sudo().await?;
    let status = Command::new("sudo")
        .args(["-n", "--", "systemctl", action, LINUX_SERVICE])
        .status()
        .await
        .wrap_err_with(|| format!("Could not {action} the Linux Hand service"))?;
    if !status.success() {
        bail!("Could not {action} the Linux Hand service: {status}");
    }
    Ok(())
}

impl Hand {
    pub(crate) async fn run(self) -> Result<()> {
        let _service_lock = if matches!(
            &self.command,
            HandCommand::Install { .. } | HandCommand::Status
        ) {
            None
        } else {
            Some(crate::update::lock_service_operation()?)
        };
        match self.command {
            HandCommand::Install {
                target,
                port,
                executable,
                account_file,
                artifacts,
            } => install_with(target, port, executable, account_file, artifacts).await,
            HandCommand::Status => {
                if cfg!(target_os = "linux") {
                    return linux_service_status().await;
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&crate::hand_service::status().await?)?
                );
                Ok(())
            }
            HandCommand::Start => {
                if cfg!(target_os = "linux") {
                    linux_service_action("start").await
                } else {
                    crate::update::start_hand().await
                }
            }
            HandCommand::Stop => {
                if cfg!(target_os = "linux") {
                    linux_service_action("stop").await
                } else {
                    crate::hand_service::stop().await
                }
            }
            HandCommand::Restart => {
                if cfg!(target_os = "linux") {
                    linux_service_action("restart").await
                } else {
                    crate::update::restart_hand().await
                }
            }
            HandCommand::Recover => {
                if cfg!(target_os = "linux") {
                    bail!("Linux Hand repairs are idempotent; rerun `nanocodex hand install`");
                }
                crate::update::recover_hand_update().await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        hand: Hand,
    }

    #[test]
    fn accepts_local_service_commands() {
        for action in ["install", "status", "start", "stop", "restart"] {
            assert!(TestCli::try_parse_from(["hand", action]).is_ok());
        }
        assert!(
            TestCli::try_parse_from([
                "hand",
                "install",
                "--executable",
                "/a path/nanocodex2",
                "--account-file",
                "/private/account.json"
            ])
            .is_ok()
        );
    }

    #[test]
    fn install_accepts_only_safe_remote_targets() {
        for target in ["paradigm", "ubuntu@192.0.2.5", "user@[2001:db8::1]"] {
            assert!(
                TestCli::try_parse_from(["hand", "install", "--target", target]).is_ok(),
                "{target}"
            );
        }
        for target in [
            "-oProxyCommand=evil",
            "host;id",
            "host\ncommand",
            "$(id)",
            "host path",
        ] {
            assert!(ssh_target(target).is_err());
        }
        assert!(TestCli::try_parse_from(["hand", "install", "--port", "2222"]).is_err());
        assert!(
            TestCli::try_parse_from([
                "hand",
                "install",
                "--target",
                "ubuntu@host",
                "--port",
                "2222",
                "--account-file",
                "/private/account.json"
            ])
            .is_err()
        );
    }
}
