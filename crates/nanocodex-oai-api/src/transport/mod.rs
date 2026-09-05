//! Responses transport policy, errors, and connection statistics.

mod api_error;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod connector;
#[cfg(not(target_family = "wasm"))]
mod crypto;
pub(crate) mod error;
/// Portable embedding-host interfaces for persistent Responses connections.
pub mod host;
#[cfg(target_family = "wasm")]
pub(crate) use host::socket;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod http;
pub(crate) mod platform;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod socket;
pub(crate) mod telemetry;
mod wire;

use std::{fmt, str::FromStr};

pub use crate::tower::attempt::{TransportStats, TransportStatsDelta, TransportStatsSnapshot};
#[cfg(not(target_family = "wasm"))]
pub use crypto::install_default_rustls_crypto_provider;
pub use error::{ResponsesError, RetryAdvice};
pub use telemetry::TRANSPORT;
pub use wire::EncodedRequest;

/// Initial Responses transport policy for a managed session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResponsesTransport {
    /// Prefer a persistent Responses WebSocket. On native targets, the
    /// standard stack falls back one-way to HTTPS after exhausting the
    /// WebSocket retry budget.
    #[default]
    WebSocket,
    /// Use HTTPS requests with server-sent event response bodies exclusively.
    Https,
}

impl ResponsesTransport {
    /// Returns the stable telemetry name for this transport.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebSocket => "responses_websocket_v2",
            Self::Https => "responses_https_sse",
        }
    }
}

impl fmt::Display for ResponsesTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebSocket => formatter.write_str("websocket"),
            Self::Https => formatter.write_str("https"),
        }
    }
}

impl FromStr for ResponsesTransport {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "websocket" | "ws" => Ok(Self::WebSocket),
            "https" | "http" => Ok(Self::Https),
            _ => Err(format!(
                "invalid Responses transport {value:?}; expected websocket or https"
            )),
        }
    }
}

/// How committed client history is represented in each Responses request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResponsesHistory {
    /// Reuse a response ID when the selected transport and storage policy make
    /// it valid, falling back to complete client-owned history as needed.
    #[default]
    Incremental,
    /// Send complete committed client-owned history on every request.
    FullReplay,
}

impl fmt::Display for ResponsesHistory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incremental => formatter.write_str("incremental"),
            Self::FullReplay => formatter.write_str("full-replay"),
        }
    }
}

impl FromStr for ResponsesHistory {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "incremental" => Ok(Self::Incremental),
            "full-replay" | "replay" => Ok(Self::FullReplay),
            _ => Err(format!(
                "invalid Responses history policy {value:?}; expected incremental or full-replay"
            )),
        }
    }
}
