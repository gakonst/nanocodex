use std::{collections::HashMap, fmt, time::Duration};

use futures_util::{SinkExt as _, StreamExt as _};
use nanocodex_tools::attachment::AttachmentTarget;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        Error as WebSocketError, Message, client::IntoClientRequest as _, protocol::WebSocketConfig,
    },
};
use url::Url;
use uuid::{Uuid, Variant, Version};
use zeroize::Zeroize as _;

use crate::{ManagedClient, ManagedError, client::validate_origin};

const PROTOCOL_VERSION: u8 = 1;
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_VMS: u16 = 64;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_MACHINE_ID_BYTES: usize = 123;
const MAX_FACTORY_NAME_BYTES: usize = 63;
const MAX_NONCE_BYTES: usize = 128;
const MAX_REASON_BYTES: usize = 1024;
const ATTACHMENT_BEARER_BYTES: usize = 43;
const MAX_ATTACHMENT_URL_BYTES: usize = 2048;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Authority scope used to lease allocations from a managed VM-host endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmHostScope {
    /// Any hosted agent owned by the authenticated account may allocate a VM.
    User,
    /// Only the named account-owned agent may allocate a VM.
    Agent(String),
    /// System scheduling authority authenticated by a separate host token.
    System,
}

/// Fixed resources assigned to every VM advertised by one host lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct VmShape {
    cpus: u8,
    memory_mib: u32,
}

impl VmShape {
    /// Creates a protocol-valid fixed VM shape.
    ///
    /// # Errors
    ///
    /// Rejects CPU counts outside 1-64 and memory outside 128-262144 MiB.
    pub fn new(cpus: u8, memory_mib: u32) -> Result<Self, ManagedError> {
        let shape = Self { cpus, memory_mib };
        shape.validate()?;
        Ok(shape)
    }

    /// Returns the virtual CPU count assigned to every VM.
    #[must_use]
    pub const fn cpus(self) -> u8 {
        self.cpus
    }

    /// Returns the memory assigned to every VM in mebibytes.
    #[must_use]
    pub const fn memory_mib(self) -> u32 {
        self.memory_mib
    }

    fn validate(self) -> Result<(), ManagedError> {
        if !(1..=64).contains(&self.cpus) {
            return Err(configuration("VM host CPU count must be 1-64"));
        }
        if !(128..=262_144).contains(&self.memory_mib) {
            return Err(configuration("VM host memory must be 128-262144 MiB"));
        }
        Ok(())
    }
}

/// One live allocation reported while reconciling a replacement host lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmHostAllocationState {
    allocation_id: Uuid,
    generation: u64,
    machine_id: String,
}

impl VmHostAllocationState {
    /// Creates one ready allocation reconciliation record.
    ///
    /// # Errors
    ///
    /// Rejects a zero/unsafe generation or invalid machine identifier.
    pub fn ready(
        allocation_id: Uuid,
        generation: u64,
        machine_id: impl Into<String>,
    ) -> Result<Self, ManagedError> {
        require_v4_uuid("allocation", allocation_id)?;
        validate_generation(generation)?;
        let machine_id = machine_id.into();
        validate_machine_id(&machine_id)?;
        Ok(Self {
            allocation_id,
            generation,
            machine_id,
        })
    }

    /// Returns the durable allocation identifier.
    #[must_use]
    pub const fn allocation_id(&self) -> Uuid {
        self.allocation_id
    }

    /// Returns the allocation generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the stable machine identifier.
    #[must_use]
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
}

/// A managed request to create one private VM and attach its tools.
#[derive(Clone)]
pub struct VmHostProvision {
    allocation_id: Uuid,
    generation: u64,
    slot: u16,
    machine_id: String,
    attachment: AttachmentTarget,
}

impl fmt::Debug for VmHostProvision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmHostProvision")
            .field("allocation_id", &self.allocation_id)
            .field("generation", &self.generation)
            .field("slot", &self.slot)
            .field("machine_id", &self.machine_id)
            .field("attachment", &self.attachment)
            .finish()
    }
}

impl VmHostProvision {
    /// Returns the durable allocation identifier.
    #[must_use]
    pub const fn allocation_id(&self) -> Uuid {
        self.allocation_id
    }

    /// Returns the allocation generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the host-local bounded slot.
    #[must_use]
    pub const fn slot(&self) -> u16 {
        self.slot
    }

    /// Returns the stable machine identifier advertised by the leaf attachment.
    #[must_use]
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// Returns the redacting leaf Hosted Tools attachment target.
    #[must_use]
    pub const fn attachment(&self) -> &AttachmentTarget {
        &self.attachment
    }

    /// Consumes the command and returns its leaf Hosted Tools target.
    #[must_use]
    pub fn into_attachment(self) -> AttachmentTarget {
        self.attachment
    }
}

/// A managed request to drain and stop one allocated VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmHostRelease {
    allocation_id: Uuid,
    generation: u64,
    machine_id: String,
}

impl VmHostRelease {
    /// Returns the durable allocation identifier.
    #[must_use]
    pub const fn allocation_id(&self) -> Uuid {
        self.allocation_id
    }

    /// Returns the allocation generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the stable machine identifier.
    #[must_use]
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }
}

/// Terminal fencing notice for a superseded VM-host lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmHostFence {
    epoch: u64,
    reason: String,
}

impl VmHostFence {
    /// Returns the authoritative successor epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the bounded non-secret fencing reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// One strict, identity-checked server command.
#[derive(Clone, Debug)]
pub enum VmHostCommand {
    /// Provision one VM in the named host slot.
    Provision(VmHostProvision),
    /// Drain and release one VM.
    Release(VmHostRelease),
    /// Stop the host because its lease was superseded.
    Fenced(VmHostFence),
}

/// One authenticated v1 VM-host control WebSocket.
pub struct VmHostConnection {
    socket: Socket,
    scope: VmHostScope,
    host_id: Uuid,
    max_vms: u16,
    vm: VmShape,
    lease_id: Uuid,
    epoch: u64,
    expires_at: u64,
    reconciled: bool,
    pending_ping: Option<String>,
    allocations: HashMap<Uuid, KnownAllocation>,
    terminal: bool,
}

impl fmt::Debug for VmHostConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmHostConnection")
            .field("scope", &self.scope)
            .field("host_id", &self.host_id)
            .field("max_vms", &self.max_vms)
            .field("vm", &self.vm)
            .field("lease_id", &self.lease_id)
            .field("epoch", &self.epoch)
            .field("expires_at", &self.expires_at)
            .field("reconciled", &self.reconciled)
            .field("allocations", &self.allocations.len())
            .field("terminal", &self.terminal)
            .finish()
    }
}

impl ManagedClient {
    /// Connects an account- or agent-scoped VM host and acquires its first lease.
    ///
    /// System scope is deliberately rejected because it requires the separate
    /// raw system host token accepted by [`connect_system_vm_host`].
    ///
    /// # Errors
    ///
    /// Returns configuration, WebSocket, or strict v1 protocol failures.
    pub async fn connect_vm_host(
        &self,
        scope: VmHostScope,
        host_id: Uuid,
        factory_name: impl Into<String>,
        max_vms: u16,
        vm: VmShape,
    ) -> Result<VmHostConnection, ManagedError> {
        let path = match &scope {
            VmHostScope::User => "/v1/account/vm-host".to_owned(),
            VmHostScope::Agent(agent_id) => {
                crate::client::validate_id("agent", agent_id)?;
                format!("/v1/agents/{agent_id}/vm-host")
            }
            VmHostScope::System => {
                return Err(configuration(
                    "system VM hosts require connect_system_vm_host and a system host token",
                ));
            }
        };
        connect_vm_host(
            self.base_url.clone(),
            path,
            self.bearer.to_string(),
            scope,
            host_id,
            factory_name.into(),
            max_vms,
            vm,
        )
        .await
    }
}

/// Connects a system-scoped VM host with its raw, separately issued host token.
///
/// The token is consumed, never parsed as an account API key, and never appears
/// in debug output or returned errors.
///
/// # Errors
///
/// Returns origin, token-header, WebSocket, or strict v1 protocol failures.
pub async fn connect_system_vm_host(
    origin: impl AsRef<str>,
    system_host_token: impl Into<String>,
    host_id: Uuid,
    factory_name: impl Into<String>,
    max_vms: u16,
    vm: VmShape,
) -> Result<VmHostConnection, ManagedError> {
    let origin =
        Url::parse(origin.as_ref()).map_err(|_| configuration("managed origin must be a URL"))?;
    validate_origin(&origin)?;
    connect_vm_host(
        origin,
        "/v1/system/vm-host".to_owned(),
        system_host_token.into(),
        VmHostScope::System,
        host_id,
        factory_name.into(),
        max_vms,
        vm,
    )
    .await
}

impl VmHostConnection {
    /// Returns the current lease identifier.
    #[must_use]
    pub const fn lease_id(&self) -> Uuid {
        self.lease_id
    }

    /// Returns the current fencing epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the current lease expiry as Unix epoch milliseconds.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Reconciles all ready allocations immediately after acquiring this lease.
    ///
    /// # Errors
    ///
    /// Rejects repeated reconciliation, over-capacity snapshots, duplicate
    /// identities, or transport failure.
    pub async fn reconcile(
        &mut self,
        allocations: &[VmHostAllocationState],
    ) -> Result<(), ManagedError> {
        if self.terminal {
            return Err(protocol("VM host connection is terminal"));
        }
        if self.reconciled {
            return Err(protocol("VM host lease was already reconciled"));
        }
        if allocations.len() > usize::from(self.max_vms) {
            return Err(configuration("VM host reconciliation exceeds max_vms"));
        }
        let mut known = HashMap::with_capacity(allocations.len());
        let mut machine_ids = HashMap::with_capacity(allocations.len());
        let mut wire = Vec::with_capacity(allocations.len());
        for allocation in allocations {
            require_v4_uuid("allocation", allocation.allocation_id)?;
            validate_generation(allocation.generation)?;
            validate_machine_id(&allocation.machine_id)?;
            if known
                .insert(
                    allocation.allocation_id,
                    KnownAllocation {
                        generation: allocation.generation,
                        machine_id: allocation.machine_id.clone(),
                        provision: None,
                        releasing: false,
                    },
                )
                .is_some()
                || machine_ids
                    .insert(allocation.machine_id.as_str(), ())
                    .is_some()
            {
                return Err(configuration(
                    "VM host reconciliation contains duplicate allocation or machine identity",
                ));
            }
            wire.push(ReconcileAllocation {
                allocation_id: allocation.allocation_id.to_string(),
                generation: allocation.generation,
                machine_id: &allocation.machine_id,
                state: ReadyState::Ready,
            });
        }
        self.send(&ClientMessage::Reconcile {
            lease_id: self.lease_id.to_string(),
            epoch: self.epoch,
            allocations: wire,
        })
        .await?;
        self.allocations = known;
        self.reconciled = true;
        Ok(())
    }

    /// Sends a lease heartbeat with an optional bounded nonce.
    ///
    /// # Errors
    ///
    /// Rejects invalid or overlapping nonces and transport failure.
    pub async fn ping(&mut self, nonce: Option<&str>) -> Result<(), ManagedError> {
        if self.terminal {
            return Err(protocol("VM host connection is terminal"));
        }
        if self.pending_ping.is_some() {
            return Err(protocol("VM host ping is already outstanding"));
        }
        if let Some(nonce) = nonce {
            validate_bounded_text("ping nonce", nonce, MAX_NONCE_BYTES)?;
        }
        self.send(&ClientMessage::Ping {
            lease_id: self.lease_id.to_string(),
            epoch: self.epoch,
            nonce,
        })
        .await?;
        self.pending_ping = Some(nonce.unwrap_or_default().to_owned());
        Ok(())
    }

    /// Reports that a provision command reached leaf attachment readiness.
    ///
    /// # Errors
    ///
    /// Rejects commands not owned by this live lease or transport failure.
    pub async fn provisioned(&mut self, provision: &VmHostProvision) -> Result<(), ManagedError> {
        self.require_known(
            provision.allocation_id,
            provision.generation,
            &provision.machine_id,
            false,
        )?;
        self.send(&ClientMessage::Provisioned {
            lease_id: self.lease_id.to_string(),
            epoch: self.epoch,
            allocation_id: provision.allocation_id.to_string(),
            generation: provision.generation,
            machine_id: &provision.machine_id,
        })
        .await
    }

    /// Reports that a release command completed leaf drain and VM shutdown.
    ///
    /// # Errors
    ///
    /// Rejects commands not owned by this live lease or transport failure.
    pub async fn released(&mut self, release: &VmHostRelease) -> Result<(), ManagedError> {
        self.require_known(
            release.allocation_id,
            release.generation,
            &release.machine_id,
            true,
        )?;
        self.send(&ClientMessage::Released {
            lease_id: self.lease_id.to_string(),
            epoch: self.epoch,
            allocation_id: release.allocation_id.to_string(),
            generation: release.generation,
            machine_id: &release.machine_id,
        })
        .await?;
        self.allocations.remove(&release.allocation_id);
        Ok(())
    }

    /// Waits for the next provision, release, or terminal fencing command.
    ///
    /// Valid pong heartbeats are consumed internally. Every application frame
    /// is size-bounded, exact-key decoded, and pinned to this lease and epoch.
    ///
    /// # Errors
    ///
    /// Returns a transport or terminal strict-protocol failure.
    pub async fn next(&mut self) -> Result<Option<VmHostCommand>, ManagedError> {
        if self.terminal {
            return Ok(None);
        }
        loop {
            let Some(encoded) = read_text(&mut self.socket).await? else {
                self.terminal = true;
                return Ok(None);
            };
            let frame = decode_server_message(&encoded).map_err(|error| {
                self.terminal = true;
                error
            })?;
            match frame {
                ServerMessage::Lease(_) => {
                    return self.fail("VM host sent a second lease on one connection");
                }
                ServerMessage::Pong(pong) => {
                    self.validate_pins(&pong.lease_id, pong.epoch)?;
                    validate_timestamp(pong.expires_at)?;
                    let expected = self
                        .pending_ping
                        .take()
                        .ok_or_else(|| protocol("VM host sent an unsolicited pong"))?;
                    if pong.nonce.as_deref().unwrap_or_default() != expected {
                        return self.fail("VM host pong nonce does not match the outstanding ping");
                    }
                    self.expires_at = pong.expires_at;
                }
                ServerMessage::Provision(raw) => {
                    self.require_reconciled()?;
                    self.validate_pins(&raw.lease_id, raw.epoch)?;
                    let provision = self.accept_provision(raw)?;
                    return Ok(Some(VmHostCommand::Provision(provision)));
                }
                ServerMessage::Release(raw) => {
                    self.require_reconciled()?;
                    self.validate_pins(&raw.lease_id, raw.epoch)?;
                    let release = self.accept_release(raw)?;
                    return Ok(Some(VmHostCommand::Release(release)));
                }
                ServerMessage::Fenced(fenced) => {
                    validate_generation(fenced.epoch)?;
                    validate_bounded_text("fencing reason", &fenced.reason, MAX_REASON_BYTES)?;
                    if fenced.epoch < self.epoch {
                        return self.fail("VM host fencing epoch is stale");
                    }
                    self.terminal = true;
                    return Ok(Some(VmHostCommand::Fenced(VmHostFence {
                        epoch: fenced.epoch,
                        reason: fenced.reason,
                    })));
                }
                ServerMessage::Error(error) => {
                    validate_machine_id_like("error code", &error.code)?;
                    validate_bounded_text("error message", &error.message, MAX_REASON_BYTES)?;
                    self.terminal = true;
                    return Err(configuration(format!(
                        "server rejected VM host control: {}: {}",
                        error.code, error.message
                    )));
                }
            }
        }
    }

    fn accept_provision(&mut self, raw: ProvisionMessage) -> Result<VmHostProvision, ManagedError> {
        let allocation_id = parse_v4_uuid("allocation", &raw.allocation_id)?;
        validate_generation(raw.generation)?;
        if raw.slot >= self.max_vms {
            return self.fail("VM host provision slot is outside max_vms");
        }
        validate_machine_id(&raw.machine_id)?;
        let fingerprint = ProvisionFingerprint {
            generation: raw.generation,
            slot: raw.slot,
            machine_id: raw.machine_id.clone(),
            url: raw.tool_attachment.url.clone(),
            bearer: raw.tool_attachment.bearer.clone(),
        };
        if self.allocations.contains_key(&allocation_id) {
            if !accept_existing_provision(&mut self.allocations, allocation_id, fingerprint) {
                return self.fail("VM host changed immutable provision identity");
            }
        } else {
            if self.allocations.len() >= usize::from(self.max_vms)
                || self
                    .allocations
                    .values()
                    .any(|entry| entry.machine_id == raw.machine_id)
            {
                return self.fail("VM host provision exceeds capacity or reuses a machine ID");
            }
            self.allocations.insert(
                allocation_id,
                KnownAllocation {
                    generation: raw.generation,
                    machine_id: raw.machine_id.clone(),
                    provision: Some(fingerprint),
                    releasing: false,
                },
            );
        }
        let attachment = attachment_target(raw.tool_attachment)?;
        Ok(VmHostProvision {
            allocation_id,
            generation: raw.generation,
            slot: raw.slot,
            machine_id: raw.machine_id,
            attachment,
        })
    }

    fn accept_release(&mut self, raw: ReleaseMessage) -> Result<VmHostRelease, ManagedError> {
        let allocation_id = parse_v4_uuid("allocation", &raw.allocation_id)?;
        validate_generation(raw.generation)?;
        validate_machine_id(&raw.machine_id)?;
        if self.allocations.contains_key(&allocation_id) {
            self.require_known(allocation_id, raw.generation, &raw.machine_id, false)?;
            self.allocations
                .get_mut(&allocation_id)
                .expect("known allocation was just validated")
                .releasing = true;
        } else {
            // A release may be replayed after its acknowledgement was lost, or
            // after a replacement lease reconciled no ready copy. Keep the
            // identity long enough for the local supervisor to apply its
            // idempotent release/tombstone and acknowledge it again.
            self.allocations.insert(
                allocation_id,
                KnownAllocation {
                    generation: raw.generation,
                    machine_id: raw.machine_id.clone(),
                    provision: None,
                    releasing: true,
                },
            );
        }
        Ok(VmHostRelease {
            allocation_id,
            generation: raw.generation,
            machine_id: raw.machine_id,
        })
    }

    fn require_known(
        &mut self,
        allocation_id: Uuid,
        generation: u64,
        machine_id: &str,
        releasing: bool,
    ) -> Result<(), ManagedError> {
        if self.terminal {
            return Err(protocol("VM host connection is terminal"));
        }
        let Some(known) = self.allocations.get(&allocation_id) else {
            return self.fail("VM host acknowledgement has unknown allocation identity");
        };
        if known.generation != generation
            || known.machine_id != machine_id
            || known.releasing != releasing
        {
            return self.fail("VM host acknowledgement changed immutable allocation identity");
        }
        Ok(())
    }

    fn require_reconciled(&mut self) -> Result<(), ManagedError> {
        if !self.reconciled {
            return self.fail("VM host command arrived before lease reconciliation");
        }
        Ok(())
    }

    fn validate_pins(&mut self, lease_id: &str, epoch: u64) -> Result<(), ManagedError> {
        let lease_id = parse_v4_uuid("lease", lease_id)?;
        validate_generation(epoch)?;
        if lease_id != self.lease_id || epoch != self.epoch {
            return self.fail("VM host frame does not match the active lease and epoch");
        }
        Ok(())
    }

    fn fail<T>(&mut self, message: impl Into<String>) -> Result<T, ManagedError> {
        self.terminal = true;
        Err(protocol(message))
    }

    async fn send(&mut self, message: &ClientMessage<'_>) -> Result<(), ManagedError> {
        let encoded = serde_json::to_string(message)
            .map_err(|_| protocol("failed to encode VM host control frame"))?;
        if encoded.len() > MAX_MESSAGE_BYTES {
            return self.fail("outbound VM host frame exceeds 256 KiB");
        }
        self.socket
            .send(Message::Text(encoded.into()))
            .await
            .map_err(|_| protocol("VM host WebSocket send failed"))
    }
}

async fn connect_vm_host(
    mut origin: Url,
    path: String,
    mut bearer: String,
    scope: VmHostScope,
    host_id: Uuid,
    factory_name: String,
    max_vms: u16,
    vm: VmShape,
) -> Result<VmHostConnection, ManagedError> {
    require_v4_uuid("host", host_id)?;
    validate_vm_factory_name(&factory_name)?;
    validate_max_vms(max_vms)?;
    vm.validate()?;
    origin.set_path(&path);
    origin.set_query(None);
    origin.set_fragment(None);
    origin
        .set_scheme(match origin.scheme() {
            "http" => "ws",
            "https" => "wss",
            _ => return Err(configuration("managed VM host origin is not HTTP(S)")),
        })
        .map_err(|_| configuration("failed to derive managed VM host endpoint"))?;
    let mut request = origin
        .as_str()
        .into_client_request()
        .map_err(|_| protocol("invalid VM host WebSocket request"))?;
    let mut authorization = format!("Bearer {bearer}")
        .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
        .map_err(|_| configuration("VM host credential cannot form authorization"))?;
    bearer.zeroize();
    authorization.set_sensitive(true);
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        authorization,
    );
    let websocket = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (mut socket, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_async_with_config(request, Some(websocket), false),
    )
    .await
    .map_err(|_| protocol("VM host WebSocket handshake timed out"))?
    .map_err(vm_host_handshake_error)?;
    send_message(
        &mut socket,
        &ClientMessage::Attach {
            protocol_version: PROTOCOL_VERSION,
            host_id: host_id.to_string(),
            factory_name: &factory_name,
            max_vms,
            vm,
        },
    )
    .await?;
    let encoded = tokio::time::timeout(CONNECT_TIMEOUT, read_text(&mut socket))
        .await
        .map_err(|_| protocol("VM host lease timed out"))??
        .ok_or_else(|| protocol("VM host WebSocket closed before lease"))?;
    let lease = match decode_server_message(&encoded)? {
        ServerMessage::Lease(lease) => lease,
        ServerMessage::Error(error) => {
            validate_machine_id_like("error code", &error.code)?;
            validate_bounded_text("error message", &error.message, MAX_REASON_BYTES)?;
            return Err(configuration(format!(
                "server rejected VM host control: {}: {}",
                error.code, error.message
            )));
        }
        _ => return Err(protocol("VM host connection did not begin with lease")),
    };
    if lease.protocol_version != PROTOCOL_VERSION {
        return Err(protocol("VM host protocol version is not 1"));
    }
    let lease_id = parse_v4_uuid("lease", &lease.lease_id)?;
    validate_generation(lease.epoch)?;
    validate_timestamp(lease.expires_at)?;
    validate_max_vms(lease.max_vms)?;
    let leased_vm = lease.vm.validate()?;
    if lease.max_vms != max_vms || leased_vm != vm {
        return Err(protocol("VM host lease changed the advertised capacity"));
    }
    Ok(VmHostConnection {
        socket,
        scope,
        host_id,
        max_vms,
        vm,
        lease_id,
        epoch: lease.epoch,
        expires_at: lease.expires_at,
        reconciled: false,
        pending_ping: None,
        allocations: HashMap::new(),
        terminal: false,
    })
}

fn vm_host_handshake_error(error: WebSocketError) -> ManagedError {
    match error {
        WebSocketError::Http(response) if matches!(response.status().as_u16(), 401 | 403) => {
            configuration("VM host authentication was rejected")
        }
        _ => protocol("VM host WebSocket handshake failed"),
    }
}

async fn send_message(
    socket: &mut Socket,
    message: &ClientMessage<'_>,
) -> Result<(), ManagedError> {
    let encoded = serde_json::to_string(message)
        .map_err(|_| protocol("failed to encode VM host control frame"))?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(protocol("outbound VM host frame exceeds 256 KiB"));
    }
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| protocol("VM host WebSocket send failed"))
}

async fn read_text(socket: &mut Socket) -> Result<Option<String>, ManagedError> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(encoded))) => {
                if encoded.len() > MAX_MESSAGE_BYTES {
                    return Err(protocol("VM host frame exceeds 256 KiB"));
                }
                return Ok(Some(encoded.to_string()));
            }
            Some(Ok(Message::Ping(payload))) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|_| protocol("VM host WebSocket pong failed"))?,
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(_)) => return Err(protocol("VM host control requires text JSON frames")),
            Some(Err(_)) => return Err(protocol("VM host WebSocket receive failed")),
        }
    }
}

fn attachment_target(raw: ToolAttachment) -> Result<AttachmentTarget, ManagedError> {
    if raw.bearer.len() != ATTACHMENT_BEARER_BYTES
        || !raw
            .bearer
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(protocol(
            "VM host tool attachment bearer must be 43 base64url bytes",
        ));
    }
    if raw.url.len() > MAX_ATTACHMENT_URL_BYTES {
        return Err(protocol("VM host tool attachment URL exceeds 2048 bytes"));
    }
    let mut url =
        Url::parse(&raw.url).map_err(|_| protocol("VM host tool attachment URL is malformed"))?;
    if !matches!(url.scheme(), "https" | "wss")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(protocol(
            "VM host tool attachment URL must be HTTPS/WSS without credentials or fragment",
        ));
    }
    if url.scheme() == "https" {
        url.set_scheme("wss")
            .map_err(|_| protocol("VM host tool attachment URL cannot become WSS"))?;
    }
    AttachmentTarget::new(url.as_str(), raw.bearer)
        .map_err(|_| protocol("VM host tool attachment target is invalid"))
}

fn decode_server_message(encoded: &str) -> Result<ServerMessage, ManagedError> {
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(protocol("VM host frame exceeds 256 KiB"));
    }
    let value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|_| protocol("VM host frame is not valid JSON"))?;
    let kind = value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| protocol("VM host frame has no string type"))?;
    macro_rules! decode {
        ($ty:ty, $variant:ident) => {
            serde_json::from_value::<$ty>(value)
                .map(ServerMessage::$variant)
                .map_err(|_| {
                    protocol(concat!(
                        "VM host ",
                        stringify!($variant),
                        " frame is malformed"
                    ))
                })
        };
    }
    match kind {
        "lease" => decode!(LeaseMessage, Lease),
        "pong" => decode!(PongMessage, Pong),
        "provision" => decode!(ProvisionMessage, Provision),
        "release" => decode!(ReleaseMessage, Release),
        "fenced" => decode!(FencedMessage, Fenced),
        "error" => decode!(ErrorMessage, Error),
        _ => Err(protocol("VM host frame type is unknown")),
    }
}

fn validate_max_vms(max_vms: u16) -> Result<(), ManagedError> {
    if !(1..=MAX_VMS).contains(&max_vms) {
        return Err(configuration("VM host max_vms must be 1-64"));
    }
    Ok(())
}

fn validate_generation(value: u64) -> Result<(), ManagedError> {
    if !(1..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(protocol(
            "VM host epoch/generation is outside safe integer bounds",
        ));
    }
    Ok(())
}

fn validate_timestamp(value: u64) -> Result<(), ManagedError> {
    if !(1..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(protocol("VM host expiry is outside safe integer bounds"));
    }
    Ok(())
}

fn require_v4_uuid(kind: &str, value: Uuid) -> Result<(), ManagedError> {
    if value.get_version() != Some(Version::Random) || value.get_variant() != Variant::RFC4122 {
        return Err(configuration(format!(
            "VM host {kind} id must be a lowercase UUIDv4"
        )));
    }
    Ok(())
}

fn parse_v4_uuid(kind: &str, value: &str) -> Result<Uuid, ManagedError> {
    let parsed = Uuid::parse_str(value)
        .map_err(|_| protocol(format!("VM host {kind} id is not a lowercase UUIDv4")))?;
    if parsed.get_version() != Some(Version::Random)
        || parsed.get_variant() != Variant::RFC4122
        || parsed.to_string() != value
    {
        return Err(protocol(format!(
            "VM host {kind} id is not a lowercase UUIDv4"
        )));
    }
    Ok(parsed)
}

fn validate_machine_id(value: &str) -> Result<(), ManagedError> {
    validate_machine_id_like("machine id", value)
}

/// Validates the canonical name used to select a connected VM factory in `/mount`.
///
/// # Errors
///
/// Rejects reserved providers and names outside the lowercase portable grammar.
pub fn validate_vm_factory_name(value: &str) -> Result<(), ManagedError> {
    let bytes = value.as_bytes();
    let portable_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if bytes.is_empty()
        || bytes.len() > MAX_FACTORY_NAME_BYTES
        || !portable_edge(bytes[0])
        || !portable_edge(bytes[bytes.len() - 1])
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        || matches!(value, "cf_sandbox" | "cloudflare" | "host")
    {
        return Err(configuration(
            "VM host factory name must be a non-reserved lowercase portable identifier of 1-63 characters",
        ));
    }
    Ok(())
}

fn validate_machine_id_like(kind: &str, value: &str) -> Result<(), ManagedError> {
    if value.is_empty()
        || value.len() > MAX_MACHINE_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(protocol(format!(
            "VM host {kind} must be 1-123 safe ASCII characters"
        )));
    }
    Ok(())
}

fn validate_bounded_text(kind: &str, value: &str, max: usize) -> Result<(), ManagedError> {
    if value.is_empty()
        || value.len() > max
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(protocol(format!("VM host {kind} is invalid")));
    }
    Ok(())
}

fn configuration(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}

fn protocol(message: impl Into<String>) -> ManagedError {
    ManagedError::VmHost(message.into())
}

#[derive(Clone)]
struct KnownAllocation {
    generation: u64,
    machine_id: String,
    provision: Option<ProvisionFingerprint>,
    releasing: bool,
}

#[derive(Clone, Eq, PartialEq)]
struct ProvisionFingerprint {
    generation: u64,
    slot: u16,
    machine_id: String,
    url: String,
    bearer: String,
}

fn accept_existing_provision(
    allocations: &mut HashMap<Uuid, KnownAllocation>,
    allocation_id: Uuid,
    fingerprint: ProvisionFingerprint,
) -> bool {
    let Some(existing) = allocations.get(&allocation_id) else {
        return false;
    };
    if existing.releasing
        || existing.generation != fingerprint.generation
        || existing.machine_id != fingerprint.machine_id
    {
        return false;
    }
    if let Some(provision) = &existing.provision {
        return provision == &fingerprint;
    }
    // Reconciliation restores allocation identity without an attachment
    // credential. Pin the first epoch-bound provision, then require exact
    // fingerprint replays through the branch above.
    if allocations.iter().any(|(known_id, known)| {
        *known_id != allocation_id
            && known
                .provision
                .as_ref()
                .is_some_and(|provision| provision.slot == fingerprint.slot)
    }) {
        return false;
    }
    allocations
        .get_mut(&allocation_id)
        .expect("known allocation was just validated")
        .provision = Some(fingerprint);
    true
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage<'a> {
    Attach {
        protocol_version: u8,
        host_id: String,
        factory_name: &'a str,
        max_vms: u16,
        vm: VmShape,
    },
    Ping {
        lease_id: String,
        epoch: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        nonce: Option<&'a str>,
    },
    Reconcile {
        lease_id: String,
        epoch: u64,
        allocations: Vec<ReconcileAllocation<'a>>,
    },
    Provisioned {
        lease_id: String,
        epoch: u64,
        allocation_id: String,
        generation: u64,
        machine_id: &'a str,
    },
    Released {
        lease_id: String,
        epoch: u64,
        allocation_id: String,
        generation: u64,
        machine_id: &'a str,
    },
}

#[derive(Serialize)]
struct ReconcileAllocation<'a> {
    allocation_id: String,
    generation: u64,
    machine_id: &'a str,
    state: ReadyState,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReadyState {
    Ready,
}

enum ServerMessage {
    Lease(LeaseMessage),
    Pong(PongMessage),
    Provision(ProvisionMessage),
    Release(ReleaseMessage),
    Fenced(FencedMessage),
    Error(ErrorMessage),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseMessage {
    #[serde(rename = "type")]
    _kind: LeaseKind,
    protocol_version: u8,
    lease_id: String,
    epoch: u64,
    expires_at: u64,
    max_vms: u16,
    vm: RawVmShape,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PongMessage {
    #[serde(rename = "type")]
    _kind: PongKind,
    lease_id: String,
    epoch: u64,
    expires_at: u64,
    #[serde(default)]
    nonce: Option<String>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ProvisionMessage {
    #[serde(rename = "type")]
    _kind: ProvisionKind,
    lease_id: String,
    epoch: u64,
    allocation_id: String,
    generation: u64,
    slot: u16,
    machine_id: String,
    tool_attachment: ToolAttachment,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseMessage {
    #[serde(rename = "type")]
    _kind: ReleaseKind,
    lease_id: String,
    epoch: u64,
    allocation_id: String,
    generation: u64,
    machine_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FencedMessage {
    #[serde(rename = "type")]
    _kind: FencedKind,
    epoch: u64,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorMessage {
    #[serde(rename = "type")]
    _kind: ErrorKind,
    code: String,
    message: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ToolAttachment {
    url: String,
    bearer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVmShape {
    cpus: u8,
    memory_mib: u32,
}

impl RawVmShape {
    fn validate(self) -> Result<VmShape, ManagedError> {
        VmShape::new(self.cpus, self.memory_mib)
            .map_err(|_| protocol("VM host lease contains an invalid VM shape"))
    }
}

macro_rules! message_kind {
    ($name:ident, $value:literal) => {
        #[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
        enum $name {
            #[serde(rename = $value)]
            Value,
        }
    };
}

message_kind!(LeaseKind, "lease");
message_kind!(PongKind, "pong");
message_kind!(ProvisionKind, "provision");
message_kind!(ReleaseKind, "release");
message_kind!(FencedKind, "fenced");
message_kind!(ErrorKind, "error");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LEASE_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const ALLOCATION_ID: &str = "6ba7b810-9dad-41d1-80b4-00c04fd430c8";
    const OTHER_ALLOCATION_ID: &str = "8dc7cdb8-edf2-4bf4-8b15-0372a7b78b2f";

    fn reconciled_allocation(machine_id: &str) -> KnownAllocation {
        KnownAllocation {
            generation: 1,
            machine_id: machine_id.to_owned(),
            provision: None,
            releasing: false,
        }
    }

    fn provision_fingerprint(
        generation: u64,
        slot: u16,
        machine_id: &str,
        bearer: char,
    ) -> ProvisionFingerprint {
        ProvisionFingerprint {
            generation,
            slot,
            machine_id: machine_id.to_owned(),
            url: "wss://tools.example/attach".to_owned(),
            bearer: bearer.to_string().repeat(ATTACHMENT_BEARER_BYTES),
        }
    }

    #[test]
    fn strict_frames_reject_unknown_fields_and_noncanonical_identities() {
        let valid = json!({
            "type":"lease", "protocol_version":1, "lease_id":LEASE_ID,
            "epoch":1, "expires_at":10, "max_vms":2,
            "vm":{"cpus":2,"memory_mib":1024}
        });
        assert!(matches!(
            decode_server_message(&valid.to_string()).unwrap(),
            ServerMessage::Lease(_)
        ));

        let mut unknown = valid.clone();
        unknown["extra"] = json!(true);
        assert!(decode_server_message(&unknown.to_string()).is_err());

        let mut uppercase = valid;
        uppercase["lease_id"] = json!(LEASE_ID.to_uppercase());
        let ServerMessage::Lease(lease) = decode_server_message(&uppercase.to_string()).unwrap()
        else {
            panic!("expected lease");
        };
        assert!(parse_v4_uuid("lease", &lease.lease_id).is_err());
    }

    #[test]
    fn shape_capacity_generation_and_text_bounds_are_enforced() {
        assert!(VmShape::new(1, 128).is_ok());
        assert!(VmShape::new(64, 262_144).is_ok());
        assert!(VmShape::new(0, 1024).is_err());
        assert!(VmShape::new(1, 127).is_err());
        assert!(validate_max_vms(0).is_err());
        assert!(validate_max_vms(65).is_err());
        assert!(validate_generation(0).is_err());
        assert!(validate_generation(MAX_SAFE_INTEGER + 1).is_err());
        assert!(validate_machine_id("safe.machine:1").is_ok());
        assert!(validate_machine_id("unsafe/machine").is_err());
        assert!(validate_vm_factory_name("garage-mac").is_ok());
        assert!(validate_vm_factory_name("cf_sandbox").is_err());
        assert!(validate_vm_factory_name("host").is_err());
        assert!(validate_vm_factory_name("Uppercase").is_err());
    }

    #[test]
    fn provision_converts_only_secure_opaque_attachment_targets() {
        let target = attachment_target(ToolAttachment {
            url: "https://tools.example/v1/attach?ticket=opaque".to_owned(),
            bearer: "x".repeat(ATTACHMENT_BEARER_BYTES),
        })
        .unwrap();
        assert_eq!(
            target.endpoint().as_str(),
            "wss://tools.example/v1/attach?ticket=opaque"
        );
        assert!(
            attachment_target(ToolAttachment {
                url: "http://tools.example/attach".to_owned(),
                bearer: "x".repeat(ATTACHMENT_BEARER_BYTES),
            })
            .is_err()
        );
        assert!(
            attachment_target(ToolAttachment {
                url: "wss://tools.example/attach".to_owned(),
                bearer: "secret".to_owned(),
            })
            .is_err()
        );
    }

    #[test]
    fn reconciled_allocation_accepts_one_credential_refresh_and_pins_it() {
        let allocation_id = parse_v4_uuid("allocation", ALLOCATION_ID).unwrap();
        let mut allocations = HashMap::from([(allocation_id, reconciled_allocation("machine-1"))]);
        let refreshed = provision_fingerprint(1, 0, "machine-1", 'a');

        assert!(accept_existing_provision(
            &mut allocations,
            allocation_id,
            refreshed.clone(),
        ));
        assert!(
            allocations
                .get(&allocation_id)
                .and_then(|known| known.provision.as_ref())
                == Some(&refreshed)
        );
        assert!(accept_existing_provision(
            &mut allocations,
            allocation_id,
            refreshed.clone(),
        ));
        assert!(!accept_existing_provision(
            &mut allocations,
            allocation_id,
            provision_fingerprint(1, 0, "machine-1", 'b'),
        ));
        assert!(!accept_existing_provision(
            &mut allocations,
            allocation_id,
            provision_fingerprint(1, 1, "machine-1", 'a'),
        ));
        assert!(
            allocations
                .get(&allocation_id)
                .and_then(|known| known.provision.as_ref())
                == Some(&refreshed)
        );
    }

    #[test]
    fn reconciled_credential_refresh_preserves_identity_and_slot_conflicts() {
        let allocation_id = parse_v4_uuid("allocation", ALLOCATION_ID).unwrap();
        let other_id = parse_v4_uuid("allocation", OTHER_ALLOCATION_ID).unwrap();
        let matching = provision_fingerprint(1, 0, "machine-1", 'a');

        for conflicting in [
            provision_fingerprint(2, 0, "machine-1", 'a'),
            provision_fingerprint(1, 0, "machine-2", 'a'),
        ] {
            let mut allocations =
                HashMap::from([(allocation_id, reconciled_allocation("machine-1"))]);
            assert!(!accept_existing_provision(
                &mut allocations,
                allocation_id,
                conflicting,
            ));
            assert!(allocations[&allocation_id].provision.is_none());
        }

        let mut releasing = reconciled_allocation("machine-1");
        releasing.releasing = true;
        let mut allocations = HashMap::from([(allocation_id, releasing)]);
        assert!(!accept_existing_provision(
            &mut allocations,
            allocation_id,
            matching.clone(),
        ));

        let mut occupied = reconciled_allocation("machine-2");
        occupied.provision = Some(provision_fingerprint(1, 0, "machine-2", 'z'));
        let mut allocations = HashMap::from([
            (allocation_id, reconciled_allocation("machine-1")),
            (other_id, occupied),
        ]);
        assert!(!accept_existing_provision(
            &mut allocations,
            allocation_id,
            matching,
        ));
        assert!(allocations[&allocation_id].provision.is_none());
    }

    #[test]
    fn decoded_provision_debug_never_contains_the_bearer() {
        let bearer = "z".repeat(ATTACHMENT_BEARER_BYTES);
        let encoded = json!({
            "type":"provision", "lease_id":LEASE_ID, "epoch":1,
            "allocation_id":ALLOCATION_ID, "generation":1, "slot":0,
            "machine_id":"machine-1",
            "tool_attachment":{"url":"wss://tools.example/attach","bearer":bearer}
        });
        let ServerMessage::Provision(raw) = decode_server_message(&encoded.to_string()).unwrap()
        else {
            panic!("expected provision");
        };
        let public = VmHostProvision {
            allocation_id: parse_v4_uuid("allocation", &raw.allocation_id).unwrap(),
            generation: raw.generation,
            slot: raw.slot,
            machine_id: raw.machine_id,
            attachment: attachment_target(raw.tool_attachment).unwrap(),
        };
        assert!(!format!("{public:?}").contains(&bearer));
    }

    #[test]
    fn oversized_and_nontext_protocol_inputs_are_rejected_before_decode() {
        let oversized = format!(
            "{{\"type\":\"error\",\"code\":\"x\",\"message\":\"{}\"}}",
            "x".repeat(MAX_MESSAGE_BYTES)
        );
        assert!(decode_server_message(&oversized).is_err());
    }
}
