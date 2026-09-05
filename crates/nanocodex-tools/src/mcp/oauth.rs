use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, OnceLock, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use http::{HeaderName, HeaderValue};
use oauth2::{AccessToken, RefreshToken, Scope, TokenResponse, basic::BasicTokenType};
use rmcp::transport::{
    AuthorizationManager, AuthorizationRequest, AuthorizationSession,
    auth::{
        AuthClient, AuthorizationMetadata, CredentialStore, InMemoryCredentialStore,
        OAuthTokenResponse, StoredCredentials, VendorExtraTokenFields,
    },
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, RwLock},
    task::JoinHandle,
};
use tracing::{Instrument, info_span};

use super::config::SecretSource;

mod refresh;

const LOGIN_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(crate) struct OAuthMetadataCache {
    entries: RwLock<HashMap<(String, String), AuthorizationMetadata>>,
}

impl OAuthMetadataCache {
    async fn get(&self, server_name: &str, server_url: &str) -> Option<AuthorizationMetadata> {
        self.entries
            .read()
            .await
            .get(&(server_name.to_owned(), server_url.to_owned()))
            .cloned()
    }

    async fn insert(&self, server_name: &str, server_url: &str, metadata: AuthorizationMetadata) {
        self.entries
            .write()
            .await
            .insert((server_name.to_owned(), server_url.to_owned()), metadata);
    }
}

/// OAuth credentials for one Streamable HTTP MCP server.
///
/// This value intentionally does not implement `Debug`: access and refresh tokens must not be
/// emitted by diagnostics. Embedders normally provide these through an [`McpOAuthStore`].
#[derive(Clone, PartialEq, Eq)]
pub struct McpOAuthCredentials {
    client_id: String,
    access_token: String,
    refresh_token: Option<String>,
    issuer: Option<String>,
    expires_at_millis: Option<u64>,
    scopes: Vec<String>,
}

/// An acquired refresh-transaction lock held until its boxed value is dropped.
pub trait McpOAuthRefreshGuard: Send {}

impl<T: Send> McpOAuthRefreshGuard for T {}

impl McpOAuthCredentials {
    /// Creates credentials from a dynamically registered client and access token.
    #[must_use]
    pub fn new(client_id: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            access_token: access_token.into(),
            refresh_token: None,
            issuer: None,
            expires_at_millis: None,
            scopes: Vec::new(),
        }
    }

    /// Attaches the optional refresh token.
    #[must_use]
    pub fn refresh_token(mut self, refresh_token: impl Into<String>) -> Self {
        self.refresh_token = Some(refresh_token.into());
        self
    }

    /// Binds these credentials to the authorization server that issued them.
    #[must_use]
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Sets the access-token expiry as Unix epoch milliseconds.
    #[must_use]
    pub const fn expires_at_millis(mut self, expires_at_millis: u64) -> Self {
        self.expires_at_millis = Some(expires_at_millis);
        self
    }

    /// Records the scopes granted by the authorization server.
    #[must_use]
    pub fn scopes(mut self, scopes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Returns the dynamically registered OAuth client ID.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Returns the bearer access token.
    #[must_use]
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Returns the refresh token when one was issued.
    #[must_use]
    pub fn refresh_token_value(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    /// Returns the authorization server issuer bound to these credentials.
    #[must_use]
    pub fn authorization_issuer(&self) -> Option<&str> {
        self.issuer.as_deref()
    }

    /// Returns access-token expiry as Unix epoch milliseconds.
    #[must_use]
    pub const fn expires_at(&self) -> Option<u64> {
        self.expires_at_millis
    }

    /// Returns the scopes granted by the authorization server.
    #[must_use]
    pub fn granted_scopes(&self) -> &[String] {
        &self.scopes
    }

    fn to_token_response(&self) -> OAuthTokenResponse {
        let mut response = OAuthTokenResponse::new(
            AccessToken::new(self.access_token.clone()),
            BasicTokenType::Bearer,
            VendorExtraTokenFields::default(),
        );
        if let Some(refresh_token) = &self.refresh_token {
            response.set_refresh_token(Some(RefreshToken::new(refresh_token.clone())));
        }
        if !self.scopes.is_empty() {
            response.set_scopes(Some(self.scopes.iter().cloned().map(Scope::new).collect()));
        }
        if let Some(expires_at) = self.expires_at_millis {
            response.set_expires_in(Some(&Duration::from_millis(
                expires_at.saturating_sub(now_millis()),
            )));
        }
        response
    }

    fn from_token_response(
        client_id: String,
        response: &OAuthTokenResponse,
        issuer: Option<String>,
    ) -> Self {
        let expires_at_millis = response.expires_in().and_then(|expires_in| {
            now_millis().checked_add(u64::try_from(expires_in.as_millis()).ok()?)
        });
        Self {
            client_id,
            access_token: response.access_token().secret().to_owned(),
            refresh_token: response
                .refresh_token()
                .map(|token| token.secret().to_owned()),
            issuer,
            expires_at_millis,
            scopes: response
                .scopes()
                .map(|scopes| {
                    scopes
                        .iter()
                        .map(|scope| scope.as_ref().to_owned())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn same_token(&self, other: &Self) -> bool {
        self.client_id == other.client_id
            && self.access_token == other.access_token
            && self.refresh_token == other.refresh_token
            && self.issuer == other.issuer
            && self.scopes == other.scopes
    }
}

/// Persistence selected by an embedding application for MCP OAuth credentials.
#[async_trait]
pub trait McpOAuthStore: Send + Sync {
    /// Loads credentials for one configured server and exact URL.
    async fn load(
        &self,
        server_name: &str,
        server_url: &str,
    ) -> Result<Option<McpOAuthCredentials>, String>;

    /// Atomically persists the latest credentials after login or refresh.
    async fn save(
        &self,
        server_name: &str,
        server_url: &str,
        credentials: &McpOAuthCredentials,
    ) -> Result<(), String>;

    /// Serializes one credential's authoritative load, provider refresh, and save transaction.
    ///
    /// The default coordinates runtimes in this process. Stores shared by multiple processes must
    /// override this with a matching cross-process or provider-backed lock and bound lock waits.
    async fn acquire_refresh_lock(
        &self,
        server_name: &str,
        server_url: &str,
    ) -> Result<Box<dyn McpOAuthRefreshGuard>, String> {
        let key = format!("{server_name}\0{server_url}");
        let lock = {
            static LOCKS: OnceLock<
                std::sync::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
            > = OnceLock::new();
            let locks = LOCKS.get_or_init(Default::default);
            let mut locks = locks
                .lock()
                .map_err(|_| "MCP OAuth refresh lock registry was poisoned".to_owned())?;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        };
        Ok(Box::new(lock.lock_owned().await))
    }
}

pub(crate) struct OAuthRuntime {
    server_name: String,
    server_url: String,
    manager: Arc<Mutex<AuthorizationManager>>,
    store: Arc<dyn McpOAuthStore>,
    authorization_issuer: Option<String>,
    last_credentials: Mutex<Option<McpOAuthCredentials>>,
}

impl OAuthRuntime {
    pub(crate) fn new(
        server_name: String,
        server_url: String,
        manager: Arc<Mutex<AuthorizationManager>>,
        store: Arc<dyn McpOAuthStore>,
        authorization_issuer: Option<String>,
        credentials: McpOAuthCredentials,
    ) -> Self {
        Self {
            server_name,
            server_url,
            manager,
            store,
            authorization_issuer,
            last_credentials: Mutex::new(Some(credentials)),
        }
    }

    pub(crate) async fn persist_if_changed(&self, parent: &tracing::Span) -> Result<(), String> {
        let (client_id, response) = self
            .manager
            .lock()
            .await
            .get_credentials()
            .await
            .map_err(|error| format!("failed to read refreshed OAuth credentials: {error}"))?;
        let Some(response) = response else {
            return Err("OAuth transport no longer has credentials".to_owned());
        };
        let mut credentials = McpOAuthCredentials::from_token_response(
            client_id,
            &response,
            self.authorization_issuer.clone(),
        );
        let mut previous = self.last_credentials.lock().await;
        if let Some(previous) = previous.as_ref() {
            if response.refresh_token().is_none() {
                if validate_refresh_token_issuer(previous, self.authorization_issuer.as_deref())
                    .is_ok()
                {
                    credentials
                        .refresh_token
                        .clone_from(&previous.refresh_token);
                } else if previous.refresh_token.is_some() {
                    // Do not relabel an unbound refresh token with the current issuer merely
                    // because RMCP returned the still-usable access token staged without it.
                    credentials.issuer = None;
                }
            }
            if response.scopes().is_none() {
                credentials.scopes.clone_from(&previous.scopes);
            }
            if credentials.same_token(previous) {
                credentials.expires_at_millis = previous.expires_at_millis;
            }
        }
        if previous.as_ref() == Some(&credentials) {
            return Ok(());
        }
        let span = info_span!(
            target: "nanocodex_tools",
            parent: parent,
            "mcp.oauth.credentials_save",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            reason = "refresh",
            status = tracing::field::Empty,
        );
        let result = self
            .store
            .save(&self.server_name, &self.server_url, &credentials)
            .instrument(span.clone())
            .await;
        span.record(
            "status",
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            },
        );
        span.record(
            "otel.status_code",
            if result.is_ok() { "OK" } else { "ERROR" },
        );
        result?;
        *previous = Some(credentials);
        Ok(())
    }
}

pub(crate) struct OAuthTransport {
    pub(crate) client: AuthClient<reqwest::Client>,
    pub(crate) runtime: Arc<OAuthRuntime>,
    pub(crate) metadata_cache_hit: bool,
}

fn credentials_for_manager(
    credentials: &McpOAuthCredentials,
    authorization_issuer: Option<&str>,
) -> McpOAuthCredentials {
    let mut staged = credentials.clone();
    if validate_refresh_token_issuer(credentials, authorization_issuer).is_err() {
        // The access token is still useful at the MCP resource. Never expose an unbound refresh
        // token to RMCP, which could otherwise send it automatically as the access token expires.
        staged.refresh_token = None;
        staged.issuer = None;
    }
    staged
}

fn validate_refresh_token_issuer(
    credentials: &McpOAuthCredentials,
    authorization_issuer: Option<&str>,
) -> Result<(), String> {
    if credentials.refresh_token.is_none() {
        return Ok(());
    }
    let Some(stored_issuer) = credentials.issuer.as_deref() else {
        return Err("OAuth refresh credentials are missing an authorization server issuer; authorization required".to_owned());
    };
    let Some(authorization_issuer) = authorization_issuer else {
        return Err(
            "OAuth metadata did not include an authorization server issuer; authorization required"
                .to_owned(),
        );
    };
    if stored_issuer != authorization_issuer {
        return Err("OAuth authorization server issuer changed; authorization required".to_owned());
    }
    Ok(())
}

fn authorization_issuer(metadata: &AuthorizationMetadata) -> Result<Option<String>, String> {
    metadata
        .issuer
        .as_deref()
        .filter(|issuer| !issuer.trim().is_empty())
        .map(|issuer| {
            url::Url::parse(issuer).map_err(|error| {
                format!("OAuth authorization server issuer is invalid: {error}")
            })?;
            Ok(issuer.to_owned())
        })
        .transpose()
}

fn validate_authorization_server_endpoints(metadata: &AuthorizationMetadata) -> Result<(), String> {
    let authorization_endpoint = url::Url::parse(&metadata.authorization_endpoint)
        .map_err(|error| format!("OAuth authorization endpoint is invalid: {error}"))?;
    let token_endpoint = url::Url::parse(&metadata.token_endpoint)
        .map_err(|error| format!("OAuth token endpoint is invalid: {error}"))?;
    let issuer = metadata
        .issuer
        .as_deref()
        .filter(|issuer| !issuer.trim().is_empty())
        .map(url::Url::parse)
        .transpose()
        .map_err(|error| format!("OAuth authorization server issuer is invalid: {error}"))?;
    let issuer_bound_callbacks = metadata
        .additional_fields
        .get("authorization_response_iss_parameter_supported")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if issuer_bound_callbacks {
        if issuer.is_none() {
            return Err(
                "OAuth issuer-bound callbacks require an authorization server issuer".to_owned(),
            );
        }
        return Ok(());
    }

    if let Some(issuer) = issuer {
        let compatible_provider = matches!(
            (
                issuer.as_str(),
                authorization_endpoint
                    .origin()
                    .ascii_serialization()
                    .as_str(),
                token_endpoint.origin().ascii_serialization().as_str(),
            ),
            (
                "https://api.figma.com/",
                "https://www.figma.com",
                "https://api.figma.com",
            ) | (
                "https://agent.robinhood.com/mcp/trading",
                "https://robinhood.com",
                "https://api.robinhood.com",
            )
        );
        if authorization_endpoint.origin() == issuer.origin()
            || authorization_endpoint.origin() == token_endpoint.origin()
            || compatible_provider
        {
            return Ok(());
        }
        return Err(
            "OAuth authorization endpoint origin does not match the authorization server origin without issuer-bound callbacks".to_owned(),
        );
    }

    if token_endpoint.origin() != authorization_endpoint.origin() {
        return Err(
            "OAuth token endpoint origin does not match the authorization server origin without issuer-bound callbacks".to_owned(),
        );
    }
    Ok(())
}

pub(crate) async fn transport_from_credentials(
    server_name: &str,
    server_url: &str,
    http_client: reqwest::Client,
    store: Arc<dyn McpOAuthStore>,
    credentials: McpOAuthCredentials,
    metadata_cache: &OAuthMetadataCache,
) -> Result<OAuthTransport, String> {
    let mut manager = AuthorizationManager::new(server_url)
        .await
        .map_err(|error| format!("failed to initialize MCP OAuth state: {error}"))?;
    manager
        .with_client(http_client.clone())
        .map_err(|error| format!("failed to configure MCP OAuth HTTP client: {error}"))?;
    let (metadata, metadata_cache_hit) =
        if let Some(metadata) = metadata_cache.get(server_name, server_url).await {
            (metadata, true)
        } else {
            let metadata = manager
                .resolve_metadata()
                .await
                .map_err(|error| format!("failed to discover MCP OAuth metadata: {error}"))?
                .metadata;
            metadata_cache
                .insert(server_name, server_url, metadata.clone())
                .await;
            (metadata, false)
        };
    validate_authorization_server_endpoints(&metadata)?;
    let authorization_issuer = authorization_issuer(&metadata)?;
    manager.set_metadata(metadata);

    let staged_credentials = credentials_for_manager(&credentials, authorization_issuer.as_deref());
    let credential_store = InMemoryCredentialStore::new();
    credential_store
        .save(
            StoredCredentials::new(
                staged_credentials.client_id.clone(),
                Some(staged_credentials.to_token_response()),
                staged_credentials.scopes.clone(),
                Some(now_seconds()),
            )
            .with_issuer(staged_credentials.issuer.clone()),
        )
        .await
        .map_err(|error| format!("failed to stage MCP OAuth credentials: {error}"))?;
    manager.set_credential_store(credential_store);
    let restored = manager
        .initialize_from_store()
        .await
        .map_err(|error| format!("failed to restore MCP OAuth credentials: {error}"))?;
    if !restored {
        return Err("restored MCP OAuth state was not authorized".to_owned());
    }
    let client = AuthClient::new(http_client, manager);
    let runtime = Arc::new(OAuthRuntime::new(
        server_name.to_owned(),
        server_url.to_owned(),
        Arc::clone(&client.auth_manager),
        store,
        authorization_issuer,
        credentials,
    ));
    Ok(OAuthTransport {
        client,
        runtime,
        metadata_cache_hit,
    })
}

pub(crate) struct OAuthLoginFlow {
    pub(crate) authorization_url: String,
    pub(crate) completion: JoinHandle<Result<(), String>>,
}

pub(crate) async fn begin_login(
    server_name: String,
    server_url: String,
    headers: BTreeMap<String, SecretSource>,
    store: Arc<dyn McpOAuthStore>,
) -> Result<OAuthLoginFlow, String> {
    let client = oauth_http_client(headers)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("failed to bind MCP OAuth callback: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("failed to inspect MCP OAuth callback: {error}"))?;
    let redirect_uri = format!("http://{address}/callback");
    let authorization_span = info_span!(
        target: "nanocodex_tools",
        "mcp.oauth.authorization_start",
        otel.kind = "client",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let authorization = async {
        let mut manager = AuthorizationManager::new(&server_url)
            .await
            .map_err(|error| format!("failed to discover MCP OAuth metadata: {error}"))?;
        manager
            .with_client(client)
            .map_err(|error| format!("failed to configure MCP OAuth HTTP client: {error}"))?;
        let metadata = manager
            .resolve_metadata()
            .await
            .map_err(|error| format!("failed to discover MCP OAuth metadata: {error}"))?
            .metadata;
        validate_authorization_server_endpoints(&metadata)?;
        let authorization_issuer = authorization_issuer(&metadata)?;
        manager.set_metadata(metadata);
        let session = AuthorizationSession::new(
            manager,
            AuthorizationRequest::new(&redirect_uri).with_client_name("Nanocodex"),
        )
        .await
        .map_err(|(_, error)| format!("failed to start MCP OAuth authorization: {error}"))?;
        let authorization_url = session.get_authorization_url().to_owned();
        Ok::<_, String>((session, authorization_url, authorization_issuer))
    }
    .instrument(authorization_span.clone())
    .await;
    authorization_span.record(
        "status",
        if authorization.is_ok() {
            "completed"
        } else {
            "failed"
        },
    );
    authorization_span.record(
        "otel.status_code",
        if authorization.is_ok() { "OK" } else { "ERROR" },
    );
    let (session, authorization_url, authorization_issuer) = authorization?;

    let parent = tracing::Span::current();
    let completion = tokio::spawn(
        complete_login(
            listener,
            redirect_uri,
            session,
            authorization_issuer,
            store,
            server_name,
            server_url,
        )
        .instrument(parent),
    );
    Ok(OAuthLoginFlow {
        authorization_url,
        completion,
    })
}

async fn complete_login(
    listener: TcpListener,
    redirect_uri: String,
    session: AuthorizationSession,
    authorization_issuer: Option<String>,
    store: Arc<dyn McpOAuthStore>,
    server_name: String,
    server_url: String,
) -> Result<(), String> {
    let callback_span = info_span!(
        target: "nanocodex_tools",
        "mcp.oauth.callback_wait",
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let callback =
        match tokio::time::timeout(LOGIN_TIMEOUT, receive_callback(listener, &redirect_uri))
            .instrument(callback_span.clone())
            .await
        {
            Ok(callback) => callback,
            Err(_) => Err("timed out waiting for MCP OAuth callback".to_owned()),
        };
    callback_span.record(
        "status",
        if callback.is_ok() {
            "completed"
        } else {
            "failed"
        },
    );
    callback_span.record(
        "otel.status_code",
        if callback.is_ok() { "OK" } else { "ERROR" },
    );
    let callback = callback?;
    let exchange_span = info_span!(
        target: "nanocodex_tools",
        "mcp.oauth.code_exchange",
        otel.kind = "client",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
    );
    let result = session
        .handle_callback_url(&callback)
        .instrument(exchange_span.clone())
        .await
        .map_err(|error| format!("failed to exchange MCP OAuth code: {error}"));
    exchange_span.record(
        "status",
        if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
    );
    exchange_span.record(
        "otel.status_code",
        if result.is_ok() { "OK" } else { "ERROR" },
    );
    result?;
    let (client_id, response) = session
        .get_credentials()
        .await
        .map_err(|error| format!("failed to read MCP OAuth credentials: {error}"))?;
    let response =
        response.ok_or_else(|| "MCP OAuth provider returned no credentials".to_owned())?;
    let credentials =
        McpOAuthCredentials::from_token_response(client_id, &response, authorization_issuer);
    let save_span = info_span!(
        target: "nanocodex_tools",
        "mcp.oauth.credentials_save",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        reason = "login",
        status = tracing::field::Empty,
    );
    let saved = store
        .save(&server_name, &server_url, &credentials)
        .instrument(save_span.clone())
        .await;
    save_span.record("status", if saved.is_ok() { "completed" } else { "failed" });
    save_span.record(
        "otel.status_code",
        if saved.is_ok() { "OK" } else { "ERROR" },
    );
    saved
}

fn oauth_http_client(headers: BTreeMap<String, SecretSource>) -> Result<reqwest::Client, String> {
    let mut resolved = reqwest::header::HeaderMap::with_capacity(headers.len());
    for (name, source) in headers {
        let name = name
            .parse::<HeaderName>()
            .map_err(|error| format!("invalid HTTP header name `{name}`: {error}"))?;
        let value = source.resolve()?;
        let mut value = HeaderValue::from_str(&value)
            .map_err(|error| format!("invalid value for HTTP header `{name}`: {error}"))?;
        value.set_sensitive(true);
        resolved.insert(name, value);
    }
    let replays_plaintext_proxy_credentials =
        resolved.contains_key(reqwest::header::PROXY_AUTHORIZATION);
    nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
    reqwest::Client::builder()
        .default_headers(resolved)
        .pool_max_idle_per_host(0)
        .redirect(super::same_origin_redirect_policy(
            replays_plaintext_proxy_credentials,
        ))
        .build()
        .map_err(|error| format!("failed to build MCP OAuth HTTP client: {error}"))
}

async fn receive_callback(listener: TcpListener, redirect_uri: &str) -> Result<String, String> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|error| format!("failed to accept MCP OAuth callback: {error}"))?;
    let mut bytes = Vec::with_capacity(2048);
    loop {
        let mut chunk = [0_u8; 1024];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| format!("failed to read MCP OAuth callback: {error}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > MAX_CALLBACK_BYTES {
            return Err("MCP OAuth callback headers were too large".to_owned());
        }
    }
    let request = std::str::from_utf8(&bytes)
        .map_err(|_| "MCP OAuth callback was not valid HTTP".to_owned())?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "MCP OAuth callback did not contain a request target".to_owned())?;
    let base = reqwest::Url::parse(redirect_uri)
        .map_err(|error| format!("invalid MCP OAuth redirect URI: {error}"))?;
    let callback = base
        .join(target)
        .map_err(|error| format!("invalid MCP OAuth callback target: {error}"))?;
    if callback.path() != base.path() {
        let _ = respond(&mut stream, 400, "Invalid OAuth callback path").await;
        return Err("MCP OAuth callback used an unexpected path".to_owned());
    }
    respond(
        &mut stream,
        200,
        "Authentication received. You may close this window.",
    )
    .await?;
    Ok(callback.to_string())
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<(), String> {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| format!("failed to answer MCP OAuth callback: {error}"))
}

fn now_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[derive(Default)]
    struct RecordingStore {
        current: Mutex<Option<McpOAuthCredentials>>,
        saved: Mutex<Vec<McpOAuthCredentials>>,
    }

    impl RecordingStore {
        fn with_credentials(credentials: McpOAuthCredentials) -> Self {
            Self {
                current: Mutex::new(Some(credentials)),
                saved: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl McpOAuthStore for RecordingStore {
        async fn load(
            &self,
            _server_name: &str,
            _server_url: &str,
        ) -> Result<Option<McpOAuthCredentials>, String> {
            Ok(self.current.lock().await.clone())
        }

        async fn save(
            &self,
            _server_name: &str,
            _server_url: &str,
            credentials: &McpOAuthCredentials,
        ) -> Result<(), String> {
            self.saved.lock().await.push(credentials.clone());
            *self.current.lock().await = Some(credentials.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn oauth_headers_do_not_follow_cross_origin_redirects() {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_url = format!("http://{}/metadata", target.local_addr().unwrap());
        let (target_requested, mut target_requested_rx) = oneshot::channel();
        let target_task = tokio::spawn(async move {
            let (mut stream, _) = target.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            target_requested
                .send(String::from_utf8_lossy(&request[..read]).into_owned())
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });

        let redirect = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_url = format!("http://{}/metadata", redirect.local_addr().unwrap());
        let redirect_task = tokio::spawn(async move {
            let (mut stream, _) = redirect.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).contains("x-api-key: secret"));
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let client = oauth_http_client(BTreeMap::from([(
            "x-api-key".to_owned(),
            SecretSource::Value("secret".to_owned()),
        )]))
        .unwrap();
        let error = client.get(source_url).send().await.unwrap_err();
        assert!(error.is_redirect(), "{error}");
        assert!(matches!(
            target_requested_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        target_task.abort();
        redirect_task.await.unwrap();
    }

    #[tokio::test]
    async fn cached_metadata_preserves_refresh_and_rotated_token_persistence() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let server_url = format!("{issuer}/mcp");
        let token_endpoint = format!("{issuer}/token");
        let responder_issuer = issuer.clone();
        let responder = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 4096];
                let read = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let first_line = request.lines().next().unwrap_or_default();
                let (status, body, complete) = match first_line {
                    line if line.starts_with("GET /mcp ") => {
                        ("404 Not Found", String::new(), false)
                    }
                    line if line.contains("oauth-protected-resource") => (
                        "200 OK",
                        format!(
                            r#"{{"resource":"{responder_issuer}/mcp","authorization_servers":["{responder_issuer}"]}}"#
                        ),
                        false,
                    ),
                    line if line.contains("oauth-authorization-server")
                        || line.contains("openid-configuration") =>
                    {
                        (
                            "200 OK",
                            format!(
                                r#"{{"authorization_endpoint":"{responder_issuer}/authorize","token_endpoint":"{responder_issuer}/token","issuer":"{responder_issuer}"}}"#
                            ),
                            false,
                        )
                    }
                    line if line.starts_with("POST /token ") => (
                        "200 OK",
                        r#"{"access_token":"refreshed-access","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated-refresh","scope":"mcp:tools"}"#.to_owned(),
                        true,
                    ),
                    _ => panic!("unexpected OAuth fixture request: {first_line}"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                if complete {
                    break;
                }
            }
        });

        let server_name = "cached";
        let metadata: AuthorizationMetadata = serde_json::from_value(serde_json::json!({
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": token_endpoint,
            "issuer": issuer,
        }))
        .unwrap();
        let metadata_cache = OAuthMetadataCache::default();
        metadata_cache
            .insert(server_name, &server_url, metadata)
            .await;
        let credentials = McpOAuthCredentials::new("client", "expired-access")
            .refresh_token("refresh-token")
            .issuer(issuer.clone())
            .expires_at_millis(0)
            .scopes(["mcp:tools"]);
        let store = Arc::new(RecordingStore::with_credentials(credentials.clone()));

        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        let transport = transport_from_credentials(
            server_name,
            &server_url,
            reqwest::Client::new(),
            store.clone(),
            credentials,
            &metadata_cache,
        )
        .await
        .unwrap();
        assert!(transport.metadata_cache_hit);
        transport.runtime.refresh_if_needed().await.unwrap();
        responder.await.unwrap();

        let saved = store.saved.lock().await;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].access_token(), "refreshed-access");
        assert_eq!(saved[0].refresh_token_value(), Some("rotated-refresh"));
        assert_eq!(saved[0].authorization_issuer(), Some(issuer.as_str()));
        assert_eq!(saved[0].granted_scopes(), ["mcp:tools"]);
    }

    #[test]
    fn oauth_endpoint_identity_rejects_unbound_delegation() {
        let metadata: AuthorizationMetadata = serde_json::from_value(serde_json::json!({
            "issuer": "https://issuer.example/tenant",
            "authorization_endpoint": "https://login.attacker.example/authorize",
            "token_endpoint": "https://issuer.example/token"
        }))
        .unwrap();
        let error = validate_authorization_server_endpoints(&metadata).unwrap_err();
        assert!(error.contains("authorization endpoint origin"), "{error}");

        let mut issuer_bound = metadata;
        issuer_bound.additional_fields.insert(
            "authorization_response_iss_parameter_supported".to_owned(),
            Value::Bool(true),
        );
        validate_authorization_server_endpoints(&issuer_bound).unwrap();
    }

    #[test]
    fn refresh_tokens_require_the_pinned_authorization_issuer() {
        let missing = McpOAuthCredentials::new("client", "access").refresh_token("refresh");
        assert!(validate_refresh_token_issuer(&missing, Some("https://issuer.example")).is_err());

        let changed = missing.issuer("https://old.example");
        assert!(validate_refresh_token_issuer(&changed, Some("https://issuer.example")).is_err());

        let current = changed.issuer("https://issuer.example");
        validate_refresh_token_issuer(&current, Some("https://issuer.example")).unwrap();
        assert!(validate_refresh_token_issuer(&current, None).is_err());
    }
}
