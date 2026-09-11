//! A Nanocodex browser Hand backed by a rented Popcorn session.
//!
//! [Popcorn](https://github.com/reclaimprotocol/popcorn-oss) rents isolated
//! headful Chromium sessions that run inside a TEE. Each session exposes a
//! full-access CDP WebSocket, a human-viewable LiveView page, and an
//! attestation route. This module rents one session, points the ordinary
//! [`Browser`] controller at its CDP endpoint, and releases the session on
//! shutdown.
//!
//! From the agent's point of view a Popcorn browser is just another Hand: the
//! brain stays wherever it is, and `tools.browser(...)` executes inside the
//! remote enclave. Because the model never receives the CDP URL, the session
//! token never enters the conversation.
//!
//! # Access paths
//!
//! The default path is Popcorn's **hosted MCP server**, which needs no
//! Reclaim-issued credentials. The first run prints an authorization URL to
//! stderr, the operator approves it in a browser, and the OAuth credentials are
//! persisted so later runs start without a login. Sessions are paid for with
//! prepaid credits bought at <https://popcorn.reclaimprotocol.org>.
//!
//! A **credentialed control plane** remains available for dedicated
//! deployments that issue their own client ID and secret.
//!
//! An agent that would rather not use the native browser tool at all can skip
//! this module and register the Popcorn MCP server as an ordinary Nanocodex MCP
//! server, driving the browser through Popcorn's own tools instead.
//!
//! # Rent a browser and hand it to an agent
//!
//! ```no_run
//! use nanocodex_browser::popcorn::{PopcornBrowser, PopcornConfig};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = PopcornConfig::from_env()?;
//! let browser = PopcornBrowser::spawn(config).await?;
//!
//! println!("watch or take over at {}", browser.live_view_url());
//!
//! let tool = browser.tool();
//! // Pass `tool` to `Tools::builder().provider(tool)`.
//! drop(tool);
//! browser.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Environment read by [`PopcornConfig::from_env`]:
//!
//! - `POPCORN_MCP_URL` (optional): overrides the hosted MCP server URL.
//! - `POPCORN_MCP_CREDENTIALS` (optional): OAuth credential file path.
//! - `POPCORN_PURPOSE` (optional): session purpose shown to the human.
//! - `POPCORN_IDEMPOTENCY_KEY` (optional): reuse to retry without paying twice.
//! - `POPCORN_PROXY_COUNTRY` (optional): ISO 3166-1 alpha-2 proxy exit country.
//! - `POPCORN_REGION` (optional): comma-separated region preference order.
//! - `POPCORN_TTL_SECONDS` (optional): requested lifetime; the hosted MCP
//!   server sells one fixed block and ignores it.
//!
//! Setting any of `POPCORN_CONTROL_PLANE_URL`, `POPCORN_CLIENT_ID`, or
//! `POPCORN_CLIENT_SECRET` selects the credentialed control plane instead, and
//! all three are then required.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nanocodex_tools::mcp::{
    Mcp, McpHandle, McpOAuthCredentials, McpOAuthRefreshGuard, McpOAuthStore, McpServer,
    McpToolCall,
};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::{debug, warn};
use url::Url;

use crate::{Browser, BrowserBuildError, BrowserBuilder, BrowserError, BrowserTool};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Popcorn's hosted Streamable HTTP MCP server.
pub const HOSTED_MCP_URL: &str = "https://popcorn-mcp-gcp.reclaimprotocol.org/mcp";

/// Where a human buys session credits for the hosted MCP server.
pub const CREDIT_CHECKOUT_URL: &str = "https://popcorn.reclaimprotocol.org";

const MCP_SERVER_NAME: &str = "popcorn";
const BALANCE_TOOL: &str = "get_balance";
const CREATE_SESSION_TOOL: &str = "create_browser_session";
const CONNECTION_TOOL: &str = "get_browser_connection";
const LIVE_VIEW_TOOL: &str = "get_live_view";
const END_SESSION_TOOL: &str = "end_browser_session";
const DEFAULT_PURPOSE: &str = "Nanocodex agent browser session";

const SESSION_ID_KEYS: &[&str] = &["session_id", "sessionId"];
const CDP_KEYS: &[&str] = &[
    "cdp_url",
    "cdpUrl",
    "cdp_internal_url",
    "cdpInternalUrl",
    "connect_url",
    "connectUrl",
];
const LIVE_VIEW_KEYS: &[&str] = &[
    "live_view_url",
    "liveViewUrl",
    "live_url",
    "liveUrl",
    "live_view",
    "liveView",
];
const REGION_KEYS: &[&str] = &["region"];
const EXPIRES_KEYS: &[&str] = &["expires_at", "expiresAt"];
const CHECKOUT_KEYS: &[&str] = &["checkout_url", "checkoutUrl"];
const NEXT_ACTION_KEYS: &[&str] = &["next_action", "nextAction"];

/// How a session is rented.
#[derive(Clone)]
enum PopcornAccess {
    /// Popcorn's hosted MCP server, authorized with OAuth and paid with credits.
    HostedMcp {
        server_url: Url,
        oauth_store: Option<Arc<dyn McpOAuthStore>>,
        credentials_path: Option<PathBuf>,
    },
    /// A control plane that issued this client its own credentials.
    ControlPlane {
        control_plane: Url,
        client_id: String,
        client_secret: String,
    },
}

/// Everything needed to rent one Popcorn session.
#[derive(Clone)]
pub struct PopcornConfig {
    access: PopcornAccess,
    regions: Vec<String>,
    ttl_seconds: Option<u64>,
    session_id: Option<String>,
    request_timeout: Duration,
    purpose: Option<String>,
    idempotency_key: Option<String>,
    proxy_country: Option<String>,
}

/// Redacts the client secret and never renders a credential path's contents.
impl std::fmt::Debug for PopcornConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut rendered = f.debug_struct("PopcornConfig");
        match &self.access {
            PopcornAccess::HostedMcp { server_url, .. } => {
                rendered
                    .field("mode", &"hosted_mcp")
                    .field("server_url", server_url);
            }
            PopcornAccess::ControlPlane {
                control_plane,
                client_id,
                ..
            } => {
                rendered
                    .field("mode", &"control_plane")
                    .field("control_plane", control_plane)
                    .field("client_id", client_id)
                    .field("client_secret", &"<redacted>");
            }
        }
        rendered
            .field("regions", &self.regions)
            .field("ttl_seconds", &self.ttl_seconds)
            .field("session_id", &self.session_id)
            .field("request_timeout", &self.request_timeout)
            .field("purpose", &self.purpose)
            .field("idempotency_key", &self.idempotency_key)
            .field("proxy_country", &self.proxy_country)
            .finish()
    }
}

impl PopcornConfig {
    /// Creates a configuration for Popcorn's hosted MCP server.
    ///
    /// The first spawn prints an OAuth authorization URL to stderr; sessions
    /// are paid for with credits bought at [`CREDIT_CHECKOUT_URL`].
    #[must_use]
    pub fn hosted_mcp() -> Self {
        Self::mcp(Url::parse(HOSTED_MCP_URL).unwrap_or_else(|error| {
            unreachable!("the hosted Popcorn MCP URL is a valid URL: {error}")
        }))
    }

    /// Creates a configuration for a self-hosted Popcorn MCP server.
    #[must_use]
    pub fn mcp(server_url: Url) -> Self {
        Self::with_access(PopcornAccess::HostedMcp {
            server_url,
            oauth_store: None,
            credentials_path: None,
        })
    }

    /// Creates a configuration for a credentialed Popcorn control plane.
    ///
    /// This is the option for dedicated deployments that issue their own client
    /// ID and secret; ordinary users want [`PopcornConfig::hosted_mcp`].
    pub fn new(
        control_plane: Url,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self::with_access(PopcornAccess::ControlPlane {
            control_plane,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        })
    }

    const fn with_access(access: PopcornAccess) -> Self {
        Self {
            access,
            regions: Vec::new(),
            ttl_seconds: None,
            session_id: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            purpose: None,
            idempotency_key: None,
            proxy_country: None,
        }
    }

    /// Reads the configuration from `POPCORN_*` environment variables.
    ///
    /// The hosted MCP server is selected unless any of
    /// `POPCORN_CONTROL_PLANE_URL`, `POPCORN_CLIENT_ID`, or
    /// `POPCORN_CLIENT_SECRET` is set, in which case all three are required.
    ///
    /// # Errors
    ///
    /// Returns [`PopcornError::Configuration`] when a required variable is
    /// missing or a URL or number does not parse.
    pub fn from_env() -> Result<Self, PopcornError> {
        Self::from_lookup(&|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: &impl Fn(&str) -> Option<String>) -> Result<Self, PopcornError> {
        let value = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let required = |name: &str| {
            value(name).ok_or_else(|| PopcornError::Configuration {
                message: format!("`{name}` is required"),
            })
        };
        let parsed_url = |name: &str, raw: &str| {
            Url::parse(raw).map_err(|error| PopcornError::Configuration {
                message: format!("`{name}` is not a valid URL: {error}"),
            })
        };

        const CREDENTIALED: [&str; 3] = [
            "POPCORN_CONTROL_PLANE_URL",
            "POPCORN_CLIENT_ID",
            "POPCORN_CLIENT_SECRET",
        ];
        let mut config = if CREDENTIALED.iter().any(|name| value(name).is_some()) {
            let control_plane = required(CREDENTIALED[0])?;
            Self::new(
                parsed_url(CREDENTIALED[0], &control_plane)?,
                required(CREDENTIALED[1])?,
                required(CREDENTIALED[2])?,
            )
        } else {
            let mut config = match value("POPCORN_MCP_URL") {
                Some(url) => Self::mcp(parsed_url("POPCORN_MCP_URL", &url)?),
                None => Self::hosted_mcp(),
            };
            if let Some(path) = value("POPCORN_MCP_CREDENTIALS") {
                config = config.credentials_path(path);
            }
            config
        };

        if let Some(regions) = value("POPCORN_REGION") {
            config = config.regions(
                regions
                    .split(',')
                    .map(str::trim)
                    .filter(|region| !region.is_empty())
                    .map(str::to_owned),
            );
        }
        if let Some(ttl) = value("POPCORN_TTL_SECONDS") {
            let ttl = ttl
                .parse::<u64>()
                .map_err(|error| PopcornError::Configuration {
                    message: format!("`POPCORN_TTL_SECONDS` must be a positive integer: {error}"),
                })?;
            config = config.ttl_seconds(ttl);
        }
        if let Some(purpose) = value("POPCORN_PURPOSE") {
            config = config.purpose(purpose);
        }
        if let Some(key) = value("POPCORN_IDEMPOTENCY_KEY") {
            config = config.idempotency_key(key);
        }
        if let Some(country) = value("POPCORN_PROXY_COUNTRY") {
            config = config.proxy_country(country);
        }
        Ok(config)
    }

    /// Returns whether this configuration rents through an MCP server.
    #[must_use]
    pub const fn uses_mcp(&self) -> bool {
        matches!(self.access, PopcornAccess::HostedMcp { .. })
    }

    /// Sets the region preference order, closest to the human first.
    #[must_use]
    pub fn regions(mut self, regions: impl IntoIterator<Item = String>) -> Self {
        self.regions = regions.into_iter().collect();
        self
    }

    /// Requests a session lifetime in seconds.
    ///
    /// MCP servers sell one fixed block and ignore this.
    #[must_use]
    pub const fn ttl_seconds(mut self, ttl_seconds: u64) -> Self {
        self.ttl_seconds = Some(ttl_seconds);
        self
    }

    /// Uses a caller-chosen session identifier instead of a generated one.
    ///
    /// In MCP mode the session identifier is allocated by the server, so this
    /// value is used as the creation idempotency key instead.
    #[must_use]
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Overrides the control-plane or MCP request timeout.
    #[must_use]
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Describes what the session is for; MCP servers show this to the human.
    #[must_use]
    pub fn purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }

    /// Pins the MCP creation idempotency key.
    ///
    /// Reuse the same key when retrying an uncertain spawn: the server returns
    /// the same session instead of renting and charging for a second one.
    #[must_use]
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Requests a deployment-managed proxy exit country, as ISO 3166-1 alpha-2.
    #[must_use]
    pub fn proxy_country(mut self, country: impl Into<String>) -> Self {
        self.proxy_country = Some(country.into());
        self
    }

    /// Persists MCP OAuth credentials at `path` instead of the default file.
    #[must_use]
    pub fn credentials_path(mut self, path: impl Into<PathBuf>) -> Self {
        if let PopcornAccess::HostedMcp {
            credentials_path, ..
        } = &mut self.access
        {
            *credentials_path = Some(path.into());
        }
        self
    }

    /// Persists MCP OAuth credentials through a caller-owned store.
    ///
    /// Use this to share one credential store with the rest of an application;
    /// otherwise a file store is used.
    #[must_use]
    pub fn oauth_store(mut self, store: Arc<dyn McpOAuthStore>) -> Self {
        if let PopcornAccess::HostedMcp { oauth_store, .. } = &mut self.access {
            *oauth_store = Some(store);
        }
        self
    }

    fn bearer(&self) -> Result<String, PopcornError> {
        match &self.access {
            PopcornAccess::ControlPlane {
                client_id,
                client_secret,
                ..
            } => Ok(format!("Bearer {client_id}:{client_secret}")),
            PopcornAccess::HostedMcp { .. } => Err(PopcornError::Configuration {
                message: "MCP mode has no control-plane bearer token".to_owned(),
            }),
        }
    }

    fn control_plane(&self) -> Result<&Url, PopcornError> {
        match &self.access {
            PopcornAccess::ControlPlane { control_plane, .. } => Ok(control_plane),
            PopcornAccess::HostedMcp { .. } => Err(PopcornError::Configuration {
                message: "MCP mode has no control-plane URL".to_owned(),
            }),
        }
    }

    fn sessions_url(&self) -> Result<Url, PopcornError> {
        self.control_plane()?
            .join("v1/sessions")
            .map_err(|error| PopcornError::Configuration {
                message: format!("cannot build sessions URL: {error}"),
            })
    }

    fn session_url(&self, session_id: &str) -> Result<Url, PopcornError> {
        self.control_plane()?
            .join(&format!("v1/session/{session_id}"))
            .map_err(|error| PopcornError::Configuration {
                message: format!("cannot build session URL: {error}"),
            })
    }

    fn creation_purpose(&self) -> &str {
        self.purpose.as_deref().unwrap_or(DEFAULT_PURPOSE)
    }

    /// Returns the creation idempotency key, generating one when unset.
    fn creation_idempotency_key(&self) -> String {
        self.idempotency_key
            .clone()
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(generated_idempotency_key)
    }
}

/// Builds a key unique to one spawn attempt in this process.
fn generated_idempotency_key() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("nanocodex-{:x}-{nanos:x}-{sequence:x}", std::process::id())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "slice_is_empty")]
    regions: &'a [String],
}

const fn slice_is_empty(regions: &&[String]) -> bool {
    regions.is_empty()
}

/// The session record returned by Popcorn.
///
/// Every URL in this record is a bearer secret. Log the session id, never
/// the URLs.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PopcornSession {
    /// Popcorn session identifier.
    pub session_id: String,
    /// Human-facing LiveView page for watching or taking over the browser.
    pub url: Url,
    /// Restricted client-facing CDP endpoint.
    pub cdp_url: Url,
    /// Trusted full-access CDP endpoint used by the agent.
    ///
    /// MCP servers return exactly one agent-facing CDP URL, so in MCP mode this
    /// holds the same endpoint as [`PopcornSession::cdp_url`].
    pub cdp_internal_url: Url,
    /// Allocated browser pod identity.
    #[serde(default)]
    pub browser_pod_id: Option<String>,
    /// Session deadline when Popcorn set one.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Selected region.
    #[serde(default)]
    pub region: Option<String>,
    /// Selected cluster.
    #[serde(default)]
    pub cluster_name: Option<String>,
}

/// Redacts every session URL, each of which is a bearer secret.
impl std::fmt::Debug for PopcornSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopcornSession")
            .field("session_id", &self.session_id)
            .field("url", &"<redacted>")
            .field("cdp_url", &"<redacted>")
            .field("cdp_internal_url", &"<redacted>")
            .field("browser_pod_id", &self.browser_pod_id)
            .field("expires_at", &self.expires_at)
            .field("region", &self.region)
            .field("cluster_name", &self.cluster_name)
            .finish()
    }
}

#[derive(Deserialize)]
struct CreateSessionResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    session: Option<PopcornSession>,
}

/// A session assembled from one or more MCP tool results.
///
/// `create_browser_session` normally returns every field at once; the optional
/// fields are filled from `get_browser_connection` and `get_live_view` when a
/// deployment splits them across tools.
struct McpSessionDraft {
    session_id: String,
    cdp_url: Option<String>,
    live_view_url: Option<String>,
    region: Option<String>,
    expires_at: Option<String>,
}

impl McpSessionDraft {
    /// Reads a `create_browser_session` payload.
    fn from_create(payload: &Value) -> Result<Self, PopcornError> {
        let session_id =
            lookup_str(payload, SESSION_ID_KEYS).ok_or(PopcornError::MissingField {
                field: "session_id",
            })?;
        Ok(Self {
            session_id,
            cdp_url: lookup_str(payload, CDP_KEYS),
            live_view_url: lookup_str(payload, LIVE_VIEW_KEYS),
            region: lookup_str(payload, REGION_KEYS),
            expires_at: lookup_str(payload, EXPIRES_KEYS),
        })
    }

    /// Merges a `get_browser_connection` payload into the missing fields.
    fn merge_connection(&mut self, payload: &Value) {
        self.cdp_url = self
            .cdp_url
            .take()
            .or_else(|| lookup_str(payload, CDP_KEYS));
        self.live_view_url = self
            .live_view_url
            .take()
            .or_else(|| lookup_str(payload, LIVE_VIEW_KEYS));
        self.region = self
            .region
            .take()
            .or_else(|| lookup_str(payload, REGION_KEYS));
        self.expires_at = self
            .expires_at
            .take()
            .or_else(|| lookup_str(payload, EXPIRES_KEYS));
    }

    /// Merges a `get_live_view` payload, which may name the link `url`.
    fn merge_live_view(&mut self, payload: &Value) {
        self.live_view_url = self.live_view_url.take().or_else(|| {
            lookup_str(payload, LIVE_VIEW_KEYS).or_else(|| lookup_str(payload, &["url"]))
        });
    }

    /// Builds the session record, requiring a CDP endpoint and a LiveView page.
    fn finish(self) -> Result<PopcornSession, PopcornError> {
        let cdp_url = parse_session_url(
            self.cdp_url
                .ok_or(PopcornError::MissingField { field: "cdp_url" })?,
            "cdp_url",
        )?;
        let url = parse_session_url(
            self.live_view_url.ok_or(PopcornError::MissingField {
                field: "live_view_url",
            })?,
            "live_view_url",
        )?;
        Ok(PopcornSession {
            session_id: self.session_id,
            url,
            cdp_url: cdp_url.clone(),
            cdp_internal_url: cdp_url,
            browser_pod_id: None,
            expires_at: self.expires_at,
            region: self.region,
            cluster_name: None,
        })
    }
}

/// Parses a session URL without ever rendering it in the error.
fn parse_session_url(raw: String, field: &'static str) -> Result<Url, PopcornError> {
    Url::parse(&raw).map_err(|error| PopcornError::Mcp {
        message: format!("`{field}` is not a valid URL: {error}"),
    })
}

/// Finds the first string at any of `keys`, searching nested objects.
///
/// MCP deployments wrap results in envelopes such as `{"session": {...}}`, so
/// the search descends through objects and arrays rather than assuming a shape.
fn lookup_str(payload: &Value, keys: &[&str]) -> Option<String> {
    const MAX_DEPTH: usize = 4;
    fn walk(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
        if depth == 0 {
            return None;
        }
        match value {
            Value::Object(fields) => {
                for key in keys {
                    if let Some(found) = fields
                        .get(*key)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|found| !found.is_empty())
                    {
                        return Some(found.to_owned());
                    }
                }
                fields
                    .values()
                    .find_map(|nested| walk(nested, keys, depth - 1))
            }
            Value::Array(items) => items
                .iter()
                .find_map(|nested| walk(nested, keys, depth - 1)),
            _ => None,
        }
    }
    walk(payload, keys, MAX_DEPTH)
}

/// Describes the shortfall when a metered account has no session credit left.
///
/// This is diagnostic only. The authoritative out-of-credit signal is the
/// `create_browser_session` response, which carries the checkout link a human
/// needs; see [`call_mcp_tool`].
fn credit_shortfall(payload: &Value) -> Option<String> {
    let metered = payload
        .get("metered")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let credits = payload.get("credits").and_then(Value::as_f64)?;
    if !metered || credits > 0.0 {
        return None;
    }
    Some(format!(
        "{credits} credits remaining on a metered deployment"
    ))
}

/// Errors from renting, driving, or releasing a Popcorn session.
#[derive(Debug, thiserror::Error)]
pub enum PopcornError {
    /// The configuration is incomplete or malformed.
    #[error("popcorn configuration error: {message}")]
    Configuration {
        /// Human-readable explanation.
        message: String,
    },
    /// The control plane could not be reached.
    #[error("popcorn control plane request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// The control plane rejected the request.
    #[error("popcorn control plane returned {status}: {body}")]
    ControlPlane {
        /// HTTP status.
        status: StatusCode,
        /// Response body, truncated.
        body: String,
    },
    /// The MCP server could not be reached, authorized, or understood.
    #[error("popcorn MCP error: {message}")]
    Mcp {
        /// Human-readable explanation.
        message: String,
    },
    /// A Popcorn response did not carry a field the session needs.
    #[error("popcorn response is missing `{field}`")]
    MissingField {
        /// Missing field name.
        field: &'static str,
    },
    /// The account has no session credit left.
    #[error("popcorn has no session credit left: {message}")]
    InsufficientCredit {
        /// What the human should do next.
        message: String,
    },
    /// The browser controller could not be configured for the session.
    #[error(transparent)]
    Build(#[from] BrowserBuildError),
    /// A browser action failed.
    #[error(transparent)]
    Browser(#[from] BrowserError),
}

/// How a rented session is reached for its remaining lifecycle calls.
enum PopcornControl {
    /// An MCP server, already connected and authorized.
    Mcp(McpHandle),
    /// A control plane, reached over HTTP with client credentials.
    ControlPlane(Client),
}

/// One rented Popcorn session wrapped as a Nanocodex browser Hand.
///
/// Dropping the value without calling [`PopcornBrowser::shutdown`] leaves the
/// session to Popcorn's TTL controller. Call `shutdown` to release it now.
pub struct PopcornBrowser {
    config: PopcornConfig,
    control: PopcornControl,
    session: PopcornSession,
    browser: Browser,
}

impl PopcornBrowser {
    /// Rents a Popcorn session and attaches the browser controller to it.
    ///
    /// In MCP mode the first call prints an OAuth authorization URL to stderr
    /// and waits for the operator to approve it in a browser.
    ///
    /// # Errors
    ///
    /// Returns an error when Popcorn refuses the session, the account is out of
    /// credit, or the browser controller cannot be configured for the remote
    /// CDP endpoint.
    pub async fn spawn(config: PopcornConfig) -> Result<Self, PopcornError> {
        Self::spawn_with(config, Browser::builder()).await
    }

    /// Rents a Popcorn session using caller-provided browser settings.
    ///
    /// The builder's `cdp_endpoint` is replaced with the rented session's
    /// endpoint; other settings compatible with a remote CDP boundary are kept.
    ///
    /// # Errors
    ///
    /// Same as [`PopcornBrowser::spawn`].
    pub async fn spawn_with(
        config: PopcornConfig,
        browser: BrowserBuilder,
    ) -> Result<Self, PopcornError> {
        nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
        let (control, session) = match &config.access {
            PopcornAccess::HostedMcp { .. } => {
                let handle = connect_mcp(&config).await?;
                let session = create_mcp_session(&handle, &config).await?;
                (PopcornControl::Mcp(handle), session)
            }
            PopcornAccess::ControlPlane { .. } => {
                let client = Client::builder().timeout(config.request_timeout).build()?;
                let session = create_session(&client, &config).await?;
                (PopcornControl::ControlPlane(client), session)
            }
        };
        debug!(
            target: "nanocodex_browser",
            session = %session.session_id,
            region = ?session.region,
            mcp = config.uses_mcp(),
            "rented popcorn session"
        );
        // The session is already rented, so release it rather than leaking it to
        // the TTL controller when the controller cannot be attached.
        let browser = match browser
            .cdp_endpoint(session.cdp_internal_url.clone())
            .build()
        {
            Ok(browser) => browser,
            Err(error) => {
                if let Err(release_error) =
                    release_session(&control, &config, &session.session_id).await
                {
                    warn!(
                        target: "nanocodex_browser",
                        session = %session.session_id,
                        error = %release_error,
                        "popcorn session release failed after a failed attach; \
                         TTL controller will reclaim it"
                    );
                }
                return Err(error.into());
            }
        };
        Ok(Self {
            config,
            control,
            session,
            browser,
        })
    }

    /// Returns the browser controller driving the remote session.
    #[must_use]
    pub const fn browser(&self) -> &Browser {
        &self.browser
    }

    /// Wraps the remote session as an ordinary Nanocodex browser tool.
    #[must_use]
    pub fn tool(&self) -> BrowserTool {
        BrowserTool::from_browser(self.browser.clone())
    }

    /// Returns the session record. Treat every URL in it as a secret.
    #[must_use]
    pub const fn session(&self) -> &PopcornSession {
        &self.session
    }

    /// Returns the LiveView page a human can open to watch or take over.
    #[must_use]
    pub const fn live_view_url(&self) -> &Url {
        &self.session.url
    }

    /// Closes the controller and releases the session.
    ///
    /// # Errors
    ///
    /// Returns the first error from closing the browser or releasing the
    /// session. The release is attempted even when the close fails.
    pub async fn shutdown(self) -> Result<(), PopcornError> {
        let close_result = self.browser.close().await.map_err(PopcornError::from);
        let release_result =
            release_session(&self.control, &self.config, &self.session.session_id).await;
        if let Err(error) = &release_result {
            warn!(
                target: "nanocodex_browser",
                session = %self.session.session_id,
                %error,
                "popcorn session release failed; TTL controller will reclaim it"
            );
        }
        close_result.and(release_result)
    }
}

/// Connects to the MCP server, running a browser OAuth login when needed.
async fn connect_mcp(config: &PopcornConfig) -> Result<McpHandle, PopcornError> {
    let PopcornAccess::HostedMcp {
        server_url,
        oauth_store,
        credentials_path,
    } = &config.access
    else {
        return Err(PopcornError::Configuration {
            message: "MCP connection requires an MCP configuration".to_owned(),
        });
    };
    let store = match oauth_store {
        Some(store) => Arc::clone(store),
        None => Arc::new(PopcornCredentialFile::new(default_credentials_path(
            credentials_path.clone(),
        )?)) as Arc<dyn McpOAuthStore>,
    };
    let provider = Mcp::builder()
        .oauth_store(store)
        .server(
            MCP_SERVER_NAME,
            McpServer::http(server_url.as_str())
                .description("Popcorn remote browser sessions")
                .startup_timeout(config.request_timeout)
                .tool_timeout(config.request_timeout),
        )
        .build()
        .map_err(|error| PopcornError::Mcp {
            message: error.to_string(),
        })?;
    let handle = provider.handle();
    // A first run has no stored credentials, so the unauthenticated connect
    // fails and the login below both authorizes and reloads the server.
    if let Err(connect_error) = handle.reload(MCP_SERVER_NAME).await {
        let login = handle
            .login(MCP_SERVER_NAME)
            .await
            .map_err(|error| PopcornError::Mcp {
                message: format!("{connect_error}; OAuth login could not start: {error}"),
            })?;
        eprintln!(
            "popcorn: authorize this client by opening\n  {}",
            login.authorization_url()
        );
        eprintln!("popcorn: waiting for the browser callback...");
        login.wait().await.map_err(|error| PopcornError::Mcp {
            message: format!("OAuth login failed: {error}"),
        })?;
        eprintln!("popcorn: authorized");
    }
    Ok(handle)
}

/// Rents one session through the MCP server.
async fn create_mcp_session(
    handle: &McpHandle,
    config: &PopcornConfig,
) -> Result<PopcornSession, PopcornError> {
    // The balance is free to read and worth logging, but it must not short
    // circuit creation: only `create_browser_session` returns the human
    // checkout link for buying credits, so an out-of-credit run has to reach it.
    if let Ok(balance) = call_mcp_tool(handle, BALANCE_TOOL, Map::new()).await
        && let Ok(payload) = balance.json()
        && let Some(shortfall) = credit_shortfall(&payload)
    {
        debug!(
            target: "nanocodex_browser",
            shortfall,
            "popcorn reports no session credit; asking the server for a checkout link"
        );
    }

    let mut arguments = Map::new();
    arguments.insert(
        "purpose".to_owned(),
        Value::String(config.creation_purpose().to_owned()),
    );
    arguments.insert(
        "idempotency_key".to_owned(),
        Value::String(config.creation_idempotency_key()),
    );
    if !config.regions.is_empty() {
        arguments.insert(
            "regions".to_owned(),
            Value::Array(
                config
                    .regions
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            ),
        );
    }
    if let Some(country) = &config.proxy_country {
        arguments.insert("proxy_country".to_owned(), Value::String(country.clone()));
    }

    let created = call_mcp_tool(handle, CREATE_SESSION_TOOL, arguments).await?;
    let mut draft = McpSessionDraft::from_create(&created.json().map_err(map_mcp_error)?)?;
    if draft.cdp_url.is_none() {
        let mut arguments = Map::new();
        arguments.insert(
            "session_id".to_owned(),
            Value::String(draft.session_id.clone()),
        );
        let connection = call_mcp_tool(handle, CONNECTION_TOOL, arguments).await?;
        draft.merge_connection(&connection.json().map_err(map_mcp_error)?);
    }
    if draft.live_view_url.is_none() {
        let mut arguments = Map::new();
        arguments.insert(
            "session_id".to_owned(),
            Value::String(draft.session_id.clone()),
        );
        let live_view = call_mcp_tool(handle, LIVE_VIEW_TOOL, arguments).await?;
        draft.merge_live_view(&live_view.json().map_err(map_mcp_error)?);
    }
    draft.finish()
}

/// Calls one Popcorn tool, turning a tool-level error into a typed error.
async fn call_mcp_tool(
    handle: &McpHandle,
    tool: &str,
    arguments: Map<String, Value>,
) -> Result<McpToolCall, PopcornError> {
    let call = handle
        .call_tool(MCP_SERVER_NAME, tool, arguments)
        .await
        .map_err(|error| PopcornError::Mcp {
            message: error.to_string(),
        })?;
    if !call.is_error() {
        return Ok(call);
    }
    let detail = call.text().trim().to_owned();
    let payload = call.json().ok();
    let checkout = payload.as_ref().and_then(checkout_link);
    // An out-of-credit creation answers with a checkout link for the human;
    // hand it over unchanged and stop rather than retrying.
    if let Some(checkout) = checkout {
        return Err(PopcornError::InsufficientCredit {
            message: format!(
                "open {checkout} to buy credits, then retry with the same idempotency key"
            ),
        });
    }
    if payload.as_ref().is_some_and(payload_is_out_of_credit) {
        // The server sent no `checkout_url` field, so pass its own wording
        // through: deployments put the payment link in the message text.
        return Err(PopcornError::InsufficientCredit {
            message: if detail.is_empty() {
                format!("buy credits at {CREDIT_CHECKOUT_URL}")
            } else {
                truncate(&detail)
            },
        });
    }
    Err(PopcornError::Mcp {
        message: format!("`{tool}` failed: {}", truncate(&detail)),
    })
}

/// Finds the human approval link in an out-of-credit refusal.
///
/// The hosted server answers with a `next_action` of type `external_approval`
/// holding the link; other deployments may name it `checkout_url`. The link is
/// read only from those positions, never from a generic `url` field, so a
/// session's LiveView URL can never be mistaken for a payment link.
fn checkout_link(payload: &Value) -> Option<String> {
    for key in NEXT_ACTION_KEYS {
        if let Some(action) = payload.get(*key)
            && let Some(url) = lookup_str(action, &["url"])
        {
            return Some(url);
        }
    }
    lookup_str(payload, CHECKOUT_KEYS)
}

/// Detects an out-of-credit refusal that carried no checkout link.
fn payload_is_out_of_credit(payload: &Value) -> bool {
    payload
        .get("error")
        .or_else(|| payload.get("code"))
        .and_then(Value::as_str)
        .is_some_and(|error| {
            let error = error.to_ascii_lowercase();
            error.contains("credit") || error.contains("payment")
        })
}

const fn map_mcp_error(message: String) -> PopcornError {
    PopcornError::Mcp { message }
}

/// Releases a rented session through whichever path rented it.
async fn release_session(
    control: &PopcornControl,
    config: &PopcornConfig,
    session_id: &str,
) -> Result<(), PopcornError> {
    match control {
        PopcornControl::Mcp(handle) => {
            let mut arguments = Map::new();
            arguments.insert(
                "session_id".to_owned(),
                Value::String(session_id.to_owned()),
            );
            call_mcp_tool(handle, END_SESSION_TOOL, arguments).await?;
            Ok(())
        }
        PopcornControl::ControlPlane(client) => delete_session(client, config, session_id).await,
    }
}

async fn create_session(
    client: &Client,
    config: &PopcornConfig,
) -> Result<PopcornSession, PopcornError> {
    let request = CreateSessionRequest {
        session_id: config.session_id.as_deref(),
        ttl_seconds: config.ttl_seconds,
        regions: &config.regions,
    };
    let response = client
        .post(config.sessions_url()?)
        .header(reqwest::header::AUTHORIZATION, config.bearer()?)
        .json(&request)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(PopcornError::ControlPlane {
            status,
            body: truncate(&body),
        });
    }
    let parsed: CreateSessionResponse =
        serde_json::from_str(&body).map_err(|error| PopcornError::ControlPlane {
            status,
            body: format!("unparseable session response: {error}"),
        })?;
    if parsed.success == Some(false) {
        return Err(PopcornError::ControlPlane {
            status,
            body: parsed
                .error
                .unwrap_or_else(|| "session creation failed".to_owned()),
        });
    }
    parsed.session.ok_or_else(|| PopcornError::ControlPlane {
        status,
        body: "session response is missing cdpInternalUrl".to_owned(),
    })
}

async fn delete_session(
    client: &Client,
    config: &PopcornConfig,
    session_id: &str,
) -> Result<(), PopcornError> {
    let response = client
        .delete(config.session_url(session_id)?)
        .header(reqwest::header::AUTHORIZATION, config.bearer()?)
        .send()
        .await?;
    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_FOUND {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(PopcornError::ControlPlane {
        status,
        body: truncate(&body),
    })
}

fn truncate(body: &str) -> String {
    const LIMIT: usize = 512;
    if body.len() <= LIMIT {
        body.to_owned()
    } else {
        let mut end = LIMIT;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &body[..end])
    }
}

/// Returns the credential file path, defaulting under the user's home.
fn default_credentials_path(configured: Option<PathBuf>) -> Result<PathBuf, PopcornError> {
    if let Some(path) = configured {
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| PopcornError::Configuration {
            message: "`HOME` is not set; set `POPCORN_MCP_CREDENTIALS` to a writable path"
                .to_owned(),
        })?;
    Ok(home.join(".nanocodex").join("popcorn-mcp-oauth.json"))
}

/// A file-backed [`McpOAuthStore`] so a second run needs no login.
///
/// The file holds one entry per server URL and is created with owner-only
/// permissions. An exclusive lock serialises readers, writers, and token
/// refreshes across processes sharing the same file.
struct PopcornCredentialFile {
    path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredCredentialFile {
    #[serde(default)]
    servers: std::collections::BTreeMap<String, StoredCredential>,
}

#[derive(Serialize, Deserialize)]
struct StoredCredential {
    client_id: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<String>,
}

/// An exclusive lock on the credential file, held until dropped.
struct CredentialFileLock {
    _file: std::fs::File,
}

impl PopcornCredentialFile {
    const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn lock_path(&self) -> PathBuf {
        let mut path = self.path.clone();
        let name = path
            .file_name()
            .map(|name| format!("{}.lock", name.to_string_lossy()))
            .unwrap_or_else(|| "popcorn-mcp-oauth.lock".to_owned());
        path.set_file_name(name);
        path
    }

    fn acquire_lock(&self) -> Result<CredentialFileLock, String> {
        let path = self.lock_path();
        create_parent(&path)?;
        let file = open_private(&path)?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(CredentialFileLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock)
                    if started.elapsed() >= Duration::from_secs(60) =>
                {
                    return Err(format!(
                        "timed out waiting for the Popcorn credential lock {}",
                        path.display()
                    ));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(format!(
                        "failed to lock the Popcorn credential file {}: {error}",
                        path.display()
                    ));
                }
            }
        }
    }

    fn read_file(&self) -> Result<StoredCredentialFile, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) if contents.trim().is_empty() => Ok(StoredCredentialFile::default()),
            Ok(contents) => serde_json::from_str(&contents).map_err(|error| {
                format!(
                    "failed to parse the Popcorn credential file {}: {error}",
                    self.path.display()
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(StoredCredentialFile::default())
            }
            Err(error) => Err(format!(
                "failed to read the Popcorn credential file {}: {error}",
                self.path.display()
            )),
        }
    }

    fn write_file(&self, file: &StoredCredentialFile) -> Result<(), String> {
        create_parent(&self.path)?;
        let encoded = serde_json::to_vec_pretty(file)
            .map_err(|error| format!("failed to encode Popcorn credentials: {error}"))?;
        // Create the file with owner-only permissions before writing a token to it.
        drop(open_private(&self.path)?);
        std::fs::write(&self.path, encoded).map_err(|error| {
            format!(
                "failed to write the Popcorn credential file {}: {error}",
                self.path.display()
            )
        })
    }

    fn load_blocking(&self, server_url: &str) -> Result<Option<McpOAuthCredentials>, String> {
        let _lock = self.acquire_lock()?;
        let Some(entry) = self.read_file()?.servers.remove(server_url) else {
            return Ok(None);
        };
        if entry.client_id.trim().is_empty() || entry.access_token.trim().is_empty() {
            return Err("stored Popcorn OAuth credentials are incomplete".to_owned());
        }
        let mut credentials =
            McpOAuthCredentials::new(entry.client_id, entry.access_token).scopes(entry.scopes);
        if let Some(refresh_token) = entry.refresh_token.filter(|token| !token.trim().is_empty()) {
            credentials = credentials.refresh_token(refresh_token);
        }
        if let Some(issuer) = entry.issuer.filter(|issuer| !issuer.trim().is_empty()) {
            credentials = credentials.issuer(issuer);
        }
        if let Some(expires_at) = entry.expires_at {
            credentials = credentials.expires_at_millis(expires_at);
        }
        Ok(Some(credentials))
    }

    fn save_blocking(
        &self,
        server_url: &str,
        credentials: &McpOAuthCredentials,
    ) -> Result<(), String> {
        let _lock = self.acquire_lock()?;
        let mut file = self.read_file()?;
        file.servers.insert(
            server_url.to_owned(),
            StoredCredential {
                client_id: credentials.client_id().to_owned(),
                access_token: credentials.access_token().to_owned(),
                refresh_token: credentials.refresh_token_value().map(ToOwned::to_owned),
                issuer: credentials.authorization_issuer().map(ToOwned::to_owned),
                expires_at: credentials.expires_at(),
                scopes: credentials.granted_scopes().to_vec(),
            },
        );
        self.write_file(&file)
    }
}

fn create_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))
}

/// Opens or creates a file readable and writable only by its owner.
fn open_private(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))
}

#[async_trait]
impl McpOAuthStore for PopcornCredentialFile {
    async fn load(
        &self,
        _server_name: &str,
        server_url: &str,
    ) -> Result<Option<McpOAuthCredentials>, String> {
        let store = Self::new(self.path.clone());
        let server_url = server_url.to_owned();
        tokio::task::spawn_blocking(move || store.load_blocking(&server_url))
            .await
            .map_err(|error| format!("Popcorn credential reader stopped: {error}"))?
    }

    async fn save(
        &self,
        _server_name: &str,
        server_url: &str,
        credentials: &McpOAuthCredentials,
    ) -> Result<(), String> {
        let store = Self::new(self.path.clone());
        let server_url = server_url.to_owned();
        let credentials = credentials.clone();
        tokio::task::spawn_blocking(move || store.save_blocking(&server_url, &credentials))
            .await
            .map_err(|error| format!("Popcorn credential writer stopped: {error}"))?
    }

    async fn acquire_refresh_lock(
        &self,
        _server_name: &str,
        _server_url: &str,
    ) -> Result<Box<dyn McpOAuthRefreshGuard>, String> {
        let store = Self::new(self.path.clone());
        tokio::task::spawn_blocking(move || {
            store
                .acquire_lock()
                .map(|lock| Box::new(lock) as Box<dyn McpOAuthRefreshGuard>)
        })
        .await
        .map_err(|error| format!("Popcorn credential lock task stopped: {error}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<std::collections::BTreeMap<_, _>>();
        move |name: &str| pairs.get(name).cloned()
    }

    #[test]
    fn create_request_omits_empty_fields() {
        let request = CreateSessionRequest {
            session_id: None,
            ttl_seconds: None,
            regions: &[],
        };
        assert_eq!(serde_json::to_string(&request).unwrap(), "{}");
    }

    #[test]
    fn create_request_serialises_camel_case() {
        let regions = vec!["asia-south1".to_owned()];
        let request = CreateSessionRequest {
            session_id: Some("demo"),
            ttl_seconds: Some(600),
            regions: &regions,
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"sessionId":"demo","ttlSeconds":600,"regions":["asia-south1"]}"#
        );
    }

    #[test]
    fn session_response_parses_reference_shape() {
        let body = r#"{
            "success": true,
            "sessionId": "demo-session",
            "url": "https://browser.example.com/liveview/demo-session/t/liveview.html",
            "cdpUrl": "wss://browser.example.com/cdp/demo-session/t/",
            "cdpInternalUrl": "wss://browser.example.com/cdp-internal/demo-session/t/",
            "apiUrl": "https://browser.example.com/api/demo-session/t/",
            "browserPodId": "browser-fleet-abc",
            "expiresAt": "2026-08-04T12:30:00.000Z",
            "region": "us-central1",
            "clusterName": "popcorn-prod-us"
        }"#;
        let parsed: CreateSessionResponse = serde_json::from_str(body).unwrap();
        let session = parsed.session.unwrap();
        assert_eq!(session.session_id, "demo-session");
        assert_eq!(session.cdp_internal_url.scheme(), "wss");
        assert_eq!(session.region.as_deref(), Some("us-central1"));
    }

    #[test]
    fn debug_output_redacts_session_urls() {
        let body = r#"{
            "sessionId": "demo-session",
            "url": "https://browser.example.com/liveview/demo-session/tok/liveview.html",
            "cdpUrl": "wss://browser.example.com/cdp/demo-session/tok/",
            "cdpInternalUrl": "wss://browser.example.com/cdp-internal/demo-session/tok/"
        }"#;
        let session: PopcornSession = serde_json::from_str(body).unwrap();
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("tok"), "session URLs leaked: {rendered}");
        assert!(!rendered.contains("browser.example.com"));
        assert!(rendered.contains("demo-session"));
    }

    #[test]
    fn debug_output_redacts_client_secret() {
        let config = PopcornConfig::new(
            Url::parse("https://control.example.com/").unwrap(),
            "client-id",
            "super-secret",
        );
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("super-secret"),
            "secret leaked: {rendered}"
        );
        assert!(rendered.contains("client-id"));
    }

    #[test]
    fn failed_response_surfaces_error_message() {
        let body = r#"{"success": false, "error": "no capacity in region"}"#;
        let parsed: CreateSessionResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.success, Some(false));
        assert_eq!(parsed.error.as_deref(), Some("no capacity in region"));
        assert!(parsed.session.is_none());
    }

    #[test]
    fn config_builds_control_plane_urls() {
        let config = PopcornConfig::new(
            Url::parse("https://control.example.com/").unwrap(),
            "id",
            "secret",
        );
        assert_eq!(
            config.sessions_url().unwrap().as_str(),
            "https://control.example.com/v1/sessions"
        );
        assert_eq!(
            config.session_url("abc").unwrap().as_str(),
            "https://control.example.com/v1/session/abc"
        );
        assert_eq!(config.bearer().unwrap(), "Bearer id:secret");
    }

    #[test]
    fn create_session_result_maps_onto_a_session() {
        // The shape returned by the hosted MCP server: flat snake_case JSON.
        let payload = json!({
            "session_id": "pop-123",
            "live_view_url": "https://browser.example.com/liveview/pop-123/tok/",
            "cdp_url": "wss://browser.example.com/cdp/pop-123/tok/",
            "region": "asia-south1",
            "expires_at": "2026-09-11T09:00:00.000Z",
            "block_seconds": 600
        });
        let session = McpSessionDraft::from_create(&payload)
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(session.session_id, "pop-123");
        assert_eq!(session.url.scheme(), "https");
        assert_eq!(session.cdp_url.scheme(), "wss");
        // MCP servers return one agent-facing CDP URL for both fields.
        assert_eq!(session.cdp_internal_url, session.cdp_url);
        assert_eq!(session.region.as_deref(), Some("asia-south1"));
        assert_eq!(
            session.expires_at.as_deref(),
            Some("2026-09-11T09:00:00.000Z")
        );
        assert!(session.browser_pod_id.is_none());
    }

    #[test]
    fn create_session_result_accepts_a_camel_case_envelope() {
        let payload = json!({
            "ok": true,
            "session": {
                "sessionId": "pop-456",
                "liveViewUrl": "https://browser.example.com/liveview/pop-456/tok/",
                "connectUrl": "wss://browser.example.com/cdp/pop-456/tok/",
                "expiresAt": "2026-09-11T09:10:00.000Z"
            }
        });
        let session = McpSessionDraft::from_create(&payload)
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(session.session_id, "pop-456");
        assert_eq!(session.cdp_url.scheme(), "wss");
        assert!(session.region.is_none());
    }

    #[test]
    fn connection_and_live_view_results_fill_a_partial_session() {
        let mut draft = McpSessionDraft::from_create(&json!({ "session_id": "pop-789" })).unwrap();
        assert!(draft.cdp_url.is_none());
        assert!(draft.live_view_url.is_none());

        draft.merge_connection(&json!({
            "cdp_url": "wss://browser.example.com/cdp/pop-789/tok/",
            "region": "us-central1",
            "expires_at": "2026-09-11T09:20:00.000Z"
        }));
        // `get_live_view` names the link `url`, unlike the creation result.
        draft.merge_live_view(&json!({
            "url": "https://browser.example.com/liveview/pop-789/tok/"
        }));

        let session = draft.finish().unwrap();
        assert_eq!(session.session_id, "pop-789");
        assert_eq!(session.region.as_deref(), Some("us-central1"));
        assert_eq!(session.url.path(), "/liveview/pop-789/tok/");
    }

    #[test]
    fn create_session_result_without_a_session_id_is_an_error() {
        let Err(error) = McpSessionDraft::from_create(&json!({ "cdp_url": "wss://example.com/" }))
        else {
            panic!("a session id is required");
        };
        assert!(
            matches!(
                error,
                PopcornError::MissingField {
                    field: "session_id"
                }
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn session_without_a_cdp_url_is_an_error() {
        let draft = McpSessionDraft::from_create(&json!({
            "session_id": "pop-1",
            "live_view_url": "https://example.com/live"
        }))
        .unwrap();
        let error = draft.finish().expect_err("a CDP URL is required");
        assert!(
            matches!(error, PopcornError::MissingField { field: "cdp_url" }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn session_without_a_live_view_url_is_an_error() {
        let draft = McpSessionDraft::from_create(&json!({
            "session_id": "pop-1",
            "cdp_url": "wss://example.com/cdp"
        }))
        .unwrap();
        let error = draft.finish().expect_err("a LiveView URL is required");
        assert!(
            matches!(
                error,
                PopcornError::MissingField {
                    field: "live_view_url"
                }
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_metered_account_without_credit_reports_a_shortfall() {
        // The shape returned by the hosted server's `get_balance`.
        let empty = json!({
            "credits": 0,
            "metered": true,
            "session_block_seconds": 600,
            "credits_per_operation": 1
        });
        assert!(credit_shortfall(&empty).is_some(), "no credit left");

        let funded = json!({ "credits": 12, "metered": true });
        assert!(credit_shortfall(&funded).is_none());
        // An unmetered deployment reports no shortfall whatever the balance is.
        let unmetered = json!({ "credits": 0, "metered": false });
        assert!(credit_shortfall(&unmetered).is_none());
        let unknown = json!({ "credits": null, "metered": true });
        assert!(credit_shortfall(&unknown).is_none());
    }

    #[test]
    fn an_out_of_credit_refusal_is_recognised_by_its_error_field() {
        assert!(payload_is_out_of_credit(
            &json!({ "error": "INSUFFICIENT_CREDIT" })
        ));
        assert!(payload_is_out_of_credit(
            &json!({ "code": "payment_required" })
        ));
        assert!(!payload_is_out_of_credit(
            &json!({ "error": "no capacity" })
        ));
        assert!(!payload_is_out_of_credit(&json!({ "session_id": "pop-1" })));
    }

    #[test]
    fn the_hosted_servers_refusal_yields_its_approval_link() {
        // Verbatim shape returned by the hosted server when credit runs out.
        let refusal = json!({
            "error": "insufficient_credit",
            "message": "Not enough usage credit for this operation.",
            "next_action": {
                "type": "external_approval",
                "url": "https://popcorn-billing-gcp.reclaimprotocol.org/checkout?token=opaque"
            },
            "next": "Give the human next_action to obtain more credit, then retry with the same idempotency_key."
        });
        assert!(payload_is_out_of_credit(&refusal));
        assert_eq!(
            checkout_link(&refusal).as_deref(),
            Some("https://popcorn-billing-gcp.reclaimprotocol.org/checkout?token=opaque")
        );
    }

    #[test]
    fn a_live_view_url_is_never_read_as_a_payment_link() {
        // A successful creation has a `url`, which must not look like checkout.
        let created = json!({
            "session_id": "pop-1",
            "url": "https://browser.example.com/liveview/pop-1/tok/",
            "cdp_url": "wss://browser.example.com/cdp/pop-1/tok/"
        });
        assert!(checkout_link(&created).is_none());
        assert!(!payload_is_out_of_credit(&created));
    }

    #[test]
    fn the_servers_checkout_link_is_found_in_a_refusal() {
        // Only the creation response carries the link a human can pay at, so it
        // must survive whatever envelope the server wraps it in.
        let flat = json!({
            "error": "insufficient_credit",
            "checkout_url": "https://checkout.example.com/session/abc"
        });
        assert_eq!(
            lookup_str(&flat, CHECKOUT_KEYS).as_deref(),
            Some("https://checkout.example.com/session/abc")
        );
        let nested = json!({
            "error": { "code": "insufficient_credit" },
            "billing": { "checkoutUrl": "https://checkout.example.com/session/xyz" }
        });
        assert_eq!(
            lookup_str(&nested, CHECKOUT_KEYS).as_deref(),
            Some("https://checkout.example.com/session/xyz")
        );
    }

    #[test]
    fn from_env_defaults_to_the_hosted_mcp_server() {
        let config = PopcornConfig::from_lookup(&lookup(&[])).unwrap();
        assert!(config.uses_mcp());
        let PopcornAccess::HostedMcp { server_url, .. } = &config.access else {
            panic!("expected MCP mode");
        };
        assert_eq!(server_url.as_str(), HOSTED_MCP_URL);
    }

    #[test]
    fn from_env_overrides_the_mcp_server_url() {
        let config = PopcornConfig::from_lookup(&lookup(&[(
            "POPCORN_MCP_URL",
            "https://popcorn.internal.example.com/mcp",
        )]))
        .unwrap();
        let PopcornAccess::HostedMcp { server_url, .. } = &config.access else {
            panic!("expected MCP mode");
        };
        assert_eq!(
            server_url.as_str(),
            "https://popcorn.internal.example.com/mcp"
        );
    }

    #[test]
    fn from_env_selects_credentialed_mode_when_all_three_are_set() {
        let config = PopcornConfig::from_lookup(&lookup(&[
            ("POPCORN_CONTROL_PLANE_URL", "https://control.example.com/"),
            ("POPCORN_CLIENT_ID", "id"),
            ("POPCORN_CLIENT_SECRET", "secret"),
            ("POPCORN_REGION", "us-central1, asia-south1"),
            ("POPCORN_TTL_SECONDS", "900"),
        ]))
        .unwrap();
        assert!(!config.uses_mcp());
        assert_eq!(config.bearer().unwrap(), "Bearer id:secret");
        assert_eq!(config.regions, ["us-central1", "asia-south1"]);
        assert_eq!(config.ttl_seconds, Some(900));
    }

    #[test]
    fn from_env_rejects_a_partial_credentialed_configuration() {
        let error = PopcornConfig::from_lookup(&lookup(&[
            ("POPCORN_CONTROL_PLANE_URL", "https://control.example.com/"),
            ("POPCORN_CLIENT_ID", "id"),
        ]))
        .expect_err("a partial credentialed configuration is an error");
        let message = error.to_string();
        assert!(message.contains("POPCORN_CLIENT_SECRET"), "{message}");
    }

    #[test]
    fn from_env_reads_the_mcp_session_options() {
        let config = PopcornConfig::from_lookup(&lookup(&[
            ("POPCORN_PURPOSE", "book a flight"),
            ("POPCORN_IDEMPOTENCY_KEY", "run-42"),
            ("POPCORN_PROXY_COUNTRY", "IN"),
            ("POPCORN_MCP_CREDENTIALS", "/tmp/popcorn.json"),
        ]))
        .unwrap();
        assert_eq!(config.creation_purpose(), "book a flight");
        assert_eq!(config.creation_idempotency_key(), "run-42");
        assert_eq!(config.proxy_country.as_deref(), Some("IN"));
        let PopcornAccess::HostedMcp {
            credentials_path, ..
        } = &config.access
        else {
            panic!("expected MCP mode");
        };
        assert_eq!(
            credentials_path.as_deref(),
            Some(Path::new("/tmp/popcorn.json"))
        );
    }

    #[test]
    fn a_generated_idempotency_key_is_unique_per_attempt() {
        let config = PopcornConfig::hosted_mcp();
        assert_eq!(config.creation_purpose(), DEFAULT_PURPOSE);
        assert_ne!(
            config.creation_idempotency_key(),
            config.creation_idempotency_key()
        );
        // A caller-chosen session id pins the key so a retry cannot pay twice.
        let pinned = PopcornConfig::hosted_mcp().session_id("run-7");
        assert_eq!(pinned.creation_idempotency_key(), "run-7");
    }

    #[test]
    fn mcp_config_debug_names_the_mode_without_a_secret() {
        let rendered = format!("{:?}", PopcornConfig::hosted_mcp());
        assert!(rendered.contains("hosted_mcp"), "{rendered}");
        assert!(!rendered.contains("client_secret"), "{rendered}");
    }

    #[tokio::test]
    async fn the_credential_file_round_trips_and_stays_private() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("oauth.json");
        let store = PopcornCredentialFile::new(path.clone());
        assert!(
            store
                .load(MCP_SERVER_NAME, HOSTED_MCP_URL)
                .await
                .unwrap()
                .is_none()
        );

        let credentials = McpOAuthCredentials::new("client-1", "access-1")
            .refresh_token("refresh-1")
            .issuer("https://issuer.example.com")
            .expires_at_millis(1_800_000_000_000)
            .scopes(["popcorn.sessions", "popcorn.credit"]);
        store
            .save(MCP_SERVER_NAME, HOSTED_MCP_URL, &credentials)
            .await
            .unwrap();

        let loaded = store
            .load(MCP_SERVER_NAME, HOSTED_MCP_URL)
            .await
            .unwrap()
            .expect("stored credentials");
        assert_eq!(loaded.client_id(), "client-1");
        assert_eq!(loaded.access_token(), "access-1");
        assert_eq!(loaded.refresh_token_value(), Some("refresh-1"));
        assert_eq!(loaded.expires_at(), Some(1_800_000_000_000));
        assert_eq!(
            loaded.granted_scopes(),
            ["popcorn.sessions", "popcorn.credit"]
        );
        // Another server URL must not see these credentials.
        assert!(
            store
                .load(MCP_SERVER_NAME, "https://other.example.com/mcp")
                .await
                .unwrap()
                .is_none()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "credential file is not owner-only");
        }
    }

    #[test]
    fn default_credentials_path_prefers_the_configured_path() {
        let path = default_credentials_path(Some(PathBuf::from("/tmp/popcorn.json"))).unwrap();
        assert_eq!(path, Path::new("/tmp/popcorn.json"));
    }
}
