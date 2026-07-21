use std::{
    collections::BTreeMap,
    fmt,
    fs::OpenOptions,
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use nanocodex_core::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};
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
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const CALLBACK_PATH: &str = "/auth/callback";
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const REFRESH_EARLY_SECONDS: i64 = 5 * 60;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Non-secret information about a stored `ChatGPT` authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatGptAuthStatus {
    pub account_id: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub fedramp: bool,
}

/// Failure while logging in, loading, persisting, or removing `ChatGPT` credentials.
#[derive(Debug, thiserror::Error)]
pub enum ChatGptAuthError {
    #[error("failed to access ChatGPT authorization file {path}: {source}")]
    Storage {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("ChatGPT authorization file {path} is invalid: {detail}")]
    InvalidStore { path: PathBuf, detail: String },
    #[error("ChatGPT OAuth response was invalid: {0}")]
    InvalidToken(String),
    #[error("could not listen for the OAuth callback on localhost ports 1455 or 1457")]
    CallbackUnavailable,
    #[error("timed out waiting for the ChatGPT OAuth callback")]
    CallbackTimeout,
    #[error("the OAuth callback did not match this login attempt")]
    StateMismatch,
    #[error("ChatGPT login was rejected: {0}")]
    LoginRejected(String),
    #[error("ChatGPT token exchange failed: {0}")]
    TokenExchange(String),
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

/// Load a persisted `ChatGPT` OAuth session as the shared authorization for an agent family.
///
/// # Errors
///
/// Returns an error when the credential file cannot be read or is invalid.
pub fn load_chatgpt_auth(auth_file: impl Into<PathBuf>) -> Result<OpenAiAuth, ChatGptAuthError> {
    let auth_file = auth_file.into();
    let credentials = read_store(&auth_file)?;
    credentials.validate(&auth_file)?;
    let manager = ManagedChatGptAuth {
        auth_file,
        issuer: Arc::from(AUTH_ISSUER),
        client: auth_client()?,
        state: RwLock::new(ManagedState {
            credentials,
            revision: 0,
            permanent_failure: None,
        }),
        refresh: Mutex::new(()),
    };
    Ok(OpenAiAuth::managed_chatgpt(Arc::new(manager)))
}

/// Inspect a stored `ChatGPT` authorization without exposing its tokens.
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

#[derive(Default, Deserialize, Serialize)]
struct CodexAuthDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<CodexTokenData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_refresh: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
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
    revision: u64,
    permanent_failure: Option<Arc<str>>,
}

impl ManagedChatGptAuth {
    fn state(&self) -> Result<std::sync::RwLockReadGuard<'_, ManagedState>, OpenAiAuthError> {
        self.state
            .read()
            .map_err(|_| OpenAiAuthError::Unavailable(Arc::from("authorization state poisoned")))
    }

    fn snapshot_now(&self) -> Result<OpenAiAuthSnapshot, OpenAiAuthError> {
        let state = self.state()?;
        if let Some(error) = &state.permanent_failure {
            return Err(OpenAiAuthError::LoginRequired(Arc::clone(error)));
        }
        Ok(OpenAiAuthSnapshot::new(
            OpenAiAuthMode::ChatGpt,
            Arc::<str>::from(state.credentials.access_token.as_str()),
            Some(Arc::<str>::from(state.credentials.account_id.as_str())),
            state.credentials.fedramp,
            state.revision,
        ))
    }

    async fn refresh_if_current(
        &self,
        rejected_revision: u64,
        reload: bool,
    ) -> Result<(), OpenAiAuthError> {
        let _refresh = self.refresh.lock().await;
        if self.state()?.revision != rejected_revision {
            return Ok(());
        }
        if reload && self.reload_if_changed()? {
            return Ok(());
        }

        let refresh_token = self.state()?.credentials.refresh_token.clone();
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
                self.state
                    .write()
                    .map_err(|_| {
                        OpenAiAuthError::Unavailable(Arc::from("authorization state poisoned"))
                    })?
                    .permanent_failure = Some(Arc::clone(&detail));
                return Err(OpenAiAuthError::LoginRequired(detail));
            }
            return Err(OpenAiAuthError::Refresh(detail));
        }
        let refreshed: RefreshResponse = serde_json::from_slice(&body)
            .map_err(|error| OpenAiAuthError::Refresh(Arc::from(error.to_string())))?;
        self.apply_refresh(refreshed)
    }

    fn reload_if_changed(&self) -> Result<bool, OpenAiAuthError> {
        let stored = read_store(&self.auth_file).map_err(|error| auth_store_error(&error))?;
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
        if stored.access_token == state.credentials.access_token {
            return Ok(false);
        }
        state.credentials = stored;
        state.revision = state.revision.wrapping_add(1);
        state.permanent_failure = None;
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
        write_store(&self.auth_file, &next).map_err(|error| auth_store_error(&error))?;
        state.credentials = next;
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
            let snapshot = self.snapshot_now()?;
            let expires_soon = jwt_expiration(&self.state()?.credentials.access_token)
                .is_some_and(|expiry| expiry <= unix_now() + REFRESH_EARLY_SECONDS);
            if expires_soon
                && let Err(error) = self.refresh_if_current(snapshot.revision(), false).await
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
            self.refresh_if_current(revision, true).await
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

fn read_store(path: &Path) -> Result<StoredCredentials, ChatGptAuthError> {
    let document = read_document(path)?;
    StoredCredentials::from_document(&document, path)
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

fn write_store(path: &Path, credentials: &StoredCredentials) -> Result<(), ChatGptAuthError> {
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
    document.last_refresh = Some(chrono::Utc::now().to_rfc3339());

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
            path: temporary.clone(),
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
    Ok(())
}

fn auth_store_error(error: &ChatGptAuthError) -> OpenAiAuthError {
    OpenAiAuthError::Unavailable(Arc::from(error.to_string()))
}

fn auth_client() -> Result<reqwest::Client, ChatGptAuthError> {
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
mod tests {
    use std::sync::{Arc, RwLock};

    use super::{
        ManagedChatGptAuth, ManagedState, StoredCredentials, authorize_url, jwt_expiration,
        read_store, refresh_error_code, unix_now, write_store,
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use nanocodex_core::{OpenAiAuth, OpenAiAuthError};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::Mutex,
    };

    fn jwt(payload: &serde_json::Value) -> String {
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap())
        )
    }

    fn credentials(access: &str, refresh: &str) -> StoredCredentials {
        StoredCredentials {
            id_token: jwt(&serde_json::json!({
                "email": "user@example.com",
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": "account-1",
                    "chatgpt_plan_type": "plus"
                }
            })),
            access_token: access.to_owned(),
            refresh_token: refresh.to_owned(),
            account_id: "account-1".into(),
            email: Some("user@example.com".into()),
            plan: Some("plus".into()),
            fedramp: false,
        }
    }

    fn temp_auth_file() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nanocodex-auth-test-{}.json",
            super::random_urlsafe().unwrap()
        ))
    }

    fn managed(
        auth_file: &std::path::Path,
        issuer: impl Into<Arc<str>>,
        credentials: StoredCredentials,
    ) -> OpenAiAuth {
        OpenAiAuth::managed_chatgpt(Arc::new(ManagedChatGptAuth {
            auth_file: auth_file.to_path_buf(),
            issuer: issuer.into(),
            client: super::auth_client().unwrap(),
            state: RwLock::new(ManagedState {
                credentials,
                revision: 0,
                permanent_failure: None,
            }),
            refresh: Mutex::new(()),
        }))
    }

    #[test]
    fn parses_account_and_expiry_without_exposing_tokens() {
        let id_token = jwt(&serde_json::json!({
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-1",
                "chatgpt_plan_type": "plus",
                "chatgpt_account_is_fedramp": true
            }
        }));
        let credentials = StoredCredentials::from_tokens(super::TokenResponse {
            id: id_token,
            access: jwt(&serde_json::json!({ "exp": 12345 })),
            refresh: "refresh-secret".into(),
        })
        .unwrap();
        assert_eq!(credentials.account_id, "account-1");
        assert_eq!(credentials.status().plan.as_deref(), Some("plus"));
        assert_eq!(jwt_expiration(&credentials.access_token), Some(12345));
    }

    #[test]
    fn recognizes_nested_refresh_error_codes() {
        assert_eq!(
            refresh_error_code(br#"{"error":{"code":"refresh_token_reused"}}"#).as_deref(),
            Some("refresh_token_reused")
        );
    }

    #[test]
    fn codex_auth_document_round_trip_preserves_unrelated_fields() {
        let auth_file = temp_auth_file();
        let original = credentials("access-1", "refresh-1");
        let document = serde_json::json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": "preserved-api-key",
            "tokens": {
                "id_token": original.id_token,
                "access_token": original.access_token,
                "refresh_token": original.refresh_token,
                "account_id": original.account_id,
                "future_token_field": {"preserved": true}
            },
            "last_refresh": "2026-01-01T00:00:00Z",
            "agent_identity": {"future": "preserved"}
        });
        std::fs::write(&auth_file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();

        let loaded = read_store(&auth_file).unwrap();
        assert_eq!(loaded.email.as_deref(), Some("user@example.com"));
        let rotated = credentials("access-2", "refresh-2");
        write_store(&auth_file, &rotated).unwrap();

        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&auth_file).unwrap()).unwrap();
        assert_eq!(stored["auth_mode"], "chatgpt");
        assert_eq!(stored["OPENAI_API_KEY"], "preserved-api-key");
        assert_eq!(stored["agent_identity"]["future"], "preserved");
        assert_eq!(stored["tokens"]["access_token"], "access-2");
        assert_eq!(stored["tokens"]["refresh_token"], "refresh-2");
        assert_eq!(stored["tokens"]["future_token_field"]["preserved"], true);
        std::fs::remove_file(auth_file).unwrap();
    }

    #[test]
    fn authorization_url_contains_the_pkce_and_offline_access_contract() {
        let url = authorize_url(
            "https://auth.openai.com",
            "http://localhost:1455/auth/callback",
            "state-value",
            "challenge-value",
        )
        .unwrap();
        let url = url::Url::parse(&url).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(url.path(), "/oauth/authorize");
        assert_eq!(query.get("response_type").unwrap(), "code");
        assert_eq!(query.get("client_id").unwrap(), super::OAUTH_CLIENT_ID);
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(query.get("code_challenge").unwrap(), "challenge-value");
        assert_eq!(query.get("state").unwrap(), "state-value");
        assert!(query.get("scope").unwrap().contains("offline_access"));
    }

    #[tokio::test]
    async fn unauthorized_recovery_reloads_a_rotated_credential_from_disk() {
        let auth_file = temp_auth_file();
        let original = credentials("access-1", "refresh-1");
        write_store(&auth_file, &original).unwrap();
        let auth = managed(&auth_file, "http://127.0.0.1:1", original);
        let rejected = auth.snapshot().await.unwrap();

        let rotated = credentials("access-2", "refresh-2");
        write_store(&auth_file, &rotated).unwrap();
        auth.recover_unauthorized(&rejected).await.unwrap();

        let recovered = auth.snapshot().await.unwrap();
        assert_eq!(recovered.bearer(), "access-2");
        assert_eq!(recovered.revision(), 1);
        std::fs::remove_file(auth_file).unwrap();
    }

    #[tokio::test]
    async fn unauthorized_recovery_refuses_a_different_stored_account() {
        let auth_file = temp_auth_file();
        let original = credentials("access-1", "refresh-1");
        write_store(&auth_file, &original).unwrap();
        let auth = managed(&auth_file, "http://127.0.0.1:1", original);
        let rejected = auth.snapshot().await.unwrap();

        let mut changed = credentials("access-2", "refresh-2");
        changed.account_id = "account-2".into();
        write_store(&auth_file, &changed).unwrap();
        assert!(matches!(
            auth.recover_unauthorized(&rejected).await,
            Err(OpenAiAuthError::AccountChanged)
        ));
        std::fs::remove_file(auth_file).unwrap();
    }

    #[tokio::test]
    async fn expired_access_token_is_refreshed_and_rotated_atomically() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
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
            assert!(request.starts_with("POST /oauth/token HTTP/1.1"));
            assert!(request.contains(r#""refresh_token":"refresh-1""#));
            let response = serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "refresh-2"
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

        let auth_file = temp_auth_file();
        let expired = jwt(&serde_json::json!({ "exp": unix_now() - 1 }));
        let original = credentials(&expired, "refresh-1");
        write_store(&auth_file, &original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&auth_file).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        let auth = managed(&auth_file, issuer, original);

        let snapshot = auth.snapshot().await.unwrap();
        assert_eq!(snapshot.bearer(), "fresh-access");
        let stored = read_store(&auth_file).unwrap();
        assert_eq!(stored.access_token, "fresh-access");
        assert_eq!(stored.refresh_token, "refresh-2");
        server.await.unwrap();
        std::fs::remove_file(auth_file).unwrap();
    }
}
