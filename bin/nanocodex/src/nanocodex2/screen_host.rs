//! Standalone Linux publisher entrypoints use the same NativeScreen lifecycle as
//! the CLI. Scoped credentials remain in the owner's private rotating file.
use super::screen_native::NativeScreen;
use clap::Args;
use nanocodex_managed::ManagedError;
use nanocodex_remote::target::PublisherTarget;
use nanocodex_tools::attachment::{AttachmentMachine, AttachmentTarget};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Wayland,
    Desktop,
    Server,
}

#[derive(Args)]
pub(crate) struct HostCommand {
    #[command(flatten)]
    observability: super::hand_observability::HandObservabilityArgs,
    #[arg(long)]
    url: String,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long)]
    machine_id: String,
    #[arg(long, default_value = "Remote Hand")]
    name: String,
    #[arg(long, default_value = "/workspace")]
    workspace: PathBuf,
    #[arg(long, default_value = "/etc/nanocodex-desktop")]
    desktop_config: PathBuf,
    #[arg(long, default_value = "waymote-streamd")]
    waymote: PathBuf,
    #[arg(long, default_value_t = 1600)]
    width: u32,
    #[arg(long, default_value_t = 900)]
    height: u32,
    #[arg(long)]
    frames: bool,
    #[arg(long)]
    include_loopback: bool,
    #[arg(long)]
    interface: Option<String>,
    #[arg(long)]
    ipv4_only: bool,
    #[arg(long, default_value_t = 0)]
    udp_port_min: u16,
    #[arg(long, default_value_t = 0)]
    udp_port_max: u16,
}

pub(crate) struct Prepared {
    command: HostCommand,
    mode: Mode,
    target: PublisherTarget,
    machine: AttachmentMachine,
    runtime: tempfile::TempDir,
}
fn error(value: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(value.to_string())
}

impl HostCommand {
    /// Called in single-threaded process startup, before Tokio or audio threads.
    /// Return environment changes to main, which applies them only at that boundary.
    pub(crate) fn prepare(
        self,
        mode: Mode,
    ) -> Result<(Prepared, Vec<(String, String)>), ManagedError> {
        if !self.workspace.is_absolute()
            || !self.desktop_config.is_absolute()
            || !(1..=16384).contains(&self.width)
            || !(1..=16384).contains(&self.height)
            || (self.udp_port_min == 0) != (self.udp_port_max == 0)
            || self.udp_port_min > self.udp_port_max
            || self
                .interface
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 128 || v.bytes().any(|b| b <= 32))
        {
            return Err(error("invalid standalone desktop configuration"));
        }
        let target = PublisherTarget::from_credential_file(&self.url, &self.credential_file)
            .map_err(error)?;
        let scope = target.endpoint().path();
        if mode == Mode::Desktop && !scope.starts_with("/v1/vm-host-attachments/")
            || mode == Mode::Server && !scope.starts_with("/v1/hand-hosts/")
        {
            return Err(error("desktop requires its scoped publisher endpoint"));
        }
        let workspace = std::fs::canonicalize(&self.workspace)
            .map_err(|_| error("desktop workspace is unavailable"))?;
        let machine = AttachmentMachine::new(
            &self.machine_id,
            &self.name,
            workspace
                .to_str()
                .ok_or_else(|| error("desktop workspace must be UTF-8"))?,
            ["screen"],
        )
        .map_err(error)?;
        let runtime = tempfile::Builder::new()
            .prefix("nanocodex-desktop-")
            .tempdir()
            .map_err(error)?;
        let mut environment = vec![
            ("NANOCODEX_SCREEN_BACKEND".into(), "wayland".into()),
            (
                "NANOCODEX_WAYMOTE".into(),
                self.waymote
                    .to_str()
                    .ok_or_else(|| error("invalid Waymote path"))?
                    .into(),
            ),
            (
                "NANOCODEX_SCREEN_TRANSPORT".into(),
                if self.frames { "frames-v1" } else { "webrtc" }.into(),
            ),
            (
                "NANOCODEX_VIDEO_INCLUDE_LOOPBACK".into(),
                if self.include_loopback { "1" } else { "0" }.into(),
            ),
            (
                "NANOCODEX_VIDEO_IPV4_ONLY".into(),
                if self.ipv4_only { "1" } else { "0" }.into(),
            ),
        ];
        if let Some(interface) = &self.interface {
            environment.push(("NANOCODEX_VIDEO_INTERFACE".into(), interface.clone()));
        }
        if self.udp_port_min != 0 {
            environment.push((
                "NANOCODEX_VIDEO_UDP_PORTS".into(),
                format!("{}-{}", self.udp_port_min, self.udp_port_max),
            ));
        }
        if mode != Mode::Wayland {
            environment.extend([
                (
                    "XDG_RUNTIME_DIR".into(),
                    runtime
                        .path()
                        .to_str()
                        .ok_or_else(|| error("invalid desktop runtime path"))?
                        .into(),
                ),
                ("WAYLAND_DISPLAY".into(), "wayland-0".into()),
                ("WLR_BACKENDS".into(), "headless".into()),
                ("WLR_RENDERER".into(), "pixman".into()),
                ("WLR_HEADLESS_OUTPUTS".into(), "1".into()),
                ("XDG_SESSION_TYPE".into(), "wayland".into()),
                ("XDG_CURRENT_DESKTOP".into(), "labwc".into()),
            ]);
        }
        Ok((
            Prepared {
                command: self,
                mode,
                target,
                machine,
                runtime,
            },
            environment,
        ))
    }
}

struct Desktop {
    compositor: Child,
    group: i32,
    playback: Option<Child>,
}
impl Drop for Desktop {
    fn drop(&mut self) {
        // The compositor owns its terminal/autostart descendants. It must not
        // outlive the allocated Hand after startup error, cancellation or exit.
        if self.group > 0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.group),
                nix::sys::signal::Signal::SIGKILL,
            );
            self.group = 0;
        }
        let _ = self.compositor.start_kill();
        if let Some(playback) = &mut self.playback {
            let _ = playback.start_kill();
        }
    }
}
impl Desktop {
    async fn start(prepared: &Prepared) -> Result<Self, ManagedError> {
        let playback_exists = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new("pactl")
                .arg("get-default-sink")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .is_ok_and(|v| v.is_ok_and(|s| s.success()));
        let playback = if playback_exists {
            None
        } else {
            Command::new("pulseaudio")
                .args([
                    "--daemonize=no",
                    "--exit-idle-time=-1",
                    "--log-target=stderr",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .ok()
        };
        let compositor = Command::new("labwc")
            .arg("--config-dir")
            .arg(&prepared.command.desktop_config)
            .current_dir(&prepared.command.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .map_err(|_| error("cannot start desktop compositor"))?;
        let group = compositor
            .id()
            .ok_or_else(|| error("desktop compositor unavailable"))? as i32;
        let mut desktop = Self {
            compositor,
            group,
            playback,
        };
        let socket = prepared.runtime.path().join("wayland-0");
        let ready = tokio::time::timeout(Duration::from_secs(20), async {
            use std::os::unix::fs::FileTypeExt;
            loop {
                if desktop.compositor.try_wait().map_err(error)?.is_some() {
                    return Err(error("desktop compositor stopped"));
                }
                if std::fs::metadata(&socket).is_ok_and(|m| m.file_type().is_socket()) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| error("desktop compositor startup timed out"))?;
        ready?;
        // Set the actual compositor mode; catalog dimensions come from capture.
        let mode = format!("{}x{}", prepared.command.width, prepared.command.height);
        let resized = tokio::time::timeout(
            Duration::from_secs(3),
            Command::new("wlr-randr")
                .args(["--output", "HEADLESS-1", "--custom-mode", &mode])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        if !resized.is_ok_and(|r| r.is_ok_and(|s| s.success())) {
            return Err(error("cannot configure desktop output"));
        }
        Ok(desktop)
    }
    async fn stop(mut self) {
        if self.group > 0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.group),
                nix::sys::signal::Signal::SIGKILL,
            );
            self.group = 0;
        }
        let _ = self.compositor.wait().await;
        if let Some(mut playback) = self.playback.take() {
            let _ = playback.kill().await;
            let _ = playback.wait().await;
        }
    }
}

struct Markers {
    ready: PathBuf,
    status: PathBuf,
    owned: Vec<(PathBuf, u64, u64)>,
}
impl Markers {
    fn new(credential: &Path) -> Self {
        let mut ready = credential.as_os_str().to_os_string();
        ready.push(".ready");
        let mut status = credential.as_os_str().to_os_string();
        status.push(".status");
        Self {
            ready: ready.into(),
            status: status.into(),
            owned: Vec::new(),
        }
    }
    fn write(&mut self, status: bool, value: &[u8]) -> Result<(), ManagedError> {
        use std::os::unix::fs::MetadataExt;
        let path = if status { &self.status } else { &self.ready };
        let parent = path
            .parent()
            .ok_or_else(|| error("publisher marker has no parent"))?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        file.write_all(value).map_err(error)?;
        let file = file
            .persist(path)
            .map_err(|_| error("cannot publish desktop readiness"))?;
        let metadata = file.metadata().map_err(error)?;
        self.owned.retain(|(owned, _, _)| owned != path);
        self.owned
            .push((path.clone(), metadata.dev(), metadata.ino()));
        Ok(())
    }
}
impl Drop for Markers {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        for (path, device, inode) in &self.owned {
            if std::fs::symlink_metadata(path)
                .is_ok_and(|m| m.dev() == *device && m.ino() == *inode)
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
fn attachment(target: &PublisherTarget) -> Result<AttachmentTarget, ManagedError> {
    AttachmentTarget::new(target.attachment_endpoint().as_str(), target.bearer()).map_err(error)
}

pub(crate) async fn serve(mut prepared: Prepared) -> Result<(), ManagedError> {
    let _observability = prepared.command.observability.install().map_err(error)?;
    let mut desktop = if prepared.mode == Mode::Wayland {
        None
    } else {
        Some(Desktop::start(&prepared).await?)
    };
    let mut markers = Markers::new(&prepared.command.credential_file);
    markers.write(false, b"ready\n")?;
    let signal = super::service::shutdown_signal();
    tokio::pin!(signal);
    // Compositor readiness is independent of account signaling. Retain it across
    // startup retries; use the same shared publisher lifecycle for every mode.
    let screen = loop {
        let target = attachment(&prepared.target)?;
        let started = tokio::select! {
            result = &mut signal => { if let Some(desktop) = desktop.take() { desktop.stop().await; } return result; }
            result = NativeScreen::start(&target, &prepared.machine, prepared.runtime.path()) => result,
        };
        match started {
            Ok(screen) => break screen,
            Err(_) => {
                markers.write(true, b"publisher unavailable\n")?;
                prepared.target = PublisherTarget::from_credential_file(
                    &prepared.command.url,
                    &prepared.command.credential_file,
                )
                .map_err(error)?;
                tokio::select! {
                    result = &mut signal => { if let Some(desktop) = desktop.take() { desktop.stop().await; } return result; }
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
    };
    markers.write(true, b"published\n")?;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut replaced = false;
    let result = loop {
        tokio::select! {
            result = &mut signal => break result,
            _ = tick.tick() => {
                if screen.is_finished() { replaced = true; break Ok(()); }
                if let Some(desktop) = &mut desktop {
                    match desktop.compositor.try_wait() { Ok(None) => {}, _ => break Err(error("desktop compositor stopped")) }
                }
                let next = match PublisherTarget::from_credential_file(&prepared.command.url, &prepared.command.credential_file) {
                    Ok(next) => next,
                    Err(_) => break Ok(()), // Owner removed or revoked this private grant.
                };
                if next.bearer() != prepared.target.bearer() || next.endpoint() != prepared.target.endpoint() {
                    if let Err(error) = screen.refresh(&attachment(&next)?).await { break Err(error); }
                    prepared.target = next;
                }
            }
        }
    };
    let stopped = screen.shutdown().await;
    if let Some(desktop) = desktop {
        desktop.stop().await;
    }
    drop(markers);
    // A service supervisor must not restart this publisher and reclaim a newer
    // host's screen. Only an explicit service restart republishes after takeover.
    if replaced {
        return (&mut signal).await.and(result).and(stopped);
    }
    result.and(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    fn command(directory: &Path, origin: String) -> HostCommand {
        let credential_file = directory.join("credential");
        std::fs::write(&credential_file, "synthetic-host-token\n").unwrap();
        std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        HostCommand {
            observability: clap::FromArgMatches::from_arg_matches(
                &super::super::hand_observability::HandObservabilityArgs::augment_args(
                    clap::Command::new("host"),
                )
                .try_get_matches_from(["host"])
                .unwrap(),
            )
            .unwrap(),
            url: origin,
            credential_file,
            machine_id: "host:test".into(),
            name: "Test desktop".into(),
            workspace: directory.into(),
            desktop_config: directory.into(),
            waymote: "waymote-streamd".into(),
            width: 1920,
            height: 1080,
            frames: false,
            include_loopback: false,
            interface: None,
            ipv4_only: false,
            udp_port_min: 0,
            udp_port_max: 0,
        }
    }
    #[test]
    fn standalone_modes_keep_their_credential_scope_and_prepare_only_owned_environment() {
        let directory = tempfile::tempdir().unwrap();
        let (host, environment) = command(directory.path(), "https://managed.example".into())
            .prepare(Mode::Wayland)
            .unwrap();
        assert_eq!(host.target.endpoint().path(), "/v1/account/hands");
        assert!(!environment.iter().any(|(key, _)| key == "XDG_RUNTIME_DIR"));
        for (mode, path) in [
            (Mode::Server, format!("/v1/hand-hosts/{ID}/{ID}/hands")),
            (
                Mode::Desktop,
                format!("/v1/vm-host-attachments/{}/{ID}/hands", "x".repeat(43)),
            ),
        ] {
            let (host, environment) =
                command(directory.path(), format!("https://managed.example{path}"))
                    .prepare(mode)
                    .unwrap();
            assert!(
                environment
                    .iter()
                    .any(|(key, value)| key == "XDG_RUNTIME_DIR"
                        && Path::new(value) == host.runtime.path())
            );
            assert_eq!(host.target.endpoint().path(), path);
        }
        assert!(
            command(directory.path(), "https://managed.example".into())
                .prepare(Mode::Server)
                .is_err()
        );
        assert!(
            command(
                directory.path(),
                format!("https://managed.example/v1/hand-hosts/{ID}/{ID}/hands")
            )
            .prepare(Mode::Desktop)
            .is_err()
        );
    }
    #[test]
    fn invalid_network_and_display_configuration_fail_before_starting_processes() {
        let directory = tempfile::tempdir().unwrap();
        for (low, high) in [(0, 10), (100, 0), (200, 100)] {
            let mut args = command(directory.path(), "https://managed.example".into());
            args.udp_port_min = low;
            args.udp_port_max = high;
            assert!(args.prepare(Mode::Wayland).is_err());
        }
        let mut args = command(directory.path(), "https://managed.example".into());
        args.width = 0;
        assert!(args.prepare(Mode::Wayland).is_err());
        let mut args = command(directory.path(), "https://managed.example".into());
        args.workspace = "relative".into();
        assert!(args.prepare(Mode::Wayland).is_err());
    }
    #[test]
    fn old_publisher_cleanup_does_not_delete_replacement_readiness() {
        let directory = tempfile::tempdir().unwrap();
        let credential = directory.path().join("credential");
        let mut first = Markers::new(&credential);
        first.write(false, b"ready\n").unwrap();
        first.write(true, b"published\n").unwrap();
        let mut second = Markers::new(&credential);
        second.write(true, b"published\n").unwrap();
        let ready = first.ready.clone();
        let status = first.status.clone();
        drop(first);
        assert!(!ready.exists());
        assert_eq!(std::fs::read(&status).unwrap(), b"published\n");
        assert_eq!(
            std::fs::metadata(&status).unwrap().permissions().mode() & 0o077,
            0
        );
        drop(second);
        assert!(!status.exists());
    }
    #[tokio::test]
    async fn shutdown_reaps_the_owned_compositor_process_group() {
        let compositor = Command::new("sh")
            .args(["-c", "sleep 60 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .unwrap();
        let group = compositor.id().unwrap() as i32;
        let desktop = Desktop {
            compositor,
            group,
            playback: None,
        };
        tokio::time::timeout(Duration::from_secs(3), desktop.stop())
            .await
            .expect("owned process must stop promptly");
    }
}
