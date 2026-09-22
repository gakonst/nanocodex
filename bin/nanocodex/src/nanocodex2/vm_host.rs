//! Bounded on-demand VM ownership behind the managed VM-host control plane.

use nanocodex_managed::ManagedError;

use super::Host;

#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod supported {
    use std::{
        collections::{BTreeMap, VecDeque},
        env,
        fs::{self, File, OpenOptions},
        future::Future,
        io::Write as _,
        os::fd::AsRawFd as _,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _},
        path::{Path, PathBuf},
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    use fs2::FileExt as _;
    use nanocodex_managed::{
        ManagedClient, ManagedError, VmHostAllocationState, VmHostCommand, VmHostConnection,
        VmHostScope, VmShape, connect_system_vm_host,
    };
    use nanocodex_tools::attachment::{
        Attachment, AttachmentError, AttachmentMetadata, AttachmentTarget,
    };
    use serde::{Deserialize, Serialize};
    use tempfile::NamedTempFile;
    use uuid::Uuid;

    use super::super::{
        Host, HostScope, SYSTEM_HOST_TOKEN_ENV, client_from_environment,
        managed_url_from_environment, vm_hand,
    };

    type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
    const ATTACHMENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
    const INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(250);
    const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);
    const HEALTHY_CONNECTION_DURATION: Duration = Duration::from_secs(20);

    /// Immutable control-plane identity for one VM allocation.
    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct VmAllocationIdentity {
        pub(crate) allocation_id: Uuid,
        pub(crate) generation: u64,
        pub(crate) machine_id: String,
    }

    /// Complete local provisioning request for one control-plane allocation.
    #[derive(Clone, Debug)]
    pub(crate) struct VmAllocationSpec {
        pub(crate) identity: VmAllocationIdentity,
        pub(crate) slot: u16,
        pub(crate) attachment_target: AttachmentTarget,
    }

    impl VmAllocationSpec {
        pub(crate) fn new(
            allocation_id: Uuid,
            slot: u16,
            generation: u64,
            machine_id: impl Into<String>,
            attachment_target: AttachmentTarget,
        ) -> Self {
            Self {
                identity: VmAllocationIdentity {
                    allocation_id,
                    generation,
                    machine_id: machine_id.into(),
                },
                slot,
                attachment_target,
            }
        }

        fn immutable_key(&self) -> ImmutableAllocation {
            ImmutableAllocation {
                identity: self.identity.clone(),
                slot: self.slot,
                attachment_endpoint: self.attachment_target.endpoint().to_string(),
            }
        }
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    struct ImmutableAllocation {
        identity: VmAllocationIdentity,
        slot: u16,
        attachment_endpoint: String,
    }

    /// Result of an idempotent provision or release request.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum VmAllocationChange {
        Changed,
        Unchanged,
    }

    /// Local VM-host policy or resource failure.
    #[derive(Debug, thiserror::Error)]
    pub(crate) enum VmHostError {
        #[error("invalid VM host configuration: {0}")]
        Configuration(String),
        #[error("VM host state failed: {0}")]
        State(String),
        #[error("VM host capacity {capacity} is exhausted")]
        Capacity { capacity: u16 },
        #[error("allocation {allocation_id} changed immutable fields")]
        ImmutableConflict { allocation_id: Uuid },
        #[error("VM allocation resource failed: {0}")]
        Resource(String),
        #[error("VM allocation operation cancelled after the control connection became terminal")]
        Cancelled,
    }

    impl From<VmHostError> for ManagedError {
        fn from(error: VmHostError) -> Self {
            Self::Configuration(error.to_string())
        }
    }

    /// Process-exclusive durable state for one stable host identity.
    struct VmHostState {
        directory: PathBuf,
        host_id: Uuid,
        _lock: File,
    }

    impl VmHostState {
        fn open(directory: &Path, explicit_host_id: Option<Uuid>) -> Result<Self, VmHostError> {
            fs::create_dir_all(directory).map_err(|error| {
                VmHostError::State(format!(
                    "failed to create host state directory {}: {error}",
                    directory.display()
                ))
            })?;
            let directory = directory.canonicalize().map_err(|error| {
                VmHostError::State(format!(
                    "failed to resolve host state directory {}: {error}",
                    directory.display()
                ))
            })?;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    VmHostError::State(format!(
                        "failed to secure host state directory {}: {error}",
                        directory.display()
                    ))
                },
            )?;
            let lock_path = directory.join("host.lock");
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|error| {
                    VmHostError::State(format!(
                        "failed to open host state lock {}: {error}",
                        lock_path.display()
                    ))
                })?;
            lock.try_lock_exclusive().map_err(|error| {
                VmHostError::State(format!(
                    "host state is already in use ({}): {error}",
                    directory.display()
                ))
            })?;

            let identity_path = directory.join("host-id");
            let persisted = match fs::read_to_string(&identity_path) {
                Ok(value) => {
                    let parsed = Uuid::parse_str(value.trim()).map_err(|error| {
                        VmHostError::State(format!(
                            "host identity {} is invalid: {error}",
                            identity_path.display()
                        ))
                    })?;
                    if parsed.get_version_num() != 4
                        || parsed.get_variant() != uuid::Variant::RFC4122
                    {
                        return Err(VmHostError::State(format!(
                            "host identity {} must be a UUID v4",
                            identity_path.display()
                        )));
                    }
                    Some(parsed)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(VmHostError::State(format!(
                        "failed to read host identity {}: {error}",
                        identity_path.display()
                    )));
                }
            };
            let host_id = match (persisted, explicit_host_id) {
                (Some(persisted), Some(explicit)) if persisted != explicit => {
                    return Err(VmHostError::Configuration(format!(
                        "--host-id {explicit} does not match the persisted host identity {persisted}"
                    )));
                }
                (Some(persisted), _) => persisted,
                (None, explicit) => {
                    let host_id = explicit.unwrap_or_else(Uuid::new_v4);
                    persist_host_id(&directory, &identity_path, host_id)?;
                    host_id
                }
            };
            let allocations = directory.join("allocations");
            fs::create_dir_all(&allocations).map_err(|error| {
                VmHostError::State(format!("failed to create allocation state: {error}"))
            })?;
            let allocation_metadata = fs::symlink_metadata(&allocations).map_err(|error| {
                VmHostError::State(format!("failed to inspect allocation state: {error}"))
            })?;
            if allocation_metadata.file_type().is_symlink() || !allocation_metadata.is_dir() {
                return Err(VmHostError::State(
                    "allocation state must be a real directory".to_owned(),
                ));
            }
            let canonical_allocations = allocations.canonicalize().map_err(|error| {
                VmHostError::State(format!("failed to resolve allocation state: {error}"))
            })?;
            if canonical_allocations.parent() != Some(directory.as_path()) {
                return Err(VmHostError::State(
                    "allocation state escapes the host state directory".to_owned(),
                ));
            }
            fs::set_permissions(&allocations, fs::Permissions::from_mode(0o700)).map_err(
                |error| VmHostError::State(format!("failed to secure allocation state: {error}")),
            )?;
            Ok(Self {
                directory,
                host_id,
                _lock: lock,
            })
        }
    }

    fn persist_host_id(
        directory: &Path,
        identity_path: &Path,
        host_id: Uuid,
    ) -> Result<(), VmHostError> {
        let mut temporary = NamedTempFile::new_in(directory).map_err(|error| {
            VmHostError::State(format!("failed to create temporary host identity: {error}"))
        })?;
        writeln!(temporary, "{host_id}").map_err(|error| {
            VmHostError::State(format!("failed to write host identity: {error}"))
        })?;
        temporary.as_file().sync_all().map_err(|error| {
            VmHostError::State(format!("failed to sync host identity: {error}"))
        })?;
        temporary
            .persist_noclobber(identity_path)
            .map_err(|error| {
                VmHostError::State(format!("failed to persist host identity: {}", error.error))
            })?;
        sync_directory(directory)?;
        Ok(())
    }

    const ALLOCATION_RECORD_VERSION: u8 = 1;

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    enum AllocationRecordState {
        Provisioning,
        Ready,
        Stopped,
        Releasing,
        ReleasingStopped,
        Released,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct AllocationRecord {
        version: u8,
        state: AllocationRecordState,
        identity: VmAllocationIdentity,
        slot: Option<u16>,
        attachment_endpoint: Option<String>,
    }

    impl AllocationRecord {
        fn tracked(state: AllocationRecordState, immutable: &ImmutableAllocation) -> Self {
            Self {
                version: ALLOCATION_RECORD_VERSION,
                state,
                identity: immutable.identity.clone(),
                slot: Some(immutable.slot),
                attachment_endpoint: Some(immutable.attachment_endpoint.clone()),
            }
        }

        fn release(
            state: AllocationRecordState,
            identity: &VmAllocationIdentity,
            immutable: Option<&ImmutableAllocation>,
        ) -> Self {
            Self {
                version: ALLOCATION_RECORD_VERSION,
                state,
                identity: identity.clone(),
                slot: immutable.map(|value| value.slot),
                attachment_endpoint: immutable.map(|value| value.attachment_endpoint.clone()),
            }
        }

        fn immutable(&self) -> Result<Option<ImmutableAllocation>, VmHostError> {
            match (&self.slot, &self.attachment_endpoint) {
                (Some(slot), Some(endpoint)) => Ok(Some(ImmutableAllocation {
                    identity: self.identity.clone(),
                    slot: *slot,
                    attachment_endpoint: endpoint.clone(),
                })),
                (None, None) => Ok(None),
                _ => Err(VmHostError::State(
                    "allocation record has incomplete immutable fields".to_owned(),
                )),
            }
        }
    }

    struct AllocationStore {
        directory: PathBuf,
    }

    struct LoadedAllocations {
        recovered: BTreeMap<Uuid, RecoveredAllocation>,
        retired: BTreeMap<Uuid, RetiredAllocation>,
    }

    struct RecoveredAllocation {
        immutable: ImmutableAllocation,
        cleanup_safe: bool,
    }

    impl AllocationStore {
        fn new(directory: PathBuf) -> Self {
            Self { directory }
        }

        fn record_path(&self, allocation_id: Uuid) -> PathBuf {
            self.directory.join(format!("{allocation_id}.json"))
        }

        fn put(&self, record: &AllocationRecord) -> Result<(), VmHostError> {
            let encoded = serde_json::to_vec(record).map_err(|error| {
                VmHostError::State(format!("failed to encode allocation record: {error}"))
            })?;
            let destination = self.record_path(record.identity.allocation_id);
            let mut temporary = NamedTempFile::new_in(&self.directory).map_err(|error| {
                VmHostError::State(format!("failed to create allocation record: {error}"))
            })?;
            temporary.write_all(&encoded).map_err(|error| {
                VmHostError::State(format!("failed to write allocation record: {error}"))
            })?;
            temporary.as_file().sync_all().map_err(|error| {
                VmHostError::State(format!("failed to sync allocation record: {error}"))
            })?;
            temporary.persist(&destination).map_err(|error| {
                VmHostError::State(format!(
                    "failed to persist allocation record: {}",
                    error.error
                ))
            })?;
            sync_directory(&self.directory)?;
            Ok(())
        }

        fn remove(&self, allocation_id: Uuid) -> Result<(), VmHostError> {
            let path = self.record_path(allocation_id);
            match fs::remove_file(&path) {
                Ok(()) => sync_directory(&self.directory),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(VmHostError::State(format!(
                    "failed to remove acknowledged allocation record {}: {error}",
                    path.display()
                ))),
            }
        }

        fn load(&self, capacity: u16) -> Result<LoadedAllocations, VmHostError> {
            let mut recovered = BTreeMap::new();
            let mut retired = BTreeMap::new();
            let mut slots = BTreeMap::new();
            let mut machines = BTreeMap::new();
            for entry in fs::read_dir(&self.directory).map_err(|error| {
                VmHostError::State(format!("failed to read allocation state: {error}"))
            })? {
                let entry = entry.map_err(|error| {
                    VmHostError::State(format!("failed to inspect allocation state: {error}"))
                })?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let file_type = entry.file_type().map_err(|error| {
                    VmHostError::State(format!(
                        "failed to inspect allocation record {}: {error}",
                        path.display()
                    ))
                })?;
                if file_type.is_symlink() || !file_type.is_file() {
                    return Err(VmHostError::State(format!(
                        "allocation record must be a real file: {}",
                        path.display()
                    )));
                }
                let filename_id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .and_then(|value| Uuid::parse_str(value).ok())
                    .ok_or_else(|| {
                        VmHostError::State(format!(
                            "allocation record has invalid filename: {}",
                            path.display()
                        ))
                    })?;
                let record: AllocationRecord =
                    serde_json::from_slice(&fs::read(&path).map_err(|error| {
                        VmHostError::State(format!(
                            "failed to read allocation record {}: {error}",
                            path.display()
                        ))
                    })?)
                    .map_err(|error| {
                        VmHostError::State(format!(
                            "allocation record {} is invalid: {error}",
                            path.display()
                        ))
                    })?;
                validate_record(&record, filename_id, capacity)?;
                match record.state {
                    AllocationRecordState::Provisioning
                    | AllocationRecordState::Ready
                    | AllocationRecordState::Stopped => {
                        let immutable = record.immutable()?.ok_or_else(|| {
                            VmHostError::State(
                                "tracked allocation record lacks immutable fields".to_owned(),
                            )
                        })?;
                        if slots.insert(immutable.slot, filename_id).is_some()
                            || machines
                                .insert(immutable.identity.machine_id.clone(), filename_id)
                                .is_some()
                        {
                            return Err(VmHostError::State(
                                "allocation records reuse a live slot or machine ID".to_owned(),
                            ));
                        }
                        recovered.insert(
                            filename_id,
                            RecoveredAllocation {
                                immutable,
                                cleanup_safe: record.state == AllocationRecordState::Stopped,
                            },
                        );
                    }
                    AllocationRecordState::Releasing
                    | AllocationRecordState::ReleasingStopped
                    | AllocationRecordState::Released => {
                        let immutable = record.immutable()?;
                        if record.state != AllocationRecordState::Released {
                            let immutable = immutable.as_ref().ok_or_else(|| {
                                VmHostError::State(
                                    "pending release record lacks immutable fields".to_owned(),
                                )
                            })?;
                            if slots.insert(immutable.slot, filename_id).is_some()
                                || machines
                                    .insert(immutable.identity.machine_id.clone(), filename_id)
                                    .is_some()
                            {
                                return Err(VmHostError::State(
                                    "allocation records reuse a reserved slot or machine ID"
                                        .to_owned(),
                                ));
                            }
                        }
                        retired.insert(
                            filename_id,
                            RetiredAllocation {
                                identity: record.identity,
                                immutable,
                                release_pending: record.state != AllocationRecordState::Released,
                                cleanup_safe: record.state
                                    == AllocationRecordState::ReleasingStopped,
                            },
                        );
                    }
                }
            }
            Ok(LoadedAllocations { recovered, retired })
        }

        /// Converts every phase which could have owned a libkrun VM in the
        /// predecessor into a durable stopped phase. This is only valid after
        /// `VmHostState` has acquired the process-exclusive state lock: libkrun
        /// VMs are process-owned, so acquiring that lock proves the predecessor
        /// (and all of its VM execution) has ended.
        fn fence_predecessor_orphans(
            &self,
            state: &VmHostState,
            capacity: u16,
        ) -> Result<LoadedAllocations, VmHostError> {
            if self.directory != state.directory.join("allocations") {
                return Err(VmHostError::State(
                    "allocation store does not belong to the locked host state".to_owned(),
                ));
            }
            let mut loaded = self.load(capacity)?;
            for recovered in loaded.recovered.values_mut() {
                if !recovered.cleanup_safe {
                    self.put(&AllocationRecord::tracked(
                        AllocationRecordState::Stopped,
                        &recovered.immutable,
                    ))?;
                    recovered.cleanup_safe = true;
                }
            }
            for retired in loaded.retired.values_mut() {
                if retired.release_pending && !retired.cleanup_safe {
                    let immutable = retired.immutable.as_ref().ok_or_else(|| {
                        VmHostError::State(
                            "pending release record lacks immutable fields".to_owned(),
                        )
                    })?;
                    self.put(&AllocationRecord::release(
                        AllocationRecordState::ReleasingStopped,
                        &retired.identity,
                        Some(immutable),
                    ))?;
                    retired.cleanup_safe = true;
                }
            }
            Ok(loaded)
        }
    }

    fn validate_record(
        record: &AllocationRecord,
        filename_id: Uuid,
        capacity: u16,
    ) -> Result<(), VmHostError> {
        if record.version != ALLOCATION_RECORD_VERSION
            || record.identity.allocation_id != filename_id
            || record.identity.allocation_id.get_version_num() != 4
            || record.identity.allocation_id.get_variant() != uuid::Variant::RFC4122
            || record.identity.generation == 0
            || record.identity.generation > 9_007_199_254_740_991
            || !valid_persisted_machine_id(&record.identity.machine_id)
        {
            return Err(VmHostError::State(
                "allocation record has invalid identity or version".to_owned(),
            ));
        }
        if let Some(slot) = record.slot
            && slot >= capacity
        {
            return Err(VmHostError::State(
                "allocation record slot is outside host capacity".to_owned(),
            ));
        }
        if let Some(endpoint) = &record.attachment_endpoint {
            AttachmentTarget::new(endpoint.clone(), "record-validation-bearer").map_err(|_| {
                VmHostError::State(
                    "allocation record has an invalid attachment endpoint".to_owned(),
                )
            })?;
        }
        match record.state {
            AllocationRecordState::Provisioning
            | AllocationRecordState::Ready
            | AllocationRecordState::Stopped
            | AllocationRecordState::Releasing
            | AllocationRecordState::ReleasingStopped
                if record.slot.is_none() || record.attachment_endpoint.is_none() =>
            {
                Err(VmHostError::State(
                    "tracked allocation record lacks immutable fields".to_owned(),
                ))
            }
            AllocationRecordState::Released
                if record.slot.is_some() || record.attachment_endpoint.is_some() =>
            {
                Err(VmHostError::State(
                    "released allocation record retains immutable fields".to_owned(),
                ))
            }
            _ => Ok(()),
        }
    }

    fn valid_persisted_machine_id(value: &str) -> bool {
        let bytes = value.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 123
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(byte))
    }

    fn sync_directory(directory: &Path) -> Result<(), VmHostError> {
        File::open(directory)
            .and_then(|handle| handle.sync_all())
            .map_err(|error| {
                VmHostError::State(format!("failed to sync allocation state: {error}"))
            })
    }

    #[derive(Clone, Default)]
    struct OperationCancellation {
        cancelled: Arc<AtomicBool>,
    }

    impl OperationCancellation {
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);
        }

        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::Acquire)
        }

        fn check(&self) -> Result<(), VmHostError> {
            if self.is_cancelled() {
                Err(VmHostError::Cancelled)
            } else {
                Ok(())
            }
        }
    }

    struct ProvisionFailure {
        error: VmHostError,
        cleanup_safe: bool,
    }

    impl ProvisionFailure {
        const fn stopped(error: VmHostError) -> Self {
            Self {
                error,
                cleanup_safe: true,
            }
        }

        const fn unproven(error: VmHostError) -> Self {
            Self {
                error,
                cleanup_safe: false,
            }
        }
    }

    struct StopOutcome {
        attachment: Result<(), VmHostError>,
        shutdown: Result<(), VmHostError>,
    }

    impl StopOutcome {
        #[cfg(test)]
        const fn stopped() -> Self {
            Self {
                attachment: Ok(()),
                shutdown: Ok(()),
            }
        }

        fn shutdown_succeeded(&self) -> bool {
            self.shutdown.is_ok()
        }

        fn into_result(self) -> Result<(), VmHostError> {
            combine_failures([self.attachment, self.shutdown])
        }
    }

    trait AllocationFactory {
        type Allocation: Send;

        fn provision<'a>(
            &'a self,
            spec: VmAllocationSpec,
            cancellation: OperationCancellation,
        ) -> BoxFuture<'a, Result<Self::Allocation, ProvisionFailure>>;

        fn refresh_attachment<'a>(
            &'a self,
            allocation: &'a mut Self::Allocation,
            target: AttachmentTarget,
            cancellation: OperationCancellation,
        ) -> BoxFuture<'a, Result<(), VmHostError>>;

        fn stop<'a>(&'a self, allocation: Self::Allocation) -> BoxFuture<'a, StopOutcome>;

        fn release_recovered<'a>(
            &'a self,
            identity: &'a VmAllocationIdentity,
        ) -> BoxFuture<'a, Result<(), VmHostError>>;
    }

    enum ActiveAllocation<A> {
        Live {
            immutable: ImmutableAllocation,
            allocation: A,
            attachment_target: AttachmentTarget,
        },
        Recovered {
            immutable: ImmutableAllocation,
            cleanup_safe: bool,
        },
    }

    impl<A> ActiveAllocation<A> {
        const fn immutable(&self) -> &ImmutableAllocation {
            match self {
                Self::Live { immutable, .. } | Self::Recovered { immutable, .. } => immutable,
            }
        }
    }

    struct RetiredAllocation {
        identity: VmAllocationIdentity,
        immutable: Option<ImmutableAllocation>,
        release_pending: bool,
        cleanup_safe: bool,
    }

    struct AllocationSupervisor<F: AllocationFactory> {
        capacity: u16,
        factory: F,
        store: Option<AllocationStore>,
        active: BTreeMap<Uuid, ActiveAllocation<F::Allocation>>,
        retired: BTreeMap<Uuid, RetiredAllocation>,
    }

    impl<F: AllocationFactory> AllocationSupervisor<F> {
        #[cfg(test)]
        fn new(capacity: u16, factory: F) -> Result<Self, VmHostError> {
            if capacity == 0 {
                return Err(VmHostError::Configuration(
                    "VM host capacity must be positive".to_owned(),
                ));
            }
            Ok(Self {
                capacity,
                factory,
                store: None,
                active: BTreeMap::new(),
                retired: BTreeMap::new(),
            })
        }

        fn with_store(
            capacity: u16,
            factory: F,
            store: AllocationStore,
            state: &VmHostState,
        ) -> Result<Self, VmHostError> {
            let loaded = store.fence_predecessor_orphans(state, capacity)?;
            Ok(Self {
                capacity,
                factory,
                store: Some(store),
                active: loaded
                    .recovered
                    .into_iter()
                    .map(|(id, recovered)| {
                        (
                            id,
                            ActiveAllocation::Recovered {
                                immutable: recovered.immutable,
                                cleanup_safe: recovered.cleanup_safe,
                            },
                        )
                    })
                    .collect(),
                retired: loaded.retired,
            })
        }

        fn reserved_capacity(&self) -> usize {
            self.active.len()
                + self
                    .retired
                    .values()
                    .filter(|allocation| allocation.release_pending)
                    .count()
        }

        fn persist(&self, record: &AllocationRecord) -> Result<(), VmHostError> {
            self.store
                .as_ref()
                .map_or(Ok(()), |store| store.put(record))
        }

        #[cfg(test)]
        async fn provision(
            &mut self,
            spec: VmAllocationSpec,
        ) -> Result<VmAllocationChange, VmHostError> {
            self.provision_controlled(spec, OperationCancellation::default())
                .await
        }

        async fn provision_controlled(
            &mut self,
            spec: VmAllocationSpec,
            cancellation: OperationCancellation,
        ) -> Result<VmAllocationChange, VmHostError> {
            cancellation.check()?;
            if spec.identity.generation == 0 {
                return Err(VmHostError::Configuration(
                    "allocation generation must be positive".to_owned(),
                ));
            }
            let immutable = spec.immutable_key();
            if self.retired.contains_key(&spec.identity.allocation_id) {
                return Err(VmHostError::ImmutableConflict {
                    allocation_id: spec.identity.allocation_id,
                });
            }
            if let Some(existing) = self.active.get(&spec.identity.allocation_id) {
                if existing.immutable() != &immutable {
                    return Err(VmHostError::ImmutableConflict {
                        allocation_id: spec.identity.allocation_id,
                    });
                }
                if matches!(
                    existing,
                    ActiveAllocation::Recovered {
                        cleanup_safe: false,
                        ..
                    }
                ) {
                    return Err(VmHostError::State(format!(
                        "allocation {} cannot be reprovisioned because its recovered VM was not proven stopped",
                        spec.identity.allocation_id
                    )));
                }
            }
            if let Some(ActiveAllocation::Live {
                allocation,
                attachment_target,
                ..
            }) = self.active.get_mut(&spec.identity.allocation_id)
            {
                if attachment_target == &spec.attachment_target {
                    return Ok(VmAllocationChange::Unchanged);
                }
                let target = spec.attachment_target;
                self.factory
                    .refresh_attachment(allocation, target.clone(), cancellation.clone())
                    .await?;
                cancellation.check()?;
                *attachment_target = target;
                return Ok(VmAllocationChange::Unchanged);
            }
            if self.active.values().any(|allocation| {
                allocation.immutable().identity.allocation_id != spec.identity.allocation_id
                    && (allocation.immutable().slot == spec.slot
                        || allocation.immutable().identity.machine_id == spec.identity.machine_id)
            }) || self.retired.values().any(|allocation| {
                allocation.release_pending
                    && allocation.immutable.as_ref().is_some_and(|immutable| {
                        immutable.slot == spec.slot
                            || immutable.identity.machine_id == spec.identity.machine_id
                    })
            }) {
                return Err(VmHostError::ImmutableConflict {
                    allocation_id: spec.identity.allocation_id,
                });
            }
            let reserved = self.reserved_capacity();
            if spec.slot >= self.capacity
                || (!self.active.contains_key(&spec.identity.allocation_id)
                    && reserved >= usize::from(self.capacity))
            {
                return Err(VmHostError::Capacity {
                    capacity: self.capacity,
                });
            }
            self.persist(&AllocationRecord::tracked(
                AllocationRecordState::Provisioning,
                &immutable,
            ))?;
            self.active.insert(
                immutable.identity.allocation_id,
                ActiveAllocation::Recovered {
                    immutable: immutable.clone(),
                    cleanup_safe: false,
                },
            );
            let attachment_target = spec.attachment_target.clone();
            let allocation = match self.factory.provision(spec, cancellation.clone()).await {
                Ok(allocation) => allocation,
                Err(failure) => {
                    let mut outcome = Err(failure.error);
                    if failure.cleanup_safe {
                        let stopped = self.persist(&AllocationRecord::tracked(
                            AllocationRecordState::Stopped,
                            &immutable,
                        ));
                        self.active.insert(
                            immutable.identity.allocation_id,
                            ActiveAllocation::Recovered {
                                immutable,
                                cleanup_safe: true,
                            },
                        );
                        outcome = combine_pair(outcome, stopped);
                    }
                    return outcome.map(|()| VmAllocationChange::Changed);
                }
            };
            if cancellation.is_cancelled() {
                let stopped = self.factory.stop(allocation).await;
                return self
                    .record_cancelled_provision(&immutable, stopped)
                    .map(|()| VmAllocationChange::Changed);
            }
            if let Err(error) = self.persist(&AllocationRecord::tracked(
                AllocationRecordState::Ready,
                &immutable,
            )) {
                let stopped = self.factory.stop(allocation).await;
                return self
                    .record_failed_ready_publish(&immutable, error, stopped)
                    .map(|()| VmAllocationChange::Changed);
            }
            self.active.insert(
                immutable.identity.allocation_id,
                ActiveAllocation::Live {
                    immutable,
                    allocation,
                    attachment_target,
                },
            );
            Ok(VmAllocationChange::Changed)
        }

        fn record_cancelled_provision(
            &mut self,
            immutable: &ImmutableAllocation,
            stopped: StopOutcome,
        ) -> Result<(), VmHostError> {
            let shutdown_succeeded = stopped.shutdown_succeeded();
            let mut outcome = combine_pair(Err(VmHostError::Cancelled), stopped.into_result());
            if shutdown_succeeded {
                let persisted = self.persist(&AllocationRecord::tracked(
                    AllocationRecordState::Stopped,
                    immutable,
                ));
                self.active.insert(
                    immutable.identity.allocation_id,
                    ActiveAllocation::Recovered {
                        immutable: immutable.clone(),
                        cleanup_safe: true,
                    },
                );
                outcome = combine_pair(outcome, persisted);
            }
            outcome
        }

        fn record_failed_ready_publish(
            &mut self,
            immutable: &ImmutableAllocation,
            publish_error: VmHostError,
            stopped: StopOutcome,
        ) -> Result<(), VmHostError> {
            let shutdown_succeeded = stopped.shutdown_succeeded();
            let mut outcome = combine_pair(Err(publish_error), stopped.into_result());
            if shutdown_succeeded {
                let persisted = self.persist(&AllocationRecord::tracked(
                    AllocationRecordState::Stopped,
                    immutable,
                ));
                self.active.insert(
                    immutable.identity.allocation_id,
                    ActiveAllocation::Recovered {
                        immutable: immutable.clone(),
                        cleanup_safe: true,
                    },
                );
                outcome = combine_pair(outcome, persisted);
            }
            outcome
        }

        async fn release(
            &mut self,
            identity: &VmAllocationIdentity,
        ) -> Result<VmAllocationChange, VmHostError> {
            if let Some(retired) = self.retired.get(&identity.allocation_id) {
                if &retired.identity != identity {
                    return Err(VmHostError::ImmutableConflict {
                        allocation_id: identity.allocation_id,
                    });
                }
                if !retired.release_pending {
                    return Ok(VmAllocationChange::Unchanged);
                }
                if !retired.cleanup_safe {
                    return Err(VmHostError::State(format!(
                        "allocation {} release remains pending because its recovered VM was not proven stopped",
                        identity.allocation_id
                    )));
                }
                let immutable = retired.immutable.clone();
                return self.finish_stopped_release(identity, immutable).await;
            }
            let existing = self.active.get(&identity.allocation_id);
            if let Some(existing) = existing
                && &existing.immutable().identity != identity
            {
                return Err(VmHostError::ImmutableConflict {
                    allocation_id: identity.allocation_id,
                });
            }
            let Some(existing) = existing else {
                self.persist(&AllocationRecord::release(
                    AllocationRecordState::Released,
                    identity,
                    None,
                ))?;
                self.retired.insert(
                    identity.allocation_id,
                    RetiredAllocation {
                        identity: identity.clone(),
                        immutable: None,
                        release_pending: false,
                        cleanup_safe: false,
                    },
                );
                return Ok(VmAllocationChange::Changed);
            };
            let immutable = existing.immutable().clone();
            let release_state = match existing {
                ActiveAllocation::Live { .. } => AllocationRecordState::Releasing,
                ActiveAllocation::Recovered {
                    cleanup_safe: true, ..
                } => AllocationRecordState::ReleasingStopped,
                ActiveAllocation::Recovered {
                    cleanup_safe: false,
                    ..
                } => AllocationRecordState::Releasing,
            };
            self.persist(&AllocationRecord::release(
                release_state,
                identity,
                Some(&immutable),
            ))?;
            let active = self
                .active
                .remove(&identity.allocation_id)
                .expect("active allocation was just observed");

            let mut stop_result = Ok(());
            let cleanup_safe = match active {
                ActiveAllocation::Live { allocation, .. } => {
                    let stopped = self.factory.stop(allocation).await;
                    let shutdown_succeeded = stopped.shutdown_succeeded();
                    stop_result = stopped.into_result();
                    if !shutdown_succeeded {
                        self.retired.insert(
                            identity.allocation_id,
                            RetiredAllocation {
                                identity: identity.clone(),
                                immutable: Some(immutable.clone()),
                                release_pending: true,
                                cleanup_safe: false,
                            },
                        );
                        return stop_result.map(|()| VmAllocationChange::Changed);
                    }
                    true
                }
                ActiveAllocation::Recovered { cleanup_safe, .. } => cleanup_safe,
            };

            if !cleanup_safe {
                let error = VmHostError::State(format!(
                    "allocation {} release remains pending because its recovered VM was not proven stopped",
                    identity.allocation_id
                ));
                self.retired.insert(
                    identity.allocation_id,
                    RetiredAllocation {
                        identity: identity.clone(),
                        immutable: Some(immutable.clone()),
                        release_pending: true,
                        cleanup_safe: false,
                    },
                );
                return Err(error);
            }

            let stopped_record = self.persist(&AllocationRecord::release(
                AllocationRecordState::ReleasingStopped,
                identity,
                Some(&immutable),
            ));
            let cleanup = self
                .finish_stopped_release(identity, Some(immutable))
                .await
                .map(|_| ());
            combine_failures([stop_result, stopped_record, cleanup])
                .map(|()| VmAllocationChange::Changed)
        }

        async fn finish_stopped_release(
            &mut self,
            identity: &VmAllocationIdentity,
            immutable: Option<ImmutableAllocation>,
        ) -> Result<VmAllocationChange, VmHostError> {
            let cleanup = self.factory.release_recovered(identity).await;
            let outcome = match cleanup {
                Ok(()) => self.persist(&AllocationRecord::release(
                    AllocationRecordState::Released,
                    identity,
                    None,
                )),
                Err(error) => Err(error),
            };
            self.retired.insert(
                identity.allocation_id,
                RetiredAllocation {
                    identity: identity.clone(),
                    immutable: if outcome.is_err() { immutable } else { None },
                    release_pending: outcome.is_err(),
                    cleanup_safe: true,
                },
            );
            outcome.map(|()| VmAllocationChange::Changed)
        }

        async fn complete_startup_releases(&mut self) -> Result<(), VmHostError> {
            let pending = self
                .retired
                .values()
                .filter(|allocation| allocation.release_pending && allocation.cleanup_safe)
                .map(|allocation| allocation.identity.clone())
                .collect::<Vec<_>>();
            let mut first_error = None;
            for identity in pending {
                if let Err(error) = self.release(&identity).await {
                    first_error.get_or_insert(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        }

        fn acknowledge_released(
            &mut self,
            identity: &VmAllocationIdentity,
        ) -> Result<(), VmHostError> {
            let retired = self.retired.get(&identity.allocation_id).ok_or_else(|| {
                VmHostError::State(format!(
                    "acknowledged allocation {} has no released journal record",
                    identity.allocation_id
                ))
            })?;
            if &retired.identity != identity || retired.release_pending {
                return Err(VmHostError::ImmutableConflict {
                    allocation_id: identity.allocation_id,
                });
            }
            if let Some(store) = &self.store {
                store.remove(identity.allocation_id)?;
            }
            self.retired.remove(&identity.allocation_id);
            Ok(())
        }

        fn reconcile(&self) -> Vec<VmAllocationIdentity> {
            self.active
                .values()
                .filter_map(|allocation| match allocation {
                    ActiveAllocation::Live { immutable, .. } => Some(immutable.identity.clone()),
                    ActiveAllocation::Recovered { .. } => None,
                })
                .collect()
        }

        async fn shutdown(&mut self) -> Result<(), VmHostError> {
            let mut first_error = None;
            let active = std::mem::take(&mut self.active);
            for (allocation_id, active) in active {
                let (immutable, stopped) = match active {
                    ActiveAllocation::Live {
                        immutable,
                        allocation,
                        ..
                    } => (immutable, self.factory.stop(allocation).await),
                    ActiveAllocation::Recovered {
                        immutable,
                        cleanup_safe,
                    } => {
                        self.active.insert(
                            allocation_id,
                            ActiveAllocation::Recovered {
                                immutable,
                                cleanup_safe,
                            },
                        );
                        continue;
                    }
                };
                let shutdown_succeeded = stopped.shutdown_succeeded();
                if let Err(error) = stopped.into_result() {
                    first_error.get_or_insert(error);
                }
                let cleanup_safe = if shutdown_succeeded {
                    if let Err(error) = self.persist(&AllocationRecord::tracked(
                        AllocationRecordState::Stopped,
                        &immutable,
                    )) {
                        first_error.get_or_insert(error);
                    }
                    true
                } else {
                    false
                };
                self.active.insert(
                    allocation_id,
                    ActiveAllocation::Recovered {
                        immutable,
                        cleanup_safe,
                    },
                );
            }
            first_error.map_or(Ok(()), Err)
        }
    }

    struct VmAllocationFactory {
        template_root: PathBuf,
        allocation_directory: PathBuf,
        hand_template: vm_hand::VmHandConfig,
        spare_directory: PathBuf,
        spare: std::sync::Mutex<Option<WarmSpare>>,
    }

    /// A private VM that has never received an allocation credential or user work.
    struct PreparedVm {
        hand: vm_hand::VmHand,
        directory: tempfile::TempDir,
    }

    struct PreparingSpare(tokio::task::JoinHandle<Result<PreparedVm, ProvisionFailure>>);
    impl Drop for PreparingSpare {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    enum WarmSpare {
        Preparing(PreparingSpare),
        Ready(Box<PreparedVm>),
    }
    impl WarmSpare {
        async fn ready(self) -> Result<PreparedVm, ProvisionFailure> {
            match self {
                Self::Ready(vm) => Ok(*vm),
                Self::Preparing(mut task) => (&mut task.0).await.map_err(|error| {
                    ProvisionFailure::unproven(VmHostError::Resource(format!(
                        "VM spare preparation failed: {error}"
                    )))
                })?,
            }
        }
    }

    impl VmAllocationFactory {
        fn fill_spare(&self) {
            let mut spare = self.spare.lock().unwrap();
            if spare.is_some() {
                return;
            }
            let template = self.template_root.clone();
            let directory = self.spare_directory.clone();
            let mut config = self.hand_template.clone();
            *spare = Some(WarmSpare::Preparing(PreparingSpare(tokio::spawn(
                async move {
                    let started = Instant::now();
                    let directory = tempfile::Builder::new()
                        .prefix("vm-")
                        .tempdir_in(directory)
                        .map_err(|error| {
                            ProvisionFailure::stopped(VmHostError::Resource(error.to_string()))
                        })?;
                    let root = directory.path().join("root.ext4");
                    config.overlay_lower = prepare_factory_root(template, root.clone())
                        .await
                        .map_err(ProvisionFailure::stopped)?;
                    tracing::info!(target: "nanocodex2", stage = "vm.spare.root", elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                    config.rootfs = root;
                    let mut hand = vm_hand::VmHand::start_config(&config)
                        .await
                        .map_err(vm_hand_start_failure)?;
                    tracing::info!(target: "nanocodex2", stage = "vm.spare.guest", elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                    if let Err(error) = hand.prepare_desktop().await {
                        let stopped = hand
                            .shutdown()
                            .await
                            .map_err(|error| VmHostError::Resource(error.to_string()));
                        let cleanup_safe = stopped.is_ok();
                        return Err(ProvisionFailure {
                            error: combine_pair(
                                Err(VmHostError::Resource(error.to_string())),
                                stopped,
                            )
                            .expect_err("desktop preparation failed"),
                            cleanup_safe,
                        });
                    }
                    tracing::info!(target: "nanocodex2", stage = "vm.spare.ready", elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                    "Fresh VM spare ready without account or allocation credentials");
                    Ok(PreparedVm { hand, directory })
                },
            ))));
        }

        fn take_spare(&self) -> Option<WarmSpare> {
            self.spare.lock().unwrap().take()
        }

        async fn discard_spare(&self) -> Result<(), VmHostError> {
            let Some(spare) = self.take_spare() else {
                return Ok(());
            };
            let ready = match spare {
                WarmSpare::Ready(vm) => Some(*vm),
                WarmSpare::Preparing(mut task) => {
                    task.0.abort();
                    match (&mut task.0).await {
                        Ok(Ok(vm)) => Some(vm),
                        Ok(Err(failure)) => return Err(failure.error),
                        Err(error) if error.is_cancelled() => None,
                        Err(error) => return Err(VmHostError::Resource(error.to_string())),
                    }
                }
            };
            if let Some(PreparedVm { hand, directory }) = ready {
                let result = hand
                    .shutdown()
                    .await
                    .map_err(|error| VmHostError::Resource(error.to_string()));
                drop(directory);
                result?;
            }
            Ok(())
        }
    }

    /// Atomically refuse any existing durable root, including symlinks. A spare
    /// is consumed once; recovered allocations always boot their retained disk.
    fn adopt_spare_root(source: &Path, destination: &Path) -> Result<(), VmHostError> {
        let source_lower = overlay_lower_path(source);
        let destination_lower = overlay_lower_path(destination);
        let has_lower = inspect_overlay_lower(&source_lower)?.is_some();
        if has_lower {
            fs::hard_link(&source_lower, &destination_lower)
                .map_err(|error| VmHostError::State(error.to_string()))?;
            sync_directory(destination.parent().expect("allocation directory"))?;
        }
        if let Err(error) = fs::hard_link(source, destination) {
            if has_lower {
                let _ = fs::remove_file(&destination_lower);
            }
            return Err(VmHostError::State(format!(
                "failed to claim VM spare root: {error}"
            )));
        }
        if let Err(error) = fs::remove_file(source) {
            let _ = fs::remove_file(destination);
            if has_lower {
                let _ = fs::remove_file(&destination_lower);
            }
            return Err(VmHostError::State(format!(
                "failed to retire spare root name: {error}"
            )));
        }
        if has_lower {
            fs::remove_file(&source_lower)
                .map_err(|error| VmHostError::State(error.to_string()))?;
        }
        sync_directory(destination.parent().expect("allocation directory"))?;
        sync_directory(source.parent().expect("spare directory"))
    }

    impl AllocationFactory for VmAllocationFactory {
        type Allocation = VmAllocation;

        fn provision<'a>(
            &'a self,
            spec: VmAllocationSpec,
            cancellation: OperationCancellation,
        ) -> BoxFuture<'a, Result<Self::Allocation, ProvisionFailure>> {
            Box::pin(async move {
                let started = Instant::now();
                let allocation_id = spec.identity.allocation_id;
                tracing::info!(target: "nanocodex2", stage = "vm.provision.start", %allocation_id);
                let private_root = self
                    .allocation_directory
                    .join(format!("{}.ext4", spec.identity.allocation_id));
                let mut config = self.hand_template.clone();
                config.rootfs = private_root.clone();
                config.machine_id = spec.identity.machine_id.clone();
                config.machine_name = spec.identity.machine_id;
                cancellation.check().map_err(ProvisionFailure::stopped)?;
                let fresh = match fs::symlink_metadata(&private_root) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                    Ok(_) => false,
                    Err(error) => {
                        return Err(ProvisionFailure::stopped(VmHostError::State(
                            error.to_string(),
                        )));
                    }
                };
                // Recovery must boot the retained upper. Keep the unrelated fresh
                // spare available instead of shutting it down on this hot path.
                let unstarted = fresh
                    && inspect_overlay_lower(&overlay_lower_path(&private_root))
                        .map_err(ProvisionFailure::stopped)?
                        .is_none();
                let spare = if unstarted { self.take_spare() } else { None };
                let mut prepared = None;
                if let Some(spare) = spare {
                    match spare.ready().await {
                        Ok(mut vm) if fresh => {
                            match vm
                                .hand
                                .assign_machine(&config.machine_id, &config.machine_name)
                                .await
                            {
                                Ok(()) => {
                                    if let Err(error) = adopt_spare_root(
                                        &vm.directory.path().join("root.ext4"),
                                        &private_root,
                                    ) {
                                        let shutdown = vm.hand.shutdown().await;
                                        return Err(match shutdown {
                                            Ok(()) => ProvisionFailure::stopped(error),
                                            Err(shutdown) => {
                                                ProvisionFailure::unproven(VmHostError::Resource(
                                                    format!("{error}; {shutdown}"),
                                                ))
                                            }
                                        });
                                    }
                                    prepared = Some(vm.hand);
                                    tracing::info!(target: "nanocodex2", stage = "vm.provision.spare_claimed", %allocation_id,
                                        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                                }
                                Err(error) => {
                                    tracing::warn!(target: "nanocodex2", %error, "Discarding unhealthy VM spare");
                                    vm.hand.shutdown().await.map_err(|error| {
                                        ProvisionFailure::unproven(VmHostError::Resource(
                                            error.to_string(),
                                        ))
                                    })?;
                                }
                            }
                        }
                        Ok(vm) => {
                            vm.hand.shutdown().await.map_err(|error| {
                                ProvisionFailure::unproven(VmHostError::Resource(error.to_string()))
                            })?;
                        }
                        Err(failure) if failure.cleanup_safe => {
                            tracing::warn!(target: "nanocodex2", error = %failure.error, "VM spare unavailable; using cold startup");
                        }
                        Err(failure) => return Err(failure),
                    }
                }
                let warm = prepared.is_some();
                let mut hand = if let Some(hand) = prepared {
                    hand
                } else {
                    config.overlay_lower =
                        prepare_factory_root(self.template_root.clone(), private_root.clone())
                            .await
                            .map_err(ProvisionFailure::stopped)?;
                    tracing::info!(target: "nanocodex2", stage = "vm.provision.root", %allocation_id, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                    cancellation.check().map_err(ProvisionFailure::stopped)?;
                    vm_hand::VmHand::start_config(&config)
                        .await
                        .map_err(vm_hand_start_failure)?
                };
                tracing::info!(target: "nanocodex2", stage = "vm.provision.guest", %allocation_id, warm, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
                if cancellation.is_cancelled() {
                    let shutdown = hand
                        .shutdown()
                        .await
                        .map_err(|error| VmHostError::Resource(error.to_string()));
                    let shutdown_succeeded = shutdown.is_ok();
                    let error = combine_pair(Err(VmHostError::Cancelled), shutdown)
                        .expect_err("cancellation is always an error");
                    return Err(if shutdown_succeeded {
                        ProvisionFailure::stopped(error)
                    } else {
                        ProvisionFailure::unproven(error)
                    });
                }
                // Desktop startup and the tools WebSocket have independent
                // readiness. Start both now, but publish the allocation only
                // when both succeeded. A failed screen still tears down any
                // tools route that connected in the meantime.
                let tools = hand.tools();
                let machine = hand.machine().clone();
                let desktop = async {
                    let result = hand
                        .start_desktop(&spec.attachment_target)
                        .await
                        .map_err(|error| VmHostError::Resource(error.to_string()));
                    tracing::info!(target: "nanocodex2", stage = "vm.provision.desktop", %allocation_id, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, success = result.is_ok());
                    result
                };
                let attachment = async {
                    let result =
                        connect_vm_tools(tools, machine, spec.attachment_target.clone()).await;
                    tracing::info!(target: "nanocodex2", stage = "vm.provision.attached", %allocation_id, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, success = result.is_ok());
                    result
                };
                let attached = match tokio::join!(desktop, attachment) {
                    (Ok(()), result) => result,
                    (Err(error), attachment) => {
                        if let Ok(attachment) = attachment {
                            let _ = attachment.detach().await;
                        }
                        Err(error)
                    }
                };
                match attached {
                    Ok(attachment) => {
                        if cancellation.is_cancelled() {
                            let stopped = VmAllocation { hand, attachment }.stop().await;
                            let shutdown_succeeded = stopped.shutdown_succeeded();
                            let error =
                                combine_pair(Err(VmHostError::Cancelled), stopped.into_result())
                                    .expect_err("cancellation is always an error");
                            Err(if shutdown_succeeded {
                                ProvisionFailure::stopped(error)
                            } else {
                                ProvisionFailure::unproven(error)
                            })
                        } else {
                            Ok(VmAllocation { hand, attachment })
                        }
                    }
                    Err(error) => {
                        let shutdown = hand
                            .shutdown()
                            .await
                            .map_err(|error| VmHostError::Resource(error.to_string()));
                        let shutdown_succeeded = shutdown.is_ok();
                        let error = combine_pair(Err(error), shutdown)
                            .expect_err("attachment failure is always an error");
                        Err(if shutdown_succeeded {
                            ProvisionFailure::stopped(error)
                        } else {
                            ProvisionFailure::unproven(error)
                        })
                    }
                }
            })
        }

        fn refresh_attachment<'a>(
            &'a self,
            allocation: &'a mut Self::Allocation,
            target: AttachmentTarget,
            cancellation: OperationCancellation,
        ) -> BoxFuture<'a, Result<(), VmHostError>> {
            Box::pin(allocation.refresh_attachment(target, cancellation))
        }

        fn stop<'a>(&'a self, allocation: Self::Allocation) -> BoxFuture<'a, StopOutcome> {
            Box::pin(allocation.stop())
        }

        fn release_recovered<'a>(
            &'a self,
            identity: &'a VmAllocationIdentity,
        ) -> BoxFuture<'a, Result<(), VmHostError>> {
            Box::pin(remove_private_root(
                self.allocation_directory
                    .join(format!("{}.ext4", identity.allocation_id)),
            ))
        }
    }

    fn vm_hand_start_failure(error: ManagedError) -> ProvisionFailure {
        // VmHand startup validates before spawning and VmWorkspace forcibly
        // terminates a session on every bounded launch failure. No live VMM
        // escapes an Err, so durable redrive may safely reuse the root.
        ProvisionFailure::stopped(VmHostError::Resource(error.to_string()))
    }

    struct VmAllocation {
        hand: vm_hand::VmHand,
        attachment: Attachment,
    }

    impl VmAllocation {
        async fn refresh_attachment(
            &mut self,
            target: AttachmentTarget,
            cancellation: OperationCancellation,
        ) -> Result<(), VmHostError> {
            let attachment = refresh_desktop_before_attachment(
                async {
                    self.hand
                        .refresh_desktop(&target)
                        .await
                        .map_err(|error| VmHostError::Resource(error.to_string()))
                },
                connect_vm_attachment(&self.hand, target.clone()),
                &cancellation,
            )
            .await?;
            if cancellation.is_cancelled() {
                let detached = attachment
                    .detach()
                    .await
                    .map_err(|error| VmHostError::Resource(error.to_string()));
                return combine_pair(Err(VmHostError::Cancelled), detached);
            }
            let previous = std::mem::replace(&mut self.attachment, attachment);
            if let Err(error) = previous.detach().await {
                tracing::warn!(
                    target: "nanocodex2",
                    error = %error,
                    "previous VM attachment failed to detach after credential refresh"
                );
            }
            Ok(())
        }

        async fn stop(self) -> StopOutcome {
            let Self { hand, attachment } = self;
            let (attachment, shutdown) =
                tokio::join!(detach_stopping_vm(attachment), hand.shutdown());
            StopOutcome {
                attachment,
                shutdown: shutdown.map_err(|error| VmHostError::Resource(error.to_string())),
            }
        }
    }

    async fn detach_stopping_vm(attachment: Attachment) -> Result<(), VmHostError> {
        match attachment.detach().await {
            // An authoritative release already requires this VM to stop. The
            // broker can fence its attachment first (e.g. agent deletion).
            // detach still waits for runtime cleanup; a prior fence does not
            // require reconnecting the factory or retrying a successful release.
            Ok(()) | Err(AttachmentError::Fenced(_)) => Ok(()),
            Err(error) => Err(VmHostError::Resource(error.to_string())),
        }
    }

    // Both routes use the newly issued, epoch-bound allocation capability.
    // Deliver it to the retained desktop before waiting for the tools socket,
    // so screen publication can recover during that independent handshake.
    async fn refresh_desktop_before_attachment<D, A, T>(
        desktop: D,
        attachment: A,
        cancellation: &OperationCancellation,
    ) -> Result<T, VmHostError>
    where
        D: Future<Output = Result<(), VmHostError>>,
        A: Future<Output = Result<T, VmHostError>>,
    {
        cancellation.check()?;
        desktop.await?;
        cancellation.check()?;
        tracing::info!(target: "nanocodex2", "VM desktop credential refreshed before tools reconnect");
        attachment.await
    }

    async fn connect_vm_attachment(
        hand: &vm_hand::VmHand,
        target: AttachmentTarget,
    ) -> Result<Attachment, VmHostError> {
        connect_vm_tools(hand.tools(), hand.machine().clone(), target).await
    }

    async fn connect_vm_tools(
        tools: nanocodex_tools::Tools,
        machine: nanocodex_tools::attachment::AttachmentMachine,
        target: AttachmentTarget,
    ) -> Result<Attachment, VmHostError> {
        let connector = tools
            .attach(target)
            .metadata(AttachmentMetadata::machine(machine));
        match tokio::time::timeout(ATTACHMENT_CONNECT_TIMEOUT, connector.connect()).await {
            Ok(Ok((attachment, _events))) => Ok(attachment),
            Ok(Err(error)) => Err(VmHostError::Resource(format!(
                "failed to attach provisioned VM: {error}"
            ))),
            Err(_) => Err(VmHostError::Resource(format!(
                "timed out after {}s attaching provisioned VM",
                ATTACHMENT_CONNECT_TIMEOUT.as_secs()
            ))),
        }
    }

    fn combine_failures<const N: usize>(
        results: [Result<(), VmHostError>; N],
    ) -> Result<(), VmHostError> {
        let failures = results
            .into_iter()
            .filter_map(Result::err)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(VmHostError::Resource(failures.join("; ")))
        }
    }

    fn combine_pair(
        first: Result<(), VmHostError>,
        second: Result<(), VmHostError>,
    ) -> Result<(), VmHostError> {
        combine_failures([first, second])
    }

    #[cfg(target_os = "linux")]
    fn cache_overlay_template(source: File, state: &Path) -> Result<File, VmHostError> {
        let metadata = source
            .metadata()
            .map_err(|error| VmHostError::State(error.to_string()))?;
        let identity = |metadata: &fs::Metadata| {
            format!(
                "{}-{}-{}-{}-{}-{}-{}",
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec()
            )
        };
        let source_identity = identity(&metadata);
        let directory = state.join("images");
        fs::create_dir_all(&directory).map_err(|error| VmHostError::State(error.to_string()))?;
        let directory_metadata = fs::symlink_metadata(&directory)
            .map_err(|error| VmHostError::State(error.to_string()))?;
        if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
            return Err(VmHostError::State(
                "VM image cache must be a real directory".into(),
            ));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| VmHostError::State(error.to_string()))?;
        let cached = directory.join(format!("{source_identity}.ext4"));
        let started = Instant::now();
        let fresh = match fs::symlink_metadata(&cached) {
            Ok(existing) if existing.is_file() && existing.len() == metadata.len() => false,
            Ok(_) => {
                return Err(VmHostError::State(
                    "VM cached base has invalid kind or length".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(VmHostError::State(error.to_string())),
        };
        if fresh {
            // Root-owned installed images cannot be hard-linked by the service
            // under Linux protected_hardlinks. Copy once per stable image identity,
            // then retain service-owned immutable bases across factory restarts.
            clone_private_root_blocking(&locked_file_path(&source), &cached)?;
            if identity(
                &source
                    .metadata()
                    .map_err(|error| VmHostError::State(error.to_string()))?,
            ) != source_identity
            {
                let _ = fs::remove_file(&cached);
                return Err(VmHostError::State(
                    "VM template changed while caching".into(),
                ));
            }
            fs::set_permissions(&cached, fs::Permissions::from_mode(0o400))
                .map_err(|error| VmHostError::State(error.to_string()))?;
            File::open(&cached)
                .and_then(|file| file.sync_all())
                .map_err(|error| VmHostError::State(error.to_string()))?;
            sync_directory(&directory)?;
        }
        let file = File::open(&cached).map_err(|error| VmHostError::State(error.to_string()))?;
        fs2::FileExt::try_lock_shared(&file)
            .map_err(|error| VmHostError::State(error.to_string()))?;
        tracing::info!(target: "nanocodex2", stage = "vm.template.cached", fresh, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0);
        Ok(file)
    }

    fn overlay_lower_path(root: &Path) -> PathBuf {
        root.with_extension("lower.ext4")
    }

    fn inspect_overlay_lower(path: &Path) -> Result<Option<PathBuf>, VmHostError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(path.to_path_buf())),
            Ok(_) => Err(VmHostError::State(
                "overlay lower must be a regular file".into(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(VmHostError::State(error.to_string())),
        }
    }

    async fn prepare_factory_root(
        template: PathBuf,
        destination: PathBuf,
    ) -> Result<Option<PathBuf>, VmHostError> {
        tokio::task::spawn_blocking(move || prepare_factory_root_blocking(&template, &destination))
            .await
            .map_err(|error| VmHostError::Resource(error.to_string()))?
    }

    fn prepare_factory_root_blocking(
        template: &Path,
        destination: &Path,
    ) -> Result<Option<PathBuf>, VmHostError> {
        let lower = overlay_lower_path(destination);
        // Existing allocations retain their original disk layout and pinned base.
        // Never reinterpret a retained writable disk after an application update.
        match fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => {
                return inspect_overlay_lower(&lower);
            }
            Ok(_) => {
                return Err(VmHostError::State(
                    "allocation root must be a private regular file".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(VmHostError::State(error.to_string())),
        }
        #[cfg(target_os = "linux")]
        {
            let parent = destination
                .parent()
                .ok_or_else(|| VmHostError::State("allocation root has no parent".into()))?;
            fs::create_dir_all(parent).map_err(|error| VmHostError::State(error.to_string()))?;
            // The lower hard link pins the exact locked image inode across image
            // replacement, installer cleanup, host restarts, and allocation recovery.
            // Publish it before the upper so a crash cannot expose an ambiguous root.
            if inspect_overlay_lower(&lower)?.is_none() {
                let linked = nix::unistd::linkat(
                    nix::fcntl::AT_FDCWD,
                    template,
                    nix::fcntl::AT_FDCWD,
                    &lower,
                    nix::fcntl::AtFlags::AT_SYMLINK_FOLLOW,
                );
                if linked == Err(nix::errno::Errno::EXDEV) {
                    // Custom state directories may live on another filesystem.
                    // Keep the existing portable copy path in that configuration.
                    clone_private_root_blocking(template, destination)?;
                    return Ok(None);
                }
                linked.map_err(|error| {
                    VmHostError::State(format!("failed to pin overlay lower: {error}"))
                })?;
                sync_directory(parent)?;
            }
            let bytes = fs::metadata(&lower)
                .map_err(|error| VmHostError::State(error.to_string()))?
                .len();
            let temporary = tempfile::Builder::new()
                .prefix(".vm-upper-")
                .tempdir_in(parent)
                .map_err(|error| VmHostError::State(error.to_string()))?;
            let upper = temporary.path().join("upper.ext4");
            nanocodex_vm::host::create_sparse_overlay_disk(&upper, bytes)
                .map_err(|error| VmHostError::Resource(error.to_string()))?;
            File::open(&upper)
                .and_then(|file| file.sync_all())
                .map_err(|error| VmHostError::State(error.to_string()))?;
            fs::hard_link(&upper, destination)
                .map_err(|error| VmHostError::State(error.to_string()))?;
            drop(temporary);
            sync_directory(parent)?;
            Ok(Some(lower))
        }
        #[cfg(not(target_os = "linux"))]
        {
            clone_private_root_blocking(template, destination)?;
            Ok(None)
        }
    }

    fn clone_private_root_blocking(template: &Path, destination: &Path) -> Result<(), VmHostError> {
        let metadata = fs::metadata(template).map_err(|error| {
            VmHostError::Configuration(format!(
                "failed to read VM template {}: {error}",
                template.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(VmHostError::Configuration(format!(
                "VM template must be a raw ext4 file: {}",
                template.display()
            )));
        }
        match fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(VmHostError::State(format!(
                    "allocation root must not be a symlink: {}",
                    destination.display()
                )));
            }
            Ok(metadata) if metadata.is_file() => {
                if metadata.nlink() != 1 {
                    return Err(VmHostError::State(format!(
                        "allocation root must not have hard links: {}",
                        destination.display()
                    )));
                }
                return Ok(());
            }
            Ok(_) => {
                return Err(VmHostError::State(format!(
                    "allocation root is not a file: {}",
                    destination.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VmHostError::State(format!(
                    "failed to inspect allocation root {}: {error}",
                    destination.display()
                )));
            }
        }
        let executable = std::env::current_exe().map_err(|error| {
            VmHostError::State(format!("failed to resolve VM host executable: {error}"))
        })?;
        let parent = destination.parent().ok_or_else(|| {
            VmHostError::State("allocation root has no parent directory".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            VmHostError::State(format!(
                "failed to create allocation root directory: {error}"
            ))
        })?;
        let temporary = NamedTempFile::new_in(parent).map_err(|error| {
            VmHostError::State(format!("failed to reserve temporary VM root: {error}"))
        })?;
        let temporary = temporary.into_temp_path();
        fs::remove_file(&temporary).map_err(|error| {
            VmHostError::State(format!("failed to prepare temporary VM root: {error}"))
        })?;
        nanocodex_vm::VmWorkspaceBuilder::private_from(template, &temporary, executable).map_err(
            |error| VmHostError::Resource(format!("failed to clone private VM root: {error}")),
        )?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|error| {
            VmHostError::State(format!("failed to secure private VM root: {error}"))
        })?;
        File::open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                VmHostError::State(format!("failed to sync private VM root: {error}"))
            })?;
        fs::rename(&temporary, destination).map_err(|error| {
            VmHostError::State(format!("failed to publish private VM root: {error}"))
        })?;
        sync_directory(parent)
    }

    async fn remove_private_root(path: PathBuf) -> Result<(), VmHostError> {
        tokio::task::spawn_blocking(move || {
            let parent = path.parent().ok_or_else(|| {
                VmHostError::State("allocation root has no parent directory".to_owned())
            })?;
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(VmHostError::State(format!(
                        "refusing to remove symlinked VM root {}",
                        path.display()
                    )));
                }
                Ok(metadata) if !metadata.is_file() => {
                    return Err(VmHostError::State(format!(
                        "VM root is not a file: {}",
                        path.display()
                    )));
                }
                Ok(metadata) => {
                    if metadata.nlink() != 1 {
                        return Err(VmHostError::State(format!(
                            "refusing to remove multiply linked VM root {}",
                            path.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(VmHostError::State(format!(
                        "failed to inspect released VM root {}: {error}",
                        path.display()
                    )));
                }
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(VmHostError::State(format!(
                        "failed to remove released VM root {}: {error}",
                        path.display()
                    )));
                }
            }
            // The upper disappears durably before the lower; recovery can never
            // mistake a retained overlay upper for a standalone root.
            sync_directory(parent)?;
            if let Some(lower) = inspect_overlay_lower(&overlay_lower_path(&path))? {
                fs::remove_file(lower).map_err(|error| VmHostError::State(error.to_string()))?;
            }
            sync_directory(parent)
        })
        .await
        .map_err(|error| VmHostError::Resource(format!("VM root cleanup task failed: {error}")))?
    }

    /// Bounded owner of all locally ready VM allocations for one host lease.
    pub(crate) struct VmHost {
        state: VmHostState,
        _template_lock: File,
        supervisor: AllocationSupervisor<VmAllocationFactory>,
        warm_spare: bool,
    }

    impl VmHost {
        pub(crate) fn open(config: &Host) -> Result<Self, VmHostError> {
            let state = VmHostState::open(&config.state_dir, config.host_id)?;
            let template_root = config.vm_template.canonicalize().map_err(|error| {
                VmHostError::Configuration(format!(
                    "failed to resolve VM template {}: {error}",
                    config.vm_template.display()
                ))
            })?;
            let template_metadata = template_root.metadata().map_err(|error| {
                VmHostError::Configuration(format!(
                    "failed to inspect VM template {}: {error}",
                    template_root.display()
                ))
            })?;
            if !template_metadata.is_file() {
                return Err(VmHostError::Configuration(format!(
                    "VM template must be a raw ext4 file: {}",
                    template_root.display()
                )));
            }
            let template_lock =
                OpenOptions::new()
                    .read(true)
                    .open(&template_root)
                    .map_err(|error| {
                        VmHostError::Configuration(format!(
                            "failed to open VM template {}: {error}",
                            template_root.display()
                        ))
                    })?;
            fs2::FileExt::try_lock_shared(&template_lock).map_err(|error| {
                VmHostError::Configuration(format!(
                    "VM template is in use as a writable VM root ({}): {error}",
                    template_root.display()
                ))
            })?;
            let hand_template = vm_hand::VmHandConfig {
                rootfs: config.vm_template.clone(),
                overlay_lower: None,
                docker: None,
                vm_guest_runtime: Some(config.vm_guest_runtime.clone()),
                vm_cache: config.vm_cache.clone(),
                vm_firmware: config.vm_firmware.clone(),
                vm_workspace: config.vm_workspace.clone(),
                vm_cpus: config.vm_cpus,
                vm_memory_mib: config.vm_memory_mib,
                vm_gpu: config.vm_gpu,
                vm_shell: config.vm_shell.clone(),
                vm_no_network: config.vm_no_network,
                machine_id: "unprovisioned".to_owned(),
                machine_name: "Unprovisioned VM".to_owned(),
                browser: false,
                browser_executable: None,
            };
            vm_hand::VmHand::preflight_host_config(&hand_template)
                .map_err(|error| VmHostError::Configuration(error.to_string()))?;
            #[cfg(target_os = "linux")]
            let template_lock = cache_overlay_template(template_lock, &state.directory)?;
            let locked_template_root = locked_file_path(&template_lock);
            let factory = VmAllocationFactory {
                // Clone through the descriptor which owns the shared lock.
                // Replacing the configured pathname cannot redirect a later
                // allocation to a different inode.
                template_root: locked_template_root,
                allocation_directory: state.directory.join("allocations"),
                hand_template,
                spare_directory: state.directory.join("spares"),
                spare: std::sync::Mutex::new(None),
            };
            fs::create_dir_all(&factory.spare_directory)
                .map_err(|error| VmHostError::State(error.to_string()))?;
            let metadata = fs::symlink_metadata(&factory.spare_directory)
                .map_err(|error| VmHostError::State(error.to_string()))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(VmHostError::State(
                    "VM spares must use a real private directory".into(),
                ));
            }
            fs::set_permissions(&factory.spare_directory, fs::Permissions::from_mode(0o700))
                .map_err(|error| VmHostError::State(error.to_string()))?;
            // The host lock fences the predecessor; these directories contain
            // only unassigned temporary disks, never durable allocation roots.
            for entry in fs::read_dir(&factory.spare_directory)
                .map_err(|error| VmHostError::State(error.to_string()))?
            {
                let entry = entry.map_err(|error| VmHostError::State(error.to_string()))?;
                if entry.file_name().to_string_lossy().starts_with("vm-")
                    && entry
                        .file_type()
                        .map_err(|error| VmHostError::State(error.to_string()))?
                        .is_dir()
                {
                    fs::remove_dir_all(entry.path())
                        .map_err(|error| VmHostError::State(error.to_string()))?;
                }
            }
            let store = AllocationStore::new(state.directory.join("allocations"));
            let supervisor =
                AllocationSupervisor::with_store(config.max_vms, factory, store, &state)?;
            Ok(Self {
                state,
                _template_lock: template_lock,
                supervisor,
                warm_spare: config.warm_spare,
            })
        }

        // Validate the complete GPU recipe before accepting allocations. A
        // permanently broken driver must not enter the managed redrive loop.
        async fn preflight_gpu(&self) -> Result<(), ManagedError> {
            let factory = &self.supervisor.factory;
            if self.warm_spare && self.spare_capacity() {
                factory.fill_spare();
                let vm = factory
                    .take_spare()
                    .expect("started spare")
                    .ready()
                    .await
                    .map_err(|failure| failure.error)?;
                *factory.spare.lock().unwrap() = Some(WarmSpare::Ready(Box::new(vm)));
                return Ok(());
            }
            if !factory.hand_template.vm_gpu {
                return Ok(());
            }
            let temporary = tempfile::tempdir_in(&self.state.directory)
                .map_err(|error| ManagedError::Configuration(error.to_string()))?;
            let root = temporary.path().join("root.ext4");
            let mut config = factory.hand_template.clone();
            config.overlay_lower =
                prepare_factory_root(factory.template_root.clone(), root.clone()).await?;
            config.rootfs = root;
            config.vm_no_network = true;
            vm_hand::VmHand::start_config(&config)
                .await?
                .shutdown()
                .await
        }

        pub(crate) const fn host_id(&self) -> Uuid {
            self.state.host_id
        }

        pub(crate) const fn capacity(&self) -> u16 {
            self.supervisor.capacity
        }

        async fn provision_controlled(
            &mut self,
            spec: VmAllocationSpec,
            cancellation: OperationCancellation,
        ) -> Result<VmAllocationChange, VmHostError> {
            let result = self
                .supervisor
                .provision_controlled(spec, cancellation)
                .await;
            self.refill_spare();
            result
        }

        fn spare_capacity(&self) -> bool {
            self.supervisor.reserved_capacity() < usize::from(self.capacity())
        }

        fn refill_spare(&self) {
            if self.warm_spare && self.spare_capacity() {
                self.supervisor.factory.fill_spare();
            }
        }

        pub(crate) async fn release(
            &mut self,
            identity: &VmAllocationIdentity,
        ) -> Result<VmAllocationChange, VmHostError> {
            let result = self.supervisor.release(identity).await;
            self.refill_spare();
            result
        }

        async fn complete_startup_releases(&mut self) -> Result<(), VmHostError> {
            self.supervisor.complete_startup_releases().await
        }

        fn acknowledge_released(
            &mut self,
            identity: &VmAllocationIdentity,
        ) -> Result<(), VmHostError> {
            self.supervisor.acknowledge_released(identity)
        }

        pub(crate) fn reconcile(&self) -> Vec<VmAllocationIdentity> {
            self.supervisor.reconcile()
        }

        pub(crate) async fn shutdown(&mut self) -> Result<(), VmHostError> {
            let spare = self.supervisor.factory.discard_spare().await;
            combine_pair(self.supervisor.shutdown().await, spare)
        }
    }

    fn locked_file_path(file: &File) -> PathBuf {
        #[cfg(target_os = "linux")]
        let directory = PathBuf::from("/proc")
            .join(std::process::id().to_string())
            .join("fd");
        #[cfg(target_os = "macos")]
        let directory = PathBuf::from("/dev/fd");
        directory.join(file.as_raw_fd().to_string())
    }

    enum ControlAuth {
        Account {
            client: ManagedClient,
            scope: VmHostScope,
        },
        System {
            origin: String,
            token: String,
        },
    }

    impl ControlAuth {
        async fn connect(
            &self,
            host_id: Uuid,
            factory_name: &str,
            max_vms: u16,
            shape: VmShape,
        ) -> Result<VmHostConnection, ManagedError> {
            match self {
                Self::Account { client, scope } => {
                    client
                        .connect_vm_host(scope.clone(), host_id, factory_name, max_vms, shape)
                        .await
                }
                Self::System { origin, token } => {
                    connect_system_vm_host(
                        origin,
                        token.clone(),
                        host_id,
                        factory_name,
                        max_vms,
                        shape,
                    )
                    .await
                }
            }
        }
    }

    enum ConnectionOutcome {
        Reconnect { made_progress: bool },
        Shutdown,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PendingCommandKind {
        Provision,
        Release,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct PendingCommandIdentity {
        allocation_id: Uuid,
        kind: PendingCommandKind,
    }

    trait PendingLifecycleCommand {
        fn pending_identity(&self) -> PendingCommandIdentity;
    }

    impl PendingLifecycleCommand for VmHostCommand {
        fn pending_identity(&self) -> PendingCommandIdentity {
            match self {
                Self::Provision(command) => PendingCommandIdentity {
                    allocation_id: command.allocation_id(),
                    kind: PendingCommandKind::Provision,
                },
                Self::Release(command) => PendingCommandIdentity {
                    allocation_id: command.allocation_id(),
                    kind: PendingCommandKind::Release,
                },
                Self::Fenced(_) => unreachable!("fencing is never queued"),
            }
        }
    }

    struct PendingCommandQueue<T> {
        limit: usize,
        commands: VecDeque<T>,
    }

    impl<T: PendingLifecycleCommand> PendingCommandQueue<T> {
        fn new(max_vms: u16) -> Self {
            Self {
                // Reconciliation can release up to one old host snapshot while
                // a disjoint desired snapshot is being provisioned.
                limit: usize::from(max_vms).saturating_mul(2),
                commands: VecDeque::new(),
            }
        }

        fn push(&mut self, command: T, active: PendingCommandIdentity) -> Result<(), VmHostError> {
            let incoming = command.pending_identity();
            if incoming.allocation_id == active.allocation_id
                && (active.kind == PendingCommandKind::Release || incoming.kind == active.kind)
            {
                return Ok(());
            }
            if let Some(position) = self.commands.iter().position(|queued| {
                queued.pending_identity().allocation_id == incoming.allocation_id
            }) {
                let queued = self.commands[position].pending_identity();
                if queued.kind == PendingCommandKind::Release || queued.kind == incoming.kind {
                    return Ok(());
                }
                // A release supersedes the queued provision for the same
                // allocation. The inverse is never allowed.
                self.commands.remove(position);
            }
            if self.commands.len() >= self.limit {
                return Err(VmHostError::State(format!(
                    "pending VM host command queue exceeded its {}-allocation bound",
                    self.limit
                )));
            }
            self.commands.push_back(command);
            Ok(())
        }

        fn pop(&mut self) -> Option<T> {
            let release = self
                .commands
                .iter()
                .position(|command| command.pending_identity().kind == PendingCommandKind::Release);
            release
                .and_then(|position| self.commands.remove(position))
                .or_else(|| self.commands.pop_front())
        }
    }

    enum ControlledOperationOutcome<T> {
        Completed(T),
        Terminal {
            result: T,
            terminal: DeferredControlOutcome,
        },
    }

    enum DeferredControlOutcome {
        Reconnect,
        Shutdown,
        Error(ManagedError),
        Fenced(VmHostCommand),
    }

    enum ControlledOperationEvent<T, H, S, C> {
        Completed(T),
        Heartbeat(H),
        Shutdown(S),
        Control(C),
    }

    async fn next_controlled_operation_event<O, H, S, C>(
        operation: Pin<&mut O>,
        heartbeat: H,
        shutdown: S,
        control: C,
    ) -> ControlledOperationEvent<O::Output, H::Output, S::Output, C::Output>
    where
        O: Future + ?Sized,
        H: Future,
        S: Future,
        C: Future,
    {
        tokio::select! {
            biased;
            signal = shutdown => ControlledOperationEvent::Shutdown(signal),
            control = control => ControlledOperationEvent::Control(control),
            heartbeat = heartbeat => ControlledOperationEvent::Heartbeat(heartbeat),
            result = operation => ControlledOperationEvent::Completed(result),
        }
    }

    async fn drive_controlled_operation<O>(
        operation: O,
        cancellation: &OperationCancellation,
        active: PendingCommandIdentity,
        connection: &mut VmHostConnection,
        heartbeat: &mut tokio::time::Interval,
        pending: &mut PendingCommandQueue<VmHostCommand>,
    ) -> Result<ControlledOperationOutcome<O::Output>, ManagedError>
    where
        O: Future,
    {
        tokio::pin!(operation);
        loop {
            match next_controlled_operation_event(
                operation.as_mut(),
                heartbeat.tick(),
                super::super::service::shutdown_signal(),
                connection.next(),
            )
            .await
            {
                ControlledOperationEvent::Completed(result) => {
                    return Ok(ControlledOperationOutcome::Completed(result));
                }
                ControlledOperationEvent::Heartbeat(_) => {
                    if let Err(error) = connection.ping(None).await {
                        tracing::warn!(target: "nanocodex2", error = %error, "VM host heartbeat failed during allocation operation");
                        return Ok(finish_terminal_operation(
                            operation.as_mut(),
                            cancellation,
                            DeferredControlOutcome::Reconnect,
                        )
                        .await);
                    }
                }
                ControlledOperationEvent::Shutdown(signal) => {
                    let terminal = match signal {
                        Ok(()) => DeferredControlOutcome::Shutdown,
                        Err(error) => DeferredControlOutcome::Error(ManagedError::Configuration(
                            format!("failed to listen for shutdown: {error}"),
                        )),
                    };
                    return Ok(finish_terminal_operation(
                        operation.as_mut(),
                        cancellation,
                        terminal,
                    )
                    .await);
                }
                ControlledOperationEvent::Control(Ok(Some(
                    command @ (VmHostCommand::Provision(_) | VmHostCommand::Release(_)),
                ))) => {
                    if let Err(error) = pending.push(command, active) {
                        return Ok(finish_terminal_operation(
                            operation.as_mut(),
                            cancellation,
                            DeferredControlOutcome::Error(error.into()),
                        )
                        .await);
                    }
                }
                ControlledOperationEvent::Control(Ok(Some(command @ VmHostCommand::Fenced(_)))) => {
                    return Ok(finish_terminal_operation(
                        operation.as_mut(),
                        cancellation,
                        DeferredControlOutcome::Fenced(command),
                    )
                    .await);
                }
                ControlledOperationEvent::Control(Ok(None)) => {
                    return Ok(finish_terminal_operation(
                        operation.as_mut(),
                        cancellation,
                        DeferredControlOutcome::Reconnect,
                    )
                    .await);
                }
                ControlledOperationEvent::Control(Err(error @ ManagedError::Configuration(_))) => {
                    return Ok(finish_terminal_operation(
                        operation.as_mut(),
                        cancellation,
                        DeferredControlOutcome::Error(error),
                    )
                    .await);
                }
                ControlledOperationEvent::Control(Err(error)) => {
                    tracing::warn!(target: "nanocodex2", error = %error, "VM host control receive failed during allocation operation");
                    return Ok(finish_terminal_operation(
                        operation.as_mut(),
                        cancellation,
                        DeferredControlOutcome::Reconnect,
                    )
                    .await);
                }
            }
        }
    }

    async fn finish_terminal_operation<O>(
        operation: Pin<&mut O>,
        cancellation: &OperationCancellation,
        terminal: DeferredControlOutcome,
    ) -> ControlledOperationOutcome<O::Output>
    where
        O: Future + ?Sized,
    {
        cancellation.cancel();
        tracing::info!(
            target: "nanocodex2",
            "VM host entered terminal control mode; retaining lifecycle operation until cleanup completes"
        );
        let result = operation.await;
        ControlledOperationOutcome::Terminal { result, terminal }
    }

    async fn finish_deferred_control_outcome(
        host: &mut VmHost,
        operation: &'static str,
        result: Result<VmAllocationChange, VmHostError>,
        terminal: DeferredControlOutcome,
        made_progress: bool,
    ) -> Result<ConnectionOutcome, ManagedError> {
        if let Err(error) = result {
            tracing::error!(
                target: "nanocodex2",
                error = %error,
                operation,
                "VM host allocation operation failed after a terminal control outcome"
            );
        }
        match terminal {
            DeferredControlOutcome::Reconnect => Ok(ConnectionOutcome::Reconnect { made_progress }),
            DeferredControlOutcome::Shutdown => {
                host.shutdown().await?;
                Ok(ConnectionOutcome::Shutdown)
            }
            DeferredControlOutcome::Error(error) => Err(error),
            DeferredControlOutcome::Fenced(VmHostCommand::Fenced(fence)) => {
                Err(ManagedError::VmHost(format!(
                    "host lease fenced at epoch {}: {}",
                    fence.epoch(),
                    fence.reason()
                )))
            }
            DeferredControlOutcome::Fenced(_) => {
                unreachable!("only fenced commands are deferred as fencing outcomes")
            }
        }
    }

    pub(super) async fn run(config: Host) -> Result<(), ManagedError> {
        config.validate()?;
        let shape = VmShape::new(config.vm_cpus, config.vm_memory_mib)?;
        // Select authority before opening durable state. In particular, system
        // hosts never construct a ManagedClient or read an account API key.
        let auth = match config.scope {
            HostScope::User => ControlAuth::Account {
                client: client_from_environment(None)?,
                scope: VmHostScope::User,
            },
            HostScope::Agent => ControlAuth::Account {
                client: client_from_environment(None)?,
                scope: VmHostScope::Agent(
                    config
                        .agent
                        .clone()
                        .expect("validated agent scope has an agent ID"),
                ),
            },
            HostScope::System => ControlAuth::System {
                origin: managed_url_from_environment(None)?,
                token: system_host_token_from_environment()?,
            },
        };
        let mut host = VmHost::open(&config)?;
        let result = run_open_host(&config, shape, &auth, &mut host).await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => match host.shutdown().await {
                Ok(()) => Err(error),
                Err(shutdown) => Err(ManagedError::Configuration(format!(
                    "{error}; VM host shutdown also failed: {shutdown}"
                ))),
            },
        }
    }

    async fn run_open_host(
        config: &Host,
        shape: VmShape,
        auth: &ControlAuth,
        host: &mut VmHost,
    ) -> Result<(), ManagedError> {
        host.preflight_gpu().await?;
        host.complete_startup_releases().await?;
        tracing::info!(
            target: "nanocodex2",
            host_id = %host.host_id(),
            vm_max = host.capacity(),
            "VM host is ready for managed allocations"
        );

        let mut retry = INITIAL_RECONNECT_DELAY;
        loop {
            let connection = auth
                .connect(host.host_id(), &config.factory_name, host.capacity(), shape)
                .await;
            let mut connection = match connection {
                Ok(connection) => connection,
                Err(error @ ManagedError::Configuration(_)) => return Err(error),
                Err(error) => {
                    tracing::warn!(
                        target: "nanocodex2",
                        stage = "vm.host.reconnecting",
                        error = %error,
                        "VM host control connection failed; retrying"
                    );
                    let delay = next_reconnect_delay(&mut retry, false);
                    if wait_to_reconnect_or_shutdown(host, delay).await? {
                        return Ok(());
                    }
                    continue;
                }
            };
            let connected_at = tokio::time::Instant::now();
            match serve_connection(host, &mut connection).await? {
                ConnectionOutcome::Shutdown => return Ok(()),
                ConnectionOutcome::Reconnect { made_progress } => {
                    tracing::warn!(
                        target: "nanocodex2",
                        stage = "vm.host.reconnecting",
                        "VM host control connection closed; reconciling on reconnect"
                    );
                    let healthy =
                        made_progress || connected_at.elapsed() >= HEALTHY_CONNECTION_DURATION;
                    let delay = next_reconnect_delay(&mut retry, healthy);
                    if wait_to_reconnect_or_shutdown(host, delay).await? {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn serve_connection(
        host: &mut VmHost,
        connection: &mut VmHostConnection,
    ) -> Result<ConnectionOutcome, ManagedError> {
        let allocations = host
            .reconcile()
            .into_iter()
            .map(|allocation| {
                VmHostAllocationState::ready(
                    allocation.allocation_id,
                    allocation.generation,
                    allocation.machine_id,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Err(error) = connection.reconcile(&allocations).await {
            tracing::warn!(target: "nanocodex2", error = %error, "VM host reconciliation failed");
            return Ok(ConnectionOutcome::Reconnect {
                made_progress: false,
            });
        }

        super::super::service::ready();
        tracing::info!(target: "nanocodex2", stage = "vm.host.ready", "VM host registration reconciled");
        let first_heartbeat = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut heartbeat = tokio::time::interval_at(first_heartbeat, Duration::from_secs(20));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut pending_commands = PendingCommandQueue::new(host.capacity());
        let mut made_progress = false;
        loop {
            let command = if let Some(command) = pending_commands.pop() {
                command
            } else {
                tokio::select! {
                    signal = super::super::service::shutdown_signal() => {
                        signal.map_err(|error| ManagedError::Configuration(
                            format!("failed to listen for shutdown: {error}")
                        ))?;
                        host.shutdown().await?;
                        return Ok(ConnectionOutcome::Shutdown);
                    }
                    command = connection.next() => match command {
                        Ok(Some(command)) => command,
                        Ok(None) => return Ok(ConnectionOutcome::Reconnect { made_progress }),
                        Err(error @ ManagedError::Configuration(_)) => return Err(error),
                        Err(error) => {
                            tracing::warn!(target: "nanocodex2", error = %error, "VM host control receive failed");
                            return Ok(ConnectionOutcome::Reconnect { made_progress });
                        }
                    },
                    _ = heartbeat.tick() => {
                        if let Err(error) = connection.ping(None).await {
                            tracing::warn!(target: "nanocodex2", error = %error, "VM host heartbeat failed");
                            return Ok(ConnectionOutcome::Reconnect { made_progress });
                        }
                        continue;
                    }
                }
            };
            match command {
                VmHostCommand::Provision(provision) => {
                    let acknowledgement = provision.clone();
                    let machine_id = provision.machine_id().to_owned();
                    let spec = VmAllocationSpec::new(
                        provision.allocation_id(),
                        provision.slot(),
                        provision.generation(),
                        machine_id,
                        provision.into_attachment(),
                    );
                    let cancellation = OperationCancellation::default();
                    let active = PendingCommandIdentity {
                        allocation_id: acknowledgement.allocation_id(),
                        kind: PendingCommandKind::Provision,
                    };
                    match drive_controlled_operation(
                        host.provision_controlled(spec, cancellation.clone()),
                        &cancellation,
                        active,
                        connection,
                        &mut heartbeat,
                        &mut pending_commands,
                    )
                    .await?
                    {
                        ControlledOperationOutcome::Completed(Ok(_)) => {}
                        ControlledOperationOutcome::Completed(Err(error)) => {
                            tracing::error!(target: "nanocodex2", error = %error, "VM host provision failed; reconnecting for durable redrive");
                            return Ok(ConnectionOutcome::Reconnect { made_progress });
                        }
                        ControlledOperationOutcome::Terminal { result, terminal } => {
                            return finish_deferred_control_outcome(
                                host,
                                "provision",
                                result,
                                terminal,
                                made_progress,
                            )
                            .await;
                        }
                    }
                    if let Err(error) = connection.provisioned(&acknowledgement).await {
                        tracing::warn!(target: "nanocodex2", error = %error, "VM host provision acknowledgement failed");
                        return Ok(ConnectionOutcome::Reconnect { made_progress });
                    }
                    made_progress = true;
                }
                VmHostCommand::Release(release) => {
                    let identity = VmAllocationIdentity {
                        allocation_id: release.allocation_id(),
                        generation: release.generation(),
                        machine_id: release.machine_id().to_owned(),
                    };
                    let cancellation = OperationCancellation::default();
                    let active = PendingCommandIdentity {
                        allocation_id: identity.allocation_id,
                        kind: PendingCommandKind::Release,
                    };
                    match drive_controlled_operation(
                        host.release(&identity),
                        &cancellation,
                        active,
                        connection,
                        &mut heartbeat,
                        &mut pending_commands,
                    )
                    .await?
                    {
                        ControlledOperationOutcome::Completed(Ok(_)) => {}
                        ControlledOperationOutcome::Completed(Err(error)) => {
                            tracing::error!(target: "nanocodex2", error = %error, "VM host release failed; reconnecting for durable redrive");
                            return Ok(ConnectionOutcome::Reconnect { made_progress });
                        }
                        ControlledOperationOutcome::Terminal { result, terminal } => {
                            return finish_deferred_control_outcome(
                                host,
                                "release",
                                result,
                                terminal,
                                made_progress,
                            )
                            .await;
                        }
                    }
                    if let Err(error) = connection.released(&release).await {
                        tracing::warn!(target: "nanocodex2", error = %error, "VM host release acknowledgement failed");
                        return Ok(ConnectionOutcome::Reconnect { made_progress });
                    }
                    host.acknowledge_released(&identity)?;
                    made_progress = true;
                }
                VmHostCommand::Fenced(fence) => {
                    return Err(ManagedError::VmHost(format!(
                        "host lease fenced at epoch {}: {}",
                        fence.epoch(),
                        fence.reason()
                    )));
                }
            }
        }
    }

    async fn wait_to_reconnect_or_shutdown(
        host: &mut VmHost,
        delay: Duration,
    ) -> Result<bool, ManagedError> {
        tokio::select! {
            signal = super::super::service::shutdown_signal() => {
                signal.map_err(|error| ManagedError::Configuration(
                    format!("failed to listen for shutdown: {error}")
                ))?;
                host.shutdown().await?;
                Ok(true)
            }
            () = tokio::time::sleep(delay) => Ok(false),
        }
    }

    fn next_reconnect_delay(retry: &mut Duration, healthy: bool) -> Duration {
        if healthy {
            *retry = INITIAL_RECONNECT_DELAY;
        }
        let delay = *retry;
        *retry = retry.saturating_mul(2).min(MAX_RECONNECT_DELAY);
        delay
    }

    fn system_host_token_from_environment() -> Result<String, ManagedError> {
        match env::var(SYSTEM_HOST_TOKEN_ENV) {
            Ok(value) if !value.trim().is_empty() => Ok(value),
            Ok(_) => Err(ManagedError::Configuration(format!(
                "{SYSTEM_HOST_TOKEN_ENV} must not be empty"
            ))),
            Err(env::VarError::NotPresent) => Err(ManagedError::Configuration(format!(
                "{SYSTEM_HOST_TOKEN_ENV} must be set for --scope system"
            ))),
            Err(env::VarError::NotUnicode(_)) => Err(ManagedError::Configuration(format!(
                "{SYSTEM_HOST_TOKEN_ENV} must be valid Unicode"
            ))),
        }
    }

    #[cfg(test)]
    mod tests {
        use std::{
            future::{pending, ready},
            sync::{Arc, Mutex},
        };

        use super::*;

        #[tokio::test]
        async fn stopping_vm_accepts_an_already_fenced_attachment() {
            use futures_util::{SinkExt as _, StreamExt as _};
            use tokio_tungstenite::tungstenite::{
                Message,
                protocol::{CloseFrame, frame::coding::CloseCode},
            };

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}/tools", listener.local_addr().unwrap());
            let (fence, ready_to_fence) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                let _catalog = socket.next().await.unwrap().unwrap();
                socket
                    .send(Message::Text(r#"{"type":"ready"}"#.into()))
                    .await
                    .unwrap();
                ready_to_fence.await.unwrap();
                socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Policy,
                        reason: "managed agent is being deleted".into(),
                    })))
                    .await
                    .unwrap();
                let _ = socket.next().await;
            });
            let tools = nanocodex_tools::Tools::builder()
                .without_defaults()
                .build()
                .unwrap();
            let (attachment, _) = tools
                .attach(AttachmentTarget::new(endpoint, "test-bearer").unwrap())
                .connect()
                .await
                .unwrap();
            fence.send(()).unwrap();
            // Normal lifecycle observers must still see the terminal failure.
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(2), attachment.closed())
                    .await
                    .unwrap(),
                Err(AttachmentError::Fenced(_))
            ));
            // Explicit VM teardown has already decided to destroy this runtime.
            detach_stopping_vm(attachment).await.unwrap();
            server.await.unwrap();
        }

        #[derive(Debug, Eq, PartialEq)]
        struct TestPendingCommand {
            allocation_id: Uuid,
            kind: PendingCommandKind,
            sequence: u8,
        }

        impl PendingLifecycleCommand for TestPendingCommand {
            fn pending_identity(&self) -> PendingCommandIdentity {
                PendingCommandIdentity {
                    allocation_id: self.allocation_id,
                    kind: self.kind,
                }
            }
        }

        #[derive(Clone)]
        struct FakeFactory {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl AllocationFactory for FakeFactory {
            type Allocation = VmAllocationIdentity;

            fn provision<'a>(
                &'a self,
                spec: VmAllocationSpec,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<Self::Allocation, ProvisionFailure>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("provision:{}", spec.identity.allocation_id));
                    Ok(spec.identity)
                })
            }

            fn refresh_attachment<'a>(
                &'a self,
                allocation: &'a mut Self::Allocation,
                _target: AttachmentTarget,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("refresh:{}", allocation.allocation_id));
                    Ok(())
                })
            }

            fn stop<'a>(&'a self, allocation: Self::Allocation) -> BoxFuture<'a, StopOutcome> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("stop:{}", allocation.allocation_id));
                    StopOutcome::stopped()
                })
            }

            fn release_recovered<'a>(
                &'a self,
                identity: &'a VmAllocationIdentity,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("release-recovered:{}", identity.allocation_id));
                    Ok(())
                })
            }
        }

        struct RootCleanupFactory {
            allocation_directory: PathBuf,
        }

        impl AllocationFactory for RootCleanupFactory {
            type Allocation = VmAllocationIdentity;

            fn provision<'a>(
                &'a self,
                _spec: VmAllocationSpec,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<Self::Allocation, ProvisionFailure>> {
                Box::pin(async {
                    Err(ProvisionFailure::stopped(VmHostError::Resource(
                        "unexpected provision".to_owned(),
                    )))
                })
            }

            fn refresh_attachment<'a>(
                &'a self,
                _allocation: &'a mut Self::Allocation,
                _target: AttachmentTarget,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async {
                    Err(VmHostError::Resource(
                        "unexpected attachment refresh".to_owned(),
                    ))
                })
            }

            fn stop<'a>(&'a self, _allocation: Self::Allocation) -> BoxFuture<'a, StopOutcome> {
                Box::pin(async { StopOutcome::stopped() })
            }

            fn release_recovered<'a>(
                &'a self,
                identity: &'a VmAllocationIdentity,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(remove_private_root(
                    self.allocation_directory
                        .join(format!("{}.ext4", identity.allocation_id)),
                ))
            }
        }

        struct DetachFailingFactory {
            events: Arc<Mutex<Vec<String>>>,
            fail_cleanup_once: Arc<AtomicBool>,
        }

        impl AllocationFactory for DetachFailingFactory {
            type Allocation = VmAllocationIdentity;

            fn provision<'a>(
                &'a self,
                spec: VmAllocationSpec,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<Self::Allocation, ProvisionFailure>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("provision:{}", spec.identity.allocation_id));
                    Ok(spec.identity)
                })
            }

            fn refresh_attachment<'a>(
                &'a self,
                _allocation: &'a mut Self::Allocation,
                _target: AttachmentTarget,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async { Ok(()) })
            }

            fn stop<'a>(&'a self, allocation: Self::Allocation) -> BoxFuture<'a, StopOutcome> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("stop:{}", allocation.allocation_id));
                    StopOutcome {
                        attachment: Err(VmHostError::Resource("detach failed".to_owned())),
                        shutdown: Ok(()),
                    }
                })
            }

            fn release_recovered<'a>(
                &'a self,
                identity: &'a VmAllocationIdentity,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("release-recovered:{}", identity.allocation_id));
                    if self.fail_cleanup_once.swap(false, Ordering::AcqRel) {
                        Err(VmHostError::Resource("cleanup failed".to_owned()))
                    } else {
                        Ok(())
                    }
                })
            }
        }

        struct BlockedFactory {
            events: Arc<Mutex<Vec<String>>>,
            started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            finish: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        }

        impl AllocationFactory for BlockedFactory {
            type Allocation = VmAllocationIdentity;

            fn provision<'a>(
                &'a self,
                spec: VmAllocationSpec,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<Self::Allocation, ProvisionFailure>> {
                let started = self.started.lock().unwrap().take().unwrap();
                let finish = self.finish.lock().unwrap().take().unwrap();
                Box::pin(async move {
                    started.send(()).unwrap();
                    finish.await.unwrap();
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("provision:{}", spec.identity.allocation_id));
                    Ok(spec.identity)
                })
            }

            fn refresh_attachment<'a>(
                &'a self,
                _allocation: &'a mut Self::Allocation,
                _target: AttachmentTarget,
                _cancellation: OperationCancellation,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async { Ok(()) })
            }

            fn stop<'a>(&'a self, allocation: Self::Allocation) -> BoxFuture<'a, StopOutcome> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("stop:{}", allocation.allocation_id));
                    StopOutcome::stopped()
                })
            }

            fn release_recovered<'a>(
                &'a self,
                identity: &'a VmAllocationIdentity,
            ) -> BoxFuture<'a, Result<(), VmHostError>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .unwrap()
                        .push(format!("release-recovered:{}", identity.allocation_id));
                    Ok(())
                })
            }
        }

        fn spec(allocation_id: Uuid, slot: u16, machine_id: &str) -> VmAllocationSpec {
            spec_with_bearer(allocation_id, slot, machine_id, "secret")
        }

        fn spec_with_bearer(
            allocation_id: Uuid,
            slot: u16,
            machine_id: &str,
            bearer: &str,
        ) -> VmAllocationSpec {
            VmAllocationSpec::new(
                allocation_id,
                slot,
                1,
                machine_id,
                AttachmentTarget::new("ws://127.0.0.1:9/tool-host", bearer).unwrap(),
            )
        }

        fn open_test_state(directory: &tempfile::TempDir) -> VmHostState {
            VmHostState::open(directory.path(), None).unwrap()
        }

        fn test_store(state: &VmHostState) -> AllocationStore {
            AllocationStore::new(state.directory.join("allocations"))
        }

        fn read_record(state: &VmHostState, allocation_id: Uuid) -> AllocationRecord {
            serde_json::from_slice(
                &fs::read(
                    state
                        .directory
                        .join("allocations")
                        .join(format!("{allocation_id}.json")),
                )
                .unwrap(),
            )
            .unwrap()
        }

        #[tokio::test]
        async fn pending_operation_allows_heartbeat_progress() {
            let (complete, operation) = tokio::sync::oneshot::channel::<u8>();
            let operation = async { operation.await.unwrap() };
            tokio::pin!(operation);

            let event = next_controlled_operation_event(
                operation.as_mut(),
                ready("heartbeat"),
                pending::<()>(),
                pending::<()>(),
            )
            .await;
            assert!(matches!(
                event,
                ControlledOperationEvent::Heartbeat("heartbeat")
            ));

            complete.send(7).unwrap();
            let event = next_controlled_operation_event(
                operation.as_mut(),
                pending::<()>(),
                pending::<()>(),
                pending::<()>(),
            )
            .await;
            assert!(matches!(event, ControlledOperationEvent::Completed(7)));
        }

        #[test]
        fn pending_commands_are_bounded_coalesced_and_release_first() {
            let active_id = Uuid::new_v4();
            let first = Uuid::new_v4();
            let second = Uuid::new_v4();
            let third = Uuid::new_v4();
            let active = PendingCommandIdentity {
                allocation_id: active_id,
                kind: PendingCommandKind::Provision,
            };
            let mut queued = PendingCommandQueue::new(1);

            for sequence in [1, 2] {
                queued
                    .push(
                        TestPendingCommand {
                            allocation_id: first,
                            kind: PendingCommandKind::Provision,
                            sequence,
                        },
                        active,
                    )
                    .unwrap();
            }
            assert_eq!(queued.commands.len(), 1, "duplicate provision coalesced");
            queued
                .push(
                    TestPendingCommand {
                        allocation_id: second,
                        kind: PendingCommandKind::Provision,
                        sequence: 3,
                    },
                    active,
                )
                .unwrap();
            assert!(
                queued
                    .push(
                        TestPendingCommand {
                            allocation_id: third,
                            kind: PendingCommandKind::Provision,
                            sequence: 4,
                        },
                        active,
                    )
                    .is_err(),
                "a third unique identity exceeds twice max_vms"
            );
            queued
                .push(
                    TestPendingCommand {
                        allocation_id: second,
                        kind: PendingCommandKind::Release,
                        sequence: 5,
                    },
                    active,
                )
                .unwrap();

            assert_eq!(queued.pop().unwrap().sequence, 5, "release has priority");
            assert_eq!(
                queued.pop().unwrap().sequence,
                1,
                "first provision retained"
            );
            assert!(queued.pop().is_none());

            let active_release = PendingCommandIdentity {
                allocation_id: active_id,
                kind: PendingCommandKind::Release,
            };
            queued
                .push(
                    TestPendingCommand {
                        allocation_id: active_id,
                        kind: PendingCommandKind::Release,
                        sequence: 6,
                    },
                    active_release,
                )
                .unwrap();
            assert!(
                queued.pop().is_none(),
                "duplicate in-flight release coalesced"
            );
        }

        #[test]
        fn reconnect_backoff_resets_only_after_meaningful_health() {
            let mut retry = INITIAL_RECONNECT_DELAY;
            assert_eq!(
                next_reconnect_delay(&mut retry, false),
                Duration::from_millis(250)
            );
            assert_eq!(
                next_reconnect_delay(&mut retry, false),
                Duration::from_millis(500)
            );
            assert_eq!(
                next_reconnect_delay(&mut retry, false),
                Duration::from_secs(1)
            );
            assert_eq!(
                next_reconnect_delay(&mut retry, true),
                INITIAL_RECONNECT_DELAY
            );
            assert_eq!(retry, Duration::from_millis(500));
        }

        #[test]
        fn vm_hand_start_failures_are_safe_for_same_process_redrive() {
            let failure = vm_hand_start_failure(ManagedError::Configuration(
                "pre-launch validation failed".to_owned(),
            ));
            assert!(failure.cleanup_safe);
            assert!(
                failure
                    .error
                    .to_string()
                    .contains("pre-launch validation failed")
            );
        }

        #[test]
        fn host_identity_is_persisted_explicit_and_exclusively_locked() {
            let directory = tempfile::tempdir().unwrap();
            let explicit = Uuid::new_v4();
            let state = VmHostState::open(directory.path(), Some(explicit)).unwrap();
            assert_eq!(state.host_id, explicit);
            assert!(VmHostState::open(directory.path(), Some(explicit)).is_err());
            drop(state);
            assert_eq!(
                VmHostState::open(directory.path(), None).unwrap().host_id,
                explicit
            );
            assert!(VmHostState::open(directory.path(), Some(Uuid::new_v4())).is_err());
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn template_cache_reuses_exact_images_and_invalidates_replacements() {
            let directory = tempfile::tempdir().unwrap();
            let template = directory.path().join("template.ext4");
            fs::write(&template, b"first template").unwrap();
            let first =
                cache_overlay_template(File::open(&template).unwrap(), directory.path()).unwrap();
            let again =
                cache_overlay_template(File::open(&template).unwrap(), directory.path()).unwrap();
            assert_eq!(
                first.metadata().unwrap().ino(),
                again.metadata().unwrap().ino()
            );
            assert_eq!(
                first.metadata().unwrap().permissions().mode() & 0o777,
                0o400
            );
            let replacement = directory.path().join("replacement");
            fs::write(&replacement, b"newer template").unwrap();
            fs::rename(replacement, &template).unwrap();
            let next =
                cache_overlay_template(File::open(&template).unwrap(), directory.path()).unwrap();
            assert_ne!(
                first.metadata().unwrap().ino(),
                next.metadata().unwrap().ino()
            );
            assert_eq!(
                fs::read(locked_file_path(&first)).unwrap(),
                b"first template"
            );
            assert_eq!(
                fs::read(locked_file_path(&next)).unwrap(),
                b"newer template"
            );
        }

        #[cfg(target_os = "linux")]
        #[tokio::test]
        async fn overlay_roots_pin_the_base_and_keep_independent_writes_across_recovery() {
            let directory = tempfile::tempdir().unwrap();
            let template = directory.path().join("template.ext4");
            let image = File::create(&template).unwrap();
            image.set_len(512 * 1024 * 1024).unwrap();
            let locked = File::open(&template).unwrap();
            let first = directory.path().join("first.ext4");
            let second = directory.path().join("second.ext4");
            let first_lower = prepare_factory_root_blocking(&locked_file_path(&locked), &first)
                .unwrap()
                .unwrap();
            prepare_factory_root_blocking(&locked_file_path(&locked), &second).unwrap();
            assert_eq!(
                fs::metadata(&template).unwrap().ino(),
                fs::metadata(&first_lower).unwrap().ino()
            );
            assert_ne!(
                fs::metadata(&first).unwrap().ino(),
                fs::metadata(&second).unwrap().ino()
            );
            assert_eq!(fs::metadata(&first).unwrap().nlink(), 1);
            assert!(fs::metadata(&first).unwrap().blocks() * 512 < 16 * 1024 * 1024);
            let replacement = directory.path().join("replacement.ext4");
            fs::write(&replacement, b"different image").unwrap();
            fs::rename(&replacement, &template).unwrap();
            assert_eq!(
                prepare_factory_root_blocking(&template, &first).unwrap(),
                Some(first_lower.clone())
            );
            assert_ne!(
                fs::metadata(&template).unwrap().ino(),
                fs::metadata(&first_lower).unwrap().ino()
            );
            remove_private_root(first.clone()).await.unwrap();
            assert!(!first.exists());
            assert!(!first_lower.exists());
            assert!(second.exists());
            remove_private_root(first).await.unwrap();
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn overlay_creation_keeps_cross_filesystem_state_directories_working() {
            let directory = tempfile::tempdir().unwrap();
            let Ok(other) = tempfile::tempdir_in("/dev/shm") else {
                return;
            };
            if fs::metadata(directory.path()).unwrap().dev()
                == fs::metadata(other.path()).unwrap().dev()
            {
                return;
            }
            let template = directory.path().join("template.ext4");
            fs::write(&template, b"standalone root on another device").unwrap();
            let root = other.path().join("root.ext4");
            assert_eq!(
                prepare_factory_root_blocking(&template, &root).unwrap(),
                None
            );
            assert_eq!(fs::read(&root).unwrap(), fs::read(&template).unwrap());
            assert!(!overlay_lower_path(&root).exists());
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn overlay_preparation_recovers_an_interrupted_lower_publication() {
            let directory = tempfile::tempdir().unwrap();
            let template = directory.path().join("template.ext4");
            File::create(&template)
                .unwrap()
                .set_len(512 * 1024 * 1024)
                .unwrap();
            let root = directory.path().join("root.ext4");
            fs::hard_link(&template, overlay_lower_path(&root)).unwrap();
            assert_eq!(
                prepare_factory_root_blocking(&template, &root).unwrap(),
                Some(overlay_lower_path(&root))
            );
            let old = directory.path().join("retained.ext4");
            fs::write(&old, b"standalone root").unwrap();
            assert_eq!(
                prepare_factory_root_blocking(&template, &old).unwrap(),
                None
            );
            assert_eq!(fs::read(&old).unwrap(), b"standalone root");
        }

        #[test]
        fn spare_overlay_adoption_pins_lower_and_rolls_back_on_upper_conflict() {
            let directory = tempfile::tempdir().unwrap();
            let spare = directory.path().join("spare.ext4");
            let target = directory.path().join("allocation.ext4");
            fs::write(&spare, b"upper").unwrap();
            fs::write(overlay_lower_path(&spare), b"lower").unwrap();
            fs::write(&target, b"existing").unwrap();
            assert!(adopt_spare_root(&spare, &target).is_err());
            assert!(!overlay_lower_path(&target).exists());
            assert_eq!(fs::read(&target).unwrap(), b"existing");
            fs::remove_file(&target).unwrap();
            adopt_spare_root(&spare, &target).unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"upper");
            assert_eq!(fs::read(overlay_lower_path(&target)).unwrap(), b"lower");
            assert!(!spare.exists());
            assert!(!overlay_lower_path(&spare).exists());
        }

        #[test]
        fn spare_root_claim_preserves_the_open_disk_and_never_overwrites_retained_state() {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("spare.ext4");
            let target = directory.path().join("allocation.ext4");
            fs::write(&source, b"fresh guest").unwrap();
            let open_disk = File::open(&source).unwrap();
            adopt_spare_root(&source, &target).unwrap();
            assert!(!source.exists());
            assert_eq!(
                open_disk.metadata().unwrap().ino(),
                fs::metadata(&target).unwrap().ino()
            );
            assert_eq!(fs::metadata(&target).unwrap().nlink(), 1);
            fs::write(&target, b"user work").unwrap();
            fs::write(&source, b"another spare").unwrap();
            assert!(adopt_spare_root(&source, &target).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"user work");
            assert_eq!(fs::read(&source).unwrap(), b"another spare");
            let alias = directory.path().join("alias.ext4");
            std::os::unix::fs::symlink(&target, &alias).unwrap();
            assert!(adopt_spare_root(&source, &alias).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"user work");
        }

        #[tokio::test]
        async fn dropping_spare_preparation_cancels_its_owner_task() {
            struct Owner(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for Owner {
                fn drop(&mut self) {
                    let _ = self.0.take().unwrap().send(());
                }
            }
            let (started, running) = tokio::sync::oneshot::channel();
            let (stopped, closed) = tokio::sync::oneshot::channel();
            let task = PreparingSpare(tokio::spawn(async move {
                let _owner = Owner(Some(stopped));
                let _ = started.send(());
                pending::<Result<PreparedVm, ProvisionFailure>>().await
            }));
            running.await.unwrap();
            drop(task);
            tokio::time::timeout(Duration::from_secs(1), closed)
                .await
                .unwrap()
                .unwrap();
        }

        #[tokio::test]
        async fn spare_capacity_includes_durable_allocations_and_pending_releases() {
            let mut supervisor = AllocationSupervisor::new(
                1,
                FakeFactory {
                    events: Arc::new(Mutex::new(Vec::new())),
                },
            )
            .unwrap();
            assert_eq!(supervisor.reserved_capacity(), 0);
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            supervisor.provision(request.clone()).await.unwrap();
            assert_eq!(supervisor.reserved_capacity(), 1);
            supervisor.shutdown().await.unwrap();
            assert_eq!(
                supervisor.reserved_capacity(),
                1,
                "stopped retained roots still reserve their slots"
            );
            supervisor.release(&request.identity).await.unwrap();
            assert_eq!(supervisor.reserved_capacity(), 0);
            supervisor
                .retired
                .get_mut(&request.identity.allocation_id)
                .unwrap()
                .release_pending = true;
            assert_eq!(supervisor.reserved_capacity(), 1);
        }

        #[test]
        fn private_root_clone_is_idempotent_and_not_linked_to_the_template() {
            let directory = tempfile::tempdir().unwrap();
            let template = directory.path().join("template.ext4");
            let clone = directory.path().join("allocations/allocation.ext4");
            fs::write(&template, b"template").unwrap();
            clone_private_root_blocking(&template, &clone).unwrap();
            fs::write(&clone, b"private").unwrap();
            clone_private_root_blocking(&template, &clone).unwrap();
            assert_eq!(fs::read(&template).unwrap(), b"template");
            assert_eq!(fs::read(&clone).unwrap(), b"private");
        }

        #[test]
        fn locked_template_descriptor_survives_path_replacement() {
            let directory = tempfile::tempdir().unwrap();
            let template = directory.path().join("template.ext4");
            let replacement = directory.path().join("replacement.ext4");
            let clone = directory.path().join("allocations/allocation.ext4");
            fs::write(&template, b"locked-template").unwrap();
            let locked = File::open(&template).unwrap();
            fs2::FileExt::try_lock_shared(&locked).unwrap();
            let locked_path = locked_file_path(&locked);

            fs::write(&replacement, b"replacement-template").unwrap();
            fs::rename(&replacement, &template).unwrap();
            clone_private_root_blocking(&locked_path, &clone).unwrap();

            assert_eq!(fs::read(&template).unwrap(), b"replacement-template");
            assert_eq!(fs::read(&clone).unwrap(), b"locked-template");
        }

        #[test]
        fn private_root_clone_rejects_an_existing_hard_link() {
            let directory = tempfile::tempdir().unwrap();
            let template = directory.path().join("template.ext4");
            let allocations = directory.path().join("allocations");
            let clone = allocations.join("allocation.ext4");
            fs::write(&template, b"template").unwrap();
            fs::create_dir(&allocations).unwrap();
            fs::hard_link(&template, &clone).unwrap();

            assert!(clone_private_root_blocking(&template, &clone).is_err());
            assert_eq!(fs::read(&template).unwrap(), b"template");
        }

        #[tokio::test]
        async fn desktop_credential_refresh_does_not_wait_for_tools_handshake() {
            let (rotated, observed) = tokio::sync::oneshot::channel();
            let (ready, handshake) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                refresh_desktop_before_attachment(
                    async {
                        rotated.send(()).unwrap();
                        Ok(())
                    },
                    async {
                        handshake.await.unwrap();
                        Ok(42)
                    },
                    &OperationCancellation::default(),
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(1), observed)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !task.is_finished(),
                "desktop capability must arrive while tools are pending"
            );
            ready.send(()).unwrap();
            assert_eq!(task.await.unwrap().unwrap(), 42);
        }

        #[tokio::test]
        async fn failed_desktop_refresh_never_connects_tools() {
            let connected = AtomicBool::new(false);
            let result = refresh_desktop_before_attachment(
                ready(Err(VmHostError::Resource(
                    "credential write failed".to_owned(),
                ))),
                async {
                    connected.store(true, Ordering::SeqCst);
                    Ok(())
                },
                &OperationCancellation::default(),
            )
            .await;
            assert!(matches!(result, Err(VmHostError::Resource(_))));
            assert!(!connected.load(Ordering::SeqCst));
        }

        #[tokio::test]
        async fn cancelled_refresh_does_not_deliver_a_desktop_capability() {
            let cancellation = OperationCancellation::default();
            cancellation.cancel();
            let delivered = AtomicBool::new(false);
            let connected = AtomicBool::new(false);
            let result = refresh_desktop_before_attachment(
                async {
                    delivered.store(true, Ordering::SeqCst);
                    Ok(())
                },
                async {
                    connected.store(true, Ordering::SeqCst);
                    Ok(())
                },
                &cancellation,
            )
            .await;
            assert!(matches!(result, Err(VmHostError::Cancelled)));
            assert!(!delivered.load(Ordering::SeqCst));
            assert!(!connected.load(Ordering::SeqCst));
        }

        #[tokio::test]
        async fn cancellation_during_credential_delivery_prevents_tools_connection() {
            let cancellation = OperationCancellation::default();
            let connected = AtomicBool::new(false);
            let result = refresh_desktop_before_attachment(
                async {
                    cancellation.cancel();
                    Ok(())
                },
                async {
                    connected.store(true, Ordering::SeqCst);
                    Ok(())
                },
                &cancellation,
            )
            .await;
            assert!(matches!(result, Err(VmHostError::Cancelled)));
            assert!(!connected.load(Ordering::SeqCst));
        }

        #[tokio::test]
        async fn tools_failure_is_reported_after_desktop_capability_delivery() {
            let delivered = AtomicBool::new(false);
            let result = refresh_desktop_before_attachment(
                async {
                    delivered.store(true, Ordering::SeqCst);
                    Ok(())
                },
                async {
                    assert!(delivered.load(Ordering::SeqCst));
                    Err::<(), _>(VmHostError::Resource("tools handshake failed".to_owned()))
                },
                &OperationCancellation::default(),
            )
            .await;
            assert!(matches!(result, Err(VmHostError::Resource(_))));
        }

        #[tokio::test]
        async fn matching_live_provision_refreshes_attachment_and_enforces_conflicts() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let factory = FakeFactory {
                events: Arc::clone(&events),
            };
            let mut supervisor = AllocationSupervisor::new(2, factory).unwrap();
            let first = spec(Uuid::new_v4(), 0, "machine-0");
            assert_eq!(
                supervisor.provision(first.clone()).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(
                supervisor.provision(first.clone()).await.unwrap(),
                VmAllocationChange::Unchanged
            );
            assert_eq!(events.lock().unwrap().len(), 1);
            let rotated = spec_with_bearer(
                first.identity.allocation_id,
                first.slot,
                &first.identity.machine_id,
                "rotated-secret",
            );
            assert_eq!(
                supervisor.provision(rotated).await.unwrap(),
                VmAllocationChange::Unchanged
            );
            assert_eq!(
                events.lock().unwrap().as_slice(),
                [
                    format!("provision:{}", first.identity.allocation_id),
                    format!("refresh:{}", first.identity.allocation_id),
                ]
            );

            let same_slot = spec(Uuid::new_v4(), 0, "machine-1");
            assert!(matches!(
                supervisor.provision(same_slot).await,
                Err(VmHostError::ImmutableConflict { .. })
            ));
            let second = spec(Uuid::new_v4(), 1, "machine-1");
            supervisor.provision(second).await.unwrap();
            let over_cap = spec(Uuid::new_v4(), 2, "machine-2");
            assert!(matches!(
                supervisor.provision(over_cap).await,
                Err(VmHostError::Capacity { capacity: 2 })
            ));
        }

        #[tokio::test]
        async fn reconcile_reports_only_ready_allocations_and_release_is_idempotent() {
            let events = Arc::new(Mutex::new(Vec::new()));
            let factory = FakeFactory {
                events: Arc::clone(&events),
            };
            let mut supervisor = AllocationSupervisor::new(1, factory).unwrap();
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            supervisor.provision(request.clone()).await.unwrap();
            assert_eq!(supervisor.reconcile(), vec![request.identity.clone()]);
            assert_eq!(
                supervisor.release(&request.identity).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(
                supervisor.release(&request.identity).await.unwrap(),
                VmAllocationChange::Unchanged
            );
            assert!(supervisor.reconcile().is_empty());
            assert_eq!(
                events.lock().unwrap().as_slice(),
                [
                    format!("provision:{}", request.identity.allocation_id),
                    format!("stop:{}", request.identity.allocation_id),
                    format!("release-recovered:{}", request.identity.allocation_id),
                ]
            );
        }

        #[tokio::test]
        async fn acknowledged_release_compacts_tombstone_and_redrive_recreates_it_safely() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocation_id = Uuid::new_v4();
            let identity = VmAllocationIdentity {
                allocation_id,
                generation: 1,
                machine_id: "machine-0".to_owned(),
            };
            let record = state
                .directory
                .join("allocations")
                .join(format!("{allocation_id}.json"));
            let mut supervisor = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::new(Mutex::new(Vec::new())),
                },
                test_store(&state),
                &state,
            )
            .unwrap();

            supervisor.release(&identity).await.unwrap();
            assert_eq!(
                read_record(&state, allocation_id).state,
                AllocationRecordState::Released
            );
            supervisor.acknowledge_released(&identity).unwrap();
            assert!(!record.exists());
            assert!(!supervisor.retired.contains_key(&allocation_id));

            // If the process dies after the WebSocket send but before the
            // service commits it, the repeated release is still idempotent.
            supervisor.release(&identity).await.unwrap();
            assert!(record.exists());
            supervisor.acknowledge_released(&identity).unwrap();
            assert!(!record.exists());
        }

        #[tokio::test]
        async fn successor_fences_ready_record_and_allows_safe_release() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocations = state.directory.join("allocations");
            let allocation_id = Uuid::new_v4();
            let request = VmAllocationSpec::new(
                allocation_id,
                0,
                1,
                "machine-0",
                AttachmentTarget::new(
                    "ws://127.0.0.1:9/tool-host",
                    "super-secret-allocation-bearer",
                )
                .unwrap(),
            );
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut first = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                AllocationStore::new(allocations.clone()),
                &state,
            )
            .unwrap();
            first.provision(request.clone()).await.unwrap();
            assert_eq!(first.reconcile(), vec![request.identity.clone()]);
            let journal =
                fs::read_to_string(allocations.join(format!("{allocation_id}.json"))).unwrap();
            assert!(!journal.contains("super-secret-allocation-bearer"));
            drop(first);
            drop(state);

            let state = open_test_state(&directory);
            let mut recovered = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                AllocationStore::new(allocations.clone()),
                &state,
            )
            .unwrap();
            assert!(recovered.reconcile().is_empty());
            assert_eq!(
                read_record(&state, allocation_id).state,
                AllocationRecordState::Stopped
            );
            assert_eq!(
                recovered.release(&request.identity).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(
                read_record(&state, allocation_id).state,
                AllocationRecordState::Released
            );
        }

        #[tokio::test]
        async fn successor_fences_provisioning_record_and_allows_reprovision() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            test_store(&state)
                .put(&AllocationRecord::tracked(
                    AllocationRecordState::Provisioning,
                    &request.immutable_key(),
                ))
                .unwrap();
            drop(state);

            let state = open_test_state(&directory);
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut recovered = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                test_store(&state),
                &state,
            )
            .unwrap();
            assert_eq!(
                read_record(&state, request.identity.allocation_id).state,
                AllocationRecordState::Stopped
            );
            assert_eq!(
                recovered.provision(request.clone()).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(recovered.reconcile(), vec![request.identity]);
        }

        #[test]
        fn lock_acquisition_failure_does_not_rewrite_ready_record() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            test_store(&state)
                .put(&AllocationRecord::tracked(
                    AllocationRecordState::Ready,
                    &request.immutable_key(),
                ))
                .unwrap();

            assert!(VmHostState::open(directory.path(), None).is_err());
            assert_eq!(
                read_record(&state, request.identity.allocation_id).state,
                AllocationRecordState::Ready
            );
        }

        #[tokio::test]
        async fn stopped_recovered_release_is_durable_and_idempotent() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocations = state.directory.join("allocations");
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut first = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                AllocationStore::new(allocations.clone()),
                &state,
            )
            .unwrap();
            first.provision(request.clone()).await.unwrap();
            first.shutdown().await.unwrap();
            drop(first);
            drop(state);

            let state = open_test_state(&directory);
            let mut recovered = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                AllocationStore::new(allocations.clone()),
                &state,
            )
            .unwrap();
            assert_eq!(
                recovered.release(&request.identity).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(
                recovered.release(&request.identity).await.unwrap(),
                VmAllocationChange::Unchanged
            );
            let record: AllocationRecord = serde_json::from_slice(
                &fs::read(allocations.join(format!("{}.json", request.identity.allocation_id)))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(record.state, AllocationRecordState::Released);
            assert!(events.lock().unwrap().iter().any(|event| {
                event == &format!("release-recovered:{}", request.identity.allocation_id)
            }));
        }

        #[tokio::test]
        async fn startup_fences_and_completes_all_predecessor_releases() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocations = state.directory.join("allocations");
            let safe = spec(Uuid::new_v4(), 0, "machine-0");
            let unsafe_allocation = spec(Uuid::new_v4(), 1, "machine-1");
            let store = AllocationStore::new(allocations.clone());
            store
                .put(&AllocationRecord::tracked(
                    AllocationRecordState::ReleasingStopped,
                    &safe.immutable_key(),
                ))
                .unwrap();
            store
                .put(&AllocationRecord::tracked(
                    AllocationRecordState::Releasing,
                    &unsafe_allocation.immutable_key(),
                ))
                .unwrap();
            drop(state);

            let state = open_test_state(&directory);
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut recovered = AllocationSupervisor::with_store(
                2,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                store,
                &state,
            )
            .unwrap();

            recovered.complete_startup_releases().await.unwrap();

            {
                let startup_events = events.lock().unwrap();
                assert_eq!(startup_events.len(), 2);
                assert!(startup_events.contains(&format!(
                    "release-recovered:{}",
                    safe.identity.allocation_id
                )));
                assert!(startup_events.contains(&format!(
                    "release-recovered:{}",
                    unsafe_allocation.identity.allocation_id
                )));
            }
            assert_eq!(
                recovered.release(&safe.identity).await.unwrap(),
                VmAllocationChange::Unchanged
            );
            assert_eq!(
                recovered
                    .release(&unsafe_allocation.identity)
                    .await
                    .unwrap(),
                VmAllocationChange::Unchanged
            );
            let unsafe_record: AllocationRecord = serde_json::from_slice(
                &fs::read(
                    allocations.join(format!("{}.json", unsafe_allocation.identity.allocation_id)),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(unsafe_record.state, AllocationRecordState::Released);
        }

        #[test]
        fn allocation_journal_fails_closed_on_corrupt_or_conflicting_state() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocations = state.directory.join("allocations");
            let allocation_id = Uuid::new_v4();
            fs::write(
                allocations.join(format!("{allocation_id}.json")),
                b"{\"version\":1,\"state\":\"ready\"}",
            )
            .unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            assert!(
                AllocationSupervisor::with_store(
                    1,
                    FakeFactory { events },
                    AllocationStore::new(allocations),
                    &state,
                )
                .is_err()
            );
        }

        #[tokio::test]
        async fn host_shutdown_stops_allocations_without_release_cleanup() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocations = state.directory.join("allocations");
            let events = Arc::new(Mutex::new(Vec::new()));
            let factory = FakeFactory {
                events: Arc::clone(&events),
            };
            let mut supervisor = AllocationSupervisor::with_store(
                1,
                factory,
                AllocationStore::new(allocations.clone()),
                &state,
            )
            .unwrap();
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            supervisor.provision(request.clone()).await.unwrap();

            supervisor.shutdown().await.unwrap();

            let events = events.lock().unwrap();
            assert!(events.iter().any(|event| event.starts_with("stop:")));
            assert!(
                !events
                    .iter()
                    .any(|event| event.starts_with("release-recovered:"))
            );
            let record: AllocationRecord = serde_json::from_slice(
                &fs::read(allocations.join(format!("{}.json", request.identity.allocation_id)))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(record.state, AllocationRecordState::Stopped);
        }

        #[tokio::test]
        async fn stopped_allocation_can_be_reprovisioned_without_releasing_its_root() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut supervisor = AllocationSupervisor::with_store(
                1,
                FakeFactory {
                    events: Arc::clone(&events),
                },
                test_store(&state),
                &state,
            )
            .unwrap();
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            supervisor.provision(request.clone()).await.unwrap();
            supervisor.shutdown().await.unwrap();

            assert_eq!(
                supervisor.provision(request.clone()).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(supervisor.reconcile(), vec![request.identity.clone()]);
            assert_eq!(
                events.lock().unwrap().as_slice(),
                [
                    format!("provision:{}", request.identity.allocation_id),
                    format!("stop:{}", request.identity.allocation_id),
                    format!("provision:{}", request.identity.allocation_id),
                ]
            );
        }

        #[tokio::test]
        async fn successor_recovery_never_removes_symlinked_or_hard_linked_roots() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let allocations = state.directory.join("allocations");
            let symlinked = spec(Uuid::new_v4(), 0, "machine-0");
            let hard_linked = spec(Uuid::new_v4(), 1, "machine-1");
            for request in [&symlinked, &hard_linked] {
                test_store(&state)
                    .put(&AllocationRecord::tracked(
                        AllocationRecordState::Ready,
                        &request.immutable_key(),
                    ))
                    .unwrap();
            }
            let symlink_target = directory.path().join("symlink-target.ext4");
            let hard_link_target = directory.path().join("hard-link-target.ext4");
            fs::write(&symlink_target, b"symlink target").unwrap();
            fs::write(&hard_link_target, b"hard link target").unwrap();
            let symlink_root =
                allocations.join(format!("{}.ext4", symlinked.identity.allocation_id));
            let hard_link_root =
                allocations.join(format!("{}.ext4", hard_linked.identity.allocation_id));
            std::os::unix::fs::symlink(&symlink_target, &symlink_root).unwrap();
            fs::hard_link(&hard_link_target, &hard_link_root).unwrap();
            drop(state);

            let state = open_test_state(&directory);
            let mut recovered = AllocationSupervisor::with_store(
                2,
                RootCleanupFactory {
                    allocation_directory: allocations,
                },
                test_store(&state),
                &state,
            )
            .unwrap();
            assert_eq!(
                read_record(&state, symlinked.identity.allocation_id).state,
                AllocationRecordState::Stopped
            );
            assert_eq!(
                read_record(&state, hard_linked.identity.allocation_id).state,
                AllocationRecordState::Stopped
            );

            assert!(recovered.release(&symlinked.identity).await.is_err());
            assert!(recovered.release(&hard_linked.identity).await.is_err());
            assert_eq!(fs::read(&symlink_target).unwrap(), b"symlink target");
            assert_eq!(fs::read(&hard_link_target).unwrap(), b"hard link target");
            assert!(
                fs::symlink_metadata(&symlink_root)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::metadata(&hard_link_root).unwrap().nlink(), 2);
        }

        #[tokio::test]
        async fn terminal_fence_cancels_blocked_provision_before_ready_publish() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let events = Arc::new(Mutex::new(Vec::new()));
            let (started_tx, mut started_rx) = tokio::sync::oneshot::channel();
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
            let factory = BlockedFactory {
                events: Arc::clone(&events),
                started: Mutex::new(Some(started_tx)),
                finish: Mutex::new(Some(finish_rx)),
            };
            let mut supervisor =
                AllocationSupervisor::with_store(1, factory, test_store(&state), &state).unwrap();
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            let cancellation = OperationCancellation::default();
            let observed = Arc::new(AtomicBool::new(false));
            let outcome = {
                let operation =
                    supervisor.provision_controlled(request.clone(), cancellation.clone());
                tokio::pin!(operation);
                tokio::select! {
                    started = &mut started_rx => started.unwrap(),
                    result = operation.as_mut() => panic!("provision unexpectedly completed: {result:?}"),
                }

                let observed_in_task = Arc::clone(&observed);
                let cancellation_in_task = cancellation.clone();
                let observer = tokio::spawn(async move {
                    while !cancellation_in_task.is_cancelled() {
                        tokio::task::yield_now().await;
                    }
                    observed_in_task.store(true, Ordering::Release);
                    finish_tx.send(()).unwrap();
                });
                let outcome = tokio::time::timeout(
                    Duration::from_secs(1),
                    finish_terminal_operation(
                        operation.as_mut(),
                        &cancellation,
                        DeferredControlOutcome::Shutdown,
                    ),
                )
                .await
                .expect("terminal cancellation must be observed promptly");
                observer.await.unwrap();
                outcome
            };
            assert!(observed.load(Ordering::Acquire));
            assert!(matches!(
                outcome,
                ControlledOperationOutcome::Terminal {
                    result: Err(_),
                    terminal: DeferredControlOutcome::Shutdown,
                }
            ));
            assert!(supervisor.reconcile().is_empty());
            assert_eq!(
                read_record(&state, request.identity.allocation_id).state,
                AllocationRecordState::Stopped
            );
            assert_eq!(
                events.lock().unwrap().as_slice(),
                [
                    format!("provision:{}", request.identity.allocation_id),
                    format!("stop:{}", request.identity.allocation_id),
                ]
            );
        }

        #[tokio::test]
        async fn detach_failure_does_not_hide_stopped_phase_or_leak_slot() {
            let directory = tempfile::tempdir().unwrap();
            let state = open_test_state(&directory);
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut supervisor = AllocationSupervisor::with_store(
                1,
                DetachFailingFactory {
                    events: Arc::clone(&events),
                    fail_cleanup_once: Arc::new(AtomicBool::new(true)),
                },
                test_store(&state),
                &state,
            )
            .unwrap();
            let request = spec(Uuid::new_v4(), 0, "machine-0");
            supervisor.provision(request.clone()).await.unwrap();

            let error = supervisor.release(&request.identity).await.unwrap_err();
            assert!(error.to_string().contains("detach failed"));
            assert!(error.to_string().contains("cleanup failed"));
            assert_eq!(
                read_record(&state, request.identity.allocation_id).state,
                AllocationRecordState::ReleasingStopped
            );
            assert_eq!(
                supervisor.release(&request.identity).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert_eq!(
                read_record(&state, request.identity.allocation_id).state,
                AllocationRecordState::Released
            );
            assert_eq!(
                supervisor.release(&request.identity).await.unwrap(),
                VmAllocationChange::Unchanged
            );
            let replacement = spec(Uuid::new_v4(), 0, "machine-1");
            assert_eq!(
                supervisor.provision(replacement).await.unwrap(),
                VmAllocationChange::Changed
            );
            assert!(events.lock().unwrap().iter().any(|event| {
                event == &format!("release-recovered:{}", request.identity.allocation_id)
            }));
        }

        #[test]
        fn release_combines_detach_shutdown_and_cleanup_failures_in_order() {
            let result = combine_failures([
                Err(VmHostError::Resource("detach".to_owned())),
                Err(VmHostError::Resource("shutdown".to_owned())),
                Err(VmHostError::Resource("cleanup".to_owned())),
            ])
            .unwrap_err()
            .to_string();
            assert!(result.find("detach").unwrap() < result.find("shutdown").unwrap());
            assert!(result.find("shutdown").unwrap() < result.find("cleanup").unwrap());
        }
    }
}

/// Runs the managed VM-host control loop.
///
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) async fn serve(config: Host) -> Result<(), ManagedError> {
    supported::run(config).await
}

#[cfg(not(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
)))]
pub(crate) async fn serve(_config: Host) -> Result<(), ManagedError> {
    Err(ManagedError::Configuration(
        "VM hosts require glibc Linux with /dev/kvm or Apple Silicon macOS".to_owned(),
    ))
}
