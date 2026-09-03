//! Generic WebSocket attachment for one immutable [`Tools`] recipe.
//!
//! This boundary knows only a final WebSocket URL and bearer credential. Agent,
//! account, placement, and endpoint discovery remain the caller's responsibility.

mod driver;
mod protocol;

use serde::Serialize;
use std::{collections::HashSet, fmt, sync::Arc};
use tokio::sync::{mpsc, watch};
use url::{Host, Url};

use crate::{
    Tools,
    prepared::{PreparedToolError, PreparedToolRuntime, PreparedTools},
};

/// Transport-only destination for an attached tool executor.
#[derive(Clone)]
pub struct AttachmentTarget {
    endpoint: Url,
    bearer: Arc<str>,
}

const MAX_MACHINES: usize = 32;
const MAX_MACHINE_CAPABILITIES: usize = 64;

/// Non-secret description of one machine served by an attached tool host.
///
/// The managed brain uses this immutable snapshot for placement only. The
/// descriptor grants no authority and carries no endpoint or credential.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AttachmentMachine {
    id: Box<str>,
    name: Box<str>,
    workspace: Box<str>,
    capabilities: Box<[Box<str>]>,
}

impl AttachmentMachine {
    /// Creates one validated machine descriptor.
    ///
    /// # Errors
    ///
    /// Rejects unsafe identifiers, empty or oversized display fields, more
    /// than 64 capabilities, and duplicate or malformed capabilities.
    pub fn new<I, C>(
        id: impl Into<String>,
        name: impl Into<String>,
        workspace: impl Into<String>,
        capabilities: I,
    ) -> Result<Self, AttachmentError>
    where
        I: IntoIterator<Item = C>,
        C: Into<String>,
    {
        let id = id.into();
        let name = name.into();
        let workspace = workspace.into();
        if !valid_identifier(&id) {
            return Err(machine_error("machine id must be a safe identifier"));
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 128 {
            return Err(machine_error("machine name must be 1-128 UTF-8 bytes"));
        }
        if workspace.trim().is_empty() || workspace.len() > 1_024 || workspace.contains('\0') {
            return Err(machine_error(
                "machine workspace must be 1-1024 UTF-8 bytes without NUL",
            ));
        }
        let capabilities = capabilities.into_iter().map(Into::into).collect::<Vec<_>>();
        if capabilities.len() > MAX_MACHINE_CAPABILITIES {
            return Err(machine_error(
                "a machine may publish at most 64 capabilities",
            ));
        }
        let mut unique = HashSet::with_capacity(capabilities.len());
        for capability in &capabilities {
            if !valid_capability(capability) {
                return Err(machine_error(
                    "machine capabilities must be safe lowercase identifiers",
                ));
            }
            if !unique.insert(capability.as_str()) {
                return Err(machine_error("machine capabilities must be unique"));
            }
        }
        Ok(Self {
            id: id.into(),
            name: name.into(),
            workspace: workspace.into(),
            capabilities: capabilities
                .into_iter()
                .map(String::into_boxed_str)
                .collect(),
        })
    }

    /// Returns the attachment-local stable machine identifier.
    #[must_use]
    pub const fn id(&self) -> &str {
        &self.id
    }

    /// Returns the human-readable machine name.
    #[must_use]
    pub const fn name(&self) -> &str {
        &self.name
    }

    /// Returns the independent workspace path visible inside the machine.
    #[must_use]
    pub const fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Returns the placement capabilities advertised by the machine.
    #[must_use]
    pub const fn capabilities(&self) -> &[Box<str>] {
        &self.capabilities
    }
}

impl AttachmentTarget {
    /// # Errors
    ///
    /// Rejects non-WebSocket URLs, embedded credentials, fragments, and empty
    /// bearer values.
    pub fn new(
        endpoint: impl AsRef<str>,
        bearer: impl Into<String>,
    ) -> Result<Self, AttachmentError> {
        let endpoint = Url::parse(endpoint.as_ref())
            .map_err(|error| AttachmentError::Transport(error.to_string().into()))?;
        if !matches!(endpoint.scheme(), "ws" | "wss")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(AttachmentError::Transport(
                "attachment target must be a final ws/wss URL without credentials or a fragment"
                    .into(),
            ));
        }
        if endpoint.scheme() == "ws" && !is_literal_loopback(&endpoint) {
            return Err(AttachmentError::Transport(
                "plaintext attachment targets require a literal loopback host; use wss otherwise"
                    .into(),
            ));
        }
        let bearer = bearer.into();
        if bearer.trim().is_empty() {
            return Err(AttachmentError::Authentication(
                "bearer credential must not be empty".into(),
            ));
        }
        Ok(Self {
            endpoint,
            bearer: bearer.into(),
        })
    }

    /// Returns the final WebSocket endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

impl fmt::Debug for AttachmentTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentTarget")
            .field("endpoint", &self.endpoint)
            .field("bearer", &"[REDACTED]")
            .finish()
    }
}

/// Prepared connection from one complete [`Tools`] recipe.
pub struct AttachmentConnector {
    tools: Tools,
    target: AttachmentTarget,
    machines: Vec<AttachmentMachine>,
}

impl Tools {
    /// Binds this exact immutable recipe to a transport target.
    #[must_use]
    pub const fn attach(self, target: AttachmentTarget) -> AttachmentConnector {
        AttachmentConnector {
            tools: self,
            target,
            machines: Vec::new(),
        }
    }
}

impl AttachmentConnector {
    /// Publishes the complete immutable machine snapshot behind this host.
    ///
    /// # Errors
    ///
    /// Rejects snapshots with more than 32 machines or duplicate identifiers.
    pub fn machines(
        mut self,
        machines: impl IntoIterator<Item = AttachmentMachine>,
    ) -> Result<Self, AttachmentError> {
        let machines = machines.into_iter().collect::<Vec<_>>();
        if machines.len() > MAX_MACHINES {
            return Err(machine_error(
                "a tool attachment may publish at most 32 machines",
            ));
        }
        let mut ids = HashSet::with_capacity(machines.len());
        for machine in &machines {
            if !ids.insert(machine.id()) {
                return Err(machine_error("attached machine ids must be unique"));
            }
        }
        self.machines = machines;
        Ok(self)
    }

    /// Prepares the exact catalog, connects, publishes it, and waits for its acknowledgement.
    ///
    /// # Errors
    ///
    /// Fails for non-attachable selections, discovery failures, transport
    /// failures, invalid protocol frames, or endpoint rejection before readiness.
    pub async fn connect(self) -> Result<(Attachment, AttachmentEvents), AttachmentError> {
        install_default_rustls_crypto_provider();
        let prepared = PreparedTools::prepare(&self.tools)?;
        let runtime = Arc::new(PreparedToolRuntime::initialize(prepared).await?);
        let catalog = runtime.catalog()?;
        let tools = serde_json::to_value(catalog)
            .map_err(|error| AttachmentError::Catalog(error.to_string().into()))?;
        let names = tools
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry
                            .pointer("/definition/name")
                            .and_then(serde_json::Value::as_str)
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| AttachmentError::Catalog("catalog tool name is missing".into()))?;
        crate::selection::validate_public_tool_catalog_names(names)
            .map_err(|error| AttachmentError::Catalog(error.to_string().into()))?;
        let config = driver::Config {
            endpoint: self.target.endpoint,
            authorization: format!("Bearer {}", self.target.bearer).into(),
            tools,
            machines: self.machines.into_boxed_slice(),
        };
        let (command_tx, command_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::channel(128);
        let (status_tx, status_rx) = watch::channel(AttachmentStatus::Connecting);
        let (closed_tx, closed_rx) = watch::channel(None);
        let refs = Arc::new(HandleRefs { command_tx });
        tokio::spawn(driver::run(
            config, runtime, command_rx, event_tx, status_tx, closed_tx,
        ));
        let attachment = Attachment {
            refs,
            status: status_rx,
            closed: closed_rx,
        };
        attachment.wait_until_ready().await?;
        Ok((attachment, AttachmentEvents { events: event_rx }))
    }
}

fn machine_error(message: &'static str) -> AttachmentError {
    AttachmentError::Catalog(message.into())
}

fn valid_identifier(value: &str) -> bool {
    value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_capability(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b':' | b'-')
        })
}

fn is_literal_loopback(endpoint: &Url) -> bool {
    match endpoint.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn install_default_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

struct HandleRefs {
    command_tx: mpsc::Sender<driver::Command>,
}

impl Drop for HandleRefs {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(driver::Command::Detach);
    }
}

/// Cheap live attachment handle.
///
/// Clones share one attachment. Dropping the last handle detaches the executor;
/// dropping its independent [`AttachmentEvents`] observer does not.
#[derive(Clone)]
pub struct Attachment {
    refs: Arc<HandleRefs>,
    status: watch::Receiver<AttachmentStatus>,
    closed: watch::Receiver<Option<Result<(), AttachmentError>>>,
}

impl fmt::Debug for Attachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Attachment")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Attachment {
    /// Returns the latest observed lifecycle state.
    #[must_use]
    pub fn status(&self) -> AttachmentStatus {
        self.status.borrow().clone()
    }

    /// Explicitly detaches and waits for terminal cleanup.
    pub async fn detach(self) -> Result<(), AttachmentError> {
        let _ = self.refs.command_tx.try_send(driver::Command::Detach);
        self.closed().await
    }

    /// Waits for authoritative terminal closure.
    pub async fn closed(&self) -> Result<(), AttachmentError> {
        let mut closed = self.closed.clone();
        loop {
            if let Some(result) = closed.borrow().clone() {
                return result;
            }
            if closed.changed().await.is_err() {
                return closed
                    .borrow()
                    .clone()
                    .unwrap_or(Err(AttachmentError::Closed));
            }
        }
    }

    async fn wait_until_ready(&self) -> Result<(), AttachmentError> {
        let mut status = self.status.clone();
        loop {
            let current = status.borrow().clone();
            match current {
                AttachmentStatus::Ready => return Ok(()),
                AttachmentStatus::Fenced => return self.closed().await,
                _ => {}
            }
            tokio::select! {
                changed = status.changed() => if changed.is_err() { return self.closed().await },
                result = self.closed() => return result,
            }
        }
    }
}

/// Best-effort ordered observations from one attachment.
///
/// Observation never applies backpressure to execution or protocol progress.
/// When this bounded stream lags, events may be dropped. Use [`Attachment::status`]
/// and [`Attachment::closed`] for authoritative lifecycle state.
pub struct AttachmentEvents {
    events: mpsc::Receiver<AttachmentEvent>,
}

impl fmt::Debug for AttachmentEvents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentEvents")
            .finish_non_exhaustive()
    }
}

impl AttachmentEvents {
    /// Receives the next available observation.
    pub async fn recv(&mut self) -> Option<AttachmentEvent> {
        self.events.recv().await
    }
}

/// Latest attachment connection state.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AttachmentStatus {
    /// A connection or reconnect is in progress.
    Connecting,
    /// The exact catalog was acknowledged.
    Ready,
    /// The transport is temporarily disconnected.
    Disconnected,
    /// The remote endpoint authoritatively rejected this socket.
    Fenced,
}

/// Best-effort ordered lifecycle and call observation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AttachmentEvent {
    /// A connection or reconnect attempt started.
    Connecting,
    /// The endpoint accepted the socket and immutable catalog.
    Attached,
    /// The immutable catalog was acknowledged.
    CatalogPublished {
        /// Number of entries in the exact catalog.
        tool_count: usize,
    },
    /// One pinned invocation was admitted.
    CallStarted {
        /// Remote call identity.
        call_id: Box<str>,
        /// Exact catalog name.
        name: Box<str>,
    },
    /// One admitted invocation reached a transport outcome.
    CallCompleted {
        /// Remote call identity.
        call_id: Box<str>,
        /// Conservative transport classification.
        outcome: AttachmentCallOutcome,
    },
    /// The executor detached normally.
    Detached {
        /// Human-readable terminal reason.
        reason: Box<str>,
    },
    /// The endpoint authoritatively rejected this executor.
    Fenced {
        /// Human-readable protocol or lease violation.
        reason: Box<str>,
    },
}

/// Transport-level terminal classification for one invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AttachmentCallOutcome {
    /// A complete tool output was published.
    Completed,
    /// Execution was not admitted or the tool was unavailable.
    Unavailable,
    /// Side effects may have happened without a publishable exact result.
    Ambiguous,
    /// Execution was cancelled before side effects were admitted.
    Cancelled,
}

/// Typed attachment preparation, transport, and protocol failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum AttachmentError {
    /// The credential was malformed or rejected.
    #[error("attachment authentication failed: {0}")]
    Authentication(Box<str>),
    /// The recipe could not produce an exact immutable catalog.
    #[error("attached catalog failed: {0}")]
    Catalog(Box<str>),
    /// The remote endpoint rejected this socket or its protocol frames.
    #[error("attachment was fenced: {0}")]
    Fenced(Box<str>),
    /// The WebSocket connection or frame exchange failed.
    #[error("attachment transport failed: {0}")]
    Transport(Box<str>),
    /// The attachment terminated before the awaited operation completed.
    #[error("attachment is closed")]
    Closed,
}

impl From<PreparedToolError> for AttachmentError {
    fn from(error: PreparedToolError) -> Self {
        Self::Catalog(error.to_string().into())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod target_tests {
    use super::{AttachmentMachine, AttachmentTarget};
    use crate::Tools;

    #[test]
    fn plaintext_targets_require_literal_loopback_hosts() {
        for endpoint in [
            "ws://localhost/tools",
            "ws://127.0.0.1/tools",
            "ws://127.255.255.254/tools",
            "ws://[::1]/tools",
        ] {
            assert!(
                AttachmentTarget::new(endpoint, "secret").is_ok(),
                "{endpoint}"
            );
        }

        for endpoint in [
            "ws://example.com/tools",
            "ws://localhost.example/tools",
            "ws://[::ffff:127.0.0.1]/tools",
            "ws://192.168.1.10/tools",
        ] {
            assert!(
                AttachmentTarget::new(endpoint, "secret").is_err(),
                "{endpoint}"
            );
        }

        assert!(AttachmentTarget::new("wss://example.com/tools", "secret").is_ok());
    }

    #[test]
    fn machine_descriptors_match_the_hosted_protocol_bounds() {
        let machine = AttachmentMachine::new(
            "vm-1",
            "  Build VM  ",
            "/workspace",
            ["cpu:8", "filesystem", "memory-mib:16384", "vm"],
        )
        .unwrap();
        assert_eq!(machine.id(), "vm-1");
        assert_eq!(machine.name(), "Build VM");
        assert_eq!(machine.workspace(), "/workspace");
        assert_eq!(machine.capabilities().len(), 4);

        assert!(AttachmentMachine::new("bad id", "VM", "/workspace", ["vm"]).is_err());
        assert!(AttachmentMachine::new("vm", "VM", "", ["vm"]).is_err());
        assert!(AttachmentMachine::new("vm", "VM", "/workspace", ["GPU"]).is_err());
        assert!(AttachmentMachine::new("vm", "VM", "/workspace", ["vm", "vm"]).is_err());

        let target = AttachmentTarget::new("ws://127.0.0.1/tools", "secret").unwrap();
        let tools = Tools::builder().without_defaults().build().unwrap();
        assert!(
            tools
                .attach(target)
                .machines([machine.clone(), machine])
                .is_err()
        );
    }
}
