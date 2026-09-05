use std::{
    collections::BTreeMap,
    fmt,
    fs::OpenOptions,
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
    time::{Duration, timeout},
};
use url::Url;

const AUTH_ISSUER: &str = "https://auth.openai.com";
const AUTH_API_BASE_URL: &str = "https://auth.openai.com/api/accounts";
const AUTH_API_BASE_URL_ENV: &str = "CODEX_AUTHAPI_BASE_URL";
const PERSONAL_ACCESS_TOKEN_PREFIX: &str = "at-";
const WHOAMI_PATH: &str = "/v1/user-auth-credential/whoami";
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const CALLBACK_PATH: &str = "/auth/callback";
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const REFRESH_EARLY_SECONDS: i64 = 5 * 60;
const LOGIN_TIMEOUT: Duration = Duration::from_mins(5);
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Non-secret information about a stored `ChatGPT` authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatGptAuthStatus {
    /// Stable account identifier attached to API requests.
    pub account_id: String,
    /// Account email decoded from the stored identity token, when present.
    pub email: Option<String>,
    /// Subscription plan decoded from the stored identity token, when present.
    pub plan: Option<String>,
    /// Whether this credential targets the `FedRAMP` service boundary.
    pub fedramp: bool,
}

/// Failure while logging in, loading, persisting, or removing `ChatGPT` credentials.
#[derive(Debug, thiserror::Error)]
pub enum ChatGptAuthError {
    /// A credential file could not be read, written, or removed.
    #[error("failed to access ChatGPT authorization file {path}: {source}")]
    Storage {
        /// Credential file involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// A credential file was readable but did not contain a valid session.
    #[error("ChatGPT authorization file {path} is invalid: {detail}")]
    InvalidStore {
        /// Invalid credential file.
        path: PathBuf,
        /// Validation failure without token contents.
        detail: String,
    },
    /// The authorization server returned a token that could not be validated.
    #[error("ChatGPT OAuth response was invalid: {0}")]
    InvalidToken(String),
    /// Neither supported loopback callback port could be bound.
    #[error("could not listen for the OAuth callback on localhost ports 1455 or 1457")]
    CallbackUnavailable,
    /// The browser did not return to the loopback callback before its deadline.
    #[error("timed out waiting for the ChatGPT OAuth callback")]
    CallbackTimeout,
    /// The callback state did not match the login attempt.
    #[error("the OAuth callback did not match this login attempt")]
    StateMismatch,
    /// The authorization request was rejected or returned an invalid callback.
    #[error("ChatGPT login was rejected: {0}")]
    LoginRejected(String),
    /// Authorization-code or refresh-token exchange failed.
    #[error("ChatGPT token exchange failed: {0}")]
    TokenExchange(String),
    /// Persistent access-token metadata could not be resolved.
    #[error("ChatGPT access-token metadata request failed: {0}")]
    AccessTokenMetadata(String),
}

/// An in-progress authorization-code login using PKCE and a loopback callback.
///
/// Start the login, open [`authorization_url`](Self::authorization_url) in the user's browser,
/// then await [`complete`](Self::complete). Completion persists the credentials before returning.
pub struct ChatGptLogin {
    issuer: String,
    authorization_url: String,
    redirect_uri: String,
    state: String,
    code_verifier: String,
    auth_file: PathBuf,
    listener: TcpListener,
    client: reqwest::Client,
}

impl fmt::Debug for ChatGptLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatGptLogin")
            .field("issuer", &"[redacted]")
            .field("authorization_url", &"[redacted]")
            .field("redirect_uri", &self.redirect_uri)
            .field("state", &"[redacted]")
            .field("code_verifier", &"[redacted]")
            .field("auth_file", &self.auth_file)
            .finish_non_exhaustive()
    }
}

impl ChatGptLogin {
    /// Starts a loopback OAuth login and returns the browser authorization URL.
    ///
    /// # Errors
    ///
    /// Returns an error when secure random data cannot be generated or neither callback port can
    /// be bound.
    pub async fn start(auth_file: impl Into<PathBuf>) -> Result<Self, ChatGptAuthError> {
        Self::start_with_issuer(auth_file.into(), AUTH_ISSUER).await
    }

    async fn start_with_issuer(auth_file: PathBuf, issuer: &str) -> Result<Self, ChatGptAuthError> {
        let listener = bind_callback().await?;
        let port = listener
            .local_addr()
            .map_err(|_| ChatGptAuthError::CallbackUnavailable)?
            .port();
        let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
        let state = random_urlsafe()?;
        let code_verifier = random_urlsafe()?;
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let authorization_url = authorize_url(issuer, &redirect_uri, &state, &code_challenge)?;

        Ok(Self {
            issuer: issuer.to_owned(),
            authorization_url,
            redirect_uri,
            state,
            code_verifier,
            auth_file,
            listener,
            client: auth_client()?,
        })
    }

    /// Returns the URL that the user must open to authorize this login.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Waits for the callback, exchanges the code, and atomically persists the credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the callback is invalid, the exchange fails, or credentials cannot
    /// be persisted.
    pub async fn complete(self) -> Result<ChatGptAuthStatus, ChatGptAuthError> {
        let callback = timeout(LOGIN_TIMEOUT, receive_callback(&self.listener))
            .await
            .map_err(|_| ChatGptAuthError::CallbackTimeout)??;
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
    ) -> Result<ChatGptAuthStatus, ChatGptAuthError> {
        let callback = Url::parse(&format!("http://localhost{callback_target}"))
            .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
        if callback.path() != CALLBACK_PATH {
            return Err(ChatGptAuthError::LoginRejected(
                "invalid callback path".into(),
            ));
        }
        let query = callback
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<std::collections::HashMap<_, _>>();
        if query.get("state") != Some(&self.state) {
            return Err(ChatGptAuthError::StateMismatch);
        }
        if let Some(error) = query.get("error") {
            let detail = query
                .get("error_description")
                .map_or(error.as_str(), String::as_str);
            return Err(ChatGptAuthError::LoginRejected(detail.to_owned()));
        }
        let code = query
            .get("code")
            .ok_or_else(|| ChatGptAuthError::LoginRejected("missing authorization code".into()))?;
        let credentials = exchange_code(
            &self.client,
            &self.issuer,
            code,
            &self.redirect_uri,
            &self.code_verifier,
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
    async fn reply(mut self, success: bool) -> Result<(), ChatGptAuthError> {
        let (status, body): (&str, &[u8]) = if success {
            (
                "200 OK",
                b"ChatGPT login completed. You can close this window.",
            )
        } else {
            (
                "400 Bad Request",
                b"ChatGPT login failed. Return to the terminal for details.",
            )
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        self.stream
            .write_all(response.as_bytes())
            .await
            .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
        self.stream
            .write_all(body)
            .await
            .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))
    }
}

/// Loads a persisted `ChatGPT` credential as shared authorization for an agent family.
///
/// The file uses Codex's `auth.json` format and can be shared with Codex and other Nanocodex
/// processes. OAuth handles reload same-account rotations before refreshing, serialize refreshes,
/// and atomically preserve unknown fields. Persistent access tokens resolve account metadata once
/// and never enter the OAuth refresh path.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read or is invalid.
pub fn load_chatgpt_auth(auth_file: impl Into<PathBuf>) -> Result<OpenAiAuth, ChatGptAuthError> {
    let auth_file = auth_file.into();
    let document = read_document(&auth_file)?;
    if let Some(access_token) = personal_access_token_from_document(&document, &auth_file)? {
        return chatgpt_access_token(access_token);
    }
    let (credentials, auth_record) = read_stored_auth(&auth_file)?;
    credentials.validate(&auth_file)?;
    let manager = ManagedChatGptAuth {
        auth_file,
        issuer: Arc::from(AUTH_ISSUER),
        client: auth_client()?,
        state: RwLock::new(ManagedState {
            credentials,
            auth_record,
            revision: 0,
            permanent_failure: None,
        }),
        refresh: Mutex::new(()),
    };
    Ok(OpenAiAuth::managed_chatgpt(Arc::new(manager)))
}

/// Creates `ChatGPT` authorization from a persistent Business or Enterprise access token.
///
/// The token must use Codex's `at-` prefix. Account routing metadata is resolved lazily from the
/// ChatGPT authorization service on the first request and reused for the lifetime of the handle.
/// Persistent access tokens do not participate in OAuth refresh.
///
/// # Errors
///
/// Returns an error when the token is empty, has the wrong credential type, or the authorization
/// client cannot be constructed.
pub fn chatgpt_access_token(
    access_token: impl Into<Arc<str>>,
) -> Result<OpenAiAuth, ChatGptAuthError> {
    personal_access_token_auth(access_token.into(), &auth_api_base_url())
}

/// Resolves non-secret account information for any supported Codex `auth.json` credential.
///
/// Unlike [`chatgpt_auth_status`], this async variant also supports persistent access tokens,
/// whose account metadata must be fetched from the authorization service.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read, is invalid, or its access-token
/// metadata cannot be resolved.
pub async fn resolve_chatgpt_auth_status(
    auth_file: impl AsRef<Path>,
) -> Result<ChatGptAuthStatus, ChatGptAuthError> {
    let auth_file = auth_file.as_ref();
    let document = read_document(auth_file)?;
    if let Some(access_token) = personal_access_token_from_document(&document, auth_file)? {
        return hydrate_personal_access_token(
            &auth_client()?,
            &whoami_endpoint(&auth_api_base_url()),
            &access_token,
        )
        .await;
    }
    let credentials = StoredCredentials::from_document(&document, auth_file)?;
    credentials.validate(auth_file)?;
    Ok(credentials.status())
}

/// Inspect a stored `ChatGPT` OAuth authorization without exposing its tokens.
///
/// Use [`resolve_chatgpt_auth_status`] when the file may contain a persistent access token.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read or is invalid.
pub fn chatgpt_auth_status(
    auth_file: impl AsRef<Path>,
) -> Result<ChatGptAuthStatus, ChatGptAuthError> {
    let auth_file = auth_file.as_ref();
    let credentials = read_store(auth_file)?;
    credentials.validate(auth_file)?;
    Ok(credentials.status())
}

/// Remove locally stored `ChatGPT` credentials. Missing files are treated as logged out.
///
/// # Errors
///
/// Returns an error when the credential file exists but cannot be removed.
pub fn logout_chatgpt(auth_file: impl AsRef<Path>) -> Result<bool, ChatGptAuthError> {
    let auth_file = auth_file.as_ref();
    match std::fs::remove_file(auth_file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ChatGptAuthError::Storage {
            path: auth_file.to_path_buf(),
            source,
        }),
    }
}

#[derive(Clone)]
struct StoredCredentials {
    id_token: String,
    access_token: String,
    refresh_token: String,
    account_id: String,
    email: Option<String>,
    plan: Option<String>,
    fedramp: bool,
}

impl StoredCredentials {
    fn from_tokens(tokens: TokenResponse) -> Result<Self, ChatGptAuthError> {
        let claims: IdClaims = decode_jwt(&tokens.id)?;
        let auth = claims.auth.unwrap_or_default();
        let account_id = auth.account_id.ok_or_else(|| {
            ChatGptAuthError::InvalidToken("ID token has no ChatGPT account ID".into())
        })?;
        Ok(Self {
            id_token: tokens.id,
            access_token: tokens.access,
            refresh_token: tokens.refresh,
            account_id,
            email: claims
                .email
                .or_else(|| claims.profile.and_then(|profile| profile.email)),
            plan: auth.plan,
            fedramp: auth.fedramp,
        })
    }

    fn from_document(document: &CodexAuthDocument, path: &Path) -> Result<Self, ChatGptAuthError> {
        if document
            .auth_mode
            .as_deref()
            .is_some_and(|mode| mode != "chatgpt")
        {
            return Err(ChatGptAuthError::InvalidStore {
                path: path.to_path_buf(),
                detail: "Codex is not logged in with ChatGPT".into(),
            });
        }
        let tokens = document
            .tokens
            .as_ref()
            .ok_or_else(|| ChatGptAuthError::InvalidStore {
                path: path.to_path_buf(),
                detail: "Codex auth.json has no ChatGPT tokens".into(),
            })?;
        let claims: IdClaims = decode_jwt(&tokens.id_token)?;
        let auth = claims.auth.unwrap_or_default();
        let account_id = tokens
            .account_id
            .clone()
            .or(auth.account_id)
            .ok_or_else(|| ChatGptAuthError::InvalidStore {
                path: path.to_path_buf(),
                detail: "Codex auth.json has no ChatGPT account ID".into(),
            })?;
        let credentials = Self {
            id_token: tokens.id_token.clone(),
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            account_id,
            email: claims
                .email
                .or_else(|| claims.profile.and_then(|profile| profile.email)),
            plan: auth.plan,
            fedramp: auth.fedramp,
        };
        credentials.validate(path)?;
        Ok(credentials)
    }

    fn validate(&self, path: &Path) -> Result<(), ChatGptAuthError> {
        if self.access_token.trim().is_empty()
            || self.refresh_token.trim().is_empty()
            || self.account_id.trim().is_empty()
        {
            return Err(ChatGptAuthError::InvalidStore {
                path: path.to_path_buf(),
                detail: "required credential field is empty".into(),
            });
        }
        Ok(())
    }

    fn status(&self) -> ChatGptAuthStatus {
        ChatGptAuthStatus {
            account_id: self.account_id.clone(),
            email: self.email.clone(),
            plan: self.plan.clone(),
            fedramp: self.fedramp,
        }
    }
}

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
struct CodexAuthDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<CodexTokenData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    personal_access_token: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
struct CodexTokenData {
    id_token: String,
    access_token: String,
    refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

struct ManagedChatGptAuth {
    auth_file: PathBuf,
    issuer: Arc<str>,
    client: reqwest::Client,
    state: RwLock<ManagedState>,
    refresh: Mutex<()>,
}

struct ManagedState {
    credentials: StoredCredentials,
    auth_record: CodexAuthDocument,
    revision: u64,
    permanent_failure: Option<AuthScopedRefreshFailure>,
}

struct AuthScopedRefreshFailure {
    auth_record: CodexAuthDocument,
    detail: Arc<str>,
}

#[derive(Deserialize)]
struct PersonalAccessTokenMetadata {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "chatgpt_user_id")]
    _chatgpt_user_id: String,
    chatgpt_account_id: String,
    chatgpt_plan_type: String,
    chatgpt_account_is_fedramp: bool,
}

struct PersonalAccessTokenAuth {
    access_token: Arc<str>,
    client: reqwest::Client,
    whoami_endpoint: Arc<str>,
    status: tokio::sync::OnceCell<ChatGptAuthStatus>,
}

impl PersonalAccessTokenAuth {
    async fn status(&self) -> Result<&ChatGptAuthStatus, OpenAiAuthError> {
        self.status
            .get_or_try_init(|| async {
                hydrate_personal_access_token(
                    &self.client,
                    &self.whoami_endpoint,
                    &self.access_token,
                )
                .await
                .map_err(|error| OpenAiAuthError::Unavailable(Arc::from(error.to_string())))
            })
            .await
    }
}

impl OpenAiAuthSource for PersonalAccessTokenAuth {
    fn validate(&self) -> Result<(), OpenAiAuthError> {
        validate_personal_access_token(&self.access_token)
            .map_err(|error| OpenAiAuthError::Unavailable(Arc::from(error.to_string())))
    }

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async move {
            let status = self.status().await?;
            Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ChatGpt,
                Arc::clone(&self.access_token),
                Some(Arc::<str>::from(status.account_id.as_str())),
                status.fedramp,
                0,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        _rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>> {
        Box::pin(async {
            Err(OpenAiAuthError::LoginRequired(Arc::from(
                "persistent access token was rejected and cannot be refreshed",
            )))
        })
    }
}

fn personal_access_token_auth(
    access_token: Arc<str>,
    auth_api_base_url: &str,
) -> Result<OpenAiAuth, ChatGptAuthError> {
    validate_personal_access_token(&access_token)?;
    let source = PersonalAccessTokenAuth {
        access_token,
        client: auth_client()?,
        whoami_endpoint: Arc::from(whoami_endpoint(auth_api_base_url)),
        status: tokio::sync::OnceCell::new(),
    };
    Ok(OpenAiAuth::managed_chatgpt(Arc::new(source)))
}

fn validate_personal_access_token(access_token: &str) -> Result<(), ChatGptAuthError> {
    if access_token.trim().is_empty() {
        return Err(ChatGptAuthError::InvalidToken(
            "persistent access token is empty".into(),
        ));
    }
    if !access_token.starts_with(PERSONAL_ACCESS_TOKEN_PREFIX) {
        return Err(ChatGptAuthError::InvalidToken(
            "persistent access token must start with `at-`".into(),
        ));
    }
    Ok(())
}

fn personal_access_token_from_document(
    document: &CodexAuthDocument,
    path: &Path,
) -> Result<Option<Arc<str>>, ChatGptAuthError> {
    let selected = match document.auth_mode.as_deref() {
        Some("personalAccessToken") => true,
        Some(_) => false,
        None => document.personal_access_token.is_some(),
    };
    if !selected {
        return Ok(None);
    }
    let access_token = document.personal_access_token.as_deref().ok_or_else(|| {
        ChatGptAuthError::InvalidStore {
            path: path.to_path_buf(),
            detail: "Codex auth.json has no personal access token".into(),
        }
    })?;
    validate_personal_access_token(access_token).map_err(|error| {
        ChatGptAuthError::InvalidStore {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    })?;
    Ok(Some(Arc::from(access_token)))
}

async fn hydrate_personal_access_token(
    client: &reqwest::Client,
    endpoint: &str,
    access_token: &str,
) -> Result<ChatGptAuthStatus, ChatGptAuthError> {
    let response = client
        .get(endpoint)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| ChatGptAuthError::AccessTokenMetadata(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(ChatGptAuthError::AccessTokenMetadata(format!(
            "authorization service returned HTTP {status}"
        )));
    }
    let metadata = response
        .json::<PersonalAccessTokenMetadata>()
        .await
        .map_err(|error| ChatGptAuthError::AccessTokenMetadata(error.to_string()))?;
    if metadata.chatgpt_account_id.trim().is_empty() {
        return Err(ChatGptAuthError::AccessTokenMetadata(
            "authorization service returned an empty ChatGPT account ID".into(),
        ));
    }
    Ok(ChatGptAuthStatus {
        account_id: metadata.chatgpt_account_id,
        email: metadata.email,
        plan: Some(metadata.chatgpt_plan_type),
        fedramp: metadata.chatgpt_account_is_fedramp,
    })
}

fn whoami_endpoint(auth_api_base_url: &str) -> String {
    format!(
        "{}{WHOAMI_PATH}",
        auth_api_base_url.trim().trim_end_matches('/')
    )
}

fn auth_api_base_url() -> String {
    std::env::var(AUTH_API_BASE_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| AUTH_API_BASE_URL.to_owned())
}

impl ManagedChatGptAuth {
    fn state(&self) -> Result<std::sync::RwLockReadGuard<'_, ManagedState>, OpenAiAuthError> {
        self.state
            .read()
            .map_err(|_| OpenAiAuthError::Unavailable(Arc::from("authorization state poisoned")))
    }

    fn snapshot_from_state(state: &ManagedState) -> OpenAiAuthSnapshot {
        OpenAiAuthSnapshot::new(
            OpenAiAuthMode::ChatGpt,
            Arc::<str>::from(state.credentials.access_token.as_str()),
            Some(Arc::<str>::from(state.credentials.account_id.as_str())),
            state.credentials.fedramp,
            state.revision,
        )
    }

    fn snapshot_unchecked(&self) -> Result<OpenAiAuthSnapshot, OpenAiAuthError> {
        let state = self.state()?;
        Ok(Self::snapshot_from_state(&state))
    }

    fn snapshot_now(&self) -> Result<OpenAiAuthSnapshot, OpenAiAuthError> {
        let state = self.state()?;
        if let Some(failure) = &state.permanent_failure
            && failure.auth_record == state.auth_record
        {
            return Err(OpenAiAuthError::LoginRequired(Arc::clone(&failure.detail)));
        }
        Ok(Self::snapshot_from_state(&state))
    }

    async fn refresh_if_current(&self, rejected_revision: u64) -> Result<(), OpenAiAuthError> {
        let _refresh = self.refresh.lock().await;
        if self.state()?.revision != rejected_revision {
            return Ok(());
        }
        if self.reload_if_changed()? {
            return Ok(());
        }

        let (refresh_token, attempted_auth_record) = {
            let state = self.state()?;
            if let Some(failure) = &state.permanent_failure
                && failure.auth_record == state.auth_record
            {
                return Err(OpenAiAuthError::LoginRequired(Arc::clone(&failure.detail)));
            }
            (
                state.credentials.refresh_token.clone(),
                state.auth_record.clone(),
            )
        };
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer.trim_end_matches('/')))
            .json(&RefreshRequest {
                client_id: OAUTH_CLIENT_ID,
                grant_type: "refresh_token",
                refresh_token,
            })
            .send()
            .await
            .map_err(|error| OpenAiAuthError::Refresh(Arc::from(error.to_string())))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| OpenAiAuthError::Refresh(Arc::from(error.to_string())))?;
        if !status.is_success() {
            let code = refresh_error_code(&body);
            let permanent = status == reqwest::StatusCode::UNAUTHORIZED
                || matches!(
                    code.as_deref(),
                    Some(
                        "refresh_token_expired"
                            | "refresh_token_reused"
                            | "refresh_token_invalidated"
                    )
                );
            let detail: Arc<str> =
                Arc::from(code.unwrap_or_else(|| format!("token endpoint returned HTTP {status}")));
            if permanent {
                // A different process can rotate the shared refresh token while this request is
                // in flight. Adopt that same-account record before treating this credential as
                // permanently rejected.
                if self.reload_if_changed()? {
                    return Ok(());
                }
                if self.record_permanent_failure_if_unchanged(
                    &attempted_auth_record,
                    Arc::clone(&detail),
                )? {
                    return Err(OpenAiAuthError::LoginRequired(detail));
                }
                return Ok(());
            }
            return Err(OpenAiAuthError::Refresh(detail));
        }
        let refreshed: RefreshResponse = serde_json::from_slice(&body)
            .map_err(|error| OpenAiAuthError::Refresh(Arc::from(error.to_string())))?;
        self.apply_refresh(refreshed)
    }

    fn reload_if_changed(&self) -> Result<bool, OpenAiAuthError> {
        let (stored, auth_record) =
            read_stored_auth(&self.auth_file).map_err(|error| auth_store_error(&error))?;
        stored
            .validate(&self.auth_file)
            .map_err(|error| auth_store_error(&error))?;
        let mut state = self
            .state
            .write()
            .map_err(|_| OpenAiAuthError::Unavailable(Arc::from("authorization state poisoned")))?;
        if stored.account_id != state.credentials.account_id {
            return Err(OpenAiAuthError::AccountChanged);
        }
        if auth_record == state.auth_record {
            return Ok(false);
        }
        state.credentials = stored;
        state.auth_record = auth_record;
        state.revision = state.revision.wrapping_add(1);
        state.permanent_failure = None;
        Ok(true)
    }

    fn record_permanent_failure_if_unchanged(
        &self,
        attempted_auth_record: &CodexAuthDocument,
        detail: Arc<str>,
    ) -> Result<bool, OpenAiAuthError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| OpenAiAuthError::Unavailable(Arc::from("authorization state poisoned")))?;
        if state.auth_record != *attempted_auth_record {
            return Ok(false);
        }
        state.permanent_failure = Some(AuthScopedRefreshFailure {
            auth_record: attempted_auth_record.clone(),
            detail,
        });
        Ok(true)
    }

    fn apply_refresh(&self, refreshed: RefreshResponse) -> Result<(), OpenAiAuthError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| OpenAiAuthError::Unavailable(Arc::from("authorization state poisoned")))?;
        let mut next = state.credentials.clone();
        if let Some(access_token) = refreshed.access {
            next.access_token = access_token;
        }
        if let Some(refresh_token) = refreshed.refresh {
            next.refresh_token = refresh_token;
        }
        if let Some(id_token) = refreshed.id {
            let claims: IdClaims = decode_jwt(&id_token)
                .map_err(|error| OpenAiAuthError::Refresh(Arc::from(error.to_string())))?;
            let auth = claims.auth.unwrap_or_default();
            if let Some(account_id) = auth.account_id
                && account_id != next.account_id
            {
                return Err(OpenAiAuthError::AccountChanged);
            }
            next.id_token = id_token;
            next.email = claims
                .email
                .or_else(|| claims.profile.and_then(|profile| profile.email));
            next.plan = auth.plan;
            next.fedramp = auth.fedramp;
        }
        let auth_record =
            write_store(&self.auth_file, &next).map_err(|error| auth_store_error(&error))?;
        state.credentials = next;
        state.auth_record = auth_record;
        state.revision = state.revision.wrapping_add(1);
        state.permanent_failure = None;
        Ok(())
    }
}

impl OpenAiAuthSource for ManagedChatGptAuth {
    fn validate(&self) -> Result<(), OpenAiAuthError> {
        self.snapshot_now().map(|_| ())
    }

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async move {
            let snapshot = self.snapshot_unchecked()?;
            let should_refresh = {
                let state = self.state()?;
                let expires_soon = jwt_expiration(&state.credentials.access_token)
                    .is_some_and(|expiry| expiry <= unix_now() + REFRESH_EARLY_SECONDS);
                let permanently_rejected = state
                    .permanent_failure
                    .as_ref()
                    .is_some_and(|failure| failure.auth_record == state.auth_record);
                expires_soon || permanently_rejected
            };
            if should_refresh && let Err(error) = self.refresh_if_current(snapshot.revision()).await
            {
                tracing::warn!(error = %error, "proactive ChatGPT token refresh failed");
            }
            self.snapshot_now()
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>> {
        let mode = rejected.mode();
        let revision = rejected.revision();
        Box::pin(async move {
            if mode != OpenAiAuthMode::ChatGpt {
                return Err(OpenAiAuthError::LoginRequired(Arc::from(
                    "authorization mode changed",
                )));
            }
            self.refresh_if_current(revision).await
        })
    }
}

#[derive(Default, Deserialize)]
struct IdClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/profile", default)]
    profile: Option<ProfileClaims>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct ProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Default, Deserialize)]
struct AuthClaims {
    #[serde(rename = "chatgpt_plan_type", default)]
    plan: Option<String>,
    #[serde(rename = "chatgpt_account_id", default)]
    account_id: Option<String>,
    #[serde(rename = "chatgpt_account_is_fedramp", default)]
    fedramp: bool,
}

#[derive(Deserialize)]
struct ExpClaims {
    #[serde(default)]
    exp: Option<i64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(rename = "id_token")]
    id: String,
    #[serde(rename = "access_token")]
    access: String,
    #[serde(rename = "refresh_token")]
    refresh: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(rename = "id_token")]
    id: Option<String>,
    #[serde(rename = "access_token")]
    access: Option<String>,
    #[serde(rename = "refresh_token")]
    refresh: Option<String>,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: String,
}

fn decode_jwt<T: DeserializeOwned>(jwt: &str) -> Result<T, ChatGptAuthError> {
    let payload = jwt
        .split('.')
        .nth(1)
        .filter(|payload| !payload.is_empty())
        .ok_or_else(|| ChatGptAuthError::InvalidToken("invalid JWT format".into()))?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|error| ChatGptAuthError::InvalidToken(error.to_string()))?;
    serde_json::from_slice(&decoded)
        .map_err(|error| ChatGptAuthError::InvalidToken(error.to_string()))
}

fn jwt_expiration(jwt: &str) -> Option<i64> {
    decode_jwt::<ExpClaims>(jwt).ok()?.exp
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn rfc3339_utc(timestamp: i64) -> String {
    const SECONDS_PER_DAY: i64 = 86_400;

    let days = timestamp.div_euclid(SECONDS_PER_DAY);
    let seconds = timestamp.rem_euclid(SECONDS_PER_DAY);
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;

    // Howard Hinnant's civil-from-days transform, with day zero at 1970-01-01.
    let shifted_days = days + 719_468;
    let era = if shifted_days >= 0 {
        shifted_days
    } else {
        shifted_days - 146_096
    } / 146_097;
    let day_of_era = shifted_days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    year += i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn read_stored_auth(
    path: &Path,
) -> Result<(StoredCredentials, CodexAuthDocument), ChatGptAuthError> {
    let document = read_document(path)?;
    let credentials = StoredCredentials::from_document(&document, path)?;
    Ok((credentials, document))
}

fn read_store(path: &Path) -> Result<StoredCredentials, ChatGptAuthError> {
    read_stored_auth(path).map(|(credentials, _)| credentials)
}

fn read_document(path: &Path) -> Result<CodexAuthDocument, ChatGptAuthError> {
    let bytes = std::fs::read(path).map_err(|source| ChatGptAuthError::Storage {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|error| ChatGptAuthError::InvalidStore {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

fn write_store(
    path: &Path,
    credentials: &StoredCredentials,
) -> Result<CodexAuthDocument, ChatGptAuthError> {
    let parent = path.parent().ok_or_else(|| ChatGptAuthError::Storage {
        path: path.to_path_buf(),
        source: io::Error::new(ErrorKind::InvalidInput, "auth file has no parent directory"),
    })?;
    std::fs::create_dir_all(parent).map_err(|source| ChatGptAuthError::Storage {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut document = match read_document(path) {
        Ok(document) => document,
        Err(ChatGptAuthError::Storage { source, .. }) if source.kind() == ErrorKind::NotFound => {
            CodexAuthDocument::default()
        }
        Err(error) => return Err(error),
    };
    let token_extra = document
        .tokens
        .take()
        .map_or_else(BTreeMap::new, |tokens| tokens.extra);
    document.auth_mode = Some("chatgpt".into());
    document.tokens = Some(CodexTokenData {
        id_token: credentials.id_token.clone(),
        access_token: credentials.access_token.clone(),
        refresh_token: credentials.refresh_token.clone(),
        account_id: Some(credentials.account_id.clone()),
        extra: token_extra,
    });
    document.last_refresh = Some(rfc3339_utc(unix_now()));

    let temporary = path.with_extension(format!("json.{}.tmp", random_urlsafe()?));
    let bytes =
        serde_json::to_vec_pretty(&document).map_err(|error| ChatGptAuthError::InvalidStore {
            path: path.to_path_buf(),
            detail: error.to_string(),
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
        .map_err(|source| ChatGptAuthError::Storage {
            path: temporary.clone(),
            source,
        })?;
    if let Err(source) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temporary);
        return Err(ChatGptAuthError::Storage {
            path: temporary,
            source,
        });
    }
    drop(file);
    if let Err(source) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(ChatGptAuthError::Storage {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(document)
}

fn auth_store_error(error: &ChatGptAuthError) -> OpenAiAuthError {
    OpenAiAuthError::Unavailable(Arc::from(error.to_string()))
}

fn auth_client() -> Result<reqwest::Client, ChatGptAuthError> {
    crate::transport::install_default_rustls_crypto_provider();
    reqwest::Client::builder()
        .timeout(AUTH_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| ChatGptAuthError::TokenExchange(error.to_string()))
}

fn refresh_error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .get("error")
        .and_then(|error| match error {
            serde_json::Value::String(code) => Some(code.as_str()),
            serde_json::Value::Object(error) => error.get("code")?.as_str(),
            _ => None,
        })
        .or_else(|| value.get("code")?.as_str())
        .map(str::to_owned)
}

async fn bind_callback() -> Result<TcpListener, ChatGptAuthError> {
    for port in CALLBACK_PORTS {
        if let Ok(listener) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            return Ok(listener);
        }
    }
    Err(ChatGptAuthError::CallbackUnavailable)
}

async fn receive_callback(listener: &TcpListener) -> Result<OAuthCallback, ChatGptAuthError> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
    let mut bytes = Vec::with_capacity(2048);
    loop {
        let read = stream
            .read_buf(&mut bytes)
            .await
            .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
        if read == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > 16 * 1024 {
            return Err(ChatGptAuthError::LoginRejected(
                "OAuth callback request was too large".into(),
            ));
        }
    }
    let request = std::str::from_utf8(&bytes)
        .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .filter(|target| target.starts_with(CALLBACK_PATH))
        .ok_or_else(|| ChatGptAuthError::LoginRejected("invalid callback request".into()))?;
    Ok(OAuthCallback {
        target: target.to_owned(),
        stream,
    })
}

fn authorize_url(
    issuer: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<String, ChatGptAuthError> {
    let mut url = Url::parse(&format!("{}/oauth/authorize", issuer.trim_end_matches('/')))
        .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", OAUTH_SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", "nanocodex");
    Ok(url.into())
}

async fn exchange_code(
    client: &reqwest::Client,
    issuer: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<StoredCredentials, ChatGptAuthError> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("client_id", OAUTH_CLIENT_ID)
        .append_pair("code_verifier", code_verifier)
        .finish();
    let response = client
        .post(format!("{}/oauth/token", issuer.trim_end_matches('/')))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|error| ChatGptAuthError::TokenExchange(error.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ChatGptAuthError::TokenExchange(error.to_string()))?;
    if !status.is_success() {
        let code = refresh_error_code(&bytes)
            .unwrap_or_else(|| format!("token endpoint returned HTTP {status}"));
        return Err(ChatGptAuthError::TokenExchange(code));
    }
    let tokens: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|error| ChatGptAuthError::TokenExchange(error.to_string()))?;
    StoredCredentials::from_tokens(tokens)
}

fn random_urlsafe() -> Result<String, ChatGptAuthError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| ChatGptAuthError::LoginRejected(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests;
