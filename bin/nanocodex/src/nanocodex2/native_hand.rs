//! Account-wide native workspace attachment, independent of an agent or VM.

use std::{
    fs::{self, File, OpenOptions},
    future::Future,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_tools::{
    Tools, WorkspaceTools,
    attachment::{AttachmentEvent, AttachmentMachine, AttachmentMetadata, AttachmentTarget},
};
use serde::{Deserialize, Serialize};

use super::{hand_observability::HandObservabilityArgs, host};

struct NativeHand {
    observability: HandObservabilityArgs,
    workspace: PathBuf,
    state_dir: Option<PathBuf>,
    machine_name: Option<String>,
    vm_provider: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    machine_id: uuid::Uuid,
    workspace: PathBuf,
}

pub(super) struct NativeState {
    pub(super) machine: AttachmentMachine,
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
    pub(super) fn advertise_vm_provider(&mut self, provider: &str) -> Result<(), ManagedError> {
        super::validate_vm_factory_name(provider)?;
        let mut capabilities: Vec<String> = self
            .machine
            .capabilities()
            .iter()
            .map(ToString::to_string)
            .collect();
        capabilities.push(format!("vm_factory:{provider}"));
        self.machine = AttachmentMachine::new(
            self.machine.id(),
            self.machine.name(),
            self.machine.workspace(),
            capabilities,
        )
        .map_err(configuration)?;
        Ok(())
    }

    pub(super) fn open(
        workspace: &Path,
        directory: &Path,
        name: String,
    ) -> Result<Self, ManagedError> {
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
        let capabilities = host::MACHINE_CAPABILITIES
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let machine = AttachmentMachine::new(
            identity.machine_id.to_string(),
            name,
            workspace,
            capabilities,
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

pub(super) async fn serve_hand(command: super::Hand) -> Result<(), ManagedError> {
    reject_browser_options(command.browser, command.browser_executable.as_deref())?;
    if command.machine_id.is_some() {
        return Err(configuration(
            "Native Hand identities are retained automatically; use --state-dir for another workspace",
        ));
    }
    if command.vm_workspace.is_none()
        && command.state_dir.is_none()
        && command.machine_name.is_none()
        && command.vm_provider.is_none()
    {
        return super::device_hand::serve(super::device_hand::DeviceHand::default()).await;
    }
    let client = super::client_from_environment(None)?;
    serve(
        &client,
        NativeHand {
            observability: command.observability,
            workspace: match command.vm_workspace {
                Some(path) => path.into(),
                None => std::env::current_dir().map_err(configuration)?,
            },
            state_dir: command.state_dir,
            machine_name: command.machine_name,
            vm_provider: command.vm_provider,
        },
    )
    .await
}

async fn serve(client: &ManagedClient, command: NativeHand) -> Result<(), ManagedError> {
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
    let mut state = NativeState::open(&command.workspace, &directory, name)?;
    if let Some(provider) = command.vm_provider {
        state.advertise_vm_provider(&provider)?;
    }
    let target = client.account_attachment_target()?;
    let screen = match super::screen_native::NativeScreen::start(
        &target,
        &state.machine,
        &directory,
    )
    .await
    {
        Ok(screen) => Some(screen),
        Err(error) => {
            tracing::warn!(target: "nanocodex2", stage = "native.screen.unavailable", %error,
                "Native screen unavailable; shell and filesystem remain connected");
            None
        }
    };
    let result = run(target, state, super::service::shutdown_signal()).await;
    let stopped = match screen {
        Some(screen) => screen.shutdown().await,
        None => Ok(()),
    };
    result.and(stopped)
}

pub(super) fn reject_browser_options(
    browser: bool,
    executable: Option<&Path>,
) -> Result<(), ManagedError> {
    if browser || executable.is_some() {
        return Err(configuration(
            "Hand browser automation is disabled; use the Hand's CUA tools to interact with its desktop browser",
        ));
    }
    Ok(())
}

async fn run(
    target: AttachmentTarget,
    state: NativeState,
    shutdown: impl Future<Output = Result<(), ManagedError>>,
) -> Result<(), ManagedError> {
    run_observed(target, &state, shutdown, |_| {}).await
}

pub(super) async fn run_observed(
    target: AttachmentTarget,
    state: &NativeState,
    shutdown: impl Future<Output = Result<(), ManagedError>>,
    mut observe: impl FnMut(&AttachmentEvent),
) -> Result<(), ManagedError> {
    // WorkspaceTools uses the existing sanitized subprocess environment. Do not
    // forward the account credential or ambient sensitive variables to programs.
    let mut tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(state.machine.workspace()));
    if let Some(config) = nanocodex_computer::ComputerConfig::discover_or_install()
        .await
        .map_err(ManagedError::Configuration)?
    {
        let computer = nanocodex_computer::ComputerTools::connect(config)
            .await
            .map_err(|error| ManagedError::Configuration(error.to_string()))?;
        for tool in computer.tools() {
            tools = tools.add(tool);
        }
    }
    let tools = tools.build().map_err(configuration)?;
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
            Some(event) = events.recv() => {
              observe(&event);
              match event {
                AttachmentEvent::Connecting => tracing::info!(target: "nanocodex2",
                    stage = "native.hand.connecting", "Connecting native Hand"),
                AttachmentEvent::CatalogPublished { .. } => {
                    super::service::ready();
                    tracing::info!(target: "nanocodex2",
                    stage = "native.hand.ready", machine_id = state.machine.id(),
                    "Native Hand is ready; press Ctrl-C to detach");
                },
                _ => {}
              }
            }
        }
    }
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
    fn native_computer_advertises_its_factory_without_changing_identity() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = private_state_directory();
        let mut state =
            NativeState::open(workspace.path(), directory.path(), "Computer".into()).unwrap();
        let identity = state.machine.id().to_owned();
        assert!(state.advertise_vm_provider("cf_sandbox").is_err());
        assert!(state.advertise_vm_provider("bad provider").is_err());
        state.advertise_vm_provider("linux-computer").unwrap();
        assert_eq!(state.machine.id(), identity);
        assert!(
            state
                .machine
                .capabilities()
                .iter()
                .any(|value| value.as_ref() == "vm_factory:linux-computer")
        );
        assert!(
            crate::Cli::try_parse_from([
                "nanocodex2",
                "hand",
                "--vm",
                "root.ext4",
                "--vm-provider",
                "linux-computer"
            ])
            .is_err()
        );
    }

    #[test]
    fn hand_defaults_to_the_computer_and_keeps_explicit_workspace_and_vm_modes() {
        for args in [
            vec!["nanocodex2", "hand"],
            vec!["nanocodex2", "hand", "--workspace", "/app"],
            vec![
                "nanocodex2",
                "hand",
                "--workspace",
                "/tmp",
                "--state-dir",
                "/tmp/identity",
            ],
        ] {
            let cli = crate::Cli::try_parse_from(args).unwrap();
            let Some(crate::Command::Hand(hand)) = cli.command else {
                panic!("expected unified Hand");
            };
            assert!(hand.rootfs.is_none() && hand.docker.is_none());
        }
        assert!(
            crate::Cli::try_parse_from([
                "nanocodex2",
                "hand",
                "--vm",
                "root.ext4",
                "--docker",
                "image",
                "--volume",
                "volume"
            ])
            .is_err()
        );
    }

    #[test]
    fn hand_parses_legacy_browser_options_and_rejects_removed_command() {
        assert!(crate::Cli::try_parse_from(["nanocodex2", "native-hand"]).is_err());
        assert!(
            crate::Cli::try_parse_from(["nanocodex2", "native-hand", "--workspace", "."]).is_err()
        );
        let cli = crate::Cli::try_parse_from([
            "nanocodex2",
            "hand",
            "--workspace",
            ".",
            "--machine-name",
            "Linux server",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(crate::Command::Hand(_))));
        assert!(
            crate::Cli::try_parse_from([
                "nanocodex2",
                "hand",
                "--workspace",
                ".",
                "--browser-executable",
                "/opt/chrome",
            ])
            .is_err()
        );
        let cli = crate::Cli::try_parse_from([
            "nanocodex2",
            "hand",
            "--workspace",
            ".",
            "--browser",
            "--browser-executable",
            "/opt/chrome",
        ])
        .unwrap();
        let Some(crate::Command::Hand(command)) = cli.command else {
            panic!("expected native Hand");
        };
        assert!(command.browser);
        assert_eq!(
            command.browser_executable,
            Some(PathBuf::from("/opt/chrome"))
        );
    }

    #[test]
    fn native_hand_has_no_browser_automation_capabilities() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = private_state_directory();
        let state =
            NativeState::open(workspace.path(), directory.path(), "CUA host".into()).unwrap();
        let machine = serde_json::to_value(&state.machine).unwrap();
        let capabilities = machine["capabilities"].as_array().unwrap();
        assert!(!capabilities.contains(&json!("browser")));
        assert!(!capabilities.contains(&json!("browser-egress")));
    }

    #[test]
    fn legacy_browser_options_require_cua() {
        reject_browser_options(false, None).unwrap();
        for (browser, executable) in [(true, None), (false, Some(Path::new("/opt/chrome")))] {
            let error = reject_browser_options(browser, executable).unwrap_err();
            assert!(error.to_string().contains("CUA"));
        }
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
        let command = if cfg!(windows) {
            "echo native-process-proof> native-proof.txt && type native-proof.txt"
        } else {
            "printf 'native-process-proof\\n' > native-proof.txt && cat native-proof.txt"
        };
        socket.send(Message::Text(json!({
            "type":"call", "session_id":"native-test-agent", "call_id":"native-file-process",
            "model":"gpt-6-astra", "name":"exec_command",
            "input":{"cmd":command},
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
            NativeState::open(workspace.path(), directory.path(), "Test host".into()).unwrap();
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
            assert!(
                second["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|tool| { tool["definition"]["name"] != "browser_execute" })
            );
            assert_eq!(second["attachment_id"], machine_id);
            assert_eq!(second["machines"][0]["id"], machine_id);
            assert_eq!(
                second["machines"][0]["capabilities"],
                json!(host::MACHINE_CAPABILITIES)
            );
            completed.recv().await.unwrap();
            assert_eq!(
                fs::read_to_string(workspace.path().join("native-proof.txt"))
                    .unwrap()
                    .trim(),
                "native-process-proof"
            );
            shutdown_tx.send(()).unwrap();
            hand.await.unwrap().unwrap();
        })
        .await
        .expect("native Hand lifecycle timed out");
        server.abort();
        let restarted =
            NativeState::open(workspace.path(), directory.path(), "Test host".into()).unwrap();
        assert_eq!(restarted.machine.id(), machine_id);
    }
}
