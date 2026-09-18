//! Host-local desired state, independent of accounts, credentials and checkouts.
//! Polling deliberately also covers detached publishers and clients started by
//! another application. No process discovery or PID-based signalling is used.
use clap::{Args, Subcommand};
use nanocodex_managed::ManagedError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Subcommand)]
pub(crate) enum Action {
    /// Disable automatic local Hands in all sessions for this OS user and host.
    StopAll,
    /// Re-enable automatic local Hands, including existing waiting sessions.
    StartAll,
    /// Show the persisted local policy (not a remote account inventory).
    Status,
}
#[derive(Args)]
pub(crate) struct Command {
    #[command(subcommand)]
    pub(crate) action: Action,
}
impl Action {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "stop-all" => Ok(Self::StopAll),
            "start-all" => Ok(Self::StartAll),
            "status" | "" => Ok(Self::Status),
            _ => Err("Usage: /hand [stop-all|start-all|status] (this OS user, this host)".into()),
        }
    }
    pub(crate) fn run(self) -> Result<String, ManagedError> {
        let gate = Gate::local()?;
        let state = match self {
            Self::StopAll => gate.set(false)?,
            Self::StartAll => gate.set(true)?,
            Self::Status => gate.read()?,
        };
        Ok(format!(
            "Local automatic Hands: {} for this OS user on this host. {} State: {}. Requires control-aware clients; older clients must be restarted after upgrading.",
            if state.enabled { "enabled" } else { "disabled" },
            if state.enabled {
                "Existing sessions reconnect automatically; NANOCODEX_DISABLE_HAND=1 still opts out."
            } else {
                "Stop requested: publishers poll every 250ms, then drain owned helpers. CLI sessions stay open; use /hand start-all to resume."
            },
            gate.directory.display(),
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct State {
    version: u32,
    enabled: bool,
    // Preserved on start: even stop/start between polls must drain the old
    // publisher before a replacement may acquire the existing singleton lock.
    stop_id: uuid::Uuid,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            enabled: true,
            stop_id: uuid::Uuid::nil(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Gate {
    directory: PathBuf,
}
fn error(value: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(format!("Local Hand control: {value}"))
}
#[cfg(target_os = "macos")]
fn host_identity() -> Result<String, ManagedError> {
    let output = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .map_err(error)?;
    if !output.status.success() {
        return Err(error("cannot read local host identity"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            if key.trim() != "\"IOPlatformUUID\"" {
                return None;
            }
            uuid::Uuid::parse_str(value.trim().trim_matches('"'))
                .ok()
                .map(|id| id.to_string())
        })
        .ok_or_else(|| error("missing local host identity"))
}
#[cfg(target_os = "linux")]
fn host_identity() -> Result<String, ManagedError> {
    fs::read_to_string("/etc/machine-id")
        .or_else(|_| fs::read_to_string("/var/lib/dbus/machine-id"))
        .map(|id| id.trim().to_owned())
        .map_err(error)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn host_identity() -> Result<String, ManagedError> {
    whoami::fallible::hostname().map_err(error)
}

impl Gate {
    pub(crate) fn local() -> Result<Self, ManagedError> {
        // A hardware/machine identity keeps the scope stable when the Mac's
        // display name or DHCP hostname changes. Nothing is keyed by checkout,
        // account credential, CLI PID or managed conversation.
        let host = host_identity()?;
        let digest = Sha256::digest(host.as_bytes());
        let scope = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Self::at(
            super::device_hand::home()?
                .join(".nanocodex/host-control")
                .join(scope),
        )
    }
    fn at(directory: PathBuf) -> Result<Self, ManagedError> {
        super::device_hand::private_directory(&directory)?;
        Ok(Self { directory })
    }
    fn read(&self) -> Result<State, ManagedError> {
        let path = self.directory.join("state.json");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(error("state must be a regular file")),
            Err(e) => return Err(error(e)),
        }
        let state: State =
            serde_json::from_slice(&fs::read(path).map_err(error)?).map_err(error)?;
        if state.version != 1 {
            return Err(error("unsupported state version"));
        }
        Ok(state)
    }
    fn set(&self, enabled: bool) -> Result<State, ManagedError> {
        let lock = super::device_hand::log_file(&self.directory, "control.lock")?;
        lock.lock().map_err(error)?;
        let result = (|| {
            let mut state = self.read()?;
            state.enabled = enabled;
            if !enabled {
                state.stop_id = uuid::Uuid::new_v4();
            }
            let mut file = tempfile::NamedTempFile::new_in(&self.directory).map_err(error)?;
            serde_json::to_writer(&mut file, &state).map_err(error)?;
            file.write_all(b"\n").map_err(error)?;
            file.as_file().sync_all().map_err(error)?;
            file.persist(self.directory.join("state.json"))
                .map_err(error)?;
            Ok(state)
        })();
        let _ = lock.unlock();
        result
    }
    pub(crate) fn ticket(&self) -> Result<Option<State>, ManagedError> {
        let state = self.read()?;
        Ok(state.enabled.then_some(state))
    }
    pub(crate) fn permits(&self, ticket: &State) -> bool {
        self.read()
            .is_ok_and(|state| state.enabled && state.stop_id == ticket.stop_id)
    }
    pub(crate) async fn wait_enabled(&self, cancel: &CancellationToken) -> Option<State> {
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            // Errors fail closed, but existing clients stay alive so correcting
            // state or permissions permits recovery without restarting them.
            if let Ok(state) = self.read()
                && state.enabled
            {
                return Some(state);
            }
            tokio::select! {
                () = cancel.cancelled() => return None,
                () = tokio::time::sleep(POLL) => {},
            }
        }
    }
    /// Keep cancellation cooperative so the owner can drain and reap its own
    /// children before releasing its singleton lock.
    pub(crate) async fn supervise<T>(
        &self,
        ticket: &State,
        cancel: &CancellationToken,
        run: impl std::future::Future<Output = T>,
    ) -> T {
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => result,
            () = self.watch(ticket, cancel) => run.await,
        }
    }
    pub(crate) async fn watch(&self, ticket: &State, cancel: &CancellationToken) {
        loop {
            if !self.permits(ticket) {
                cancel.cancel();
                return;
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(POLL) => {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_start_between_polls_invalidates_existing_publishers() {
        let root = tempfile::tempdir().unwrap();
        let first = Gate::at(root.path().join("host-a")).unwrap();
        let other_process = Gate::at(root.path().join("host-a")).unwrap();
        let other_host = Gate::at(root.path().join("host-b")).unwrap();
        let old = first.ticket().unwrap().unwrap();
        other_process.set(false).unwrap();
        other_process.set(true).unwrap();
        assert!(!first.permits(&old));
        assert!(first.ticket().unwrap().is_some());
        assert!(other_host.permits(&old));
        let new = first.ticket().unwrap().unwrap();
        other_process.set(true).unwrap();
        assert!(
            first.permits(&new),
            "repeated start must not interrupt work"
        );
    }

    #[tokio::test]
    async fn malformed_state_fails_closed_and_waiters_recover() {
        let root = tempfile::tempdir().unwrap();
        let gate = Gate::at(root.path().join("control")).unwrap();
        let ticket = gate.ticket().unwrap().unwrap();
        fs::write(gate.directory.join("state.json"), b"{broken").unwrap();
        assert!(!gate.permits(&ticket));
        let cancel = CancellationToken::new();
        tokio::time::timeout(Duration::from_secs(1), gate.watch(&ticket, &cancel))
            .await
            .unwrap();
        assert!(cancel.is_cancelled());
        let parent = CancellationToken::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), gate.wait_enabled(&parent))
                .await
                .is_err()
        );
        fs::remove_file(gate.directory.join("state.json")).unwrap();
        assert!(gate.wait_enabled(&parent).await.is_some());
        gate.set(false).unwrap();
        parent.cancel();
        assert!(gate.wait_enabled(&parent).await.is_none());
    }

    #[test]
    fn concurrent_writers_publish_complete_state_and_keep_it_private() {
        let root = tempfile::tempdir().unwrap();
        let gate = Gate::at(root.path().join("control")).unwrap();
        std::thread::scope(|scope| {
            for enabled in [true, false, true, false] {
                let gate = &gate;
                scope.spawn(move || {
                    for _ in 0..30 {
                        gate.set(enabled).unwrap();
                        assert_eq!(gate.read().unwrap().version, 1);
                    }
                });
            }
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(gate.directory.join("state.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn unsafe_control_paths_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        let gate = Gate::at(real.clone()).unwrap();
        let link = root.path().join("linked");
        symlink(&real, &link).unwrap();
        assert!(Gate::at(link).is_err());
        let target = root.path().join("unrelated");
        fs::write(&target, b"keep me").unwrap();
        symlink(&target, real.join("state.json")).unwrap();
        assert!(gate.set(false).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep me");
        fs::remove_file(real.join("state.json")).unwrap();
        fs::remove_file(real.join("control.lock")).unwrap();
        symlink(&target, real.join("control.lock")).unwrap();
        assert!(gate.set(false).is_err());
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Gate::at(real).is_err());
    }

    // Executed by the test below in independent processes. Paths are passed
    // only to this fixture: production state cannot be redirected by a prompt.
    #[tokio::test]
    #[cfg(unix)]
    async fn process_peer() {
        let Some(root) = std::env::var_os("NCX_TEST_HAND_GATE") else {
            return;
        };
        let name = std::env::var("NCX_TEST_HAND_PEER").unwrap();
        let root = PathBuf::from(root);
        let gate = Gate::at(root.join("control")).unwrap();
        let parent = CancellationToken::new();
        fs::write(root.join(format!("{name}-waiting")), b"ready").unwrap();
        for generation in 0..2 {
            let ticket = gate.wait_enabled(&parent).await.unwrap();
            let mut owned = tokio::process::Command::new("/bin/sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            fs::write(
                root.join(format!("{name}-active-{generation}")),
                owned.id().unwrap().to_string(),
            )
            .unwrap();
            let active = parent.child_token();
            gate.supervise(&ticket, &active, async {
                active.cancelled().await;
                owned.kill().await.unwrap();
                owned.wait().await.unwrap();
            })
            .await;
            assert!(
                !parent.is_cancelled(),
                "stopping hands must preserve the CLI session"
            );
            fs::write(root.join(format!("{name}-stopped-{generation}")), b"reaped").unwrap();
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn independent_sessions_stop_owned_children_and_resume_without_respawning_while_disabled()
    {
        let root = tempfile::tempdir().unwrap();
        let gate = Gate::at(root.path().join("control")).unwrap();
        gate.set(false).unwrap();
        let mut unrelated = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut peers = Vec::new();
        for name in ["one", "two"] {
            peers.push(
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "hand_control::tests::process_peer",
                        "--nocapture",
                    ])
                    .env("NCX_TEST_HAND_GATE", root.path())
                    .env("NCX_TEST_HAND_PEER", name)
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap(),
            );
        }
        let wait_for = |suffix: String| {
            let path = root.path().to_owned();
            async move {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while !["one", "two"]
                        .iter()
                        .all(|name| path.join(format!("{name}-{suffix}")).exists())
                    {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                .unwrap();
            }
        };
        wait_for("waiting".into()).await;
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!root.path().join("one-active-0").exists());
        for generation in 0..2 {
            Gate::at(root.path().join("control"))
                .unwrap()
                .set(true)
                .unwrap();
            wait_for(format!("active-{generation}")).await;
            gate.set(false).unwrap();
            wait_for(format!("stopped-{generation}")).await;
            assert!(unrelated.try_wait().unwrap().is_none());
            if generation == 0 {
                tokio::time::sleep(Duration::from_millis(750)).await;
                assert!(!root.path().join("one-active-1").exists());
                assert!(!root.path().join("two-active-1").exists());
                for peer in &mut peers {
                    assert!(peer.try_wait().unwrap().is_none());
                }
            }
        }
        for peer in &mut peers {
            assert!(
                tokio::time::timeout(Duration::from_secs(5), peer.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
        }
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().await.unwrap();
        unrelated.wait().await.unwrap();
    }
}
