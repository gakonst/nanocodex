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

impl From<HandCommand> for Hand {
    fn from(command: HandCommand) -> Self {
        Self { command }
    }
}

#[derive(Subcommand)]
pub(crate) enum HandCommand {
    /// Install or repair the Hand on this machine or a remote Linux host.
    Install {
        /// SSH alias, hostname, IP, or user@host. Omit for this machine.
        #[arg(long, value_parser = ssh_target)]
        target: Option<String>,
        /// SSH port for --target; otherwise use normal SSH configuration.
        #[arg(short, long, requires = "target")]
        port: Option<u16>,
        /// Hand executable override for local macOS or Windows development (default: this binary).
        #[arg(long, conflicts_with = "target")]
        executable: Option<PathBuf>,
        /// macOS account file override for local development.
        #[arg(long, conflicts_with = "target")]
        account_file: Option<PathBuf>,
        /// Directory containing a development Linux nanocodex (or nanocodex2) binary.
        #[arg(long, value_name = "DIRECTORY", hide = true)]
        artifacts: Option<PathBuf>,
        /// First-launch enrollment only; never replace or restart an owner.
        #[arg(long, hide = true, conflicts_with_all = ["target", "port", "account_file", "artifacts"])]
        if_missing: bool,
        /// Prepare a dormant local Hand service before account sign-in.
        #[arg(long, conflicts_with_all = ["target", "port", "account_file", "artifacts", "if_missing"])]
        prepare: bool,
    },
    /// Connect the local Hand using the exact login saved by account sign-in.
    Connect {
        /// Absolute path to the saved account credential file.
        #[arg(long)]
        account_file: Option<PathBuf>,
        /// Managed account origin used for this login.
        #[arg(long)]
        managed_url: Option<String>,
        /// Restart this owner after its saved credentials were replaced.
        #[arg(long)]
        credentials_changed: bool,
    },
    /// Install, repair, or reopen the standalone macOS Hand menu bar.
    MenuBar,
    /// Read-only menu snapshot: local service, verified login and connected Hands.
    MenuStatus,
    /// Show local Hand service status as JSON.
    Status,
    /// Keep this Mac available through idle sleep and screen lock (default: on).
    KeepAwake {
        /// Omit to inspect; on/off persists and applies without restarting the Hand.
        #[arg(value_parser = ["on", "off"])]
        setting: Option<String>,
    },
    /// Start the local Hand service.
    Start,
    /// Stop the local Hand service.
    Stop,
    /// Restart the local Hand service.
    Restart {
        /// Restart the existing macOS owner with a local development binary.
        #[arg(long)]
        executable: Option<PathBuf>,
    },
    /// Recover an interrupted coordinated CLI and device Hand update.
    Recover,
    /// Ask the running macOS Hand service to request Screen Recording and
    /// Accessibility consent for its own executable. You confirm in macOS.
    Permissions {
        /// Also open the matching System Settings pane for anything not yet allowed.
        #[arg(long, conflicts_with_all = ["check", "guide"])]
        open_settings: bool,
        /// Report what macOS allows the running Hand without prompting.
        #[arg(long, conflicts_with = "guide")]
        check: bool,
        /// With --check, print one JSON object for tools such as the menu bar.
        #[arg(long, requires = "check")]
        json: bool,
        /// Open System Settings with a floating panel to drag the Hand into the list.
        #[arg(long)]
        guide: bool,
    },
    #[command(flatten)]
    Registry(crate::hand_registry::Command),
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

/// Automatic first launch is limited to the unprivileged macOS LaunchAgent.
/// Hold the same lock as updates, then recheck ownership before any mutation.
/// Existing or concurrently installed publishers always retain their identity
/// and are never asked for consent; only an owner activated here is.
async fn install_missing_user_service(executable: Option<PathBuf>) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "Automatic Hand installation is unavailable on this platform. Run nanocodex setup to connect this computer."
        );
    }
    {
        let _lock = service_lock().await?;
        let state = crate::hand_service::status().await?;
        if (state.installed || state.loaded) && !crate::hand_service::is_pending().await? {
            crate::hand_menu_bar::ensure_with_warning(false).await;
            return Ok(());
        }
        let account_file = nanocodex_cli_auth::saved_enrollment_account_file()?;
        crate::hand_service::prepare(executable).await?;
        crate::hand_service::connect_saved_login(
            account_file,
            nanocodex_cli_auth::managed_url_from_environment(None)?,
            false,
        )
        .await?;
        crate::hand_menu_bar::ensure_with_warning(false).await;
    }
    // Output is discarded by the first-launch caller; macOS shows its own dialogs.
    request_onboarding_permissions().await;
    Ok(())
}

/// Serialize preparation, sign-in activation, repairs, and coordinated updates.
async fn service_lock() -> Result<fs::File> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match crate::update::lock_service_operation() {
            Ok(lock) => return Ok(lock),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Prepare the local OS service before authentication; desktop setup may continue in the background.
pub(crate) async fn prepare_default(executable: Option<PathBuf>) -> Result<()> {
    if cfg!(target_os = "linux") {
        return run_linux_installer(
            Destination::Local,
            None,
            executable,
            json!({"prepare": true}),
        )
        .await;
    }
    if !cfg!(target_os = "macos") {
        bail!("Preparing a Hand before sign-in is unavailable on this platform");
    }
    let _lock = service_lock().await?;
    crate::hand_service::prepare(executable).await?;
    crate::hand_menu_bar::ensure_with_warning(false).await;
    eprintln!("Hand service is installed; sign in to connect this computer.");
    Ok(())
}

/// Activate only the owner selected by this successful saved account login,
/// then request its OS permissions. Returns whether they are all allowed.
pub(crate) async fn connect_saved_login(
    account_file: PathBuf,
    managed_url: String,
    credentials_changed: bool,
) -> Result<bool> {
    if cfg!(target_os = "linux") {
        let (origin, key) =
            nanocodex_cli_auth::saved_enrollment_credentials(&account_file, &managed_url)?;
        install_linux_with_login(Destination::Local, None, origin, key.as_str()).await?;
        return Ok(true);
    }
    if !cfg!(target_os = "macos") {
        bail!("Saved-login Hand activation is unavailable on this platform");
    }
    {
        let _lock = service_lock().await?;
        crate::hand_service::connect_saved_login(account_file, managed_url, credentials_changed)
            .await?;
        crate::hand_menu_bar::ensure_with_warning(false).await;
    }
    eprintln!("Hand service is installed and connected.");
    Ok(request_onboarding_permissions().await)
}

/// One idempotent install entry point for guided setup and direct commands.
/// Returns whether a local macOS Hand's OS permissions are all allowed.
pub(crate) async fn install_default(
    target: Option<String>,
    port: Option<u16>,
    executable: Option<PathBuf>,
    account_file: Option<PathBuf>,
) -> Result<bool> {
    install_with(target, port, executable, account_file, None).await
}

async fn install_with(
    target: Option<String>,
    port: Option<u16>,
    executable: Option<PathBuf>,
    account_file: Option<PathBuf>,
    artifacts: Option<PathBuf>,
) -> Result<bool> {
    if target.is_none() && cfg!(target_os = "macos") {
        if artifacts.is_some() {
            bail!("--artifacts is only for a Linux Hand");
        }
        {
            let _lock = service_lock().await?;
            eprintln!("Installing or repairing the local Hand service…");
            crate::hand_service::ensure(executable, account_file).await?;
            crate::hand_menu_bar::ensure_with_warning(true).await;
        }
        eprintln!("Hand service is installed and connected.");
        // Consent belongs to the connected launchd owner, after the lock.
        return Ok(request_onboarding_permissions().await);
    }
    if target.is_none() && cfg!(target_os = "windows") {
        if artifacts.is_some() {
            bail!("--artifacts is only for a Linux Hand");
        }
        if account_file.is_some() {
            bail!("--account-file is only for a local macOS Hand");
        }
        let _lock = crate::update::lock_service_operation()?;
        crate::windows_hand::ensure(executable).await?;
        return Ok(true);
    }
    if executable.is_some() || account_file.is_some() {
        bail!(
            "--executable applies only to a local macOS or Windows Hand; --account-file applies only to macOS"
        );
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
    install_linux(destination, artifacts).await?;
    Ok(true)
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
        // `sudo -v` can require a password even when the requested command is
        // covered by NOPASSWD (for example a user also in Ubuntu's sudo group).
        // Honor existing unattended authorization before asking interactively.
        if self
            .command("sudo", &["-n", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?
            .success()
        {
            return Ok(());
        }
        if !matches!(self, Self::Local) || !self.command("sudo", &["-v"]).status().await?.success()
        {
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
    install_linux_with_login(destination, artifacts, origin, key.as_str()).await
}

async fn install_linux_with_login(
    destination: Destination,
    artifacts: Option<PathBuf>,
    origin: String,
    key: &str,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = client
        .get(format!("{origin}/v1/me"))
        .bearer_auth(key)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("Account verification failed: {}", response.status());
    }
    let identity: serde_json::Value = response.json().await?;
    let owner = identity["user"]["id"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("Invalid account identity"))?;

    let request = json!({"origin": origin, "credential": key, "owner": owner});
    run_linux_installer(destination, artifacts, None, request).await
}

async fn run_linux_installer(
    destination: Destination,
    artifacts: Option<PathBuf>,
    executable: Option<PathBuf>,
    request: serde_json::Value,
) -> Result<()> {
    destination.authorize_sudo().await?;
    eprintln!(
        "Preparing the native Rust Hand for {}…",
        destination.label()
    );
    let binary = match artifacts {
        Some(directory) => ["nanocodex2", "nanocodex"]
            .iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file())
            .map_or_else(
                || Err(eyre::eyre!("Missing nanocodex in {}", directory.display())),
                |path| fs::read(path).wrap_err("Could not read the Linux Hand binary"),
            )?,
        None => {
            let local = executable.or_else(|| crate::hand_executable::hand_binary().ok());
            if let Some(local) =
                local.filter(|local| matches!(destination, Destination::Local) && local.is_file())
            {
                fs::read(&local).wrap_err("Could not read the installed Hand binary")?
            } else {
                crate::update::linux_hand_binary().await?
            }
        }
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
    pub(crate) fn is_observation(&self) -> bool {
        matches!(
            self.command,
            HandCommand::MenuStatus
                | HandCommand::Permissions { check: true, .. }
                | HandCommand::KeepAwake { setting: None }
                | HandCommand::Status
                | HandCommand::Registry(crate::hand_registry::Command::List)
        )
    }

    pub(crate) async fn run(self) -> Result<()> {
        let _service_lock = if matches!(
            &self.command,
            HandCommand::Install { .. }
                | HandCommand::Connect { .. }
                | HandCommand::Status
                | HandCommand::MenuStatus
                | HandCommand::MenuBar
                // Consent is requested inside the already-running service;
                // its PID check, not the service lock, pins the target.
                | HandCommand::Permissions { .. }
                | HandCommand::KeepAwake { setting: None }
                // Registry edits are account-side; they never touch this
                // machine's service and must not queue behind its lock.
                | HandCommand::Registry(_)
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
                if_missing,
                prepare,
            } => {
                if prepare {
                    prepare_default(executable).await
                } else if if_missing {
                    install_missing_user_service(executable).await
                } else {
                    install_with(target, port, executable, account_file, artifacts)
                        .await
                        .map(drop)
                }
            }
            HandCommand::Connect {
                account_file,
                managed_url,
                credentials_changed,
            } => {
                let account_file = match account_file {
                    Some(path) => path,
                    None => nanocodex_cli_auth::saved_enrollment_account_file()?,
                };
                let managed_url = match managed_url {
                    Some(origin) => origin,
                    None => nanocodex_cli_auth::managed_url_from_environment(None)?,
                };
                connect_saved_login(account_file, managed_url, credentials_changed)
                    .await
                    .map(drop)
            }
            HandCommand::MenuBar => crate::hand_menu_bar::show().await,
            HandCommand::MenuStatus => crate::hand_menu_status::run().await,
            HandCommand::Registry(command) => command.run().await,
            HandCommand::Status => {
                #[cfg(target_os = "linux")]
                {
                    crate::linux_hand_service::print_status().await
                }
                #[cfg(not(target_os = "linux"))]
                {
                    if cfg!(target_os = "windows") {
                        return crate::windows_hand::print_status().await;
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&crate::hand_service::status().await?)?
                    );
                    Ok(())
                }
            }
            HandCommand::Start => crate::update::start_hand().await,
            HandCommand::Stop => {
                if cfg!(target_os = "linux") {
                    linux_service_action("stop").await
                } else if cfg!(target_os = "windows") {
                    crate::windows_hand::stop().await
                } else {
                    crate::hand_service::stop().await
                }
            }
            HandCommand::Restart { executable } => match executable {
                Some(path) => crate::update::restart_hand_with_executable(&path).await,
                None => crate::update::restart_hand().await,
            },
            HandCommand::Recover => crate::update::recover_hand_update().await,
            HandCommand::Permissions {
                check: true, json, ..
            } => check_permissions(json).await,
            HandCommand::Permissions { guide: true, .. } => crate::hand_menu_bar::guide().await,
            HandCommand::Permissions { open_settings, .. } => {
                request_permissions(open_settings).await
            }
            HandCommand::KeepAwake { setting } => keep_awake(setting.as_deref()).await,
        }
    }
}

/// macOS permissions the Hand needs for live screen and input, by reply key.
const PERMISSIONS: [(&str, &str, &str); 2] = [
    (
        "screenCapture",
        "Screen & System Audio Recording (live screen)",
        "Privacy_ScreenCapture",
    ),
    (
        "input",
        "Accessibility (mouse and keyboard control)",
        "Privacy_Accessibility",
    ),
];

/// Route the consent request to the process launchd is running. A request
/// made by this CLI would be attributed to the terminal app, not the Hand.
/// macOS skips already-allowed permissions and alone decides what is allowed.
async fn ask_daemon_for_consent() -> Result<(u32, PathBuf, serde_json::Value)> {
    ask_daemon_permissions(false).await
}

/// `check` asks for status only: no prompt, no new System Settings entry.
async fn ask_daemon_permissions(check: bool) -> Result<(u32, PathBuf, serde_json::Value)> {
    let state = crate::hand_service::status().await?;
    let (Some(pid), Some(executable)) = (state.pid, state.executable) else {
        bail!("The Hand service is not running. Start it with `nanocodex hand start`, then retry.");
    };
    // The running daemon's own executable speaks its own IPC protocol.
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        Command::new(&executable)
            .args([
                "__device-hand",
                if check {
                    "--check-permissions"
                } else {
                    "--request-permissions"
                },
                "--daemon-pid",
            ])
            .arg(pid.to_string())
            .arg("--daemon-executable")
            .arg(&executable)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .wrap_err("The running Hand did not answer the permission request")??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().map(str::trim).find(|line| !line.is_empty());
        bail!(
            "The running Hand could not request permissions: {}\nIf it predates `nanocodex hand permissions`, update it with `nanocodex update` and retry.",
            reason.unwrap_or("it exited without a reason")
        );
    }
    let reply: serde_json::Value = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok())
        .ok_or_else(|| eyre::eyre!("The running Hand returned no permission status"))?;
    for (key, ..) in PERMISSIONS {
        if !reply["permissions"][key]["granted"].is_boolean()
            || !reply["permissions"][key]["requested"].is_boolean()
        {
            bail!("The running Hand returned incomplete permission status; no grant was confirmed");
        }
    }
    Ok((pid, executable, reply))
}

fn executable_name(executable: &std::path::Path) -> String {
    executable.file_name().map_or_else(
        || executable.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Onboarding step after a connected macOS Hand is verified: request every
/// required permission together so macOS shows its consent dialogs now, not at
/// first screen or input use. Never undoes the connected service; returns
/// whether macOS currently allows everything. Other platforms need no consent.
pub(crate) async fn request_onboarding_permissions() -> bool {
    if !cfg!(target_os = "macos") {
        return true;
    }
    eprintln!(
        "Requesting Screen Recording and Accessibility for this Hand… Confirm any macOS dialogs that appear; nothing is allowed until you do."
    );
    let (pid, executable, reply) = match ask_daemon_for_consent().await {
        Ok(consent) => consent,
        Err(error) => {
            eprintln!(
                "Warning: the Hand is connected, but its macOS permissions could not be requested: {error:#}\nAllow them with `nanocodex hand permissions --open-settings`."
            );
            return false;
        }
    };
    let name = executable_name(&executable);
    let pending: Vec<&str> = PERMISSIONS
        .iter()
        .filter(|(key, ..)| reply["permissions"][key]["granted"] != true)
        .map(|(_, label, _)| *label)
        .collect();
    if pending.is_empty() {
        eprintln!(
            "✓ macOS allows the Hand ({name}, PID {pid}) Screen & System Audio Recording and Accessibility"
        );
        return true;
    }
    eprintln!(
        "Action needed: allow {} for \"{name}\" in the macOS prompts or System Settings > Privacy & Security, then run `nanocodex hand restart`.\nLive screen and input stay unavailable until then. If no prompt appeared: nanocodex hand permissions --open-settings",
        pending.join(" and ")
    );
    false
}

/// After an interactive update or restart starts the macOS Hand from a new
/// versioned executable, macOS keys Screen Recording and Accessibility to that
/// executable's path and code signature, so earlier grants do not carry over.
/// Re-request them from the new owner instead of leaving screen sharing
/// silently unavailable. Already-allowed owners print nothing.
pub(crate) async fn request_permissions_after_update() {
    if !cfg!(target_os = "macos") {
        return;
    }
    if let Ok((_, executable, reply)) = ask_daemon_permissions(true).await {
        if PERMISSIONS
            .iter()
            .all(|(key, ..)| reply["permissions"][key]["granted"] == true)
        {
            return;
        }
        eprintln!(
            "macOS has not allowed this Hand build ({}). It keys Screen Recording and Accessibility to the exact executable, so a new versioned Hand needs them again.",
            executable.display()
        );
    }
    request_onboarding_permissions().await;
}

/// `hand permissions --check`: read-only, safe to poll from the permission guide.
async fn check_permissions(json: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Hand permissions are checked only on macOS; this platform needs no consent step");
    }
    let (pid, executable, reply) = ask_daemon_permissions(true).await?;
    for (key, _, _) in PERMISSIONS {
        if reply["permissions"][key]["granted"].as_bool().is_none() {
            bail!("The running Hand returned an invalid permission status for {key}");
        }
    }
    if json {
        let permissions: serde_json::Map<String, serde_json::Value> = PERMISSIONS
            .iter()
            .map(|(key, _, pane)| {
                (
                    (*key).to_owned(),
                    serde_json::json!({
                        "granted": reply["permissions"][key]["granted"] == true,
                        "pane": pane,
                    }),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1,
                "daemon": {"pid": pid, "executable": executable},
                "permissions": permissions,
            })
        );
        return Ok(());
    }
    println!("Hand service PID {pid}, {}", executable.display());
    for (key, label, _) in PERMISSIONS {
        let granted = reply["permissions"][key]["granted"] == true;
        println!(
            "  {label}: {}",
            if granted { "allowed" } else { "not allowed" }
        );
    }
    Ok(())
}

/// Explicit `hand permissions`: one request per invocation with full status.
async fn request_permissions(open_settings: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Hand permissions are requested only on macOS; this platform needs no consent step");
    }
    println!("For a drag-to-allow panel, run: nanocodex hand permissions --guide");
    let (pid, executable, reply) = ask_daemon_for_consent().await?;
    println!(
        "Asked the running Hand service (PID {pid}, {}) to request macOS permissions for itself.",
        executable.display()
    );
    let mut pending = Vec::new();
    for (key, label, pane) in PERMISSIONS {
        let permission = &reply["permissions"][key];
        let status = match (
            permission["granted"].as_bool(),
            permission["requested"].as_bool(),
        ) {
            (Some(true), Some(false)) => "already allowed",
            (Some(true), _) => "allowed",
            (Some(false), _) => {
                pending.push(pane);
                "waiting for you to allow it in macOS"
            }
            _ => "not reported by this Hand",
        };
        println!("  {label}: {status}");
    }
    if pending.is_empty() {
        println!("If live screen is still unavailable, restart the Hand: nanocodex hand restart");
        return Ok(());
    }
    let name = executable_name(&executable);
    println!(
        "Nothing is allowed until you confirm it. macOS shows its prompt only once per Hand executable; if none appeared, enable \"{name}\" in System Settings > Privacy & Security{}.",
        if open_settings {
            ""
        } else {
            " (or rerun with --open-settings)"
        }
    );
    println!(
        "After allowing, restart the Hand so it uses the new permission: nanocodex hand restart"
    );
    if open_settings {
        for pane in pending {
            let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
            let opened = Command::new("/usr/bin/open")
                .arg(&url)
                .stdin(Stdio::null())
                .status()
                .await
                .is_ok_and(|status| status.success());
            if !opened {
                eprintln!(
                    "Could not open System Settings ({url}); open Privacy & Security manually."
                );
            }
        }
    }
    Ok(())
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

async fn keep_awake(setting: Option<&str>) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = setting;
        bail!("Hand keep-awake is currently available on macOS");
    }
    #[cfg(target_os = "macos")]
    {
        use crate::hand_keep_awake as preference;
        let home =
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is unset"))?);
        if !home.is_absolute() {
            bail!("HOME must be absolute");
        }
        let owner = crate::hand_service::status().await?;
        let before = preference::snapshot(&home, owner.pid)?;
        if let Some(setting) = setting {
            let enabled = setting == "on";
            if owner.pid.is_some() && before["supported_daemon"] != true {
                bail!(
                    "The running Hand has not reported keep-awake support. Update the Hand and retry; no setting was changed."
                );
            }
            if enabled && before["environment_override"] == true {
                bail!(
                    "The Hand service has {}=0. Remove that service override and restart it before enabling keep-awake; no setting was changed.",
                    preference::ENVIRONMENT
                );
            }
            preference::write(
                &preference::setting_path(&home),
                &json!({"enabled": enabled}),
            )?;
            if owner.pid.is_some() {
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let observed = preference::snapshot(&home, owner.pid)?;
                    if observed["active"] == enabled && observed["error"].is_null() {
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        bail!(
                            "Keep-awake preference was saved, but the running Hand has not confirmed applying it. Inspect with `nanocodex hand keep-awake`; do not assume its assertion changed."
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        // Re-read launchd after a concurrent service change; an old receipt
        // must never claim the replacement owner's assertion is active.
        let owner = crate::hand_service::status().await?;
        println!("{}", preference::snapshot(&home, owner.pid)?);
        Ok(())
    }
}
