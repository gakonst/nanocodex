//! Private, short-lived credential delivery to restored tmux children.
//!
//! The socket path and agent ID are public launch arguments. Credentials remain
//! in memory and never enter tmux's environment, options, commands, or files.
use clap::Args as ClapArgs;
use nanocodex_managed::ManagedError;
use std::path::{Path, PathBuf};

#[derive(ClapArgs)]
pub(super) struct Args {
    pub(super) socket: PathBuf,
    // IDs are already validated by the handoff; leading hyphens are legal.
    #[arg(allow_hyphen_values = true)]
    pub(super) agent_id: String,
}

fn failure(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}

#[cfg(unix)]
mod unix {
    use super::*;
    use nanocodex_managed::{ManagedApiKey, ManagedClient};
    use serde::{Deserialize, Serialize};
    use std::{collections::HashSet, os::unix::fs::PermissionsExt, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
        task::JoinHandle,
    };

    const MAX_RESPONSE: usize = 16 * 1024;
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    const FINISH_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Serialize)]
    struct Credentials<'a> {
        origin: &'a str,
        api_key: &'a str,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReceivedCredentials {
        origin: String,
        api_key: String,
    }

    pub(crate) struct Handoff {
        // Keep the private directory alive until its listener and children settle.
        _directory: tempfile::TempDir,
        path: PathBuf,
        task: Option<JoinHandle<Result<(), ManagedError>>>,
    }

    impl Handoff {
        /// Resolve exactly the credential selection used by the normal CLI and
        /// deliver it at most once for each expected child session.
        pub(crate) async fn start<I, S>(expected_agent_ids: I) -> Result<Self, ManagedError>
        where
            I: IntoIterator<Item = S>,
            S: AsRef<str>,
        {
            let expected: HashSet<String> = expected_agent_ids
                .into_iter()
                .map(|id| id.as_ref().to_owned())
                .collect();
            if expected
                .iter()
                .any(|id| !super::super::valid_managed_agent_id(id))
            {
                return Err(failure("Invalid session ID for credential handoff"));
            }
            let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
            let directory = tempfile::Builder::new()
                .prefix("ncx-")
                .tempdir()
                .map_err(|_| failure("Could not create private credential handoff directory"))?;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .map_err(|_| failure("Could not protect credential handoff directory"))?;
            let path = directory.path().join("auth");
            let listener = UnixListener::bind(&path)
                .map_err(|_| failure("Could not open private credential handoff socket"))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| failure("Could not protect credential handoff socket"))?;
            let task = tokio::spawn(async move {
                let mut expected = expected;
                while !expected.is_empty() {
                    let (mut stream, _) = listener
                        .accept()
                        .await
                        .map_err(|_| failure("Credential handoff listener failed"))?;
                    let requested =
                        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_agent_id(&mut stream)).await;
                    let Ok(Ok(agent_id)) = requested else {
                        // A malformed/local client must not consume a legitimate
                        // child's one-time delivery slot.
                        continue;
                    };
                    if !expected.remove(&agent_id) {
                        continue;
                    }
                    let mut response = serde_json::to_vec(&Credentials {
                        origin: &origin,
                        api_key: key.as_str(),
                    })
                    .map_err(|_| failure("Could not encode credential handoff"))?;
                    if response.len() > MAX_RESPONSE {
                        response.fill(0);
                        return Err(failure("Credential handoff response exceeds its limit"));
                    }
                    // Removal precedes writing: even an ambiguous partial write
                    // cannot lead to a second delivery for the same session.
                    let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
                        stream.write_all(&response).await?;
                        stream.shutdown().await
                    })
                    .await;
                    response.fill(0);
                    if !matches!(result, Ok(Ok(()))) {
                        return Err(failure(
                            "Credential handoff could not be confirmed; rerun continue",
                        ));
                    }
                }
                Ok(())
            });
            Ok(Self {
                _directory: directory,
                path,
                task: Some(task),
            })
        }

        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        /// Wait for credential delivery, not for the attached sessions to finish.
        /// The caller should invoke this before attaching its own tmux terminal.
        pub(crate) async fn finish(mut self) -> Result<(), ManagedError> {
            let task = self.task.as_mut().expect("handoff owns its task");
            // Keep the handle owned by self while awaiting: cancellation of
            // finish must still abort the listener through Handoff::drop.
            match tokio::time::timeout(FINISH_TIMEOUT, &mut *task).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(failure("Credential handoff stopped unexpectedly")),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Err(failure(
                        "Timed out delivering credentials to restored sessions; rerun continue",
                    ))
                }
            }
        }
    }

    impl Drop for Handoff {
        fn drop(&mut self) {
            if let Some(task) = &self.task {
                task.abort();
            }
        }
    }

    async fn read_agent_id(stream: &mut UnixStream) -> std::io::Result<String> {
        let mut bytes = Vec::with_capacity(128);
        loop {
            let byte = stream.read_u8().await?;
            if byte == b'\n' {
                let id = String::from_utf8(bytes).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid session ID")
                })?;
                if !super::super::valid_managed_agent_id(&id) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid session ID",
                    ));
                }
                return Ok(id);
            }
            if bytes.len() >= 128 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "session ID exceeds limit",
                ));
            }
            bytes.push(byte);
        }
    }

    pub(crate) async fn attach(args: Args) -> Result<(), ManagedError> {
        if !super::super::valid_managed_agent_id(&args.agent_id) {
            return Err(failure("Invalid session ID for credential handoff"));
        }
        let credentials = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let mut stream = UnixStream::connect(&args.socket)
                .await
                .map_err(|_| failure("Could not connect to private credential handoff"))?;
            stream
                .write_all(format!("{}\n", args.agent_id).as_bytes())
                .await
                .map_err(|_| failure("Could not request session credentials"))?;
            let mut response = Vec::new();
            let read = stream
                .take((MAX_RESPONSE + 1) as u64)
                .read_to_end(&mut response)
                .await;
            if read.is_err() || response.len() > MAX_RESPONSE {
                response.fill(0);
                return Err(failure("Invalid credential handoff response"));
            }
            let credentials = serde_json::from_slice::<ReceivedCredentials>(&response)
                .map_err(|_| failure("Invalid credential handoff response"));
            response.fill(0);
            credentials
        })
        .await
        .map_err(|_| failure("Timed out receiving session credentials"))??;
        // Do not consult the tmux server's inherited keys or saved-login path.
        let client = ManagedClient::new(
            &credentials.origin,
            ManagedApiKey::parse(credentials.api_key)?,
        )?;
        let device = nanocodex_bin_shared::hand_client::BackgroundHandTask::start(client.clone());
        let result = super::super::attach_tui(&client, Some(args.agent_id)).await;
        device.stop().await;
        result
    }
}

#[cfg(unix)]
pub(super) use unix::{Handoff, attach};

#[cfg(not(unix))]
pub(super) struct Handoff;

#[cfg(not(unix))]
impl Handoff {
    pub(super) async fn start<I, S>(_expected_agent_ids: I) -> Result<Self, ManagedError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Err(failure("Session credential handoff requires Unix sockets"))
    }
    pub(crate) fn path(&self) -> &Path {
        Path::new("")
    }
    pub(super) async fn finish(self) -> Result<(), ManagedError> {
        Err(failure("Session credential handoff requires Unix sockets"))
    }
}

#[cfg(not(unix))]
pub(super) async fn attach(_args: Args) -> Result<(), ManagedError> {
    Err(failure("Session credential handoff requires Unix sockets"))
}
