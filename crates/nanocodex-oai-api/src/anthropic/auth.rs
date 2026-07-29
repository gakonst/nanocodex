//! Anthropic authorization shared by one agent family and its branches.
//!
//! This mirrors [`crate::OpenAiAuth`]: the concrete credential store, OAuth login, and
//! refresh live beside this transport-free handle, which the service layer consumes.

use std::{
    fmt,
    future::{Future, ready},
    pin::Pin,
    sync::Arc,
};

/// The Anthropic API version header sent on every request.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The beta header required when authorizing with an OAuth access token.
///
/// OAuth tokens are rejected on `x-api-key`; they must be sent as a bearer token
/// alongside this header. The requirement is endpoint-dependent, but `/v1/messages`
/// enforces it, so it is always sent.
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";

/// Authentication mode for the Anthropic service family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnthropicAuthMode {
    /// A long-lived `ANTHROPIC_API_KEY`, sent on the `x-api-key` header.
    ApiKey,
    /// A short-lived OAuth access token, sent as a bearer token.
    OAuth,
}

impl AnthropicAuthMode {
    /// Returns the default Anthropic API base URL.
    #[must_use]
    pub const fn default_api_base_url(self) -> &'static str {
        "https://api.anthropic.com/v1"
    }

    /// Whether credentials in this mode expire and must be refreshed before use.
    #[must_use]
    pub const fn is_refreshable(self) -> bool {
        matches!(self, Self::OAuth)
    }
}

/// One immutable authorization value used for an Anthropic HTTP request.
///
/// The bearer value is deliberately omitted from `Debug` output. A revision identifies
/// the credential generation that a rejected request used, allowing concurrent callers to
/// observe another caller's completed refresh without reusing a rotating refresh token.
#[derive(Clone)]
pub struct AnthropicAuthSnapshot {
    mode: AnthropicAuthMode,
    bearer: Arc<str>,
    beta: Option<Arc<str>>,
    revision: u64,
}

impl AnthropicAuthSnapshot {
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        mode: AnthropicAuthMode,
        bearer: impl Into<Arc<str>>,
        beta: Option<impl Into<Arc<str>>>,
        revision: u64,
    ) -> Self {
        Self {
            mode,
            bearer: bearer.into(),
            beta: beta.map(Into::into),
            revision,
        }
    }

    /// Returns the authentication mode represented by this snapshot.
    #[must_use]
    pub const fn mode(&self) -> AnthropicAuthMode {
        self.mode
    }

    #[doc(hidden)]
    #[must_use]
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    /// Extra `anthropic-beta` value this credential requires, if any.
    #[must_use]
    pub fn beta(&self) -> Option<&str> {
        self.beta.as_deref()
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the header name and value carrying this credential.
    ///
    /// API keys authenticate on `x-api-key`; OAuth access tokens authenticate on
    /// `Authorization: Bearer`. Sending an OAuth token on `x-api-key` returns a 401.
    #[must_use]
    pub fn authorization_header(&self) -> (&'static str, String) {
        match self.mode {
            AnthropicAuthMode::ApiKey => ("x-api-key", self.bearer.to_string()),
            AnthropicAuthMode::OAuth => ("authorization", format!("Bearer {}", self.bearer)),
        }
    }
}

impl fmt::Debug for AnthropicAuthSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicAuthSnapshot")
            .field("mode", &self.mode)
            .field("bearer", &"[redacted]")
            .field("beta", &self.beta)
            .field("revision", &self.revision)
            .finish()
    }
}

/// Error produced while resolving or refreshing Anthropic credentials.
#[derive(Clone, Debug, thiserror::Error)]
pub enum AnthropicAuthError {
    /// The configured credential contained no non-whitespace bytes.
    #[error("Anthropic credentials are empty")]
    Empty,
    /// The managed credential source is temporarily unavailable.
    #[error("Anthropic credentials are unavailable: {0}")]
    Unavailable(Arc<str>),
    /// A refreshed credential resolved to a different Anthropic account.
    #[error("the stored Anthropic account changed while the agent was active")]
    AccountChanged,
    /// The user must complete an interactive login before retrying.
    #[error("Anthropic authorization must be refreshed by logging in again: {0}")]
    LoginRequired(Arc<str>),
    /// Managed Anthropic credential refresh failed.
    #[error("failed to refresh Anthropic authorization: {0}")]
    Refresh(Arc<str>),
}

#[cfg(not(target_family = "wasm"))]
/// Boxed native future returned by a managed Anthropic authentication source.
pub type AnthropicAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_family = "wasm")]
/// Boxed browser future returned by a managed Anthropic authentication source.
pub type AnthropicAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Private cross-crate capability behind [`AnthropicAuth`].
///
/// Applications should normally use [`AnthropicAuth`] constructors. The trait remains
/// public so embedding applications can own a refreshing credential source.
#[doc(hidden)]
pub trait AnthropicAuthSource: Send + Sync {
    fn validate(&self) -> Result<(), AnthropicAuthError>;

    fn snapshot(
        &self,
    ) -> AnthropicAuthFuture<'_, Result<AnthropicAuthSnapshot, AnthropicAuthError>>;

    fn recover_unauthorized(
        &self,
        rejected: &AnthropicAuthSnapshot,
    ) -> AnthropicAuthFuture<'_, Result<(), AnthropicAuthError>>;
}

/// Cloneable Anthropic authorization shared by one agent family and its branches.
#[derive(Clone)]
pub struct AnthropicAuth {
    mode: AnthropicAuthMode,
    source: Arc<dyn AnthropicAuthSource>,
}

impl AnthropicAuth {
    /// Creates static Anthropic API-key authentication.
    #[must_use]
    pub fn api_key(api_key: impl Into<Arc<str>>) -> Self {
        let source = ApiKeyAuth {
            api_key: api_key.into(),
        };
        Self {
            mode: AnthropicAuthMode::ApiKey,
            source: Arc::new(source),
        }
    }

    /// Wraps a static OAuth access token that this process will not refresh.
    ///
    /// Prefer the managed constructor in the `nanocodex` crate for anything long-lived:
    /// OAuth access tokens are short lived, and a static token simply starts failing
    /// once it expires.
    #[must_use]
    pub fn oauth_token(access_token: impl Into<Arc<str>>) -> Self {
        let source = StaticOAuthAuth {
            access_token: access_token.into(),
        };
        Self {
            mode: AnthropicAuthMode::OAuth,
            source: Arc::new(source),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn managed_oauth(source: Arc<dyn AnthropicAuthSource>) -> Self {
        Self {
            mode: AnthropicAuthMode::OAuth,
            source,
        }
    }

    /// Returns the authentication mode.
    #[must_use]
    pub const fn mode(&self) -> AnthropicAuthMode {
        self.mode
    }

    /// Checks that this authorization can provide credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials are empty, unavailable, or require a new login.
    pub fn validate(&self) -> Result<(), AnthropicAuthError> {
        self.source.validate()
    }

    /// Resolves one immutable credential generation for an outbound request.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials cannot be loaded or refreshed.
    pub async fn snapshot(&self) -> Result<AnthropicAuthSnapshot, AnthropicAuthError> {
        self.source.snapshot().await
    }

    /// Recovers after the service rejects a credential snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery fails or the user must log in again.
    pub async fn recover_unauthorized(
        &self,
        rejected: &AnthropicAuthSnapshot,
    ) -> Result<(), AnthropicAuthError> {
        self.source.recover_unauthorized(rejected).await
    }
}

impl fmt::Debug for AnthropicAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicAuth")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ApiKeyAuth {
    api_key: Arc<str>,
}

impl AnthropicAuthSource for ApiKeyAuth {
    fn validate(&self) -> Result<(), AnthropicAuthError> {
        if self.api_key.trim().is_empty() {
            Err(AnthropicAuthError::Empty)
        } else {
            Ok(())
        }
    }

    fn snapshot(
        &self,
    ) -> AnthropicAuthFuture<'_, Result<AnthropicAuthSnapshot, AnthropicAuthError>> {
        let result = self.validate().map(|()| {
            AnthropicAuthSnapshot::new(
                AnthropicAuthMode::ApiKey,
                Arc::clone(&self.api_key),
                None::<Arc<str>>,
                0,
            )
        });
        Box::pin(ready(result))
    }

    fn recover_unauthorized(
        &self,
        _rejected: &AnthropicAuthSnapshot,
    ) -> AnthropicAuthFuture<'_, Result<(), AnthropicAuthError>> {
        Box::pin(ready(Err(AnthropicAuthError::LoginRequired(Arc::from(
            "the API key was rejected",
        )))))
    }
}

#[derive(Debug)]
struct StaticOAuthAuth {
    access_token: Arc<str>,
}

impl AnthropicAuthSource for StaticOAuthAuth {
    fn validate(&self) -> Result<(), AnthropicAuthError> {
        if self.access_token.trim().is_empty() {
            Err(AnthropicAuthError::Empty)
        } else {
            Ok(())
        }
    }

    fn snapshot(
        &self,
    ) -> AnthropicAuthFuture<'_, Result<AnthropicAuthSnapshot, AnthropicAuthError>> {
        let result = self.validate().map(|()| {
            AnthropicAuthSnapshot::new(
                AnthropicAuthMode::OAuth,
                Arc::clone(&self.access_token),
                Some(Arc::<str>::from(ANTHROPIC_OAUTH_BETA)),
                0,
            )
        });
        Box::pin(ready(result))
    }

    fn recover_unauthorized(
        &self,
        _rejected: &AnthropicAuthSnapshot,
    ) -> AnthropicAuthFuture<'_, Result<(), AnthropicAuthError>> {
        Box::pin(ready(Err(AnthropicAuthError::LoginRequired(Arc::from(
            "the static OAuth access token expired; run `nanocodex auth login --anthropic`",
        )))))
    }
}

#[cfg(test)]
mod tests {
    use super::{ANTHROPIC_OAUTH_BETA, AnthropicAuth, AnthropicAuthMode};

    #[tokio::test]
    async fn api_key_snapshots_are_redacted() {
        let auth = AnthropicAuth::api_key("secret-sentinel");
        let snapshot = auth.snapshot().await.unwrap();
        assert_eq!(snapshot.mode(), AnthropicAuthMode::ApiKey);
        assert_eq!(snapshot.bearer(), "secret-sentinel");
        assert!(!format!("{auth:?}{snapshot:?}").contains("secret-sentinel"));
    }

    #[tokio::test]
    async fn api_keys_authenticate_on_the_api_key_header() {
        let auth = AnthropicAuth::api_key("sk-ant-key");
        let snapshot = auth.snapshot().await.unwrap();
        let (name, value) = snapshot.authorization_header();
        assert_eq!(name, "x-api-key");
        assert_eq!(value, "sk-ant-key");
        assert!(snapshot.beta().is_none());
    }

    #[tokio::test]
    async fn oauth_tokens_authenticate_as_bearer_with_the_beta_header() {
        let auth = AnthropicAuth::oauth_token("oat-token");
        let snapshot = auth.snapshot().await.unwrap();
        let (name, value) = snapshot.authorization_header();
        assert_eq!(name, "authorization");
        assert_eq!(value, "Bearer oat-token");
        assert_eq!(snapshot.beta(), Some(ANTHROPIC_OAUTH_BETA));
    }

    #[test]
    fn empty_credentials_are_rejected_before_any_request() {
        assert!(AnthropicAuth::api_key("").validate().is_err());
        assert!(AnthropicAuth::oauth_token("   ").validate().is_err());
    }

    #[test]
    fn only_oauth_credentials_are_refreshable() {
        assert!(AnthropicAuthMode::OAuth.is_refreshable());
        assert!(!AnthropicAuthMode::ApiKey.is_refreshable());
    }
}
