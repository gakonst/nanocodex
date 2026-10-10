//! Local, full-duplex client leases. No network port or account credential is
//! exposed by the transport. A disconnected client releases only its lease.
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
pub use tokio::net::UnixStream as Client;
#[cfg(windows)]
pub use tokio::net::windows::named_pipe::NamedPipeClient as Client;

pub struct Listener {
    path: PathBuf,
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: tokio::net::windows::named_pipe::NamedPipeServer,
}
impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        let inner = {
            // Caller holds the publisher's OS lock, so only a stale socket can
            // exist here. Never unlink the socket from a competing client.
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            tokio::net::UnixListener::bind(path)?
        };
        #[cfg(windows)]
        let inner = pipe(path, true)?;
        Ok(Self {
            path: path.into(),
            inner,
        })
    }
    #[cfg(unix)]
    pub async fn accept(&mut self) -> io::Result<tokio::net::UnixStream> {
        self.inner.accept().await.map(|(stream, _)| stream)
    }
    #[cfg(windows)]
    pub async fn accept(&mut self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        self.inner.connect().await?;
        // Keep an instance alive while replacing the listener, including when
        // the previous client closes: the pipe name must never be unowned.
        let next = pipe(&self.path, false)?;
        Ok(std::mem::replace(&mut self.inner, next))
    }
}
#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
#[cfg(windows)]
fn pipe(path: &Path, first: bool) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    // The default Windows pipe ACL grants full access to the creator/system;
    // other users' read-only access cannot open our required duplex client.
    // Remote clients are rejected and first-instance ownership is mandatory.
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(path)
}
pub async fn connect(path: &Path) -> io::Result<Client> {
    #[cfg(unix)]
    {
        Client::connect(path).await
    }
    #[cfg(windows)]
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new()
                .read(true)
                .write(true)
                .open(path)
            {
                Err(e)
                    if e.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                result => return result,
            }
        }
    }
}

// Existing lease clients send no bytes. A single versioned opcode requests an
// update barrier; older daemons close this stream without an acknowledgement.
pub const PREPARE_IDLE_UPDATE: u8 = 0xA1;
pub const UPDATE_PREPARED: u8 = 0xA2;
pub const UPDATE_DEFERRED: u8 = 0xA3;

pub async fn prepare_idle_update(path: &Path) -> io::Result<bool> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = connect(path).await?;
        stream.write_all(&[PREPARE_IDLE_UPDATE]).await?;
        match stream.read_u8().await? {
            UPDATE_PREPARED => Ok(true),
            UPDATE_DEFERRED => Ok(false),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Hand update acknowledgement",
            )),
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Hand update request timed out"))?
}

// Explicit user action only: ask the daemon process itself to request macOS
// consent so the OS attributes it to the executable that captures the screen.
// Reply is one bounded JSON object; older daemons close without replying.
pub const REQUEST_PERMISSIONS: u8 = 0xB1;
/// Read-only status for permission guides; never prompts or adds a TCC entry.
pub const CHECK_PERMISSIONS: u8 = 0xB2;
#[cfg(unix)]
const PERMISSIONS_REPLY_LIMIT: u64 = 16 * 1024;

/// Refuses before sending anything unless the kernel-attested socket owner is
/// `expected_pid`. The OS request is non-blocking; the bound covers a stalled peer.
#[cfg(unix)]
pub async fn permissions(
    path: &Path,
    expected_pid: u32,
    opcode: u8,
) -> io::Result<serde_json::Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = connect(path).await?;
        let peer = stream.peer_cred()?.pid();
        if peer != i32::try_from(expected_pid).ok() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "the Hand control socket is owned by PID {}, not the running service PID {expected_pid}; no permission was requested",
                    peer.map_or_else(|| "unknown".to_owned(), |peer| peer.to_string())
                ),
            ));
        }
        stream.write_all(&[opcode]).await?;
        let mut reply = Vec::new();
        (&mut stream)
            .take(PERMISSIONS_REPLY_LIMIT)
            .read_to_end(&mut reply)
            .await?;
        if reply.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the running Hand does not support permission requests",
            ));
        }
        serde_json::from_slice(&reply)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid Hand permission reply"))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Hand permission request timed out"))?
}
