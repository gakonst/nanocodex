use std::{
    fmt,
    future::{Future, ready},
    pin::Pin,
    sync::Arc,
};

#[cfg(not(target_family = "wasm"))]
mod chatgpt;
mod subscription;

#[cfg(not(target_family = "wasm"))]
#[cfg_attr(docsrs, doc(cfg(not(target_family = "wasm"))))]
pub use chatgpt::{
    ChatGptAuthError, ChatGptAuthStatus, ChatGptLogin, chatgpt_access_token, chatgpt_auth_status,
    load_chatgpt_auth, logout_chatgpt, resolve_chatgpt_auth_status,
};
pub use subscription::{
    ChatGptCredential, ChatGptCredentialSeed, ChatGptLoginStatus, ChatGptSubscription,
    ChatGptSubscriptionError, ChatGptSubscriptionHost, SubscriptionCommit, SubscriptionFuture,
    SubscriptionHostError, SubscriptionHttpRequest, SubscriptionHttpResponse,
    SubscriptionStoreValue,
};

/// Authentication mode for the single `OpenAI` service family supported by Nanocodex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenAiAuthMode {
    /// `OpenAI` Platform API key authentication.
    ApiKey,
    /// Managed `ChatGPT` subscription authentication.
    ChatGpt,
}

impl OpenAiAuthMode {
    /// Whether the default endpoint for this authorization mode supports
    /// retained Responses checkpoints.
    #[must_use]
    pub const fn supports_stored_responses(self) -> bool {
        matches!(self, Self::ApiKey)
    }

    /// Returns the default HTTPS API root for this authentication mode.
    #[must_use]
    pub const fn default_api_base_url(self) -> &'static str {
        match self {
            Self::ApiKey => "https://api.openai.com/v1",
            Self::ChatGpt => "https://chatgpt.com/backend-api/codex",
        }
    }

    /// Returns the default Responses WebSocket URL for this authentication mode.
    #[must_use]
    pub const fn default_websocket_url(self) -> &'static str {
        match self {
            Self::ApiKey => "wss://api.openai.com/v1/responses",
            Self::ChatGpt => "wss://chatgpt.com/backend-api/codex/responses",
        }
    }
}

/// One immutable authorization value used for an HTTP request or WebSocket handshake.
///
/// The bearer value is deliberately omitted from `Debug` output. A revision identifies
/// the credential generation that a rejected request used, allowing concurrent callers to
/// observe another caller's completed refresh without reusing a rotating refresh token.
#[derive(Clone)]
pub struct OpenAiAuthSnapshot {
    mode: OpenAiAuthMode,
    bearer: Arc<str>,
    account_id: Option<Arc<str>>,
    fedramp: bool,
    revision: u64,
}

impl OpenAiAuthSnapshot {
    #[doc(hidden)]
    #[must_use]
    pub fn new(
        mode: OpenAiAuthMode,
        bearer: impl Into<Arc<str>>,
        account_id: Option<impl Into<Arc<str>>>,
        fedramp: bool,
        revision: u64,
    ) -> Self {
        Self {
            mode,
            bearer: bearer.into(),
            account_id: account_id.map(Into::into),
            fedramp,
            revision,
        }
    }

    /// Returns the authentication mode captured by this snapshot.
    #[must_use]
    pub const fn mode(&self) -> OpenAiAuthMode {
        self.mode
    }

    #[doc(hidden)]
    #[must_use]
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    /// Returns the `ChatGPT` account ID when the credential is account-scoped.
    #[must_use]
    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    /// Returns whether the credential targets a `FedRAMP` environment.
    #[must_use]
    pub const fn is_fedramp(&self) -> bool {
        self.fedramp
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl fmt::Debug for OpenAiAuthSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiAuthSnapshot")
            .field("mode", &self.mode)
            .field("bearer", &"[redacted]")
            .field("account_id", &self.account_id)
            .field("fedramp", &self.fedramp)
            .field("revision", &self.revision)
            .finish()
    }
}

/// Error produced while resolving or refreshing `OpenAI` credentials.
#[derive(Clone, Debug, thiserror::Error)]
pub enum OpenAiAuthError {
    /// The configured credential contained no non-whitespace bytes.
    #[error("OpenAI credentials are empty")]
    Empty,
    /// The managed credential source is temporarily unavailable.
    #[error("OpenAI credentials are unavailable: {0}")]
    Unavailable(Arc<str>),
    /// A refreshed credential resolved to a different `ChatGPT` account.
    #[error("the stored ChatGPT account changed while the agent was active")]
    AccountChanged,
    /// The user must complete an interactive login before retrying.
    #[error("ChatGPT authorization must be refreshed by logging in again: {0}")]
    LoginRequired(Arc<str>),
    /// Managed `ChatGPT` credential refresh failed.
    #[error("failed to refresh ChatGPT authorization: {0}")]
    Refresh(Arc<str>),
}

#[cfg(not(target_family = "wasm"))]
/// Boxed native future returned by a managed authentication source.
pub type OpenAiAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_family = "wasm")]
/// Boxed browser future returned by a managed authentication source.
pub type OpenAiAuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Private cross-crate capability behind [`OpenAiAuth`].
///
/// This is public only because the concrete managed ChatGPT implementation and its consumers
/// live in separate Nanocodex crates. Applications should use [`OpenAiAuth`] constructors.
#[doc(hidden)]
pub trait OpenAiAuthSource: Send + Sync {
    fn validate(&self) -> Result<(), OpenAiAuthError>;

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>>;

    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>>;
}

/// Cloneable `OpenAI` authorization shared by one agent family and its branches.
#[derive(Clone)]
pub struct OpenAiAuth {
    mode: OpenAiAuthMode,
    source: Arc<dyn OpenAiAuthSource>,
}

impl OpenAiAuth {
    /// Creates static `OpenAI` Platform API-key authentication.
    #[must_use]
    pub fn api_key(api_key: impl Into<Arc<str>>) -> Self {
        let source = ApiKeyAuth {
            api_key: api_key.into(),
        };
        Self {
            mode: OpenAiAuthMode::ApiKey,
            source: Arc::new(source),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn managed_chatgpt(source: Arc<dyn OpenAiAuthSource>) -> Self {
        Self {
            mode: OpenAiAuthMode::ChatGpt,
            source,
        }
    }

    /// Returns the authentication mode.
    #[must_use]
    pub const fn mode(&self) -> OpenAiAuthMode {
        self.mode
    }

    /// Checks that this authorization can provide credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials are empty, unavailable, or require a new login.
    pub fn validate(&self) -> Result<(), OpenAiAuthError> {
        self.source.validate()
    }

    /// Resolves one immutable credential generation for an outbound request.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials cannot be loaded or refreshed.
    pub async fn snapshot(&self) -> Result<OpenAiAuthSnapshot, OpenAiAuthError> {
        self.source.snapshot().await
    }

    /// Recovers after the service rejects a credential snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery fails or the user must log in again.
    pub async fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> Result<(), OpenAiAuthError> {
        self.source.recover_unauthorized(rejected).await
    }
}

impl fmt::Debug for OpenAiAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiAuth")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl From<String> for OpenAiAuth {
    fn from(api_key: String) -> Self {
        Self::api_key(api_key)
    }
}

impl From<&str> for OpenAiAuth {
    fn from(api_key: &str) -> Self {
        Self::api_key(api_key.to_owned())
    }
}

#[derive(Debug)]
struct ApiKeyAuth {
    api_key: Arc<str>,
}

impl OpenAiAuthSource for ApiKeyAuth {
    fn validate(&self) -> Result<(), OpenAiAuthError> {
        if self.api_key.trim().is_empty() {
            Err(OpenAiAuthError::Empty)
        } else {
            Ok(())
        }
    }

    fn snapshot(&self) -> OpenAiAuthFuture<'_, Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        let result = self.validate().map(|()| {
            OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ApiKey,
                Arc::clone(&self.api_key),
                None::<Arc<str>>,
                false,
                0,
            )
        });
        Box::pin(ready(result))
    }

    fn recover_unauthorized(
        &self,
        _rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, Result<(), OpenAiAuthError>> {
        Box::pin(ready(Err(OpenAiAuthError::LoginRequired(Arc::from(
            "the API key was rejected",
        )))))
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenAiAuth, OpenAiAuthMode};

    #[tokio::test]
    async fn api_key_snapshots_are_redacted() {
        let auth = OpenAiAuth::api_key("secret-sentinel");
        let snapshot = auth.snapshot().await.unwrap();
        assert_eq!(snapshot.mode(), OpenAiAuthMode::ApiKey);
        assert_eq!(snapshot.bearer(), "secret-sentinel");
        assert!(!format!("{auth:?}{snapshot:?}").contains("secret-sentinel"));
    }

    #[test]
    fn auth_modes_select_their_service_endpoints() {
        assert_eq!(
            OpenAiAuthMode::ApiKey.default_websocket_url(),
            "wss://api.openai.com/v1/responses"
        );
        assert_eq!(
            OpenAiAuthMode::ChatGpt.default_websocket_url(),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            OpenAiAuthMode::ChatGpt.default_api_base_url(),
            "https://chatgpt.com/backend-api/codex"
        );
    }
}
