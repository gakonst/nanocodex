//! Content-addressed OCI and Dockerfile root disks for Nanocodex VMs.
//!
//! The cache owns immutable prepared disks. Each VM attempt should use
//! [`PreparedRootDisk::private_workspace`] to take a cheap copy-on-write clone,
//! inherit the image's runtime metadata, and mutate only that disposable copy.
//!
//! # Prepare and instantiate an image
//!
//! ```no_run
//! use nanocodex_vm::{
//!     image::{CachePolicy, VmImageBuilder},
//!     tools::GuestRuntimeDisk,
//! };
//!
//! # async fn prepare() -> Result<(), Box<dyn std::error::Error>> {
//! let runtime = GuestRuntimeDisk::prepare(
//!     "target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest",
//!     ".cache/vm",
//! )?;
//! let images = VmImageBuilder::new(
//!     "target/debug/vm-tools",
//!     runtime.path(),
//! )
//! .firmware_directory(".cache/libkrunfw/libkrunfw")
//! .vmm_arg("--vmm");
//! let image = images
//!     .prepare(
//!         "tasks/project/environment",
//!         10 * 1024 * 1024 * 1024,
//!         ".cache/vm",
//!         CachePolicy::Reuse,
//!     )
//!     .await?;
//! let workspace = image
//!     .private_workspace(
//!         ".nanocodex/attempts/018f/root.ext4",
//!         "target/debug/vm-tools",
//!     )?
//!     .vmm_argument("--vmm")
//!     .guest_runtime_disk(runtime.path())
//!     .firmware_directory(".cache/libkrunfw/libkrunfw")
//!     .launch()
//!     .await?;
//! # workspace.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The context contains an ordinary, deliberately constrained Dockerfile, for
//! example:
//!
//! ```dockerfile
//! FROM python:3.13-slim-bookworm
//! WORKDIR /app
//! COPY requirements.txt /app/
//! RUN pip install -r requirements.txt
//! ```

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod disk;

pub use disk::reflink_or_sparse_copy;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    command::GuestCommand,
    config::{BlockDevice, Network, VmConfig},
    egress::EgressLease,
    tools::{VmCommand, VmToolSession, VmToolSessionError},
    workspace::{VmWorkspaceBuilder, VmWorkspaceError},
};
use arcbox_ext4::{Formatter, Reader};
use flate2::read::GzDecoder;
use futures_util::{StreamExt, TryStreamExt, stream};
use ignore::WalkBuilder;
use oci_client::{
    Client, Reference, client::ClientConfig, config::ConfigFile, manifest::ImageIndexEntry,
    secrets::RegistryAuth,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::{process::Command, sync::Mutex as AsyncMutex};
use tracing::{Instrument, info, info_span};

const BLOCK_SIZE: u32 = 4_096;
const MINIMUM_DISK_BYTES: u64 = 512 * 1024 * 1024;
const CACHE_RECORD_VERSION: u32 = 2;
const IMAGE_BUILD_CACHE_VERSION: u32 = 2;
const PREPARED_DISK_RECORD_VERSION: u32 = 3;
const CACHED_EXT4_RECORD_VERSION: u32 = 2;
const BLOB_RECORD_VERSION: u32 = 2;
const CONTEXT_DISK_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_mins(30);
const DEFAULT_COPY_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_CONCURRENT_IMAGE_RESOLVES: usize = 8;
const MAX_CONCURRENT_LAYER_DOWNLOADS: usize = 8;
const MAX_VMM_BUILD_CACHE_IDENTITY_BYTES: usize = 4096;
const BUILD_RUNTIME_ID: &str = "nanocodex-runtime";
const BUILD_CONTEXT_ID: &str = "nanocodex-context";
const BUILD_RUNTIME_DEVICE: &str = "/dev/vdb";
const BUILD_CONTEXT_DEVICE: &str = "/dev/vdc";
const BUILD_RUNTIME_MOUNT: &str = "/run/nanocodex";
const BUILD_CONTEXT_MOUNT: &str = "/mnt/nanocodex-context";
const BUILD_RESOLVER_STATE: &str = "/run/nanocodex-build-resolver";
const RESTORE_BUILD_RESOLVER_SCRIPT: &str = r#"set -eu
rm -f /etc/resolv.conf
if [ -e /run/nanocodex-build-resolver/original ] || [ -L /run/nanocodex-build-resolver/original ]; then
  mv /run/nanocodex-build-resolver/original /etc/resolv.conf
fi
rm -rf /run/nanocodex-build-resolver"#;
const DEFAULT_GUEST_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const DEFAULT_BUILD_VM_CPUS: u8 = 2;
const DEFAULT_BUILD_VM_MEMORY_MIB: u32 = 4_096;
#[cfg(target_os = "linux")]
const FIRMWARE_LIBRARY_FILENAME: &str = "libkrunfw.so.5";
#[cfg(target_os = "macos")]
const FIRMWARE_LIBRARY_FILENAME: &str = "libkrunfw.5.dylib";
#[cfg(target_os = "linux")]
const FIRMWARE_LIBRARY_PATH_ENVIRONMENT: &str = "LD_LIBRARY_PATH";
#[cfg(target_os = "macos")]
const FIRMWARE_LIBRARY_PATH_ENVIRONMENT: &str = "DYLD_LIBRARY_PATH";
#[cfg(target_arch = "aarch64")]
const GUEST_ARCHITECTURE: &str = "arm64";
#[cfg(target_arch = "x86_64")]
const GUEST_ARCHITECTURE: &str = "amd64";
const COPY_SCRIPT: &str = r#"set -eu
dest=$1
shift
if [ "$#" -gt 1 ]; then
  mkdir -p "$dest"
  for src do cp -a "$src" "$dest/"; done
elif [ -d "$1" ]; then
  mkdir -p "$dest"
  cp -a "$1/." "$dest/"
elif [ "${dest%/}" != "$dest" ]; then
  mkdir -p "$dest"
  cp -a "$1" "$dest/"
else
  mkdir -p "$(dirname "$dest")"
  cp -a "$1" "$dest"
fi"#;

/// Configuration used to prepare immutable VM root disks.
///
/// The VMM and runtime disk are used only for Dockerfiles that require build
/// steps such as `RUN` or `COPY`. A single-stage `FROM` plus `WORKDIR` can be
/// flattened directly from OCI layers.
#[derive(Clone, Debug)]
pub struct VmImageBuilder {
    vmm: PathBuf,
    vmm_arguments: Vec<OsString>,
    vmm_build_cache_identity: Option<String>,
    runtime_image: PathBuf,
    firmware_directory: Option<PathBuf>,
    cpus: u8,
    memory_mib: u32,
    prefer_ipv4: bool,
    run_timeout: Duration,
    copy_timeout: Duration,
    egress: EgressLease,
    vmm_digest: Arc<AsyncMutex<Option<CachedFileDigest>>>,
    runtime_digest: Arc<AsyncMutex<Option<CachedFileDigest>>>,
    firmware_digest: Arc<AsyncMutex<Option<CachedFileDigest>>>,
}

impl VmImageBuilder {
    /// Creates an image builder backed by a dedicated VMM executable and guest
    /// runtime disk.
    ///
    /// For Dockerfiles with `RUN` or `COPY`, the executable must accept a
    /// private [`crate::host::VmProcessConfig`] path as its final argument. Use
    /// [`Self::vmm_arg`] when that entry point requires a preceding flag or
    /// subcommand. Flatten-only Dockerfiles do not launch it.
    #[must_use]
    pub fn new(vmm: impl Into<PathBuf>, runtime_image: impl Into<PathBuf>) -> Self {
        Self {
            vmm: vmm.into(),
            vmm_arguments: Vec::new(),
            vmm_build_cache_identity: None,
            runtime_image: runtime_image.into(),
            firmware_directory: None,
            cpus: DEFAULT_BUILD_VM_CPUS,
            memory_mib: DEFAULT_BUILD_VM_MEMORY_MIB,
            prefer_ipv4: false,
            run_timeout: DEFAULT_RUN_TIMEOUT,
            copy_timeout: DEFAULT_COPY_TIMEOUT,
            egress: EgressLease::internet(),
            vmm_digest: Arc::new(AsyncMutex::new(None)),
            runtime_digest: Arc::new(AsyncMutex::new(None)),
            firmware_digest: Arc::new(AsyncMutex::new(None)),
        }
    }

    /// Sets the directory containing the platform's libkrun firmware library.
    ///
    /// A present `libkrunfw.5.dylib` on macOS or `libkrunfw.so.5` on Linux is
    /// added to the dedicated VMM's platform library search path. Builders and
    /// VMMs with system-installed firmware can omit this setting. Set it
    /// explicitly when reproducible build-cache invalidation across firmware
    /// upgrades matters; system-installed firmware is treated as caller-owned
    /// stable runtime state.
    #[must_use]
    pub fn firmware_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.firmware_directory = Some(directory.into());
        self.firmware_digest = Arc::new(AsyncMutex::new(None));
        self
    }

    /// Appends one argument before the private VM process-config path.
    ///
    /// Use this when one executable exposes its dedicated VMM entry point
    /// behind a subcommand or flag. For example, the end-to-end proof binary
    /// uses `.vmm_arg("--vmm")`.
    #[must_use]
    pub fn vmm_arg(mut self, argument: impl Into<OsString>) -> Self {
        self.vmm_arguments.push(argument.into());
        self
    }

    /// Appends arguments before the private VM process-config path.
    #[must_use]
    pub fn vmm_args<I, A>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        self.vmm_arguments
            .extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Uses a caller-owned semantic identity for the Dockerfile build VMM.
    ///
    /// The default hashes the complete VMM executable, which is the safest
    /// policy for arbitrary callers. An application whose VMM entry point is
    /// embedded in a larger executable may use this override to keep unrelated
    /// application changes from invalidating every built root disk.
    ///
    /// The identity must change whenever the configured VMM's execution
    /// semantics can change a Dockerfile build result. VMM arguments, guest
    /// runtime, firmware, resource policy, networking, resolver state, and
    /// egress scope remain independent parts of the cache key.
    /// [`Self::prepare`] rejects an empty identity or one larger than 4 KiB.
    #[must_use]
    pub fn vmm_build_cache_identity(mut self, identity: impl Into<String>) -> Self {
        self.vmm_build_cache_identity = Some(identity.into());
        self
    }

    /// Sets the virtual CPU count for Dockerfile build VMs.
    ///
    /// The default is 2. The underlying VMM returns a typed configuration
    /// error when the value is unsupported.
    #[must_use]
    pub const fn cpus(mut self, cpus: u8) -> Self {
        self.cpus = cpus;
        self
    }

    /// Sets memory for Dockerfile build VMs in mebibytes.
    ///
    /// The default is 4096 MiB.
    #[must_use]
    pub const fn memory_mib(mut self, memory_mib: u32) -> Self {
        self.memory_mib = memory_mib;
        self
    }

    /// Prefers IPv4 inside each ephemeral Dockerfile build VM.
    ///
    /// The default leaves address selection unchanged. Under libkrun TSI this
    /// policy makes normal/default address selection prefer IPv4; it does not
    /// prevent a process from explicitly requesting `AF_INET6`. The policy
    /// changes only the build guest's in-memory `/proc` network state, not the
    /// prepared root filesystem or eventual task VM network policy.
    #[must_use]
    pub const fn prefer_ipv4(mut self) -> Self {
        self.prefer_ipv4 = true;
        self
    }

    /// Sets the timeout for each Dockerfile `RUN` instruction.
    ///
    /// The default is 30 minutes. Timeout cancellation terminates the guest
    /// process group before the build returns an error.
    #[must_use]
    pub const fn run_timeout(mut self, timeout: Duration) -> Self {
        self.run_timeout = timeout;
        self
    }

    /// Sets the timeout for each build-context mount, `WORKDIR`, and `COPY`
    /// operation.
    ///
    /// The default is 10 minutes.
    #[must_use]
    pub const fn copy_timeout(mut self, timeout: Duration) -> Self {
        self.copy_timeout = timeout;
        self
    }

    /// Sets the complete egress lease cloned into each Dockerfile build VM.
    ///
    /// The default permits ordinary internet access. Use
    /// `EgressLease::disabled()` for an offline build, or supply a composed
    /// host-owned MPP/secret lease. Lease values remain redacted from `Debug`.
    #[must_use]
    pub fn egress(mut self, egress: EgressLease) -> Self {
        self.egress = egress;
        self
    }

    async fn build_cache_inputs(&self) -> Result<BuildCacheInputs, ImageError> {
        if self
            .vmm_build_cache_identity
            .as_ref()
            .is_some_and(|identity| {
                identity.is_empty() || identity.len() > MAX_VMM_BUILD_CACHE_IDENTITY_BYTES
            })
        {
            return Err(ImageError::InvalidVmmBuildCacheIdentity);
        }
        let network = match self.egress.network() {
            Network::Disabled => "disabled",
            Network::Internet => "internet",
            Network::Gvproxy { .. } => "gvproxy",
        }
        .to_owned();
        let egress_scope = self
            .egress
            .build_cache_scope()
            .ok_or(ImageError::UnscopedBuildEgress)?
            .to_owned();
        let (vmm_digest, runtime_digest, firmware_digest) =
            if let Some(directory) = &self.firmware_directory {
                // Versioned libkrunfw installations expose the ABI filename as
                // a symlink to the immutable versioned library. Hash the
                // resolved regular file while retaining the configured
                // directory for the dynamic loader.
                let firmware = directory.join(FIRMWARE_LIBRARY_FILENAME).canonicalize()?;
                let (vmm_digest, runtime_digest, firmware_digest) = tokio::try_join!(
                    cached_file_digest(&self.vmm, &self.vmm_digest),
                    cached_file_digest(&self.runtime_image, &self.runtime_digest),
                    cached_file_digest(&firmware, &self.firmware_digest),
                )?;
                (vmm_digest, runtime_digest, firmware_digest)
            } else {
                let (vmm_digest, runtime_digest) = tokio::try_join!(
                    cached_file_digest(&self.vmm, &self.vmm_digest),
                    cached_file_digest(&self.runtime_image, &self.runtime_digest),
                )?;
                (vmm_digest, runtime_digest, "system-firmware".to_owned())
            };
        let vmm_identity = self
            .vmm_build_cache_identity
            .as_ref()
            .map_or(vmm_digest, |identity| format!("caller-owned:{identity}"));
        Ok(BuildCacheInputs {
            vmm_identity,
            runtime_digest,
            firmware_digest,
            resolver: match self.egress.network() {
                Network::Internet => Some(host_resolver_configuration()?),
                Network::Disabled | Network::Gvproxy { .. } => None,
            },
            network,
            egress_scope,
        })
    }

    /// Prepares one immutable root disk from `directory/Dockerfile`.
    ///
    /// `disk_bytes` is rounded up to the minimum supported root-disk size.
    /// Cache keys include the Dockerfile, ordered context archive, base image
    /// manifests, target platform and disk size, plus the complete build VM,
    /// resolver, and non-secret egress execution identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the Dockerfile is unsupported, an OCI artifact
    /// cannot be resolved, a build step fails, or the resulting ext4 disk is
    /// invalid.
    pub async fn prepare(
        &self,
        directory: impl AsRef<Path>,
        disk_bytes: u64,
        cache: impl AsRef<Path>,
        policy: CachePolicy,
    ) -> Result<PreparedRootDisk, ImageError> {
        let directory = directory.as_ref();
        let cache = cache.as_ref();
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.image.prepare",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            image.context.path = %directory.display(),
            image.cache.path = %cache.display(),
            image.disk.bytes = disk_bytes.max(MINIMUM_DISK_BYTES),
            image.cache.policy = policy.as_str(),
            image.build.prefer_ipv4 = self.prefer_ipv4,
            image.cache.status = tracing::field::Empty,
            image.manifest.source = tracing::field::Empty,
            image.manifest.digest = tracing::field::Empty,
            image.root.path = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
            duration_ns = tracing::field::Empty,
        );
        let started_at = std::time::Instant::now();
        let result =
            PreparedRootDisk::prepare_directory(directory, disk_bytes, cache, policy, self)
                .instrument(span.clone())
                .await;
        span.record(
            "duration_ns",
            u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        match &result {
            Ok(image) => {
                span.record("otel.status_code", "OK");
                span.record("status", "completed");
                span.record("image.cache.status", image.disk_status().as_str());
                span.record("image.manifest.source", image.manifest_source().as_str());
                span.record("image.manifest.digest", image.manifest_digest());
                span.record("image.root.path", image.path().display().to_string());
            }
            Err(error) => {
                span.record("otel.status_code", "ERROR");
                span.record("status", "failed");
                span.record("error.message", error.to_string());
            }
        }
        result
    }
}

/// An immutable, content-addressed root disk in the image cache.
#[derive(Clone, Debug)]
pub struct PreparedRootDisk {
    path: PathBuf,
    workdir: String,
    shell: String,
    environment: BTreeMap<String, String>,
    manifest_digest: String,
    manifest_source: ManifestSource,
    disk_status: DiskStatus,
}

/// Whether mutable OCI references may use their validated local resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachePolicy {
    /// Reuse a valid local manifest record and its cached blobs.
    Reuse,
    /// Resolve the mutable image reference from the registry again.
    Refresh,
}

impl CachePolicy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reuse => "reuse",
            Self::Refresh => "refresh",
        }
    }
}

/// Where the image manifest used for this preparation was resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestSource {
    /// A validated local manifest record supplied the immutable digest.
    Local,
    /// The OCI registry supplied the immutable digest.
    Registry,
}

impl ManifestSource {
    /// Returns the stable telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Registry => "registry",
        }
    }
}

/// Whether the prepared disk was reused or materialized by this call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiskStatus {
    /// The complete prepared disk was already present.
    Hit,
    /// This call materialized and atomically published the prepared disk.
    Created,
}

impl DiskStatus {
    /// Returns the stable telemetry spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Created => "created",
        }
    }
}

/// Failure to resolve, build, validate, or instantiate a VM root disk.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// A filesystem operation failed.
    #[error("VM image filesystem operation failed: {0}")]
    Io(#[from] io::Error),

    /// An OCI image reference could not be parsed.
    #[error("invalid OCI image reference {image}: {source}")]
    Reference {
        /// The caller-provided image reference.
        image: String,
        /// The OCI reference parser failure.
        #[source]
        source: oci_client::ParseError,
    },

    /// An OCI registry operation failed.
    #[error("OCI registry operation failed: {0}")]
    Registry(#[from] oci_client::errors::OciDistributionError),

    /// Formatting an ext4 disk failed.
    #[error("failed to format ext4 root disk: {0}")]
    Ext4(#[from] arcbox_ext4::error::FormatError),

    /// Inspecting an ext4 disk failed.
    #[error("failed to inspect ext4 root disk: {0}")]
    ReadExt4(#[from] arcbox_ext4::error::ReadError),

    /// A blocking disk materialization task failed.
    #[error("root disk formatting task failed: {0}")]
    Join(#[from] tokio::task::JoinError),

    /// Guest-visible egress state can affect a build but has no safe cache
    /// identity.
    #[error(
        "Dockerfile build output cannot be cached because the egress lease has no build-cache scope"
    )]
    UnscopedBuildEgress,

    /// A caller-owned VMM build-cache identity was empty or unreasonably large.
    #[error("VMM build-cache identity must contain between 1 and 4096 bytes")]
    InvalidVmmBuildCacheIdentity,

    /// The constrained Dockerfile parser rejected an instruction.
    #[error("unsupported Dockerfile instruction: {0}")]
    UnsupportedDockerfile(String),

    /// A Dockerfile did not contain a valid base stage.
    #[error("Dockerfile must contain exactly one FROM instruction")]
    InvalidFrom,

    /// An OCI layer uses a compression or media type that is not supported.
    #[error("unsupported OCI layer media type: {0}")]
    UnsupportedLayer(String),

    /// A prepared root disk is missing a required path.
    #[error("prepared image is missing required path {0}")]
    MissingPreparedPath(&'static str),

    /// OCI image configuration JSON was invalid.
    #[error("invalid OCI image JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// A build VM operation failed.
    #[error("VM image build failed: {0}")]
    Vm(#[from] VmToolSessionError),

    /// A Dockerfile instruction returned a nonzero status.
    #[error(
        "Dockerfile stage {stage} instruction {instruction} exited with {exit_code}\nstdout (tail):\n{stdout}\nstderr (tail):\n{stderr}"
    )]
    BuildStep {
        /// Zero-based Dockerfile stage index.
        stage: usize,
        /// Zero-based instruction index within the stage.
        instruction: usize,
        /// Guest process exit code.
        exit_code: i32,
        /// Bounded tail of standard output.
        stdout: String,
        /// Bounded tail of standard error.
        stderr: String,
    },

    /// A context-relative `COPY` source did not exist.
    #[error("COPY source does not exist in the build context: {0}")]
    MissingCopySource(String),

    /// `COPY --from` referred to neither an earlier stage nor an OCI image.
    #[error("COPY --from refers to unknown stage or image: {0}")]
    UnknownCopySource(String),
}

impl PreparedRootDisk {
    async fn prepare_directory(
        directory: &Path,
        disk_bytes: u64,
        cache: &Path,
        policy: CachePolicy,
        builder: &VmImageBuilder,
    ) -> Result<Self, ImageError> {
        let dockerfile_path = directory.join("Dockerfile");
        let dockerfile = fs::read_to_string(&dockerfile_path)?;
        info!(
            target: "nanocodex_vm",
            content_kind = "dockerfile",
            content = dockerfile,
            "VM image input"
        );
        let recipe = DockerfileRecipe::parse(&dockerfile)?;
        let disk_bytes = disk_bytes.max(MINIMUM_DISK_BYTES);
        let images = resolve_recipe_images(&recipe, cache, policy).await?;
        let final_stage = recipe.final_stage().ok_or(ImageError::InvalidFrom)?;
        let final_image = images
            .get(&final_stage.base_image)
            .ok_or_else(|| ImageError::UnknownCopySource(final_stage.base_image.clone()))?;
        let (path, disk_status, environment, shell) = if recipe.requires_build() {
            let build_cache_inputs = builder.build_cache_inputs().await?;
            let execution = BuildExecution {
                builder,
                cache_inputs: &build_cache_inputs,
            };
            prepare_built_disk(
                directory,
                &dockerfile,
                &recipe,
                &images,
                cache,
                disk_bytes,
                &execution,
            )
            .await?
        } else {
            let (path, status, shell) =
                prepare_flattened_disk(cache, final_image, disk_bytes).await?;
            (
                path,
                status,
                final_recipe_environment(&recipe, &images)?,
                shell,
            )
        };
        Ok(Self {
            shell,
            path,
            workdir: recipe
                .final_workdir()
                .unwrap_or(&final_image.config.working_directory)
                .to_owned(),
            environment,
            manifest_digest: final_image.manifest_digest.clone(),
            manifest_source: final_image.source,
            disk_status,
        })
    }

    /// Returns the immutable cached root-disk path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the final Dockerfile working directory.
    #[must_use]
    pub fn workdir(&self) -> &str {
        &self.workdir
    }

    /// Returns the detected guest shell name, either `bash` or `sh`.
    #[must_use]
    pub fn shell(&self) -> &str {
        &self.shell
    }

    /// Returns the final Docker process environment.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    /// Returns the immutable digest of the final stage's base manifest.
    #[must_use]
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    /// Returns how the final stage's base manifest was resolved.
    #[must_use]
    pub const fn manifest_source(&self) -> ManifestSource {
        self.manifest_source
    }

    /// Returns whether this preparation reused or created the cached disk.
    #[must_use]
    pub const fn disk_status(&self) -> DiskStatus {
        self.disk_status
    }

    /// Creates a disposable copy-on-write attempt disk.
    ///
    /// The destination must not exist and its parent directory must already
    /// exist. Filesystems without reflink support use a sparse copy fallback.
    /// The returned value is the logical disk size in bytes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the destination exists or cannot be cloned.
    pub fn reflink_to(&self, destination: impl AsRef<Path>) -> Result<u64, ImageError> {
        let destination = destination.as_ref();
        if destination.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "attempt root disk already exists: {}",
                    destination.display()
                ),
            )
            .into());
        }
        Ok(reflink_or_sparse_copy(&self.path, destination)?)
    }

    /// Materializes a private writable root and applies this image's runtime
    /// working directory, shell, and environment to its workspace builder.
    ///
    /// This is the normal library path from a prepared image to a retained VM.
    /// The caller can continue configuring the returned builder with a guest
    /// runtime disk, firmware, resources, and egress policy before launch.
    ///
    /// # Errors
    ///
    /// Returns an error when the private root cannot be materialized.
    pub fn private_workspace(
        &self,
        private_rootfs: impl Into<PathBuf>,
        vmm_executable: impl Into<PathBuf>,
    ) -> Result<VmWorkspaceBuilder, VmWorkspaceError> {
        Ok(
            VmWorkspaceBuilder::private_from(&self.path, private_rootfs, vmm_executable)?
                .guest_workspace(self.workdir.clone())
                .shell(self.shell.clone())
                .environment(self.environment.clone()),
        )
    }
}

#[derive(Deserialize, Serialize)]
struct ReferenceRecord {
    version: u32,
    image_reference: String,
    manifest_digest: String,
    layers: Vec<LayerRecord>,
    #[serde(default)]
    config: ImageRuntimeConfig,
}

#[derive(Deserialize, Serialize)]
struct PreparedDiskRecord {
    version: u32,
    file: CacheFileIdentity,
    shell: String,
}

#[derive(Deserialize, Serialize)]
struct BlobRecord {
    version: u32,
    digest: String,
    file: CacheFileIdentity,
}

#[derive(Deserialize, Serialize)]
struct CachedExt4Record {
    version: u32,
    file: CacheFileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CacheFileIdentity {
    bytes: u64,
    modified_nanos: u128,
    changed_seconds: i64,
    changed_nanos: i64,
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug)]
struct CachedFileDigest {
    file: CacheFileIdentity,
    digest: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct LayerRecord {
    digest: String,
    media_type: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct ImageRuntimeConfig {
    environment: BTreeMap<String, String>,
    working_directory: String,
}

impl Default for ImageRuntimeConfig {
    fn default() -> Self {
        Self {
            environment: BTreeMap::new(),
            working_directory: "/".to_owned(),
        }
    }
}

#[derive(Clone)]
struct BuildCacheInputs {
    vmm_identity: String,
    runtime_digest: String,
    firmware_digest: String,
    resolver: Option<String>,
    network: String,
    egress_scope: String,
}

struct BuildExecution<'a> {
    builder: &'a VmImageBuilder,
    cache_inputs: &'a BuildCacheInputs,
}

fn read_cache_record<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ImageError> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(record) => Ok(Some(record)),
            Err(error) => {
                info!(
                    target: "nanocodex_vm",
                    cache_record_path = %path.display(),
                    error = %error,
                    "ignoring invalid VM image cache record"
                );
                Ok(None)
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_cache_record(path: &Path, record: &impl Serialize) -> Result<(), ImageError> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("VM cache record path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".record.")
        .tempfile_in(parent)?;
    serde_json::to_writer(temporary.as_file_mut(), record)?;
    publish(temporary.into_temp_path(), path)?;
    Ok(())
}

struct CacheLock(File);

impl CacheLock {
    fn acquire(cache: &Path, namespace: &str, key: &str) -> io::Result<Self> {
        let directory = cache.join("locks").join(namespace);
        fs::create_dir_all(&directory)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .truncate(false)
            .write(true)
            .open(directory.join(format!("{key}.lock")))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self(file))
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

async fn acquire_cache_lock(
    cache: &Path,
    namespace: &'static str,
    key: &str,
) -> Result<CacheLock, ImageError> {
    let span = info_span!(
        target: "nanocodex_vm",
        "vm.image.cache_lock",
        otel.kind = "internal",
        image.cache.namespace = namespace,
        image.cache.key = key,
    );
    let cache = cache.to_path_buf();
    let key = key.to_owned();
    tokio::task::spawn_blocking(move || {
        span.in_scope(|| CacheLock::acquire(&cache, namespace, &key))
    })
    .await?
    .map_err(Into::into)
}

fn temporary_path(directory: &Path, prefix: &str) -> io::Result<tempfile::TempPath> {
    Ok(tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(directory)?
        .into_temp_path())
}

fn publish(temporary: tempfile::TempPath, destination: &Path) -> io::Result<()> {
    temporary.persist(destination).map_err(|error| error.error)
}

#[derive(Debug, Eq, PartialEq)]
struct DockerfileRecipe {
    stages: Vec<DockerfileStage>,
}

#[derive(Debug, Eq, PartialEq)]
struct DockerfileStage {
    base_image: String,
    name: Option<String>,
    instructions: Vec<DockerfileInstruction>,
}

#[derive(Debug, Eq, PartialEq)]
enum DockerfileInstruction {
    Run(String),
    Copy(DockerfileCopy),
    Workdir(String),
    Env {
        name: String,
        value: String,
    },
    Arg {
        name: String,
        default: Option<String>,
    },
    Cmd(String),
}

#[derive(Debug, Eq, PartialEq)]
struct DockerfileCopy {
    from: Option<String>,
    sources: Vec<String>,
    destination: String,
}

impl DockerfileRecipe {
    fn parse(dockerfile: &str) -> Result<Self, ImageError> {
        let mut stages = Vec::<DockerfileStage>::new();
        for line in dockerfile_logical_lines(dockerfile)? {
            let (instruction, arguments) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| ImageError::UnsupportedDockerfile(line.clone()))?;
            match instruction.to_ascii_uppercase().as_str() {
                "FROM" => {
                    let fields = arguments.split_whitespace().collect::<Vec<_>>();
                    let (base_image, name) = match fields.as_slice() {
                        [base_image] => ((*base_image).to_owned(), None),
                        [base_image, keyword, name] if keyword.eq_ignore_ascii_case("AS") => {
                            ((*base_image).to_owned(), Some((*name).to_owned()))
                        }
                        _ => return Err(ImageError::InvalidFrom),
                    };
                    if base_image.is_empty() {
                        return Err(ImageError::InvalidFrom);
                    }
                    stages.push(DockerfileStage {
                        base_image,
                        name,
                        instructions: Vec::new(),
                    });
                }
                "WORKDIR" => {
                    if arguments.split_whitespace().count() != 1 || !valid_guest_workdir(arguments)
                    {
                        return Err(ImageError::UnsupportedDockerfile(line.clone()));
                    }
                    current_stage(&mut stages, &line)?
                        .instructions
                        .push(DockerfileInstruction::Workdir(arguments.to_owned()));
                }
                "RUN" if !arguments.trim().is_empty() => current_stage(&mut stages, &line)?
                    .instructions
                    .push(DockerfileInstruction::Run(arguments.to_owned())),
                "COPY" => current_stage(&mut stages, &line)?
                    .instructions
                    .push(DockerfileInstruction::Copy(parse_copy(arguments, &line)?)),
                "ENV" => {
                    let assignments = parse_environment_assignments(arguments, &line)?;
                    current_stage(&mut stages, &line)?.instructions.extend(
                        assignments
                            .into_iter()
                            .map(|(name, value)| DockerfileInstruction::Env { name, value }),
                    );
                }
                "ARG" => {
                    let (name, default) = match arguments.split_once('=') {
                        Some((name, value)) => (name, Some(value.to_owned())),
                        None => (arguments, None),
                    };
                    if !valid_environment_name(name) {
                        return Err(ImageError::UnsupportedDockerfile(line));
                    }
                    current_stage(&mut stages, &line)?.instructions.push(
                        DockerfileInstruction::Arg {
                            name: name.to_owned(),
                            default,
                        },
                    );
                }
                "CMD" if !arguments.trim().is_empty() => current_stage(&mut stages, &line)?
                    .instructions
                    .push(DockerfileInstruction::Cmd(arguments.to_owned())),
                _ => return Err(ImageError::UnsupportedDockerfile(line.clone())),
            }
        }
        if stages.is_empty() {
            return Err(ImageError::InvalidFrom);
        }
        Ok(Self { stages })
    }

    fn final_stage(&self) -> Option<&DockerfileStage> {
        self.stages.last()
    }

    fn final_workdir(&self) -> Option<&str> {
        self.final_stage()
            .into_iter()
            .flat_map(|stage| stage.instructions.iter().rev())
            .find_map(|instruction| match instruction {
                DockerfileInstruction::Workdir(workdir) => Some(workdir.as_str()),
                DockerfileInstruction::Run(_)
                | DockerfileInstruction::Copy(_)
                | DockerfileInstruction::Env { .. }
                | DockerfileInstruction::Arg { .. }
                | DockerfileInstruction::Cmd(_) => None,
            })
    }

    fn requires_build(&self) -> bool {
        self.stages.iter().any(|stage| {
            stage.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    DockerfileInstruction::Run(_) | DockerfileInstruction::Copy(_)
                )
            })
        })
    }
}

fn dockerfile_logical_lines(dockerfile: &str) -> Result<Vec<String>, ImageError> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for raw_line in dockerfile.lines() {
        let trimmed = raw_line.trim();
        if current.is_empty() && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        let continuation = raw_line.trim_end().ends_with('\\');
        let fragment = if continuation {
            raw_line.trim_end().strip_suffix('\\').unwrap_or(raw_line)
        } else {
            raw_line
        };
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(fragment.trim());
        if !continuation {
            let logical = std::mem::take(&mut current);
            if !logical.is_empty() {
                lines.push(logical);
            }
        }
    }
    if !current.is_empty() {
        return Err(ImageError::UnsupportedDockerfile(current));
    }
    Ok(lines)
}

fn current_stage<'a>(
    stages: &'a mut [DockerfileStage],
    line: &str,
) -> Result<&'a mut DockerfileStage, ImageError> {
    stages
        .last_mut()
        .ok_or_else(|| ImageError::UnsupportedDockerfile(line.to_owned()))
}

fn parse_copy(arguments: &str, line: &str) -> Result<DockerfileCopy, ImageError> {
    let mut fields = arguments.split_whitespace();
    let first = fields
        .next()
        .ok_or_else(|| ImageError::UnsupportedDockerfile(line.to_owned()))?;
    let (from, first_source) = match first.strip_prefix("--from=") {
        Some(from) if !from.is_empty() => (Some(from.to_owned()), fields.next()),
        Some(_) => return Err(ImageError::UnsupportedDockerfile(line.to_owned())),
        None => (None, Some(first)),
    };
    let mut paths = first_source
        .into_iter()
        .chain(fields)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if paths.len() < 2 {
        return Err(ImageError::UnsupportedDockerfile(line.to_owned()));
    }
    let destination = paths
        .pop()
        .ok_or_else(|| ImageError::UnsupportedDockerfile(line.to_owned()))?;
    Ok(DockerfileCopy {
        from,
        sources: paths,
        destination,
    })
}

fn parse_environment_assignments(
    arguments: &str,
    line: &str,
) -> Result<Vec<(String, String)>, ImageError> {
    let mut fields = shlex::split(arguments)
        .ok_or_else(|| ImageError::UnsupportedDockerfile(line.to_owned()))?;
    let Some(first) = fields.first() else {
        return Err(ImageError::UnsupportedDockerfile(line.to_owned()));
    };
    if !first.contains('=') {
        if fields.len() < 2 || !valid_environment_name(first) {
            return Err(ImageError::UnsupportedDockerfile(line.to_owned()));
        }
        let name = fields.remove(0);
        return Ok(vec![(name, fields.join(" "))]);
    }

    fields
        .into_iter()
        .map(|assignment| {
            let (name, value) = assignment
                .split_once('=')
                .ok_or_else(|| ImageError::UnsupportedDockerfile(line.to_owned()))?;
            if !valid_environment_name(name) {
                return Err(ImageError::UnsupportedDockerfile(line.to_owned()));
            }
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn valid_guest_workdir(workdir: &str) -> bool {
    let path = Path::new(workdir);
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

async fn resolve_recipe_images(
    recipe: &DockerfileRecipe,
    cache: &Path,
    policy: CachePolicy,
) -> Result<BTreeMap<String, PulledImage>, ImageError> {
    let stage_names = recipe
        .stages
        .iter()
        .filter_map(|stage| stage.name.as_deref())
        .collect::<BTreeSet<_>>();
    let mut references = recipe
        .stages
        .iter()
        .map(|stage| stage.base_image.clone())
        .collect::<BTreeSet<_>>();
    for copy in recipe.stages.iter().flat_map(|stage| {
        stage
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                DockerfileInstruction::Copy(copy) => Some(copy),
                DockerfileInstruction::Run(_)
                | DockerfileInstruction::Workdir(_)
                | DockerfileInstruction::Env { .. }
                | DockerfileInstruction::Arg { .. }
                | DockerfileInstruction::Cmd(_) => None,
            })
    }) {
        if let Some(from) = copy.from.as_deref()
            && !stage_names.contains(from)
            && from.parse::<usize>().is_err()
        {
            references.insert(from.to_owned());
        }
    }
    let images = stream::iter(references.into_iter().map(|reference| {
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.image.resolve",
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            image.reference = reference.as_str(),
            image.cache.policy = policy.as_str(),
            image.manifest.source = tracing::field::Empty,
            image.manifest.digest = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
        );
        let operation_span = span.clone();
        async move {
            let result = resolve_image(&reference, cache, policy).await;
            match &result {
                Ok(image) => {
                    operation_span.record("otel.status_code", "OK");
                    operation_span.record("status", "completed");
                    operation_span.record("image.manifest.source", image.source.as_str());
                    operation_span.record("image.manifest.digest", image.manifest_digest.as_str());
                }
                Err(error) => {
                    operation_span.record("otel.status_code", "ERROR");
                    operation_span.record("status", "failed");
                    operation_span.record("error.message", error.to_string());
                }
            }
            result.map(|image| (reference, image))
        }
        .instrument(span)
    }))
    .buffered(MAX_CONCURRENT_IMAGE_RESOLVES)
    .try_collect::<Vec<_>>()
    .await?;
    Ok(images.into_iter().collect())
}

async fn prepare_flattened_disk(
    cache: &Path,
    image: &PulledImage,
    disk_bytes: u64,
) -> Result<(PathBuf, DiskStatus, String), ImageError> {
    let key = disk_cache_key(&image.manifest_digest, disk_bytes);
    let path = cache.join("images").join(format!("{key}.ext4"));
    if let Some(shell) = cached_root_disk(&path) {
        return Ok((path, DiskStatus::Hit, shell));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("prepared root disk cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let _lock = acquire_cache_lock(cache, "images", &key).await?;
    if let Some(shell) = cached_root_disk(&path) {
        return Ok((path, DiskStatus::Hit, shell));
    }
    let temporary = temporary_path(parent, &format!(".{key}."))?;
    let temporary_for_task = temporary.to_path_buf();
    let layers = image.layers.clone();
    let span = info_span!(
        target: "nanocodex_vm",
        "vm.image.format",
        otel.kind = "internal",
        image.disk.bytes = disk_bytes,
        image.layer.count = layers.len(),
    );
    tokio::task::spawn_blocking(move || {
        span.in_scope(|| format_root_disk(&temporary_for_task, disk_bytes, &layers))
    })
    .await??;
    validate_root_disk(&temporary)?;
    publish(temporary, &path)?;
    let shell = record_prepared_disk(&path)?;
    Ok((path, DiskStatus::Created, shell))
}

async fn prepare_copy_source_disk(
    cache: &Path,
    image: &PulledImage,
    disk_bytes: u64,
) -> Result<PathBuf, ImageError> {
    let key = disk_cache_key(&image.manifest_digest, disk_bytes);
    let path = cache.join("images").join(format!("{key}.ext4"));
    if valid_cached_ext4_disk(&path)? {
        return Ok(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("prepared copy source disk cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let _lock = acquire_cache_lock(cache, "images", &key).await?;
    if valid_cached_ext4_disk(&path)? {
        return Ok(path);
    }
    let temporary = temporary_path(parent, &format!(".{key}."))?;
    let temporary_for_task = temporary.to_path_buf();
    let layers = image.layers.clone();
    let span = info_span!(
        target: "nanocodex_vm",
        "vm.image.format",
        otel.kind = "internal",
        image.disk.bytes = disk_bytes,
        image.layer.count = layers.len(),
    );
    tokio::task::spawn_blocking(move || {
        span.in_scope(|| format_root_disk(&temporary_for_task, disk_bytes, &layers))
    })
    .await??;
    validate_ext4_disk(&temporary)?;
    publish(temporary, &path)?;
    record_cached_ext4_disk(&path)?;
    Ok(path)
}

async fn prepare_built_disk(
    context_directory: &Path,
    dockerfile: &str,
    recipe: &DockerfileRecipe,
    images: &BTreeMap<String, PulledImage>,
    cache: &Path,
    disk_bytes: u64,
    execution: &BuildExecution<'_>,
) -> Result<(PathBuf, DiskStatus, BTreeMap<String, String>, String), ImageError> {
    let builder = execution.builder;
    let build_cache_inputs = execution.cache_inputs;
    let context_directory = context_directory.to_path_buf();
    let context_cache = cache.to_path_buf();
    let span = info_span!(
        target: "nanocodex_vm",
        "vm.image.context",
        otel.kind = "internal",
        image.context.path = %context_directory.display(),
        image.disk.bytes = disk_bytes,
    );
    let (context_image, context_digest) = tokio::task::spawn_blocking(move || {
        span.in_scope(|| prepare_context_disk(&context_directory, &context_cache, disk_bytes))
    })
    .await??;
    let key = build_cache_key(
        dockerfile,
        &context_digest,
        recipe,
        images,
        disk_bytes,
        builder,
        build_cache_inputs,
    );
    let builds = cache.join("builds");
    fs::create_dir_all(&builds)?;
    let path = builds.join(format!("{key}.ext4"));
    let final_environment = final_recipe_environment(recipe, images)?;
    if let Some(shell) = cached_root_disk(&path) {
        return Ok((path, DiskStatus::Hit, final_environment, shell));
    }
    let _lock = acquire_cache_lock(cache, "builds", &key).await?;
    if let Some(shell) = cached_root_disk(&path) {
        return Ok((path, DiskStatus::Hit, final_environment, shell));
    }

    let temporary = tempfile::Builder::new()
        .prefix(&format!("{key}."))
        .tempdir_in(&builds)?;
    let mut stage_disks = Vec::with_capacity(recipe.stages.len());
    for (stage_index, stage) in recipe.stages.iter().enumerate() {
        let image = images
            .get(&stage.base_image)
            .ok_or_else(|| ImageError::UnknownCopySource(stage.base_image.clone()))?;
        let (base, _, _) = prepare_flattened_disk(cache, image, disk_bytes).await?;
        let stage_root = temporary.path().join(format!("stage-{stage_index}.ext4"));
        reflink_or_sparse_copy(&base, &stage_root)?;
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.image.stage",
            otel.kind = "internal",
            image.stage.index = stage_index,
            image.stage.base = stage.base_image.as_str(),
            image.stage.instructions = stage.instructions.len(),
        );
        execute_stage(
            stage_index,
            stage,
            recipe,
            images,
            cache,
            disk_bytes,
            &context_image,
            &stage_disks,
            &stage_root,
            builder,
            build_cache_inputs.resolver.as_deref(),
        )
        .instrument(span)
        .await?;
        stage_disks.push(stage_root);
    }
    let final_stage = stage_disks.last().ok_or(ImageError::InvalidFrom)?;
    let published = temporary.path().join("published.ext4");
    reflink_or_sparse_copy(final_stage, &published)?;
    validate_root_disk(&published)?;
    fs::rename(published, &path)?;
    let shell = record_prepared_disk(&path)?;
    Ok((path, DiskStatus::Created, final_environment, shell))
}

fn prepare_context_disk(
    environment: &Path,
    cache: &Path,
    task_disk_bytes: u64,
) -> Result<(PathBuf, String), ImageError> {
    let contexts = cache.join("contexts");
    fs::create_dir_all(&contexts)?;
    let mut archive_file = tempfile::NamedTempFile::new_in(&contexts)?;
    {
        let mut archive = tar::Builder::new(archive_file.as_file_mut());
        append_normalized_context_entry(&mut archive, environment, Path::new("context"))?;
        let mut walker = WalkBuilder::new(environment);
        walker
            .hidden(false)
            .parents(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(false)
            .sort_by_file_path(std::cmp::Ord::cmp);
        for entry in walker.build() {
            let entry = entry.map_err(io::Error::other)?;
            let relative = entry
                .path()
                .strip_prefix(environment)
                .map_err(io::Error::other)?;
            if relative.as_os_str().is_empty() {
                continue;
            }
            append_normalized_context_entry(
                &mut archive,
                entry.path(),
                &Path::new("context").join(relative),
            )?;
        }
        archive.finish()?;
    }
    archive_file.as_file_mut().seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = archive_file.as_file_mut().read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hex::encode(hasher.finalize());
    let disk_bytes = task_disk_bytes.max(CONTEXT_DISK_BYTES);
    let mut identity = Sha256::new();
    identity.update(b"nanocodex-vm-context-ext4-v1\0");
    identity.update(digest.as_bytes());
    identity.update(disk_bytes.to_le_bytes());
    let key = hex::encode(identity.finalize());
    let path = contexts.join(format!("{key}.ext4"));
    let _lock = CacheLock::acquire(cache, "contexts", &key)?;
    if !valid_cached_ext4_disk(&path)? {
        archive_file.as_file_mut().seek(SeekFrom::Start(0))?;
        let temporary = temporary_path(&contexts, &format!(".{key}."))?;
        let mut formatter = Formatter::new(&temporary, BLOCK_SIZE, disk_bytes)?;
        formatter.unpack_tar(BufReader::new(archive_file.as_file_mut()))?;
        formatter.close()?;
        validate_ext4_disk(&temporary)?;
        publish(temporary, &path)?;
        record_cached_ext4_disk(&path)?;
    }
    validate_ext4_disk(&path)?;
    Ok((path, digest))
}

fn append_normalized_context_entry<W>(
    archive: &mut tar::Builder<W>,
    source: &Path,
    archive_path: &Path,
) -> Result<(), ImageError>
where
    W: Write,
{
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        return Err(ImageError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "VM build-context symlinks are unsupported: {}",
                source.display()
            ),
        )));
    }
    let (entry_type, size) = if metadata.is_dir() {
        (tar::EntryType::Directory, 0)
    } else if metadata.is_file() {
        (tar::EntryType::Regular, metadata.len())
    } else {
        return Err(ImageError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported VM build-context entry: {}", source.display()),
        )));
    };
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(entry_type);
    header.set_size(size);
    #[cfg(unix)]
    header.set_mode(metadata.permissions().mode() & 0o7777);
    #[cfg(not(unix))]
    header.set_mode(if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    });
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    if metadata.is_file() {
        archive.append_data(
            &mut header,
            archive_path,
            BufReader::new(File::open(source)?),
        )?;
    } else {
        archive.append_data(&mut header, archive_path, io::empty())?;
    }
    Ok(())
}

struct BuildMount {
    key: String,
    disk: PathBuf,
    device: String,
    mount: String,
}

#[allow(clippy::too_many_arguments)]
async fn execute_stage(
    stage_index: usize,
    stage: &DockerfileStage,
    recipe: &DockerfileRecipe,
    images: &BTreeMap<String, PulledImage>,
    cache: &Path,
    disk_bytes: u64,
    context_image: &Path,
    stage_disks: &[PathBuf],
    stage_root: &Path,
    builder: &VmImageBuilder,
    resolver: Option<&str>,
) -> Result<(), ImageError> {
    let mut mounts = Vec::<BuildMount>::new();
    let mut source_mounts = BTreeMap::<String, String>::new();
    for copy in stage
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            DockerfileInstruction::Copy(copy) => Some(copy),
            DockerfileInstruction::Run(_)
            | DockerfileInstruction::Workdir(_)
            | DockerfileInstruction::Env { .. }
            | DockerfileInstruction::Arg { .. }
            | DockerfileInstruction::Cmd(_) => None,
        })
    {
        let Some(from) = copy.from.as_deref() else {
            continue;
        };
        if source_mounts.contains_key(from) {
            continue;
        }
        let disk = if let Some(source_stage) = resolve_stage_index(recipe, from) {
            if source_stage >= stage_index {
                return Err(ImageError::UnknownCopySource(from.to_owned()));
            }
            stage_disks
                .get(source_stage)
                .cloned()
                .ok_or_else(|| ImageError::UnknownCopySource(from.to_owned()))?
        } else {
            let image = images
                .get(from)
                .ok_or_else(|| ImageError::UnknownCopySource(from.to_owned()))?;
            prepare_copy_source_disk(cache, image, disk_bytes).await?
        };
        let source_number = mounts.len();
        let mount = format!("/mnt/nanocodex-source-{source_number}");
        source_mounts.insert(from.to_owned(), mount.clone());
        mounts.push(BuildMount {
            key: format!("source-{source_number}"),
            disk,
            device: guest_block_device(source_number + 3)?,
            mount,
        });
    }

    let (command, config, guest) =
        build_vmm_inputs(builder, stage_root, context_image, &mounts, resolver)?;
    let session =
        VmToolSession::spawn_configured(command, config, guest, builder.egress.clone()).await?;
    let execution = execute_stage_inner(
        &session,
        stage_index,
        stage,
        images,
        &source_mounts,
        &mounts,
        builder,
    )
    .await;
    let resolver_cleanup = if resolver.is_some() {
        run_build_command(
            &session,
            stage_index,
            stage.instructions.len(),
            VmCommand::new("/bin/sh")
                .arg("-c")
                .arg(RESTORE_BUILD_RESOLVER_SCRIPT)
                .timeout(builder.copy_timeout),
        )
        .await
    } else {
        Ok(())
    };
    let shutdown = session.shutdown().await;
    execution?;
    resolver_cleanup?;
    shutdown?;
    Ok(())
}

fn build_vmm_inputs(
    builder: &VmImageBuilder,
    stage_root: &Path,
    context_image: &Path,
    mounts: &[BuildMount],
    resolver: Option<&str>,
) -> Result<(Command, VmConfig, GuestCommand), ImageError> {
    let mut command = Command::new(&builder.vmm);
    command.args(&builder.vmm_arguments);
    if let Some(directory) = &builder.firmware_directory {
        configure_firmware_library_path(&mut command, directory)?;
    }
    let mut config = VmConfig::ext4(stage_root)
        .cpus(builder.cpus)
        .memory_mib(builder.memory_mib)
        .block_device(BlockDevice::read_only(
            BUILD_RUNTIME_ID,
            &builder.runtime_image,
        ))
        .block_device(BlockDevice::read_only(BUILD_CONTEXT_ID, context_image));
    for mount in mounts {
        config = config.block_device(BlockDevice::read_only(&mount.key, &mount.disk));
    }
    let guest = GuestCommand::new("/bin/sh")
        .arg("-c")
        .arg(build_guest_bootstrap_script(resolver, builder.prefer_ipv4))
        .arg("nanocodex-vm-image-build");
    Ok((command, config, guest))
}

fn build_guest_bootstrap_script(resolver: Option<&str>, prefer_ipv4: bool) -> String {
    let address_preference = if prefer_ipv4 {
        concat!(
            "if ! printf 1 > /proc/sys/net/ipv6/conf/all/disable_ipv6; then printf '%s\\n' 'nanocodex image build bootstrap: failed to write /proc/sys/net/ipv6/conf/all/disable_ipv6' >&2; exit 125; fi; ",
            "if ! printf 1 > /proc/sys/net/ipv6/conf/default/disable_ipv6; then printf '%s\\n' 'nanocodex image build bootstrap: failed to write /proc/sys/net/ipv6/conf/default/disable_ipv6' >&2; exit 125; fi; "
        )
    } else {
        ""
    };
    let resolver = resolver.map_or_else(String::new, |resolver| {
        format!(
            "rm -rf {BUILD_RESOLVER_STATE} && mkdir -p {BUILD_RESOLVER_STATE} && if [ -e /etc/resolv.conf ] || [ -L /etc/resolv.conf ]; then mv /etc/resolv.conf {BUILD_RESOLVER_STATE}/original; fi && printf '{resolver}' > /etc/resolv.conf && "
        )
    });
    format!(
        "{address_preference}{resolver}mkdir -p {BUILD_RUNTIME_MOUNT} && mount -t ext4 -o ro {BUILD_RUNTIME_DEVICE} {BUILD_RUNTIME_MOUNT} && exec {BUILD_RUNTIME_MOUNT}/nanocodex-vm-guest /"
    )
}

fn configure_firmware_library_path(command: &mut Command, directory: &Path) -> io::Result<()> {
    if directory.join(FIRMWARE_LIBRARY_FILENAME).is_file() {
        command.env(FIRMWARE_LIBRARY_PATH_ENVIRONMENT, directory.canonicalize()?);
    }
    Ok(())
}

async fn execute_stage_inner(
    session: &VmToolSession,
    stage_index: usize,
    stage: &DockerfileStage,
    images: &BTreeMap<String, PulledImage>,
    source_mounts: &BTreeMap<String, String>,
    mounts: &[BuildMount],
    builder: &VmImageBuilder,
) -> Result<(), ImageError> {
    mount_build_disk(
        session,
        BUILD_CONTEXT_DEVICE,
        BUILD_CONTEXT_MOUNT,
        stage_index,
        0,
        builder.copy_timeout,
    )
    .await?;
    for (index, mount) in mounts.iter().enumerate() {
        mount_build_disk(
            session,
            &mount.device,
            &mount.mount,
            stage_index,
            index + 1,
            builder.copy_timeout,
        )
        .await?;
    }

    let image = images
        .get(&stage.base_image)
        .ok_or_else(|| ImageError::UnknownCopySource(stage.base_image.clone()))?;
    let mut environment = docker_process_environment(&image.config.environment);
    let mut arguments = BTreeMap::<String, String>::new();
    let mut workdir = image.config.working_directory.clone();
    if !valid_guest_workdir(&workdir) {
        "/".clone_into(&mut workdir);
    }

    for (instruction_index, instruction) in stage.instructions.iter().enumerate() {
        match instruction {
            DockerfileInstruction::Workdir(directory) => {
                workdir.clone_from(directory);
                run_build_command(
                    session,
                    stage_index,
                    instruction_index,
                    VmCommand::new("/bin/mkdir")
                        .arg("-p")
                        .arg(directory)
                        .timeout(builder.copy_timeout),
                )
                .await?;
            }
            DockerfileInstruction::Env { name, value } => {
                let value = expand_variables(value, &environment, &arguments);
                environment.insert(name.clone(), value);
            }
            DockerfileInstruction::Arg { name, default } => {
                let value = default
                    .as_deref()
                    .map(|value| expand_variables(value, &environment, &arguments))
                    .unwrap_or_default();
                arguments.insert(name.clone(), value);
            }
            DockerfileInstruction::Run(script) => {
                let mut command = VmCommand::new("/bin/sh")
                    .arg("-c")
                    .arg(script)
                    .current_directory(&workdir)
                    .timeout(builder.run_timeout);
                command = command.environment(build_environment(&environment, &arguments));
                run_build_command(session, stage_index, instruction_index, command).await?;
            }
            DockerfileInstruction::Copy(copy) => {
                execute_copy(CopyExecution {
                    session,
                    stage_index,
                    instruction_index,
                    copy,
                    workdir: &workdir,
                    source_mounts,
                    environment: &environment,
                    arguments: &arguments,
                    timeout: builder.copy_timeout,
                })
                .await?;
            }
            DockerfileInstruction::Cmd(_) => {}
        }
    }
    Ok(())
}

async fn mount_build_disk(
    session: &VmToolSession,
    device: &str,
    mount: &str,
    stage: usize,
    instruction: usize,
    timeout: Duration,
) -> Result<(), ImageError> {
    run_build_command(
        session,
        stage,
        instruction,
        VmCommand::new("/bin/sh")
            .arg("-c")
            .arg("mkdir -p \"$1\" && mount -t ext4 -o ro \"$2\" \"$1\"")
            .arg("nanocodex-vm-mount")
            .arg(mount)
            .arg(device)
            .timeout(timeout),
    )
    .await
}

struct CopyExecution<'a> {
    session: &'a VmToolSession,
    stage_index: usize,
    instruction_index: usize,
    copy: &'a DockerfileCopy,
    workdir: &'a str,
    source_mounts: &'a BTreeMap<String, String>,
    environment: &'a BTreeMap<String, String>,
    arguments: &'a BTreeMap<String, String>,
    timeout: Duration,
}

async fn execute_copy(input: CopyExecution<'_>) -> Result<(), ImageError> {
    let source_root = match input.copy.from.as_deref() {
        None => format!("{BUILD_CONTEXT_MOUNT}/context"),
        Some(from) => input
            .source_mounts
            .get(from)
            .cloned()
            .ok_or_else(|| ImageError::UnknownCopySource(from.to_owned()))?,
    };
    let mut sources = Vec::with_capacity(input.copy.sources.len());
    for source in &input.copy.sources {
        let expanded = expand_variables(source, input.environment, input.arguments);
        let relative = expanded.strip_prefix('/').unwrap_or(&expanded);
        let relative = relative.strip_prefix("./").unwrap_or(relative);
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(ImageError::MissingCopySource(expanded));
        }
        let source_path = Path::new(&source_root).join(relative);
        sources.push(source_path.to_string_lossy().into_owned());
    }
    let destination = expand_variables(&input.copy.destination, input.environment, input.arguments);
    let destination = if Path::new(&destination).is_absolute() {
        destination
    } else {
        Path::new(input.workdir)
            .join(destination)
            .to_string_lossy()
            .into_owned()
    };
    let mut command = VmCommand::new("/bin/sh")
        .arg("-c")
        .arg(COPY_SCRIPT)
        .arg("nanocodex-vm-copy")
        .arg(destination)
        .timeout(input.timeout);
    for source in sources {
        command = command.arg(source);
    }
    run_build_command(
        input.session,
        input.stage_index,
        input.instruction_index,
        command,
    )
    .await
}

async fn run_build_command(
    session: &VmToolSession,
    stage: usize,
    instruction: usize,
    command: VmCommand,
) -> Result<(), ImageError> {
    let output = session.command(command).await?;
    info!(
        target: "nanocodex_vm",
        build_stage = stage,
        build_instruction = instruction,
        process.exit_code = output.exit_code,
        process.stdout.bytes = output.stdout.len(),
        process.stderr.bytes = output.stderr.len(),
        "VM image build instruction completed"
    );
    if output.exit_code == 0 {
        return Ok(());
    }
    let stdout = output_tail(&output.stdout);
    let stderr = output_tail(&output.stderr);
    Err(ImageError::BuildStep {
        stage,
        instruction,
        exit_code: output.exit_code,
        stdout,
        stderr,
    })
}

fn output_tail(output: &[u8]) -> String {
    const MAXIMUM_CHARS: usize = 8_192;

    let output = String::from_utf8_lossy(output);
    let skip = output.chars().count().saturating_sub(MAXIMUM_CHARS);
    output.chars().skip(skip).collect()
}

fn build_environment(
    environment: &BTreeMap<String, String>,
    arguments: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut result = environment.clone();
    result.extend(arguments.clone());
    result.into_iter().collect()
}

fn docker_process_environment(
    image_environment: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut environment = image_environment.clone();
    environment
        .entry("HOME".to_owned())
        .or_insert_with(|| "/root".to_owned());
    environment
        .entry("PATH".to_owned())
        .or_insert_with(|| DEFAULT_GUEST_PATH.to_owned());
    environment
}

fn resolve_stage_index(recipe: &DockerfileRecipe, reference: &str) -> Option<usize> {
    reference.parse::<usize>().ok().or_else(|| {
        recipe
            .stages
            .iter()
            .position(|stage| stage.name.as_deref() == Some(reference))
    })
}

fn guest_block_device(index: usize) -> Result<String, ImageError> {
    let suffix = u8::try_from(index)
        .ok()
        .and_then(|index| b'a'.checked_add(index))
        .filter(u8::is_ascii_lowercase)
        .ok_or_else(|| {
            ImageError::UnsupportedDockerfile("too many build source disks".to_owned())
        })?;
    Ok(format!("/dev/vd{}", char::from(suffix)))
}

fn final_recipe_environment(
    recipe: &DockerfileRecipe,
    images: &BTreeMap<String, PulledImage>,
) -> Result<BTreeMap<String, String>, ImageError> {
    let stage = recipe.final_stage().ok_or(ImageError::InvalidFrom)?;
    let image = images
        .get(&stage.base_image)
        .ok_or_else(|| ImageError::UnknownCopySource(stage.base_image.clone()))?;
    let mut environment = docker_process_environment(&image.config.environment);
    let mut arguments = BTreeMap::new();
    for instruction in &stage.instructions {
        match instruction {
            DockerfileInstruction::Env { name, value } => {
                environment.insert(
                    name.clone(),
                    expand_variables(value, &environment, &arguments),
                );
            }
            DockerfileInstruction::Arg { name, default } => {
                arguments.insert(
                    name.clone(),
                    default
                        .as_deref()
                        .map(|value| expand_variables(value, &environment, &arguments))
                        .unwrap_or_default(),
                );
            }
            DockerfileInstruction::Run(_)
            | DockerfileInstruction::Copy(_)
            | DockerfileInstruction::Workdir(_)
            | DockerfileInstruction::Cmd(_) => {}
        }
    }
    Ok(environment)
}

fn expand_variables(
    input: &str,
    environment: &BTreeMap<String, String>,
    arguments: &BTreeMap<String, String>,
) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let Some(character) = input[index..].chars().next() else {
                break;
            };
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if bytes.get(index + 1) == Some(&b'{') {
            let Some(end) = bytes[index + 2..].iter().position(|byte| *byte == b'}') else {
                output.push('$');
                index += 1;
                continue;
            };
            let end = index + 2 + end;
            let name = &input[index + 2..end];
            output.push_str(
                arguments
                    .get(name)
                    .or_else(|| environment.get(name))
                    .map_or("", String::as_str),
            );
            index = end + 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric()) {
            end += 1;
        }
        if end == start {
            output.push('$');
            index += 1;
            continue;
        }
        let name = &input[start..end];
        output.push_str(
            arguments
                .get(name)
                .or_else(|| environment.get(name))
                .map_or("", String::as_str),
        );
        index = end;
    }
    output
}

fn build_cache_key(
    dockerfile: &str,
    context_digest: &str,
    recipe: &DockerfileRecipe,
    images: &BTreeMap<String, PulledImage>,
    disk_bytes: u64,
    builder: &VmImageBuilder,
    inputs: &BuildCacheInputs,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nanocodex-vm-image-build\0");
    hasher.update(IMAGE_BUILD_CACHE_VERSION.to_le_bytes());
    hasher.update(b"linux\0");
    hasher.update(GUEST_ARCHITECTURE.as_bytes());
    hasher.update([0]);
    hasher.update(disk_bytes.to_le_bytes());
    hasher.update([builder.cpus]);
    hasher.update(builder.memory_mib.to_le_bytes());
    hasher.update([u8::from(builder.prefer_ipv4)]);
    for argument in &builder.vmm_arguments {
        hasher.update([0]);
        hasher.update(argument.as_os_str().as_bytes());
    }
    for value in [
        &inputs.vmm_identity,
        &inputs.runtime_digest,
        &inputs.firmware_digest,
        &inputs.network,
        &inputs.egress_scope,
    ] {
        hasher.update([0]);
        hasher.update(value.as_bytes());
    }
    hasher.update([0]);
    match &inputs.resolver {
        Some(resolver) => {
            hasher.update(b"resolver\0");
            hasher.update(resolver.as_bytes());
        }
        None => hasher.update(b"no-resolver"),
    }
    hasher.update([0]);
    hasher.update(context_digest.as_bytes());
    hasher.update([0]);
    hasher.update(dockerfile.as_bytes());
    for stage in &recipe.stages {
        hasher.update([0]);
        hasher.update(stage.base_image.as_bytes());
        if let Some(image) = images.get(&stage.base_image) {
            hasher.update([0]);
            hasher.update(image.manifest_digest.as_bytes());
        }
    }
    for (reference, image) in images {
        hasher.update([0]);
        hasher.update(reference.as_bytes());
        hasher.update([0]);
        hasher.update(image.manifest_digest.as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub(crate) fn host_resolver_configuration() -> io::Result<String> {
    for path in ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"] {
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let configuration = resolver_configuration(&contents);
        if !configuration.is_empty() {
            return Ok(configuration);
        }
    }
    Err(io::Error::other("host resolver has no usable nameserver"))
}

fn resolver_configuration(contents: &str) -> String {
    let mut configuration = String::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("nameserver") {
            continue;
        }
        let Some(address) = fields.next() else {
            continue;
        };
        let Ok(address) = address.parse::<std::net::IpAddr>() else {
            continue;
        };
        if fields.next().is_some() || address.is_loopback() || address.is_unspecified() {
            continue;
        }
        configuration.push_str("nameserver ");
        configuration.push_str(&address.to_string());
        configuration.push_str("\\n");
    }
    configuration
}

#[derive(Clone)]
struct PulledLayer {
    digest: String,
    path: PathBuf,
    media_type: String,
}

#[derive(Clone)]
struct PulledImage {
    manifest_digest: String,
    layers: Vec<PulledLayer>,
    source: ManifestSource,
    config: ImageRuntimeConfig,
}

async fn resolve_image(
    image_reference: &str,
    cache: &Path,
    policy: CachePolicy,
) -> Result<PulledImage, ImageError> {
    let reference_key = reference_cache_key(image_reference);
    let reference_path = cache
        .join("references")
        .join(format!("{reference_key}.json"));
    if policy == CachePolicy::Reuse
        && let Some(record) = read_cache_record::<ReferenceRecord>(&reference_path)?
        && let Some(image) = local_image(image_reference, cache, record).await?
    {
        return Ok(image);
    }
    let _lock = acquire_cache_lock(cache, "references", &reference_key).await?;
    if policy == CachePolicy::Reuse
        && let Some(record) = read_cache_record::<ReferenceRecord>(&reference_path)?
        && let Some(image) = local_image(image_reference, cache, record).await?
    {
        return Ok(image);
    }

    let image = pull_layers(image_reference, &cache.join("blobs")).await?;
    let record = ReferenceRecord {
        version: CACHE_RECORD_VERSION,
        image_reference: image_reference.to_owned(),
        manifest_digest: image.manifest_digest.clone(),
        layers: image
            .layers
            .iter()
            .map(|layer| LayerRecord {
                digest: layer.digest.clone(),
                media_type: layer.media_type.clone(),
            })
            .collect(),
        config: image.config.clone(),
    };
    write_cache_record(&reference_path, &record)?;
    Ok(image)
}

async fn local_image(
    image: &str,
    cache: &Path,
    record: ReferenceRecord,
) -> Result<Option<PulledImage>, ImageError> {
    if record.version != CACHE_RECORD_VERSION
        || record.image_reference != image
        || !valid_digest(&record.manifest_digest)
        || record.layers.is_empty()
        || record
            .layers
            .iter()
            .any(|layer| !valid_digest(&layer.digest) || layer.media_type.is_empty())
    {
        return Ok(None);
    }
    let layers = record
        .layers
        .into_iter()
        .map(|layer| PulledLayer {
            path: blob_path(&cache.join("blobs"), &layer.digest),
            digest: layer.digest,
            media_type: layer.media_type,
        })
        .collect::<Vec<_>>();
    for layer in &layers {
        if !valid_cached_blob(&layer.path, &layer.digest).await? {
            return Ok(None);
        }
    }
    Ok(Some(PulledImage {
        manifest_digest: record.manifest_digest,
        layers,
        source: ManifestSource::Local,
        config: record.config,
    }))
}

async fn pull_layers(image: &str, blobs: &Path) -> Result<PulledImage, ImageError> {
    fs::create_dir_all(blobs)?;
    let reference = Reference::try_from(image).map_err(|source| ImageError::Reference {
        image: image.to_owned(),
        source,
    })?;
    let config = ClientConfig {
        platform_resolver: Some(Box::new(linux_guest_manifest)),
        ..ClientConfig::default()
    };
    let client = Client::new(config);
    let (manifest, manifest_digest, config_json) = client
        .pull_manifest_and_config(&reference, &RegistryAuth::Anonymous)
        .await?;
    let config = parse_image_config(&config_json)?;
    let layers = stream::iter(manifest.layers.into_iter().map(|descriptor| {
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.image.blob",
            otel.kind = "client",
            image.reference = image,
            image.layer.digest = descriptor.digest.as_str(),
            image.layer.media_type = descriptor.media_type.as_str(),
        );
        let client = &client;
        let reference = &reference;
        async move {
            let path = blob_path(blobs, &descriptor.digest);
            if !valid_cached_blob(&path, &descriptor.digest).await? {
                let lock_key = reference_cache_key(&descriptor.digest);
                let _lock = acquire_cache_lock(blobs, "blobs", &lock_key).await?;
                if !valid_cached_blob(&path, &descriptor.digest).await? {
                    let temporary = temporary_path(blobs, &format!(".{lock_key}."))?;
                    let mut output = tokio::fs::File::create(&temporary).await?;
                    client
                        .pull_blob(reference, &descriptor, &mut output)
                        .await?;
                    output.sync_all().await?;
                    drop(output);
                    publish(temporary, &path)?;
                    if !valid_cached_blob(&path, &descriptor.digest).await? {
                        return Err(ImageError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "downloaded OCI blob does not match digest {}",
                                descriptor.digest
                            ),
                        )));
                    }
                }
            }
            Ok::<_, ImageError>(PulledLayer {
                digest: descriptor.digest,
                path,
                media_type: descriptor.media_type,
            })
        }
        .instrument(span)
    }))
    .buffered(MAX_CONCURRENT_LAYER_DOWNLOADS)
    .try_collect::<Vec<_>>()
    .await?;
    Ok(PulledImage {
        manifest_digest,
        layers,
        source: ManifestSource::Registry,
        config,
    })
}

fn parse_image_config(config: &str) -> Result<ImageRuntimeConfig, ImageError> {
    let config = serde_json::from_str::<ConfigFile>(config)?
        .config
        .unwrap_or_default();
    let mut environment = BTreeMap::new();
    for entry in config.env.unwrap_or_default() {
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if valid_environment_name(name) {
            environment.insert(name.to_owned(), value.to_owned());
        }
    }
    let working_directory = config
        .working_dir
        .filter(|directory| valid_guest_workdir(directory))
        .unwrap_or_else(|| "/".to_owned());
    Ok(ImageRuntimeConfig {
        environment,
        working_directory,
    })
}

fn blob_path(cache: &Path, digest: &str) -> PathBuf {
    cache.join(digest.replace(':', "-"))
}

async fn valid_cached_blob(path: &Path, digest: &str) -> Result<bool, ImageError> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    let record_path = path.with_extension("blob.json");
    if let Some(record) = read_cache_record::<BlobRecord>(&record_path)?
        && record.version == BLOB_RECORD_VERSION
        && record.digest == digest
        && record.file == cache_file_identity(&metadata)?
        && metadata.permissions().mode() & 0o222 == 0
    {
        return Ok(true);
    }

    let Some(expected) = digest.strip_prefix("sha256:") else {
        return Ok(false);
    };
    let before = cache_file_identity(&metadata)?;
    let path_for_hash = path.to_path_buf();
    let actual = tokio::task::spawn_blocking(move || sha256_file(&path_for_hash)).await??;
    let after = fs::symlink_metadata(path)?;
    if !after.file_type().is_file() || cache_file_identity(&after)? != before || actual != expected
    {
        return Ok(false);
    }
    mark_cache_file_read_only(path)?;
    let metadata = fs::symlink_metadata(path)?;
    write_cache_record(
        &record_path,
        &BlobRecord {
            version: BLOB_RECORD_VERSION,
            digest: digest.to_owned(),
            file: cache_file_identity(&metadata)?,
        },
    )?;
    Ok(true)
}

fn valid_digest(digest: &str) -> bool {
    let Some(hash) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn linux_guest_manifest(manifests: &[ImageIndexEntry]) -> Option<String> {
    manifests
        .iter()
        .find(|entry| {
            entry.platform.as_ref().is_some_and(|platform| {
                platform.os.to_string() == "linux"
                    && platform.architecture.to_string() == GUEST_ARCHITECTURE
            })
        })
        .map(|entry| entry.digest.clone())
}

fn format_root_disk(path: &Path, size: u64, layers: &[PulledLayer]) -> Result<(), ImageError> {
    let mut formatter = Formatter::new(path, BLOCK_SIZE, size)?;
    for layer in layers {
        let file = File::open(&layer.path)?;
        match layer.media_type.as_str() {
            "application/vnd.docker.image.rootfs.diff.tar.gzip"
            | "application/vnd.oci.image.layer.v1.tar+gzip"
            | "application/vnd.oci.image.layer.nondistributable.v1.tar+gzip" => {
                formatter.unpack_tar(GzDecoder::new(BufReader::new(file)))?;
            }
            "application/vnd.oci.image.layer.v1.tar+zstd" => {
                formatter.unpack_tar(zstd::stream::read::Decoder::new(BufReader::new(file))?)?;
            }
            "application/vnd.docker.image.rootfs.diff.tar"
            | "application/vnd.oci.image.layer.v1.tar"
            | "application/vnd.oci.image.layer.nondistributable.v1.tar" => {
                formatter.unpack_tar(BufReader::new(file))?;
            }
            media_type => return Err(ImageError::UnsupportedLayer(media_type.to_owned())),
        }
    }
    formatter.close()?;
    Ok(())
}

fn validate_root_disk(path: &Path) -> Result<(), ImageError> {
    validate_ext4_disk(path)?;
    let mut reader = Reader::new(path)?;
    for required in ["/bin/sh"] {
        if !reader.exists(required) {
            return Err(ImageError::MissingPreparedPath(required));
        }
    }
    Ok(())
}

fn cached_root_disk(path: &Path) -> Option<String> {
    match validated_prepared_disk_record(path) {
        Ok(shell) => Some(shell),
        Err(error) => {
            info!(
                target: "nanocodex_vm",
                image_root_path = %path.display(),
                error = %error,
                "rebuilding invalid VM image cache disk"
            );
            None
        }
    }
}

fn valid_cached_ext4_disk(path: &Path) -> Result<bool, ImageError> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    let record_path = path.with_extension("ext4.json");
    let Some(record) = read_cache_record::<CachedExt4Record>(&record_path)? else {
        return Ok(false);
    };
    Ok(record.version == CACHED_EXT4_RECORD_VERSION
        && record.file == cache_file_identity(&metadata)?
        && metadata.permissions().mode() & 0o222 == 0
        && validate_ext4_disk(path).is_ok())
}

fn validated_prepared_disk_record(path: &Path) -> Result<String, ImageError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(ImageError::Io(io::Error::other(
            "prepared disk cache path is not a regular file",
        )));
    }
    let record_path = path.with_extension("prepared.json");
    let Some(record) = read_cache_record::<PreparedDiskRecord>(&record_path)? else {
        return Err(ImageError::Io(io::Error::other(
            "prepared disk cache record is missing",
        )));
    };
    if record.version != PREPARED_DISK_RECORD_VERSION
        || record.file != cache_file_identity(&metadata)?
        || !matches!(record.shell.as_str(), "bash" | "sh")
        || metadata.permissions().mode() & 0o222 != 0
    {
        return Err(ImageError::Io(io::Error::other(
            "prepared disk cache record does not match the disk",
        )));
    }
    validate_root_disk(path)?;
    prepared_shell(path).map(str::to_owned)
}

fn record_prepared_disk(path: &Path) -> Result<String, ImageError> {
    validate_root_disk(path)?;
    mark_cache_file_read_only(path)?;
    let metadata = fs::symlink_metadata(path)?;
    let file = cache_file_identity(&metadata)?;
    let record_path = path.with_extension("prepared.json");
    let shell = prepared_shell(path)?.to_owned();
    write_cache_record(
        &path.with_extension("ext4.json"),
        &CachedExt4Record {
            version: CACHED_EXT4_RECORD_VERSION,
            file: file.clone(),
        },
    )?;
    write_cache_record(
        &record_path,
        &PreparedDiskRecord {
            version: PREPARED_DISK_RECORD_VERSION,
            file,
            shell: shell.clone(),
        },
    )?;
    Ok(shell)
}

fn record_cached_ext4_disk(path: &Path) -> Result<(), ImageError> {
    validate_ext4_disk(path)?;
    mark_cache_file_read_only(path)?;
    let metadata = fs::symlink_metadata(path)?;
    write_cache_record(
        &path.with_extension("ext4.json"),
        &CachedExt4Record {
            version: CACHED_EXT4_RECORD_VERSION,
            file: cache_file_identity(&metadata)?,
        },
    )
}

fn mark_cache_file_read_only(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::other("cache path is not a regular file"));
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() & !0o222);
    fs::set_permissions(path, permissions)
}

fn cache_file_identity(metadata: &fs::Metadata) -> io::Result<CacheFileIdentity> {
    let modified_nanos = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(io::Error::other)?;
    Ok(CacheFileIdentity {
        bytes: metadata.len(),
        modified_nanos,
        changed_seconds: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

async fn cached_file_digest(
    path: &Path,
    cache: &Arc<AsyncMutex<Option<CachedFileDigest>>>,
) -> Result<String, ImageError> {
    let mut cached = cache.lock().await;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::other(format!(
            "build runtime input is not a regular file: {}",
            path.display()
        ))
        .into());
    }
    let before = cache_file_identity(&metadata)?;
    if let Some(entry) = cached.as_ref()
        && entry.file == before
    {
        return Ok(entry.digest.clone());
    }
    let path = path.to_path_buf();
    let hash_path = path.clone();
    let digest = tokio::task::spawn_blocking(move || sha256_file(&hash_path)).await??;
    let after = cache_file_identity(&fs::symlink_metadata(&path)?)?;
    if before != after {
        return Err(io::Error::other(format!(
            "build runtime input changed while it was being hashed: {}",
            path.display()
        ))
        .into());
    }
    *cached = Some(CachedFileDigest {
        file: after,
        digest: digest.clone(),
    });
    Ok(digest)
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn prepared_shell(path: &Path) -> Result<&'static str, ImageError> {
    let mut reader = Reader::new(path)?;
    let configured = reader
        .read_file("/etc/passwd", 0, None)
        .ok()
        .and_then(|passwd| configured_root_shell(&passwd));
    if let Some((shell, path)) = configured
        && reader.exists(&path)
    {
        return Ok(shell);
    }
    if reader.exists("/bin/bash") || reader.exists("/usr/bin/bash") {
        Ok("bash")
    } else if reader.exists("/bin/sh") {
        Ok("sh")
    } else {
        Err(ImageError::MissingPreparedPath("/bin/sh"))
    }
}

fn configured_root_shell(passwd: &[u8]) -> Option<(&'static str, String)> {
    passwd.split(|byte| *byte == b'\n').find_map(|entry| {
        let mut fields = entry.split(|byte| *byte == b':');
        fields.next()?;
        fields.next()?;
        if fields.next()? != b"0" {
            return None;
        }
        fields.next()?;
        fields.next()?;
        fields.next()?;
        let path = std::str::from_utf8(fields.next()?).ok()?;
        let shell = match Path::new(path).file_name()?.to_str()? {
            "bash" => "bash",
            "sh" => "sh",
            _ => return None,
        };
        Some((shell, path.to_owned()))
    })
}

fn validate_ext4_disk(path: &Path) -> Result<(), ImageError> {
    let mut reader = Reader::new(path)?;
    if !reader.exists("/") {
        return Err(ImageError::MissingPreparedPath("/"));
    }
    Ok(())
}

fn disk_cache_key(manifest_digest: &str, disk_bytes: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nanocodex-vm-ext4-v3\0linux\0");
    hasher.update(GUEST_ARCHITECTURE.as_bytes());
    hasher.update([0]);
    hasher.update(manifest_digest.as_bytes());
    hasher.update([0]);
    hasher.update(disk_bytes.to_le_bytes());
    hex::encode(hasher.finalize())
}

fn reference_cache_key(image: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nanocodex-vm-reference-v1\0linux\0");
    hasher.update(GUEST_ARCHITECTURE.as_bytes());
    hasher.update([0]);
    hasher.update(image.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        ffi::OsStr,
        fs::{self, File},
        path::{Path, PathBuf},
        process::Command,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use crate::{config::Network, egress::EgressLease};

    use super::{
        BuildCacheInputs, CACHE_RECORD_VERSION, CONTEXT_DISK_BYTES, COPY_SCRIPT, CachePolicy,
        DiskStatus, DockerfileRecipe, FIRMWARE_LIBRARY_FILENAME, FIRMWARE_LIBRARY_PATH_ENVIRONMENT,
        ImageError, ImageRuntimeConfig, LayerRecord, ManifestSource, PulledImage, PulledLayer,
        Reader, ReferenceRecord, VmImageBuilder, append_normalized_context_entry, blob_path,
        build_cache_key, build_guest_bootstrap_script, cached_file_digest,
        configure_firmware_library_path, configured_root_shell, disk_cache_key,
        docker_process_environment, output_tail, prepare_copy_source_disk, prepare_flattened_disk,
        reference_cache_key, resolver_configuration, valid_cached_blob, valid_cached_ext4_disk,
        write_cache_record,
    };
    use flate2::{Compression, write::GzEncoder};
    use tracing::{
        Event, Id, Instrument, Subscriber,
        field::Visit,
        span::{Attributes, Record},
    };
    use tracing_subscriber::{
        Layer, layer::Context as LayerContext, prelude::*, registry::LookupSpan,
    };

    const FIXTURE_IMAGE: &str = "example.invalid/nanocodex-vm-fixture:latest";
    const FIXTURE_MANIFEST: &str =
        "sha256:56249d7a2f93306106f6d8bcdf6423afb73c1b747d874febcc778beee25cb8bb";
    static IMAGE_PREPARE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn configured_root_shell_uses_the_uid_zero_account() {
        let passwd = b"service:x:1000:1000::/srv:/bin/bash\n\
                       root:x:0:0:root:/root:/bin/sh\n";

        assert_eq!(
            configured_root_shell(passwd),
            Some(("sh", "/bin/sh".to_owned()))
        );
    }

    #[test]
    fn configured_root_shell_ignores_unsupported_shells() {
        let passwd = b"root:x:0:0:root:/root:/bin/ash\n";

        assert_eq!(configured_root_shell(passwd), None);
    }

    #[cfg(unix)]
    #[test]
    fn build_context_headers_preserve_modes_and_normalize_host_metadata() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("run");
        fs::write(&source, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).unwrap();
        let mut encoded = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut encoded);
            append_normalized_context_entry(&mut archive, &source, Path::new("context/run"))
                .unwrap();
            archive.finish().unwrap();
        }

        let mut archive = tar::Archive::new(std::io::Cursor::new(encoded));
        let entry = archive.entries().unwrap().next().unwrap().unwrap();
        let header = entry.header();
        assert_eq!(header.mode().unwrap(), 0o751);
        assert_eq!(header.uid().unwrap(), 0);
        assert_eq!(header.gid().unwrap(), 0);
        assert_eq!(header.mtime().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn build_context_rejects_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("escape");
        std::os::unix::fs::symlink("/etc/passwd", &source).unwrap();
        let mut encoded = Vec::new();
        let mut archive = tar::Builder::new(&mut encoded);

        let error =
            append_normalized_context_entry(&mut archive, &source, Path::new("context/escape"))
                .unwrap_err();

        assert!(error.to_string().contains("symlinks are unsupported"));
    }

    struct LocalImageFixture {
        root: tempfile::TempDir,
        context: PathBuf,
        cache: PathBuf,
    }

    #[derive(Clone, Default)]
    struct TraceCapture {
        spans: Arc<Mutex<HashMap<u64, CapturedSpan>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    struct CapturedSpan {
        name: &'static str,
        parent: Option<u64>,
        fields: HashMap<String, String>,
    }

    struct CapturedEvent {
        parent: Option<u64>,
        fields: HashMap<String, String>,
    }

    struct FieldCapture<'a>(&'a mut HashMap<String, String>);

    impl Visit for FieldCapture<'_> {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    impl<S> Layer<S> for TraceCapture
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: LayerContext<'_, S>) {
            let parent = attributes
                .parent()
                .map(|parent| parent.clone().into_u64())
                .or_else(|| {
                    attributes
                        .is_contextual()
                        .then(|| context.current_span().id().map(Id::into_u64))
                        .flatten()
                });
            let mut fields = HashMap::new();
            attributes.record(&mut FieldCapture(&mut fields));
            self.spans.lock().unwrap().insert(
                id.clone().into_u64(),
                CapturedSpan {
                    name: attributes.metadata().name(),
                    parent,
                    fields,
                },
            );
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, _context: LayerContext<'_, S>) {
            if let Some(span) = self.spans.lock().unwrap().get_mut(&id.clone().into_u64()) {
                values.record(&mut FieldCapture(&mut span.fields));
            }
        }

        fn on_event(&self, event: &Event<'_>, context: LayerContext<'_, S>) {
            let parent = event
                .parent()
                .map(|parent| parent.clone().into_u64())
                .or_else(|| context.current_span().id().map(Id::into_u64));
            let mut fields = HashMap::new();
            event.record(&mut FieldCapture(&mut fields));
            self.events
                .lock()
                .unwrap()
                .push(CapturedEvent { parent, fields });
        }
    }

    impl LocalImageFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let context = root.path().join("context");
            let cache = root.path().join("cache");
            fs::create_dir_all(&context).unwrap();
            fs::write(
                context.join("Dockerfile"),
                format!(
                    "FROM {FIXTURE_IMAGE}\nENV NANOCODEX_FIXTURE_MODE=flattened\nWORKDIR /workspace\n"
                ),
            )
            .unwrap();

            let blobs = cache.join("blobs");
            fs::create_dir_all(&blobs).unwrap();
            let temporary_layer = blobs.join("fixture-layer");
            write_shell_layer(&temporary_layer);
            let layer_digest = format!("sha256:{}", super::sha256_file(&temporary_layer).unwrap());
            fs::rename(&temporary_layer, blob_path(&blobs, &layer_digest)).unwrap();
            let record = ReferenceRecord {
                version: CACHE_RECORD_VERSION,
                image_reference: FIXTURE_IMAGE.to_owned(),
                manifest_digest: FIXTURE_MANIFEST.to_owned(),
                layers: vec![LayerRecord {
                    digest: layer_digest,
                    media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_owned(),
                }],
                config: ImageRuntimeConfig::default(),
            };
            write_cache_record(
                &cache
                    .join("references")
                    .join(format!("{}.json", reference_cache_key(FIXTURE_IMAGE))),
                &record,
            )
            .unwrap();
            Self {
                root,
                context,
                cache,
            }
        }

        fn builder(&self) -> VmImageBuilder {
            VmImageBuilder::new(
                self.root.path().join("unused-vmm"),
                self.root.path().join("unused-runtime.ext4"),
            )
        }
    }

    fn write_shell_layer(path: &Path) {
        let output = File::create(path).unwrap();
        let encoder = GzEncoder::new(output, Compression::fast());
        let mut archive = tar::Builder::new(encoder);

        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_mode(0o755);
        directory.set_size(0);
        directory.set_cksum();
        archive
            .append_data(&mut directory, "bin/", std::io::empty())
            .unwrap();

        let contents = b"#!/bin/sh\n";
        let mut shell = tar::Header::new_gnu();
        shell.set_entry_type(tar::EntryType::Regular);
        shell.set_mode(0o755);
        shell.set_size(contents.len() as u64);
        shell.set_cksum();
        archive
            .append_data(&mut shell, "bin/sh", contents.as_slice())
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        drop(encoder.finish().unwrap());
    }

    fn write_copy_source_layer(path: &Path) {
        let output = File::create(path).unwrap();
        let encoder = GzEncoder::new(output, Compression::fast());
        let mut archive = tar::Builder::new(encoder);

        let contents = b"uv fixture\n";
        let mut executable = tar::Header::new_gnu();
        executable.set_entry_type(tar::EntryType::Regular);
        executable.set_mode(0o755);
        executable.set_size(contents.len() as u64);
        executable.set_cksum();
        archive
            .append_data(&mut executable, "uv", contents.as_slice())
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        drop(encoder.finish().unwrap());
    }

    #[test]
    fn accepts_the_first_proof_dockerfile() {
        let recipe =
            DockerfileRecipe::parse("FROM python:3.13-slim-bookworm\nWORKDIR /app\n").unwrap();

        assert_eq!(
            recipe.final_stage().unwrap().base_image,
            "python:3.13-slim-bookworm"
        );
        assert_eq!(recipe.final_workdir(), Some("/app"));
        assert!(!recipe.requires_build());
    }

    #[test]
    fn builder_retains_explicit_vm_execution_policy() {
        let builder = VmImageBuilder::new("/opt/nanocodex-vmm", "/cache/runtime.ext4")
            .firmware_directory("/opt/libkrunfw")
            .vmm_args(["run", "--private-config"])
            .vmm_build_cache_identity("vm-process-v3")
            .cpus(6)
            .memory_mib(8_192)
            .run_timeout(Duration::from_mins(15))
            .copy_timeout(Duration::from_mins(2))
            .egress(EgressLease::disabled());

        assert_eq!(builder.vmm, Path::new("/opt/nanocodex-vmm"));
        assert_eq!(builder.runtime_image, Path::new("/cache/runtime.ext4"));
        assert_eq!(
            builder.firmware_directory.as_deref(),
            Some(Path::new("/opt/libkrunfw"))
        );
        assert_eq!(builder.vmm_arguments, ["run", "--private-config"]);
        assert_eq!(
            builder.vmm_build_cache_identity.as_deref(),
            Some("vm-process-v3")
        );
        assert_eq!(builder.cpus, 6);
        assert_eq!(builder.memory_mib, 8_192);
        assert_eq!(builder.run_timeout, Duration::from_mins(15));
        assert_eq!(builder.copy_timeout, Duration::from_mins(2));
        assert_eq!(builder.egress.network(), &Network::Disabled);
    }

    #[test]
    fn caller_owned_vmm_identity_survives_unrelated_executable_changes() {
        let directory = tempfile::tempdir().unwrap();
        let vmm = directory.path().join("vmm");
        let runtime_image = directory.path().join("runtime.ext4");
        fs::write(&vmm, b"first evaluator build").unwrap();
        fs::write(&runtime_image, b"stable guest runtime").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let default_inputs = || {
            let builder = VmImageBuilder::new(&vmm, &runtime_image).egress(EgressLease::disabled());
            runtime.block_on(builder.build_cache_inputs()).unwrap()
        };
        let inputs = |identity| {
            let builder = VmImageBuilder::new(&vmm, &runtime_image)
                .vmm_build_cache_identity(identity)
                .egress(EgressLease::disabled());
            runtime.block_on(builder.build_cache_inputs()).unwrap()
        };
        let default_first = default_inputs();
        let first = inputs("vm-process-v1");
        fs::write(&vmm, b"second evaluator build").unwrap();
        let default_second = default_inputs();
        let second = inputs("vm-process-v1");
        let bumped = inputs("vm-process-v2");

        assert_ne!(default_first.vmm_identity, default_second.vmm_identity);
        assert_eq!(first.vmm_identity, "caller-owned:vm-process-v1");
        assert_eq!(first.vmm_identity, second.vmm_identity);
        assert_ne!(first.vmm_identity, bumped.vmm_identity);
        assert_eq!(first.runtime_digest, second.runtime_digest);
        assert_eq!(first.firmware_digest, second.firmware_digest);
    }

    #[test]
    fn caller_owned_vmm_identity_rejects_invalid_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let vmm = directory.path().join("vmm");
        let runtime_image = directory.path().join("runtime.ext4");
        fs::write(&vmm, b"vmm").unwrap();
        fs::write(&runtime_image, b"runtime").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        for identity in [
            String::new(),
            "x".repeat(super::MAX_VMM_BUILD_CACHE_IDENTITY_BYTES + 1),
        ] {
            let builder = VmImageBuilder::new(&vmm, &runtime_image)
                .vmm_build_cache_identity(identity)
                .egress(EgressLease::disabled());
            assert!(matches!(
                runtime.block_on(builder.build_cache_inputs()),
                Err(ImageError::InvalidVmmBuildCacheIdentity)
            ));
        }
    }

    #[test]
    fn build_address_family_preference_is_explicit_and_ephemeral() {
        let builder = VmImageBuilder::new("/opt/nanocodex-vmm", "/cache/runtime.ext4");
        assert!(!builder.prefer_ipv4);
        let builder = builder.prefer_ipv4();
        assert!(builder.prefer_ipv4);

        let resolver = "nameserver 213.186.33.99\\n";
        let dual_stack = build_guest_bootstrap_script(Some(resolver), false);
        assert!(!dual_stack.contains("/proc/sys/net/ipv6"));

        let prefer_ipv4 = build_guest_bootstrap_script(Some(resolver), true);
        let all = prefer_ipv4
            .find("/proc/sys/net/ipv6/conf/all/disable_ipv6")
            .unwrap();
        let default = prefer_ipv4
            .find("/proc/sys/net/ipv6/conf/default/disable_ipv6")
            .unwrap();
        let resolver = prefer_ipv4.find("/etc/resolv.conf").unwrap();
        let runtime = prefer_ipv4
            .find("exec /run/nanocodex/nanocodex-vm-guest")
            .unwrap();
        assert!(all < default);
        assert!(default < resolver);
        assert!(resolver < runtime);
        assert!(!prefer_ipv4.contains("/etc/sysctl"));
        assert!(prefer_ipv4.contains(
            "nanocodex image build bootstrap: failed to write /proc/sys/net/ipv6/conf/all/disable_ipv6"
        ));
        assert!(prefer_ipv4.contains(
            "nanocodex image build bootstrap: failed to write /proc/sys/net/ipv6/conf/default/disable_ipv6"
        ));
        assert!(prefer_ipv4.contains("mv /etc/resolv.conf /run/nanocodex-build-resolver/original"));
        assert!(
            super::RESTORE_BUILD_RESOLVER_SCRIPT
                .contains("mv /run/nanocodex-build-resolver/original /etc/resolv.conf")
        );
        let offline = build_guest_bootstrap_script(None, false);
        assert!(!offline.contains("resolv.conf"));
    }

    #[test]
    fn firmware_directory_sets_the_platform_library_path() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(FIRMWARE_LIBRARY_FILENAME),
            b"firmware",
        )
        .unwrap();
        let mut command = tokio::process::Command::new("/opt/nanocodex-vmm");

        configure_firmware_library_path(&mut command, directory.path()).unwrap();

        let configured = command
            .as_std()
            .get_envs()
            .find(|(name, _)| *name == OsStr::new(FIRMWARE_LIBRARY_PATH_ENVIRONMENT))
            .and_then(|(_, value)| value)
            .map(OsStr::to_owned);
        assert_eq!(
            configured.as_deref(),
            Some(directory.path().canonicalize().unwrap().as_os_str())
        );
    }

    #[test]
    fn build_cache_hashes_a_versioned_firmware_symlink_target() {
        let directory = tempfile::tempdir().unwrap();
        let vmm = directory.path().join("vmm");
        let runtime_image = directory.path().join("runtime.ext4");
        let versioned_firmware = directory.path().join("libkrunfw-versioned");
        fs::write(&vmm, b"vmm").unwrap();
        fs::write(&runtime_image, b"runtime").unwrap();
        fs::write(&versioned_firmware, b"firmware").unwrap();
        std::os::unix::fs::symlink(
            versioned_firmware.file_name().unwrap(),
            directory.path().join(FIRMWARE_LIBRARY_FILENAME),
        )
        .unwrap();
        let builder = VmImageBuilder::new(&vmm, &runtime_image)
            .firmware_directory(directory.path())
            .egress(EgressLease::disabled());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let inputs = runtime
            .block_on(builder.build_cache_inputs())
            .expect("versioned firmware symlink should be a valid runtime input");

        assert_eq!(
            inputs.firmware_digest,
            super::sha256_file(&versioned_firmware).unwrap()
        );
    }

    #[test]
    fn supplies_docker_root_process_defaults() {
        let environment = docker_process_environment(&BTreeMap::new());

        assert_eq!(environment.get("HOME").map(String::as_str), Some("/root"));
        assert_eq!(
            environment.get("PATH").map(String::as_str),
            Some(super::DEFAULT_GUEST_PATH)
        );
    }

    #[test]
    fn docker_environment_removes_quotes_and_expands_in_order() {
        let recipe = DockerfileRecipe::parse(
            r#"FROM oven/bun:1.2.15-debian
ENV PATH="/opt/venv/bin:${PATH}" MODE='frontier bench' EMPTY=
ENV LEGACY value with spaces
"#,
        )
        .unwrap();
        let mut environment = BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]);
        let arguments = BTreeMap::new();
        for instruction in &recipe.stages[0].instructions {
            let super::DockerfileInstruction::Env { name, value } = instruction else {
                continue;
            };
            environment.insert(
                name.clone(),
                super::expand_variables(value, &environment, &arguments),
            );
        }

        assert_eq!(environment["PATH"], "/opt/venv/bin:/bin");
        assert_eq!(environment["MODE"], "frontier bench");
        assert_eq!(environment["EMPTY"], "");
        assert_eq!(environment["LEGACY"], "value with spaces");
    }

    #[test]
    fn docker_variable_expansion_preserves_utf8() {
        let environment = BTreeMap::from([("NAME".to_owned(), "世界".to_owned())]);
        let arguments = BTreeMap::new();

        assert_eq!(
            super::expand_variables("café/$NAME/🦀", &environment, &arguments),
            "café/世界/🦀"
        );
    }

    #[test]
    fn missing_oci_runtime_config_uses_the_container_root() {
        let config = ImageRuntimeConfig::default();

        assert_eq!(config.working_directory, "/");
        assert!(config.environment.is_empty());
    }

    #[test]
    fn build_cache_identity_covers_the_complete_execution_policy() {
        let dockerfile = "FROM example.invalid/base:latest\nRUN printf ready > /proof\n";
        let recipe = DockerfileRecipe::parse(dockerfile).unwrap();
        let images = BTreeMap::from([(
            "example.invalid/base:latest".to_owned(),
            PulledImage {
                manifest_digest: FIXTURE_MANIFEST.to_owned(),
                layers: Vec::new(),
                source: ManifestSource::Local,
                config: ImageRuntimeConfig::default(),
            },
        )]);
        let builder = VmImageBuilder::new("/vmm", "/runtime");
        let inputs = BuildCacheInputs {
            vmm_identity: "vmm-a".to_owned(),
            runtime_digest: "runtime-a".to_owned(),
            firmware_digest: "firmware-a".to_owned(),
            resolver: Some("nameserver 192.0.2.1\\n".to_owned()),
            network: "internet".to_owned(),
            egress_scope: "internet-a".to_owned(),
        };
        let key = |builder: &VmImageBuilder, inputs: &BuildCacheInputs| {
            build_cache_key(
                dockerfile,
                "context-a",
                &recipe,
                &images,
                1024,
                builder,
                inputs,
            )
        };
        let baseline = key(&builder, &inputs);

        for changed in [
            BuildCacheInputs {
                vmm_identity: "vmm-b".to_owned(),
                ..inputs.clone()
            },
            BuildCacheInputs {
                runtime_digest: "runtime-b".to_owned(),
                ..inputs.clone()
            },
            BuildCacheInputs {
                firmware_digest: "firmware-b".to_owned(),
                ..inputs.clone()
            },
            BuildCacheInputs {
                resolver: Some("nameserver 192.0.2.2\\n".to_owned()),
                ..inputs.clone()
            },
            BuildCacheInputs {
                resolver: None,
                ..inputs.clone()
            },
            BuildCacheInputs {
                network: "gvproxy".to_owned(),
                ..inputs.clone()
            },
            BuildCacheInputs {
                egress_scope: "internet-b".to_owned(),
                ..inputs.clone()
            },
        ] {
            assert_ne!(key(&builder, &changed), baseline);
        }
        assert_ne!(key(&builder.clone().cpus(3), &inputs), baseline);
        assert_ne!(key(&builder.clone().memory_mib(2048), &inputs), baseline);
        assert_ne!(key(&builder.clone().prefer_ipv4(), &inputs), baseline);
        assert_ne!(key(&builder.vmm_arg("--different"), &inputs), baseline);
    }

    #[test]
    fn cached_runtime_digest_is_invalidated_by_file_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runtime.ext4");
        fs::write(&path, b"first").unwrap();
        let cache = Arc::new(tokio::sync::Mutex::new(None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let first = cached_file_digest(&path, &cache).await.unwrap();
            assert_eq!(cached_file_digest(&path, &cache).await.unwrap(), first);
            fs::write(&path, b"second-and-different").unwrap();
            let second = cached_file_digest(&path, &cache).await.unwrap();
            assert_ne!(second, first);
        });
    }

    #[test]
    fn cached_oci_blob_detects_same_size_content_corruption() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sha256-fixture");
        fs::write(&path, b"correct").unwrap();
        let digest = format!("sha256:{}", super::sha256_file(&path).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            assert!(valid_cached_blob(&path, &digest).await.unwrap());
            let pristine_metadata = fs::metadata(&path).unwrap();
            assert_eq!(pristine_metadata.permissions().mode() & 0o222, 0);

            let mut permissions = pristine_metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o200);
            fs::set_permissions(&path, permissions).unwrap();
            fs::write(&path, b"corrupt").unwrap();
            filetime::set_file_mtime(
                &path,
                filetime::FileTime::from_last_modification_time(&pristine_metadata),
            )
            .unwrap();
            assert!(!valid_cached_blob(&path, &digest).await.unwrap());

            fs::write(&path, b"correct").unwrap();
            assert!(valid_cached_blob(&path, &digest).await.unwrap());
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o222, 0);
        });
    }

    #[test]
    fn resolver_configuration_rejects_host_local_stubs() {
        assert_eq!(
            resolver_configuration(
                "nameserver 127.0.0.53\nnameserver ::1\nnameserver 213.186.33.99\n"
            ),
            "nameserver 213.186.33.99\\n"
        );
    }

    #[test]
    fn copy_creates_a_missing_directory_for_a_trailing_slash_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("client.conf");
        let destination = format!("{}/", temporary.path().join("etc/pipewire").display());
        fs::write(&source, "configured").unwrap();

        let status = Command::new("/bin/sh")
            .args([
                "-c",
                COPY_SCRIPT,
                "nanocodex-vm-copy",
                &destination,
                source.to_str().unwrap(),
            ])
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(
            fs::read_to_string(temporary.path().join("etc/pipewire/client.conf")).unwrap(),
            "configured"
        );
    }

    #[test]
    fn parses_the_complete_terminal_bench_instruction_shape() {
        let recipe = DockerfileRecipe::parse(
            r#"FROM ubuntu:24.04 AS build
ARG SOURCE=https://example.com/input
ENV MODE=test
RUN apt-get update && \
    apt-get install -y curl
COPY input.txt /root/
FROM ubuntu:24.04 AS target
COPY --from=build /root/input.txt /app/input.txt
WORKDIR /app
CMD ["/bin/sh"]
"#,
        )
        .unwrap();

        assert_eq!(recipe.stages.len(), 2);
        assert_eq!(recipe.stages[0].name.as_deref(), Some("build"));
        assert_eq!(recipe.stages[1].name.as_deref(), Some("target"));
        assert_eq!(recipe.final_workdir(), Some("/app"));
        assert!(recipe.requires_build());
        let super::DockerfileInstruction::Copy(copy) = &recipe.stages[1].instructions[0] else {
            panic!("expected the final-stage COPY instruction");
        };
        assert_eq!(copy.from.as_deref(), Some("build"));
        assert_eq!(copy.sources, ["/root/input.txt"]);
        assert_eq!(copy.destination, "/app/input.txt");
    }

    #[test]
    fn rejects_workdir_parent_traversal() {
        let error =
            DockerfileRecipe::parse("FROM python:3.13-slim-bookworm\nWORKDIR /app/../root\n")
                .err()
                .unwrap();

        assert!(error.to_string().contains("WORKDIR /app/../root"));
    }

    #[test]
    fn flattened_disk_identity_is_task_recipe_independent() {
        let digest = "sha256:56249d7a2f93306106f6d8bcdf6423afb73c1b747d874febcc778beee25cb8bb";
        let first =
            DockerfileRecipe::parse("FROM python:3.13-slim-bookworm\nWORKDIR /app\n").unwrap();
        let second =
            DockerfileRecipe::parse("FROM python:3.13-slim-bookworm\nWORKDIR /workspace\n")
                .unwrap();

        assert_ne!(first.final_workdir(), second.final_workdir());
        assert_eq!(
            first.final_stage().unwrap().base_image,
            second.final_stage().unwrap().base_image
        );
        let first_key = disk_cache_key(digest, 1024);
        let second_key = disk_cache_key(digest, 1024);
        assert_eq!(first_key, second_key);
        assert_ne!(disk_cache_key(digest, 1024), disk_cache_key(digest, 2048));
        assert_ne!(
            reference_cache_key("python:3.13-slim-bookworm"),
            reference_cache_key("python:3.12-slim-bookworm")
        );
    }

    #[test]
    fn build_failure_diagnostics_keep_the_output_tail() {
        let mut output = vec![b'a'; 8_192];
        output.extend_from_slice(b"final compiler error");

        let retained = output_tail(&output);

        assert_eq!(retained.chars().count(), 8_192);
        assert!(retained.ends_with("final compiler error"));
    }

    #[test]
    fn copy_only_external_image_does_not_require_a_shell() {
        let _test_guard = IMAGE_PREPARE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let layer = root.path().join("copy-source.tar.gz");
        write_copy_source_layer(&layer);
        let image = PulledImage {
            manifest_digest: "sha256:copy-source".to_owned(),
            layers: vec![PulledLayer {
                digest: "sha256:copy-source-layer".to_owned(),
                path: layer,
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".to_owned(),
            }],
            source: ManifestSource::Local,
            config: ImageRuntimeConfig::default(),
        };
        let cache = root.path().join("cache");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let path = prepare_copy_source_disk(&cache, &image, CONTEXT_DISK_BYTES)
                .await
                .unwrap();
            let mut reader = Reader::new(&path).unwrap();
            assert!(reader.exists("/uv"));
            assert!(!reader.exists("/bin/sh"));

            let error = prepare_flattened_disk(&cache, &image, CONTEXT_DISK_BYTES)
                .await
                .unwrap_err();
            assert!(matches!(error, ImageError::MissingPreparedPath("/bin/sh")));
        });
    }

    #[test]
    fn concurrent_preparation_singleflights_the_same_immutable_disk() {
        let _test_guard = IMAGE_PREPARE_TEST_LOCK.lock().unwrap();
        let fixture = LocalImageFixture::new();
        let builder = fixture.builder();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (first, second) = runtime.block_on(async {
            tokio::join!(
                builder.prepare(
                    &fixture.context,
                    super::MINIMUM_DISK_BYTES,
                    &fixture.cache,
                    CachePolicy::Reuse,
                ),
                builder.prepare(
                    &fixture.context,
                    super::MINIMUM_DISK_BYTES,
                    &fixture.cache,
                    CachePolicy::Reuse,
                ),
            )
        });
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.path(), second.path());
        assert_eq!(first.workdir(), "/workspace");
        assert_eq!(first.shell(), "sh");
        assert_eq!(first.environment()["NANOCODEX_FIXTURE_MODE"], "flattened");
        assert_eq!(
            [first.disk_status(), second.disk_status()]
                .into_iter()
                .filter(|status| *status == DiskStatus::Created)
                .count(),
            1
        );
        assert!(valid_cached_ext4_disk(first.path()).unwrap());

        let attempt = fixture.root.path().join("attempt.ext4");
        assert_eq!(
            first.reflink_to(&attempt).unwrap(),
            super::MINIMUM_DISK_BYTES
        );
        assert!(attempt.is_file());
        assert!(first.reflink_to(&attempt).is_err());
    }

    #[test]
    fn invalid_prepared_disk_is_rebuilt_under_the_same_cache_identity() {
        let _test_guard = IMAGE_PREPARE_TEST_LOCK.lock().unwrap();
        let fixture = LocalImageFixture::new();
        let builder = fixture.builder();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let first = builder
                .prepare(
                    &fixture.context,
                    super::MINIMUM_DISK_BYTES,
                    &fixture.cache,
                    CachePolicy::Reuse,
                )
                .await
                .unwrap();
            let mut permissions = fs::metadata(first.path()).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            fs::set_permissions(first.path(), permissions).unwrap();
            fs::write(first.path(), b"corrupt").unwrap();

            let repaired = builder
                .prepare(
                    &fixture.context,
                    super::MINIMUM_DISK_BYTES,
                    &fixture.cache,
                    CachePolicy::Reuse,
                )
                .await
                .unwrap();

            assert_eq!(repaired.path(), first.path());
            assert_eq!(repaired.disk_status(), DiskStatus::Created);
            assert_eq!(repaired.shell(), "sh");
        });
    }

    #[test]
    fn preparation_has_one_bounded_root_with_parallel_work_below_it() {
        let _test_guard = IMAGE_PREPARE_TEST_LOCK.lock().unwrap();
        let fixture = LocalImageFixture::new();
        let builder = fixture.builder().prefer_ipv4();
        let capture = TraceCapture::default();
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::callsite::rebuild_interest_cache();
            runtime.block_on(
                async {
                    builder
                        .prepare(
                            &fixture.context,
                            super::MINIMUM_DISK_BYTES,
                            &fixture.cache,
                            CachePolicy::Reuse,
                        )
                        .await
                        .unwrap();
                }
                .instrument(tracing::info_span!("test.image.caller")),
            );
        });

        let spans = capture.spans.lock().unwrap();
        let (caller_id, _) = spans
            .iter()
            .find(|(_, span)| span.name == "test.image.caller")
            .unwrap();
        let (prepare_id, prepare) = spans
            .iter()
            .find(|(_, span)| span.name == "vm.image.prepare")
            .unwrap();
        assert_eq!(prepare.parent, Some(*caller_id));
        assert_eq!(
            prepare.fields.get("status").map(String::as_str),
            Some("completed")
        );
        assert_eq!(
            prepare.fields.get("image.cache.status").map(String::as_str),
            Some("created")
        );
        assert!(prepare.fields.contains_key("duration_ns"));
        assert_eq!(
            prepare
                .fields
                .get("image.build.prefer_ipv4")
                .map(String::as_str),
            Some("true")
        );

        let resolve = spans
            .values()
            .find(|span| span.name == "vm.image.resolve")
            .unwrap();
        let format = spans
            .values()
            .find(|span| span.name == "vm.image.format")
            .unwrap();
        assert_eq!(resolve.parent, Some(*prepare_id));
        assert_eq!(format.parent, Some(*prepare_id));

        let dockerfile_event = capture.events.lock().unwrap().iter().find_map(|event| {
            (event.parent == Some(*prepare_id)
                && event.fields.get("content_kind").map(String::as_str) == Some("dockerfile"))
            .then(|| event.fields.get("content").cloned())
            .flatten()
        });
        assert_eq!(
            dockerfile_event.as_deref(),
            Some(
                "FROM example.invalid/nanocodex-vm-fixture:latest\n\
                 ENV NANOCODEX_FIXTURE_MODE=flattened\n\
                 WORKDIR /workspace\n"
            )
        );
    }
}
