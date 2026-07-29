//! Nanocodex-owned Anthropic OAuth login and credential store.
//!
//! This mirrors [`nanocodex_oai_api::auth::ChatGptLogin`]: an authorization-code login with PKCE over a
//! loopback callback, an atomically written `0600` credential file, and in-process
//! refresh with the same revision-counted concurrency handling. Once a session is
//! logged in this way, no external CLI is involved at runtime.
//!
//! By default the flow uses Claude Code's public OAuth client registration. Callers
//! can provide another [`AnthropicOAuthConfig`], and the CLI supports
//! `NANOCODEX_ANTHROPIC_OAUTH_*` overrides for private or test registrations.

use std::{
    fmt,
    fs::OpenOptions,
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    ANTHROPIC_OAUTH_BETA, AnthropicAuth, AnthropicAuthError, AnthropicAuthFuture,
    AnthropicAuthMode, AnthropicAuthSnapshot, AnthropicAuthSource,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
    time::{Duration, timeout},
};
use url::Url;

const DEFAULT_ISSUER: &str = "https://platform.claude.com";
const DEFAULT_AUTHORIZE_ENDPOINT: &str = "https://claude.com/cai/oauth/authorize";
const DEFAULT_TOKEN_ENDPOINT: &str = "https://platform.claude.com/v1/oauth/token";
const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const DEFAULT_SCOPES: &str = "org:create_api_key user:profile user:inference \
                             user:sessions:claude_code user:mcp_servers user:file_upload";
const DEFAULT_CALLBACK_PATH: &str = "/callback";
const CALLBACK_PORTS: [u16; 3] = [1456, 1458, 1460];
const REFRESH_EARLY_SECONDS: i64 = 5 * 60;
const LOGIN_TIMEOUT: Duration = Duration::from_mins(5);
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// OAuth client registration used to sign in to Anthropic.
///
/// [`Default`] matches Claude Code's public registration. Every field remains public
/// so an embedding application can use a registration it controls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnthropicOAuthConfig {
    /// Authorization server base used to derive endpoints when explicit endpoints are
    /// absent.
    pub issuer: String,
    /// OAuth client identifier.
    pub client_id: String,
    /// Space-separated scopes, exactly as the client registration defines them.
    pub scopes: String,
    /// Callback path. This must match the client registration exactly, or the
    /// authorization server rejects the redirect before the user ever sees a consent
    /// screen.
    pub redirect_path: String,
    /// Fixed callback port, for registrations that pin one rather than allowing any
    /// loopback port. Defaults to the first free port of 1456, 1458, 1460.
    pub redirect_port: Option<u16>,
    /// Full authorization endpoint, when it is not `{issuer}/oauth/authorize`.
    pub authorize_endpoint: Option<String>,
    /// Full token endpoint, when it is not `{issuer}/oauth/token`. Authorization
    /// servers routinely place the exchange somewhere other than the OAuth default,
    /// in which case consent succeeds and only the exchange fails.
    pub token_endpoint: Option<String>,
}

impl Default for AnthropicOAuthConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_owned(),
            client_id: DEFAULT_CLIENT_ID.to_owned(),
            scopes: DEFAULT_SCOPES.to_owned(),
            redirect_path: DEFAULT_CALLBACK_PATH.to_owned(),
            redirect_port: None,
            authorize_endpoint: Some(DEFAULT_AUTHORIZE_ENDPOINT.to_owned()),
            token_endpoint: Some(DEFAULT_TOKEN_ENDPOINT.to_owned()),
        }
    }
}

impl AnthropicOAuthConfig {
    /// Returns the Claude Code registration with environment overrides applied.
    ///
    /// `NANOCODEX_ANTHROPIC_OAUTH_ISSUER` selects a custom issuer and derives its
    /// conventional `/oauth/authorize` and `/oauth/token` endpoints. The endpoint,
    /// client ID, scopes, callback path, and fixed callback port can each be overridden
    /// independently with the corresponding `NANOCODEX_ANTHROPIC_OAUTH_*` variable.
    #[must_use]
    pub fn from_env() -> Self {
        Self::with_overrides(env_value)
    }

    fn with_overrides(mut value: impl FnMut(&str) -> Option<String>) -> Self {
        let mut config = Self::default();
        if let Some(issuer) = value("NANOCODEX_ANTHROPIC_OAUTH_ISSUER") {
            config.issuer = issuer;
            config.authorize_endpoint = None;
            config.token_endpoint = None;
        }
        if let Some(client_id) = value("NANOCODEX_ANTHROPIC_OAUTH_CLIENT_ID") {
            config.client_id = client_id;
        }
        if let Some(scopes) = value("NANOCODEX_ANTHROPIC_OAUTH_SCOPES") {
            config.scopes = scopes;
        }
        if let Some(redirect_path) = value("NANOCODEX_ANTHROPIC_OAUTH_REDIRECT_PATH") {
            config.redirect_path = redirect_path;
        }
        if let Some(port) = value("NANOCODEX_ANTHROPIC_OAUTH_PORT")
            && let Ok(port) = port.parse()
        {
            config.redirect_port = Some(port);
        }
        if let Some(endpoint) = value("NANOCODEX_ANTHROPIC_OAUTH_AUTHORIZE_ENDPOINT") {
            config.authorize_endpoint = Some(endpoint);
        }
        if let Some(endpoint) = value("NANOCODEX_ANTHROPIC_OAUTH_TOKEN_ENDPOINT") {
            config.token_endpoint = Some(endpoint);
        }
        config
    }

    fn authorize_endpoint(&self) -> String {
        self.authorize_endpoint
            .clone()
            .unwrap_or_else(|| format!("{}/oauth/authorize", self.issuer.trim_end_matches('/')))
    }

    fn token_endpoint(&self) -> String {
        self.token_endpoint
            .clone()
            .unwrap_or_else(|| format!("{}/oauth/token", self.issuer.trim_end_matches('/')))
    }
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Non-secret information about a stored Anthropic authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnthropicLoginStatus {
    /// Anthropic account identifier, when returned by the token endpoint.
    pub account_id: Option<String>,
    /// Anthropic account email, when returned by the token endpoint.
    pub email: Option<String>,
    /// Seconds until the access token expires, when the response recorded one.
    pub expires_in_seconds: Option<i64>,
}

/// Failure while logging in, loading, persisting, or removing Anthropic credentials.
#[derive(Debug, thiserror::Error)]
pub enum AnthropicLoginError {
    /// The credential store could not be read, written, or removed.
    #[error("failed to access Anthropic authorization file {path}: {source}")]
    Storage {
        /// Credential store path.
        path: PathBuf,
        #[source]
        /// Underlying filesystem error.
        source: io::Error,
    },
    /// The credential store did not contain a valid login.
    #[error("Anthropic authorization file {path} is invalid: {detail}")]
    InvalidStore {
        /// Credential store path.
        path: PathBuf,
        /// Validation failure without credential values.
        detail: String,
    },
    /// The token endpoint returned an invalid token response.
    #[error("Anthropic OAuth response was invalid: {0}")]
    InvalidToken(String),
    /// None of the registered loopback callback ports could be bound.
    #[error("could not listen for the OAuth callback on localhost ports 1456, 1458, or 1460")]
    CallbackUnavailable,
    /// The browser did not complete the callback before the login deadline.
    #[error("timed out waiting for the Anthropic OAuth callback")]
    CallbackTimeout,
    /// The callback state did not match the active login.
    #[error("the OAuth callback did not match this login attempt")]
    StateMismatch,
    /// The authorization server rejected the login.
    #[error("Anthropic login was rejected: {0}")]
    LoginRejected(String),
    /// The authorization code or refresh-token exchange failed.
    #[error("Anthropic token exchange failed: {0}")]
    TokenExchange(String),
}

/// An in-progress authorization-code login using PKCE and a loopback callback.
///
/// Start the login, open [`authorization_url`](Self::authorization_url) in the user's
/// browser, then await [`complete`](Self::complete), which persists the credentials
/// before returning.
pub struct AnthropicLogin {
    config: AnthropicOAuthConfig,
    authorization_url: String,
    redirect_uri: String,
    state: String,
    code_verifier: String,
    auth_file: PathBuf,
    listener: TcpListener,
    client: reqwest::Client,
}

impl fmt::Debug for AnthropicLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicLogin")
            .field("authorization_url", &"[redacted]")
            .field("redirect_uri", &self.redirect_uri)
            .field("state", &"[redacted]")
            .field("code_verifier", &"[redacted]")
            .field("auth_file", &self.auth_file)
            .finish_non_exhaustive()
    }
}

impl AnthropicLogin {
    /// Starts a Claude Code-compatible loopback OAuth login.
    ///
    /// # Errors
    ///
    /// Returns an error when secure random data cannot be generated or no callback
    /// port can be bound.
    pub async fn start(auth_file: impl Into<PathBuf>) -> Result<Self, AnthropicLoginError> {
        Self::start_with_config(AnthropicOAuthConfig::default(), auth_file).await
    }

    /// Starts a loopback OAuth login with an explicit client registration.
    ///
    /// # Errors
    ///
    /// Returns an error when secure random data cannot be generated or no callback
    /// port can be bound.
    pub async fn start_with_config(
        config: AnthropicOAuthConfig,
        auth_file: impl Into<PathBuf>,
    ) -> Result<Self, AnthropicLoginError> {
        let listener = bind_callback(config.redirect_port).await?;
        let port = listener
            .local_addr()
            .map_err(|_| AnthropicLoginError::CallbackUnavailable)?
            .port();
        let redirect_uri = format!("http://localhost:{port}{}", config.redirect_path);
        let state = random_urlsafe()?;
        let code_verifier = random_urlsafe()?;
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let authorization_url = authorize_url(&config, &redirect_uri, &state, &code_challenge)?;

        Ok(Self {
            config,
            authorization_url,
            redirect_uri,
            state,
            code_verifier,
            auth_file: auth_file.into(),
            listener,
            client: auth_client()?,
        })
    }

    #[must_use]
    /// Returns the browser URL for this login attempt.
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Waits for the callback, exchanges the code, and atomically persists credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the callback is invalid, the exchange fails, or the
    /// credentials cannot be persisted.
    pub async fn complete(self) -> Result<AnthropicLoginStatus, AnthropicLoginError> {
        let callback = timeout(
            LOGIN_TIMEOUT,
            receive_callback(&self.listener, &self.config.redirect_path),
        )
        .await
        .map_err(|_| AnthropicLoginError::CallbackTimeout)??;
        let result = self.complete_callback(&callback.target).await;
        let reply = callback.reply(result.is_ok()).await;
        match (result, reply) {
            (Ok(status), Ok(())) => Ok(status),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn complete_callback(
        &self,
        callback_target: &str,
    ) -> Result<AnthropicLoginStatus, AnthropicLoginError> {
        let callback = Url::parse(&format!("http://localhost{callback_target}"))
            .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
        if callback.path() != self.config.redirect_path {
            return Err(AnthropicLoginError::LoginRejected(
                "invalid callback path".into(),
            ));
        }
        let query = callback
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<std::collections::HashMap<_, _>>();
        // Validate the anti-forgery state before touching the authorization code.
        if query.get("state") != Some(&self.state) {
            return Err(AnthropicLoginError::StateMismatch);
        }
        if let Some(error) = query.get("error") {
            let detail = query
                .get("error_description")
                .map_or(error.as_str(), String::as_str);
            return Err(AnthropicLoginError::LoginRejected(detail.to_owned()));
        }
        let code = query.get("code").ok_or_else(|| {
            AnthropicLoginError::LoginRejected("missing authorization code".into())
        })?;
        let credentials = exchange_code(
            &self.client,
            &self.config,
            code,
            &self.redirect_uri,
            &self.code_verifier,
            &self.state,
        )
        .await?;
        write_store(&self.auth_file, &credentials)?;
        Ok(credentials.status())
    }
}

struct OAuthCallback {
    target: String,
    stream: tokio::net::TcpStream,
}

impl OAuthCallback {
    async fn reply(mut self, success: bool) -> Result<(), AnthropicLoginError> {
        let (status, body): (&str, &[u8]) = if success {
            (
                "200 OK",
                b"Anthropic login completed. You can close this window.",
            )
        } else {
            (
                "400 Bad Request",
                b"Anthropic login failed. Return to the terminal for details.",
            )
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        self.stream
            .write_all(response.as_bytes())
            .await
            .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
        self.stream
            .write_all(body)
            .await
            .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))
    }
}

/// The default credential file for a nanocodex-owned Anthropic login.
///
/// `NANOCODEX_ANTHROPIC_AUTH_FILE` overrides it; otherwise it sits beside the Codex
/// credential file so one home directory holds a session's logins.
#[must_use]
pub fn default_anthropic_auth_file() -> Option<PathBuf> {
    if let Some(path) = env_value("NANOCODEX_ANTHROPIC_AUTH_FILE") {
        return Some(PathBuf::from(path));
    }
    if let Some(home) = env_value("CODEX_HOME") {
        return Some(PathBuf::from(home).join("anthropic_auth.json"));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".codex/anthropic_auth.json"))
}

/// Loads a nanocodex-owned Anthropic login with the Claude Code registration.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read or is invalid.
pub fn load_stored_anthropic_auth(
    auth_file: impl Into<PathBuf>,
) -> Result<AnthropicAuth, AnthropicLoginError> {
    load_stored_anthropic_auth_with_config(auth_file, AnthropicOAuthConfig::default())
}

/// Loads a nanocodex-owned Anthropic login with an explicit client registration.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read or is invalid.
pub fn load_stored_anthropic_auth_with_config(
    auth_file: impl Into<PathBuf>,
    config: AnthropicOAuthConfig,
) -> Result<AnthropicAuth, AnthropicLoginError> {
    let auth_file = auth_file.into();
    let credentials = read_store(&auth_file)?;
    credentials.validate(&auth_file)?;
    let manager = ManagedAnthropicAuth {
        auth_file,
        config,
        client: auth_client()?,
        state: RwLock::new(ManagedState {
            credentials,
            revision: 0,
            permanent_failure: None,
        }),
        refresh: Mutex::new(()),
    };
    Ok(AnthropicAuth::managed_oauth(Arc::new(manager)))
}

/// Inspects a stored Anthropic login without exposing its tokens.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read or is invalid.
pub fn stored_anthropic_status(
    auth_file: impl AsRef<Path>,
) -> Result<AnthropicLoginStatus, AnthropicLoginError> {
    let auth_file = auth_file.as_ref();
    let credentials = read_store(auth_file)?;
    credentials.validate(auth_file)?;
    Ok(credentials.status())
}

/// Removes a stored Anthropic login. A missing file is treated as logged out.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be removed.
pub fn logout_anthropic(auth_file: impl AsRef<Path>) -> Result<bool, AnthropicLoginError> {
    let auth_file = auth_file.as_ref();
    match std::fs::remove_file(auth_file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(AnthropicLoginError::Storage {
            path: auth_file.to_path_buf(),
            source,
        }),
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct StoredCredentials {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scopes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

impl StoredCredentials {
    fn validate(&self, path: &Path) -> Result<(), AnthropicLoginError> {
        if self.access_token.trim().is_empty() {
            return Err(AnthropicLoginError::InvalidStore {
                path: path.to_path_buf(),
                detail: "the stored access token is empty".into(),
            });
        }
        Ok(())
    }

    fn status(&self) -> AnthropicLoginStatus {
        AnthropicLoginStatus {
            account_id: self.account_id.clone(),
            email: self.email.clone(),
            expires_in_seconds: self.expires_at.map(|at| at.saturating_sub(unix_now())),
        }
    }

    fn is_stale(&self) -> bool {
        self.expires_at
            .is_some_and(|expiry| expiry <= unix_now() + REFRESH_EARLY_SECONDS)
    }
}

struct ManagedAnthropicAuth {
    auth_file: PathBuf,
    config: AnthropicOAuthConfig,
    client: reqwest::Client,
    state: RwLock<ManagedState>,
    refresh: Mutex<()>,
}

struct ManagedState {
    credentials: StoredCredentials,
    revision: u64,
    permanent_failure: Option<Arc<str>>,
}

impl ManagedAnthropicAuth {
    fn state(&self) -> Result<std::sync::RwLockReadGuard<'_, ManagedState>, AnthropicAuthError> {
        self.state
            .read()
            .map_err(|_| AnthropicAuthError::Unavailable(Arc::from("authorization state poisoned")))
    }

    fn snapshot_now(&self) -> Result<AnthropicAuthSnapshot, AnthropicAuthError> {
        let state = self.state()?;
        if let Some(error) = &state.permanent_failure {
            return Err(AnthropicAuthError::LoginRequired(Arc::clone(error)));
        }
        Ok(AnthropicAuthSnapshot::new(
            AnthropicAuthMode::OAuth,
            Arc::<str>::from(state.credentials.access_token.as_str()),
            Some(Arc::<str>::from(ANTHROPIC_OAUTH_BETA)),
            state.revision,
        ))
    }

    /// Refreshes unless another caller already moved past `rejected_revision`.
    ///
    /// When `reload` is set, a rotation written by another process is adopted first so
    /// a token someone else already refreshed does not consume this one.
    async fn refresh_if_current(
        &self,
        rejected_revision: u64,
        reload: bool,
    ) -> Result<(), AnthropicAuthError> {
        let _guard = self.refresh.lock().await;
        if self.state()?.revision != rejected_revision {
            return Ok(());
        }
        if reload && self.reload_if_changed()? {
            return Ok(());
        }

        let (refresh_token, scopes) = {
            let state = self.state()?;
            let refresh_token = state.credentials.refresh_token.clone().ok_or_else(|| {
                AnthropicAuthError::LoginRequired(Arc::from(
                    "the stored login has no refresh token; log in again",
                ))
            })?;
            let scopes = state
                .credentials
                .scopes
                .as_deref()
                .filter(|scopes| !scopes.trim().is_empty())
                .unwrap_or(&self.config.scopes)
                .to_owned();
            (refresh_token, scopes)
        };
        let request = RefreshTokenRequest {
            grant_type: "refresh_token",
            refresh_token: &refresh_token,
            client_id: &self.config.client_id,
            scope: (!scopes.trim().is_empty()).then_some(scopes.as_str()),
        };
        let response = self
            .client
            .post(self.config.token_endpoint())
            .json(&request)
            .send()
            .await
            .map_err(|error| AnthropicAuthError::Refresh(Arc::from(error.to_string())))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| AnthropicAuthError::Refresh(Arc::from(error.to_string())))?;
        if !status.is_success() {
            let code = error_code(&body);
            let permanent = status == reqwest::StatusCode::UNAUTHORIZED
                || matches!(
                    code.as_deref(),
                    Some("invalid_grant" | "invalid_client" | "unauthorized_client")
                );
            let detail: Arc<str> =
                Arc::from(code.unwrap_or_else(|| format!("token endpoint returned HTTP {status}")));
            if permanent {
                self.state
                    .write()
                    .map_err(|_| {
                        AnthropicAuthError::Unavailable(Arc::from("authorization state poisoned"))
                    })?
                    .permanent_failure = Some(Arc::clone(&detail));
                return Err(AnthropicAuthError::LoginRequired(detail));
            }
            return Err(AnthropicAuthError::Refresh(detail));
        }
        let refreshed: TokenResponse = serde_json::from_slice(&body)
            .map_err(|error| AnthropicAuthError::Refresh(Arc::from(error.to_string())))?;
        if refreshed.access_token.trim().is_empty() {
            return Err(AnthropicAuthError::Refresh(Arc::from(
                "token endpoint returned an empty access token",
            )));
        }
        self.apply_refresh(refreshed)
    }

    fn reload_if_changed(&self) -> Result<bool, AnthropicAuthError> {
        let stored = read_store(&self.auth_file).map_err(|error| store_error(&error))?;
        stored
            .validate(&self.auth_file)
            .map_err(|error| store_error(&error))?;
        let mut state = self.state.write().map_err(|_| {
            AnthropicAuthError::Unavailable(Arc::from("authorization state poisoned"))
        })?;
        if stored.account_id.is_some()
            && state.credentials.account_id.is_some()
            && stored.account_id != state.credentials.account_id
        {
            return Err(AnthropicAuthError::AccountChanged);
        }
        if stored.access_token == state.credentials.access_token {
            return Ok(false);
        }
        state.credentials = stored;
        state.revision = state.revision.wrapping_add(1);
        state.permanent_failure = None;
        Ok(true)
    }

    fn apply_refresh(&self, refreshed: TokenResponse) -> Result<(), AnthropicAuthError> {
        let mut state = self.state.write().map_err(|_| {
            AnthropicAuthError::Unavailable(Arc::from("authorization state poisoned"))
        })?;
        let mut next = state.credentials.clone();
        next.access_token = refreshed.access_token;
        // Refresh tokens may rotate; keep the previous one when the server omits it.
        if let Some(refresh_token) = refreshed.refresh_token {
            next.refresh_token = Some(refresh_token);
        }
        if refreshed.scope.is_some() {
            next.scopes = refreshed.scope;
        }
        next.expires_at = refreshed.expires_in.map(|seconds| unix_now() + seconds);
        write_store(&self.auth_file, &next).map_err(|error| store_error(&error))?;
        state.credentials = next;
        state.revision = state.revision.wrapping_add(1);
        state.permanent_failure = None;
        Ok(())
    }
}

impl AnthropicAuthSource for ManagedAnthropicAuth {
    fn validate(&self) -> Result<(), AnthropicAuthError> {
        self.snapshot_now().map(|_| ())
    }

    fn snapshot(
        &self,
    ) -> AnthropicAuthFuture<'_, Result<AnthropicAuthSnapshot, AnthropicAuthError>> {
        Box::pin(async move {
            let snapshot = self.snapshot_now()?;
            if self.state()?.credentials.is_stale()
                && let Err(error) = self.refresh_if_current(snapshot.revision(), true).await
            {
                tracing::warn!(error = %error, "proactive Anthropic token refresh failed");
            }
            self.snapshot_now()
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &AnthropicAuthSnapshot,
    ) -> AnthropicAuthFuture<'_, Result<(), AnthropicAuthError>> {
        let revision = rejected.revision();
        Box::pin(async move { self.refresh_if_current(revision, true).await })
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    account: Option<TokenAccount>,
}

#[derive(Deserialize)]
struct TokenAccount {
    uuid: Option<String>,
    email_address: Option<String>,
}

impl From<TokenResponse> for StoredCredentials {
    fn from(response: TokenResponse) -> Self {
        let account_id = response.account_id.or_else(|| {
            response
                .account
                .as_ref()
                .and_then(|account| account.uuid.clone())
        });
        let email = response
            .email
            .or_else(|| response.account.and_then(|account| account.email_address));
        Self {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            scopes: response.scope,
            expires_at: response.expires_in.map(|seconds| unix_now() + seconds),
            account_id,
            email,
        }
    }
}

#[derive(Serialize)]
struct AuthorizationCodeRequest<'a> {
    grant_type: &'static str,
    code: &'a str,
    redirect_uri: &'a str,
    client_id: &'a str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(Serialize)]
struct RefreshTokenRequest<'a> {
    grant_type: &'static str,
    refresh_token: &'a str,
    client_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn store_error(error: &AnthropicLoginError) -> AnthropicAuthError {
    AnthropicAuthError::Unavailable(Arc::from(error.to_string()))
}

fn read_store(path: &Path) -> Result<StoredCredentials, AnthropicLoginError> {
    let bytes = std::fs::read(path).map_err(|source| AnthropicLoginError::Storage {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|error| AnthropicLoginError::InvalidStore {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

/// Writes credentials atomically with owner-only permissions.
fn write_store(path: &Path, credentials: &StoredCredentials) -> Result<(), AnthropicLoginError> {
    let parent = path.parent().ok_or_else(|| AnthropicLoginError::Storage {
        path: path.to_path_buf(),
        source: io::Error::new(ErrorKind::InvalidInput, "auth file has no parent directory"),
    })?;
    std::fs::create_dir_all(parent).map_err(|source| AnthropicLoginError::Storage {
        path: parent.to_path_buf(),
        source,
    })?;

    let temporary = path.with_extension(format!("json.{}.tmp", random_urlsafe()?));
    let bytes = serde_json::to_vec_pretty(credentials).map_err(|error| {
        AnthropicLoginError::InvalidStore {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    })?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|source| AnthropicLoginError::Storage {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temporary);
        return Err(AnthropicLoginError::Storage {
            path: temporary,
            source,
        });
    }
    drop(file);
    if let Err(source) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(AnthropicLoginError::Storage {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn auth_client() -> Result<reqwest::Client, AnthropicLoginError> {
    reqwest::Client::builder()
        .timeout(AUTH_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| AnthropicLoginError::TokenExchange(error.to_string()))
}

fn error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("error")
        .and_then(|error| match error {
            serde_json::Value::String(code) => Some(code.as_str()),
            serde_json::Value::Object(error) => error.get("type")?.as_str(),
            _ => None,
        })
        .map(str::to_owned)
}

async fn bind_callback(fixed: Option<u16>) -> Result<TcpListener, AnthropicLoginError> {
    if let Some(port) = fixed {
        return TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| AnthropicLoginError::CallbackUnavailable);
    }
    for port in CALLBACK_PORTS {
        if let Ok(listener) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            return Ok(listener);
        }
    }
    Err(AnthropicLoginError::CallbackUnavailable)
}

async fn receive_callback(
    listener: &TcpListener,
    redirect_path: &str,
) -> Result<OAuthCallback, AnthropicLoginError> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
    let mut bytes = Vec::with_capacity(2048);
    loop {
        let read = stream
            .read_buf(&mut bytes)
            .await
            .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
        if read == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > 16 * 1024 {
            return Err(AnthropicLoginError::LoginRejected(
                "OAuth callback request was too large".into(),
            ));
        }
    }
    let request = std::str::from_utf8(&bytes)
        .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .filter(|target| target.starts_with(redirect_path))
        .ok_or_else(|| AnthropicLoginError::LoginRejected("invalid callback request".into()))?;
    Ok(OAuthCallback {
        target: target.to_owned(),
        stream,
    })
}

fn authorize_url(
    config: &AnthropicOAuthConfig,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<String, AnthropicLoginError> {
    let mut url = Url::parse(&config.authorize_endpoint())
        .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    // An unknown scope is rejected before the consent screen, so send none unless the
    // registration defines one.
    if !config.scopes.trim().is_empty() {
        url.query_pairs_mut().append_pair("scope", &config.scopes);
    }
    Ok(url.into())
}

async fn exchange_code(
    client: &reqwest::Client,
    config: &AnthropicOAuthConfig,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    state: &str,
) -> Result<StoredCredentials, AnthropicLoginError> {
    let request = AuthorizationCodeRequest {
        grant_type: "authorization_code",
        code,
        redirect_uri,
        client_id: &config.client_id,
        code_verifier,
        state,
    };
    let response = client
        .post(config.token_endpoint())
        .json(&request)
        .send()
        .await
        .map_err(|error| AnthropicLoginError::TokenExchange(error.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| AnthropicLoginError::TokenExchange(error.to_string()))?;
    if !status.is_success() {
        let code =
            error_code(&bytes).unwrap_or_else(|| format!("token endpoint returned HTTP {status}"));
        return Err(AnthropicLoginError::TokenExchange(code));
    }
    let tokens: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|error| AnthropicLoginError::InvalidToken(error.to_string()))?;
    if tokens.access_token.trim().is_empty() {
        return Err(AnthropicLoginError::InvalidToken(
            "token response had no access token".into(),
        ));
    }
    let mut credentials: StoredCredentials = tokens.into();
    if credentials.scopes.is_none() && !config.scopes.trim().is_empty() {
        credentials.scopes = Some(config.scopes.clone());
    }
    Ok(credentials)
}

fn random_urlsafe() -> Result<String, AnthropicLoginError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| AnthropicLoginError::LoginRejected(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use super::AnthropicAuth;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Mutex,
    };

    use super::{
        AnthropicOAuthConfig, AuthorizationCodeRequest, DEFAULT_AUTHORIZE_ENDPOINT,
        DEFAULT_CLIENT_ID, DEFAULT_SCOPES, DEFAULT_TOKEN_ENDPOINT, ManagedAnthropicAuth,
        ManagedState, RefreshTokenRequest, StoredCredentials, TokenResponse, auth_client,
        authorize_url, error_code, load_stored_anthropic_auth, read_store, stored_anthropic_status,
        unix_now, write_store,
    };

    fn config() -> AnthropicOAuthConfig {
        AnthropicOAuthConfig {
            issuer: "https://auth.example.com".to_owned(),
            client_id: "client-123".to_owned(),
            scopes: String::new(),
            redirect_path: super::DEFAULT_CALLBACK_PATH.to_owned(),
            redirect_port: None,
            authorize_endpoint: None,
            token_endpoint: None,
        }
    }

    fn credentials(access: &str, expires_at: Option<i64>) -> StoredCredentials {
        StoredCredentials {
            access_token: access.to_owned(),
            refresh_token: Some("refresh-1".to_owned()),
            scopes: Some("user:inference".to_owned()),
            expires_at,
            account_id: Some("acct-1".to_owned()),
            email: Some("user@example.com".to_owned()),
        }
    }

    fn temp_file() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nanocodex-anthropic-test-{}.json",
            super::random_urlsafe().unwrap()
        ))
    }

    fn managed(
        auth_file: &std::path::Path,
        config: AnthropicOAuthConfig,
        credentials: StoredCredentials,
    ) -> AnthropicAuth {
        AnthropicAuth::managed_oauth(Arc::new(ManagedAnthropicAuth {
            auth_file: auth_file.to_path_buf(),
            config,
            client: auth_client().unwrap(),
            state: RwLock::new(ManagedState {
                credentials,
                revision: 0,
                permanent_failure: None,
            }),
            refresh: Mutex::new(()),
        }))
    }

    #[test]
    fn default_config_matches_claude_code() {
        let config = AnthropicOAuthConfig::default();
        assert_eq!(config.client_id, DEFAULT_CLIENT_ID);
        assert_eq!(config.scopes, DEFAULT_SCOPES);
        assert_eq!(config.redirect_path, "/callback");
        assert_eq!(config.authorize_endpoint(), DEFAULT_AUTHORIZE_ENDPOINT);
        assert_eq!(config.token_endpoint(), DEFAULT_TOKEN_ENDPOINT);
    }

    #[test]
    fn default_authorization_url_matches_claude_code() {
        let url = authorize_url(
            &AnthropicOAuthConfig::default(),
            "http://localhost:1456/callback",
            "state-1",
            "challenge-1",
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let query = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            parsed.as_str().split('?').next(),
            Some(DEFAULT_AUTHORIZE_ENDPOINT)
        );
        assert_eq!(query.get("client_id").unwrap(), DEFAULT_CLIENT_ID);
        assert_eq!(query.get("scope").unwrap(), DEFAULT_SCOPES);
        assert_eq!(
            query.get("redirect_uri").unwrap(),
            "http://localhost:1456/callback"
        );
        assert_eq!(query.get("code").unwrap(), "true");
    }

    #[test]
    fn environment_values_override_defaults_independently() {
        let config = AnthropicOAuthConfig::with_overrides(|name| match name {
            "NANOCODEX_ANTHROPIC_OAUTH_CLIENT_ID" => Some("private-client".to_owned()),
            "NANOCODEX_ANTHROPIC_OAUTH_SCOPES" => Some("user:inference".to_owned()),
            "NANOCODEX_ANTHROPIC_OAUTH_PORT" => Some("9876".to_owned()),
            _ => None,
        });
        assert_eq!(config.client_id, "private-client");
        assert_eq!(config.scopes, "user:inference");
        assert_eq!(config.redirect_port, Some(9876));
        assert_eq!(config.authorize_endpoint(), DEFAULT_AUTHORIZE_ENDPOINT);
        assert_eq!(config.token_endpoint(), DEFAULT_TOKEN_ENDPOINT);
    }

    #[test]
    fn issuer_override_derives_endpoints_unless_they_are_explicit() {
        let config = AnthropicOAuthConfig::with_overrides(|name| match name {
            "NANOCODEX_ANTHROPIC_OAUTH_ISSUER" => Some("https://auth.example.com/".to_owned()),
            "NANOCODEX_ANTHROPIC_OAUTH_TOKEN_ENDPOINT" => {
                Some("https://tokens.example.com/exchange".to_owned())
            }
            _ => None,
        });
        assert_eq!(
            config.authorize_endpoint(),
            "https://auth.example.com/oauth/authorize"
        );
        assert_eq!(
            config.token_endpoint(),
            "https://tokens.example.com/exchange"
        );
    }

    #[test]
    fn the_authorization_url_carries_the_pkce_contract() {
        let url = authorize_url(
            &config(),
            "http://localhost:1456/auth/callback",
            "state-1",
            "chal-1",
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let query = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(parsed.path(), "/oauth/authorize");
        assert_eq!(query.get("response_type").unwrap(), "code");
        assert_eq!(query.get("code").unwrap(), "true");
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query.get("code_challenge").unwrap(), "chal-1");
        assert_eq!(query.get("state").unwrap(), "state-1");
    }

    #[test]
    fn token_requests_match_claude_codes_json_contract() {
        let exchange = AuthorizationCodeRequest {
            grant_type: "authorization_code",
            code: "code-1",
            redirect_uri: "http://localhost:1456/callback",
            client_id: "client-1",
            code_verifier: "verifier-1",
            state: "state-1",
        };
        assert_eq!(
            serde_json::to_value(exchange).unwrap(),
            serde_json::json!({
                "grant_type": "authorization_code",
                "code": "code-1",
                "redirect_uri": "http://localhost:1456/callback",
                "client_id": "client-1",
                "code_verifier": "verifier-1",
                "state": "state-1"
            })
        );

        let refresh = RefreshTokenRequest {
            grant_type: "refresh_token",
            refresh_token: "refresh-1",
            client_id: "client-1",
            scope: Some("user:profile user:inference"),
        };
        assert_eq!(
            serde_json::to_value(refresh).unwrap(),
            serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": "refresh-1",
                "client_id": "client-1",
                "scope": "user:profile user:inference"
            })
        );
    }

    #[test]
    fn token_account_metadata_is_retained() {
        let response: TokenResponse = serde_json::from_value(serde_json::json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "expires_in": 3600,
            "scope": "user:profile user:inference",
            "account": {
                "uuid": "account-1",
                "email_address": "user@example.com"
            }
        }))
        .unwrap();
        let stored: StoredCredentials = response.into();
        assert_eq!(stored.account_id.as_deref(), Some("account-1"));
        assert_eq!(stored.email.as_deref(), Some("user@example.com"));
        assert_eq!(
            stored.scopes.as_deref(),
            Some("user:profile user:inference")
        );
    }

    #[tokio::test]
    async fn expired_login_refreshes_with_json_and_its_granted_scopes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_endpoint = format!("http://{}/v1/oauth/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let read = stream.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                request.extend_from_slice(&chunk[..read]);
                let Some(headers_end) = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|position| position + 4)
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .and_then(|length| length.parse::<usize>().ok())
                    })
                    .unwrap();
                if request.len() >= headers_end + content_length {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("POST /v1/oauth/token HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("content-type: application/json")
            );
            let body = request.split_once("\r\n\r\n").unwrap().1;
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(
                body,
                serde_json::json!({
                    "grant_type": "refresh_token",
                    "refresh_token": "refresh-1",
                    "client_id": DEFAULT_CLIENT_ID,
                    "scope": "user:profile user:inference"
                })
            );

            let response = serde_json::json!({
                "access_token": "access-2",
                "refresh_token": "refresh-2",
                "expires_in": 3600,
                "scope": "user:inference"
            })
            .to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let auth_file = temp_file();
        let mut original = credentials("access-1", Some(unix_now() - 1));
        original.scopes = Some("user:profile user:inference".to_owned());
        write_store(&auth_file, &original).unwrap();
        let config = AnthropicOAuthConfig {
            token_endpoint: Some(token_endpoint),
            ..AnthropicOAuthConfig::default()
        };
        let auth = managed(&auth_file, config, original);

        let snapshot = auth.snapshot().await.unwrap();
        assert_eq!(snapshot.bearer(), "access-2");
        let stored = read_store(&auth_file).unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-2"));
        assert_eq!(stored.scopes.as_deref(), Some("user:inference"));
        server.await.unwrap();
        std::fs::remove_file(auth_file).unwrap();
    }

    #[test]
    fn an_unset_scope_is_omitted_rather_than_sent_empty() {
        let url =
            authorize_url(&config(), "http://localhost:1456/auth/callback", "s", "c").unwrap();
        assert!(
            !url.contains("scope="),
            "an unknown or empty scope is rejected before the consent screen: {url}"
        );

        let mut scoped = config();
        scoped.scopes = "user:inference".to_owned();
        let url = authorize_url(&scoped, "http://localhost:1456/auth/callback", "s", "c").unwrap();
        assert!(url.contains("scope=user%3Ainference"), "{url}");
    }

    #[test]
    fn the_redirect_path_is_configurable_to_match_a_registration() {
        let mut config = config();
        config.redirect_path = "/callback".to_owned();
        let url = authorize_url(&config, "http://localhost:1456/callback", "s", "c").unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let query = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("redirect_uri").unwrap(),
            "http://localhost:1456/callback",
            "the redirect must be sent exactly as registered"
        );
    }

    #[test]
    fn explicit_endpoints_override_the_oauth_defaults() {
        let mut config = config();
        config.token_endpoint = Some("https://auth.example.com/api/oauth/create_key".to_owned());
        config.authorize_endpoint = Some("https://auth.example.com/authorize".to_owned());
        assert_eq!(
            config.token_endpoint(),
            "https://auth.example.com/api/oauth/create_key"
        );
        assert_eq!(
            config.authorize_endpoint(),
            "https://auth.example.com/authorize"
        );
    }

    #[test]
    fn endpoints_tolerate_a_trailing_slash_on_the_issuer() {
        let mut config = config();
        config.issuer = "https://auth.example.com/".to_owned();
        assert_eq!(
            config.token_endpoint(),
            "https://auth.example.com/oauth/token"
        );
        assert_eq!(
            config.authorize_endpoint(),
            "https://auth.example.com/oauth/authorize"
        );
    }

    #[test]
    fn credentials_round_trip_and_are_owner_only() {
        let path = temp_file();
        let original = credentials("access-1", Some(unix_now() + 3600));
        write_store(&path, &original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0,
                "credentials must not be group or world readable"
            );
        }
        let loaded = read_store(&path).unwrap();
        assert_eq!(loaded.access_token, "access-1");
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh-1"));

        let status = stored_anthropic_status(&path).unwrap();
        assert_eq!(status.email.as_deref(), Some("user@example.com"));
        assert!(status.expires_in_seconds.unwrap() > 0);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn default_loader_uses_the_embedded_registration() {
        let path = temp_file();
        write_store(&path, &credentials("access-1", Some(unix_now() + 3600))).unwrap();
        let auth = load_stored_anthropic_auth(&path).unwrap();
        assert_eq!(auth.snapshot().await.unwrap().bearer(), "access-1");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn an_empty_access_token_is_rejected_rather_than_stored_as_valid() {
        let path = temp_file();
        write_store(&path, &credentials("   ", None)).unwrap();
        assert!(stored_anthropic_status(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn staleness_uses_the_early_refresh_window() {
        assert!(
            !credentials("a", None).is_stale(),
            "no expiry means use until rejected"
        );
        assert!(credentials("a", Some(unix_now() + 60)).is_stale());
        assert!(!credentials("a", Some(unix_now() + 3600)).is_stale());
    }

    #[test]
    fn oauth_error_bodies_are_recognized_in_both_shapes() {
        assert_eq!(
            error_code(br#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(
            error_code(br#"{"error":{"type":"invalid_client"}}"#).as_deref(),
            Some("invalid_client")
        );
        assert!(error_code(b"not json").is_none());
    }
}
