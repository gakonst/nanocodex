//! Generic WebSocket attachment for one immutable [`Tools`] recipe.
//!
//! This boundary knows only a final WebSocket URL and bearer credential. Agent,
//! account, placement, and endpoint discovery remain the caller's responsibility.

mod driver;
mod observation;
mod protocol;
mod screen;
pub use observation::{
    MAX_OBSERVATION_IMAGE_BYTES, OBSERVATION_TIMEOUT, ObservationFrame, ObservationImageFormat,
    ObservationKind, ObservationProvider, ObservationSurface,
};
pub use screen::ScreenObservation;

use std::{collections::HashSet, fmt, sync::Arc};
use tokio::sync::{mpsc, watch};
use url::{Host, Url};

use crate::{
    Tools,
    prepared::{PreparedToolError, PreparedToolRuntime, PreparedTools},
};

const MAX_ATTACHMENT_ID_BYTES: usize = 123;
const MAX_MACHINE_NAME_BYTES: usize = 128;
const MAX_MACHINE_WORKSPACE_BYTES: usize = 1024;
const MAX_MACHINE_CAPABILITIES: usize = 64;
const MAX_MACHINE_CAPABILITY_BYTES: usize = 64;

/// Non-secret description of the one machine represented by an attachment.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct AttachmentMachine {
    id: Box<str>,
    name: Box<str>,
    workspace: Box<str>,
    capabilities: Box<[Box<str>]>,
}

impl AttachmentMachine {
    /// Creates a bounded machine descriptor accepted by the managed wire contract.
    ///
    /// # Errors
    ///
    /// Rejects unsafe identifiers, empty or oversized display fields, and
    /// duplicate or unsafe capability identifiers.
    pub fn new<I, S>(
        id: impl Into<String>,
        name: impl Into<String>,
        workspace: impl Into<String>,
        capabilities: I,
    ) -> Result<Self, AttachmentError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let id = id.into();
        if !valid_safe_identifier(&id, MAX_ATTACHMENT_ID_BYTES) {
            return Err(AttachmentError::Catalog(
                "attachment machine id must be a safe ASCII identifier of at most 123 bytes".into(),
            ));
        }
        let name = name.into();
        let name = name.trim();
        if name.is_empty() || name.len() > MAX_MACHINE_NAME_BYTES {
            return Err(AttachmentError::Catalog(
                "attachment machine name must be 1-128 UTF-8 bytes".into(),
            ));
        }
        let workspace = workspace.into();
        if workspace.trim().is_empty()
            || workspace.len() > MAX_MACHINE_WORKSPACE_BYTES
            || workspace.contains('\0')
        {
            return Err(AttachmentError::Catalog(
                "attachment machine workspace must be 1-1024 UTF-8 bytes without NUL".into(),
            ));
        }
        let capabilities = capabilities
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();
        if capabilities.len() > MAX_MACHINE_CAPABILITIES {
            return Err(AttachmentError::Catalog(
                "attachment machines support at most 64 capabilities".into(),
            ));
        }
        let mut unique = HashSet::with_capacity(capabilities.len());
        if capabilities
            .iter()
            .any(|capability| !valid_capability(capability) || !unique.insert(capability.as_str()))
        {
            return Err(AttachmentError::Catalog(
                "attachment machine capabilities must be unique safe ASCII identifiers".into(),
            ));
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

    /// Stable process-owned machine identifier.
    #[must_use]
    pub const fn id(&self) -> &str {
        &self.id
    }

    /// Human-readable host display name.
    #[must_use]
    pub const fn name(&self) -> &str {
        &self.name
    }

    /// Workspace represented by this attachment.
    #[must_use]
    pub const fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Non-secret capabilities available on this machine.
    #[must_use]
    pub const fn capabilities(&self) -> &[Box<str>] {
        &self.capabilities
    }
}

/// Optional stable routing metadata published with every attachment catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentMetadata {
    attachment_id: Box<str>,
    machine: Option<AttachmentMachine>,
}

impl AttachmentMetadata {
    /// Creates a stable named attachment without machine topology.
    ///
    /// # Errors
    ///
    /// Rejects IDs outside the safe ASCII, 123-byte source-ID contract.
    pub fn named(attachment_id: impl Into<String>) -> Result<Self, AttachmentError> {
        let attachment_id = attachment_id.into();
        if !valid_safe_identifier(&attachment_id, MAX_ATTACHMENT_ID_BYTES) {
            return Err(AttachmentError::Catalog(
                "attachment id must be a safe ASCII identifier of at most 123 bytes".into(),
            ));
        }
        Ok(Self {
            attachment_id: attachment_id.into(),
            machine: None,
        })
    }

    /// Creates a machine attachment whose routing ID is the machine ID.
    #[must_use]
    pub fn machine(machine: AttachmentMachine) -> Self {
        Self {
            attachment_id: machine.id.clone(),
            machine: Some(machine),
        }
    }

    /// Stable source routing identifier.
    #[must_use]
    pub const fn attachment_id(&self) -> &str {
        &self.attachment_id
    }

    /// Machine topology, when this attachment represents a user machine.
    #[must_use]
    pub const fn attached_machine(&self) -> Option<&AttachmentMachine> {
        self.machine.as_ref()
    }
}

fn valid_safe_identifier(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_capability(value: &str) -> bool {
    value.len() <= MAX_MACHINE_CAPABILITY_BYTES
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b':' | b'-')
        })
}

/// Transport-only destination for an attached tool executor.
#[derive(Clone, Eq, PartialEq)]
pub struct AttachmentTarget {
    endpoint: Url,
    bearer: Arc<str>,
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
    metadata: Option<AttachmentMetadata>,
    observation: Option<Arc<dyn ObservationProvider>>,
}

impl Tools {
    /// Binds this exact immutable recipe to a transport target.
    #[must_use]
    pub const fn attach(self, target: AttachmentTarget) -> AttachmentConnector {
        AttachmentConnector {
            tools: self,
            target,
            metadata: None,
            observation: None,
        }
    }
}

impl AttachmentConnector {
    /// Publishes stable non-secret routing metadata with every catalog reconnect.
    #[must_use]
    pub fn metadata(mut self, metadata: AttachmentMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Enables opt-in live viewing for this exact machine attachment.
    #[must_use]
    pub fn observation(mut self, provider: Arc<dyn ObservationProvider>) -> Self {
        self.observation = Some(provider);
        self
    }

    /// Validates the recipe and starts its complete lifecycle in the background.
    ///
    /// # Errors
    ///
    /// Fails synchronously when the selected tools cannot produce an immutable
    /// attached catalog. Discovery and transport failures are reported through
    /// the returned handle while its driver initializes and reconnects.
    pub fn start(self) -> Result<(Attachment, AttachmentEvents), AttachmentError> {
        let observation_surfaces = match &self.observation {
            None => Vec::new(),
            Some(provider) => {
                let surfaces = provider.surfaces();
                let unique: HashSet<_> = surfaces.iter().map(ObservationSurface::id).collect();
                if self
                    .metadata
                    .as_ref()
                    .and_then(AttachmentMetadata::attached_machine)
                    .is_none()
                    || surfaces.is_empty()
                    || surfaces.len() > 8
                    || unique.len() != surfaces.len()
                {
                    return Err(AttachmentError::Catalog(
                        "observation requires one machine and 1-8 unique surfaces".into(),
                    ));
                }
                surfaces.to_vec()
            }
        };
        install_default_rustls_crypto_provider();
        let prepared = PreparedTools::prepare(&self.tools)?;
        let (command_tx, command_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::channel(128);
        let (status_tx, status_rx) = watch::channel(AttachmentStatus::Connecting);
        let (closed_tx, closed_rx) = watch::channel(None);
        let refs = Arc::new(HandleRefs { command_tx });
        tokio::spawn(initialize_and_run(
            prepared,
            self.target,
            self.metadata,
            self.observation,
            observation_surfaces,
            command_rx,
            event_tx,
            status_tx,
            closed_tx,
        ));
        Ok((
            Attachment {
                refs,
                status: status_rx,
                closed: closed_rx,
            },
            AttachmentEvents { events: event_rx },
        ))
    }

    /// Prepares the exact catalog, connects, publishes it, and waits for its acknowledgement.
    ///
    /// # Errors
    ///
    /// Fails for non-attachable selections, discovery failures, invalid protocol
    /// frames, or endpoint rejection before readiness. Transient transport
    /// failures reconnect in the background.
    pub async fn connect(self) -> Result<(Attachment, AttachmentEvents), AttachmentError> {
        let (attachment, events) = self.start()?;
        attachment.wait_until_ready().await?;
        Ok((attachment, events))
    }
}

async fn initialize_and_run(
    prepared: PreparedTools,
    target: AttachmentTarget,
    metadata: Option<AttachmentMetadata>,
    observation: Option<Arc<dyn ObservationProvider>>,
    observation_surfaces: Vec<ObservationSurface>,
    mut command_rx: mpsc::Receiver<driver::Command>,
    event_tx: mpsc::Sender<AttachmentEvent>,
    status_tx: watch::Sender<AttachmentStatus>,
    closed_tx: watch::Sender<Option<Result<(), AttachmentError>>>,
) {
    let runtime = tokio::select! {
        biased;
        command = command_rx.recv() => {
            debug_assert!(matches!(command, Some(driver::Command::Detach) | None));
            let _ = closed_tx.send(Some(Ok(())));
            return;
        }
        initialized = PreparedToolRuntime::initialize(prepared) => match initialized {
            Ok(runtime) => Arc::new(runtime),
            Err(error) => {
                let _ = closed_tx.send(Some(Err(error.into())));
                return;
            }
        },
    };

    let config = (|| {
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
        Ok::<_, AttachmentError>(driver::Config {
            endpoint: target.endpoint,
            authorization: format!("Bearer {}", target.bearer).into(),
            tools,
            metadata,
            observation,
            observation_surfaces,
        })
    })();
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            runtime.shutdown().await;
            let _ = closed_tx.send(Some(Err(error)));
            return;
        }
    };
    driver::run(config, runtime, command_rx, event_tx, status_tx, closed_tx).await;
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

    /// Waits for catalog acknowledgement while the caller retains cancellation ownership.
    ///
    /// # Errors
    /// Returns the terminal initialization or transport error if readiness fails.
    pub async fn wait_until_ready(&self) -> Result<(), AttachmentError> {
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
    use super::AttachmentTarget;

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
}
