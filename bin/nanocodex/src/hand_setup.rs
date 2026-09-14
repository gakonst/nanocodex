//! SSH enrollment is client-owned; runtime ownership remains with systemd.
use clap::{Args, Subcommand};
use eyre::{Result, WrapErr, bail};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Stdio};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Args)]
pub(crate) struct Hand {
    #[command(subcommand)]
    command: HandCommand,
}

#[derive(Subcommand)]
enum HandCommand {
    /// Install or update a persistent Linux Hand and KVM factory over SSH.
    Add(Add),
    /// Install or update the Hand and VM factory on this device after account login.
    Setup(Setup),
}

#[derive(Args)]
struct Add {
    /// SSH alias, hostname, IP, or user@host. Uses your normal SSH configuration.
    #[arg(value_parser = ssh_target)]
    target: String,
    /// SSH port, when not specified in SSH configuration.
    #[arg(short, long)]
    port: Option<u16>,
    #[command(flatten)]
    setup: Setup,
}

#[derive(Args)]
struct Setup {
    /// Exact provider name passed to mount. Defaults to linux-<remote hostname>.
    #[arg(long)]
    factory_name: Option<String>,
    /// Skip the VM factory on a machine without KVM.
    #[arg(long)]
    native_only: bool,
    #[arg(long, default_value_t = 4)]
    max_vms: u16,
    #[arg(long, default_value_t = 2)]
    vm_cpus: u8,
    #[arg(long, default_value_t = 4096)]
    vm_memory_mib: u32,
    /// Use locally built nanocodex2 and nanocodex-vm-guest instead of a release.
    #[arg(long, value_name = "DIRECTORY")]
    artifacts: Option<PathBuf>,
}

fn ssh_target(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-@:%[]".contains(&b))
    {
        return Err("Expected an SSH alias, IP, hostname, or user@host".into());
    }
    Ok(value.into())
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
    fn command(&self, script: &str) -> Command {
        let mut command = match self {
            Self::Local => {
                let mut command = Command::new("sh");
                command.args(["-c", script]);
                command
            }
            Self::Ssh { target, port } => {
                let mut command = Command::new("ssh");
                command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]);
                if let Some(port) = port {
                    command.args(["-p", &port.to_string()]);
                }
                command.arg("--").arg(target).arg(script);
                command
            }
        };
        command.kill_on_drop(true);
        command
    }
}

impl Setup {
    async fn run(self, destination: Destination) -> Result<()> {
        if self.max_vms == 0 || self.vm_cpus == 0 || self.vm_memory_mib < 128 {
            bail!("VM capacity, CPU count, and memory must be positive (memory at least 128 MiB)");
        }
        let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
        // Verify account authority before changing a remote machine.
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
        if matches!(destination, Destination::Local)
            && !Command::new("sudo").arg("-v").status().await?.success()
        {
            bail!("Administrator access is required to install the Hand services");
        }
        eprintln!(
            "Checking SSH, sudo, systemd, and Linux on {}…",
            destination.label()
        );
        let preflight = destination.command("set -eu; test \"$(uname -s)\" = Linux; test \"$(uname -m)\" = x86_64; command -v python3 >/dev/null; command -v systemctl >/dev/null; sudo -n true; python3 -c 'import json,socket; print(json.dumps({\"hostname\":socket.gethostname()}))'")
            .output().await.wrap_err("Could not start SSH")?;
        if !preflight.status.success() {
            // No credentials have been sent; OpenSSH diagnostics are safe here.
            bail!(
                "SSH preflight failed. The host needs x86_64 Linux, systemd, Python 3, and passwordless sudo.\n{}",
                String::from_utf8_lossy(&preflight.stderr)
            );
        }
        let remote: serde_json::Value = serde_json::from_slice(&preflight.stdout)?;
        let hostname = remote["hostname"]
            .as_str()
            .ok_or_else(|| eyre::eyre!("SSH returned no hostname"))?;
        let factory = self
            .factory_name
            .clone()
            .unwrap_or_else(|| format!("linux-{}", hostname.split('.').next().unwrap_or(hostname)));
        if factory.len() > 63
            || !factory
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            || !factory
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !factory
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            bail!("Use --factory-name with a lowercase portable name of at most 63 characters");
        }
        let temporary = tempfile::tempdir()?;
        let mut bundle = if let Some(directory) = &self.artifacts {
            let mut artifacts = Vec::new();
            for name in ["nanocodex2", "nanocodex-vm-guest"] {
                let bytes = fs::read(directory.join(name))
                    .wrap_err_with(|| format!("Missing {name} in {}", directory.display()))?;
                if !bytes.starts_with(b"\x7fELF") {
                    bail!("{name} must be a Linux ELF binary");
                }
                let hash = hex::encode(Sha256::digest(&bytes));
                fs::write(temporary.path().join(name), bytes)?;
                artifacts.push(json!({"name":name,"sha256":hash,"local":true}));
            }
            json!({"release":"local", "artifacts":artifacts})
        } else {
            crate::update::linux_hand_artifacts().await?
        };
        bundle["origin"] = json!(origin);
        bundle["owner"] = json!(owner);
        bundle["factory_name"] = json!(factory);
        bundle["native_only"] = json!(self.native_only);
        bundle["max_vms"] = json!(self.max_vms);
        bundle["vm_cpus"] = json!(self.vm_cpus);
        bundle["vm_memory_mib"] = json!(self.vm_memory_mib);
        fs::write(
            temporary.path().join("install.py"),
            include_str!("hand_setup/install.py"),
        )?;
        fs::create_dir(temporary.path().join("toolkit"))?;
        for (name, content) in [
            (
                "toolkit/install-alpine.sh",
                include_str!(
                    "../../../crates/experimental/nanocodex-vm/image/toolkit/install-alpine.sh"
                ),
            ),
            (
                "toolkit/install-paths.sh",
                include_str!(
                    "../../../crates/experimental/nanocodex-vm/image/toolkit/install-paths.sh"
                ),
            ),
            (
                "toolkit/python.txt",
                include_str!("../../../crates/experimental/nanocodex-vm/image/toolkit/python.txt"),
            ),
            (
                "toolkit/check.py",
                include_str!("../../../crates/experimental/nanocodex-vm/image/toolkit/check.py"),
            ),
            (
                "Dockerfile",
                include_str!("../../../crates/experimental/nanocodex-vm/image/Dockerfile"),
            ),
            (
                "Dockerfile.ext4",
                include_str!("../../../crates/experimental/nanocodex-vm/image/Dockerfile.ext4"),
            ),
            (
                "populate-ext4.sh",
                include_str!("../../../crates/experimental/nanocodex-vm/image/populate-ext4.sh"),
            ),
            (
                "build-root.sh",
                include_str!("../../../crates/experimental/nanocodex-vm/image/build-root.sh"),
            ),
        ] {
            fs::write(temporary.path().join(name), content)?;
        }
        let archive = tempfile::NamedTempFile::new()?;
        let status = Command::new("tar")
            .arg("-czf")
            .arg(archive.path())
            .arg("-C")
            .arg(temporary.path())
            .arg(".")
            .status()
            .await?;
        if !status.success() {
            bail!("Could not package Hand setup");
        }
        let remote_dir = format!("/tmp/nanocodex-hand-{}", uuid::Uuid::new_v4());
        let mut upload = destination
            .command(&format!(
                "umask 077; mkdir {remote_dir} && tar -xzf - -C {remote_dir}"
            ))
            .stdin(Stdio::piped())
            .spawn()?;
        upload
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&fs::read(archive.path())?)
            .await?;
        if !upload.wait().await?.success() {
            bail!("Could not upload Hand setup");
        }
        eprintln!(
            "Installing Hand and {} on {}…",
            if self.native_only {
                "desktop"
            } else {
                "VM factory"
            },
            destination.label()
        );
        // Only this encrypted stdin carries the credential. It is never in argv,
        // the archive, progress output, or a local persisted setup file.
        bundle["credential"] = json!(key.as_str());
        let mut install = destination.command(&format!("sudo -n python3 {remote_dir}/install.py {remote_dir}; result=$?; rm -rf {remote_dir}; exit $result"))
            .stdin(Stdio::piped()).spawn()?;
        install
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&serde_json::to_vec(&bundle)?)
            .await?;
        if !install.wait().await?.success() {
            bail!(
                "Hand setup did not become ready. Its private state was retained; rerun the command after correcting the reported error."
            );
        }
        Ok(())
    }
}

impl Hand {
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            HandCommand::Add(add) => {
                add.setup
                    .run(Destination::Ssh {
                        target: add.target,
                        port: add.port,
                    })
                    .await
            }
            HandCommand::Setup(setup) => setup.run(Destination::Local).await,
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
    fn accepts_ssh_config_aliases_and_rejects_options_or_shell_programs() {
        for target in ["paradigm", "ubuntu@192.0.2.5", "user@[2001:db8::1]"] {
            assert!(TestCli::try_parse_from(["hand", "add", target]).is_ok());
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
    }
}
