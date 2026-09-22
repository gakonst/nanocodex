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
    desktop: Option<tokio::process::Child>,
    #[cfg(target_os = "linux")]
    wayland: Option<super::screen_wayland::Platform>,
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
                            if input["action"].as_str() == Some("capabilities") {
                                return Ok(serde_json::json!({"status":"ok","relativePointer":true,"gamepad":false}));
                            }
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
                Some(native_command()),
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
            if std::env::var("NANOCODEX_SCREEN_BACKEND").as_deref() == Ok("wayland")
                || (std::env::var("NANOCODEX_SCREEN_BACKEND").is_err()
                    && std::env::var_os("WAYLAND_DISPLAY").is_some())
            {
                let wayland = super::screen_wayland::Platform::start().await?;
                let publisher = ScreenPublisher::start(
                    target,
                    machine,
                    wayland.backend(),
                    Some(wayland.video()),
                    None,
                    super::screen_audio::native_source(),
                    super::observation_providers::Registry::local(),
                )
                .await?;
                return Ok(Self {
                    publisher: Some(publisher),
                    desktop: None,
                    wayland: Some(wayland),
                    runtime: directory.join("desktop"),
                });
            }
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
                desktop: Some(desktop),
                wayland: None,
                runtime: runtime.clone(),
            };
            let ready = async {
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    if screen
                        .desktop
                        .as_mut()
                        .expect("desktop child")
                        .try_wait()
                        .map_err(configuration)?
                        .is_some()
                    {
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
                        Some(native_video(video_runtime.clone())),
                        Some(native_command(video_runtime)),
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
    #[cfg(target_os = "linux")]
    pub(crate) async fn refresh(&self, target: &AttachmentTarget) -> Result<(), ManagedError> {
        match &self.publisher {
            Some(publisher) => publisher.refresh(target).await,
            None => Err(configuration("native screen publisher unavailable")),
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn is_finished(&self) -> bool {
        self.publisher
            .as_ref()
            .is_none_or(ScreenPublisher::is_finished)
    }
    pub(crate) async fn shutdown(mut self) -> Result<(), ManagedError> {
        let result = if let Some(publisher) = self.publisher.take() {
            publisher.shutdown().await
        } else {
            Ok(())
        };
        #[cfg(target_os = "linux")]
        {
            if let Some(wayland) = self.wayland.take() {
                wayland.shutdown().await;
            }
            if let Some(mut desktop) = self.desktop.take() {
                let _ = desktop_request(
                    self.runtime.clone(),
                    serde_json::json!({"action":"shutdown"}),
                )
                .await;
                if tokio::time::timeout(std::time::Duration::from_secs(5), desktop.wait())
                    .await
                    .is_err()
                {
                    let _ = desktop.kill().await;
                }
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
fn native_command(runtime: PathBuf) -> super::screen_broadcast::Source {
    std::sync::Arc::new(move || {
        let runtime = runtime.clone();
        Box::pin(async move {
            let command = nanocodex_vm::desktop::video_command(&runtime)?;
            Ok(command)
        })
    })
}

#[cfg(target_os = "macos")]
fn native_command() -> super::screen_broadcast::Source {
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
            let (width, height) = nanocodex_hand::main_display_pixel_dimensions()?;
            let settings =
                nanocodex_hand::VideoSettings::from_environment(width, height, 3840, 24000)?;
            let scale = format!("scale={}:{}", settings.width, settings.height);
            let bitrate = format!("{}k", settings.bitrate_kbps);
            let buffer = format!("{}k", settings.bitrate_kbps / 30);
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
                settings.level,
                "-vf",
                &scale,
                "-c:v",
                "h264_videotoolbox",
                "-realtime",
                "1",
                "-profile:v",
                "baseline",
                "-b:v",
                &bitrate,
                "-maxrate",
                &bitrate,
                "-bufsize",
                &buffer,
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
            Ok(command)
        })
    })
}

#[cfg(target_os = "windows")]
fn native_command() -> super::screen_broadcast::Source {
    std::sync::Arc::new(|| {
        Box::pin(async {
            let command = nanocodex_hand::video_command()?;
            Ok(command)
        })
    })
}

#[cfg(all(test, target_os = "macos"))]
#[path = "screen_macos_live_test.rs"]
mod screen_macos_live_test;

#[cfg(target_os = "linux")]
fn native_video(runtime: PathBuf) -> super::screen_video::VideoSource {
    preview(native_command(runtime))
}
#[cfg(target_os = "windows")]
fn native_video() -> super::screen_video::VideoSource {
    preview(native_command())
}
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn preview(source: super::screen_broadcast::Source) -> super::screen_video::VideoSource {
    std::sync::Arc::new(move || {
        let source = source.clone();
        Box::pin(async move { super::screen_video::Capture::ffmpeg(source().await?) })
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod broadcast_live_tests {
    #[tokio::test]
    #[ignore = "requires screen permission and a local RTMP receiver"]
    async fn local_rtmp_native() {
        use serde_json::json;
        use std::time::Duration;
        let url = std::env::var("NANOCODEX_RTMP_TEST_URL").unwrap();
        let preset = std::env::var("NANOCODEX_RTMP_TEST_PRESET").unwrap_or("source".into());
        let broadcast = crate::screen_broadcast::Broadcast::new(
            Some(super::native_command()),
            crate::screen_audio::native_source(),
        );
        #[cfg(target_os = "macos")]
        let broadcast = broadcast.with_raw(super::native_broadcast_frames());
        let mut broadcast = broadcast;
        assert_eq!(
            broadcast
                .request(&json!({"action":"start","url":url,"preset":preset}))
                .await["status"],
            "starting"
        );
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let status = broadcast.request(&json!({"action":"status"})).await;
                if status["status"] == "live" {
                    break;
                }
                assert_ne!(status["status"], "failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        let seconds = std::env::var("NANOCODEX_RTMP_TEST_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        assert_eq!(
            broadcast.request(&json!({"action":"status"})).await["status"],
            "live"
        );
        assert_eq!(
            broadcast.request(&json!({"action":"stop"})).await["status"],
            "stopped"
        );
    }
}

/// Broadcast capture uses ScreenCaptureKit; AVFoundation can stall on newer
/// macOS releases. The native callback retains only the latest complete frame.
#[cfg(target_os = "macos")]
pub(crate) fn native_broadcast_frames() -> super::screen_broadcast::RawSource {
    native_raw_frames(3840, 2160)
}
#[cfg(target_os = "macos")]
fn native_raw_frames(max_width: usize, max_height: usize) -> super::screen_broadcast::RawSource {
    std::sync::Arc::new(move || {
        Box::pin(async move {
            use std::sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            };
            use tokio::io::AsyncWriteExt;
            struct Stop(Arc<AtomicBool>);
            impl Drop for Stop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            struct Writer {
                pipe: tokio::io::DuplexStream,
                runtime: tokio::runtime::Handle,
            }
            impl std::io::Write for Writer {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.runtime.block_on(self.pipe.write(bytes))
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let (native_width, native_height) = nanocodex_hand::main_display_pixel_dimensions()?;
            let scale = (max_width as f64 / native_width as f64)
                .min(max_height as f64 / native_height as f64)
                .min(1.0);
            let width = ((native_width as f64 * scale) as usize / 2 * 2).max(2);
            let height = ((native_height as f64 * scale) as usize / 2 * 2).max(2);
            let (reader, pipe) = tokio::io::duplex(256 * 1024);
            let stop = Arc::new(AtomicBool::new(false));
            let guard = Stop(stop.clone());
            let runtime = tokio::runtime::Handle::current();
            let worker = tokio::task::spawn_blocking(move || {
                nanocodex_hand::capture_video(Writer { pipe, runtime }, stop, width, height)
            });
            let owner = super::screen_video::Task(tokio::spawn(async move {
                let _guard = guard;
                let _ = worker.await;
            }));
            Ok((
                super::screen_video::Capture {
                    reader: Box::new(reader),
                    owner,
                },
                width,
                height,
            ))
        })
    })
}

#[cfg(target_os = "macos")]
fn native_video() -> super::screen_video::VideoSource {
    preview(native_command())
}
