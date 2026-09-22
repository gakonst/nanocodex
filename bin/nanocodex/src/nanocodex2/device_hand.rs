//! One account Hand per computer, shared by the terminal and desktop clients.
//! Each client holds a local IPC lease through a child process. A single
//! publisher is owned by the OS service and survives all client disconnects.
use clap::Args;
use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_tools::attachment::AttachmentEvent;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

use super::native_hand::NativeState;

mod account;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
mod service_start;
mod transport;

#[derive(Args, Default)]
pub(crate) struct DeviceHand {
    /// Report service update compatibility without reading account credentials.
    #[arg(long, hide = true, conflicts_with_all = ["describe", "daemon", "parent_pipe", "prepare_update"])]
    service_protocol: bool,
    /// Request an authoritative idle barrier from the currently running daemon.
    #[arg(long, hide = true, conflicts_with_all = ["describe", "daemon", "parent_pipe"])]
    prepare_update: bool,
    /// Print the shared identity without publishing a Hand.
    #[arg(long)]
    describe: bool,
    #[arg(long, hide = true)]
    pub(super) daemon: bool,
    /// Exit when the owning application closes stdin.
    #[arg(long)]
    parent_pipe: bool,
}

pub(crate) struct BackgroundHand {
    child: Option<Child>,
}
impl BackgroundHand {
    pub(crate) async fn start(client: &ManagedClient) -> Result<Self, ManagedError> {
        if std::env::var_os("NANOCODEX_DISABLE_HAND").is_some_and(|v| v == "1") {
            return Ok(Self { child: None });
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
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        ensure_service().await?;
        let mut command = Command::new(std::env::current_exe().map_err(error)?);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command
            .args(["__device-hand", "--parent-pipe"])
            .env("NANOCODEX_API_KEY", target.bearer())
            .env("NANOCODEX_MANAGED_URL", origin.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log_file(&home()?.join(".nanocodex/logs"), "hand.log")?)
            .kill_on_drop(true)
            .spawn()
            .map_err(error)?;
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let ready = tokio::time::timeout(Duration::from_secs(30), async {
            let line = lines.next_line().await.map_err(error)?.ok_or_else(|| {
                error("The computer Hand observer exited before connecting. Check ~/.nanocodex/logs/hand.log and the service owner's saved login; the CLI and service must use the same account. Set NANOCODEX_DISABLE_HAND=1 to continue without a local Hand.")
            })?;
            observer_ready(&line)
        }).await;
        match ready {
            Ok(Ok(())) => {}
            result => {
                let _ = child.kill().await;
                return Err(match result {
                    Ok(Err(e)) => error(e.to_string().replace(target.bearer(), "[redacted]")),
                    _ => error(
                        "Timed out connecting to the computer Hand OS service. Check its logs and saved login; the CLI and service must use the same account. Set NANOCODEX_DISABLE_HAND=1 to continue without a local Hand.",
                    ),
                });
            }
        }
        // Keep the observer's status pipe drained for its entire lifetime.
        tokio::spawn(async move { while matches!(lines.next_line().await, Ok(Some(_))) {} });
        Ok(Self { child: Some(child) })
    }
    pub(crate) async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            if tokio::time::timeout(Duration::from_secs(25), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
    }
}

fn observer_ready(line: &str) -> Result<(), ManagedError> {
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
async fn ensure_service() -> Result<(), ManagedError> {
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

fn error(value: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(value.to_string())
}
fn home() -> Result<PathBuf, ManagedError> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| error("A user home directory is required for the device Hand"))
}
fn digest(value: &str) -> String {
    Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn private_directory(path: &Path) -> Result<(), ManagedError> {
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
fn log_file(directory: &Path, name: &str) -> Result<fs::File, ManagedError> {
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
async fn directory(origin: &str, key: &str) -> Result<PathBuf, ManagedError> {
    // Credentials rotate and the desktop and CLI may use different keys. Cache
    // their authenticated account identity so they still share one computer.
    // This happens in the background helper, never on the prompt path.
    let origin = origin.trim_end_matches('/');
    let accounts = home()?.join(".nanocodex/hand-accounts");
    private_directory(&accounts)?;
    let cache = accounts.join(digest(&format!("{origin}\0{key}")));
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
    };
    let owner = match fs::read_to_string(&cache) {
        Ok(owner) if valid(&owner) => owner,
        _ => {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(error)?;
            let body = account::identify(&client, origin, key).await?;
            let owner = body["user"]["id"]
                .as_str()
                .filter(|id| valid(id))
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
fn open(directory: &Path) -> Result<NativeState, ManagedError> {
    let workspace = home()?.join("Nanocodex");
    fs::create_dir_all(&workspace).map_err(error)?;
    NativeState::open(
        &workspace,
        directory,
        super::host::bounded_display_name(whoami::devicename()),
    )
}
fn identity(directory: &Path) -> Result<Value, ManagedError> {
    // Creation is serialized by NativeState's OS lock. Reading the published
    // identity never needs that lock and cannot steal a live attachment.
    let path = directory.join("identity.json");
    if !path.exists() {
        drop(open(directory)?);
    }
    let value: Value = serde_json::from_slice(&fs::read(path).map_err(error)?).map_err(error)?;
    Ok(
        json!({"id": value["machine_id"], "name": super::host::bounded_display_name(whoami::devicename()), "workspace": value["workspace"], "kind": "local"}),
    )
}
fn publish(directory: &Path, value: &Value) -> Result<(), ManagedError> {
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(error)?;
    serde_json::to_writer(&mut file, value).map_err(error)?;
    file.write_all(b"\n").map_err(error)?;
    file.persist(directory.join("status.json")).map_err(error)?;
    Ok(())
}
fn emit(value: &Value) {
    let _ = writeln!(std::io::stdout().lock(), "{value}");
}

pub(crate) async fn serve(command: DeviceHand) -> Result<(), ManagedError> {
    if command.service_protocol {
        emit(&json!({"serviceProtocol": 1, "version": env!("CARGO_PKG_VERSION")}));
        return Ok(());
    }
    if command.prepare_update {
        let prepared = prepare_idle_update().await?;
        emit(&json!({"prepared": prepared}));
        return if prepared {
            Ok(())
        } else {
            Err(error(
                "Computer Hand update deferred: idleness is not established",
            ))
        };
    }
    let daemon = command.daemon;
    match serve_inner(command).await {
        Err(error)
            if daemon
                && matches!(&error, ManagedError::Http { status, .. } if matches!(status.as_u16(), 401 | 403)) =>
        {
            tracing::error!(%error, "Computer Hand stopped; update account access and restart the service");
            Ok(()) // A normal exit prevents the OS service from retrying rejected credentials.
        }
        result => result,
    }
}

/// Requests the running publisher's barrier. Missing/old daemons and timeouts
/// are errors, never evidence that replacing a running service is safe.
pub(crate) async fn prepare_idle_update() -> Result<bool, ManagedError> {
    let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
    let _client = super::client_from_environment(None)?;
    let directory = directory(&origin, &key).await?;
    transport::prepare_idle_update(&socket_path(&directory)?)
        .await
        .map_err(error)
}

async fn serve_inner(command: DeviceHand) -> Result<(), ManagedError> {
    let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
    // The managed client installs the shared TLS provider before any identity HTTP request.
    let client = super::client_from_environment(None)?;
    let directory = directory(&origin, &key).await?;
    if command.describe {
        // Another client can be publishing the initial identity at this instant.
        for _ in 0..20 {
            match identity(&directory) {
                Ok(value) => {
                    emit(&value);
                    return Ok(());
                }
                Err(e) if e.to_string().contains("another native Hand") => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(e) => return Err(e),
            }
        }
        return Err(error("The computer Hand is still preparing its identity"));
    }
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let parent_pipe = command.parent_pipe;
    let watcher = tokio::spawn(async move {
        let eof = async {
            if !parent_pipe {
                std::future::pending::<()>().await;
            }
            let mut stdin = tokio::io::stdin();
            let mut bytes = [0_u8; 64];
            while matches!(stdin.read(&mut bytes).await, Ok(n) if n > 0) {}
        };
        tokio::select! { _ = super::service::shutdown_signal() => {}, () = eof => {} }
        shutdown.cancel();
    });
    let result = if command.daemon {
        share(&client, &directory, &origin, &key, &cancel).await
    } else {
        connect(&directory, &cancel).await
    };
    if let Err(error) = &result {
        emit(&json!({"status": "error", "error": error.to_string()}));
    }
    cancel.cancel();
    watcher.abort();
    result
}

async fn share(
    client: &ManagedClient,
    directory: &Path,
    origin: &str,
    key: &str,
    cancel: &CancellationToken,
) -> Result<(), ManagedError> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    // The installed owner has one publisher across all account identities.
    let publisher = super::native_hand::NativeStateLock(log_file(
        &home()?.join(".nanocodex"),
        "hand-daemon.lock",
    )?);
    publisher
        .0
        .try_lock()
        .map_err(|_| error("another computer Hand daemon is running"))?;
    match open(directory) {
        Ok(mut state) => {
            let socket = socket_path(directory)?;
            let listener = transport::Listener::bind(&socket).map_err(error)?;
            let lease_cancel = cancel.clone();
            let leases = tokio::spawn(async move {
                watch_clients(listener, lease_cancel).await;
            });
            let machine = serde_json::to_value(&state.machine).map_err(error)?;
            let recipe = factory_recipe(directory, state.machine.id());
            let factory_error = recipe.as_ref().err().map(ToString::to_string);
            let recipe = recipe.unwrap_or(None);
            if let Some(recipe) = &recipe {
                state.advertise_vm_provider(&recipe.name)?;
            }
            let status = std::sync::Arc::new(std::sync::Mutex::new(
                json!({"machine": machine, "status": "connecting", "daemon": {"pid": std::process::id(), "executable": std::env::current_exe().ok(), "version": env!("CARGO_PKG_VERSION")}}),
            ));
            {
                let mut status = status.lock().unwrap();
                if recipe.is_none() {
                    status["factory"] = json!({"status": "unavailable", "error": factory_error.unwrap_or_else(|| "No desktop VM image is configured".into())});
                }
                publish(directory, &status)?;
            }
            let factory = recipe.map(|recipe| {
                let (directory, origin, key, cancel, status) = (
                    directory.to_owned(),
                    origin.to_owned(),
                    key.to_owned(),
                    cancel.clone(),
                    status.clone(),
                );
                tokio::spawn(async move {
                    supervise_factory(recipe, &directory, &origin, &key, &cancel, &status).await;
                })
            });
            // Capture permissions and optional VM startup must not hold up
            // publication of the native shell/filesystem catalog.
            let screen_cancel = cancel.clone();
            let screen_target = client.account_attachment_target()?;
            let screen_machine = state.machine.clone();
            let screen_directory = directory.to_owned();
            let mut screen = tokio::spawn(async move {
                loop {
                    let started = tokio::select! {
                        () = screen_cancel.cancelled() => break,
                        result = super::screen_native::NativeScreen::start(
                            &screen_target, &screen_machine, &screen_directory,
                        ) => result,
                    };
                    match started {
                        Ok(screen) => {
                            screen_cancel.cancelled().await;
                            let _ = screen.shutdown().await;
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "native screen startup failed; retrying")
                        }
                    }
                    tokio::select! {
                        () = screen_cancel.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_secs(5)) => {},
                    }
                }
            });
            let result = super::native_hand::run_observed(
                client.account_attachment_target()?,
                &state,
                async {
                    cancel.cancelled().await;
                    Ok(())
                },
                |event| {
                    let next = match event {
                        AttachmentEvent::CatalogPublished { .. } => "connected",
                        AttachmentEvent::Connecting => "connecting",
                        _ => return,
                    };
                    let mut status = status.lock().unwrap();
                    status["status"] = json!(next);
                    let _ = publish(directory, &status);
                    emit(&status);
                },
            )
            .await;
            cancel.cancel();
            let _ = leases.await;
            if let Some(factory) = factory {
                let _ = factory.await;
            }
            if tokio::time::timeout(Duration::from_secs(5), &mut screen)
                .await
                .is_err()
            {
                screen.abort();
            }
            let _ = fs::remove_file(directory.join("status.json"));
            result
        }
        Err(e) => Err(e),
    }
}

fn socket_path(directory: &Path) -> Result<PathBuf, ManagedError> {
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
fn unix_socket_path(base: PathBuf, directory: &Path) -> Result<PathBuf, ManagedError> {
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

async fn connect(directory: &Path, cancel: &CancellationToken) -> Result<(), ManagedError> {
    let socket = socket_path(directory)?;
    // A successful service-manager start can precede account lookup and IPC bind.
    let mut stream = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match transport::connect(&socket).await {
                Ok(stream) => return stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }).await.map_err(|_| error("The computer Hand OS service did not accept a connection. Check the service logs and saved login; the CLI and service must use the same account. Set NANOCODEX_DISABLE_HAND=1 to continue without a local Hand."))?;
    let mut previous = Value::Null;
    let mut bytes = [0u8; 1];
    loop {
        if let Ok(bytes) = fs::read(directory.join("status.json"))
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && previous != value
        {
            emit(&value);
            previous = value;
        }
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            _ = stream.read(&mut bytes) => return Err(error("The shared computer Hand stopped")),
            () = tokio::time::sleep(Duration::from_millis(200)) => {},
        }
    }
}
async fn watch_clients(listener: transport::Listener, cancel: CancellationToken) {
    // No safe runtime barrier exists yet: lease absence does not prove remote
    // tools, retained CUA/process sessions, or independently hosted VMs idle.
    // Keep the wire request usable by updaters but fail closed until all those
    // owners participate in the admission barrier.
    watch_clients_with_barrier(listener, cancel, || async { false }).await;
}

async fn watch_clients_with_barrier<F, Fut>(
    mut listener: transport::Listener,
    cancel: CancellationToken,
    mut prepare: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut clients = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(mut stream) => {
                    clients.spawn(async move {
                        match stream.read_u8().await {
                            Ok(transport::PREPARE_IDLE_UPDATE) => Some(stream),
                            _ => None,
                        }
                    });
                }
                Err(_) => break,
            },
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(Ok(Some(mut stream))) = completed {
                    // This loop owns lease admission. While the authoritative
                    // barrier runs it cannot admit another client. The barrier
                    // must itself atomically reject new remote/runtime work.
                    let prepared = clients.is_empty()
                        && tokio::time::timeout(Duration::from_secs(2), prepare())
                            .await.unwrap_or(false);
                    if prepared {
                        // Close local admission before acknowledging. A queued
                        // connection cannot become a lease in the old daemon.
                        drop(listener);
                        cancel.cancel();
                        let _ = stream.write_all(&[transport::UPDATE_PREPARED]).await;
                        return;
                    }
                    let _ = stream.write_all(&[transport::UPDATE_DEFERRED]).await;
                }
            },
        }
    }
    cancel.cancel();
}

struct FactoryRecipe {
    name: String,
    binary: PathBuf,
    args: Vec<String>,
}
fn factory_recipe(
    directory: &Path,
    machine_id: &str,
) -> Result<Option<FactoryRecipe>, ManagedError> {
    let data = desktop_data()?;
    let config: Value = match fs::read(data.join("vm.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(error(e)),
    };
    let path = |env: &str, field: &str| {
        std::env::var_os(env)
            .map(PathBuf::from)
            .or_else(|| config[field].as_str().map(PathBuf::from))
    };
    // Only a desktop recipe can fulfill mount's desktop namespace contract.
    let Some(root) = path("NANOCODEX_VM_DESKTOP_ROOTFS", "desktopRootfs") else {
        return Ok(None);
    };
    let Some(guest) = path("NANOCODEX_VM_GUEST_RUNTIME", "guestRuntime") else {
        return Ok(None);
    };
    let binary =
        path("NANOCODEX_HAND_BINARY", "binary").unwrap_or(std::env::current_exe().map_err(error)?);
    let wsl = config["wslDistribution"].as_str();
    if cfg!(windows) && wsl.is_none() {
        return Err(error(
            "Configure wslDistribution and Linux VM asset paths in vm.json to host VMs on Windows",
        ));
    }
    for path in [&root, &guest, &binary] {
        let valid = if cfg!(windows) {
            path.to_str()
                .is_some_and(|path| path.starts_with('/') && !path.contains('\0'))
        } else {
            path.is_absolute() && path.is_file()
        };
        if !valid {
            return Err(error(format!(
                "Hand VM asset unavailable: {}",
                path.display()
            )));
        }
    }
    // Preserve existing Mac provider identities; other platforms own their
    // native host and VM provider under the same computer identity as well.
    let platform = if cfg!(target_os = "macos") {
        "mac"
    } else {
        std::env::consts::OS
    };
    let name = config["factoryName"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{platform}-{}", machine_id.replace('-', "")));
    let mut args = vec![
        "host".into(),
        "--factory-name".into(),
        name.clone(),
        "--vm-template".into(),
        root.display().to_string(),
        "--vm-guest-runtime".into(),
        guest.display().to_string(),
        "--vm-workspace".into(),
        "/workspace".into(),
        "--vm-memory-mib".into(),
        config["vmMemoryMiB"].as_u64().unwrap_or(2048).to_string(),
        "--vm-cpus".into(),
        config["vmCpus"].as_u64().unwrap_or(2).to_string(),
        "--max-vms".into(),
        config["maxVms"].as_u64().unwrap_or(4).to_string(),
        "--log-format".into(),
        "json".into(),
    ];
    if let Some(firmware) = path("NANOCODEX_KRUNFW_DIR", "firmware") {
        args.extend(["--vm-firmware".into(), firmware.display().to_string()]);
    }
    if config["gpu"] == true {
        args.push("--vm-gpu".into());
    }
    let binary = if cfg!(windows) {
        let distro = wsl
            .filter(|name| !name.is_empty() && !name.starts_with('-') && !name.contains('\0'))
            .ok_or_else(|| error("wslDistribution must name a configured WSL2 distribution"))?;
        args = wsl_factory_args(distro, &binary, &name, args);
        PathBuf::from(
            std::env::var_os("SystemRoot")
                .ok_or_else(|| error("SystemRoot is required for WSL"))?,
        )
        .join("System32/wsl.exe")
    } else {
        args.extend([
            "--state-dir".into(),
            directory.join("vms").display().to_string(),
            "--vm-cache".into(),
            directory.join("vm-cache").display().to_string(),
        ]);
        binary
    };
    Ok(Some(FactoryRecipe { name, binary, args }))
}
fn desktop_data() -> Result<PathBuf, ManagedError> {
    if let Some(path) = std::env::var_os("NANOCODEX_DESKTOP_DATA") {
        return Ok(path.into());
    }
    let home = home()?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Nanocodex/Native")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map_or_else(|| home.join("AppData/Local"), PathBuf::from)
            .join("Nanocodex/Native")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map_or_else(|| home.join(".local/share"), PathBuf::from)
            .join("nanocodex/native")
    })
}

fn wsl_factory_args(
    distribution: &str,
    binary: &Path,
    name: &str,
    args: Vec<String>,
) -> Vec<String> {
    // All configured strings are argv entries, never shell source. Only the
    // known-safe provider identifier enters this wrapper; HOME is resolved by
    // the Linux user. Credentials cross via WSLENV, never the command line.
    let script = format!(
        "test -r /dev/kvm && test -w /dev/kvm || {{ echo 'WSL2 VM hosting requires accessible /dev/kvm and nested virtualization' >&2; exit 1; }}; exec \"$@\" --state-dir \"$HOME/.nanocodex/hands/{name}/vms\" --vm-cache \"$HOME/.nanocodex/hands/{name}/vm-cache\""
    );
    let mut command = vec![
        "--distribution".into(),
        distribution.into(),
        "--exec".into(),
        "/bin/sh".into(),
        "-c".into(),
        script,
        "nanocodex-vm-host".into(),
        binary.display().to_string(),
    ];
    command.extend(args);
    command
}

async fn supervise_factory(
    recipe: FactoryRecipe,
    directory: &Path,
    origin: &str,
    key: &str,
    cancel: &CancellationToken,
    status: &std::sync::Mutex<Value>,
) {
    let update = |state: &str| {
        let mut value = status.lock().unwrap();
        value["factory"] = json!({"name": recipe.name, "status": state});
        let _ = publish(directory, &value);
        emit(&value);
    };
    while !cancel.is_cancelled() {
        update("connecting");
        let mut command = Command::new(&recipe.binary);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        // WSLENV lists names only. Preserve the user's unrelated environment
        // transfers while making these three variables Linux-visible.
        let wslenv = [
            std::env::var("WSLENV").unwrap_or_default(),
            "NANOCODEX_API_KEY:NANOCODEX_MANAGED_URL:NANOCODEX_PARENT_PIPE".into(),
        ]
        .join(":");
        let child = command
            .args(&recipe.args)
            .env("NANOCODEX_API_KEY", key)
            .env("NANOCODEX_MANAGED_URL", origin)
            .env("NANOCODEX_PARENT_PIPE", "1")
            .env("WSLENV", wslenv)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        if let Ok(mut child) = child {
            let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
            let mut log = log_file(directory, "vm.log").ok();
            // A live factory owns reconnects, including its initial connection.
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => {
                            if let Some(log) = &mut log { let _ = writeln!(log, "{}", line.replace(key, "[redacted]")); }
                            if let Ok(entry) = serde_json::from_str::<Value>(&line) {
                                match entry["fields"]["stage"].as_str() {
                                    Some("vm.host.ready") => update("connected"),
                                    Some("vm.host.reconnecting") => update("connecting"),
                                    _ => {},
                                }
                            }
                        }
                        _ => break,
                    }
                }
            }
            // EOF shuts down both native and WSL-hosted factories gracefully.
            drop(child.stdin.take());
            #[cfg(unix)]
            if let Some(id) = child.id() {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(id as i32),
                    nix::sys::signal::Signal::SIGINT,
                );
            }
            if tokio::time::timeout(Duration::from_secs(20), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
        if cancel.is_cancelled() {
            break;
        }
        update("error");
        tokio::select! { () = cancel.cancelled() => break, () = tokio::time::sleep(Duration::from_secs(5)) => {} }
    }
    update("stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn wsl_launch_keeps_paths_and_distribution_out_of_shell_source() {
        let binary = Path::new("/home/user/My Tools/nanocodex2");
        let root = "/images/desktop; touch /tmp/unwanted";
        let args = wsl_factory_args(
            "Ubuntu Test",
            binary,
            "windows-1234",
            vec!["host".into(), "--vm-template".into(), root.into()],
        );
        assert_eq!(
            &args[..5],
            ["--distribution", "Ubuntu Test", "--exec", "/bin/sh", "-c"]
        );
        assert!(!args[5].contains(root));
        assert!(!args[5].contains("My Tools"));
        assert!(args[5].contains("/dev/kvm"));
        assert_eq!(
            &args[6..],
            [
                "nanocodex-vm-host",
                "/home/user/My Tools/nanocodex2",
                "host",
                "--vm-template",
                root
            ]
        );
    }

    #[test]
    fn publisher_lock_is_exclusive_and_released_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let publisher = super::super::native_hand::NativeStateLock(
            log_file(temp.path(), "hand-daemon.lock").unwrap(),
        );
        publisher.0.try_lock().unwrap();
        let contender = log_file(temp.path(), "hand-daemon.lock").unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(publisher);
        contender.try_lock().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn installed_log_directory_can_be_shared_while_log_contents_remain_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let file = log_file(temp.path(), "hand.log").unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(log_file(temp.path(), "hand.log").is_err());
    }
    #[tokio::test]
    async fn publisher_survives_last_client_until_service_shutdown() {
        #[cfg(unix)]
        let path = PathBuf::from(format!("/tmp/ncx-{}.sock", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"\\.\pipe\ncx-test-{}", uuid::Uuid::new_v4()));
        let listener = transport::Listener::bind(&path).unwrap();
        let cancel = CancellationToken::new();
        let watching = tokio::spawn(watch_clients(listener, cancel.clone()));
        let first = transport::connect(&path).await.unwrap();
        let second = transport::connect(&path).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(first);
        tokio::time::sleep(Duration::from_millis(2300)).await;
        assert!(
            !cancel.is_cancelled(),
            "closing the CLI must not stop the app's native host or VMs"
        );
        drop(second);
        tokio::time::sleep(Duration::from_millis(2300)).await;
        assert!(
            !cancel.is_cancelled(),
            "last client must not stop the service"
        );
        cancel.cancel();
        watching.await.unwrap();
    }
    #[tokio::test]
    async fn reconnect_preserves_publisher() {
        #[cfg(unix)]
        let path = PathBuf::from(format!("/tmp/ncx-{}.sock", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"\\.\pipe\ncx-test-{}", uuid::Uuid::new_v4()));
        let listener = transport::Listener::bind(&path).unwrap();
        let cancel = CancellationToken::new();
        let watching = tokio::spawn(watch_clients(listener, cancel.clone()));
        let first = transport::connect(&path).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(first);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let second = transport::connect(&path).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2300)).await;
        assert!(!cancel.is_cancelled());
        cancel.cancel();
        drop(second);
        watching.await.unwrap();
    }
}

#[cfg(test)]
mod idle_update_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn endpoint() -> PathBuf {
        #[cfg(unix)]
        {
            PathBuf::from(format!("/tmp/ncx-update-{}.sock", uuid::Uuid::new_v4()))
        }
        #[cfg(windows)]
        {
            PathBuf::from(format!(r"\\.\pipe\ncx-update-{}", uuid::Uuid::new_v4()))
        }
    }

    #[tokio::test]
    async fn update_request_fails_closed_without_runtime_barrier() {
        let path = endpoint();
        let listener = transport::Listener::bind(&path).unwrap();
        let cancel = CancellationToken::new();
        let watching = tokio::spawn(watch_clients(listener, cancel.clone()));
        assert!(!transport::prepare_idle_update(&path).await.unwrap());
        assert!(!cancel.is_cancelled());
        assert!(transport::connect(&path).await.is_ok());
        cancel.cancel();
        watching.await.unwrap();
    }

    #[tokio::test]
    async fn lease_prevents_barrier_and_success_closes_admission_before_ack() {
        let path = endpoint();
        let listener = transport::Listener::bind(&path).unwrap();
        let cancel = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let recorded = calls.clone();
        let watching = tokio::spawn(watch_clients_with_barrier(
            listener,
            cancel.clone(),
            move || {
                recorded.fetch_add(1, Ordering::SeqCst);
                async { true }
            },
        ));
        let lease = transport::connect(&path).await.unwrap();
        assert!(!transport::prepare_idle_update(&path).await.unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!cancel.is_cancelled());
        drop(lease);
        // EOF processing is asynchronous; retry only the explicit deferred
        // response, never an ambiguous connection failure.
        let accepted = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if transport::prepare_idle_update(&path).await.unwrap() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        accepted.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cancel.is_cancelled());
        watching.await.unwrap();
        assert!(transport::connect(&path).await.is_err());
    }

    #[tokio::test]
    async fn old_daemon_eof_is_not_an_update_acknowledgement() {
        let path = endpoint();
        let mut listener = transport::Listener::bind(&path).unwrap();
        let old_daemon = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let _ = stream.read_u8().await;
        });
        assert!(transport::prepare_idle_update(&path).await.is_err());
        old_daemon.await.unwrap();
    }

    #[tokio::test]
    async fn unresponsive_daemon_request_is_bounded() {
        let path = endpoint();
        let mut listener = transport::Listener::bind(&path).unwrap();
        let stalled = tokio::spawn(async move {
            let _stream = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let request = transport::prepare_idle_update(&path).await.unwrap_err();
        assert_eq!(request.kind(), std::io::ErrorKind::TimedOut);
        stalled.abort();
    }
}
