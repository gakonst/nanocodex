//! Native screen lifecycle shared by the CLI and the desktop app's Hand.
use super::screen_publisher::{ScreenBackend, ScreenPublisher};
use clap::Args;
use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_tools::attachment::{AttachmentMachine, AttachmentTarget};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub(crate) struct ScreenCommand {
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    machine_id: String,
    #[arg(long)]
    machine_name: String,
    #[arg(long)]
    state_dir: PathBuf,
}
#[cfg(target_os = "linux")]
#[derive(Args)]
pub(crate) struct DesktopCommand {
    #[arg(long)]
    workspace: PathBuf,
    #[arg(long)]
    runtime: PathBuf,
}
#[cfg(target_os = "linux")]
pub(crate) async fn serve_desktop(command: DesktopCommand) -> Result<(), ManagedError> {
    nanocodex_vm::desktop::serve(command.workspace, command.runtime)
        .await
        .map_err(configuration)
}
pub(crate) struct NativeScreen {
    publisher: Option<ScreenPublisher>,
    #[cfg(target_os = "linux")]
    desktop: tokio::process::Child,
    #[cfg(target_os = "linux")]
    runtime: PathBuf,
}
impl NativeScreen {
    pub(crate) async fn start(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        directory: &Path,
    ) -> Result<Self, ManagedError> {
        #[cfg(target_os = "macos")]
        {
            let _ = directory;
            let backend: ScreenBackend = std::sync::Arc::new(|input| {
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || super::screen_macos::request(input))
                        .await
                        .map_err(configuration)?
                })
            });
            let publisher = ScreenPublisher::start(target, machine, backend).await?;
            Ok(Self {
                publisher: Some(publisher),
            })
        }
        #[cfg(target_os = "linux")]
        {
            use std::time::{Duration, Instant};
            let runtime = directory.join("desktop");
            let mut command =
                tokio::process::Command::new(std::env::current_exe().map_err(configuration)?);
            command
                .arg("__hand-desktop")
                .arg("--workspace")
                .arg(machine.workspace())
                .arg("--runtime")
                .arg(&runtime)
                .current_dir("/")
                .env_clear()
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            for key in ["PATH", "HOME", "LANG", "LC_ALL"] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            let desktop = command.spawn().map_err(configuration)?;
            let mut screen = Self {
                publisher: None,
                desktop,
                runtime: runtime.clone(),
            };
            let ready = async {
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    if screen.desktop.try_wait().map_err(configuration)?.is_some() {
                        return Err(configuration(
                            "Hand desktop failed to start; install Xvfb, openbox, xterm, and fonts",
                        ));
                    }
                    if desktop_request(runtime.clone(), serde_json::json!({"action":"observe"}))
                        .await
                        .is_ok_and(|reply| reply["status"] == "ok")
                    {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(configuration(
                            "Hand desktop did not become ready within 30 seconds",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let backend: ScreenBackend = std::sync::Arc::new(move |input| {
                    let runtime = runtime.clone();
                    Box::pin(async move { desktop_request(runtime, input).await })
                });
                screen.publisher = Some(ScreenPublisher::start(target, machine, backend).await?);
                Ok(())
            }
            .await;
            if let Err(error) = ready {
                let _ = screen.shutdown().await;
                return Err(error);
            }
            Ok(screen)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (target, machine, directory);
            Err(configuration("native screens require macOS or Linux"))
        }
    }
    pub(crate) async fn shutdown(mut self) -> Result<(), ManagedError> {
        let result = if let Some(publisher) = self.publisher.take() {
            publisher.shutdown().await
        } else {
            Ok(())
        };
        #[cfg(target_os = "linux")]
        {
            let _ = desktop_request(
                self.runtime.clone(),
                serde_json::json!({"action":"shutdown"}),
            )
            .await;
            if tokio::time::timeout(std::time::Duration::from_secs(5), self.desktop.wait())
                .await
                .is_err()
            {
                let _ = self.desktop.kill().await;
            }
        }
        result
    }
}
pub(crate) async fn serve(
    client: &ManagedClient,
    command: ScreenCommand,
) -> Result<(), ManagedError> {
    let workspace = std::fs::canonicalize(command.workspace).map_err(configuration)?;
    let workspace = workspace
        .to_str()
        .ok_or_else(|| configuration("screen workspace must be UTF-8"))?;
    let machine = AttachmentMachine::new(
        command.machine_id,
        command.machine_name,
        workspace,
        ["screen"],
    )
    .map_err(configuration)?;
    let screen = NativeScreen::start(
        &client.account_attachment_target()?,
        &machine,
        &command.state_dir,
    )
    .await?;
    eprintln!("Hand screen is ready");
    let result = super::native_hand::shutdown_signal().await;
    let stopped = screen.shutdown().await;
    result.and(stopped)
}
fn configuration(error: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(error.to_string())
}

#[cfg(target_os = "linux")]
async fn desktop_request(
    runtime: PathBuf,
    input: serde_json::Value,
) -> Result<serde_json::Value, ManagedError> {
    tokio::task::spawn_blocking(move || {
        nanocodex_vm::desktop::request(&runtime, input).map_err(configuration)
    })
    .await
    .map_err(configuration)?
}
