//! One account Hand per computer, shared by the terminal and desktop clients.
//! The CLI holds an in-process IPC lease; other clients may use the helper. A single
//! publisher is owned by the OS service and survives all client disconnects.
use clap::Args;
use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_oai_tools::attachment::AttachmentEvent;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use super::native_hand::NativeState;

#[cfg(any(target_os = "macos", test))]
mod power;

use nanocodex_bin_shared::hand_client::{directory, error, home, log_file, socket_path, transport};

#[derive(Args, Default)]
pub(crate) struct DeviceHand {
    /// Report service update compatibility without reading account credentials.
    #[arg(long, hide = true, conflicts_with_all = ["describe", "daemon", "parent_pipe", "prepare_update"])]
    service_protocol: bool,
    /// Request an authoritative idle barrier from the currently running daemon.
    #[arg(long, hide = true, conflicts_with_all = ["describe", "daemon", "parent_pipe"])]
    prepare_update: bool,
    /// Ask the running daemon process to request OS consent for itself.
    #[arg(long, hide = true, requires_all = ["daemon_pid", "daemon_executable"],
        conflicts_with_all = ["describe", "daemon", "parent_pipe", "prepare_update", "service_protocol"])]
    request_permissions: bool,
    /// Ask the running daemon for its OS consent status without prompting.
    #[arg(long, hide = true, requires_all = ["daemon_pid", "daemon_executable"],
        conflicts_with_all = ["describe", "daemon", "parent_pipe", "prepare_update", "service_protocol", "request_permissions"])]
    check_permissions: bool,
    /// PID the service manager reports for the running daemon.
    #[arg(long, hide = true)]
    daemon_pid: Option<u32>,
    /// Executable the service manager reports for the running daemon.
    #[arg(long, hide = true)]
    daemon_executable: Option<PathBuf>,
    /// Print the shared identity without publishing a Hand.
    #[arg(long)]
    describe: bool,
    #[arg(long, hide = true)]
    pub(super) daemon: bool,
    /// Exit when the owning application closes stdin.
    #[arg(long)]
    parent_pipe: bool,
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
        emit(
            &json!({"serviceProtocol": 1, "version": env!("CARGO_PKG_VERSION"), "handIdentity": crate::version::hand_identity()}),
        );
        return Ok(());
    }
    if command.request_permissions || command.check_permissions {
        let (Some(pid), Some(executable)) = (command.daemon_pid, command.daemon_executable) else {
            return Err(error("--daemon-pid and --daemon-executable are required"));
        };
        let opcode = if command.check_permissions {
            transport::CHECK_PERMISSIONS
        } else {
            transport::REQUEST_PERMISSIONS
        };
        emit(&request_daemon_permissions(pid, &executable, opcode).await?);
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

/// This process as published in status.json and permission replies.
fn daemon_identity() -> Value {
    json!({"pid": std::process::id(), "executable": std::env::current_exe().ok(), "version": env!("CARGO_PKG_VERSION")})
}

/// Credential-free: the publisher records its PID in its account directory's
/// status.json. Only the directory naming the service manager's PID is used.
fn daemon_directory(hands: &Path, pid: u32) -> Result<PathBuf, ManagedError> {
    fs::read_dir(hands)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|directory| {
            fs::read(directory.join("status.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|status| status["daemon"]["pid"].as_u64() == Some(u64::from(pid)))
        })
        .ok_or_else(|| {
            error(format!(
                "The running Hand (PID {pid}) has not published a control endpoint. It may still be starting, or predate permission requests; update it and retry."
            ))
        })
}

async fn request_daemon_permissions(
    pid: u32,
    executable: &Path,
    opcode: u8,
) -> Result<Value, ManagedError> {
    let directory = daemon_directory(&home()?.join(".nanocodex/hands"), pid)?;
    permissions_at(&socket_path(&directory)?, pid, executable, opcode).await
}

#[cfg(test)]
async fn request_permissions_at(
    socket: &Path,
    pid: u32,
    executable: &Path,
) -> Result<Value, ManagedError> {
    permissions_at(socket, pid, executable, transport::REQUEST_PERMISSIONS).await
}

/// The OS attributes consent to the process that asks. Refuse unless the
/// kernel-reported socket owner is the service manager's PID, and confirm the
/// reply names the same process and executable. Never retried automatically.
async fn permissions_at(
    socket: &Path,
    pid: u32,
    executable: &Path,
    opcode: u8,
) -> Result<Value, ManagedError> {
    #[cfg(unix)]
    {
        let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let expected = canonical(executable);
        let reply = transport::permissions(socket, pid, opcode)
            .await
            .map_err(error)?;
        let daemon = &reply["daemon"];
        let reported = daemon["executable"]
            .as_str()
            .map(|path| canonical(Path::new(path)));
        if daemon["pid"].as_u64() != Some(u64::from(pid)) || reported.as_deref() != Some(&*expected)
        {
            return Err(error(format!(
                "The Hand that answered ({daemon}) is not the running service (PID {pid}, {}).",
                expected.display()
            )));
        }
        Ok(reply)
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, pid, executable, opcode);
        Err(error("Hand permission requests are only used on macOS"))
    }
}

/// Read-only: what macOS allows this daemon right now. Never prompts.
fn check_os_permissions() -> Value {
    #[cfg(target_os = "macos")]
    {
        nanocodex_hand::access_status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        json!({"unsupported": format!("no OS consent is needed on {}", std::env::consts::OS)})
    }
}

/// Executed by the daemon on explicit request only. macOS shows its own
/// consent sheet; nothing here can grant, and `granted` is the OS's answer.
fn request_os_permissions() -> Value {
    #[cfg(target_os = "macos")]
    {
        nanocodex_hand::request_access()
    }
    #[cfg(not(target_os = "macos"))]
    {
        json!({"unsupported": format!("no OS consent is needed on {}", std::env::consts::OS)})
    }
}

/// Latest live screen outcome of this daemon's screen supervisor, answered on
/// permission checks so a status report reflects the running process itself.
/// Null until the supervisor reports its first outcome.
static SCREEN: std::sync::Mutex<Value> = std::sync::Mutex::new(Value::Null);

async fn answer_permissions(
    mut stream: impl tokio::io::AsyncWrite + Unpin,
    consent: fn() -> Value,
    requested: bool,
) {
    let permissions = tokio::task::spawn_blocking(consent)
        .await
        .unwrap_or_else(|_| json!({"error": "the permission request failed"}));
    if requested {
        tracing::info!(target: "nanocodex2", stage = "hand.permissions.requested", %permissions,
            "Requested OS permissions on explicit user action");
    }
    let screen = SCREEN
        .lock()
        .map(|screen| screen.clone())
        .unwrap_or(Value::Null);
    let mut reply = serde_json::to_vec(
        &json!({"daemon": daemon_identity(), "permissions": permissions, "screen": screen}),
    )
    .unwrap_or_default();
    reply.push(b'\n');
    let _ = stream.write_all(&reply).await;
    let _ = stream.shutdown().await;
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
            // Hold through reconnects and cleanup, after both publisher locks.
            #[cfg(target_os = "macos")]
            let _keep_awake = power::Monitor::start(home()?)
                .map_err(|error| {
                    tracing::warn!(%error, "Cannot start Hand power watcher");
                })
                .ok();
            let socket = socket_path(directory)?;
            let listener = transport::Listener::bind(&socket).map_err(error)?;
            let lease_cancel = cancel.clone();
            let leases = tokio::spawn(async move {
                watch_clients(listener, lease_cancel).await;
            });
            // Explicit migration reference for a separately managed factory.
            // Advertising its provider never takes ownership of its lifetime,
            // starts a second factory, or weakens the broker's allocation checks.
            let external_factory = std::env::var("NANOCODEX_EXTERNAL_VM_FACTORY")
                .ok()
                .filter(|name| !name.is_empty());
            let recipe = if external_factory.is_some() {
                Ok(None)
            } else {
                factory_recipe(directory, state.machine.id())
            };
            let factory_error = recipe.as_ref().err().map(ToString::to_string);
            let recipe = recipe.unwrap_or(None);
            if let Some(name) = external_factory.as_deref() {
                state.advertise_vm_provider(name)?;
            } else if let Some(recipe) = &recipe {
                state.advertise_vm_provider(&recipe.name)?;
            }
            let machine = serde_json::to_value(&state.machine).map_err(error)?;
            let status = std::sync::Arc::new(std::sync::Mutex::new(
                json!({"machine": machine, "status": "connecting", "daemon": daemon_identity()}),
            ));
            {
                let mut status = status.lock().unwrap();
                if let Some(name) = &external_factory {
                    status["factory"] = json!({"status": "external", "provider": name});
                } else if recipe.is_none() {
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
            // Share the native Hand's capture supervision: keep the shell ready
            // while capture starts, repair helpers in place, and retain replacement
            // fences instead of leaving a failed screen idle until daemon restart.
            let screen_target = client.account_attachment_target()?;
            let desktop = super::screen_native::DesktopSlot::default();
            let result = super::screen_supervisor::while_attached_reported(
                || {
                    super::screen_native::NativeScreen::start_retaining(
                        &screen_target,
                        &state.machine,
                        directory,
                        &desktop,
                    )
                },
                super::native_hand::run_observed(
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
                ),
                |report| {
                    use super::screen_supervisor::{Report, Stop};
                    let (state, error, reason) = match report {
                        Report::Starting => ("starting", None, None),
                        Report::Ready => ("ready", None, None),
                        Report::Reconnecting => ("reconnecting", None, Some("publication_lost")),
                        Report::Recovering => ("recovering", None, Some("capture_repair")),
                        Report::Unavailable(error) => ("unavailable", Some(error.to_string()), None),
                        Report::Stopped(Stop::Finished) => ("stopped", None, Some("publisher_stopped")),
                        Report::Stopped(Stop::Shutdown) => ("stopped", None, Some("attachment_ended")),
                    };
                    let since_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_millis() as u64);
                    let screen = json!({"status": state, "transport": "webrtc", "error": error, "reason": reason, "since_ms": since_ms});
                    if let Ok(mut current) = SCREEN.lock() {
                        *current = screen.clone();
                    }
                    let mut status = status.lock().unwrap();
                    status["screen"] = screen;
                    let _ = publish(directory, &status);
                },
            )
            .await;
            cancel.cancel();
            let _ = leases.await;
            if let Some(factory) = factory {
                let _ = factory.await;
            }
            let _ = fs::remove_file(directory.join("status.json"));
            result
        }
        Err(e) => Err(e),
    }
}

async fn connect(directory: &Path, cancel: &CancellationToken) -> Result<(), ManagedError> {
    let socket = socket_path(directory)?;
    #[cfg(target_os = "macos")]
    if transport::connect(&socket).await.is_err() {
        nanocodex_bin_shared::hand_client::ensure_service().await?;
    }
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
    watch_clients_with_barrier(listener, cancel, || async { false }, request_os_permissions).await;
}

async fn watch_clients_with_barrier<F, Fut>(
    mut listener: transport::Listener,
    cancel: CancellationToken,
    mut prepare: F,
    consent: fn() -> Value,
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
                            // Answered on this client's task: never admits a
                            // lease or participates in the update barrier.
                            Ok(transport::REQUEST_PERMISSIONS) => {
                                answer_permissions(stream, consent, true).await;
                                None
                            }
                            // Polled by permission guides; read-only.
                            Ok(transport::CHECK_PERMISSIONS) => {
                                answer_permissions(stream, check_os_permissions, false).await;
                                None
                            }
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
            || -> Value { unreachable!("an update barrier never requests OS consent") },
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

/// Real lease socket and daemon accept loop; only the macOS consent sheet is
/// replaced, because a test must never ask TCC on behalf of the test runner.
#[cfg(all(test, unix))]
mod permission_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CONSENTS: AtomicUsize = AtomicUsize::new(0);
    fn pending_screen_consent() -> Value {
        CONSENTS.fetch_add(1, Ordering::SeqCst);
        json!({"screenCapture": {"granted": false, "requested": true}, "input": {"granted": true, "requested": false}})
    }

    #[tokio::test]
    async fn permission_request_reaches_only_the_verified_daemon_without_disturbing_leases() {
        let path = PathBuf::from(format!(
            "/tmp/ncx-permissions-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let listener = transport::Listener::bind(&path).unwrap();
        let cancel = CancellationToken::new();
        let watching = tokio::spawn(watch_clients_with_barrier(
            listener,
            cancel.clone(),
            || async { false },
            pending_screen_consent,
        ));
        let mut lease = transport::connect(&path).await.unwrap();
        let pid = std::process::id();
        let executable = std::env::current_exe().unwrap();

        // A different service PID is refused before the opcode is sent.
        let refused = request_permissions_at(&path, pid + 1, &executable)
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("no permission was requested"), "{refused}");
        assert_eq!(CONSENTS.load(Ordering::SeqCst), 0);

        // A guide's status poll answers without requesting consent.
        let checked = permissions_at(&path, pid, &executable, transport::CHECK_PERMISSIONS)
            .await
            .unwrap();
        assert_eq!(CONSENTS.load(Ordering::SeqCst), 0);
        assert_eq!(checked["daemon"]["pid"], pid);
        assert!(checked["permissions"].is_object());

        let reply = request_permissions_at(&path, pid, &executable)
            .await
            .unwrap();
        assert_eq!(CONSENTS.load(Ordering::SeqCst), 1);
        assert_eq!(reply["daemon"]["pid"], pid);
        assert_eq!(
            reply["permissions"]["screenCapture"],
            json!({"granted": false, "requested": true})
        );

        // A process running a different binary than the service manager reports
        // is reported, not trusted.
        let mismatch = request_permissions_at(&path, pid, Path::new("/nonexistent/nanocodex2"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            mismatch.contains("is not the running service"),
            "{mismatch}"
        );

        // The existing lease and publisher are untouched by the request.
        let mut byte = [0];
        assert!(
            tokio::time::timeout(Duration::from_millis(200), lease.read(&mut byte))
                .await
                .is_err()
        );
        assert!(!cancel.is_cancelled());
        cancel.cancel();
        watching.await.unwrap();
    }

    #[tokio::test]
    async fn old_daemon_and_missing_endpoint_are_actionable_errors() {
        let path = PathBuf::from(format!(
            "/tmp/ncx-permissions-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let mut listener = transport::Listener::bind(&path).unwrap();
        let old_daemon = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let _ = stream.read_u8().await; // Unknown opcode: close without replying.
        });
        let old =
            request_permissions_at(&path, std::process::id(), &std::env::current_exe().unwrap())
                .await
                .unwrap_err()
                .to_string();
        assert!(
            old.contains("does not support permission requests"),
            "{old}"
        );
        old_daemon.await.unwrap();

        let hands = tempfile::tempdir().unwrap();
        for (name, pid) in [("stale", 1_u32), ("current", 4242)] {
            fs::create_dir(hands.path().join(name)).unwrap();
            fs::write(
                hands.path().join(name).join("status.json"),
                json!({"status": "connected", "daemon": {"pid": pid}}).to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            daemon_directory(hands.path(), 4242).unwrap(),
            hands.path().join("current")
        );
        let missing = daemon_directory(hands.path(), 7).unwrap_err().to_string();
        assert!(missing.contains("PID 7"), "{missing}");
    }
}
