//! Muse Code device OAuth and account-bound inference-key authentication.
//!
//! Shares Nanocodex's OpenAiAuth snapshots and bounded HTTP recovery. Meta's
//! device protocol differs from ChatGPT's PKCE/device-code exchange. Parameters
//! are verified against Muse CLI and the OpenCode Muse plugins (see README).
//! Credentials and recovery state remain in memory; callers own persistence.
use std::{fmt, sync::Arc, time::Duration};

use nanocodex_oai_api::auth::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};
use reqwest::{
    Client, Response,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::Mutex,
    time::{Instant, timeout},
};

const ISSUER: &str = "https://auth.meta.com";
const API_ORIGIN: &str = "https://api.meta.ai";
const CLIENT_ID: &str = "1031625952748946";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// Failure resolving or exchanging Muse credentials. Never contains tokens or HTTP bodies.
#[derive(Debug, thiserror::Error)]
pub enum MuseAuthError {
    /// A credential document or server response is invalid.
    #[error("Muse authorization data is invalid")]
    Invalid,
    /// A bounded network operation failed.
    #[error("Muse authorization service is unavailable")]
    Unavailable,
    /// An endpoint rejected the exchange.
    #[error("Muse authorization exchange returned HTTP {0}")]
    Http(u16),
    /// The exchange is rate limited; the delay is receipt-time server advice.
    #[error("Muse authorization exchange is rate limited; retry after {retry_after:?}")]
    RateLimited {
        /// Remaining delay requested by Meta, or a conservative fallback.
        retry_after: Duration,
    },
    /// The device login expired or approval was denied.
    #[error("Muse device login expired or was denied")]
    LoginRejected,
    /// No usable credential exists.
    #[error("Muse login required")]
    LoginRequired,
    /// The account has no active Muse subscription.
    #[error("an active Muse Code subscription is required")]
    SubscriptionRequired,
}

/// Muse identity and inference credentials. Both are explicitly available to the caller.
/// Serialization is opt-in: the library never writes this value to disk or agent history.
/// Debug output omits both secrets. No expiry is assumed when Meta supplies none.
#[derive(Clone, Deserialize, Serialize)]
pub struct MuseCredential {
    /// OAuth identity token used only at `/muse-code/key`. Absent for API-key-only auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// Inference bearer used at the Responses endpoint.
    pub api_key: String,
}

impl MuseCredential {
    fn validate(&self) -> Result<(), MuseAuthError> {
        bearer(&self.api_key)?;
        if let Some(token) = &self.access_token {
            bearer(token)?;
        }
        Ok(())
    }
}

impl fmt::Debug for MuseCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MuseCredential")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[redacted]"),
            )
            .field("api_key", &"[redacted]")
            .finish()
    }
}

/// In-memory credential manager using Nanocodex's auth-source and snapshot conventions.
/// Clones and authorizations share one serialized exchange, including late-401 protection.
/// Call `credentials()` after a turn to obtain any rotated key for host-owned storage.
/// Separate instances/processes require host coordination if they share an identity.
#[derive(Clone)]
pub struct MuseAuth {
    source: Arc<MuseAuthSource>,
}

impl fmt::Debug for MuseAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("MuseAuth").finish_non_exhaustive()
    }
}

impl MuseAuth {
    /// Creates an in-memory manager. An existing inference key is reused without exchange.
    ///
    /// # Errors
    /// Returns an error for empty or invalid bearer credentials.
    pub fn new(credentials: MuseCredential) -> Result<Self, MuseAuthError> {
        Self::with_endpoint(credentials, key_url(API_ORIGIN))
    }

    fn with_endpoint(credentials: MuseCredential, key_url: String) -> Result<Self, MuseAuthError> {
        credentials.validate()?;
        Ok(Self {
            source: Arc::new(MuseAuthSource {
                client: auth_client()?,
                key_url,
                state: Mutex::new(MuseAuthState {
                    credentials,
                    revision: 0,
                    failure: None,
                }),
            }),
        })
    }

    /// Authorization accepted by `Muse::builder`; it shares this manager's state.
    #[must_use]
    pub fn authorization(&self) -> OpenAiAuth {
        OpenAiAuth::managed_api_key(self.source.clone())
    }

    /// Copies both current credentials for caller-owned persistence or inspection.
    pub async fn credentials(&self) -> MuseCredential {
        self.source.state.lock().await.credentials.clone()
    }
}

struct MuseAuthSource {
    client: Client,
    key_url: String,
    state: Mutex<MuseAuthState>,
}

struct MuseAuthState {
    credentials: MuseCredential,
    revision: u64,
    // None deadline means terminal rejection, requiring a new manager/login.
    failure: Option<(OpenAiAuthError, Option<Instant>)>,
}

impl OpenAiAuthSource for MuseAuthSource {
    fn validate(&self) -> Result<(), OpenAiAuthError> {
        Ok(())
    }

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async move {
            let state = self.state.lock().await;
            Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ApiKey,
                state.credentials.api_key.clone(),
                None::<Arc<str>>,
                false,
                state.revision,
            ))
        })
    }

    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>> {
        let rejected = rejected.clone();
        Box::pin(async move {
            let mut state = self.state.lock().await;
            // Lifted from Codex/Claude: a late 401 cannot discard a newer generation.
            if state.revision != rejected.revision()
                || state.credentials.api_key != rejected.bearer()
            {
                return Ok(());
            }
            if let Some((error, deadline)) = &state.failure
                && deadline.is_none_or(|deadline| Instant::now() < deadline)
            {
                return Err(error.clone());
            }
            let result = match state.credentials.access_token.as_deref() {
                Some(access) => mint_key(&self.client, &self.key_url, access, false).await,
                None => Err(MuseAuthError::LoginRequired),
            };
            match result {
                Ok(key) => {
                    state.credentials.api_key = key;
                    // Meta may return the same key; generation still prevents duplicate exchanges.
                    state.revision = state.revision.wrapping_add(1);
                    state.failure = None;
                    Ok(())
                }
                Err(error) => {
                    let error = match error {
                        MuseAuthError::Http(401 | 403) => MuseAuthError::LoginRequired,
                        error => error,
                    };
                    let terminal = matches!(
                        error,
                        MuseAuthError::LoginRequired | MuseAuthError::SubscriptionRequired
                    );
                    let delay = match &error {
                        MuseAuthError::RateLimited { retry_after } => *retry_after,
                        _ => REQUEST_TIMEOUT,
                    };
                    let error = OpenAiAuthError::Provider(error.to_string().into());
                    state.failure = Some((
                        error.clone(),
                        if terminal {
                            None
                        } else {
                            Instant::now().checked_add(delay)
                        },
                    ));
                    Err(error)
                }
            }
        })
    }
}

/// An in-progress Meta device login. Debug output deliberately omits device credentials.
pub struct MuseLogin {
    device: DeviceAuthorization,
    client: Client,
    token_url: String,
    key_url: String,
    deadline: Instant,
}

impl fmt::Debug for MuseLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("MuseLogin").finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    interval: u64,
    expires_in: u64,
}

const fn default_interval() -> u64 {
    5
}

impl MuseLogin {
    /// Starts device OAuth; display the verification URL and user code, then call `complete`.
    ///
    /// # Errors
    /// Returns a redacted error if authorization cannot be started.
    pub async fn start() -> Result<Self, MuseAuthError> {
        Self::start_with_endpoints(ISSUER, API_ORIGIN).await
    }

    async fn start_with_endpoints(issuer: &str, origin: &str) -> Result<Self, MuseAuthError> {
        let client = auth_client()?;
        let response = client
            .post(format!("{issuer}/oidc/device/authorization/"))
            .form(&[("client_id", CLIENT_ID)])
            .send()
            .await
            .map_err(|_| MuseAuthError::Unavailable)?;
        let value = successful_json(response).await?;
        let device: DeviceAuthorization =
            serde_json::from_value(value).map_err(|_| MuseAuthError::Invalid)?;
        if device.device_code.trim().is_empty()
            || device.user_code.trim().is_empty()
            || device.expires_in == 0
            || !valid_verification_url(&device.verification_uri)
            || device
                .verification_uri_complete
                .as_deref()
                .is_some_and(|url| !valid_verification_url(url))
        {
            return Err(MuseAuthError::Invalid);
        }
        let deadline = Instant::now() + Duration::from_secs(device.expires_in.min(1800));
        Ok(Self {
            device,
            client,
            token_url: format!("{issuer}/oidc/device/token/"),
            key_url: key_url(origin),
            deadline,
        })
    }

    /// URL the user opens to approve this device (may contain their user code).
    #[must_use]
    pub fn verification_url(&self) -> &str {
        self.device
            .verification_uri_complete
            .as_deref()
            .unwrap_or(&self.device.verification_uri)
    }

    /// Code the user verifies or enters on Meta's login page.
    #[must_use]
    pub fn user_code(&self) -> &str {
        &self.device.user_code
    }

    /// Polls within the device deadline and returns both OAuth and inference credentials.
    /// Cancelling this future stops polling. `slow_down` increases the interval by five seconds.
    ///
    /// # Errors
    /// Returns a redacted error for rejection, timeout, inactive subscription.
    pub async fn complete(self) -> Result<MuseCredential, MuseAuthError> {
        timeout(
            self.deadline.saturating_duration_since(Instant::now()),
            self.poll_and_exchange(),
        )
        .await
        .map_err(|_| MuseAuthError::LoginRejected)?
    }

    async fn poll_and_exchange(&self) -> Result<MuseCredential, MuseAuthError> {
        let mut interval = self.device.interval.clamp(1, 30);
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            let response = self
                .client
                .post(&self.token_url)
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", CLIENT_ID),
                    ("device_code", self.device.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(|_| MuseAuthError::Unavailable)?;
            let status = response.status();
            let value = bounded_json(response).await?;
            match value.get("error").and_then(Value::as_str) {
                Some("authorization_pending") if status.as_u16() == 400 || status.is_success() => {
                    continue;
                }
                Some("slow_down") if status.as_u16() == 400 || status.is_success() => {
                    interval = interval.saturating_add(5);
                    continue;
                }
                Some(_) => return Err(MuseAuthError::LoginRejected),
                None if !status.is_success() => return Err(MuseAuthError::Http(status.as_u16())),
                None => {}
            }
            let access = nonempty(&value, "access_token")?;
            return exchange_with_client(&self.client, &self.key_url, access).await;
        }
    }
}

/// Exchanges an OAuth identity token and returns both credentials without storing them.
///
/// # Errors
/// Returns a redacted error for an invalid token or failed exchange.
pub async fn exchange_muse_key(access_token: &str) -> Result<MuseCredential, MuseAuthError> {
    exchange_with_client(&auth_client()?, &key_url(API_ORIGIN), access_token).await
}

async fn exchange_with_client(
    client: &Client,
    key_url: &str,
    access: &str,
) -> Result<MuseCredential, MuseAuthError> {
    let api_key = mint_key(client, key_url, access, true).await?;
    Ok(MuseCredential {
        access_token: Some(access.to_owned()),
        api_key,
    })
}

fn key_url(origin: &str) -> String {
    format!("{origin}/muse-code/key")
}

fn bearer(value: &str) -> Result<HeaderValue, MuseAuthError> {
    if value.trim().is_empty() {
        return Err(MuseAuthError::Invalid);
    }
    let mut header =
        HeaderValue::from_str(&format!("Bearer {value}")).map_err(|_| MuseAuthError::Invalid)?;
    header.set_sensitive(true);
    Ok(header)
}

async fn mint_key(
    client: &Client,
    key_url: &str,
    access: &str,
    onboard: bool,
) -> Result<String, MuseAuthError> {
    let response = client
        .post(key_url)
        .header(AUTHORIZATION, bearer(access)?)
        .json(&if onboard {
            serde_json::json!({"onboard": true})
        } else {
            serde_json::json!({})
        })
        .send()
        .await
        .map_err(|_| MuseAuthError::Unavailable)?;
    if response.status().as_u16() == 429 {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(nanocodex_oai_api::transport::RetryAfter::from_header)
            .map_or(
                REQUEST_TIMEOUT,
                nanocodex_oai_api::transport::RetryAfter::remaining_delay,
            );
        return Err(MuseAuthError::RateLimited { retry_after });
    }
    let value = successful_json(response).await?;
    if value.get("is_subs_active").and_then(Value::as_bool) == Some(false)
        || value.get("require_payment").and_then(Value::as_bool) == Some(true)
    {
        return Err(MuseAuthError::SubscriptionRequired);
    }
    let key = nonempty(&value, "api_key")?;
    bearer(key)?;
    Ok(key.to_owned())
}

fn auth_client() -> Result<Client, MuseAuthError> {
    nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(
            [
                (
                    reqwest::header::USER_AGENT,
                    HeaderValue::from_static(concat!("nanocodex/", env!("CARGO_PKG_VERSION"))),
                ),
                (
                    reqwest::header::ACCEPT,
                    HeaderValue::from_static("application/json"),
                ),
                (
                    reqwest::header::HeaderName::from_static(crate::API_VERSION_HEADER.0),
                    HeaderValue::from_static(crate::API_VERSION_HEADER.1),
                ),
            ]
            .into_iter()
            .collect(),
        )
        .build()
        .map_err(|_| MuseAuthError::Unavailable)
}

async fn successful_json(response: Response) -> Result<Value, MuseAuthError> {
    if !response.status().is_success() {
        return Err(MuseAuthError::Http(response.status().as_u16()));
    }
    bounded_json(response).await
}

async fn bounded_json(mut response: Response) -> Result<Value, MuseAuthError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| MuseAuthError::Unavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(MuseAuthError::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| MuseAuthError::Invalid)
}

fn nonempty<'a>(value: &'a Value, name: &str) -> Result<&'a str, MuseAuthError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(MuseAuthError::Invalid)
}

fn valid_verification_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

#[cfg(test)]
mod tests;
