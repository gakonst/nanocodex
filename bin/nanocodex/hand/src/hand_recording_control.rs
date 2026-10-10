//! Owner-local recording control. One length-prefixed JSON request per connection.
//! This endpoint is independent of account authentication and network availability.
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, bail};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const REQUEST_LIMIT: usize = 16 * 1024;
const RESPONSE_LIMIT: usize = 600 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONCURRENCY: usize = 8;
pub(crate) type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, Value> + Send + Sync>;

/// Run the recorder or send it one control request.
pub(crate) async fn run(args: &nanocodex_bin_shared::hand_args::HandRecordingArgs) -> Result<()> {
    if args.desktop_runtime.is_some() && !args.serve {
        bail!("desktop_runtime_requires_serve");
    }
    if args.serve {
        return super::hand_recording::serve(args.state_dir.clone(), args.desktop_runtime.clone())
            .await;
    }
    let request = args
        .request
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("invalid_control_request"))?;
    if request.len() > REQUEST_LIMIT {
        bail!("request_too_large");
    }
    let request: Value =
        serde_json::from_str(request).map_err(|_| anyhow::anyhow!("invalid_json"))?;
    let response = request_local(&args.state_dir, request).await?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

pub(crate) struct ControlGuard {
    task: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    socket: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    _lock: nix::fcntl::Flock<std::fs::File>,
}
impl Drop for ControlGuard {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Never remove a replacement installed after this server started.
            if std::fs::symlink_metadata(&self.socket)
                .is_ok_and(|m| (m.dev(), m.ino()) == self.identity)
            {
                let _ = std::fs::remove_file(&self.socket);
            }
        }
    }
}

pub(crate) fn validate_root(root: &Path, create: bool) -> Result<PathBuf> {
    // Inherited Windows ACLs do not establish owner-private evidence storage.
    // Reject before creating directories; native screen access remains available.
    if cfg!(windows) {
        bail!("private_recording_storage_unavailable");
    }
    let root = if root.is_absolute() {
        root.to_owned()
    } else {
        std::env::current_dir()?.join(root)
    };
    // Reject symlinks in every component, including a missing leaf's ancestors.
    let mut prefix = PathBuf::new();
    for component in root.components() {
        if matches!(component, std::path::Component::ParentDir) {
            bail!("unsafe_state_directory");
        }
        prefix.push(component);
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!("unsafe_state_directory")
            }
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        bail!("unsafe_state_directory");
                    }
                }
                #[cfg(not(windows))]
                let _ = metadata;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                let builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    builder
                };
                builder.create(&prefix)?;
            }
            Err(_) => bail!("state_directory_unavailable"),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(&root)?;
        if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            bail!("unsafe_state_directory");
        }
    }
    std::fs::canonicalize(root).map_err(|_| anyhow::anyhow!("state_directory_unavailable"))
}

#[cfg(unix)]
fn socket_path(root: &Path) -> PathBuf {
    root.join("control.sock")
}
#[cfg(windows)]
fn socket_path(root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    use std::os::windows::ffi::OsStrExt;
    // Machine/root scope, without disclosing the private directory in the name.
    let mut digest = Sha256::new();
    for word in root.as_os_str().encode_wide() {
        digest.update(word.to_le_bytes());
    }
    digest.update(std::env::var("COMPUTERNAME").unwrap_or_default().as_bytes());
    PathBuf::from(format!(
        r"\\.\pipe\nanocodex-recording-{}",
        hex::encode(digest.finalize())
    ))
}

#[cfg(unix)]
fn check_socket(path: &Path) -> Result<std::fs::Metadata> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| anyhow::anyhow!("control_unavailable"))?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        bail!("unsafe_control_socket");
    }
    Ok(metadata)
}

/// Start once per recorder. Dropping the guard cancels all accepted requests.
pub(crate) async fn start(root: &Path, handler: Handler) -> Result<ControlGuard> {
    let root = validate_root(root, true)?;
    let path = socket_path(&root);
    #[cfg(unix)]
    let (listener, lock, identity) = {
        use nix::fcntl::{Flock, FlockArg};
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(root.join("control.lock"))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            bail!("unsafe_control_lock");
        }
        let lock = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|_| anyhow::anyhow!("control_already_running"))?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                check_socket(&path)?;
                // Also protect active endpoints created without our lock.
                match tokio::time::timeout(
                    Duration::from_millis(250),
                    tokio::net::UnixStream::connect(&path),
                )
                .await
                {
                    Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => (),
                    _ => bail!("control_already_running"),
                }
                std::fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => bail!("control_unavailable"),
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        if std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).is_err() {
            let _ = std::fs::remove_file(&path);
            bail!("control_unavailable");
        }
        let metadata = check_socket(&path)?;
        (listener, lock, (metadata.dev(), metadata.ino()))
    };
    #[cfg(windows)]
    let mut listener = pipe(&path, true)?;
    let task = tokio::spawn(async move {
        // JoinSet cancels children when the server task is aborted.
        let mut requests = tokio::task::JoinSet::new();
        loop {
            if requests.len() >= CONCURRENCY {
                requests.join_next().await;
            }
            #[cfg(unix)]
            let accepted = listener.accept().await.map(|(stream, _)| stream);
            #[cfg(windows)]
            let accepted = async {
                listener.connect().await?;
                let next = pipe(&path, false)?;
                Ok::<_, io::Error>(std::mem::replace(&mut listener, next))
            }
            .await;
            let Ok(stream) = accepted else {
                break;
            };
            #[cfg(unix)]
            if !stream
                .peer_cred()
                .is_ok_and(|peer| peer.uid() == nix::unistd::geteuid().as_raw())
            {
                continue;
            }
            let handler = handler.clone();
            requests.spawn(async move {
                let _ = tokio::time::timeout(REQUEST_TIMEOUT, serve(stream, handler)).await;
            });
            while requests.try_join_next().is_some() {}
        }
    });
    Ok(ControlGuard {
        task,
        #[cfg(unix)]
        socket: path,
        #[cfg(unix)]
        identity,
        #[cfg(unix)]
        _lock: lock,
    })
}

#[cfg(windows)]
fn pipe(path: &Path, first: bool) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    // Match device_hand/transport.rs: the creator/system have full access;
    // other users' default read-only access cannot open this duplex protocol.
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(path)
}

async fn read_frame(stream: &mut (impl AsyncRead + Unpin), limit: usize) -> Result<Value> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > limit {
        bail!("frame_too_large");
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("invalid_json"))
}
async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &Value,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > limit {
        bail!("frame_too_large");
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
async fn serve(mut stream: impl AsyncRead + AsyncWrite + Unpin, handler: Handler) -> Result<()> {
    let reply = match read_frame(&mut stream, REQUEST_LIMIT).await {
        Ok(request) if request.is_object() => handler(request).await,
        _ => json!({"ok": false, "error": "invalid_control_request"}),
    };
    let reply = if serde_json::to_vec(&reply)?.len() > RESPONSE_LIMIT {
        json!({"ok": false, "error": "control_response_too_large"})
    } else {
        reply
    };
    write_frame(&mut stream, &reply, RESPONSE_LIMIT).await
}

/// Send a local request. Errors are stable codes and never contain evidence paths.
pub(crate) async fn request_local(root: &Path, request: Value) -> Result<Value> {
    if !request.is_object() {
        bail!("invalid_control_request");
    }
    if serde_json::to_vec(&request)?.len() > REQUEST_LIMIT {
        bail!("request_too_large");
    }
    let root = validate_root(root, false)?;
    let path = socket_path(&root);
    #[cfg(unix)]
    check_socket(&path)?;
    tokio::time::timeout(REQUEST_TIMEOUT + Duration::from_secs(2), async {
        #[cfg(unix)]
        let mut stream = tokio::net::UnixStream::connect(&path).await?;
        #[cfg(unix)]
        if !stream
            .peer_cred()
            .is_ok_and(|peer| peer.uid() == nix::unistd::geteuid().as_raw())
        {
            bail!("control_owner_mismatch");
        }
        #[cfg(windows)]
        let mut stream = {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            loop {
                match tokio::net::windows::named_pipe::ClientOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                {
                    Err(error)
                        if error.raw_os_error() == Some(231)
                            && tokio::time::Instant::now() < deadline =>
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    result => break result?,
                }
            }
        };
        write_frame(&mut stream, &request, REQUEST_LIMIT).await?;
        read_frame(&mut stream, RESPONSE_LIMIT).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("control_timeout"))?
    .map_err(|_| anyhow::anyhow!("control_unavailable"))
}

#[cfg(all(test, unix))]
mod journeys {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    // Exercise the public server/client boundary with synthetic recorder state.
    #[tokio::test]
    async fn local_controls_recovery_and_untrusted_requests() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?.join("recordings");
        let state = Arc::new(std::sync::Mutex::new("stopped"));
        let handler: Handler = Arc::new(move |request| {
            let state = state.clone();
            Box::pin(async move {
                let operation = request["operation"].as_str().unwrap_or("");
                if operation == "oversize" {
                    return json!({"data": "x".repeat(RESPONSE_LIMIT)});
                }
                if operation == "wait" {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                let mut state = state.lock().unwrap();
                match operation {
                    "start" => *state = "recording",
                    "pause" => *state = "paused",
                    "stop" => *state = "stopped",
                    _ => (),
                }
                json!({"state": *state, "active": *state == "recording"})
            })
        });
        let guard = start(&root, handler.clone()).await?;
        for (operation, expected, active) in [
            ("status", "stopped", false),
            ("start", "recording", true),
            ("pause", "paused", false),
            ("stop", "stopped", false),
        ] {
            let response = request_local(&root, json!({"operation": operation})).await?;
            assert_eq!(response, json!({"state": expected, "active": active}));
            eprintln!("IPC {operation}: {response}");
        }
        assert!(start(&root, handler.clone()).await.is_err());
        assert_eq!(
            request_local(&root, json!({"operation": "status"})).await?["state"],
            "stopped"
        );
        assert_eq!(
            std::fs::metadata(root.join("control.sock"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            request_local(&root, json!({"data": "x".repeat(REQUEST_LIMIT)}))
                .await
                .is_err()
        );
        // A malicious peer bypasses client validation and sends only a huge header.
        let mut stream = tokio::net::UnixStream::connect(root.join("control.sock")).await?;
        stream.write_u32(u32::MAX).await?;
        let size = stream.read_u32().await? as usize;
        assert!(size < 100);
        let mut reply = vec![0; size];
        stream.read_exact(&mut reply).await?;
        assert_eq!(
            serde_json::from_slice::<Value>(&reply)?["error"],
            "invalid_control_request"
        );
        assert_eq!(
            request_local(&root, json!({"operation": "oversize"})).await?["error"],
            "control_response_too_large"
        );
        eprintln!(
            "IPC bounds: oversized request rejected before allocation; oversized response replaced with bounded error"
        );
        // All slots become stalled clients; timeout must return capacity to status.
        let mut stalled = Vec::new();
        for _ in 0..CONCURRENCY {
            stalled.push(tokio::net::UnixStream::connect(root.join("control.sock")).await?);
        }
        let response = request_local(&root, json!({"operation": "status"})).await?;
        assert_eq!(response["state"], "stopped");
        eprintln!("IPC timeout: status recovers after eight stalled peers expire");
        drop(stalled);
        drop(guard);
        assert!(!root.join("control.sock").exists());
        // A crashed server leaves a socket; restart safely removes it under the lock.
        let stale = std::os::unix::net::UnixListener::bind(root.join("control.sock"))?;
        std::fs::set_permissions(
            root.join("control.sock"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        drop(stale);
        let guard = start(&root, handler.clone()).await?;
        assert_eq!(
            request_local(&root, json!({"operation": "status"})).await?["state"],
            "stopped"
        );
        drop(guard);
        let target = root.join("evidence-do-not-touch");
        std::fs::write(&target, "sentinel")?;
        symlink(&target, root.join("control.sock"))?;
        assert!(start(&root, handler.clone()).await.is_err());
        assert_eq!(std::fs::read_to_string(&target)?, "sentinel");
        let alias = temporary.path().canonicalize()?.join("alias");
        symlink(&root, &alias)?;
        assert!(start(&alias, handler.clone()).await.is_err());
        std::fs::remove_file(root.join("control.sock"))?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;
        assert!(start(&root, handler).await.is_err());
        eprintln!(
            "IPC recovery: competing server rejected, shutdown cleaned socket, stale socket recovered, symlink/public-root refused"
        );
        Ok(())
    }
}
