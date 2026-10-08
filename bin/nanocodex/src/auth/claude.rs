//! Native, private host capabilities for Rust-owned Claude subscription OAuth.
use std::{
    fs::{self, File, OpenOptions},
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use clap::Args;
use eyre::{Result, eyre};
use futures_util::StreamExt;
use nanocodex::claude::{
    ClaudeAuthFuture, ClaudeAuthProvider, ClaudeClient, SubscriptionIdentity,
    subscription::{
        ClaudeLoginMode, ClaudeSubscription, ClaudeSubscriptionCommit, ClaudeSubscriptionConfig,
        ClaudeSubscriptionHost, ClaudeSubscriptionHostError, ClaudeSubscriptionHttpRequest,
        ClaudeSubscriptionHttpResponse, ClaudeSubscriptionStatus, ClaudeSubscriptionStoreValue,
    },
};
use serde::{Deserialize, Serialize};

const STORE_KEY: &str = "claude-subscription";
const MAGIC: &[u8] = b"nanocodex-claude-auth-v1\0";
const MAX_STORE_BYTES: u64 = 128 * 1024;
type HostResult<T> = std::result::Result<T, ClaudeSubscriptionHostError>;

#[derive(Args, Clone, Default)]
pub(crate) struct ClaudeAuthArgs {
    /// Private encrypted Claude subscription store (default: CODEX_HOME/claude/private/auth).
    #[arg(long, env = "NANOCODEX_CLAUDE_AUTH_FILE", global = true)]
    claude_auth_file: Option<PathBuf>,
    /// Public ClaudeSubscriptionConfig JSON for explicit OAuth deployments.
    #[arg(long, global = true)]
    claude_oauth_config: Option<PathBuf>,
}

impl ClaudeAuthArgs {
    pub(crate) async fn client(
        self,
        api_key: Option<String>,
        messages_url: Option<String>,
    ) -> Result<ClaudeClient> {
        if let Some(api_key) = api_key {
            if api_key.trim().is_empty() {
                eyre::bail!("Claude API key must not be empty");
            }
            return Ok(ClaudeClient::new(
                reqwest::Client::new(),
                messages_url.unwrap_or_else(|| nanocodex::claude::ANTHROPIC_MESSAGES_URL.into()),
                api_key,
            ));
        }
        let (host, manager) = self.manager()?;
        let mut status = manager
            .status()
            .await
            .map_err(|error| eyre!("{error}; run `nanocodex --claude auth login` to sign in"))?;
        if status == ClaudeSubscriptionStatus::Validating {
            // A prior process may have persisted a rotated token before profile
            // validation completed. Let the manager safely resume that phase.
            manager.headers().await.map_err(|_| {
                eyre!("Claude subscription validation failed; run `nanocodex --claude auth login` to recover")
            })?;
            status = manager.status().await?;
        }
        let ClaudeSubscriptionStatus::Authenticated { account_id, .. } = status else {
            eyre::bail!(
                "Claude subscription is not authenticated; run `nanocodex --claude auth login`"
            );
        };
        let identity = SubscriptionIdentity {
            install_id: Some(host.install_id().await?),
            account_uuid: Some(account_id),
            ..SubscriptionIdentity::default()
        };
        let provider = Arc::new(manager);
        let http = subscription_http()?;
        let client = match messages_url {
            Some(endpoint) => ClaudeClient::with_auth_provider(http, endpoint, provider)
                .subscription_compatibility(),
            None => ClaudeClient::subscription(http, provider),
        };
        Ok(client.with_subscription_identity(identity))
    }

    /// Whether a local Claude subscription store exists; validity is checked on use.
    pub(crate) fn has_saved_credentials(&self) -> bool {
        match &self.claude_auth_file {
            Some(path) => !path.as_os_str().is_empty() && path.is_file(),
            None => crate::config::default_codex_home()
                .is_ok_and(|home| home.join("claude/private/auth").is_file()),
        }
    }

    fn manager(self) -> Result<(Arc<NativeHost>, ClaudeSubscription)> {
        let path = match self.claude_auth_file {
            Some(path) if !path.as_os_str().is_empty() => path,
            Some(_) => eyre::bail!("--claude-auth-file must not be empty"),
            None => crate::config::default_codex_home()?.join("claude/private/auth"),
        };
        let config = self
            .claude_oauth_config
            .as_deref()
            .map(read_public_config)
            .transpose()?
            .unwrap_or_default();
        let host = Arc::new(NativeHost {
            path,
            http: subscription_http()?,
        });
        let manager = ClaudeSubscription::new(host.clone(), STORE_KEY, config)?;
        Ok((host, manager))
    }

    pub(super) async fn login(self, open_automatically: bool) -> Result<()> {
        let (_, manager) = self.manager()?;
        let login = manager.begin_login(ClaudeLoginMode::Manual).await?;
        eprintln!(
            "Open this URL to sign in with Claude:\n\n{}\n",
            login.authorization_url
        );
        if open_automatically && super::open_browser(&login.authorization_url).is_err() {
            eprintln!("Could not open a browser automatically. Open the URL above manually.");
        }
        eprintln!("Paste the returned code#state and press Enter (input is hidden):");
        let code = tokio::task::spawn_blocking(read_login_code)
            .await
            .map_err(|_| eyre!("could not read Claude login response"))??;
        let status = manager.complete_login(&code).await?;
        println!("{}", serde_json::to_string(&status)?);
        Ok(())
    }

    pub(super) async fn status(self) -> Result<()> {
        let (_, manager) = self.manager()?;
        println!("{}", serde_json::to_string(&manager.status().await?)?);
        Ok(())
    }

    pub(super) async fn logout(self) -> Result<()> {
        let (_, manager) = self.manager()?;
        manager.logout().await?;
        println!("Logged out of Claude subscription.");
        Ok(())
    }
}

fn subscription_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| eyre!("could not initialize Claude subscription HTTP host"))
}

fn read_public_config(path: &Path) -> Result<ClaudeSubscriptionConfig> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| eyre!("could not read public Claude OAuth configuration"))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        eyre::bail!("public Claude OAuth configuration is too large");
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| eyre!("invalid public Claude OAuth configuration JSON"))
}

fn read_login_code() -> Result<String> {
    const MAX_CODE: usize = 8192;
    let mut value = String::new();
    if std::io::stdin().is_terminal() {
        use crossterm::{
            event::{self, Event, KeyCode, KeyModifiers},
            terminal,
        };
        struct RestoreTerminal;
        impl Drop for RestoreTerminal {
            fn drop(&mut self) {
                let _ = terminal::disable_raw_mode();
            }
        }
        terminal::enable_raw_mode().map_err(|_| eyre!("could not hide Claude login input"))?;
        let _restore = RestoreTerminal;
        loop {
            match event::read().map_err(|_| eyre!("could not read Claude login input"))? {
                Event::Key(key) if key.kind != event::KeyEventKind::Release => match key.code {
                    KeyCode::Enter => break,
                    KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        eyre::bail!("Claude login input cancelled");
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.push(character)
                    }
                    _ => {}
                },
                Event::Paste(paste) => value.push_str(&paste),
                _ => {}
            }
            if value.len() > MAX_CODE {
                eyre::bail!("Claude login input is too large");
            }
        }
        eprintln!();
    } else {
        let mut input = std::io::stdin().lock();
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => bytes.push(byte[0]),
                Err(_) => eyre::bail!("could not read Claude login input"),
            }
            if bytes.len() > MAX_CODE {
                eyre::bail!("Claude login input is too large");
            }
        }
        value = String::from_utf8(bytes).map_err(|_| eyre!("invalid Claude login input"))?;
    }
    Ok(value.trim().to_owned())
}

#[derive(Clone)]
struct NativeHost {
    path: PathBuf,
    http: reqwest::Client,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PrivateStore {
    revision: u64,
    install_id: String,
    payload: Option<String>,
}

struct StoreLock(File);

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl NativeHost {
    async fn install_id(&self) -> Result<String> {
        let host = self.clone();
        tokio::task::spawn_blocking(move || host.with_store(|store| Ok(store.install_id.clone())))
            .await
            .map_err(|_| eyre!("Claude subscription host failed"))?
            .map_err(Into::into)
    }

    fn with_store<T>(
        &self,
        action: impl FnOnce(&mut PrivateStore) -> HostResult<T>,
    ) -> HostResult<T> {
        let directory = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        private_directory(directory)?;
        let key_path = self.path.with_added_extension("key");
        let lock_path = self.path.with_added_extension("lock");
        let lock = private_open(&lock_path, true)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(|_| ClaudeSubscriptionHostError)?;
        let _lock = StoreLock(lock);
        // RAII releases this lock on every return; the OS releases it on process
        // exit. First key publication uses the same lock as all loads and commits.
        let key = match private_read(&key_path, 32)? {
            Some(bytes) if bytes.len() == 32 => bytes,
            Some(_) => return Err(ClaudeSubscriptionHostError),
            None => {
                if fs::symlink_metadata(&self.path).is_ok() {
                    return Err(ClaudeSubscriptionHostError);
                }
                let key: [u8; 32] = rand::random();
                atomic_private_write(&key_path, &key)?;
                key.to_vec()
            }
        };
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| ClaudeSubscriptionHostError)?;
        let stored = private_read(&self.path, MAX_STORE_BYTES)?;
        let is_new = stored.is_none();
        let mut store = match stored {
            Some(bytes) => {
                if !bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() + 12 + 16 {
                    return Err(ClaudeSubscriptionHostError);
                }
                let plaintext = cipher
                    .decrypt(
                        Nonce::from_slice(&bytes[MAGIC.len()..MAGIC.len() + 12]),
                        Payload {
                            msg: &bytes[MAGIC.len() + 12..],
                            aad: MAGIC,
                        },
                    )
                    .map_err(|_| ClaudeSubscriptionHostError)?;
                let store: PrivateStore =
                    serde_json::from_slice(&plaintext).map_err(|_| ClaudeSubscriptionHostError)?;
                if store.install_id.is_empty() || store.install_id.len() > 128 {
                    return Err(ClaudeSubscriptionHostError);
                }
                store
            }
            None => PrivateStore {
                revision: 0,
                install_id: uuid::Uuid::new_v4().to_string(),
                payload: None,
            },
        };
        let prior_revision = store.revision;
        let result = action(&mut store)?;
        if is_new || store.revision != prior_revision {
            let plaintext = serde_json::to_vec(&store).map_err(|_| ClaudeSubscriptionHostError)?;
            let nonce: [u8; 12] = rand::random();
            let encrypted = cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &plaintext,
                        aad: MAGIC,
                    },
                )
                .map_err(|_| ClaudeSubscriptionHostError)?;
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(&nonce);
            bytes.extend_from_slice(&encrypted);
            if bytes.len() as u64 > MAX_STORE_BYTES {
                return Err(ClaudeSubscriptionHostError);
            }
            atomic_private_write(&self.path, &bytes)?;
        }
        Ok(result)
    }
}

impl ClaudeSubscriptionHost for NativeHost {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> ClaudeAuthFuture<'a, HostResult<ClaudeSubscriptionStoreValue>> {
        Box::pin(async move {
            if key != STORE_KEY {
                return Err(ClaudeSubscriptionHostError);
            }
            let host = self.clone();
            tokio::task::spawn_blocking(move || {
                host.with_store(|store| {
                    Ok(ClaudeSubscriptionStoreValue {
                        revision: store.revision,
                        payload: store.payload.clone(),
                    })
                })
            })
            .await
            .map_err(|_| ClaudeSubscriptionHostError)?
        })
    }

    fn compare_and_swap<'a>(
        &'a self,
        key: &'a str,
        expected_revision: u64,
        payload: &'a str,
    ) -> ClaudeAuthFuture<'a, HostResult<ClaudeSubscriptionCommit>> {
        Box::pin(async move {
            if key != STORE_KEY || payload.len() > 64 * 1024 {
                return Err(ClaudeSubscriptionHostError);
            }
            let host = self.clone();
            let payload = payload.to_owned();
            tokio::task::spawn_blocking(move || {
                host.with_store(|store| {
                    if store.revision != expected_revision {
                        return Ok(ClaudeSubscriptionCommit::Conflict(store.revision));
                    }
                    store.revision = store
                        .revision
                        .checked_add(1)
                        .ok_or(ClaudeSubscriptionHostError)?;
                    store.payload = Some(payload);
                    Ok(ClaudeSubscriptionCommit::Committed(store.revision))
                })
            })
            .await
            .map_err(|_| ClaudeSubscriptionHostError)?
        })
    }

    fn request(
        &self,
        request: ClaudeSubscriptionHttpRequest,
    ) -> ClaudeAuthFuture<'_, HostResult<ClaudeSubscriptionHttpResponse>> {
        Box::pin(async move {
            let deadline = Duration::from_millis(request.timeout_millis());
            tokio::time::timeout(deadline, async {
                let method = reqwest::Method::from_bytes(request.method().as_bytes())
                    .map_err(|_| ClaudeSubscriptionHostError)?;
                let response = self
                    .http
                    .request(method, request.url())
                    .headers(request.headers().clone())
                    .header(reqwest::header::CONTENT_TYPE, request.content_type())
                    .body(request.body().to_owned())
                    .timeout(deadline)
                    .send()
                    .await
                    .map_err(|_| ClaudeSubscriptionHostError)?;
                let status = response.status().as_u16();
                let bound = request.max_response_bytes();
                if response
                    .content_length()
                    .is_some_and(|length| length > bound as u64)
                {
                    return Err(ClaudeSubscriptionHostError);
                }
                let mut stream = response.bytes_stream();
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| ClaudeSubscriptionHostError)?;
                    if chunk.len() > bound.saturating_sub(bytes.len()) {
                        return Err(ClaudeSubscriptionHostError);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let body = String::from_utf8(bytes).map_err(|_| ClaudeSubscriptionHostError)?;
                Ok(ClaudeSubscriptionHttpResponse { status, body })
            })
            .await
            .map_err(|_| ClaudeSubscriptionHostError)?
        })
    }
}

fn private_directory(path: &Path) -> HostResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| ClaudeSubscriptionHostError)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| ClaudeSubscriptionHostError)?;
    if !metadata.is_dir() {
        return Err(ClaudeSubscriptionHostError);
    }
    check_private(&metadata)
}

fn check_private(_metadata: &fs::Metadata) -> HostResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if _metadata.permissions().mode() & 0o077 != 0
            || _metadata.uid() != nix::unistd::geteuid().as_raw()
        {
            return Err(ClaudeSubscriptionHostError);
        }
    }
    Ok(())
}

fn private_open(path: &Path, create: bool) -> HostResult<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(ClaudeSubscriptionHostError);
            }
            check_private(&metadata)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {}
        Err(_) => return Err(ClaudeSubscriptionHostError),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| ClaudeSubscriptionHostError)?;
    let metadata = file.metadata().map_err(|_| ClaudeSubscriptionHostError)?;
    if !metadata.is_file() {
        return Err(ClaudeSubscriptionHostError);
    }
    check_private(&metadata)?;
    Ok(file)
}

fn private_read(path: &Path, max_bytes: u64) -> HostResult<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ClaudeSubscriptionHostError),
        Ok(metadata) if !metadata.is_file() => return Err(ClaudeSubscriptionHostError),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    private_open(path, false)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ClaudeSubscriptionHostError)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ClaudeSubscriptionHostError);
    }
    Ok(Some(bytes))
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> HostResult<()> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Refuse existing symlinks/public files, including when replacing a store.
    if fs::symlink_metadata(path).is_ok() {
        let _ = private_open(path, false)?;
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| ClaudeSubscriptionHostError)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ClaudeSubscriptionHostError)?;
    }
    temporary
        .write_all(bytes)
        .map_err(|_| ClaudeSubscriptionHostError)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| ClaudeSubscriptionHostError)?;
    temporary
        .persist(path)
        .map_err(|_| ClaudeSubscriptionHostError)?;
    #[cfg(unix)]
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| ClaudeSubscriptionHostError)?;
    Ok(())
}
