//! Native screen lifecycle shared by the CLI and the desktop app's Hand.
use super::screen_publisher::{ScreenBackend, ScreenPublisher};
use clap::Args;
use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_oai_tools::attachment::{AttachmentMachine, AttachmentTarget};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub(crate) struct ScreenCommand {
    #[command(flatten)]
    observability: super::hand_observability::HandObservabilityArgs,
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
    recorder: Option<super::hand_recording::Recorder>,
    #[cfg(target_os = "linux")]
    desktop: Option<DesktopChild>,
    #[cfg(target_os = "linux")]
    wayland: Option<super::screen_wayland::Platform>,
    #[cfg(target_os = "linux")]
    runtime: PathBuf,
    #[cfg(target_os = "linux")]
    _desktop_directory: Option<tempfile::TempDir>,
    #[cfg(target_os = "linux")]
    workspace: PathBuf,
}
// A cancelled startup future cannot await NativeScreen::shutdown. Keep the
// helper's graceful cleanup in its ownership guard, including runtime teardown.
#[cfg(target_os = "linux")]
struct DesktopChild(tokio::process::Child);
#[cfg(target_os = "linux")]
impl DesktopChild {
    fn terminate(&self) {
        if let Some(id) = self.0.id() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(id as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
    }
}
#[cfg(target_os = "linux")]
impl Drop for DesktopChild {
    fn drop(&mut self) {
        // Do not signal a reaped child's PID: it may now belong to someone else.
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        self.terminate();
        // The helper installs SIGTERM before starting infrastructure. Let its
        // cancellation guard reap Xvfb, the window manager and terminal tree.
        // Synchronous bounded cleanup cannot be abandoned by Tokio teardown.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !matches!(self.0.try_wait(), Ok(None)) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                let _ = self.0.start_kill();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
/// One attachment's healthy private desktop, kept between publication attempts
/// so a broker outage before the first publication does not restart the
/// desktop and the apps running in it. Dropping the slot stops that desktop.
#[derive(Default)]
pub(crate) struct DesktopSlot {
    #[cfg(target_os = "linux")]
    retained: std::sync::Mutex<Option<(DesktopChild, tempfile::TempDir)>>,
}
#[cfg(target_os = "linux")]
impl DesktopSlot {
    /// Only a still-running helper with its IPC socket is reused.
    fn take(&self) -> Option<(DesktopChild, tempfile::TempDir)> {
        let mut retained = self.retained.lock().ok()?.take()?;
        (matches!(retained.0.0.try_wait(), Ok(None))
            && retained.1.path().join("hand.sock").exists())
        .then_some(retained)
    }
    fn keep(&self, desktop: DesktopChild, directory: tempfile::TempDir) {
        if let Ok(mut retained) = self.retained.lock() {
            *retained = Some((desktop, directory));
        }
    }
}
impl NativeScreen {
    pub(crate) async fn start(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        directory: &Path,
    ) -> Result<Self, ManagedError> {
        Self::start_in(
            target,
            machine,
            directory,
            Some(&directory.join("recordings")),
            None,
        )
        .await
    }
    /// Like `start`, reusing `slot`'s private desktop across failed
    /// publication attempts of the same attachment.
    pub(crate) async fn start_retaining(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        directory: &Path,
        slot: &DesktopSlot,
    ) -> Result<Self, ManagedError> {
        Self::start_in(
            target,
            machine,
            directory,
            Some(&directory.join("recordings")),
            Some(slot),
        )
        .await
    }
    pub(crate) async fn start_with_recordings(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        directory: &Path,
        recording_root: Option<&Path>,
    ) -> Result<Self, ManagedError> {
        Self::start_in(target, machine, directory, recording_root, None).await
    }
    async fn start_in(
        target: &AttachmentTarget,
        machine: &AttachmentMachine,
        directory: &Path,
        recording_root: Option<&Path>,
        slot: Option<&DesktopSlot>,
    ) -> Result<Self, ManagedError> {
        #[cfg(not(target_os = "linux"))]
        let _ = slot;
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
            let (recorder, backend) =
                super::hand_recording::attach(recording_root, None, backend).await;
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
                recorder,
            })
        }
        #[cfg(target_os = "linux")]
        {
            if let super::screen_linux_session::Selection::Wayland(session) =
                super::screen_linux_session::select()?
            {
                // Once selected, Wayland startup/recovery errors remain errors;
                // never silently substitute an unrelated private desktop.
                let wayland = super::screen_wayland::Platform::start(session).await?;
                let (recorder, backend) =
                    super::hand_recording::attach(recording_root, None, wayland.backend()).await;
                let publisher = match ScreenPublisher::start(
                    target,
                    machine,
                    backend,
                    Some(wayland.video()),
                    None,
                    super::screen_audio::native_source(),
                    super::observation_providers::Registry::local(),
                )
                .await
                {
                    Ok(publisher) => publisher,
                    Err(error) => {
                        wayland.shutdown().await;
                        return Err(error);
                    }
                };
                return Ok(Self {
                    publisher: Some(publisher),
                    recorder,
                    desktop: None,
                    wayland: Some(wayland),
                    runtime: directory.join("desktop"),
                    _desktop_directory: None,
                    workspace: machine.workspace().into(),
                });
            }
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)
                .map_err(configuration)?;
            // A retired or failed publisher must never send shutdown to another
            // session's helper, even when both were given the same state-dir.
            // Account state paths contain a full identity hash and can exceed
            // sockaddr_un's path limit. Keep ephemeral desktop IPC in a short,
            // atomically created owner-private directory; durable state and
            // recordings remain under the account directory.
            let (desktop, desktop_directory) = match slot.and_then(DesktopSlot::take) {
                Some(retained) => {
                    tracing::info!(target: "nanocodex2", stage = "native.screen.desktop_reused",
                        "Reusing the running private desktop for another publication attempt");
                    retained
                }
                None => {
                    let desktop_directory = tempfile::Builder::new()
                        .prefix("nanocodex-desktop-")
                        .tempdir_in("/tmp")
                        .map_err(configuration)?;
                    let desktop = Self::spawn_desktop(
                        Path::new(machine.workspace()),
                        desktop_directory.path(),
                    )?;
                    (desktop, desktop_directory)
                }
            };
            let runtime = desktop_directory.path().to_owned();
            let mut screen = Self {
                publisher: None,
                recorder: None,
                desktop: Some(desktop),
                wayland: None,
                runtime: runtime.clone(),
                _desktop_directory: Some(desktop_directory),
                workspace: machine.workspace().into(),
            };
            let mut desktop_ready = false;
            let ready = async {
                screen.wait_desktop().await?;
                desktop_ready = true;
                let recording_runtime = runtime.clone();
                let video_runtime = runtime.clone();
                let backend: ScreenBackend = std::sync::Arc::new(move |input| {
                    let runtime = runtime.clone();
                    Box::pin(async move { desktop_request(runtime, input).await })
                });
                let (recorder, backend) =
                    super::hand_recording::attach(recording_root, Some(recording_runtime), backend)
                        .await;
                screen.recorder = recorder;
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
                // Publication failed after the desktop became ready: keep the
                // healthy desktop for this attachment's next attempt. Capture
                // and helper failures still stop it.
                if desktop_ready
                    && screen.publisher.is_none()
                    && let Some(slot) = slot
                    && let (Some(desktop), Some(directory)) =
                        (screen.desktop.take(), screen._desktop_directory.take())
                {
                    slot.keep(desktop, directory);
                }
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
    fn spawn_desktop(workspace: &Path, runtime: &Path) -> Result<DesktopChild, ManagedError> {
        #[cfg(not(test))]
        let executable = std::env::current_exe().map_err(configuration)?;
        // A Rust test executable cannot dispatch __hand-desktop. Opt-in Linux
        // integration tests supply the separately built companion, never a host
        // service command or an executable fetched from a broker response.
        #[cfg(test)]
        let executable = match std::env::var_os("NANOCODEX_TEST_NATIVE_SCREEN_BINARY") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe().map_err(configuration)?,
        };
        let mut command = tokio::process::Command::new(executable);
        command
            .arg("__hand-desktop")
            .arg("--workspace")
            .arg(workspace)
            .arg("--runtime")
            .arg(runtime)
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
        command.spawn().map(DesktopChild).map_err(configuration)
    }
    #[cfg(target_os = "linux")]
    async fn wait_desktop(&mut self) -> Result<(), ManagedError> {
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self
                .desktop
                .as_mut()
                .expect("desktop child")
                .0
                .try_wait()
                .map_err(configuration)?
                .is_some()
            {
                return Err(configuration(
                    "Hand desktop failed to start; install Xvfb, openbox, xterm, and fonts",
                ));
            }
            if desktop_request(
                self.runtime.clone(),
                serde_json::json!({"action":"observe"}),
            )
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
        Ok(())
    }
    async fn maintain_capture(&mut self) -> Result<bool, ManagedError> {
        // A replacement fence is terminal even if capture also needs repair.
        if self.is_finished() {
            return Ok(false);
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(wayland) = self.wayland.as_mut() {
                if wayland.is_finished() {
                    wayland.restart().await?;
                    return Ok(true);
                }
            } else {
                let running = match self.desktop.as_mut() {
                    Some(desktop) => desktop.0.try_wait().map_err(configuration)?.is_none(),
                    None => false,
                };
                // Existing X11 connections can survive an unlinked socket while
                // fresh encoder connections fail. Child liveness is not health.
                let reachable = if running {
                    let runtime = self.runtime.clone();
                    tokio::task::spawn_blocking(move || {
                        nanocodex_vm::desktop::display_socket_available(&runtime)
                    })
                    .await
                    .map_err(configuration)?
                    .map_err(configuration)?
                } else {
                    false
                };
                if !reachable {
                    // Stop/reap only our helper before reusing its runtime. Its
                    // IPC shutdown releases held input and its compositor tree.
                    self.stop_owned_desktop().await?;
                    if self.is_finished() {
                        return Ok(false);
                    }
                    self.desktop = Some(Self::spawn_desktop(&self.workspace, &self.runtime)?);
                    if let Err(error) = self.wait_desktop().await {
                        drop(self.desktop.take());
                        return Err(error);
                    }
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    #[cfg(target_os = "linux")]
    async fn stop_owned_desktop(&mut self) -> Result<(), ManagedError> {
        if let Some(mut desktop) = self.desktop.take()
            && desktop.0.try_wait().map_err(configuration)?.is_none()
        {
            let _ = desktop_request(
                self.runtime.clone(),
                serde_json::json!({"action":"shutdown"}),
            )
            .await;
            desktop.terminate();
            match tokio::time::timeout(std::time::Duration::from_secs(10), desktop.0.wait()).await {
                Ok(result) => {
                    result.map_err(configuration)?;
                }
                Err(_) => {
                    desktop.0.kill().await.map_err(configuration)?;
                }
            }
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    pub(crate) async fn refresh(&self, target: &AttachmentTarget) -> Result<(), ManagedError> {
        match &self.publisher {
            Some(publisher) => publisher.refresh(target).await,
            None => Err(configuration("native screen publisher unavailable")),
        }
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.publisher
            .as_ref()
            .is_none_or(ScreenPublisher::is_finished)
    }
    pub(crate) fn is_connected(&self) -> bool {
        self.publisher
            .as_ref()
            .is_some_and(ScreenPublisher::is_connected)
    }
    /// Owned capture infrastructure that visibly died and needs repair.
    fn capture_lost(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        {
            if let Some(wayland) = self.wayland.as_mut() {
                return wayland.is_finished();
            }
            self.desktop
                .as_mut()
                .is_none_or(|desktop| !matches!(desktop.0.try_wait(), Ok(None)))
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    pub(crate) async fn shutdown(mut self) -> Result<(), ManagedError> {
        if let Some(recorder) = self.recorder.take() {
            recorder.shutdown().await;
        }
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
            self.stop_owned_desktop().await?;
        }
        result
    }
}
impl super::screen_supervisor::Session for NativeScreen {
    type Error = ManagedError;
    fn is_finished(&self) -> bool {
        self.is_finished()
    }
    fn is_connected(&self) -> bool {
        self.is_connected()
    }
    fn capture_lost(&mut self) -> bool {
        self.capture_lost()
    }
    async fn maintain(&mut self) -> Result<bool, Self::Error> {
        self.maintain_capture().await
    }
    async fn shutdown(self) -> Result<(), Self::Error> {
        self.shutdown().await
    }
}
pub(crate) async fn serve(
    client: &ManagedClient,
    command: ScreenCommand,
) -> Result<(), ManagedError> {
    let _observability = command.observability.install().map_err(configuration)?;
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
    let target = client.account_attachment_target()?;
    let mut signal_result = Ok(());
    let mut shutdown_requested = false;
    let desktop = DesktopSlot::default();
    let stopped = super::screen_supervisor::supervise_observed(
        || NativeScreen::start_retaining(&target, &machine, &command.state_dir, &desktop),
        async {
            signal_result = super::service::shutdown_signal().await;
            shutdown_requested = true;
        },
        |error| match error {
            None => eprintln!("Hand screen is ready"),
            // Keep the process-owner status channel bounded and credential-free;
            // the configured tracing sink retains the underlying diagnostic.
            Some(_) => {
                eprintln!("Hand screen unavailable: capture startup or recovery failed; retrying")
            }
        },
    )
    .await;
    if !shutdown_requested && stopped.is_ok() {
        eprintln!("Hand screen publisher stopped");
    }
    // A terminal publisher exits without reclaiming its replacement. A shutdown
    // signal failure still takes precedence over capture cleanup, as before.
    signal_result.and(stopped)
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
            // Native Hands may start without Homebrew in PATH, just like the
            // recorder. Use its packaged/PATH/standard-install discovery.
            let ffmpeg = super::voice_recording::audio_program("ffmpeg");
            // Resolve AVFoundation's screen device explicitly; camera indices vary
            // with attached cameras. Never fall back to a camera or microphone.
            let devices = tokio::process::Command::new(&ffmpeg)
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
            let mut command = std::process::Command::new(ffmpeg);
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
                super::screen_video::Capture::bytes(reader, owner),
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

#[cfg(all(test, target_os = "linux"))]
mod recovery_integration_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::{os::unix::fs::MetadataExt, process::Stdio, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::Message;

    async fn assert_fresh_native_h264(runtime: &Path) {
        let command = nanocodex_vm::desktop::video_command(runtime).unwrap();
        let mut args: Vec<_> = command.get_args().map(|arg| arg.to_os_string()).collect();
        args.pop();
        args.extend(["-frames:v".into(), "2".into(), "pipe:1".into()]);
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(command.get_program())
                .args(args)
                .envs(command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(output.status.success(), "native X11 encoder failed");
        assert!(!output.stdout.is_empty());
        // Decode actual fresh X11 H.264, not a JPEG observation or a fixture packet.
        let mut decoder = tokio::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-frames:v",
                "2",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = decoder.stdin.take().unwrap();
        input.write_all(&output.stdout).await.unwrap();
        drop(input);
        let decoded = tokio::time::timeout(Duration::from_secs(30), decoder.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            decoded.status.success(),
            "fresh native H.264 did not decode"
        );
    }

    #[tokio::test]
    #[ignore = "requires isolated Linux Xvfb/openbox/xterm/FFmpeg and NANOCODEX_TEST_NATIVE_SCREEN_BINARY built from the fixed allocator"]
    async fn linux_missing_display_socket_restarts_owned_helper_and_keeps_webrtc() {
        let executable = PathBuf::from(
            std::env::var_os("NANOCODEX_TEST_NATIVE_SCREEN_BINARY")
                .expect("set the absolute separately built test companion path"),
        );
        assert!(executable.is_absolute() && executable.is_file());
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("desktop");
        let desktop = NativeScreen::spawn_desktop(workspace.path(), &runtime).unwrap();
        let mut screen = NativeScreen {
            publisher: None,
            recorder: None,
            desktop: Some(desktop),
            wayland: None,
            runtime: runtime.clone(),
            _desktop_directory: Some(directory),
            workspace: workspace.path().to_owned(),
        };
        screen.wait_desktop().await.unwrap();
        assert_fresh_native_h264(&runtime).await;
        let helper_pid = screen.desktop.as_ref().unwrap().0.id().unwrap();
        let display = std::fs::read_to_string(runtime.join("display")).unwrap();
        let number: u16 = display.strip_prefix(':').unwrap().parse().unwrap();
        assert!(
            number >= 100,
            "refuse to touch any legacy/production display such as X0"
        );
        let x_pid: u32 = std::fs::read_to_string(format!("/tmp/.X{number}-lock"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let status = std::fs::read_to_string(format!("/proc/{x_pid}/status")).unwrap();
        let parent = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert_eq!(
            parent, helper_pid,
            "only our isolated helper's Xvfb may be disrupted"
        );

        // Dummy loopback broker: authentic media is captured by the real helper;
        // no remote account, human viewer/control or microphone is involved.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (catalogs, mut published) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let broker = tokio::spawn(async move {
            let mut stopped = stopped;
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = stopped.changed() => break,
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.unwrap();
                        let catalogs = catalogs.clone();
                        connections.spawn(async move {
                            let mut prefix = [0; 4096];
                            let n = loop {
                                let n = stream.peek(&mut prefix).await.unwrap();
                                if n == 0 { return; }
                                if n >= 4 { break n; }
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            };
                            if prefix[..n].starts_with(b"GET ") {
                                let mut wire = tokio_tungstenite::accept_async(stream).await.unwrap();
                                wire.send(Message::Text(json!({"type":"ready","connection_id":"isolated"}).to_string().into())).await.unwrap();
                                while let Some(Ok(Message::Text(text))) = wire.next().await {
                                    let value: Value = serde_json::from_str(&text).unwrap();
                                    if value["type"] == "catalog" {
                                        catalogs.send(value).unwrap();
                                        wire.send(Message::Text(json!({"type":"published","generation":"isolated"}).to_string().into())).await.unwrap();
                                    }
                                }
                            } else {
                                let mut request = [0; 4096];
                                let _ = stream.read(&mut request).await;
                                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"iceServers\":[]}").await.unwrap();
                            }
                        });
                    }
                }
            }
            connections.shutdown().await;
        });
        let target = AttachmentTarget::new(
            format!("ws://{address}/v1/account/tool-host"),
            "isolated-test-token",
        )
        .unwrap();
        let machine = AttachmentMachine::new(
            "isolated-recovery",
            "Isolated recovery",
            workspace.path().to_str().unwrap(),
            ["screen"],
        )
        .unwrap();
        let backend_runtime = runtime.clone();
        let backend: ScreenBackend = std::sync::Arc::new(move |input| {
            let runtime = backend_runtime.clone();
            Box::pin(async move { desktop_request(runtime, input).await })
        });
        screen.publisher = Some(
            ScreenPublisher::start(
                &target,
                &machine,
                backend,
                Some(native_video(runtime.clone())),
                None,
                None,
                super::super::observation_providers::Registry::local(),
            )
            .await
            .unwrap(),
        );
        let publisher = screen.publisher.as_ref().unwrap() as *const ScreenPublisher;
        let first = tokio::time::timeout(Duration::from_secs(30), published.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(first["surfaces"][0].get("transport").is_none());
        assert!(first["surfaces"][0].get("frame_window").is_none());

        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
        let metadata = std::fs::symlink_metadata(&socket).unwrap();
        use std::os::unix::fs::FileTypeExt;
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.uid(), nix::unistd::getuid().as_raw());
        assert!(
            screen
                .desktop
                .as_mut()
                .unwrap()
                .0
                .try_wait()
                .unwrap()
                .is_none()
        );
        std::fs::remove_file(&socket).unwrap(); // ONLY the verified isolated helper socket (>= X100).
        assert!(!nanocodex_vm::desktop::display_socket_available(&runtime).unwrap());
        assert!(
            screen
                .desktop
                .as_mut()
                .unwrap()
                .0
                .try_wait()
                .unwrap()
                .is_none()
        );
        assert_eq!(
            desktop_request(runtime.clone(), json!({"action":"observe"}))
                .await
                .unwrap()["status"],
            "ok",
            "retained X11 capture must still work while fresh encoder connection fails"
        );
        assert!(nanocodex_vm::desktop::video_command(&runtime).is_err());
        assert!(screen.maintain_capture().await.unwrap());
        assert!(std::ptr::eq(publisher, screen.publisher.as_ref().unwrap()));
        let new_pid = screen.desktop.as_ref().unwrap().0.id().unwrap();
        assert_ne!(new_pid, helper_pid);
        assert!(
            !PathBuf::from(format!("/proc/{helper_pid}")).exists(),
            "old helper must be reaped before replacement"
        );
        assert!(
            !PathBuf::from(format!("/proc/{x_pid}")).exists(),
            "old owned Xvfb must be stopped"
        );
        assert!(nanocodex_vm::desktop::display_socket_available(&runtime).unwrap());
        assert_fresh_native_h264(&runtime).await;
        let recovered = tokio::time::timeout(Duration::from_secs(30), published.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            recovered["surfaces"][0].get("transport").is_none(),
            "recovery must remain WebRTC"
        );
        assert!(recovered["surfaces"][0].get("frame_window").is_none());
        screen.shutdown().await.unwrap();
        stop.send(true).unwrap();
        broker.await.unwrap();
    }
}
