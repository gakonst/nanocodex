use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as SyncMutex},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;
use web_time::{SystemTime, UNIX_EPOCH};

use super::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};

const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const LOGIN_TTL_MILLIS: i64 = 15 * 60 * 1_000;
const REFRESH_EARLY_MILLIS: i64 = 5 * 60 * 1_000;
const MAX_RESPONSE_BYTES: usize = 16 * 1_024;

#[cfg(not(target_family = "wasm"))]
/// Boxed native future returned by a hosted ChatGPT subscription capability.
pub type SubscriptionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_family = "wasm")]
/// Boxed browser future returned by a hosted ChatGPT subscription capability.
pub type SubscriptionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Host failure while loading credentials or making an OAuth request.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{detail}")]
pub struct SubscriptionHostError {
    detail: Arc<str>,
}

impl SubscriptionHostError {
    /// Creates a host failure without retaining credential contents.
    #[must_use]
    pub fn new(detail: impl Into<Arc<str>>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// One opaque subscription value loaded from host-owned durable storage.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SubscriptionStoreValue {
    /// Monotonic storage revision used for compare-and-swap updates.
    pub revision: u64,
    /// Rust-owned serialized state. Hosts must treat this as secret.
    pub payload: Option<String>,
}

/// Result of atomically replacing one subscription value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionCommit {
    /// The value was stored at this new revision.
    Committed(u64),
    /// Another owner changed the value first.
    Conflict(u64),
}

/// Bounded outbound request required by the Rust subscription state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionHttpRequest {
    method: &'static str,
    url: String,
    content_type: &'static str,
    body: String,
    max_response_bytes: usize,
}

impl SubscriptionHttpRequest {
    /// HTTP method selected by the state machine.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        self.method
    }

    /// Complete allowlisted OAuth endpoint.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Request content type.
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        self.content_type
    }

    /// Encoded request body. Hosts must not log it.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Maximum response bytes the host may return.
    #[must_use]
    pub const fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }
}

/// Complete bounded response returned by a hosted HTTP capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionHttpResponse {
    /// HTTP response status.
    pub status: u16,
    /// Response body, bounded according to [`SubscriptionHttpRequest::max_response_bytes`].
    pub body: String,
}

/// Minimal host capabilities needed by managed ChatGPT subscription authentication.
///
/// The host owns placement, encryption at rest, and outbound networking. Rust owns OAuth,
/// device login, token decoding and rotation, account continuity, and unauthorized recovery.
pub trait ChatGptSubscriptionHost: Send + Sync + 'static {
    /// Loads one opaque state value. A missing value has revision zero.
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> SubscriptionFuture<'a, Result<SubscriptionStoreValue, SubscriptionHostError>>;

    /// Atomically replaces one opaque state value.
    fn compare_and_swap<'a>(
        &'a self,
        key: &'a str,
        expected_revision: u64,
        payload: &'a str,
    ) -> SubscriptionFuture<'a, Result<SubscriptionCommit, SubscriptionHostError>>;

    /// Executes one bounded OAuth HTTP request.
    fn request<'a>(
        &'a self,
        request: SubscriptionHttpRequest,
    ) -> SubscriptionFuture<'a, Result<SubscriptionHttpResponse, SubscriptionHostError>>;
}

/// Initial credentials imported from a trusted host secret or Codex login.
#[derive(Clone)]
pub struct ChatGptCredentialSeed {
    access_token: String,
    refresh_token: String,
    account_id: String,
    fedramp: bool,
}

/// One immutable credential generation resolved by the Rust subscription manager.
///
/// Hosts receive this only to perform an authenticated outbound request. `Debug` redacts the
/// bearer token.
#[derive(Clone)]
pub struct ChatGptCredential {
    access_token: Arc<str>,
    account_id: Arc<str>,
    fedramp: bool,
    revision: u64,
}

impl ChatGptCredential {
    /// Bearer token for one outbound request. Do not retain or log it.
    #[must_use]
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Stable ChatGPT account routing identifier.
    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Whether the request targets FedRAMP service boundaries.
    #[must_use]
    pub const fn is_fedramp(&self) -> bool {
        self.fedramp
    }

    /// Credential generation used for unauthorized recovery.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl std::fmt::Debug for ChatGptCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptCredential")
            .field("access_token", &"[redacted]")
            .field("account_id", &self.account_id)
            .field("fedramp", &self.fedramp)
            .field("revision", &self.revision)
            .finish()
    }
}

impl ChatGptCredentialSeed {
    /// Creates an importable credential. Values are validated when the subscription is opened.
    #[must_use]
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        account_id: impl Into<String>,
        fedramp: bool,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            account_id: account_id.into(),
            fedramp,
        }
    }
}

impl std::fmt::Debug for ChatGptCredentialSeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptCredentialSeed")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("account_id", &self.account_id)
            .field("fedramp", &self.fedramp)
            .finish()
    }
}

/// Public state of one hosted ChatGPT device login.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChatGptLoginStatus {
    /// No credential or pending login exists.
    SignedOut,
    /// The device login expired before authorization completed.
    Expired,
    /// The caller should display the code and poll again after the suggested delay.
    Pending {
        /// Browser URL used to authorize the device.
        #[serde(rename = "verificationUrl")]
        verification_url: String,
        /// Short code displayed to the user.
        #[serde(rename = "userCode")]
        user_code: String,
        /// Absolute Unix time in milliseconds when this attempt expires.
        #[serde(rename = "expiresAt")]
        expires_at: i64,
        /// Minimum delay before polling again.
        #[serde(rename = "pollAfterMs")]
        poll_after_ms: i64,
    },
    /// A refreshable credential is ready for an agent.
    Authenticated {
        /// Stable ChatGPT account ID.
        #[serde(rename = "accountId")]
        account_id: String,
        /// Access-token expiry when the token contains one.
        #[serde(rename = "expiresAt")]
        expires_at: Option<i64>,
    },
}

/// Managed subscription failure.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ChatGptSubscriptionError {
    /// Host storage or networking failed.
    #[error("ChatGPT subscription host failed: {0}")]
    Host(Arc<str>),
    /// Stored state or an OAuth response violated the protocol.
    #[error("invalid ChatGPT subscription state: {0}")]
    Invalid(Arc<str>),
    /// No authenticated credential is available.
    #[error("ChatGPT subscription is not authenticated")]
    NotAuthenticated,
    /// A rotating refresh token can no longer be used.
    #[error("ChatGPT login is required: {0}")]
    LoginRequired(Arc<str>),
    /// Concurrent updates did not converge within the bounded retry budget.
    #[error("ChatGPT subscription state remained contended")]
    Contended,
}

/// Cloneable Rust-owned ChatGPT device-login and credential lifecycle.
#[derive(Clone)]
pub struct ChatGptSubscription {
    inner: Arc<SubscriptionInner>,
}

impl std::fmt::Debug for ChatGptSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptSubscription")
            .field("key", &self.inner.key)
            .field("issuer", &self.inner.issuer)
            .finish_non_exhaustive()
    }
}

struct SubscriptionInner {
    host: Arc<dyn ChatGptSubscriptionHost>,
    key: Arc<str>,
    issuer: Arc<str>,
    refresh: Mutex<()>,
    known_authenticated: SyncMutex<bool>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct PersistedState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential: Option<StoredCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingLogin>,
}

#[derive(Clone, Deserialize, Serialize)]
struct StoredCredential {
    access_token: String,
    refresh_token: String,
    account_id: String,
    fedramp: bool,
    generation: u64,
    expires_at: Option<i64>,
}

#[derive(Clone, Deserialize, Serialize)]
struct PendingLogin {
    device_auth_id: String,
    user_code: String,
    verification_url: String,
    interval_ms: i64,
    next_poll_at: i64,
    expires_at: i64,
}

struct LoadedState {
    revision: u64,
    state: PersistedState,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: Option<String>,
    user_code: Option<String>,
    usercode: Option<String>,
    interval: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Default, Deserialize)]
struct IdClaims {
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: AuthClaims,
}

#[derive(Default, Deserialize)]
struct AuthClaims {
    #[serde(rename = "chatgpt_account_id", default)]
    account_id: Option<String>,
    #[serde(rename = "chatgpt_account_is_fedramp", default)]
    fedramp: Option<bool>,
}

#[derive(Default, Deserialize)]
struct ExpClaims {
    #[serde(default)]
    exp: Option<i64>,
}

impl ChatGptSubscription {
    /// Opens one subscription over generic host persistence and HTTP capabilities.
    ///
    /// When storage is empty, `seed` is imported atomically. A same-account seed may repair an
    /// existing credential that has no refresh token; otherwise durable state always wins.
    pub async fn open(
        host: impl ChatGptSubscriptionHost,
        key: impl Into<Arc<str>>,
        seed: Option<ChatGptCredentialSeed>,
    ) -> Result<Self, ChatGptSubscriptionError> {
        Self::open_with_issuer(host, key, seed, DEFAULT_ISSUER).await
    }

    #[doc(hidden)]
    pub async fn open_with_issuer(
        host: impl ChatGptSubscriptionHost,
        key: impl Into<Arc<str>>,
        seed: Option<ChatGptCredentialSeed>,
        issuer: impl Into<Arc<str>>,
    ) -> Result<Self, ChatGptSubscriptionError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(ChatGptSubscriptionError::Invalid(Arc::from(
                "subscription key is empty",
            )));
        }
        let issuer = issuer.into();
        let issuer = Arc::<str>::from(issuer.trim().trim_end_matches('/'));
        validate_issuer(&issuer)?;
        let subscription = Self {
            inner: Arc::new(SubscriptionInner {
                host: Arc::new(host),
                key,
                issuer,
                refresh: Mutex::new(()),
                known_authenticated: SyncMutex::new(false),
            }),
        };
        let loaded = subscription.load().await?;
        if let Some(credential) = &loaded.state.credential {
            validate_credential(credential)?;
            if let Some(seed) = seed {
                subscription
                    .repair_unrefreshable_credential(loaded, seed)
                    .await?;
            }
            subscription.set_known_authenticated(true);
        } else if let Some(seed) = seed {
            let credential = credential_from_seed(seed)?;
            let state = PersistedState {
                credential: Some(credential),
                pending: None,
            };
            match subscription.commit(loaded.revision, &state).await? {
                SubscriptionCommit::Committed(_) => subscription.set_known_authenticated(true),
                SubscriptionCommit::Conflict(_) => {
                    let current = subscription.load().await?;
                    if current.state.credential.is_none() {
                        return Err(ChatGptSubscriptionError::Contended);
                    }
                    subscription.set_known_authenticated(true);
                }
            }
        }
        Ok(subscription)
    }

    async fn repair_unrefreshable_credential(
        &self,
        loaded: LoadedState,
        seed: ChatGptCredentialSeed,
    ) -> Result<(), ChatGptSubscriptionError> {
        let current = loaded
            .state
            .credential
            .as_ref()
            .ok_or(ChatGptSubscriptionError::NotAuthenticated)?;
        if !current.refresh_token.trim().is_empty() || seed.refresh_token.trim().is_empty() {
            return Ok(());
        }
        let mut replacement = credential_from_seed(seed)?;
        if replacement.account_id != current.account_id {
            return Err(ChatGptSubscriptionError::Invalid(Arc::from(
                "credential seed changed the stored ChatGPT account",
            )));
        }
        replacement.generation = current.generation.wrapping_add(1);
        let account_id = replacement.account_id.clone();
        let state = PersistedState {
            credential: Some(replacement),
            pending: None,
        };
        match self.commit(loaded.revision, &state).await? {
            SubscriptionCommit::Committed(_) => Ok(()),
            SubscriptionCommit::Conflict(_) => {
                let current = self
                    .load()
                    .await?
                    .state
                    .credential
                    .ok_or(ChatGptSubscriptionError::Contended)?;
                validate_credential(&current)?;
                if current.account_id == account_id && !current.refresh_token.trim().is_empty() {
                    Ok(())
                } else {
                    Err(ChatGptSubscriptionError::Contended)
                }
            }
        }
    }

    /// Returns managed authorization for an agent.
    ///
    /// # Errors
    ///
    /// Returns an error while signed out or while a device login is pending.
    pub async fn authorization(&self) -> Result<OpenAiAuth, ChatGptSubscriptionError> {
        self.current_snapshot().await?;
        Ok(OpenAiAuth::managed_chatgpt(Arc::new(
            SubscriptionAuthSource {
                subscription: self.clone(),
            },
        )))
    }

    /// Resolves one credential generation for a host-owned outbound request.
    pub async fn credential(&self) -> Result<ChatGptCredential, ChatGptSubscriptionError> {
        let snapshot = self.current_snapshot().await?;
        Ok(ChatGptCredential {
            access_token: Arc::from(snapshot.bearer()),
            account_id: Arc::from(snapshot.account_id().unwrap_or_default()),
            fedramp: snapshot.is_fedramp(),
            revision: snapshot.revision(),
        })
    }

    /// Refreshes the rejected generation and resolves the credential now current.
    pub async fn recover(
        &self,
        rejected_revision: u64,
    ) -> Result<ChatGptCredential, ChatGptSubscriptionError> {
        self.refresh_if_current(rejected_revision).await?;
        self.credential().await
    }

    /// Starts a new device login, replacing any prior credential or pending attempt.
    pub async fn start_login(&self) -> Result<ChatGptLoginStatus, ChatGptSubscriptionError> {
        let response = self
            .request_json(
                format!("{}/api/accounts/deviceauth/usercode", self.inner.issuer),
                "application/json",
                serde_json::json!({ "client_id": OAUTH_CLIENT_ID }).to_string(),
            )
            .await?;
        ensure_success(&response, "login start")?;
        let body: DeviceCodeResponse = decode_response(&response)?;
        let device_auth_id = required(body.device_auth_id, "device authorization ID")?;
        let user_code = required(body.user_code.or(body.usercode), "device user code")?;
        let interval_ms = parse_interval(body.interval.as_ref()) * 1_000;
        let now = unix_millis();
        let pending = PendingLogin {
            device_auth_id,
            user_code,
            verification_url: format!("{}/codex/device", self.inner.issuer),
            interval_ms,
            next_poll_at: now.saturating_add(interval_ms),
            expires_at: now.saturating_add(LOGIN_TTL_MILLIS),
        };
        for _ in 0..4 {
            let loaded = self.load().await?;
            let state = PersistedState {
                credential: None,
                pending: Some(pending.clone()),
            };
            if matches!(
                self.commit(loaded.revision, &state).await?,
                SubscriptionCommit::Committed(_)
            ) {
                self.set_known_authenticated(false);
                return Ok(pending_status(&pending, now));
            }
        }
        Err(ChatGptSubscriptionError::Contended)
    }

    /// Polls a pending login when due and reports only non-secret state.
    pub async fn status(&self) -> Result<ChatGptLoginStatus, ChatGptSubscriptionError> {
        let loaded = self.load().await?;
        if let Some(credential) = loaded.state.credential {
            self.set_known_authenticated(true);
            return Ok(authenticated_status(&credential));
        }
        self.set_known_authenticated(false);
        let Some(mut pending) = loaded.state.pending else {
            return Ok(ChatGptLoginStatus::SignedOut);
        };
        let now = unix_millis();
        if pending.expires_at <= now {
            let _ = self
                .commit(loaded.revision, &PersistedState::default())
                .await?;
            return Ok(ChatGptLoginStatus::Expired);
        }
        if pending.next_poll_at > now {
            return Ok(pending_status(&pending, now));
        }

        pending.next_poll_at = now.saturating_add(pending.interval_ms);
        let claimed = PersistedState {
            credential: None,
            pending: Some(pending.clone()),
        };
        let claimed_revision = match self.commit(loaded.revision, &claimed).await? {
            SubscriptionCommit::Committed(revision) => revision,
            SubscriptionCommit::Conflict(_) => return self.status_without_poll().await,
        };
        let response = self
            .request_json(
                format!("{}/api/accounts/deviceauth/token", self.inner.issuer),
                "application/json",
                serde_json::json!({
                    "device_auth_id": pending.device_auth_id,
                    "user_code": pending.user_code,
                })
                .to_string(),
            )
            .await?;
        if matches!(response.status, 403 | 404) {
            return Ok(pending_status(&pending, now));
        }
        ensure_success(&response, "device authorization")?;
        let body: DeviceTokenResponse = decode_response(&response)?;
        let code = required(body.authorization_code, "authorization code")?;
        let verifier = required(body.code_verifier, "PKCE verifier")?;
        let credential = self.exchange_code(&code, &verifier).await?;
        let authenticated = PersistedState {
            credential: Some(credential.clone()),
            pending: None,
        };
        match self.commit(claimed_revision, &authenticated).await? {
            SubscriptionCommit::Committed(_) => {
                self.set_known_authenticated(true);
                Ok(authenticated_status(&credential))
            }
            SubscriptionCommit::Conflict(_) => self.status_without_poll().await,
        }
    }

    /// Clears durable credentials and pending login state.
    pub async fn logout(&self) -> Result<(), ChatGptSubscriptionError> {
        for _ in 0..4 {
            let loaded = self.load().await?;
            if matches!(
                self.commit(loaded.revision, &PersistedState::default())
                    .await?,
                SubscriptionCommit::Committed(_)
            ) {
                self.set_known_authenticated(false);
                return Ok(());
            }
        }
        Err(ChatGptSubscriptionError::Contended)
    }

    async fn status_without_poll(&self) -> Result<ChatGptLoginStatus, ChatGptSubscriptionError> {
        let loaded = self.load().await?;
        if let Some(credential) = loaded.state.credential {
            self.set_known_authenticated(true);
            return Ok(authenticated_status(&credential));
        }
        self.set_known_authenticated(false);
        let Some(pending) = loaded.state.pending else {
            return Ok(ChatGptLoginStatus::SignedOut);
        };
        let now = unix_millis();
        Ok(if pending.expires_at <= now {
            ChatGptLoginStatus::Expired
        } else {
            pending_status(&pending, now)
        })
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
    ) -> Result<StoredCredential, ChatGptSubscriptionError> {
        let body = form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            (
                "redirect_uri",
                &format!("{}/deviceauth/callback", self.inner.issuer),
            ),
            ("client_id", OAUTH_CLIENT_ID),
            ("code_verifier", verifier),
        ]);
        let response = self
            .request_json(
                format!("{}/oauth/token", self.inner.issuer),
                "application/x-www-form-urlencoded",
                body,
            )
            .await?;
        ensure_success(&response, "token exchange")?;
        credential_from_tokens(decode_response(&response)?, None, 0)
    }

    async fn current_snapshot(&self) -> Result<OpenAiAuthSnapshot, ChatGptSubscriptionError> {
        let loaded = self.load().await?;
        let credential = loaded
            .state
            .credential
            .ok_or(ChatGptSubscriptionError::NotAuthenticated)?;
        validate_credential(&credential)?;
        if credential
            .expires_at
            .is_some_and(|expiry| expiry <= unix_millis().saturating_add(REFRESH_EARLY_MILLIS))
        {
            if let Err(error) = self.refresh_if_current(credential.generation).await {
                tracing::warn!(error = %error, "proactive ChatGPT token refresh failed");
            }
            let refreshed = self
                .load()
                .await?
                .state
                .credential
                .ok_or(ChatGptSubscriptionError::NotAuthenticated)?;
            return Ok(snapshot(&refreshed));
        }
        Ok(snapshot(&credential))
    }

    async fn refresh_if_current(
        &self,
        rejected_generation: u64,
    ) -> Result<(), ChatGptSubscriptionError> {
        let _guard = self.inner.refresh.lock().await;
        let loaded = self.load().await?;
        let current = loaded
            .state
            .credential
            .ok_or(ChatGptSubscriptionError::NotAuthenticated)?;
        if current.generation != rejected_generation {
            return Ok(());
        }
        if current.refresh_token.trim().is_empty() {
            return Err(ChatGptSubscriptionError::LoginRequired(Arc::from(
                "the stored credential has no refresh token",
            )));
        }
        let response = self
            .request_json(
                format!("{}/oauth/token", self.inner.issuer),
                "application/json",
                serde_json::json!({
                    "client_id": OAUTH_CLIENT_ID,
                    "grant_type": "refresh_token",
                    "refresh_token": current.refresh_token,
                })
                .to_string(),
            )
            .await?;
        if !(200..300).contains(&response.status) {
            let reloaded = self.load().await?;
            if reloaded
                .state
                .credential
                .as_ref()
                .is_some_and(|credential| credential.generation != rejected_generation)
            {
                return Ok(());
            }
            let code = refresh_error_code(&response.body)
                .unwrap_or_else(|| format!("token endpoint returned HTTP {}", response.status));
            if response.status == 401
                || matches!(
                    code.as_str(),
                    "refresh_token_expired" | "refresh_token_reused" | "refresh_token_invalidated"
                )
            {
                return Err(ChatGptSubscriptionError::LoginRequired(Arc::from(code)));
            }
            return Err(ChatGptSubscriptionError::Host(Arc::from(code)));
        }
        let next = credential_from_tokens(
            decode_response(&response)?,
            Some(&current),
            current.generation.wrapping_add(1),
        )?;
        if next.account_id != current.account_id {
            return Err(ChatGptSubscriptionError::Invalid(Arc::from(
                "the refreshed credential changed accounts",
            )));
        }
        let state = PersistedState {
            credential: Some(next),
            pending: None,
        };
        match self.commit(loaded.revision, &state).await? {
            SubscriptionCommit::Committed(_) => Ok(()),
            SubscriptionCommit::Conflict(_) => {
                let reloaded = self.load().await?;
                if reloaded
                    .state
                    .credential
                    .as_ref()
                    .is_some_and(|credential| {
                        credential.account_id == current.account_id
                            && credential.generation != rejected_generation
                    })
                {
                    Ok(())
                } else {
                    Err(ChatGptSubscriptionError::Contended)
                }
            }
        }
    }

    async fn load(&self) -> Result<LoadedState, ChatGptSubscriptionError> {
        let stored = self
            .inner
            .host
            .load(&self.inner.key)
            .await
            .map_err(host_error)?;
        let state = match stored.payload {
            Some(payload) => serde_json::from_str(&payload).map_err(|error| {
                ChatGptSubscriptionError::Invalid(Arc::from(format!(
                    "host returned malformed state: {error}"
                )))
            })?,
            None => PersistedState::default(),
        };
        Ok(LoadedState {
            revision: stored.revision,
            state,
        })
    }

    async fn commit(
        &self,
        expected_revision: u64,
        state: &PersistedState,
    ) -> Result<SubscriptionCommit, ChatGptSubscriptionError> {
        let payload = serde_json::to_string(state)
            .map_err(|error| ChatGptSubscriptionError::Invalid(Arc::from(error.to_string())))?;
        self.inner
            .host
            .compare_and_swap(&self.inner.key, expected_revision, &payload)
            .await
            .map_err(host_error)
    }

    async fn request_json(
        &self,
        url: String,
        content_type: &'static str,
        body: String,
    ) -> Result<SubscriptionHttpResponse, ChatGptSubscriptionError> {
        let request = SubscriptionHttpRequest {
            method: "POST",
            url,
            content_type,
            body,
            max_response_bytes: MAX_RESPONSE_BYTES,
        };
        let response = self.inner.host.request(request).await.map_err(host_error)?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(ChatGptSubscriptionError::Invalid(Arc::from(
                "OAuth response exceeded 16384 bytes",
            )));
        }
        Ok(response)
    }

    fn set_known_authenticated(&self, value: bool) {
        if let Ok(mut authenticated) = self.inner.known_authenticated.lock() {
            *authenticated = value;
        }
    }
}

struct SubscriptionAuthSource {
    subscription: ChatGptSubscription,
}

impl OpenAiAuthSource for SubscriptionAuthSource {
    fn validate(&self) -> Result<(), OpenAiAuthError> {
        match self.subscription.inner.known_authenticated.lock() {
            Ok(authenticated) if *authenticated => Ok(()),
            Ok(_) => Err(OpenAiAuthError::LoginRequired(Arc::from(
                "the hosted subscription is signed out",
            ))),
            Err(_) => Err(OpenAiAuthError::Unavailable(Arc::from(
                "subscription state poisoned",
            ))),
        }
    }

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async move {
            self.subscription
                .current_snapshot()
                .await
                .map_err(auth_error)
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>> {
        let mode = rejected.mode();
        let generation = rejected.revision();
        Box::pin(async move {
            if mode != OpenAiAuthMode::ChatGpt {
                return Err(OpenAiAuthError::LoginRequired(Arc::from(
                    "authorization mode changed",
                )));
            }
            self.subscription
                .refresh_if_current(generation)
                .await
                .map_err(auth_error)
        })
    }
}

fn credential_from_seed(
    seed: ChatGptCredentialSeed,
) -> Result<StoredCredential, ChatGptSubscriptionError> {
    let credential = StoredCredential {
        expires_at: jwt_expiration_millis(&seed.access_token),
        access_token: seed.access_token,
        refresh_token: seed.refresh_token,
        account_id: seed.account_id,
        fedramp: seed.fedramp,
        generation: 0,
    };
    validate_credential(&credential)?;
    Ok(credential)
}

fn credential_from_tokens(
    tokens: TokenResponse,
    previous: Option<&StoredCredential>,
    generation: u64,
) -> Result<StoredCredential, ChatGptSubscriptionError> {
    let access_token = required(tokens.access_token, "access token")?;
    let refresh_token = tokens
        .refresh_token
        .filter(|value| !value.trim().is_empty())
        .or_else(|| previous.map(|credential| credential.refresh_token.clone()))
        .unwrap_or_default();
    let claims = tokens
        .id_token
        .as_deref()
        .map(decode_jwt::<IdClaims>)
        .transpose()?;
    let account_id = claims
        .as_ref()
        .and_then(|claims| claims.auth.account_id.clone())
        .or_else(|| previous.map(|credential| credential.account_id.clone()))
        .ok_or_else(|| {
            ChatGptSubscriptionError::Invalid(Arc::from(
                "token response omitted the ChatGPT account ID",
            ))
        })?;
    let fedramp = claims
        .and_then(|claims| claims.auth.fedramp)
        .or_else(|| previous.map(|credential| credential.fedramp))
        .unwrap_or(false);
    let credential = StoredCredential {
        expires_at: jwt_expiration_millis(&access_token),
        access_token,
        refresh_token,
        account_id,
        fedramp,
        generation,
    };
    validate_credential(&credential)?;
    Ok(credential)
}

fn validate_credential(credential: &StoredCredential) -> Result<(), ChatGptSubscriptionError> {
    if credential.access_token.trim().is_empty() || credential.account_id.trim().is_empty() {
        return Err(ChatGptSubscriptionError::Invalid(Arc::from(
            "required credential field is empty",
        )));
    }
    Ok(())
}

fn snapshot(credential: &StoredCredential) -> OpenAiAuthSnapshot {
    OpenAiAuthSnapshot::new(
        OpenAiAuthMode::ChatGpt,
        Arc::<str>::from(credential.access_token.as_str()),
        Some(Arc::<str>::from(credential.account_id.as_str())),
        credential.fedramp,
        credential.generation,
    )
}

fn pending_status(pending: &PendingLogin, now: i64) -> ChatGptLoginStatus {
    ChatGptLoginStatus::Pending {
        verification_url: pending.verification_url.clone(),
        user_code: pending.user_code.clone(),
        expires_at: pending.expires_at,
        poll_after_ms: pending.next_poll_at.saturating_sub(now).max(250),
    }
}

fn authenticated_status(credential: &StoredCredential) -> ChatGptLoginStatus {
    ChatGptLoginStatus::Authenticated {
        account_id: credential.account_id.clone(),
        expires_at: credential.expires_at,
    }
}

fn validate_issuer(issuer: &str) -> Result<(), ChatGptSubscriptionError> {
    let issuer = issuer.trim().trim_end_matches('/');
    if issuer == DEFAULT_ISSUER || issuer.starts_with("http://127.0.0.1:") {
        Ok(())
    } else {
        Err(ChatGptSubscriptionError::Invalid(Arc::from(
            "issuer must be auth.openai.com",
        )))
    }
}

fn ensure_success(
    response: &SubscriptionHttpResponse,
    operation: &str,
) -> Result<(), ChatGptSubscriptionError> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        Err(ChatGptSubscriptionError::Host(Arc::from(format!(
            "ChatGPT {operation} failed with HTTP {}",
            response.status
        ))))
    }
}

fn decode_response<T: DeserializeOwned>(
    response: &SubscriptionHttpResponse,
) -> Result<T, ChatGptSubscriptionError> {
    serde_json::from_str(&response.body).map_err(|error| {
        ChatGptSubscriptionError::Invalid(Arc::from(format!(
            "OAuth endpoint returned invalid JSON: {error}"
        )))
    })
}

fn decode_jwt<T: DeserializeOwned>(jwt: &str) -> Result<T, ChatGptSubscriptionError> {
    let payload = jwt
        .split('.')
        .nth(1)
        .filter(|payload| !payload.is_empty())
        .ok_or_else(|| ChatGptSubscriptionError::Invalid(Arc::from("invalid JWT format")))?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).map_err(|error| {
        ChatGptSubscriptionError::Invalid(Arc::from(format!("invalid JWT payload: {error}")))
    })?;
    serde_json::from_slice(&decoded).map_err(|error| {
        ChatGptSubscriptionError::Invalid(Arc::from(format!("invalid JWT claims: {error}")))
    })
}

fn jwt_expiration_millis(jwt: &str) -> Option<i64> {
    decode_jwt::<ExpClaims>(jwt)
        .ok()?
        .exp
        .and_then(|seconds| seconds.checked_mul(1_000))
}

fn parse_interval(value: Option<&serde_json::Value>) -> i64 {
    let parsed = match value {
        Some(serde_json::Value::Number(number)) => number.as_i64(),
        Some(serde_json::Value::String(value)) => value.parse().ok(),
        _ => None,
    };
    parsed.filter(|seconds| *seconds > 0).unwrap_or(5).min(30)
}

fn required(value: Option<String>, name: &str) -> Result<String, ChatGptSubscriptionError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ChatGptSubscriptionError::Invalid(Arc::from(format!(
                "OAuth response omitted the {name}"
            )))
        })
}

fn refresh_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    match &value["error"] {
        serde_json::Value::String(code) => Some(code.clone()),
        serde_json::Value::Object(error) => error
            .get("code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", form_component(name), form_component(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn form_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn host_error(error: SubscriptionHostError) -> ChatGptSubscriptionError {
    ChatGptSubscriptionError::Host(error.detail)
}

fn auth_error(error: ChatGptSubscriptionError) -> OpenAiAuthError {
    match error {
        ChatGptSubscriptionError::NotAuthenticated => {
            OpenAiAuthError::LoginRequired(Arc::from("the hosted subscription is signed out"))
        }
        ChatGptSubscriptionError::LoginRequired(detail) => OpenAiAuthError::LoginRequired(detail),
        ChatGptSubscriptionError::Invalid(detail) => OpenAiAuthError::Refresh(detail),
        ChatGptSubscriptionError::Host(detail) => OpenAiAuthError::Unavailable(detail),
        ChatGptSubscriptionError::Contended => {
            OpenAiAuthError::Unavailable(Arc::from("subscription state remained contended"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;

    #[derive(Default)]
    struct MemoryHost {
        stored: Mutex<SubscriptionStoreValue>,
        responses: Mutex<VecDeque<SubscriptionHttpResponse>>,
        requests: Mutex<Vec<SubscriptionHttpRequest>>,
    }

    impl MemoryHost {
        fn with_responses(responses: impl IntoIterator<Item = SubscriptionHttpResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                ..Self::default()
            }
        }
    }

    impl ChatGptSubscriptionHost for Arc<MemoryHost> {
        fn load<'a>(
            &'a self,
            _key: &'a str,
        ) -> SubscriptionFuture<'a, Result<SubscriptionStoreValue, SubscriptionHostError>> {
            Box::pin(async move { Ok(self.stored.lock().unwrap().clone()) })
        }

        fn compare_and_swap<'a>(
            &'a self,
            _key: &'a str,
            expected_revision: u64,
            payload: &'a str,
        ) -> SubscriptionFuture<'a, Result<SubscriptionCommit, SubscriptionHostError>> {
            Box::pin(async move {
                let mut stored = self.stored.lock().unwrap();
                if stored.revision != expected_revision {
                    return Ok(SubscriptionCommit::Conflict(stored.revision));
                }
                stored.revision += 1;
                stored.payload = Some(payload.to_owned());
                Ok(SubscriptionCommit::Committed(stored.revision))
            })
        }

        fn request<'a>(
            &'a self,
            request: SubscriptionHttpRequest,
        ) -> SubscriptionFuture<'a, Result<SubscriptionHttpResponse, SubscriptionHostError>>
        {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request);
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| SubscriptionHostError::new("unexpected request"))
            })
        }
    }

    #[tokio::test]
    async fn same_account_seed_repairs_only_non_refreshable_durable_credentials() {
        let expiry = unix_millis() / 1_000 + 3_600;
        let host = Arc::new(MemoryHost::default());
        ChatGptSubscription::open(
            Arc::clone(&host),
            "account",
            Some(ChatGptCredentialSeed::new(
                jwt(expiry, None, None),
                "",
                "account-1",
                false,
            )),
        )
        .await
        .unwrap();

        let mismatch = ChatGptSubscription::open(
            Arc::clone(&host),
            "account",
            Some(ChatGptCredentialSeed::new(
                jwt(expiry, None, None),
                "refresh-other",
                "account-2",
                false,
            )),
        )
        .await
        .unwrap_err();
        assert!(
            mismatch
                .to_string()
                .contains("changed the stored ChatGPT account")
        );

        let fresh_access = jwt(expiry + 1, None, None);
        let repaired = ChatGptSubscription::open(
            Arc::clone(&host),
            "account",
            Some(ChatGptCredentialSeed::new(
                fresh_access.clone(),
                "refresh-1",
                "account-1",
                true,
            )),
        )
        .await
        .unwrap();
        let credential = repaired.credential().await.unwrap();
        assert_eq!(credential.access_token(), fresh_access);
        assert_eq!(credential.revision(), 1);
        assert!(credential.is_fedramp());

        let stale_access = jwt(expiry - 1, None, None);
        let reopened = ChatGptSubscription::open(
            Arc::clone(&host),
            "account",
            Some(ChatGptCredentialSeed::new(
                stale_access,
                "refresh-stale",
                "account-1",
                false,
            )),
        )
        .await
        .unwrap();
        assert_eq!(
            reopened.credential().await.unwrap().access_token(),
            fresh_access
        );

        let stored = host.stored.lock().unwrap();
        assert_eq!(stored.revision, 2);
        let state: PersistedState =
            serde_json::from_str(stored.payload.as_deref().unwrap()).unwrap();
        let stored = state.credential.unwrap();
        assert_eq!(stored.refresh_token, "refresh-1");
        assert_eq!(stored.generation, 1);
    }

    #[tokio::test]
    async fn hosted_subscription_refreshes_in_rust_and_rotates_storage() {
        let expired = jwt(1, None, None);
        let fresh = jwt(unix_millis() / 1_000 + 3_600, Some("account-1"), Some(true));
        let host = Arc::new(MemoryHost::with_responses([SubscriptionHttpResponse {
            status: 200,
            body: serde_json::json!({
                "access_token": fresh,
                "refresh_token": "refresh-2",
                "id_token": jwt(0, Some("account-1"), Some(true)),
            })
            .to_string(),
        }]));
        let subscription = ChatGptSubscription::open(
            Arc::clone(&host),
            "account",
            Some(ChatGptCredentialSeed::new(
                expired,
                "refresh-1",
                "account-1",
                false,
            )),
        )
        .await
        .unwrap();

        let auth = subscription.authorization().await.unwrap();
        let snapshot = auth.snapshot().await.unwrap();
        assert_eq!(snapshot.account_id(), Some("account-1"));
        assert!(snapshot.is_fedramp());
        assert_eq!(snapshot.revision(), 1);
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body().contains("refresh-1"));
        assert!(!format!("{subscription:?}{auth:?}{snapshot:?}").contains("refresh-1"));
    }

    #[tokio::test]
    async fn device_login_state_machine_is_host_agnostic() {
        let host = Arc::new(MemoryHost::with_responses([
            SubscriptionHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "device_auth_id": "device-1",
                    "user_code": "ABCD-EFGH",
                    "interval": 1,
                })
                .to_string(),
            },
            SubscriptionHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "authorization_code": "code-1",
                    "code_verifier": "verifier-1",
                })
                .to_string(),
            },
            SubscriptionHttpResponse {
                status: 200,
                body: serde_json::json!({
                    "access_token": jwt(unix_millis() / 1_000 + 3600, None, None),
                    "refresh_token": "refresh-1",
                    "id_token": jwt(0, Some("account-1"), Some(false)),
                })
                .to_string(),
            },
        ]));
        let subscription = ChatGptSubscription::open(Arc::clone(&host), "account", None)
            .await
            .unwrap();

        let pending = subscription.start_login().await.unwrap();
        assert!(matches!(pending, ChatGptLoginStatus::Pending { .. }));
        {
            let mut stored = host.stored.lock().unwrap();
            let mut state: PersistedState =
                serde_json::from_str(stored.payload.as_deref().unwrap()).unwrap();
            state.pending.as_mut().unwrap().next_poll_at = 0;
            stored.payload = Some(serde_json::to_string(&state).unwrap());
        }
        let status = subscription.status().await.unwrap();
        assert_eq!(
            status,
            ChatGptLoginStatus::Authenticated {
                account_id: "account-1".to_owned(),
                expires_at: jwt_expiration_millis(
                    &subscription
                        .load()
                        .await
                        .unwrap()
                        .state
                        .credential
                        .unwrap()
                        .access_token,
                ),
            }
        );
        assert!(subscription.authorization().await.is_ok());
        assert_eq!(host.requests.lock().unwrap().len(), 3);
    }

    fn jwt(exp: i64, account_id: Option<&str>, fedramp: Option<bool>) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "exp": exp,
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id,
                    "chatgpt_account_is_fedramp": fedramp,
                },
            }))
            .unwrap(),
        );
        format!("{header}.{payload}.")
    }
}
