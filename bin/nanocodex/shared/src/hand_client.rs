//! Client side of the one account Hand per computer: the CLI's in-process IPC
//! lease on the OS-owned publisher, the shared account/state directory layout,
//! and service (re)start. The publisher itself lives in nanocodex-hand-daemon.
use nanocodex_managed::{ManagedClient, ManagedError};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub mod account;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub mod service_start;
pub mod transport;

/// Owns the observer while the interface and agent connect independently.
/// Dropping an unfinished start cancels it and releases only its local IPC lease.
pub struct BackgroundHandTask {
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<Option<String>>,
}
impl BackgroundHandTask {
    pub fn start(client: ManagedClient) -> Self {
        Self::start_with(async move { BackgroundHand::start(&client).await })
    }

    fn start_with(
        start: impl std::future::Future<Output = Result<BackgroundHand, ManagedError>> + Send + 'static,
    ) -> Self {
        let cancel = CancellationToken::new();
        let stopping = cancel.clone();
        let task = tokio::spawn(async move {
            let result = {
                let _timing = crate::startup_timing::Stage::new("hand_observer");
                tokio::select! {
                    biased;
                    () = stopping.cancelled() => return None,
                    result = start => result,
                }
            };
            match result {
                Ok(mut device) => {
                    stopping.cancelled().await;
                    device.stop().await;
                    None
                }
                Err(error) => Some(error.to_string()),
            }
        });
        Self { cancel, task }
    }

    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Ok(Some(error)) = (&mut self.task).await {
            // Report after terminal restoration, not over an active TUI frame.
            eprintln!("Warning: local computer Hand unavailable: {error}");
        }
    }
}
impl Drop for BackgroundHandTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

/// Describe the existing computer without starting a publisher or performing
/// discovery on the admission path. A missing identity remains unknown until
/// enrollment publishes it; a terminal must never invent a replacement Hand.
pub fn with_client_context(
    client: ManagedClient,
    workspace: &Path,
) -> Result<ManagedClient, ManagedError> {
    let machine = if std::env::var_os("NANOCODEX_DISABLE_HAND").is_some_and(|v| v == "1") {
        None
    } else {
        let target = client.account_attachment_target()?;
        let mut origin = target.endpoint().clone();
        origin
            .set_scheme(if target.endpoint().scheme() == "wss" {
                "https"
            } else {
                "http"
            })
            .map_err(|()| error("invalid origin"))?;
        origin.set_path("");
        cached_directory(origin.as_str(), target.bearer()).and_then(|directory| {
            let value: Value =
                serde_json::from_slice(&fs::read(directory.join("identity.json")).ok()?).ok()?;
            let id = uuid::Uuid::parse_str(value["machine_id"].as_str()?).ok()?;
            (id.get_version_num() == 4).then(|| id.to_string())
        })
    };
    let hand = machine.as_ref().map(|id| format!("user:{id}"));
    let cwd = machine.as_ref().map(|id| format!("/{id}"));
    let client = client.with_request_origin("nanocodex2", hand.as_deref(), cwd.as_deref())?;
    // JSON turn context cannot carry a non-UTF-8 path exactly, and a lossy
    // path would describe (and route commands to) a different directory. Omit
    // the descriptive hint; the session itself stays usable.
    let Some(workspace) = workspace.to_str() else {
        tracing::warn!(
            workspace = %workspace.display(),
            "native working directory is not UTF-8; omitting it from turn context"
        );
        return Ok(client);
    };
    client.with_native_cwd(workspace)
}

pub struct BackgroundHand {
    lease: Option<transport::Client>,
}
impl BackgroundHand {
    pub async fn start(client: &ManagedClient) -> Result<Self, ManagedError> {
        if std::env::var_os("NANOCODEX_DISABLE_HAND").is_some_and(|v| v == "1") {
            return Ok(Self { lease: None });
        }
        let target = client.account_attachment_target()?;
        let mut origin = target.endpoint().clone();
        origin
            .set_scheme(if target.endpoint().scheme() == "wss" {
                "https"
            } else {
                "http"
            })
            .map_err(|()| error("invalid origin"))?;
        origin.set_path("");
        // Resolve the authenticated account before enrolling a missing service.
        let directory = directory(origin.as_str(), target.bearer()).await?;
        // A healthy publisher already owns the cloud connection. Reuse its IPC
        // lease directly; launchctl/systemctl are only recovery paths.
        if let Some(device) = Self::observe_existing(&directory).await? {
            return Ok(device);
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        ensure_service().await?;
        Self::observe(&directory)
            .await
            .map_err(|e| error(e.to_string().replace(target.bearer(), "[redacted]")))
    }

    async fn observe_existing(directory: &Path) -> Result<Option<Self>, ManagedError> {
        let socket = socket_path(directory)?;
        let Ok(lease) = transport::connect(&socket).await else {
            return Ok(None);
        };
        let Ok(status) = fs::read_to_string(directory.join("status.json")) else {
            return Ok(None);
        };
        observer_ready(&status)?;
        Ok(Some(Self { lease: Some(lease) }))
    }

    async fn observe(directory: &Path) -> Result<Self, ManagedError> {
        // The CLI can own the same private IPC lease as the standalone helper.
        // No second CLI process, credential read, HTTP pool or status pipe is
        // needed merely to keep the OS-owned publisher visible to this client.
        let socket = socket_path(directory)?;
        tokio::time::timeout(Duration::from_secs(30), async {
            let lease = loop {
                match transport::connect(&socket).await {
                    Ok(stream) => break stream,
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            };
            loop {
                if let Ok(status) = fs::read_to_string(directory.join("status.json")) {
                    observer_ready(&status)?;
                    return Ok(Self { lease: Some(lease) });
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.map_err(|_| error("Timed out connecting to the computer Hand OS service. Check its logs and saved login; the CLI and service must use the same account."))?
    }

    pub async fn stop(&mut self) {
        let _timing = crate::startup_timing::Stage::new("hand_observer_stop");
        drop(self.lease.take());
    }
}

pub fn observer_ready(line: &str) -> Result<(), ManagedError> {
    let status: Value = serde_json::from_str(line).map_err(error)?;
    if status["status"] == "error" {
        return Err(error(
            status["error"]
                .as_str()
                .unwrap_or("Computer Hand connection failed"),
        ));
    }
    if status.get("machine").is_none() {
        return Err(error(
            "The computer Hand observer returned an invalid readiness status",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub async fn ensure_service() -> Result<(), ManagedError> {
    use service_start::{Platform, Reply};
    let platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else {
        Platform::Linux
    };
    #[cfg(target_os = "macos")]
    let gui = Some((
        nix::unistd::geteuid().as_raw(),
        home()?.join("Library/LaunchAgents/com.nanocodex.hand.plist"),
    ));
    #[cfg(not(target_os = "macos"))]
    let gui: Option<(u32, PathBuf)> = None;
    let gui_context = gui.as_ref().map(|(uid, path)| (*uid, path.as_path()));
    service_start::ensure_with(
        platform,
        Path::new("/Library/LaunchDaemons/com.nanocodex.hand.plist").is_file(),
        gui.as_ref().map(|(_, path)| path.is_file()),
        |action| async move {
            if action == service_start::Action::MacInstall {
                return install_user_service().await;
            }
            let (program, args) = action.command(gui_context);
            // Service managers need no account credentials or interactive input.
            let output = tokio::time::timeout(
                Duration::from_secs(10),
                Command::new(program)
                    .args(args)
                    .env_clear()
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| "service manager timed out".to_owned())?
            .map_err(|_| format!("cannot execute {program}"))?;
            Ok(Reply {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            })
        },
    )
    .await
    .map_err(error)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub async fn install_user_service() -> Result<service_start::Reply, String> {
    // Use the companion of this exact release, never a PATH-selected installer.
    // The installer locks/rechecks ownership and requires a matching saved login.
    let account =
        nanocodex_cli_auth::saved_enrollment_account_file().map_err(|error| error.to_string())?;
    // The Hand executable is this process when the daemon runs it, otherwise
    // the installed Hand beside the CLI; the CLI owns service installation.
    let (binary, installer) = if crate::hand_executable::is_hand_role() {
        (
            std::env::current_exe().map_err(|error| error.to_string())?,
            crate::hand_executable::cli_binary().map_err(|error| error.to_string())?,
        )
    } else {
        (
            crate::hand_executable::hand_binary().map_err(|error| error.to_string())?,
            std::env::current_exe().map_err(|error| error.to_string())?,
        )
    };
    let status = Command::new(&installer)
        .args(["hand", "install", "--if-missing", "--executable"])
        .arg(&binary)
        .env("NANOCODEX_ACCOUNT_FILE", account)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Cancellation of a client must not interrupt the installer's service
        // transaction. It owns its lock and rollback until it exits.
        .kill_on_drop(false)
        .status()
        .await
        .map_err(|_| {
            "Cannot start the companion installer. Run nanocodex setup to connect this computer."
                .to_owned()
        })?;
    if !status.success() {
        return Err("Automatic Hand installation did not complete. Run nanocodex setup to sign in and connect this computer.".into());
    }
    Ok(service_start::Reply {
        success: true,
        stdout: String::new(),
    })
}

pub fn error(value: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(value.to_string())
}
pub fn home() -> Result<PathBuf, ManagedError> {
    // A system installation retains state independently of the login user's
    // HOME. Only its owning OS user may reuse that private daemon and IPC.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let root = fs::symlink_metadata("/opt/nanocodex");
        let record = fs::symlink_metadata("/opt/nanocodex/installation.json");
        let state = fs::symlink_metadata("/srv/nanocodex");
        if let (Ok(root), Ok(record), Ok(state)) = (root, record, state)
            && root.is_dir()
            && root.uid() == 0
            && root.mode() & 0o022 == 0
            && record.is_file()
            && record.uid() == 0
            && record.mode() & 0o022 == 0
            && state.is_dir()
            && state.uid() == nix::unistd::geteuid().as_raw()
        {
            return Ok(PathBuf::from("/srv/nanocodex"));
        }
    }
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| error("A user home directory is required for the device Hand"))
}
pub fn digest(value: &str) -> String {
    Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn private_directory(path: &Path) -> Result<(), ManagedError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(error)?;
    let metadata = fs::symlink_metadata(path).map_err(error)?;
    if !metadata.is_dir() {
        return Err(error("Hand state must be a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(error(
                "The computer Hand state directory must be private (0700)",
            ));
        }
    }
    Ok(())
}
pub fn log_file(directory: &Path, name: &str) -> Result<fs::File, ManagedError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(error)?;
    let metadata = fs::symlink_metadata(directory).map_err(error)?;
    if !metadata.is_dir() {
        return Err(error("Hand logs must use a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The installer owns a 0755 log directory; its private files can be shared.
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(error(
                "The Hand log directory must not be writable by other users",
            ));
        }
    }
    let path = directory.join(name);
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && !metadata.is_file()
    {
        return Err(error("Hand logs must be regular files"));
    }
    let mut options = OpenOptions::new();
    // Windows file locking requires read or write access beyond append-only.
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(error)?;
    if !file.metadata().map_err(error)?.is_file() {
        return Err(error("Hand logs must be regular files"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(error)?;
    }
    Ok(file)
}
pub fn valid_owner(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}

pub fn cached_directory(origin: &str, key: &str) -> Option<PathBuf> {
    let origin = origin.trim_end_matches('/');
    let home = home().ok()?;
    let cache = home
        .join(".nanocodex/hand-accounts")
        .join(digest(&format!("{origin}\0{key}")));
    let owner = fs::read_to_string(cache).ok()?;
    valid_owner(&owner).then(|| {
        home.join(".nanocodex/hands")
            .join(digest(&format!("{origin}\0{owner}")))
    })
}

pub async fn directory(origin: &str, key: &str) -> Result<PathBuf, ManagedError> {
    // Credentials rotate and the desktop and CLI may use different keys. Cache
    // their authenticated account identity so they still share one computer.
    // This happens in the background helper, never on the prompt path.
    let origin = origin.trim_end_matches('/');
    let accounts = home()?.join(".nanocodex/hand-accounts");
    private_directory(&accounts)?;
    let cache = accounts.join(digest(&format!("{origin}\0{key}")));
    let owner = match fs::read_to_string(&cache) {
        Ok(owner) if valid_owner(&owner) => owner,
        _ => {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(error)?;
            let body = account::identify(&client, origin, key).await?;
            let owner = body["user"]["id"]
                .as_str()
                .filter(|id| valid_owner(id))
                .ok_or_else(|| error("Invalid Hand account identity"))?
                .to_owned();
            let mut file = tempfile::NamedTempFile::new_in(&accounts).map_err(error)?;
            file.write_all(owner.as_bytes()).map_err(error)?;
            file.persist(&cache).map_err(error)?;
            owner
        }
    };
    Ok(home()?
        .join(".nanocodex/hands")
        .join(digest(&format!("{origin}\0{owner}"))))
}

pub fn socket_path(directory: &Path) -> Result<PathBuf, ManagedError> {
    #[cfg(unix)]
    {
        unix_socket_path(home()?.join(".nanocodex/s"), directory)
    }
    #[cfg(windows)]
    {
        // Profile path prevents different OS users with the same account from
        // sharing a pipe. The account identity still scopes the state itself.
        Ok(PathBuf::from(format!(
            r"\\.\pipe\nanocodex-hand-{}",
            digest(&directory.to_string_lossy())
        )))
    }
}
#[cfg(unix)]
pub fn unix_socket_path(base: PathBuf, directory: &Path) -> Result<PathBuf, ManagedError> {
    use std::os::unix::ffi::OsStrExt;
    let scope = directory.file_name().unwrap().to_string_lossy();
    let path = base.join(format!("{}.sock", &scope[..24]));
    let path = if path.as_os_str().as_bytes().len() < 104 {
        path
    } else {
        // sockaddr_un is only 104 bytes on macOS. Long home directories and
        // test profiles still need a private, stable per-user IPC endpoint.
        PathBuf::from(format!("/tmp/nanocodex-{}", nix::unistd::geteuid())).join(format!(
            "{}.sock",
            &digest(&directory.to_string_lossy())[..24]
        ))
    };
    private_directory(path.parent().unwrap())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt as _;

    use super::*;
    #[tokio::test]
    async fn pending_observer_does_not_block_client_or_close() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let owned = Dropped(dropped.clone());
        let observer = BackgroundHandTask::start_with(async move {
            let _owned = owned;
            std::future::pending::<Result<BackgroundHand, ManagedError>>().await
        });
        tokio::task::yield_now().await;
        // The interface can run while observer readiness is still pending.
        assert!(!dropped.load(Ordering::SeqCst));
        tokio::time::timeout(Duration::from_secs(1), observer.stop())
            .await
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn local_observer_releases_only_its_lease_on_close() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(digest(&root.path().to_string_lossy()));
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("status.json"),
            r#"{"status":"connected","machine":{"id":"fixture"}}"#,
        )
        .unwrap();
        let socket = socket_path(&directory).unwrap();
        let mut listener = transport::Listener::bind(&socket).unwrap();
        let observer_path = directory.clone();
        let task =
            tokio::spawn(async move { BackgroundHand::observe(&observer_path).await.unwrap() });
        let mut accepted = listener.accept().await.unwrap();
        let mut observer = task.await.unwrap();
        observer.stop().await;
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), accepted.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        // Closing a client did not close the publisher listener.
        let second_path = socket.clone();
        let second = tokio::spawn(async move { transport::connect(&second_path).await.unwrap() });
        let _accepted = listener.accept().await.unwrap();
        let _second = second.await.unwrap();
    }

    #[test]
    fn observer_failure_reaches_cli_startup() {
        assert!(observer_ready(r#"{"status":"connecting","machine":{"id":"test"}}"#).is_ok());
        let failure =
            observer_ready(r#"{"status":"error","error":"service did not accept a connection"}"#)
                .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("service did not accept a connection")
        );
        assert!(observer_ready(r#"{"status":"connecting"}"#).is_err());
        assert!(observer_ready("not JSON").is_err());
    }
    #[test]
    #[cfg(unix)]
    fn long_home_directory_uses_a_private_short_socket_path() {
        let directory = PathBuf::from("/a/very/long/home").join("a".repeat(64));
        let path =
            unix_socket_path(PathBuf::from("/".to_owned() + &"a".repeat(110)), &directory).unwrap();
        assert!(path.as_os_str().len() < 104);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
