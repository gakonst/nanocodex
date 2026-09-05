//! Portable embedding-host boundary for the standard Responses state machine.
//!
//! This module contains no browser, Node, or `wasm-bindgen` types. An embedding
//! implements [`HostTransport`](crate::transport::host::HostTransport) and
//! [`HostConnection`](crate::transport::host::HostConnection), then installs
//! that transport with `OpenAiBuilder::host_transport` on WebAssembly. The
//! standard client continues to own request encoding, retries, continuation
//! state, streamed event decoding, and Tower semantics.

use std::{future::Future, pin::Pin, time::Duration};

#[cfg(target_family = "wasm")]
pub(crate) mod socket;

/// Boxed future returned by a native embedding host.
#[cfg(not(target_family = "wasm"))]
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Boxed future returned by a single-threaded WebAssembly embedding host.
#[cfg(target_family = "wasm")]
pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Factory for persistent host-owned Responses connections.
///
/// Implementations translate their environment's socket and timer APIs into
/// Rust values. They must not parse Responses events or implement retries;
/// those policies remain in `nanocodex-oai-api`.
pub trait HostTransport: Send + Sync + 'static {
    /// Opens one persistent Responses connection.
    ///
    /// The returned connection is owned by one managed session until it closes
    /// or the SDK replaces it during recovery.
    fn connect<'a>(
        &'a self,
        request: HostConnectRequest<'a>,
    ) -> HostFuture<'a, Result<ConnectedHost, HostError>>;

    /// Waits without blocking the embedding thread.
    ///
    /// The SDK calls this only for its typed retry backoff.
    fn sleep<'a>(&'a self, session_id: &'a str, duration: Duration) -> HostFuture<'a, ()>;
}

/// One persistent connection created by a [`HostTransport`].
pub trait HostConnection: Send + 'static {
    /// Sends one complete text frame.
    fn send<'a>(&'a self, message: &'a str) -> HostFuture<'a, Result<(), HostError>>;

    /// Waits for the next data, closure, or timeout result.
    fn next(&mut self, idle_timeout: Duration) -> HostFuture<'_, Result<HostMessage, HostError>>;

    /// Releases the environment's connection handle synchronously.
    fn close(&mut self);
}

/// Immutable inputs for one host connection attempt.
///
/// `Debug` is intentionally not implemented because the value contains the
/// resolved bearer credential.
#[derive(Clone, Copy)]
pub struct HostConnectRequest<'a> {
    endpoint: &'a str,
    bearer_token: &'a str,
    account_id: Option<&'a str>,
    fedramp: bool,
    session_id: &'a str,
    thread_id: &'a str,
    turn_state: Option<&'a str>,
}

impl<'a> HostConnectRequest<'a> {
    /// Creates the immutable inputs for one host connection attempt.
    ///
    /// Most implementations only inspect requests received through
    /// [`HostTransport::connect`]. This constructor is useful when exercising
    /// a host transport independently from the managed client.
    #[must_use]
    pub const fn new(
        endpoint: &'a str,
        bearer_token: &'a str,
        account_id: Option<&'a str>,
        fedramp: bool,
        session_id: &'a str,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self::new_with_thread_id(
            endpoint,
            bearer_token,
            account_id,
            fedramp,
            session_id,
            session_id,
            turn_state,
        )
    }

    pub(crate) const fn new_with_thread_id(
        endpoint: &'a str,
        bearer_token: &'a str,
        account_id: Option<&'a str>,
        fedramp: bool,
        session_id: &'a str,
        thread_id: &'a str,
        turn_state: Option<&'a str>,
    ) -> Self {
        Self {
            endpoint,
            bearer_token,
            account_id,
            fedramp,
            session_id,
            thread_id,
            turn_state,
        }
    }

    /// Returns the complete Responses WebSocket endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &'a str {
        self.endpoint
    }

    /// Returns the resolved bearer credential for this connection attempt.
    ///
    /// Hosts must treat this value as a secret and must not retain it beyond
    /// the handshake.
    #[must_use]
    pub const fn bearer_token(&self) -> &'a str {
        self.bearer_token
    }

    /// Returns the `ChatGPT` account ID when authentication is account-scoped.
    #[must_use]
    pub const fn account_id(&self) -> Option<&'a str> {
        self.account_id
    }

    /// Returns whether the credential targets a `FedRAMP` environment.
    #[must_use]
    pub const fn is_fedramp(&self) -> bool {
        self.fedramp
    }

    /// Returns the stable session identity sent on the handshake.
    #[must_use]
    pub const fn session_id(&self) -> &'a str {
        self.session_id
    }

    /// Returns the current agent thread identity sent on the handshake.
    #[must_use]
    pub const fn thread_id(&self) -> &'a str {
        self.thread_id
    }

    /// Returns opaque server turn state retained across a replacement socket.
    #[must_use]
    pub const fn turn_state(&self) -> Option<&'a str> {
        self.turn_state
    }
}

/// A successfully opened host connection and its handshake metadata.
pub struct ConnectedHost {
    pub(crate) connection: Box<dyn HostConnection>,
    pub(crate) metadata: HostConnectionMetadata,
}

impl ConnectedHost {
    /// Combines a host-owned connection with its handshake metadata.
    #[must_use]
    pub fn new(connection: impl HostConnection, metadata: HostConnectionMetadata) -> Self {
        Self {
            connection: Box::new(connection),
            metadata,
        }
    }

    /// Separates the host-owned connection from its handshake metadata.
    #[must_use]
    pub fn into_parts(self) -> (Box<dyn HostConnection>, HostConnectionMetadata) {
        (self.connection, self.metadata)
    }
}

/// Handshake metadata observed by an embedding host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostConnectionMetadata {
    pub(crate) status: u16,
    pub(crate) request_id: Option<String>,
    pub(crate) server_model: Option<String>,
    pub(crate) reasoning_included: bool,
    pub(crate) turn_state: Option<String>,
}

impl HostConnectionMetadata {
    /// Starts metadata with the handshake's HTTP status.
    #[must_use]
    pub const fn new(status: u16) -> Self {
        Self {
            status,
            request_id: None,
            server_model: None,
            reasoning_included: false,
            turn_state: None,
        }
    }

    /// Records the provider request identifier.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Records the model selected by the server.
    #[must_use]
    pub fn with_server_model(mut self, server_model: impl Into<String>) -> Self {
        self.server_model = Some(server_model.into());
        self
    }

    /// Records whether the server included reasoning data.
    #[must_use]
    pub const fn with_reasoning_included(mut self, included: bool) -> Self {
        self.reasoning_included = included;
        self
    }

    /// Records opaque server turn state for a replacement connection.
    #[must_use]
    pub fn with_turn_state(mut self, turn_state: impl Into<String>) -> Self {
        self.turn_state = Some(turn_state.into());
        self
    }

    /// Returns the handshake's HTTP status.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Returns the provider request identifier, when present.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns the model selected by the server, when present.
    #[must_use]
    pub fn server_model(&self) -> Option<&str> {
        self.server_model.as_deref()
    }

    /// Returns whether the server included reasoning data.
    #[must_use]
    pub const fn reasoning_included(&self) -> bool {
        self.reasoning_included
    }

    /// Returns opaque server turn state, when present.
    #[must_use]
    pub fn turn_state(&self) -> Option<&str> {
        self.turn_state.as_deref()
    }
}

/// One inbound result from a host-owned Responses connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostMessage {
    /// One complete text frame containing a Responses event.
    Text(String),
    /// The peer closed the connection with retained detail.
    Closed {
        /// Close code and reason formatted by the host.
        detail: String,
    },
    /// The idle deadline elapsed without an event.
    Timeout,
    /// A binary frame arrived where the protocol requires JSON text.
    Binary,
}

/// Failure reported by an embedding host.
#[derive(Clone, Debug, thiserror::Error)]
pub enum HostError {
    /// A socket or environment operation failed.
    #[error("{detail}")]
    Transport {
        /// Complete platform failure detail.
        detail: String,
        /// Whether replacing the connection may safely recover.
        reconnectable: bool,
    },
    /// The provider rejected the WebSocket handshake.
    #[error("WebSocket handshake was rejected with HTTP {status}: {body}")]
    HandshakeRejected {
        /// HTTP response status.
        status: u16,
        /// Complete retained response body.
        body: String,
        /// Provider-requested minimum retry delay.
        retry_after: Option<Duration>,
    },
}

impl HostError {
    /// Creates a terminal host failure with complete retained detail.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self::Transport {
            detail: detail.into(),
            reconnectable: false,
        }
    }

    /// Creates a structured provider handshake rejection.
    #[must_use]
    pub fn handshake_rejected(
        status: u16,
        body: impl Into<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        Self::HandshakeRejected {
            status,
            body: body.into(),
            retry_after,
        }
    }

    /// Marks whether replacing the connection may safely recover this failure.
    #[must_use]
    pub fn with_reconnectable(self, reconnectable: bool) -> Self {
        match self {
            Self::Transport { detail, .. } => Self::Transport {
                detail,
                reconnectable,
            },
            rejected @ Self::HandshakeRejected { .. } => rejected,
        }
    }

    /// Returns the complete host failure detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Transport { detail, .. } => detail,
            Self::HandshakeRejected { body, .. } => body,
        }
    }

    /// Returns whether replacing the connection may safely recover.
    #[must_use]
    pub const fn is_reconnectable(&self) -> bool {
        matches!(
            self,
            Self::Transport {
                reconnectable: true,
                ..
            }
        )
    }
}
