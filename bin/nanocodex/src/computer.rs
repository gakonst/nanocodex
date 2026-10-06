//! Installation management shared by both native CLIs.
use clap::{Args, Subcommand};
use std::{fs, path::PathBuf, process::Stdio};

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
        /// Internal background preparation; coalesce concurrent startup requests.
        #[arg(long, hide = true)]
        background: bool,
    },
}

impl Computer {
    pub(crate) async fn run(self) -> Result<(), String> {
        let Command::Setup {
            refresh,
            background,
        } = self.command;
        let _background_lock = if background {
            let directory = setup_directory()?;
            fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let mut options = fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
            }
            let lock = options
                .open(directory.join("background.lock"))
                .map_err(|error| error.to_string())?;
            match lock.try_lock() {
                Ok(()) => Some(lock),
                Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
        } else {
            None
        };
        if background {
            eprintln!("Preparing signed Computer Use components…");
        }
        let receipt = nanocodex_computer::provision::provision_upstream(refresh).await?;
        // Discover the exact provider catalog off the interactive path, so a
        // later attachment can register it from the version-bound cache.
        if background && receipt["status"] == "installed" {
            eprintln!("Components verified; preparing the Computer Use tool catalog…");
            let config = nanocodex_computer::provision::config_from_receipt(&receipt)?;
            nanocodex_computer::ComputerTools::connect(config)
                .await
                .map_err(|error| error.to_string())?;
            eprintln!("Computer Use is ready for the next attachment.");
        }
        println!("{receipt}");
        Ok(())
    }
}

/// Connect the Hand before optional CUA downloads. The child owns provisioning
/// (including its cross-process lock) and survives installer exit. Explicit
/// provider selections, including `off`, never trigger a download.
#[allow(dead_code)] // Also compiled into the managed CLI.
pub(crate) fn setup_in_background(refresh: bool) -> Result<Option<PathBuf>, String> {
    if !cfg!(target_os = "macos")
        || std::env::var_os("NANOCODEX_COMPUTER").is_some_and(|value| !value.is_empty())
    {
        return Ok(None);
    }
    let directory = setup_directory()?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let log_path = directory.join("setup.log");
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let log = options.open(&log_path).map_err(|error| error.to_string())?;
    if !log.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err("Computer Use setup log must be a regular file".into());
    }
    let mut command =
        std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.args(["computer", "setup", "--background"]);
    if refresh {
        command.arg("--refresh");
    }
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    // Reap while the caller stays open. Exiting the caller does not terminate
    // the independently owned installer.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(Some(log_path))
}

fn setup_directory() -> Result<PathBuf, String> {
    let base = std::env::var_os("NANOCODEX_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".nanocodex")))
        .ok_or("HOME or NANOCODEX_DIR is required for Computer Use setup")?;
    Ok(base.join("runtimes/openai-cua"))
}

/// Optional managed CUA may not hold shell, file access, or first input behind
/// MCP startup. Explicit custom providers retain their normal error contract.
pub(crate) async fn connect_for_startup()
-> Result<Option<nanocodex_computer::ComputerTools>, String> {
    if let Err(error) = setup_in_background(false) {
        tracing::warn!(%error, "could not start background Computer Use setup");
    }
    let Some(config) = nanocodex_computer::ComputerConfig::discover() else {
        return Ok(None);
    };
    if std::env::var_os("NANOCODEX_COMPUTER").is_some_and(|value| !value.is_empty()) {
        return nanocodex_computer::ComputerTools::connect(config)
            .await
            .map(Some)
            .map_err(|error| error.to_string());
    }
    match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        nanocodex_computer::ComputerTools::connect(config),
    )
    .await
    {
        Ok(Ok(computer)) => Ok(Some(computer)),
        Ok(Err(error)) => {
            tracing::warn!(%error, "optional Computer Use provider unavailable; continuing startup");
            Ok(None)
        }
        Err(_) => {
            tracing::info!(
                "Computer Use is still preparing; continuing startup with native Hand controls"
            );
            Ok(None)
        }
    }
}
