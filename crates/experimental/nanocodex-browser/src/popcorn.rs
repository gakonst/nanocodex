//! A Nanocodex browser Hand backed by a rented Popcorn session.
//!
//! [Popcorn](https://github.com/reclaimprotocol/popcorn-oss) rents isolated
//! headful Chromium sessions that run inside a TEE. Each session exposes a
//! full-access CDP WebSocket, a human-viewable LiveView page, and an
//! attestation route. This module rents one session from a Popcorn control
//! plane, points the ordinary [`Browser`] controller at its CDP endpoint, and
//! releases the session on shutdown.
//!
//! From the agent's point of view a Popcorn browser is just another Hand: the
//! brain stays wherever it is, and `tools.browser(...)` executes inside the
//! remote enclave. Because the model never receives the CDP URL, the session
//! token never enters the conversation.
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
//! Required environment for [`PopcornConfig::from_env`]:
//!
//! - `POPCORN_CONTROL_PLANE_URL`: base URL of the Popcorn control plane.
//! - `POPCORN_CLIENT_ID` and `POPCORN_CLIENT_SECRET`: credentialed client.
//! - `POPCORN_REGION` (optional): comma-separated region preference order.
//! - `POPCORN_TTL_SECONDS` (optional): requested session lifetime.

use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;

use crate::{Browser, BrowserBuildError, BrowserBuilder, BrowserError, BrowserTool};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Everything needed to rent one Popcorn session.
#[derive(Clone)]
pub struct PopcornConfig {
    control_plane: Url,
    client_id: String,
    client_secret: String,
    regions: Vec<String>,
    ttl_seconds: Option<u64>,
    session_id: Option<String>,
    request_timeout: Duration,
}

impl std::fmt::Debug for PopcornConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopcornConfig")
            .field("control_plane", &self.control_plane)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("regions", &self.regions)
            .field("ttl_seconds", &self.ttl_seconds)
            .field("session_id", &self.session_id)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl PopcornConfig {
    /// Creates a configuration for a credentialed Popcorn client.
    pub fn new(
        control_plane: Url,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            control_plane,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            regions: Vec::new(),
            ttl_seconds: None,
            session_id: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Reads the configuration from `POPCORN_*` environment variables.
    ///
    /// # Errors
    ///
    /// Returns [`PopcornError::Configuration`] when a required variable is
    /// missing or the control-plane URL does not parse.
    pub fn from_env() -> Result<Self, PopcornError> {
        fn required(name: &str) -> Result<String, PopcornError> {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| PopcornError::Configuration {
                    message: format!("`{name}` is required"),
                })
        }

        let control_plane =
            Url::parse(&required("POPCORN_CONTROL_PLANE_URL")?).map_err(|error| {
                PopcornError::Configuration {
                    message: format!("`POPCORN_CONTROL_PLANE_URL` is not a valid URL: {error}"),
                }
            })?;
        let mut config = Self::new(
            control_plane,
            required("POPCORN_CLIENT_ID")?,
            required("POPCORN_CLIENT_SECRET")?,
        );
        if let Ok(regions) = std::env::var("POPCORN_REGION") {
            config = config.regions(
                regions
                    .split(',')
                    .map(str::trim)
                    .filter(|region| !region.is_empty())
                    .map(str::to_owned),
            );
        }
        if let Ok(ttl) = std::env::var("POPCORN_TTL_SECONDS") {
            let ttl = ttl
                .trim()
                .parse::<u64>()
                .map_err(|error| PopcornError::Configuration {
                    message: format!("`POPCORN_TTL_SECONDS` must be a positive integer: {error}"),
                })?;
            config = config.ttl_seconds(ttl);
        }
        Ok(config)
    }

    /// Sets the region preference order tried by the control plane.
    #[must_use]
    pub fn regions(mut self, regions: impl IntoIterator<Item = String>) -> Self {
        self.regions = regions.into_iter().collect();
        self
    }

    /// Requests a session lifetime in seconds.
    #[must_use]
    pub const fn ttl_seconds(mut self, ttl_seconds: u64) -> Self {
        self.ttl_seconds = Some(ttl_seconds);
        self
    }

    /// Uses a caller-chosen session identifier instead of a generated one.
    #[must_use]
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Overrides the control-plane HTTP timeout.
    #[must_use]
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    fn bearer(&self) -> String {
        format!("Bearer {}:{}", self.client_id, self.client_secret)
    }

    fn sessions_url(&self) -> Result<Url, PopcornError> {
        self.control_plane
            .join("v1/sessions")
            .map_err(|error| PopcornError::Configuration {
                message: format!("cannot build sessions URL: {error}"),
            })
    }

    fn session_url(&self, session_id: &str) -> Result<Url, PopcornError> {
        self.control_plane
            .join(&format!("v1/session/{session_id}"))
            .map_err(|error| PopcornError::Configuration {
                message: format!("cannot build session URL: {error}"),
            })
    }
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

/// The session record returned by the Popcorn control plane.
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
    pub cdp_internal_url: Url,
    /// Allocated browser pod identity.
    #[serde(default)]
    pub browser_pod_id: Option<String>,
    /// Session deadline when the control plane set one.
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
    /// The browser controller could not be configured for the session.
    #[error(transparent)]
    Build(#[from] BrowserBuildError),
    /// A browser action failed.
    #[error(transparent)]
    Browser(#[from] BrowserError),
}

/// One rented Popcorn session wrapped as a Nanocodex browser Hand.
///
/// Dropping the value without calling [`PopcornBrowser::shutdown`] leaves the
/// session to Popcorn's TTL controller. Call `shutdown` to release it now.
pub struct PopcornBrowser {
    config: PopcornConfig,
    client: Client,
    session: PopcornSession,
    browser: Browser,
}

impl PopcornBrowser {
    /// Rents a Popcorn session and attaches the browser controller to it.
    ///
    /// # Errors
    ///
    /// Returns an error when the control plane refuses the session or when the
    /// browser controller cannot be configured for the remote CDP endpoint.
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
        let client = Client::builder().timeout(config.request_timeout).build()?;
        let session = create_session(&client, &config).await?;
        debug!(
            target: "nanocodex_browser",
            session = %session.session_id,
            region = ?session.region,
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
                    delete_session(&client, &config, &session.session_id).await
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
            client,
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

    /// Closes the controller and releases the session on the control plane.
    ///
    /// # Errors
    ///
    /// Returns the first error from closing the browser or deleting the
    /// session. The delete is attempted even when the close fails.
    pub async fn shutdown(self) -> Result<(), PopcornError> {
        let close_result = self.browser.close().await.map_err(PopcornError::from);
        let delete_result =
            delete_session(&self.client, &self.config, &self.session.session_id).await;
        if let Err(error) = &delete_result {
            warn!(
                target: "nanocodex_browser",
                session = %self.session.session_id,
                %error,
                "popcorn session release failed; TTL controller will reclaim it"
            );
        }
        close_result.and(delete_result)
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
        .header(reqwest::header::AUTHORIZATION, config.bearer())
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
        .header(reqwest::header::AUTHORIZATION, config.bearer())
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(config.bearer(), "Bearer id:secret");
    }
}
