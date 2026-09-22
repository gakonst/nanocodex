//! Local, full-duplex client leases. No network port or account credential is
//! exposed by the transport. A disconnected client releases only its lease.
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
pub(super) use tokio::net::UnixStream as Client;
#[cfg(windows)]
pub(super) use tokio::net::windows::named_pipe::NamedPipeClient as Client;

pub(super) struct Listener {
    path: PathBuf,
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: tokio::net::windows::named_pipe::NamedPipeServer,
}
impl Listener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
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
    pub(super) async fn accept(&mut self) -> io::Result<tokio::net::UnixStream> {
        self.inner.accept().await.map(|(stream, _)| stream)
    }
    #[cfg(windows)]
    pub(super) async fn accept(
        &mut self,
    ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
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
pub(super) async fn connect(path: &Path) -> io::Result<Client> {
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
pub(super) const PREPARE_IDLE_UPDATE: u8 = 0xA1;
pub(super) const UPDATE_PREPARED: u8 = 0xA2;
pub(super) const UPDATE_DEFERRED: u8 = 0xA3;

pub(super) async fn prepare_idle_update(path: &Path) -> io::Result<bool> {
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
