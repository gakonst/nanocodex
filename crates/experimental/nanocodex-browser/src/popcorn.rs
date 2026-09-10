//! A Nanocodex browser Hand backed by a rented Popcorn session.
//!
//! [Popcorn](https://popcorn.reclaimprotocol.org) rents isolated headful
//! Chromium sessions that run inside a TEE. Each session exposes a CDP
//! WebSocket for the agent and a LiveView page a human can open to watch or
//! take over. This module rents one session, points the ordinary [`Browser`]
//! controller at its CDP endpoint, and releases the session on shutdown.
//!
//! From the agent's point of view a Popcorn browser is just another Hand: the
//! brain stays wherever it is, and `tools.browser(...)` executes inside the
//! remote enclave. Because the model never receives a connection URL, the
//! session capability never enters the conversation.
//!
//! # Access modes
//!
//! The default is the public pay-per-use endpoint, which needs no account.
//! Each five-minute block costs $0.01 in USDC on Base, paid per request with
//! [x402](https://popcorn.reclaimprotocol.org/ai.md): the first request is
//! answered with a payment challenge, the client validates the offer against a
//! policy compiled into this module, signs an EIP-3009 transfer authorization,
//! and repeats the request. Set `POPCORN_PAYER_PRIVATE_KEY` to an EVM key that
//! holds a little USDC and enough ETH for gas on Base.
//!
//! A credentialed control plane remains available for dedicated deployments.
//! Set `POPCORN_CONTROL_PLANE_URL`, `POPCORN_CLIENT_ID`, and
//! `POPCORN_CLIENT_SECRET` instead, and no payment is involved.
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
//! - `POPCORN_PAYER_PRIVATE_KEY`: EVM key that pays for public sessions. When
//!   set, the public x402 endpoint is used and nothing else is required.
//! - `POPCORN_CONTROL_PLANE_URL`, `POPCORN_CLIENT_ID`, `POPCORN_CLIENT_SECRET`:
//!   credentialed access to a dedicated deployment.
//! - `POPCORN_REGION` (optional, credentialed): comma-separated preference order.
//! - `POPCORN_TTL_SECONDS` (optional, credentialed): requested session lifetime.

use std::borrow::Cow;
use std::time::Duration;

use alloy_primitives::{Address, B256, U256, hex};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{Eip712Domain, SolStruct, sol};
use rand::RngCore;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracing::{debug, warn};
use url::Url;
use uuid::Uuid;

use crate::{Browser, BrowserBuildError, BrowserBuilder, BrowserError, BrowserTool};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Trusted x402 payment policy
//
// These values are the client's independent copy of the payment terms. An
// offer is signed only when it matches them exactly; a challenge is never
// trusted merely because it arrived in a `402` response.
// ---------------------------------------------------------------------------

/// Public pay-per-use session endpoint.
const X402_SESSIONS_URL: &str = "https://app.popcorn.reclaimprotocol.org/v1/x402/sessions";
/// CAIP-2 identity of Base mainnet.
const X402_NETWORK: &str = "eip155:8453";
/// Base mainnet chain id, used in the EIP-712 domain.
const X402_CHAIN_ID: u64 = 8453;
/// USDC on Base mainnet.
const X402_ASSET: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
/// The only account a session payment may pay.
const X402_PAY_TO: &str = "0x28f26a191D6bCa1FfD129261E726033188d65138";
/// Atomic USDC units for one block, `$0.01` at six decimals.
const X402_ATOMIC_PER_BLOCK: u64 = 10_000;
/// Only the exact scheme is accepted.
const X402_SCHEME: &str = "exact";
/// Only x402 v2 is accepted.
const X402_VERSION: u32 = 2;
/// Attempts for one paid action, reusing the original key, body, and signature.
const X402_MAX_ATTEMPTS: u32 = 5;

/// Seconds of browser time bought by one payment block.
pub const X402_BLOCK_SECONDS: u64 = 300;

sol! {
    /// EIP-3009 authorization signed to move USDC for one session action.
    struct TransferWithAuthorization {
        address from;
        address to;
        uint256 value;
        uint256 validAfter;
        uint256 validBefore;
        bytes32 nonce;
    }
}

/// How sessions are obtained from Popcorn.
#[derive(Clone)]
enum Access {
    /// Public pay-per-use endpoint settled with x402 payments.
    X402 {
        /// Session collection endpoint.
        endpoint: Url,
        /// Hex-encoded EVM key that signs payment authorizations.
        payer_key: String,
    },
    /// Credentialed control plane for a dedicated deployment.
    Credentialed {
        /// Control-plane base URL.
        control_plane: Url,
        /// Client identity.
        client_id: String,
        /// Client secret.
        client_secret: String,
    },
}

/// Everything needed to rent one Popcorn session.
#[derive(Clone)]
pub struct PopcornConfig {
    access: Access,
    regions: Vec<String>,
    ttl_seconds: Option<u64>,
    session_id: Option<String>,
    request_timeout: Duration,
}

/// Redacts the payer key and client secret, both of which are credentials.
impl std::fmt::Debug for PopcornConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("PopcornConfig");
        match &self.access {
            Access::X402 { .. } => {
                out.field("access", &"x402")
                    .field("payer_key", &"<redacted>");
            }
            Access::Credentialed {
                control_plane,
                client_id,
                ..
            } => {
                out.field("access", &"credentialed")
                    .field("control_plane", control_plane)
                    .field("client_id", client_id)
                    .field("client_secret", &"<redacted>");
            }
        }
        out.field("regions", &self.regions)
            .field("ttl_seconds", &self.ttl_seconds)
            .field("session_id", &self.session_id)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl PopcornConfig {
    /// Creates a configuration that pays for public sessions with x402.
    ///
    /// `payer_key` is a hex-encoded EVM private key holding USDC and a little
    /// ETH for gas on Base. It never leaves this process.
    #[must_use]
    pub fn x402(payer_key: impl Into<String>) -> Self {
        Self {
            access: Access::X402 {
                endpoint: Url::parse(X402_SESSIONS_URL).expect("static endpoint parses"),
                payer_key: payer_key.into(),
            },
            regions: Vec::new(),
            ttl_seconds: None,
            session_id: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Creates a configuration for a credentialed Popcorn client.
    pub fn new(
        control_plane: Url,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            access: Access::Credentialed {
                control_plane,
                client_id: client_id.into(),
                client_secret: client_secret.into(),
            },
            regions: Vec::new(),
            ttl_seconds: None,
            session_id: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Reads the configuration from `POPCORN_*` environment variables.
    ///
    /// `POPCORN_PAYER_PRIVATE_KEY` selects the public pay-per-use endpoint.
    /// Otherwise the credentialed variables are required.
    ///
    /// # Errors
    ///
    /// Returns [`PopcornError::Configuration`] when neither access mode is
    /// fully configured or the control-plane URL does not parse.
    pub fn from_env() -> Result<Self, PopcornError> {
        if let Some(payer_key) = optional_env("POPCORN_PAYER_PRIVATE_KEY") {
            return Ok(Self::x402(payer_key));
        }
        if optional_env("POPCORN_CONTROL_PLANE_URL").is_none()
            && optional_env("POPCORN_CLIENT_ID").is_none()
            && optional_env("POPCORN_CLIENT_SECRET").is_none()
        {
            return Err(PopcornError::Configuration {
                message: "set `POPCORN_PAYER_PRIVATE_KEY` for public pay-per-use sessions, or \
                          `POPCORN_CONTROL_PLANE_URL`, `POPCORN_CLIENT_ID`, and \
                          `POPCORN_CLIENT_SECRET` for a dedicated deployment"
                    .to_owned(),
            });
        }

        let control_plane =
            Url::parse(&required_env("POPCORN_CONTROL_PLANE_URL")?).map_err(|error| {
                PopcornError::Configuration {
                    message: format!("`POPCORN_CONTROL_PLANE_URL` is not a valid URL: {error}"),
                }
            })?;
        let mut config = Self::new(
            control_plane,
            required_env("POPCORN_CLIENT_ID")?,
            required_env("POPCORN_CLIENT_SECRET")?,
        );
        if let Some(regions) = optional_env("POPCORN_REGION") {
            config = config.regions(
                regions
                    .split(',')
                    .map(str::trim)
                    .filter(|region| !region.is_empty())
                    .map(str::to_owned),
            );
        }
        if let Some(ttl) = optional_env("POPCORN_TTL_SECONDS") {
            let ttl = ttl
                .parse::<u64>()
                .map_err(|error| PopcornError::Configuration {
                    message: format!("`POPCORN_TTL_SECONDS` must be a positive integer: {error}"),
                })?;
            config = config.ttl_seconds(ttl);
        }
        Ok(config)
    }

    /// Sets the region preference order tried by a credentialed control plane.
    #[must_use]
    pub fn regions(mut self, regions: impl IntoIterator<Item = String>) -> Self {
        self.regions = regions.into_iter().collect();
        self
    }

    /// Requests a session lifetime in seconds from a credentialed control plane.
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

    /// Overrides the HTTP timeout applied to Popcorn requests.
    #[must_use]
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Reports whether this configuration pays per use over x402.
    #[must_use]
    pub const fn is_x402(&self) -> bool {
        matches!(self.access, Access::X402 { .. })
    }

    fn bearer(&self) -> Option<String> {
        match &self.access {
            Access::Credentialed {
                client_id,
                client_secret,
                ..
            } => Some(format!("Bearer {client_id}:{client_secret}")),
            Access::X402 { .. } => None,
        }
    }
}

/// Reads a non-blank environment variable.
fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Reads a required non-blank environment variable.
fn required_env(name: &str) -> Result<String, PopcornError> {
    optional_env(name).ok_or_else(|| PopcornError::Configuration {
        message: format!("`{name}` is required"),
    })
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

/// One rented browser session.
///
/// Every connection URL, and in pay-per-use mode the session id itself, is a
/// bearer capability. They are private to this type and [`Debug`] renders none
/// of the URLs. Give them only to the process that owns the session.
#[derive(Clone)]
pub struct PopcornSession {
    session_id: String,
    live_view: Url,
    cdp: Url,
    expires_at: Option<String>,
    region: Option<String>,
    cluster_name: Option<String>,
    browser_pod_id: Option<String>,
    paid_seconds: Option<u64>,
}

impl PopcornSession {
    /// Returns the session identifier.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the LiveView page a human can open to watch or take over.
    #[must_use]
    pub const fn live_view_url(&self) -> &Url {
        &self.live_view
    }

    /// Returns the session deadline reported by Popcorn.
    #[must_use]
    pub fn expires_at(&self) -> Option<&str> {
        self.expires_at.as_deref()
    }

    /// Returns the selected region.
    #[must_use]
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// Returns the selected cluster.
    #[must_use]
    pub fn cluster_name(&self) -> Option<&str> {
        self.cluster_name.as_deref()
    }

    /// Returns the allocated browser pod identity.
    #[must_use]
    pub fn browser_pod_id(&self) -> Option<&str> {
        self.browser_pod_id.as_deref()
    }

    /// Returns the browser time paid for so far, in seconds.
    #[must_use]
    pub const fn paid_seconds(&self) -> Option<u64> {
        self.paid_seconds
    }
}

/// Redacts every session URL, each of which is a bearer capability.
impl std::fmt::Debug for PopcornSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PopcornSession")
            .field("session_id", &self.session_id)
            .field("live_view", &"<redacted>")
            .field("cdp", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("region", &self.region)
            .field("cluster_name", &self.cluster_name)
            .field("browser_pod_id", &self.browser_pod_id)
            .field("paid_seconds", &self.paid_seconds)
            .finish()
    }
}

/// The credentialed control-plane session record.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialedSession {
    session_id: String,
    url: Url,
    cdp_internal_url: Url,
    #[serde(default)]
    browser_pod_id: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    cluster_name: Option<String>,
}

impl From<CredentialedSession> for PopcornSession {
    fn from(value: CredentialedSession) -> Self {
        Self {
            session_id: value.session_id,
            live_view: value.url,
            cdp: value.cdp_internal_url,
            expires_at: value.expires_at,
            region: value.region,
            cluster_name: value.cluster_name,
            browser_pod_id: value.browser_pod_id,
            paid_seconds: None,
        }
    }
}

#[derive(Deserialize)]
struct CreateSessionResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    session: Option<CredentialedSession>,
}

/// The public pay-per-use session record.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct X402Session {
    session_id: String,
    connect_url: Url,
    live_view_url: Url,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    paid_seconds: Option<u64>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    cluster_name: Option<String>,
}

impl From<X402Session> for PopcornSession {
    fn from(value: X402Session) -> Self {
        Self {
            session_id: value.session_id,
            live_view: value.live_view_url,
            cdp: value.connect_url,
            expires_at: value.expires_at,
            region: value.region,
            cluster_name: value.cluster_name,
            browser_pod_id: None,
            paid_seconds: value.paid_seconds,
        }
    }
}

/// The result of buying more time on an existing session.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct X402Extension {
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    paid_seconds_total: Option<u64>,
}

/// One payment offer inside a challenge.
///
/// Carries only public payment terms, so it is safe to render.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaymentRequirements {
    scheme: String,
    network: String,
    amount: String,
    asset: String,
    pay_to: String,
    max_timeout_seconds: u64,
    #[serde(default)]
    extra: Option<PaymentExtra>,
}

/// EIP-712 domain parameters carried by an offer.
#[derive(Debug, Deserialize)]
struct PaymentExtra {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

/// A decoded `402` challenge.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaymentRequired {
    x402_version: u32,
    #[serde(default)]
    resource: Option<serde_json::Value>,
    accepts: Vec<serde_json::Value>,
}

/// The signed authorization sent back with the paid request.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Authorization {
    from: String,
    to: String,
    value: String,
    valid_after: String,
    valid_before: String,
    nonce: String,
}

#[derive(Serialize)]
struct Eip3009Payload {
    authorization: Authorization,
    signature: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentPayload<'a> {
    x402_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a serde_json::Value>,
    accepted: &'a serde_json::Value,
    payload: Eip3009Payload,
}

/// Errors from renting, driving, paying for, or releasing a Popcorn session.
#[derive(Debug, thiserror::Error)]
pub enum PopcornError {
    /// The configuration is incomplete or malformed.
    #[error("popcorn configuration error: {message}")]
    Configuration {
        /// Human-readable explanation.
        message: String,
    },
    /// Popcorn could not be reached.
    #[error("popcorn request failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// Popcorn rejected the request.
    #[error("popcorn returned {status}: {body}")]
    ControlPlane {
        /// HTTP status.
        status: StatusCode,
        /// Response body, truncated.
        body: String,
    },
    /// A payment offer did not match the trusted policy, so nothing was signed.
    #[error("untrusted popcorn payment offer: {detail}")]
    UntrustedOffer {
        /// Which term failed the check.
        detail: String,
    },
    /// The payment authorization could not be signed.
    #[error("popcorn payment could not be signed: {detail}")]
    Payment {
        /// Human-readable explanation, never containing key material.
        detail: String,
    },
    /// A Popcorn response could not be decoded.
    #[error("popcorn response could not be decoded: {detail}")]
    Decode {
        /// Human-readable explanation.
        detail: String,
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
/// session to expire on its own. Call `shutdown` to release it now; unused
/// paid time is not refunded.
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
    /// Returns an error when a payment offer is untrusted, when Popcorn
    /// refuses the session, or when the controller cannot attach.
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
        let session = match &config.access {
            Access::X402 {
                endpoint,
                payer_key,
            } => {
                let signer = parse_signer(payer_key)?;
                let created: X402Session = paid_post(
                    &client,
                    &signer,
                    endpoint.clone(),
                    &serde_json::json!({}),
                    X402_ATOMIC_PER_BLOCK,
                )
                .await?;
                PopcornSession::from(created)
            }
            Access::Credentialed { .. } => create_credentialed_session(&client, &config).await?,
        };
        debug!(
            target: "nanocodex_browser",
            session = %session.session_id,
            region = ?session.region,
            paid_seconds = ?session.paid_seconds,
            "rented popcorn session"
        );
        // The session is already rented, so release it rather than leaving it
        // to expire when the controller cannot be attached.
        let browser = match browser.cdp_endpoint(session.cdp.clone()).build() {
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
                         it will expire on its own"
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

    /// Returns the session record.
    #[must_use]
    pub const fn session(&self) -> &PopcornSession {
        &self.session
    }

    /// Returns the LiveView page a human can open to watch or take over.
    #[must_use]
    pub const fn live_view_url(&self) -> &Url {
        &self.session.live_view
    }

    /// Buys `blocks` more whole time blocks of
    /// [`X402_BLOCK_SECONDS`] each, keeping the same browser and URLs.
    ///
    /// Start an extension while at least four minutes remain; one presented
    /// later is rejected before settlement.
    ///
    /// # Errors
    ///
    /// Returns [`PopcornError::Configuration`] for a credentialed session or a
    /// zero block count, and a payment or transport error otherwise.
    pub async fn extend(&mut self, blocks: u32) -> Result<(), PopcornError> {
        if blocks == 0 {
            return Err(PopcornError::Configuration {
                message: "an extension must buy at least one block".to_owned(),
            });
        }
        let Access::X402 {
            endpoint,
            payer_key,
        } = &self.config.access
        else {
            return Err(PopcornError::Configuration {
                message: "session extension is only available for public pay-per-use sessions"
                    .to_owned(),
            });
        };
        let signer = parse_signer(payer_key)?;
        let url = x402_session_url(endpoint, &self.session.session_id, Some("extend"))?;
        let amount = X402_ATOMIC_PER_BLOCK
            .checked_mul(u64::from(blocks))
            .ok_or_else(|| PopcornError::Configuration {
                message: "requested extension is too large".to_owned(),
            })?;
        let extended: X402Extension = paid_post(
            &self.client,
            &signer,
            url,
            &serde_json::json!({ "blocks": blocks }),
            amount,
        )
        .await?;
        if extended.expires_at.is_some() {
            self.session.expires_at = extended.expires_at;
        }
        if extended.paid_seconds_total.is_some() {
            self.session.paid_seconds = extended.paid_seconds_total;
        }
        debug!(
            target: "nanocodex_browser",
            session = %self.session.session_id,
            blocks,
            paid_seconds = ?self.session.paid_seconds,
            "extended popcorn session"
        );
        Ok(())
    }

    /// Closes the controller and ends the session.
    ///
    /// # Errors
    ///
    /// Returns the first error from closing the browser or ending the session.
    /// The termination is attempted even when the close fails.
    pub async fn shutdown(self) -> Result<(), PopcornError> {
        let close_result = self.browser.close().await.map_err(PopcornError::from);
        let delete_result =
            delete_session(&self.client, &self.config, &self.session.session_id).await;
        if let Err(error) = &delete_result {
            warn!(
                target: "nanocodex_browser",
                session = %self.session.session_id,
                %error,
                "popcorn session release failed; it will expire on its own"
            );
        }
        close_result.and(delete_result)
    }
}

/// Builds `<endpoint>/<session id>[/<action>]`.
fn x402_session_url(
    endpoint: &Url,
    session_id: &str,
    action: Option<&str>,
) -> Result<Url, PopcornError> {
    let mut url = endpoint.clone();
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|()| PopcornError::Configuration {
                message: "the x402 endpoint cannot have path segments".to_owned(),
            })?;
        path.pop_if_empty().push(session_id);
        if let Some(action) = action {
            path.push(action);
        }
    }
    Ok(url)
}

/// Parses the payer key without ever placing it in an error message.
fn parse_signer(payer_key: &str) -> Result<PrivateKeySigner, PopcornError> {
    payer_key
        .parse::<PrivateKeySigner>()
        .map_err(|_| PopcornError::Payment {
            detail: "`POPCORN_PAYER_PRIVATE_KEY` is not a valid EVM private key".to_owned(),
        })
}

/// Validates one offer against the trusted policy.
fn validate_offer(offer: &PaymentRequirements, expected_atomic: u64) -> Result<(), PopcornError> {
    let reject = |detail: String| PopcornError::UntrustedOffer { detail };
    if offer.scheme != X402_SCHEME {
        return Err(reject(format!(
            "scheme is `{}`, expected `{X402_SCHEME}`",
            offer.scheme
        )));
    }
    if offer.network != X402_NETWORK {
        return Err(reject(format!(
            "network is `{}`, expected `{X402_NETWORK}`",
            offer.network
        )));
    }
    if !offer.asset.eq_ignore_ascii_case(X402_ASSET) {
        return Err(reject(format!(
            "asset is `{}`, expected `{X402_ASSET}`",
            offer.asset
        )));
    }
    if !offer.pay_to.eq_ignore_ascii_case(X402_PAY_TO) {
        return Err(reject(format!(
            "payee is `{}`, expected `{X402_PAY_TO}`",
            offer.pay_to
        )));
    }
    let expected = expected_atomic.to_string();
    if offer.amount != expected {
        return Err(reject(format!(
            "amount is `{}`, expected `{expected}`",
            offer.amount
        )));
    }
    Ok(())
}

/// Returns the single trusted offer in a challenge.
fn select_offer(
    challenge: &PaymentRequired,
    expected_atomic: u64,
) -> Result<(&serde_json::Value, PaymentRequirements), PopcornError> {
    if challenge.x402_version != X402_VERSION {
        return Err(PopcornError::UntrustedOffer {
            detail: format!(
                "x402 version is {}, expected {X402_VERSION}",
                challenge.x402_version
            ),
        });
    }
    let mut trusted = None;
    let mut last_rejection = None;
    for raw in &challenge.accepts {
        let Ok(offer) = serde_json::from_value::<PaymentRequirements>(raw.clone()) else {
            continue;
        };
        match validate_offer(&offer, expected_atomic) {
            Ok(()) => {
                if trusted.is_some() {
                    return Err(PopcornError::UntrustedOffer {
                        detail: "the challenge carried more than one trusted offer".to_owned(),
                    });
                }
                trusted = Some((raw, offer));
            }
            Err(error) => last_rejection = Some(error),
        }
    }
    trusted.ok_or_else(|| {
        last_rejection.unwrap_or_else(|| PopcornError::UntrustedOffer {
            detail: "the challenge carried no usable offer".to_owned(),
        })
    })
}

/// Signs the EIP-3009 authorization for one validated offer.
fn sign_authorization(
    signer: &PrivateKeySigner,
    offer: &PaymentRequirements,
) -> Result<Eip3009Payload, PopcornError> {
    let extra = offer.extra.as_ref();
    let (Some(name), Some(version)) = (
        extra.and_then(|extra| extra.name.clone()),
        extra.and_then(|extra| extra.version.clone()),
    ) else {
        return Err(PopcornError::UntrustedOffer {
            detail: "the offer is missing the EIP-712 domain name and version".to_owned(),
        });
    };
    let asset = offer
        .asset
        .parse::<Address>()
        .map_err(|error| PopcornError::UntrustedOffer {
            detail: format!("the offer asset is not an address: {error}"),
        })?;
    let pay_to = offer
        .pay_to
        .parse::<Address>()
        .map_err(|error| PopcornError::UntrustedOffer {
            detail: format!("the offer payee is not an address: {error}"),
        })?;
    let value = offer
        .amount
        .parse::<U256>()
        .map_err(|error| PopcornError::UntrustedOffer {
            detail: format!("the offer amount is not an integer: {error}"),
        })?;

    let valid_before = unix_now()
        .checked_add(offer.max_timeout_seconds)
        .ok_or_else(|| PopcornError::Payment {
            detail: "the offer timeout overflows the authorization deadline".to_owned(),
        })?;
    let mut nonce_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce_bytes);

    sign_transfer_authorization(
        signer,
        &AuthorizationTerms {
            asset,
            pay_to,
            value,
            valid_before,
            nonce: B256::from(nonce_bytes),
            domain_name: name,
            domain_version: version,
        },
    )
}

/// The fully resolved terms of one EIP-3009 authorization.
struct AuthorizationTerms {
    asset: Address,
    pay_to: Address,
    value: U256,
    valid_before: u64,
    nonce: B256,
    domain_name: String,
    domain_version: String,
}

/// Signs one authorization, given every term already resolved.
///
/// Split out from [`sign_authorization`] so a fixed vector can be checked
/// against an independent EIP-712 implementation.
fn sign_transfer_authorization(
    signer: &PrivateKeySigner,
    terms: &AuthorizationTerms,
) -> Result<Eip3009Payload, PopcornError> {
    let authorization = TransferWithAuthorization {
        from: signer.address(),
        to: terms.pay_to,
        value: terms.value,
        validAfter: U256::ZERO,
        validBefore: U256::from(terms.valid_before),
        nonce: terms.nonce,
    };
    let domain = Eip712Domain::new(
        Some(Cow::Owned(terms.domain_name.clone())),
        Some(Cow::Owned(terms.domain_version.clone())),
        Some(U256::from(X402_CHAIN_ID)),
        Some(terms.asset),
        None,
    );
    let signature = signer
        .sign_hash_sync(&authorization.eip712_signing_hash(&domain))
        .map_err(|error| PopcornError::Payment {
            detail: format!("the wallet refused to sign the authorization: {error}"),
        })?;

    Ok(Eip3009Payload {
        authorization: Authorization {
            from: authorization.from.to_checksum(None),
            to: authorization.to.to_checksum(None),
            value: authorization.value.to_string(),
            valid_after: "0".to_owned(),
            valid_before: terms.valid_before.to_string(),
            nonce: format!("0x{}", hex::encode(terms.nonce)),
        },
        signature: format!("0x{}", hex::encode(signature.as_bytes())),
    })
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Decodes a challenge from the `PAYMENT-REQUIRED` header, else the body.
fn decode_payment_required(
    header: Option<&str>,
    body: &str,
) -> Result<PaymentRequired, PopcornError> {
    if let Some(header) = header {
        let decoded = base64_decode(header)?;
        return serde_json::from_slice(&decoded).map_err(|error| PopcornError::Decode {
            detail: format!("the PAYMENT-REQUIRED header is not a valid challenge: {error}"),
        });
    }
    serde_json::from_str(body).map_err(|error| PopcornError::Decode {
        detail: format!("the challenge body could not be parsed: {error}"),
    })
}

/// Decodes standard base64 as used by the x402 headers.
fn base64_decode(value: &str) -> Result<Vec<u8>, PopcornError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map_err(|error| PopcornError::Decode {
            detail: format!("a payment header was not valid base64: {error}"),
        })
}

/// Encodes standard base64 as used by the x402 headers.
fn base64_encode(value: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value)
}

/// Performs one paid action: challenge, validate, sign, then retry-safe replay.
///
/// A retry reuses the original idempotency key, body, and signature so the
/// server can return the first result instead of charging twice.
async fn paid_post<T: DeserializeOwned>(
    client: &Client,
    signer: &PrivateKeySigner,
    url: Url,
    body: &serde_json::Value,
    expected_atomic: u64,
) -> Result<T, PopcornError> {
    let idempotency_key = Uuid::new_v4().to_string();
    let body_text = serde_json::to_string(body).map_err(|error| PopcornError::Decode {
        detail: format!("the request body could not be encoded: {error}"),
    })?;

    let challenge = client
        .post(url.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("Idempotency-Key", &idempotency_key)
        .body(body_text.clone())
        .send()
        .await?;
    let status = challenge.status();
    let header = challenge
        .headers()
        .get("PAYMENT-REQUIRED")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let challenge_body = challenge.text().await?;
    if status != StatusCode::PAYMENT_REQUIRED {
        return Err(PopcornError::ControlPlane {
            status,
            body: truncate(&challenge_body),
        });
    }

    let required = decode_payment_required(header.as_deref(), &challenge_body)?;
    let (raw_offer, offer) = select_offer(&required, expected_atomic)?;
    let payload = sign_authorization(signer, &offer)?;
    let signature_header = base64_encode(
        serde_json::to_string(&PaymentPayload {
            x402_version: X402_VERSION,
            resource: required.resource.as_ref(),
            accepted: raw_offer,
            payload,
        })
        .map_err(|error| PopcornError::Decode {
            detail: format!("the payment payload could not be encoded: {error}"),
        })?
        .as_bytes(),
    );

    for attempt in 0..X402_MAX_ATTEMPTS {
        let response = client
            .post(url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("Idempotency-Key", &idempotency_key)
            .header("PAYMENT-SIGNATURE", &signature_header)
            .body(body_text.clone())
            .send()
            .await?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let text = response.text().await?;

        if status.is_success() {
            return serde_json::from_str(&text).map_err(|error| PopcornError::Decode {
                detail: format!("the paid response could not be parsed: {error}"),
            });
        }

        let retryable = matches!(status.as_u16(), 409 | 503);
        if retryable && retry_after > 0 && attempt + 1 < X402_MAX_ATTEMPTS {
            debug!(
                target: "nanocodex_browser",
                status = status.as_u16(),
                retry_after,
                "popcorn payment is settling; replaying the same signed request"
            );
            tokio::time::sleep(Duration::from_secs(retry_after)).await;
            continue;
        }
        return Err(PopcornError::ControlPlane {
            status,
            body: truncate(&text),
        });
    }
    Err(PopcornError::ControlPlane {
        status: StatusCode::SERVICE_UNAVAILABLE,
        body: "the paid request did not settle after the allowed retries".to_owned(),
    })
}

/// Creates a session against a credentialed control plane.
async fn create_credentialed_session(
    client: &Client,
    config: &PopcornConfig,
) -> Result<PopcornSession, PopcornError> {
    let Access::Credentialed { control_plane, .. } = &config.access else {
        return Err(PopcornError::Configuration {
            message: "a credentialed control plane is not configured".to_owned(),
        });
    };
    let sessions_url =
        control_plane
            .join("v1/sessions")
            .map_err(|error| PopcornError::Configuration {
                message: format!("cannot build sessions URL: {error}"),
            })?;
    let request = CreateSessionRequest {
        session_id: config.session_id.as_deref(),
        ttl_seconds: config.ttl_seconds,
        regions: &config.regions,
    };
    let mut builder = client.post(sessions_url);
    if let Some(bearer) = config.bearer() {
        builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
    }
    let response = builder.json(&request).send().await?;
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
    parsed
        .session
        .map(PopcornSession::from)
        .ok_or_else(|| PopcornError::ControlPlane {
            status,
            body: "the session response is missing its connection URL".to_owned(),
        })
}

/// Ends a session. Termination never requires another payment.
async fn delete_session(
    client: &Client,
    config: &PopcornConfig,
    session_id: &str,
) -> Result<(), PopcornError> {
    let url = match &config.access {
        Access::X402 { endpoint, .. } => x402_session_url(endpoint, session_id, None)?,
        Access::Credentialed { control_plane, .. } => control_plane
            .join(&format!("v1/session/{session_id}"))
            .map_err(|error| PopcornError::Configuration {
                message: format!("cannot build session URL: {error}"),
            })?,
    };
    let mut builder = client.delete(url);
    if let Some(bearer) = config.bearer() {
        builder = builder.header(reqwest::header::AUTHORIZATION, bearer);
    }
    let response = builder.send().await?;
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

    /// A challenge matching the trusted policy exactly.
    fn trusted_challenge(amount: &str) -> serde_json::Value {
        serde_json::json!({
            "x402Version": 2,
            "resource": { "url": X402_SESSIONS_URL },
            "accepts": [{
                "scheme": "exact",
                "network": "eip155:8453",
                "amount": amount,
                "asset": X402_ASSET,
                "payTo": X402_PAY_TO,
                "maxTimeoutSeconds": 120,
                "extra": { "name": "USD Coin", "version": "2" }
            }]
        })
    }

    fn offer_with(field: &str, value: serde_json::Value) -> PaymentRequirements {
        let mut challenge = trusted_challenge("10000");
        challenge["accepts"][0][field] = value;
        serde_json::from_value(challenge["accepts"][0].clone()).unwrap()
    }

    #[test]
    fn accepts_an_exact_policy_match() {
        let offer = offer_with("scheme", serde_json::json!("exact"));
        assert!(validate_offer(&offer, X402_ATOMIC_PER_BLOCK).is_ok());
    }

    #[test]
    fn accepts_a_checksum_variant_of_the_trusted_addresses() {
        let offer = offer_with("asset", serde_json::json!(X402_ASSET.to_lowercase()));
        assert!(validate_offer(&offer, X402_ATOMIC_PER_BLOCK).is_ok());
        let offer = offer_with("payTo", serde_json::json!(X402_PAY_TO.to_uppercase()));
        assert!(validate_offer(&offer, X402_ATOMIC_PER_BLOCK).is_ok());
    }

    #[test]
    fn rejects_a_different_network() {
        let offer = offer_with("network", serde_json::json!("eip155:84532"));
        let error = validate_offer(&offer, X402_ATOMIC_PER_BLOCK).unwrap_err();
        assert!(format!("{error}").contains("network"), "{error}");
    }

    #[test]
    fn rejects_a_different_asset() {
        let offer = offer_with(
            "asset",
            serde_json::json!("0x0000000000000000000000000000000000000001"),
        );
        let error = validate_offer(&offer, X402_ATOMIC_PER_BLOCK).unwrap_err();
        assert!(format!("{error}").contains("asset"), "{error}");
    }

    #[test]
    fn rejects_a_different_payee() {
        let offer = offer_with(
            "payTo",
            serde_json::json!("0x0000000000000000000000000000000000000002"),
        );
        let error = validate_offer(&offer, X402_ATOMIC_PER_BLOCK).unwrap_err();
        assert!(format!("{error}").contains("payee"), "{error}");
    }

    #[test]
    fn rejects_a_different_amount() {
        let offer = offer_with("amount", serde_json::json!("20000"));
        let error = validate_offer(&offer, X402_ATOMIC_PER_BLOCK).unwrap_err();
        assert!(format!("{error}").contains("amount"), "{error}");
    }

    #[test]
    fn rejects_a_different_scheme() {
        let offer = offer_with("scheme", serde_json::json!("upto"));
        let error = validate_offer(&offer, X402_ATOMIC_PER_BLOCK).unwrap_err();
        assert!(format!("{error}").contains("scheme"), "{error}");
    }

    #[test]
    fn rejects_an_unsupported_protocol_version() {
        let mut challenge = trusted_challenge("10000");
        challenge["x402Version"] = serde_json::json!(1);
        let challenge: PaymentRequired = serde_json::from_value(challenge).unwrap();
        let error = select_offer(&challenge, X402_ATOMIC_PER_BLOCK).unwrap_err();
        assert!(format!("{error}").contains("x402 version"), "{error}");
    }

    #[test]
    fn selects_the_single_trusted_offer_among_untrusted_ones() {
        let mut challenge = trusted_challenge("10000");
        let mut hostile = challenge["accepts"][0].clone();
        hostile["payTo"] = serde_json::json!("0x0000000000000000000000000000000000000003");
        challenge["accepts"] = serde_json::json!([hostile, challenge["accepts"][0].clone()]);
        let challenge: PaymentRequired = serde_json::from_value(challenge).unwrap();
        let (_, offer) = select_offer(&challenge, X402_ATOMIC_PER_BLOCK).unwrap();
        assert!(offer.pay_to.eq_ignore_ascii_case(X402_PAY_TO));
    }

    #[test]
    fn extension_amount_scales_by_whole_blocks() {
        let offer = offer_with("amount", serde_json::json!("30000"));
        assert!(validate_offer(&offer, X402_ATOMIC_PER_BLOCK * 3).is_ok());
        assert!(validate_offer(&offer, X402_ATOMIC_PER_BLOCK).is_err());
    }

    #[test]
    fn parses_the_x402_session_shape() {
        let body = serde_json::json!({
            "sessionId": "x402s_abc",
            "sessionUrl": "https://app.example.com/v1/x402/sessions/x402s_abc",
            "connectUrl": "wss://gateway.example.com/cdp/x402s_abc/tok",
            "liveViewUrl": "https://gateway.example.com/liveview/x402s_abc/tok/liveview.html",
            "vncUrl": "https://gateway.example.com/liveview/x402s_abc/tok/liveview.html",
            "vncWsUrl": "wss://gateway.example.com/liveview-ws/x402s_abc/tok",
            "expiresAt": "2026-08-04T12:30:00.000Z",
            "paidSeconds": 300,
            "region": "asia-south1",
            "clusterName": "popcorn-prod"
        });
        let parsed: X402Session = serde_json::from_value(body).unwrap();
        let session = PopcornSession::from(parsed);
        assert_eq!(session.session_id(), "x402s_abc");
        assert_eq!(session.cdp.scheme(), "wss");
        assert_eq!(session.paid_seconds(), Some(300));
        assert_eq!(session.region(), Some("asia-south1"));
    }

    #[test]
    fn parses_the_x402_extension_shape() {
        let body = serde_json::json!({
            "sessionId": "x402s_abc",
            "additionalSeconds": 600,
            "paidSecondsTotal": 900,
            "expiresAt": "2026-08-04T12:40:00.000Z"
        });
        let parsed: X402Extension = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.paid_seconds_total, Some(900));
        assert_eq!(
            parsed.expires_at.as_deref(),
            Some("2026-08-04T12:40:00.000Z")
        );
    }

    #[test]
    fn decodes_a_challenge_from_the_header_before_the_body() {
        let header = base64_encode(trusted_challenge("10000").to_string().as_bytes());
        let decoded = decode_payment_required(Some(&header), "not json").unwrap();
        assert_eq!(decoded.x402_version, 2);
        assert_eq!(decoded.accepts.len(), 1);
    }

    #[test]
    fn decodes_a_challenge_from_the_body_when_no_header_is_present() {
        let body = trusted_challenge("10000").to_string();
        let decoded = decode_payment_required(None, &body).unwrap();
        assert_eq!(decoded.x402_version, 2);
    }

    #[test]
    fn signs_the_authorization_the_policy_validated() {
        let signer = PrivateKeySigner::random();
        let offer = offer_with("scheme", serde_json::json!("exact"));
        let payload = sign_authorization(&signer, &offer).unwrap();
        assert!(payload.authorization.to.eq_ignore_ascii_case(X402_PAY_TO));
        assert_eq!(payload.authorization.value, "10000");
        assert_eq!(payload.authorization.valid_after, "0");
        assert_eq!(payload.signature.len(), 2 + 130);
        assert!(payload.authorization.nonce.starts_with("0x"));
        assert_eq!(payload.authorization.nonce.len(), 2 + 64);
    }

    /// Cross-checks the EIP-712 signature against `viem`, the implementation
    /// the Popcorn reference client uses, for one fixed vector. A mistake in
    /// the domain, the struct definition, or the signature encoding changes
    /// this value.
    ///
    /// The key is the published Anvil test key, never used to hold funds.
    #[test]
    fn signature_matches_the_reference_eip712_implementation() {
        let signer: PrivateKeySigner =
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .unwrap();
        assert_eq!(
            signer.address().to_checksum(None),
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
        );
        let terms = AuthorizationTerms {
            asset: X402_ASSET.parse().unwrap(),
            pay_to: X402_PAY_TO.parse().unwrap(),
            value: U256::from(10_000u64),
            valid_before: 1_789_000_000,
            nonce: "0x00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
                .parse()
                .unwrap(),
            domain_name: "USD Coin".to_owned(),
            domain_version: "2".to_owned(),
        };
        let payload = sign_transfer_authorization(&signer, &terms).unwrap();
        assert_eq!(
            payload.signature,
            "0xb8ab7bc63d1242be4e03cf85096be78cdd465f15c84d64f6fab87bbe0631cdbc\
6644968e60ad0e7b243957af4a5ddfb19591b5d88f687a1a2b0153e4c68bfc7c1b"
                .replace('\n', "")
        );
        assert_eq!(payload.authorization.valid_before, "1789000000");
        assert_eq!(payload.authorization.value, "10000");
    }

    #[test]
    fn builds_x402_session_urls() {
        let endpoint = Url::parse(X402_SESSIONS_URL).unwrap();
        assert_eq!(
            x402_session_url(&endpoint, "x402s_abc", None)
                .unwrap()
                .path(),
            "/v1/x402/sessions/x402s_abc"
        );
        assert_eq!(
            x402_session_url(&endpoint, "x402s_abc", Some("extend"))
                .unwrap()
                .path(),
            "/v1/x402/sessions/x402s_abc/extend"
        );
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
        let session = PopcornSession::from(parsed.session.unwrap());
        assert_eq!(session.session_id(), "demo-session");
        assert_eq!(session.cdp.scheme(), "wss");
        assert_eq!(session.region(), Some("us-central1"));
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
    fn debug_output_redacts_session_urls() {
        let body = serde_json::json!({
            "sessionId": "demo-session",
            "connectUrl": "wss://browser.example.com/cdp/demo-session/tok",
            "liveViewUrl": "https://browser.example.com/liveview/demo-session/tok/liveview.html"
        });
        let parsed: X402Session = serde_json::from_value(body).unwrap();
        let rendered = format!("{:?}", PopcornSession::from(parsed));
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
    fn debug_output_redacts_the_payer_key() {
        let config = PopcornConfig::x402(
            "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
        );
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("59c6995e"),
            "payer key leaked: {rendered}"
        );
        assert!(rendered.contains("x402"));
        assert!(config.is_x402());
    }

    #[test]
    fn credentialed_config_builds_control_plane_urls() {
        let config = PopcornConfig::new(
            Url::parse("https://control.example.com/").unwrap(),
            "id",
            "secret",
        );
        assert!(!config.is_x402());
        assert_eq!(config.bearer().as_deref(), Some("Bearer id:secret"));
    }

    #[test]
    fn x402_config_sends_no_bearer_credential() {
        let config = PopcornConfig::x402("0x01");
        assert!(config.bearer().is_none());
    }
}
