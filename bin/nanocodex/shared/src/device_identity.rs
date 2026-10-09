//! Hand device identity (interface contract v1).
//!
//! A Hand proves it is enrolled device D of account A with an Ed25519 key that
//! never leaves this machine. The key only obtains short-lived, Hand-only
//! publication credentials (`ncxhd1.*`). It is never an SSH client key and
//! never confers account authority.
//!
//! Files, all inside the private (0700, owner-only) Hand state directory:
//! - `device-key.v1`: PKCS#8 v2 Ed25519 private key, mode 0600, created with
//!   `O_CREAT|O_EXCL|O_NOFOLLOW` and fsynced; type, owner and mode are checked
//!   on every load and symbolic links are refused.
//! - `device-key.v1.pending`: the pending replacement during rotation.
//! - `device.json`: non-secret enrollment state.
//! - `device.lock`: serializes first run, credential issue, attestation and
//!   rotation across processes (daemon and CLI).
//!
//! Rotation is crash safe. The new key is durably written to `.pending`
//! before the service is asked to rotate and becomes `device-key.v1` by
//! atomic rename only after the service confirms. Every device challenge
//! reports the service's current key fingerprint and version. When it names
//! the pending key, the service accepted the rotation (the response was lost or
//! the process died before promotion), so the pending key is promoted. When it
//! names the current key, the rotation never landed; the pending key is kept
//! and reused by the next rotation attempt (never by credential issue), which
//! stays correct even if a timed-out rotation request is applied late.
//!
//! Once `device.json` exists this Hand never authenticates publication with
//! the account API key again. A legacy account-key Hand remains possible only
//! for an install that never enrolled, against a service without the
//! enrollment route (HTTP 404).
use std::{
    fmt, fs,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use futures_util::future::BoxFuture;
use nanocodex_managed::ManagedError;
use nanocodex_oai_tools::attachment::{AttachmentCredentials, AttachmentError, AttachmentTarget};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair as _},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use url::Url;
use zeroize::Zeroizing;

const KEY_FILE: &str = "device-key.v1";
const NEXT_KEY_FILE: &str = "device-key.v1.pending";
const STATE_FILE: &str = "device.json";
const LOCK_FILE: &str = "device.lock";
const DOMAIN: &str = "nanocodex-hand-device:v1";
const MAX_KEY_BYTES: u64 = 512;
const MAX_STATE_BYTES: u64 = 16 * 1024;
const MAX_HOST_KEY_BYTES: u64 = 16 * 1024;
const MAX_HOST_KEYS: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_MARGIN: Duration = Duration::from_secs(60);
/// Logged whenever a never-enrolled Hand publishes with the account API key.
pub const LEGACY_WARNING: &str = "legacy account API key Hand authentication";
const REENROLL_MESSAGE: &str = "This Hand's device enrollment was revoked or is no longer valid (hand_reenroll_required). It will not fall back to the account API key. Run `nanocodex hand devices reenroll` on this computer, then restart the Hand to enroll it again.";

/// Failure classes. Messages are actionable and never contain credentials,
/// challenges, signatures or key material.
#[derive(Debug)]
pub enum DeviceError {
    /// The service no longer accepts this device; publishing must stop.
    Reenroll,
    /// A permanent rejection or local misconfiguration.
    Fatal(String),
    /// Retry with backoff.
    Transient(String),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reenroll => formatter.write_str(REENROLL_MESSAGE),
            Self::Fatal(message) | Self::Transient(message) => formatter.write_str(message),
        }
    }
}

impl DeviceError {
    const fn reason(&self) -> &'static str {
        match self {
            Self::Reenroll => "reenroll_required",
            Self::Fatal(_) => "rejected",
            Self::Transient(_) => "transient",
        }
    }
    fn into_attachment(self) -> AttachmentError {
        match self {
            Self::Transient(message) => AttachmentError::Transport(message.into()),
            error => AttachmentError::Authentication(error.to_string().into()),
        }
    }
    fn into_io(self) -> io::Error {
        match self {
            Self::Transient(message) => io::Error::other(message),
            error => io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()),
        }
    }
}

impl From<DeviceError> for ManagedError {
    fn from(error: DeviceError) -> Self {
        Self::Configuration(error.to_string())
    }
}

fn fatal(message: impl Into<String>) -> DeviceError {
    DeviceError::Fatal(message.into())
}

fn local(context: &str, error: &io::Error) -> DeviceError {
    fatal(format!("{context}: {error}"))
}

/// OpenSSH-style `SHA256:` fingerprint of raw public key bytes.
pub fn fingerprint(raw: &[u8]) -> String {
    format!(
        "SHA256:{}",
        STANDARD_NO_PAD.encode(Sha256::digest(raw).as_slice())
    )
}

/// An Ed25519 device key. Debug output shows only the public fingerprint.
pub struct DeviceKey {
    pair: Ed25519KeyPair,
}

impl DeviceKey {
    fn from_pkcs8(bytes: &[u8]) -> Result<Self, DeviceError> {
        Ed25519KeyPair::from_pkcs8(bytes)
            .map(|pair| Self { pair })
            .map_err(|_| {
                fatal("The Hand device key file is invalid; run `nanocodex hand devices reenroll`")
            })
    }
    fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref())
    }
    fn fingerprint(&self) -> String {
        fingerprint(self.pair.public_key().as_ref())
    }
    fn sign(&self, message: &str) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.sign(message.as_bytes()).as_ref())
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceKey")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

/// Domain-separated signed message: fields joined by newlines, no trailing newline.
fn message(fields: &[&str]) -> String {
    let mut message = String::from(DOMAIN);
    for field in fields {
        message.push('\n');
        message.push_str(field);
    }
    message
}

/// Non-secret enrollment state stored in `device.json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceState {
    pub owner_id: String,
    pub device_id: String,
    pub machine_id: String,
    pub key_version: u64,
    pub fingerprint: String,
    pub origin: String,
    /// Host key fingerprints most recently attested to the service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_host_keys: Vec<String>,
}

fn valid_owner(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}

fn valid_device_id(value: &str) -> bool {
    value.len() == 36
        && uuid::Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 4)
        && value == value.to_ascii_lowercase()
}

#[cfg(unix)]
fn euid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

fn check_private_directory(directory: &Path) -> Result<(), DeviceError> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| local("Cannot inspect the Hand state directory", &error))?;
    if !metadata.is_dir() {
        return Err(fatal(
            "The Hand state directory must be a real directory, not a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != euid() || metadata.mode() & 0o077 != 0 {
            return Err(fatal(
                "The Hand state directory must be owned by this user and private (0700)",
            ));
        }
    }
    Ok(())
}

fn create_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        let _ = fs::remove_file(path);
    }
    written
}

fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

fn read_private_file(path: &Path, max: u64) -> Result<Option<Zeroizing<Vec<u8>>>, DeviceError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Hand device file");
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(nix::libc::ELOOP) => {
            return Err(fatal(format!(
                "Refusing to load {name}: it is a symbolic link"
            )));
        }
        Err(error) => return Err(local(&format!("Cannot open {name}"), &error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| local(&format!("Cannot inspect {name}"), &error))?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(fatal(format!(
            "Refusing to load {name}: it is not a regular file of the expected size"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != euid() || metadata.mode() & 0o077 != 0 {
            return Err(fatal(format!(
                "Refusing to load {name}: it must be owned by this user with mode 0600"
            )));
        }
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| local(&format!("Cannot read {name}"), &error))?;
    if bytes.len() as u64 > max {
        return Err(fatal(format!("Refusing to load {name}: it is too large")));
    }
    Ok(Some(bytes))
}

/// Atomically replace `name`: exclusive 0600 temp file, fsync, rename, fsync directory.
fn replace_private_file(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = directory.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    create_private_file(&temporary, bytes)?;
    if let Err(error) = fs::rename(&temporary, directory.join(name)) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_directory(directory)
}

/// Exclusive advisory lock on `device.lock`, released on drop or process exit.
struct DeviceLock(#[allow(dead_code)] fs::File);

async fn lock(directory: &Path) -> Result<DeviceLock, DeviceError> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let file = options
        .open(directory.join(LOCK_FILE))
        .map_err(|error| local("Cannot open the Hand device lock", &error))?;
    let file = tokio::task::spawn_blocking(move || file.lock().map(|()| file))
        .await
        .map_err(|_| fatal("The Hand device lock task failed"))?
        .map_err(|error| local("Cannot lock the Hand device state", &error))?;
    Ok(DeviceLock(file))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

/// Accepts epoch milliseconds, epoch seconds, or an RFC 3339 timestamp.
fn expires_at_ms(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64().map(|value| {
            if value < 100_000_000_000 {
                value.saturating_mul(1000)
            } else {
                value
            }
        }),
        Value::String(text) => chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .and_then(|time| u64::try_from(time.timestamp_millis()).ok()),
        _ => None,
    }
}

fn error_code(body: &Value) -> &str {
    let code = body["error"]
        .as_str()
        .or_else(|| body["error"]["code"].as_str())
        .or_else(|| body["code"].as_str())
        .unwrap_or("");
    if code.len() <= 64 && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        code
    } else {
        ""
    }
}

struct Reply {
    status: u16,
    body: Value,
    /// The service implements Hand devices (`x-nanocodex-hand-devices`).
    capable: bool,
}

/// The service's current view of this device's key, from a challenge response.
struct Service {
    key_version: u64,
    fingerprint: Option<String>,
}

impl Reply {
    const fn success(&self) -> bool {
        matches!(self.status, 200..=299)
    }
    fn describe(&self) -> String {
        match error_code(&self.body) {
            "" => format!("HTTP {}", self.status),
            code => format!("HTTP {} {code}", self.status),
        }
    }
    /// Challenges are opaque service tokens; only their shape is checked.
    fn challenge(&self) -> Result<String, DeviceError> {
        self.body["challenge"]
            .as_str()
            .filter(|value| {
                (16..=512).contains(&value.len())
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
            .map(str::to_owned)
            .ok_or_else(|| fatal("The managed service returned an invalid Hand device challenge"))
    }
    fn service(&self) -> Result<Service, DeviceError> {
        Ok(Service {
            key_version: self.key_version()?,
            fingerprint: self.body["fingerprint"].as_str().map(str::to_owned),
        })
    }
    fn key_version(&self) -> Result<u64, DeviceError> {
        self.body["key_version"]
            .as_u64()
            .filter(|version| *version >= 1)
            .ok_or_else(|| fatal("The managed service returned an invalid Hand device key version"))
    }
}

/// Classify a failed device-route response.
fn device_failure(reply: &Reply, action: &str) -> DeviceError {
    match (reply.status, error_code(&reply.body)) {
        (401 | 403, "hand_reenroll_required") => DeviceError::Reenroll,
        (401, "challenge_invalid") => DeviceError::Transient(format!(
            "Hand device {action} challenge expired before use; retrying"
        )),
        (404, _) => fatal(
            "The managed service does not offer Hand device credentials (HTTP 404). This enrolled Hand will not fall back to the account API key; update the service or run `nanocodex hand devices reenroll`.",
        ),
        (408 | 429 | 500..=599, _) => DeviceError::Transient(format!(
            "Hand device {action} failed ({}); retrying",
            reply.describe()
        )),
        _ => fatal(format!(
            "Hand device {action} was rejected ({}). Run `nanocodex hand devices` to inspect this device.",
            reply.describe()
        )),
    }
}

/// How a new device enrolls. Its bearer (account API key or one-time server
/// bootstrap grant) is used only for enrollment, never for publication.
pub enum EnrollRoute {
    /// Challenge then enroll with the account login.
    Account {
        challenges: String,
        enroll: String,
        bearer: Zeroizing<String>,
    },
    /// Server Hand bootstrap: a one-time `ncxhg1.{owner}.{host_id}.{secret}`
    /// grant; its digest replaces the challenge in the signed message.
    Grant {
        enroll: String,
        owner_id: String,
        digest: String,
        bearer: Zeroizing<String>,
    },
}

impl EnrollRoute {
    pub fn account(origin: &str, account_key: &str) -> Self {
        Self::Account {
            challenges: format!("{origin}/v1/account/hand-devices/challenges"),
            enroll: format!("{origin}/v1/account/hand-devices"),
            bearer: Zeroizing::new(account_key.to_owned()),
        }
    }
    /// The bootstrap grant route for scoped publisher endpoint
    /// `/v1/hand-hosts/{owner}/{host_id}/hands`.
    pub fn grant(origin: &str, publisher: &Url, grant: &str) -> Result<Self, DeviceError> {
        let invalid = || fatal("The server Hand device grant is invalid");
        let parts: Vec<&str> = publisher.path().trim_end_matches('/').split('/').collect();
        let [_, "v1", "hand-hosts", owner, host, "hands"] = parts.as_slice() else {
            return Err(fatal(
                "Server Hand device enrollment requires a scoped Hand host endpoint",
            ));
        };
        let mut fields = grant.trim().splitn(4, '.');
        let (Some("ncxhg1"), Some(grant_owner), Some(grant_host), Some(secret)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid());
        };
        if grant_owner != *owner
            || grant_host != *host
            || secret.is_empty()
            || secret.len() > 256
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid());
        }
        Ok(Self::Grant {
            enroll: format!("{origin}/v1/hand-hosts/{owner}/{host}/hands/device"),
            owner_id: (*owner).to_owned(),
            digest: URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()).as_slice()),
            bearer: Zeroizing::new(grant.trim().to_owned()),
        })
    }
}

/// The HTTP origin of a managed ws(s)/http(s) endpoint, as signed in messages.
pub fn managed_origin(endpoint: &Url) -> Result<String, DeviceError> {
    let mut endpoint = endpoint.clone();
    let scheme = match endpoint.scheme() {
        "wss" | "https" => "https",
        "ws" | "http" => "http",
        _ => return Err(fatal("Unsupported managed service URL for Hand devices")),
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|()| fatal("Unsupported managed service URL for Hand devices"))?;
    Ok(endpoint.origin().ascii_serialization())
}

pub enum Enrollment {
    Device(DeviceState),
    /// Never enrolled, and the service has no enrollment route.
    Legacy,
}

struct Issued {
    credential: Zeroizing<String>,
    expires_at_ms: u64,
    key_version: u64,
    device_id: String,
}

/// Local device identity bound to one private Hand state directory and origin.
pub struct DeviceIdentity {
    directory: PathBuf,
    origin: String,
    http: reqwest::Client,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceIdentity")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    pub fn new(directory: &Path, origin: &str) -> Result<Self, DeviceError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| fatal("Cannot prepare the Hand device HTTP client"))?;
        Ok(Self {
            directory: directory.to_owned(),
            origin: origin.trim_end_matches('/').to_owned(),
            http,
        })
    }

    pub fn state(&self) -> Result<Option<DeviceState>, DeviceError> {
        let Some(bytes) = read_private_file(&self.directory.join(STATE_FILE), MAX_STATE_BYTES)?
        else {
            return Ok(None);
        };
        let state: DeviceState = serde_json::from_slice(&bytes)
            .map_err(|_| fatal("device.json is invalid; run `nanocodex hand devices reenroll`"))?;
        if !valid_owner(&state.owner_id) || !valid_device_id(&state.device_id) {
            return Err(fatal(
                "device.json is invalid; run `nanocodex hand devices reenroll`",
            ));
        }
        if state.origin != self.origin {
            return Err(fatal(format!(
                "This Hand is enrolled with {} rather than {}; run `nanocodex hand devices reenroll` to enroll it with this service",
                state.origin, self.origin
            )));
        }
        Ok(Some(state))
    }

    fn save(&self, state: &DeviceState) -> Result<(), DeviceError> {
        let mut bytes =
            serde_json::to_vec_pretty(state).map_err(|_| fatal("Cannot encode device.json"))?;
        bytes.push(b'\n');
        replace_private_file(&self.directory, STATE_FILE, &bytes)
            .map_err(|error| local("Cannot write device.json", &error))
    }

    fn key(&self, name: &str) -> Result<Option<DeviceKey>, DeviceError> {
        read_private_file(&self.directory.join(name), MAX_KEY_BYTES)?
            .map(|bytes| DeviceKey::from_pkcs8(&bytes))
            .transpose()
    }

    fn generate(&self, name: &str) -> Result<DeviceKey, DeviceError> {
        // ring's PKCS#8 document cannot be zeroized; keep it scoped to this
        // copy and zeroize the copy that is written and parsed.
        let bytes = {
            let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map_err(|_| fatal("Cannot generate a Hand device key"))?;
            Zeroizing::new(document.as_ref().to_vec())
        };
        create_private_file(&self.directory.join(name), &bytes)
            .and_then(|()| sync_directory(&self.directory))
            .map_err(|error| local("Cannot create the Hand device key", &error))?;
        DeviceKey::from_pkcs8(&bytes)
    }

    fn promote(&self) -> Result<(), DeviceError> {
        fs::rename(
            self.directory.join(NEXT_KEY_FILE),
            self.directory.join(KEY_FILE),
        )
        .and_then(|()| sync_directory(&self.directory))
        .map_err(|error| local("Cannot install the rotated Hand device key", &error))
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> Result<Reply, DeviceError> {
        let mut request = self
            .http
            .request(method, url)
            .header("origin", &self.origin)
            .header("cache-control", "no-store")
            .json(body);
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        let response = request.send().await.map_err(|error| {
            DeviceError::Transient(format!(
                "Cannot reach the managed service for Hand device authentication ({})",
                error.without_url()
            ))
        })?;
        let status = response.status().as_u16();
        let capable = response.headers().contains_key("x-nanocodex-hand-devices");
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok(Reply {
            status,
            body,
            capable,
        })
    }

    async fn device_request(
        &self,
        method: reqwest::Method,
        state: &DeviceState,
        suffix: &str,
        body: &Value,
    ) -> Result<Reply, DeviceError> {
        let url = format!(
            "{}/v1/hand-devices/{}/{}/{suffix}",
            self.origin, state.owner_id, state.device_id
        );
        self.send(method, &url, None, body).await
    }

    async fn challenge(
        &self,
        state: &DeviceState,
        purpose: &str,
    ) -> Result<(String, Service), DeviceError> {
        let reply = self
            .device_request(
                reqwest::Method::POST,
                state,
                "challenges",
                &json!({ "purpose": purpose }),
            )
            .await?;
        if !reply.success() {
            return Err(device_failure(&reply, purpose));
        }
        Ok((reply.challenge()?, reply.service()?))
    }

    /// Align local key state with the service's current key (see module docs).
    fn reconcile(
        &self,
        state: &mut DeviceState,
        service: &Service,
    ) -> Result<DeviceKey, DeviceError> {
        let key = self.key(KEY_FILE)?.ok_or_else(|| {
            fatal("The Hand device key is missing; run `nanocodex hand devices reenroll`")
        })?;
        let pending = self.key(NEXT_KEY_FILE)?;
        let current = key.fingerprint();
        // Promotion is always proven by the service's current key fingerprint.
        let reported = service.fingerprint.as_deref().ok_or_else(|| {
            fatal("The managed service did not report this Hand device's key fingerprint")
        })?;
        let installed = if reported == current && current == state.fingerprint {
            if service.key_version != state.key_version {
                return Err(fatal(format!(
                    "This Hand's device key version {} does not match the service ({}); run `nanocodex hand devices reenroll`",
                    state.key_version, service.key_version
                )));
            }
            return Ok(key);
        } else if let Some(pending) = pending.filter(|pending| pending.fingerprint() == reported) {
            self.promote()?;
            pending
        } else if current != state.fingerprint && reported == current {
            // Promoted before device.json was updated.
            key
        } else {
            return Err(DeviceError::Reenroll);
        };
        if service.key_version <= state.key_version {
            return Err(fatal(
                "The managed service reported a stale Hand device key version; run `nanocodex hand devices reenroll`",
            ));
        }
        state.key_version = service.key_version;
        state.fingerprint = installed.fingerprint();
        // The service drops attestations on rotation; re-attest the new key.
        state.ssh_host_keys.clear();
        self.save(state)?;
        tracing::info!(target: "nanocodex2", stage = "hand.device.rotation_recovered",
            device_id = state.device_id.as_str(), key_version = state.key_version,
            "Completed an interrupted Hand device key rotation");
        Ok(installed)
    }

    /// Load or create the key and enroll once. Never re-enrolls an existing
    /// `device.json`, including after revocation.
    pub async fn ensure_enrolled(
        &self,
        route: &EnrollRoute,
        machine_id: &str,
        name: &str,
    ) -> Result<Enrollment, DeviceError> {
        check_private_directory(&self.directory)?;
        let _lock = lock(&self.directory).await?;
        if let Some(state) = self.state()? {
            if state.machine_id != machine_id {
                return Err(fatal(
                    "device.json belongs to another Hand machine identity; run `nanocodex hand devices reenroll`",
                ));
            }
            return Ok(Enrollment::Device(state));
        }
        let key = match self.key(KEY_FILE)? {
            Some(key) => key,
            None => self.generate(KEY_FILE)?,
        };
        let public_key = key.public_key();
        let (owner_id, reply) = match route {
            EnrollRoute::Account {
                challenges,
                enroll,
                bearer,
            } => {
                let reply = self
                    .send(reqwest::Method::POST, challenges, Some(bearer), &json!({}))
                    .await?;
                // Only a route-level 404 from a service that predates Hand
                // devices permits legacy: no capability header and no error
                // other than the generic route miss.
                if reply.status == 404
                    && !reply.capable
                    && (reply.body.is_null() || error_code(&reply.body) == "not_found")
                {
                    return Ok(Enrollment::Legacy);
                }
                if !reply.success() {
                    return Err(enroll_failure(&reply));
                }
                let challenge = reply.challenge()?;
                let owner_id = reply.body["owner_id"]
                    .as_str()
                    .filter(|owner| valid_owner(owner))
                    .ok_or_else(|| {
                        fatal("The managed service returned an invalid account identity")
                    })?
                    .to_owned();
                let signature = key.sign(&message(&[
                    "enroll",
                    &self.origin,
                    &owner_id,
                    &challenge,
                    machine_id,
                    &public_key,
                ]));
                let body = json!({
                    "machine_id": machine_id,
                    "name": name,
                    "algorithm": "ed25519",
                    "public_key": public_key,
                    "challenge": challenge,
                    "signature": signature,
                });
                let reply = self
                    .send(reqwest::Method::POST, enroll, Some(bearer), &body)
                    .await?;
                (owner_id, reply)
            }
            EnrollRoute::Grant {
                enroll,
                owner_id,
                digest,
                bearer,
            } => {
                let signature = key.sign(&message(&[
                    "enroll",
                    &self.origin,
                    owner_id,
                    digest,
                    machine_id,
                    &public_key,
                ]));
                let body = json!({
                    "name": name,
                    "algorithm": "ed25519",
                    "public_key": public_key,
                    "signature": signature,
                });
                let reply = self
                    .send(reqwest::Method::POST, enroll, Some(bearer), &body)
                    .await?;
                if matches!(reply.status, 401 | 403 | 404) {
                    return Err(fatal(format!(
                        "The server Hand device grant was rejected or has expired ({}); reconnect this server with server_hand connect",
                        reply.describe()
                    )));
                }
                (owner_id.clone(), reply)
            }
        };
        if reply.status == 409 {
            return Err(fatal(
                "This computer already has an active Hand device with a different key. Revoke it with `nanocodex hand devices revoke <id>` (list with `nanocodex hand devices`), then restart the Hand.",
            ));
        }
        if !reply.success() {
            return Err(enroll_failure(&reply));
        }
        let device_id = reply.body["id"]
            .as_str()
            .filter(|id| valid_device_id(id))
            .ok_or_else(|| fatal("The managed service returned an invalid device"))?
            .to_owned();
        if reply.body["fingerprint"].as_str() != Some(key.fingerprint().as_str()) {
            return Err(fatal(
                "The managed service recorded a different Hand device key fingerprint",
            ));
        }
        let state = DeviceState {
            owner_id,
            device_id,
            machine_id: machine_id.to_owned(),
            key_version: reply.key_version()?,
            fingerprint: key.fingerprint(),
            origin: self.origin.clone(),
            ssh_host_keys: Vec::new(),
        };
        self.save(&state)?;
        tracing::info!(target: "nanocodex2", stage = "hand.device.enrolled",
            device_id = state.device_id.as_str(), key_version = state.key_version,
            fingerprint = state.fingerprint.as_str(), "Enrolled this Hand as a device");
        Ok(Enrollment::Device(state))
    }

    async fn issue(&self) -> Result<Issued, DeviceError> {
        check_private_directory(&self.directory)?;
        let _lock = lock(&self.directory).await?;
        let mut state = self.state()?.ok_or(DeviceError::Reenroll)?;
        let (challenge, service) = self.challenge(&state, "credential").await?;
        let key = self.reconcile(&mut state, &service)?;
        let version = service.key_version;
        let signature = key.sign(&message(&[
            "credential",
            &self.origin,
            &state.owner_id,
            &state.device_id,
            &version.to_string(),
            &challenge,
        ]));
        let reply = self
            .device_request(
                reqwest::Method::POST,
                &state,
                "credentials",
                &json!({ "challenge": challenge, "signature": signature }),
            )
            .await?;
        if !reply.success() {
            return Err(device_failure(&reply, "credential"));
        }
        let prefix = format!("ncxhd1.{}.{}.", state.owner_id, state.device_id);
        let credential = reply.body["credential"]
            .as_str()
            .filter(|value| {
                value.starts_with(&prefix)
                    && value.len() <= 512
                    && value.bytes().all(|b| b.is_ascii_graphic())
            })
            .map(|value| Zeroizing::new(value.to_owned()))
            .ok_or_else(|| {
                fatal("The managed service returned an invalid Hand device credential")
            })?;
        let expires_at_ms = expires_at_ms(&reply.body["expires_at"])
            .ok_or_else(|| fatal("The managed service returned an invalid credential expiry"))?;
        Ok(Issued {
            credential,
            expires_at_ms,
            key_version: state.key_version,
            device_id: state.device_id,
        })
    }

    /// Rotate the device key (see module docs for crash ordering).
    pub async fn rotate(&self) -> Result<DeviceState, DeviceError> {
        check_private_directory(&self.directory)?;
        let lock_guard = lock(&self.directory).await?;
        let mut state = self.state()?.ok_or_else(|| {
            fatal("This Hand is not enrolled as a device yet; start the Hand once first")
        })?;
        let (challenge, service) = self.challenge(&state, "rotate").await?;
        let before = state.key_version;
        let key = self.reconcile(&mut state, &service)?;
        if state.key_version != before {
            // A previous rotation was accepted; it is now complete.
            drop(lock_guard);
            return Ok(state);
        }
        let version = service.key_version;
        let next = match self.key(NEXT_KEY_FILE)? {
            Some(next) => next,
            None => self.generate(NEXT_KEY_FILE)?,
        };
        let new_public_key = next.public_key();
        let signed = message(&[
            "rotate",
            &self.origin,
            &state.owner_id,
            &state.device_id,
            &version.to_string(),
            &challenge,
            &new_public_key,
        ]);
        let reply = self
            .device_request(
                reqwest::Method::POST,
                &state,
                "rotate",
                &json!({
                    "challenge": challenge,
                    "signature": key.sign(&signed),
                    "new_public_key": new_public_key,
                    "new_signature": next.sign(&signed),
                }),
            )
            .await?;
        if reply.status == 409 {
            let _ = fs::remove_file(self.directory.join(NEXT_KEY_FILE));
            return Err(fatal(
                "The service rejected the new Hand device key; run `nanocodex hand devices rotate` again",
            ));
        }
        if !reply.success() {
            // Keep .next: the next challenge proves whether it was accepted.
            return Err(device_failure(&reply, "rotation"));
        }
        let rotated = reply.key_version()?;
        if rotated != version.saturating_add(1)
            || reply.body["fingerprint"].as_str() != Some(next.fingerprint().as_str())
        {
            return Err(fatal(
                "The managed service reported an unexpected rotated Hand device key",
            ));
        }
        self.promote()?;
        state.key_version = rotated;
        state.fingerprint = next.fingerprint();
        // The service drops attestations on rotation; re-attest the new key.
        state.ssh_host_keys.clear();
        self.save(&state)?;
        tracing::info!(target: "nanocodex2", stage = "hand.device.rotated",
            device_id = state.device_id.as_str(), key_version = state.key_version,
            fingerprint = state.fingerprint.as_str(), "Rotated the Hand device key");
        drop(lock_guard);
        Ok(state)
    }

    /// Attest the readable sshd host public keys when they changed (or when
    /// `force`). Only fingerprints are sent; a service without the optional
    /// capability (404) is ignored.
    pub async fn attest_ssh_host_keys(&self, force: bool) -> Result<bool, DeviceError> {
        let fingerprints = ssh_host_fingerprints();
        if fingerprints.is_empty() {
            return Ok(false);
        }
        check_private_directory(&self.directory)?;
        let _lock = lock(&self.directory).await?;
        let mut state = self.state()?.ok_or(DeviceError::Reenroll)?;
        if !force && state.ssh_host_keys == fingerprints {
            return Ok(false);
        }
        let reply = self
            .device_request(
                reqwest::Method::POST,
                &state,
                "challenges",
                &json!({ "purpose": "ssh-host-keys" }),
            )
            .await?;
        if reply.status == 404 && error_code(&reply.body) != "hand_reenroll_required" {
            tracing::debug!(target: "nanocodex2", stage = "hand.device.ssh_attestation_unsupported", "SSH host key attestation is unavailable");
            return Ok(false);
        }
        if !reply.success() {
            return Err(device_failure(&reply, "ssh-host-keys"));
        }
        let (challenge, service) = (reply.challenge()?, reply.service()?);
        let key = self.reconcile(&mut state, &service)?;
        let version = service.key_version;
        let joined = fingerprints.join(",");
        let signature = key.sign(&message(&[
            "ssh-host-keys",
            &self.origin,
            &state.owner_id,
            &state.device_id,
            &version.to_string(),
            &challenge,
            &joined,
        ]));
        let reply = self
            .device_request(
                reqwest::Method::PUT,
                &state,
                "ssh-host-keys",
                &json!({ "challenge": challenge, "signature": signature, "fingerprints": fingerprints }),
            )
            .await?;
        if !reply.success() {
            return Err(device_failure(&reply, "ssh-host-keys"));
        }
        state.ssh_host_keys = fingerprints;
        self.save(&state)?;
        tracing::info!(target: "nanocodex2", stage = "hand.device.ssh_host_keys_attested",
            device_id = state.device_id.as_str(), count = state.ssh_host_keys.len(),
            "Attested this Hand's SSH host key fingerprints");
        Ok(true)
    }

    /// Forget local enrollment and keys so the next start enrolls a new device.
    pub async fn forget(&self) -> Result<Option<DeviceState>, DeviceError> {
        check_private_directory(&self.directory)?;
        let _lock = lock(&self.directory).await?;
        let state = self.state().ok().flatten();
        for name in [STATE_FILE, NEXT_KEY_FILE, KEY_FILE] {
            match fs::remove_file(self.directory.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(local(&format!("Cannot remove {name}"), &error)),
            }
        }
        sync_directory(&self.directory)
            .map_err(|error| local("Cannot sync the Hand state directory", &error))?;
        Ok(state)
    }
}

fn enroll_failure(reply: &Reply) -> DeviceError {
    match reply.status {
        401 | 403 => fatal(format!(
            "The saved login cannot enroll this Hand as a device ({}); sign in again with `nanocodex account login`",
            reply.describe()
        )),
        408 | 429 | 500..=599 => DeviceError::Transient(format!(
            "Hand device enrollment failed ({}); retrying",
            reply.describe()
        )),
        _ => fatal(format!(
            "Hand device enrollment was rejected ({})",
            reply.describe()
        )),
    }
}

/// OpenSSH SHA256 fingerprints of readable sshd host public keys, sorted.
/// Only public keys are read; unreadable or missing files are skipped.
pub fn ssh_host_fingerprints() -> Vec<String> {
    let directory = std::env::var_os("NANOCODEX_HAND_SSH_HOST_KEY_DIR")
        .map_or_else(|| PathBuf::from("/etc/ssh"), PathBuf::from);
    let mut fingerprints = Vec::new();
    for algorithm in ["ed25519", "ecdsa", "rsa"] {
        let path = directory.join(format!("ssh_host_{algorithm}_key.pub"));
        match host_key_fingerprint(&path) {
            Ok(fingerprint) => fingerprints.push(fingerprint),
            Err(reason) => {
                tracing::debug!(target: "nanocodex2", stage = "hand.device.ssh_host_key_skipped", algorithm, reason, "Skipped an SSH host public key");
            }
        }
    }
    fingerprints.sort();
    fingerprints.dedup();
    fingerprints.truncate(MAX_HOST_KEYS);
    fingerprints
}

fn host_key_fingerprint(path: &Path) -> Result<String, &'static str> {
    let file = fs::File::open(path).map_err(|_| "unreadable")?;
    let metadata = file.metadata().map_err(|_| "unreadable")?;
    if !metadata.is_file() || metadata.len() > MAX_HOST_KEY_BYTES {
        return Err("not a regular file");
    }
    let mut text = String::new();
    file.take(MAX_HOST_KEY_BYTES)
        .read_to_string(&mut text)
        .map_err(|_| "unreadable")?;
    let mut fields = text.split_whitespace();
    let (Some(kind), Some(encoded)) = (fields.next(), fields.next()) else {
        return Err("malformed");
    };
    if !matches!(
        kind,
        "ssh-ed25519"
            | "ssh-rsa"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
    ) {
        return Err("unsupported key type");
    }
    let blob = STANDARD.decode(encoded).map_err(|_| "malformed")?;
    // The wire blob starts with its own length-prefixed key type.
    let declared = blob
        .get(..4)
        .map(|length| u32::from_be_bytes([length[0], length[1], length[2], length[3]]) as usize)
        .and_then(|length| blob.get(4..4 + length))
        .ok_or("malformed")?;
    if declared != kind.as_bytes() {
        return Err("mismatched key type");
    }
    Ok(fingerprint(&blob))
}

struct Cached {
    credential: Zeroizing<String>,
    device_id: String,
    key_version: u64,
    refresh_at: Instant,
}

/// Short-lived device credentials, cached until shortly before expiry and
/// re-issued whenever `device.json` changes (rotation) or the service rejects one.
pub struct DeviceCredentials {
    identity: DeviceIdentity,
    cache: tokio::sync::Mutex<Option<Cached>>,
}

impl fmt::Debug for DeviceCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCredentials")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl DeviceCredentials {
    pub fn new(identity: DeviceIdentity) -> Self {
        Self {
            identity,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    pub const fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub async fn current(&self) -> Result<Zeroizing<String>, DeviceError> {
        let state = self.identity.state()?.ok_or(DeviceError::Reenroll)?;
        let mut cache = self.cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.device_id == state.device_id
            && cached.key_version == state.key_version
            && Instant::now() < cached.refresh_at
        {
            return Ok(cached.credential.clone());
        }
        *cache = None;
        let issued = self.identity.issue().await?;
        let lifetime = Duration::from_millis(issued.expires_at_ms.saturating_sub(now_ms()))
            .min(Duration::from_secs(900));
        let margin = REFRESH_MARGIN.min(lifetime / 2);
        *cache = Some(Cached {
            credential: issued.credential.clone(),
            device_id: issued.device_id,
            key_version: issued.key_version,
            refresh_at: Instant::now() + lifetime.saturating_sub(margin),
        });
        Ok(issued.credential)
    }

    /// Drop the cached credential after the service rejected it.
    pub fn invalidate(&self) {
        if let Ok(mut cache) = self.cache.try_lock() {
            *cache = None;
        }
    }

    async fn bearer_or_log(&self) -> Result<String, DeviceError> {
        self.current()
            .await
            .map(|credential| credential.to_string())
            .inspect_err(|error| match error {
                DeviceError::Transient(_) => {
                    tracing::warn!(target: "nanocodex2", stage = "hand.device.credential_failed", reason = error.reason(), %error, "Hand device credential unavailable");
                }
                _ => {
                    tracing::error!(target: "nanocodex2", stage = "hand.device.credential_failed", reason = error.reason(), %error, "Hand device credential permanently unavailable");
                }
            })
    }
}

impl AttachmentCredentials for DeviceCredentials {
    fn bearer(&self) -> BoxFuture<'_, Result<String, AttachmentError>> {
        Box::pin(async move {
            self.bearer_or_log()
                .await
                .map_err(DeviceError::into_attachment)
        })
    }
    fn rejected(&self) {
        self.invalidate();
    }
}

impl nanocodex_remote::target::PublisherCredentials for DeviceCredentials {
    fn bearer(&self) -> BoxFuture<'_, io::Result<String>> {
        Box::pin(async move { self.bearer_or_log().await.map_err(DeviceError::into_io) })
    }
    fn rejected(&self) {
        self.invalidate();
    }
}

/// How a Hand authenticates publication.
pub struct HandAuthorization {
    pub target: AttachmentTarget,
    pub device: Option<Arc<DeviceCredentials>>,
}

impl HandAuthorization {
    /// Replace an opaque rejection with the actionable re-enrollment error
    /// when the service no longer accepts this device.
    pub async fn explain(&self, error: ManagedError) -> ManagedError {
        let Some(device) = &self.device else {
            return error;
        };
        device.invalidate();
        match device.current().await {
            Err(DeviceError::Reenroll) => DeviceError::Reenroll.into(),
            _ => error,
        }
    }
}

/// Retry transient failures with bounded backoff; permanent failures return.
async fn retrying<T, F, Fut>(mut operation: F) -> Result<T, DeviceError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, DeviceError>>,
{
    let mut delay = Duration::from_secs(1);
    loop {
        match operation().await {
            Err(DeviceError::Transient(message)) => {
                tracing::warn!(target: "nanocodex2", stage = "hand.device.retry", error = message.as_str(), retry_ms = delay.as_millis() as u64, "Hand device authentication will retry");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
            result => return result,
        }
    }
}

/// Enroll on first run (or reuse the enrollment) and return the publication
/// target: device credentials for enrolled Hands, the account key only for a
/// never-enrolled Hand on a service without enrollment.
pub async fn authorize(
    account: &AttachmentTarget,
    directory: &Path,
    machine_id: &str,
    name: &str,
) -> Result<HandAuthorization, ManagedError> {
    let origin = managed_origin(account.endpoint())?;
    let route = EnrollRoute::account(&origin, account.bearer());
    let identity = DeviceIdentity::new(directory, &origin)?;
    let enrollment = retrying(|| identity.ensure_enrolled(&route, machine_id, name)).await?;
    drop(route);
    match enrollment {
        Enrollment::Legacy => {
            tracing::warn!(target: "nanocodex2", stage = "hand.device.legacy", auth_mode = "account_api_key", machine_id,
                "{LEGACY_WARNING}: the managed service does not offer Hand device enrollment");
            eprintln!(
                "warning: {LEGACY_WARNING} (the managed service does not offer Hand device enrollment)"
            );
            Ok(HandAuthorization {
                target: account.clone(),
                device: None,
            })
        }
        Enrollment::Device(state) => {
            tracing::info!(target: "nanocodex2", stage = "hand.device.authenticating", auth_mode = "device",
                device_id = state.device_id.as_str(), key_version = state.key_version, machine_id,
                "Publishing this Hand with device credentials");
            let credentials = Arc::new(DeviceCredentials::new(identity));
            let initial = retrying(|| credentials.current()).await?;
            attest(&credentials, false).await;
            let target = AttachmentTarget::with_credentials(
                account.endpoint().as_str(),
                initial.as_str(),
                credentials.clone(),
            )
            .map_err(|error| ManagedError::Configuration(error.to_string()))?;
            Ok(HandAuthorization {
                target,
                device: Some(credentials),
            })
        }
    }
}

/// Server Hand publisher: enroll once with the one-time bootstrap grant (then
/// delete it), or reuse the existing enrollment, and return its credentials.
/// The grant is ignored once `device.json` exists.
pub async fn authorize_server_host(
    endpoint: &Url,
    directory: &Path,
    grant_file: Option<&Path>,
    machine_id: &str,
    name: &str,
) -> Result<Arc<DeviceCredentials>, ManagedError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .map_err(|error| local("Cannot create the Hand device state directory", &error))?;
    let origin = managed_origin(endpoint)?;
    let identity = DeviceIdentity::new(directory, &origin)?;
    if identity.state()?.is_none() {
        let grant_file = grant_file.ok_or_else(|| {
            fatal("This server Hand is not enrolled and has no device grant; reconnect it with server_hand connect")
        })?;
        let grant = read_private_file(grant_file, 4096)?.ok_or_else(|| {
            fatal("The server Hand device grant is missing; reconnect it with server_hand connect")
        })?;
        let grant = std::str::from_utf8(&grant)
            .map_err(|_| fatal("The server Hand device grant is invalid"))?;
        let route = EnrollRoute::grant(&origin, endpoint, grant)?;
        retrying(|| identity.ensure_enrolled(&route, machine_id, name)).await?;
    } else if let Some(state) = identity.state()?
        && state.machine_id != machine_id
    {
        return Err(fatal(
            "device.json belongs to another Hand machine identity; reconnect this server with server_hand connect",
        )
        .into());
    }
    // A grant is single use; never leave it behind once enrolled.
    if let Some(grant_file) = grant_file {
        match fs::remove_file(grant_file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(target: "nanocodex2", stage = "hand.device.grant_cleanup_failed", error = %error.kind(), "Cannot remove the used server Hand device grant");
            }
        }
    }
    let credentials = Arc::new(DeviceCredentials::new(identity));
    retrying(|| credentials.current()).await?;
    attest(&credentials, false).await;
    Ok(credentials)
}

/// Best-effort host key attestation; failures never stop publication.
pub async fn attest(credentials: &DeviceCredentials, force: bool) {
    if let Err(error) = credentials.identity().attest_ssh_host_keys(force).await {
        tracing::warn!(target: "nanocodex2", stage = "hand.device.ssh_attestation_failed", reason = error.reason(), %error, "SSH host key attestation failed");
    }
}

/// `nanocodex hand devices`: manage this account's enrolled Hand devices.
#[derive(clap::Subcommand)]
pub enum DevicesCommand {
    /// List enrolled Hand devices and legacy account-key Hands.
    List {
        /// Print the service's JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Revoke an enrolled device; its Hand disconnects and cannot reconnect.
    Revoke { id: String },
    /// Rotate this computer's device key.
    Rotate {
        /// Hand state directory (defaults to this account's computer Hand).
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Forget this computer's local enrollment; the next Hand start enrolls a new device.
    Reenroll {
        /// Hand state directory (defaults to this account's computer Hand).
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

impl DevicesCommand {
    pub async fn run(self) -> Result<(), ManagedError> {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let (origin, key) = nanocodex_cli_auth::enrollment_credentials(None)?;
        let origin = managed_origin(
            &Url::parse(&origin).map_err(|_| fatal("Invalid managed service URL"))?,
        )?;
        match self {
            Self::List { json } => {
                let identity = DeviceIdentity::new(Path::new("."), &origin)?;
                let reply = identity
                    .send(
                        reqwest::Method::GET,
                        &format!("{origin}/v1/account/hand-devices"),
                        Some(&key),
                        &Value::Null,
                    )
                    .await?;
                if !reply.success() {
                    return Err(
                        fatal(format!("Cannot list Hand devices ({})", reply.describe())).into(),
                    );
                }
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&reply.body).unwrap_or_default()
                    );
                    return Ok(());
                }
                for device in reply.body["data"].as_array().into_iter().flatten() {
                    let keys = device["ssh_host_keys"].as_array().map_or(0, Vec::len);
                    println!(
                        "{}  {:<7}  v{}  {}  {}  {}  ssh_host_keys={keys}",
                        device["id"].as_str().unwrap_or("?"),
                        device["status"].as_str().unwrap_or("?"),
                        device["key_version"].as_u64().unwrap_or(0),
                        device["fingerprint"].as_str().unwrap_or("?"),
                        device["machine_id"].as_str().unwrap_or("?"),
                        device["name"].as_str().unwrap_or(""),
                    );
                }
                for legacy in reply.body["legacy"].as_array().into_iter().flatten() {
                    println!(
                        "legacy   {}  ({LEGACY_WARNING})",
                        legacy["machine_id"].as_str().unwrap_or("?")
                    );
                }
                Ok(())
            }
            Self::Revoke { id } => {
                if !valid_device_id(&id) {
                    return Err(
                        fatal("Expected a Hand device id from `nanocodex hand devices`").into(),
                    );
                }
                let identity = DeviceIdentity::new(Path::new("."), &origin)?;
                let reply = identity
                    .send(
                        reqwest::Method::DELETE,
                        &format!("{origin}/v1/account/hand-devices/{id}"),
                        Some(&key),
                        &Value::Null,
                    )
                    .await?;
                if !reply.success() {
                    return Err(fatal(format!(
                        "Cannot revoke Hand device {id} ({})",
                        reply.describe()
                    ))
                    .into());
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reply.body).unwrap_or_default()
                );
                Ok(())
            }
            Self::Rotate { state_dir } => {
                let directory = state_directory(state_dir, &origin, &key).await?;
                let identity = DeviceIdentity::new(&directory, &origin)?;
                let state = identity.rotate().await?;
                if let Err(error) = identity.attest_ssh_host_keys(true).await {
                    eprintln!("warning: SSH host key attestation failed: {error}");
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "id": state.device_id,
                        "key_version": state.key_version,
                        "fingerprint": state.fingerprint,
                    }))
                    .unwrap_or_default()
                );
                Ok(())
            }
            Self::Reenroll { state_dir } => {
                let directory = state_directory(state_dir, &origin, &key).await?;
                let identity = DeviceIdentity::new(&directory, &origin)?;
                let previous = identity.forget().await?;
                match previous {
                    Some(state) => println!(
                        "Forgot local Hand device {}. If it is still active, revoke it with `nanocodex hand devices revoke {}`; then restart the Hand to enroll a new device.",
                        state.device_id, state.device_id
                    ),
                    None => println!(
                        "Removed local Hand device keys; restart the Hand to enroll this computer."
                    ),
                }
                Ok(())
            }
        }
    }
}

async fn state_directory(
    explicit: Option<PathBuf>,
    origin: &str,
    key: &str,
) -> Result<PathBuf, ManagedError> {
    match explicit {
        Some(directory) => Ok(directory),
        None => crate::hand_client::directory(origin, key).await,
    }
}
