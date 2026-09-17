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
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let _ = directory;
            #[cfg(target_os = "windows")]
            nanocodex_hand::ensure_interactive_session().map_err(configuration)?;
            let backend: ScreenBackend = std::sync::Arc::new(|input| {
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || {
                        #[cfg(target_os = "macos")]
                        {
                            super::screen_macos::request(input)
                        }
                        #[cfg(target_os = "windows")]
                        {
                            nanocodex_hand::request(input).map_err(configuration)
                        }
                    })
                    .await
                    .map_err(configuration)?
                })
            });
            let publisher = ScreenPublisher::start(
                target,
                machine,
                backend,
                Some(native_video()),
                super::screen_audio::native_source(),
                super::observation_providers::Registry::local(),
            )
            .await?;
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
                let video_runtime = runtime.clone();
                let backend: ScreenBackend = std::sync::Arc::new(move |input| {
                    let runtime = runtime.clone();
                    Box::pin(async move { desktop_request(runtime, input).await })
                });
                screen.publisher = Some(
                    ScreenPublisher::start(
                        target,
                        machine,
                        backend,
                        Some(native_video(video_runtime)),
                        super::screen_audio::native_source(),
                        super::observation_providers::Registry::local(),
                    )
                    .await?,
                );
                Ok(())
            }
            .await;
            if let Err(error) = ready {
                let _ = screen.shutdown().await;
                return Err(error);
            }
            Ok(screen)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = (target, machine, directory);
            Err(configuration(
                "native screens require macOS, Windows, or Linux",
            ))
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
    let result = super::service::shutdown_signal().await;
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

#[cfg(target_os = "linux")]
fn native_video(runtime: PathBuf) -> super::screen_video::VideoSource {
    std::sync::Arc::new(move || {
        let runtime = runtime.clone();
        Box::pin(async move {
            let command = nanocodex_vm::desktop::video_command(&runtime)?;
            super::screen_video::Capture::ffmpeg(command)
        })
    })
}

#[cfg(target_os = "macos")]
fn native_video() -> super::screen_video::VideoSource {
    std::sync::Arc::new(|| {
        Box::pin(async {
            use std::process::Stdio;
            // Resolve AVFoundation's screen device explicitly; camera indices vary
            // with attached cameras. Never fall back to a camera or microphone.
            let devices = tokio::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-f",
                    "avfoundation",
                    "-list_devices",
                    "true",
                    "-i",
                    "",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output()
                .await?;
            let listing = String::from_utf8_lossy(&devices.stderr);
            let screen_name = format!("Capture screen {}", nanocodex_hand::main_display_index()?);
            let screen = listing
                .lines()
                .find_map(|line| {
                    let (prefix, _) = line.split_once(&screen_name)?;
                    let (_, index) = prefix.rsplit_once('[')?;
                    index
                        .trim()
                        .strip_suffix(']')
                        .and_then(|s| s.parse::<u16>().ok())
                })
                .ok_or("AVFoundation screen capture unavailable")?;
            let input = format!("{screen}:none");
            let mut command = std::process::Command::new("ffmpeg");
            command.args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "avfoundation",
                "-framerate",
                "60",
                "-capture_cursor",
                "1",
                "-pixel_format",
                "uyvy422",
                "-i",
                &input,
                "-an",
                "-r",
                "60",
                "-level",
                "3.2",
                "-vf",
                "scale=1280:1280:force_original_aspect_ratio=decrease:force_divisible_by=2",
                "-c:v",
                "h264_videotoolbox",
                "-realtime",
                "1",
                "-profile:v",
                "baseline",
                "-b:v",
                "6M",
                "-maxrate",
                "6M",
                "-bufsize",
                "100k",
                "-g",
                "30",
                "-bf",
                "0",
                "-bsf:v",
                "h264_metadata=aud=insert",
                "-flush_packets",
                "1",
                "-f",
                "h264",
                "pipe:1",
            ]);
            super::screen_video::Capture::ffmpeg(command)
        })
    })
}

#[cfg(target_os = "windows")]
fn native_video() -> super::screen_video::VideoSource {
    std::sync::Arc::new(|| {
        Box::pin(async {
            let command = nanocodex_hand::video_command()?;
            super::screen_video::Capture::ffmpeg(command)
        })
    })
}
