//! Account-wide native workspace attachment, independent of an agent or VM.

use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use clap::Args;
use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_tools::{
    Tools, WorkspaceTools,
    attachment::{AttachmentEvent, AttachmentMachine, AttachmentMetadata, AttachmentTarget},
};
use serde::{Deserialize, Serialize};

use super::{hand_observability::HandObservabilityArgs, host};

#[derive(Args)]
pub(crate) struct NativeHand {
    #[command(flatten)]
    observability: HandObservabilityArgs,

    /// Existing workspace whose native files and programs this Hand exposes.
    #[arg(long, value_name = "PATH")]
    workspace: PathBuf,

    /// Private identity directory; defaults to the account config's native-hand directory.
    #[arg(long, value_name = "PATH")]
    state_dir: Option<PathBuf>,

    /// Human-readable machine name; defaults to this device's name.
    #[arg(long, value_name = "NAME")]
    machine_name: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    machine_id: uuid::Uuid,
    workspace: PathBuf,
}

struct NativeState {
    machine: AttachmentMachine,
    _lock: NativeStateLock,
}

struct NativeStateLock(File);

impl Drop for NativeStateLock {
    fn drop(&mut self) {
        // Closing alone leaves the lock held by descriptors inherited during a
        // concurrent fork. Release ownership explicitly, including error paths.
        let _ = self.0.unlock();
    }
}

impl NativeState {
    fn open(workspace: &Path, directory: &Path, name: String) -> Result<Self, ManagedError> {
        let workspace = fs::canonicalize(workspace).map_err(configuration)?;
        if !workspace.is_dir() {
            return Err(configuration(
                "native Hand workspace must be an existing directory",
            ));
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(directory).map_err(configuration)?;
        require_regular(directory, true)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if fs::metadata(directory)
                .map_err(configuration)?
                .permissions()
                .mode()
                & 0o077
                != 0
            {
                return Err(configuration(
                    "native Hand state directory must be private; restrict its permissions to 0700 or use a separate --state-dir",
                ));
            }
        }
        let lock_path = directory.join("host.lock");
        if lock_path.exists() {
            require_regular(&lock_path, false)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let lock = options.open(lock_path).map_err(configuration)?;
        lock.try_lock()
            .map_err(|_| configuration("another native Hand is using this state directory"))?;
        let lock = NativeStateLock(lock);

        let path = directory.join("identity.json");
        let identity = match fs::symlink_metadata(&path) {
            Ok(_) => {
                require_regular(&path, false)?;
                let identity: Identity =
                    serde_json::from_slice(&fs::read(&path).map_err(configuration)?)
                        .map_err(configuration)?;
                if identity.machine_id.get_version_num() != 4 || identity.workspace != workspace {
                    return Err(configuration(
                        "native Hand identity belongs to another workspace or is invalid; use its workspace or a separate --state-dir",
                    ));
                }
                identity
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let identity = Identity {
                    machine_id: uuid::Uuid::new_v4(),
                    workspace,
                };
                let mut temporary =
                    tempfile::NamedTempFile::new_in(directory).map_err(configuration)?;
                serde_json::to_writer(&mut temporary, &identity).map_err(configuration)?;
                temporary.write_all(b"\n").map_err(configuration)?;
                temporary.as_file().sync_all().map_err(configuration)?;
                temporary.persist_noclobber(&path).map_err(configuration)?;
                identity
            }
            Err(error) => return Err(configuration(error)),
        };
        let workspace = identity
            .workspace
            .to_str()
            .ok_or_else(|| configuration("native Hand workspace must be valid UTF-8"))?;
        let machine = AttachmentMachine::new(
            identity.machine_id.to_string(),
            name,
            workspace,
            host::MACHINE_CAPABILITIES,
        )
        .map_err(configuration)?;
        Ok(Self {
            machine,
            _lock: lock,
        })
    }
}

fn require_regular(path: &Path, directory: bool) -> Result<(), ManagedError> {
    let kind = fs::symlink_metadata(path)
        .map_err(configuration)?
        .file_type();
    if (directory && kind.is_dir()) || (!directory && kind.is_file()) {
        Ok(())
    } else {
        Err(configuration(
            "native Hand state must use regular files and directories",
        ))
    }
}

pub(crate) async fn serve(client: &ManagedClient, command: NativeHand) -> Result<(), ManagedError> {
    let _observability = command.observability.install().map_err(configuration)?;
    let directory = match command.state_dir {
        Some(directory) => directory,
        None => host::config_path()
            .map_err(configuration)?
            .with_file_name("native-hand"),
    };
    let name = command
        .machine_name
        .unwrap_or_else(|| host::bounded_display_name(whoami::devicename()));
    let state = NativeState::open(&command.workspace, &directory, name)?;
    run(
        client.account_attachment_target()?,
        state,
        shutdown_signal(),
    )
    .await
}

async fn run(
    target: AttachmentTarget,
    state: NativeState,
    shutdown: impl Future<Output = Result<(), ManagedError>>,
) -> Result<(), ManagedError> {
    // WorkspaceTools uses the existing sanitized subprocess environment. Do not
    // forward the account credential or ambient sensitive variables to programs.
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(state.machine.workspace()))
        .build()
        .map_err(configuration)?;
    let (attachment, mut events) = tools
        .attach(target)
        .metadata(AttachmentMetadata::machine(state.machine.clone()))
        .start()
        .map_err(configuration)?;
    let closed = attachment.clone();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            result = &mut shutdown => {
                result?;
                return attachment.detach().await.map_err(configuration);
            }
            result = closed.closed() => return result.map_err(configuration),
            Some(event) = events.recv() => match event {
                AttachmentEvent::Connecting => tracing::info!(target: "nanocodex2",
                    stage = "native.hand.connecting", "Connecting native Hand"),
                AttachmentEvent::CatalogPublished { .. } => tracing::info!(target: "nanocodex2",
                    stage = "native.hand.ready", machine_id = state.machine.id(),
                    "Native Hand is ready; press Ctrl-C to detach"),
                _ => {}
            }
        }
    }
}

async fn shutdown_signal() -> Result<(), ManagedError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(configuration)?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(configuration),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.map_err(configuration)
}

fn configuration(error: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        extract::{
            State, WebSocketUpgrade,
            ws::{Message, WebSocket},
        },
        http::HeaderMap,
        routing::get,
    };
    use clap::Parser as _;
    use serde_json::{Value, json};
    use tokio::sync::{mpsc, oneshot};

    use super::*;

    fn private_state_directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        directory
    }

    #[test]
    fn native_hand_requires_an_explicit_workspace() {
        assert!(crate::Cli::try_parse_from(["nanocodex2", "native-hand"]).is_err());
        let cli = crate::Cli::try_parse_from([
            "nanocodex2",
            "native-hand",
            "--workspace",
            ".",
            "--machine-name",
            "Linux server",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(crate::Command::NativeHand(_))));
    }

    #[test]
    fn native_identity_survives_restart_and_excludes_other_workspaces_or_processes() {
        let workspace = tempfile::tempdir().unwrap();
        let other_workspace = tempfile::tempdir().unwrap();
        let directory = private_state_directory();
        let state = NativeState::open(workspace.path(), directory.path(), "Server".into()).unwrap();
        let id = state.machine.id().to_owned();
        assert!(NativeState::open(workspace.path(), directory.path(), "Other".into()).is_err());
        drop(state);
        assert!(
            NativeState::open(other_workspace.path(), directory.path(), "Other".into()).is_err()
        );
        let reopened =
            NativeState::open(workspace.path(), directory.path(), "Renamed".into()).unwrap();
        assert_eq!(reopened.machine.id(), id);
        assert_eq!(reopened.machine.name(), "Renamed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(directory.path().join("identity.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_identity_releases_its_lock_while_an_inherited_descriptor_exists() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = private_state_directory();
        let state = NativeState::open(workspace.path(), directory.path(), "Server".into()).unwrap();
        // A duplicated descriptor has the same lock lifetime as one inherited at fork.
        let inherited = state._lock.0.try_clone().unwrap();
        drop(state);
        let reopened = NativeState::open(workspace.path(), directory.path(), "Restarted".into());
        assert!(
            reopened.is_ok(),
            "the owner released the lock: {:?}",
            reopened.err()
        );
        drop(inherited);
    }

    #[cfg(unix)]
    #[test]
    fn native_identity_rejects_symlinked_state() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let linked = directory.path().join("linked");
        std::os::unix::fs::symlink(workspace.path(), &linked).unwrap();
        assert!(NativeState::open(workspace.path(), &linked, "Server".into()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn native_identity_does_not_repurpose_a_shared_state_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let workspace = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(NativeState::open(workspace.path(), directory.path(), "Server".into()).is_err());
        assert!(!directory.path().join("identity.json").exists());
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(NativeState::open(workspace.path(), directory.path(), "Server".into()).is_ok());
    }

    #[derive(Clone)]
    struct LocalService {
        connections: Arc<AtomicUsize>,
        catalogs: mpsc::UnboundedSender<Value>,
        completed: mpsc::UnboundedSender<()>,
    }

    async fn accept(
        State(state): State<LocalService>,
        headers: HeaderMap,
        upgrade: WebSocketUpgrade,
    ) -> axum::response::Response {
        assert_eq!(
            headers["authorization"],
            "Bearer native-hand-test-credential"
        );
        upgrade.on_upgrade(move |socket| serve_socket(socket, state))
    }

    async fn receive(socket: &mut WebSocket) -> Value {
        loop {
            let Some(Ok(Message::Text(frame))) = socket.recv().await else {
                panic!("native Hand closed before its protocol response");
            };
            let frame: Value = serde_json::from_str(&frame).unwrap();
            if frame["type"] == "ping" {
                socket
                    .send(Message::Text(
                        json!({"type":"pong", "nonce":frame["nonce"]})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            } else {
                return frame;
            }
        }
    }

    async fn serve_socket(mut socket: WebSocket, state: LocalService) {
        let connection = state.connections.fetch_add(1, Ordering::SeqCst);
        let catalog = receive(&mut socket).await;
        assert_eq!(catalog["type"], "catalog");
        state.catalogs.send(catalog).unwrap();
        socket
            .send(Message::Text(json!({"type":"ready"}).to_string().into()))
            .await
            .unwrap();
        if connection == 0 {
            socket.send(Message::Close(None)).await.unwrap();
            return;
        }
        socket.send(Message::Text(json!({
            "type":"call", "session_id":"native-test-agent", "call_id":"native-file-process",
            "model":"gpt-6-astra", "name":"exec_command",
            "input":{"cmd":"printf 'native-process-proof\\n' > native-proof.txt && cat native-proof.txt"},
            "output_token_budget":1024, "output_byte_budget":131072,
            "deadline_at":9_000_000_000_000_u64,
        }).to_string().into())).await.unwrap();
        let result = receive(&mut socket).await;
        assert_eq!(result["type"], "result");
        assert_eq!(result["call_id"], "native-file-process");
        assert_eq!(result["outcome"]["status"], "completed");
        assert_eq!(result["outcome"]["output"]["success"], true, "{result}");
        assert!(
            result["outcome"]["output"]["output"]
                .as_str()
                .unwrap()
                .contains("native-process-proof")
        );
        socket
            .send(Message::Text(
                json!({"type":"ack", "call_id":"native-file-process"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        state.completed.send(()).unwrap();
        assert_eq!(receive(&mut socket).await["type"], "drain");
        socket
            .send(Message::Text(json!({"type":"draining"}).to_string().into()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn native_hand_executes_in_its_workspace_after_outbound_reconnect_and_detaches() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = private_state_directory();
        let state =
            NativeState::open(workspace.path(), directory.path(), "Test Linux".into()).unwrap();
        let machine_id = state.machine.id().to_owned();
        let (catalogs_tx, mut catalogs) = mpsc::unbounded_channel();
        let (completed_tx, mut completed) = mpsc::unbounded_channel();
        let service = LocalService {
            connections: Arc::new(AtomicUsize::new(0)),
            catalogs: catalogs_tx,
            completed: completed_tx,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/account/tool-host", get(accept))
            .with_state(service);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let target = AttachmentTarget::new(
            format!("ws://{address}/v1/account/tool-host"),
            "native-hand-test-credential",
        )
        .unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let hand = tokio::spawn(run(target, state, async {
            shutdown_rx.await.unwrap();
            Ok(())
        }));
        tokio::time::timeout(Duration::from_secs(20), async {
            let first = catalogs.recv().await.unwrap();
            let second = catalogs.recv().await.unwrap();
            assert_eq!(first, second);
            assert_eq!(second["attachment_id"], machine_id);
            assert_eq!(second["machines"][0]["id"], machine_id);
            assert_eq!(
                second["machines"][0]["capabilities"],
                json!(host::MACHINE_CAPABILITIES)
            );
            completed.recv().await.unwrap();
            assert_eq!(
                fs::read_to_string(workspace.path().join("native-proof.txt")).unwrap(),
                "native-process-proof\n"
            );
            shutdown_tx.send(()).unwrap();
            hand.await.unwrap().unwrap();
        })
        .await
        .expect("native Hand lifecycle timed out");
        server.abort();
        let restarted =
            NativeState::open(workspace.path(), directory.path(), "Test Linux".into()).unwrap();
        assert_eq!(restarted.machine.id(), machine_id);
    }
}
