use std::{
    any::Any,
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use thiserror::Error;

use crate::{
    command::GuestCommand,
    config::{Network, SharedDirectory, VmConfig},
};

/// Exclusive guest root under which provider egress assets may be exposed.
pub const GUEST_EGRESS_ROOT: &str = "/tmp/nanocodex/egress";
/// Maximum size of one public file provisioned through an egress lease.
pub const MAX_EGRESS_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_BUILD_CACHE_SCOPE_BYTES: usize = 256;
const INTERNET_BUILD_CACHE_SCOPE: &str = "network-internet-v1";
const DISABLED_BUILD_CACHE_SCOPE: &str = "network-disabled-v1";

/// VM-facing outbound-access configuration retained for one guest lifetime.
///
/// An application-specific provider can resolve MPP, secret, or capability
/// policy into this type without exposing that policy to the VM runtime.
/// Values are deliberately omitted from `Debug`: proxy URLs may contain short-lived
/// credentials.
#[derive(Clone)]
pub struct EgressLease {
    network: Network,
    guest_environment: BTreeMap<String, String>,
    guest_mounts: BTreeMap<String, EgressMount>,
    guest_files: BTreeMap<PathBuf, EgressFile>,
    guards: Vec<Arc<dyn Any + Send + Sync>>,
    build_cache_scope: Option<String>,
}

/// One provider-owned host directory mounted read-only into the guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EgressMount {
    tag: String,
    host_path: PathBuf,
    guest_path: PathBuf,
}

impl EgressMount {
    /// Creates one provider directory that will be mounted read-only.
    #[must_use]
    pub fn read_only(
        tag: impl Into<String>,
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            tag: tag.into(),
            host_path: host_path.into(),
            guest_path: guest_path.into(),
        }
    }

    /// Returns the virtiofs mount tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the provider-owned host directory.
    #[must_use]
    pub fn host_path(&self) -> &Path {
        &self.host_path
    }

    /// Returns the path at which the guest mounts the directory.
    #[must_use]
    pub fn guest_path(&self) -> &Path {
        &self.guest_path
    }
}

/// One provider-owned public file provisioned before agent tools are exposed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EgressFile {
    guest_path: PathBuf,
    contents: Vec<u8>,
    mode: u32,
}

impl EgressFile {
    /// Creates one public file to provision before guest tools are exposed.
    #[must_use]
    pub fn new(guest_path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>, mode: u32) -> Self {
        Self {
            guest_path: guest_path.into(),
            contents: contents.into(),
            mode,
        }
    }

    /// Returns the normalized guest destination.
    #[must_use]
    pub fn guest_path(&self) -> &Path {
        &self.guest_path
    }

    /// Returns the complete public file contents.
    #[must_use]
    pub fn contents(&self) -> &[u8] {
        &self.contents
    }

    /// Returns Unix permission bits applied to the guest file.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }
}

impl EgressLease {
    /// Creates an empty lease for an explicit network mode.
    #[must_use]
    pub fn new(network: Network) -> Self {
        let build_cache_scope = match &network {
            Network::Disabled => Some(DISABLED_BUILD_CACHE_SCOPE.to_owned()),
            Network::Internet => Some(INTERNET_BUILD_CACHE_SCOPE.to_owned()),
            Network::Gvproxy { .. } => None,
        };
        Self {
            network,
            guest_environment: BTreeMap::new(),
            guest_mounts: BTreeMap::new(),
            guest_files: BTreeMap::new(),
            guards: Vec::new(),
            build_cache_scope,
        }
    }

    /// Creates an empty lease with outbound internet access.
    #[must_use]
    pub fn internet() -> Self {
        Self::new(Network::Internet)
    }

    /// Creates an empty lease with networking disabled.
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(Network::Disabled)
    }

    /// Adds one guest environment value.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is empty or was already assigned a
    /// different value by another egress component.
    pub fn insert_environment(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), EgressError> {
        let name = name.into();
        if !valid_environment_name(&name) {
            return Err(EgressError::InvalidEnvironmentName(name));
        }
        let value = value.into();
        if value.contains('\0') {
            return Err(EgressError::EnvironmentValueContainsNul(name));
        }
        if self
            .guest_environment
            .get(&name)
            .is_some_and(|current| current != &value)
        {
            return Err(EgressError::EnvironmentConflict(name));
        }
        if self.guest_environment.get(&name) == Some(&value) {
            return Ok(());
        }
        self.guest_environment.insert(name, value);
        self.build_cache_scope = None;
        Ok(())
    }

    /// Adds one read-only provider mount.
    ///
    /// # Errors
    ///
    /// Returns an error when the tag or guest path collides with a different
    /// mount.
    pub fn insert_mount(&mut self, mount: EgressMount) -> Result<(), EgressError> {
        if !valid_mount_tag(&mount.tag) {
            return Err(EgressError::InvalidMountTag(mount.tag));
        }
        if !mount.host_path.is_absolute() {
            return Err(EgressError::HostMountPathNotAbsolute(mount.host_path));
        }
        if !valid_guest_egress_path(&mount.guest_path) {
            return Err(EgressError::GuestMountPathOutsideRoot(mount.guest_path));
        }
        if self.guest_mounts.values().any(|current| {
            paths_overlap(&current.guest_path, &mount.guest_path) && current != &mount
        }) {
            return Err(EgressError::GuestMountConflict(mount.guest_path));
        }
        if self
            .guest_mounts
            .get(&mount.tag)
            .is_some_and(|current| current != &mount)
        {
            return Err(EgressError::MountTagConflict(mount.tag));
        }
        if self.guest_mounts.get(&mount.tag) == Some(&mount) {
            return Ok(());
        }
        if self
            .guest_files
            .keys()
            .any(|path| path.starts_with(&mount.guest_path))
        {
            return Err(EgressError::GuestMountFileOverlap(mount.guest_path));
        }
        self.guest_mounts.insert(mount.tag.clone(), mount);
        self.build_cache_scope = None;
        Ok(())
    }

    /// Adds one public CA or provider configuration file to provision in the guest.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest path is not absolute or conflicts with
    /// a different provider file.
    pub fn insert_file(&mut self, file: EgressFile) -> Result<(), EgressError> {
        if !valid_guest_egress_path(&file.guest_path) {
            return Err(EgressError::GuestFilePathOutsideRoot(file.guest_path));
        }
        if file.contents.len() > MAX_EGRESS_FILE_BYTES {
            return Err(EgressError::GuestFileTooLarge {
                path: file.guest_path,
                size: file.contents.len(),
                limit: MAX_EGRESS_FILE_BYTES,
            });
        }
        if file.mode & !0o777 != 0 {
            return Err(EgressError::InvalidGuestFileMode {
                path: file.guest_path,
                mode: file.mode,
            });
        }
        if self
            .guest_mounts
            .values()
            .any(|mount| file.guest_path.starts_with(&mount.guest_path))
        {
            return Err(EgressError::GuestMountFileOverlap(file.guest_path));
        }
        if self
            .guest_files
            .get(&file.guest_path)
            .is_some_and(|current| current != &file)
        {
            return Err(EgressError::GuestFileConflict(file.guest_path));
        }
        if self.guest_files.get(&file.guest_path) == Some(&file) {
            return Ok(());
        }
        self.guest_files.insert(file.guest_path.clone(), file);
        self.build_cache_scope = None;
        Ok(())
    }

    /// Assigns an opaque non-secret identity for outputs built through this
    /// exact egress policy.
    ///
    /// Adding environment, mounts, or files clears the scope. Set it only after
    /// composing the complete lease. The value becomes part of VM image cache
    /// identity and must distinguish credential or provider state that can
    /// change a Dockerfile `RUN` result, but it must never contain the
    /// credential itself.
    ///
    /// # Errors
    ///
    /// Returns an error when the scope is empty, too large, or contains a NUL
    /// byte.
    pub fn set_build_cache_scope(&mut self, scope: impl Into<String>) -> Result<(), EgressError> {
        let scope = scope.into();
        if scope.is_empty() || scope.len() > MAX_BUILD_CACHE_SCOPE_BYTES || scope.contains('\0') {
            return Err(EgressError::InvalidBuildCacheScope);
        }
        self.build_cache_scope = Some(scope);
        Ok(())
    }

    /// Returns this complete lease with an opaque non-secret build-cache
    /// identity.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::set_build_cache_scope`].
    pub fn with_build_cache_scope(mut self, scope: impl Into<String>) -> Result<Self, EgressError> {
        self.set_build_cache_scope(scope)?;
        Ok(self)
    }

    /// Retains provider state, such as a revocable proxy lease, until the guest
    /// is dropped.
    pub fn retain<T>(&mut self, guard: Arc<T>)
    where
        T: Any + Send + Sync,
    {
        self.guards.push(guard);
    }

    /// Combines independently provisioned egress fragments.
    ///
    /// Identical network, environment, mount, and guest-file configuration is
    /// idempotent.
    /// Conflicting configuration fails closed.
    ///
    /// # Errors
    ///
    /// Returns an error when the fragments select different network modes or
    /// assign incompatible environment, mount, or guest-file values.
    pub fn merge(&mut self, other: Self) -> Result<(), EgressError> {
        if self.network != other.network {
            return Err(EgressError::NetworkConflict);
        }
        let self_contributes_policy = !self.guest_environment.is_empty()
            || !self.guest_mounts.is_empty()
            || !self.guest_files.is_empty()
            || !self.guards.is_empty();
        let other_contributes_policy = !other.guest_environment.is_empty()
            || !other.guest_mounts.is_empty()
            || !other.guest_files.is_empty()
            || !other.guards.is_empty();
        let other_adds_policy = !other.guards.is_empty()
            || other
                .guest_environment
                .iter()
                .any(|(name, value)| self.guest_environment.get(name) != Some(value))
            || other
                .guest_mounts
                .iter()
                .any(|(tag, mount)| self.guest_mounts.get(tag) != Some(mount))
            || other
                .guest_files
                .iter()
                .any(|(path, file)| self.guest_files.get(path) != Some(file));
        let self_build_cache_scope = self.build_cache_scope.clone();
        let other_build_cache_scope = other.build_cache_scope.clone();
        let mut merged = self.clone();
        for (name, value) in other.guest_environment {
            merged.insert_environment(name, value)?;
        }
        for mount in other.guest_mounts.into_values() {
            merged.insert_mount(mount)?;
        }
        for file in other.guest_files.into_values() {
            merged.insert_file(file)?;
        }
        merged.guards.extend(other.guards);
        merged.build_cache_scope = match (self_contributes_policy, other_contributes_policy) {
            (false, false) | (true, false) => self_build_cache_scope,
            (false, true) => other_build_cache_scope,
            (true, true)
                if !other_adds_policy && self_build_cache_scope == other_build_cache_scope =>
            {
                self_build_cache_scope
            }
            (true, true) => None,
        };
        *self = merged;
        Ok(())
    }

    /// Returns this lease with one independently provisioned layer applied.
    ///
    /// This is the fluent form of [`Self::merge`]. It lets an application
    /// assemble a VM route from concrete MPP, secret-gateway, and other
    /// provider leases without teaching the VM package about those policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the new layer conflicts with an earlier layer.
    pub fn with_layer(mut self, layer: Self) -> Result<Self, EgressError> {
        self.merge(layer)?;
        Ok(self)
    }

    /// Applies this lease to one VM configuration and guest command.
    ///
    /// Provider directories are attached read-only and mounted before the
    /// requested program starts. Guest environment from the lease overrides
    /// the command's value for the same name. The lease itself must remain
    /// alive for at least as long as the resulting VM so its provider guards
    /// continue to protect revocable proxy and secret routes.
    #[must_use]
    pub fn configure(&self, mut vm: VmConfig, command: &GuestCommand) -> (VmConfig, GuestCommand) {
        vm = vm.network(self.network.clone());
        let mut configured = if self.guest_mounts.is_empty() {
            command.clone()
        } else {
            let mut script = OsString::from("set -eu;");
            for mount in self.guest_mounts() {
                vm = vm.shared_directory(SharedDirectory::read_only(&mount.tag, &mount.host_path));
                append_shell_command(&mut script, " mkdir -p -- ", [mount.guest_path.as_os_str()]);
                append_shell_command(
                    &mut script,
                    "; mount -t virtiofs -o ro ",
                    [OsStr::new(&mount.tag), mount.guest_path.as_os_str()],
                );
            }
            append_shell_command(
                &mut script,
                "; cd -- ",
                [command.current_directory().as_os_str()],
            );
            append_shell_command(
                &mut script,
                "; exec ",
                std::iter::once(command.program().as_os_str())
                    .chain(command.arguments().iter().map(OsString::as_os_str)),
            );
            let mut configured = GuestCommand::new("/bin/sh").args(["-c"]).arg(script);
            for (name, value) in command.environment() {
                configured = configured.env(name, value);
            }
            configured
        };
        for (name, value) in self.guest_environment() {
            configured = configured.env(name, value);
        }
        (vm, configured)
    }

    /// Returns the network mode required by the complete lease.
    #[must_use]
    pub const fn network(&self) -> &Network {
        &self.network
    }

    /// Returns guest-visible environment entries.
    ///
    /// Values may contain short-lived capabilities and must not be logged.
    #[must_use]
    pub const fn guest_environment(&self) -> &BTreeMap<String, String> {
        &self.guest_environment
    }

    /// Returns the non-secret identity under which Dockerfile build outputs may
    /// be reused.
    ///
    /// `None` means guest-visible provider state was added after the last
    /// explicit scope assignment, so a cached `RUN` output must not be reused.
    #[must_use]
    pub fn build_cache_scope(&self) -> Option<&str> {
        self.build_cache_scope.as_deref()
    }

    /// Iterates over read-only provider mounts.
    pub fn guest_mounts(&self) -> impl Iterator<Item = &EgressMount> {
        self.guest_mounts.values()
    }

    /// Iterates over public files to provision before tool execution.
    pub fn guest_files(&self) -> impl Iterator<Item = &EgressFile> {
        self.guest_files.values()
    }
}

impl fmt::Debug for EgressLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EgressLease")
            .field("network", &self.network)
            .field(
                "guest_environment_keys",
                &self.guest_environment.keys().collect::<Vec<_>>(),
            )
            .field(
                "guest_mounts",
                &self.guest_mounts.values().collect::<Vec<_>>(),
            )
            .field(
                "guest_file_paths",
                &self.guest_files.keys().collect::<Vec<_>>(),
            )
            .field("guards", &self.guards.len())
            .field("build_cache_scoped", &self.build_cache_scope.is_some())
            .finish()
    }
}

/// Invalid or conflicting egress capability composition.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum EgressError {
    /// A build-cache scope was empty, too large, or contained a NUL byte.
    #[error("egress build-cache scope must be 1 to 256 non-NUL bytes")]
    InvalidBuildCacheScope,
    /// Two layers require different guest network modes.
    #[error("egress fragments require conflicting VM network modes")]
    NetworkConflict,
    /// A guest environment name is not a portable shell identifier.
    #[error("guest environment name `{0}` is not a shell identifier")]
    InvalidEnvironmentName(String),
    /// Two layers assign different values to one guest environment name.
    #[error("guest environment `{0}` has conflicting egress values")]
    EnvironmentConflict(String),
    /// A guest environment value cannot be represented as a process value.
    #[error("guest environment `{0}` contains a NUL byte")]
    EnvironmentValueContainsNul(String),
    /// A virtiofs tag is empty, too long, or contains unsupported characters.
    #[error("egress mount tag `{0}` is not a portable identifier")]
    InvalidMountTag(String),
    /// A provider mount refers to a relative host path.
    #[error("host egress mount path must be absolute: {0}")]
    HostMountPathNotAbsolute(PathBuf),
    /// A mount destination escapes the exclusive guest egress root.
    #[error("guest egress mount path must be a normalized child of {GUEST_EGRESS_ROOT}: {0}")]
    GuestMountPathOutsideRoot(PathBuf),
    /// Two different provider mounts claim the same virtiofs tag.
    #[error("egress mount tag `{0}` has conflicting host paths")]
    MountTagConflict(String),
    /// Two different provider mounts overlap inside the guest.
    #[error("guest egress mount path `{0}` has conflicting providers")]
    GuestMountConflict(PathBuf),
    /// A provisioned file overlaps a provider directory mount.
    #[error("guest egress mount and file paths overlap at `{0}`")]
    GuestMountFileOverlap(PathBuf),
    /// A provisioned file escapes the exclusive guest egress root.
    #[error("guest egress file path must be a normalized child of {GUEST_EGRESS_ROOT}: {0}")]
    GuestFilePathOutsideRoot(PathBuf),
    /// A provisioned file exceeds [`MAX_EGRESS_FILE_BYTES`].
    #[error("guest egress file `{path}` is {size} bytes, exceeding the {limit}-byte limit")]
    GuestFileTooLarge {
        /// Guest destination.
        path: PathBuf,
        /// Requested file size.
        size: usize,
        /// Enforced maximum.
        limit: usize,
    },
    /// A provisioned file contains mode bits outside Unix permissions.
    #[error("guest egress file `{path}` has invalid mode {mode:#o}")]
    InvalidGuestFileMode {
        /// Guest destination.
        path: PathBuf,
        /// Rejected mode bits.
        mode: u32,
    },
    /// Two layers assign different files to one guest destination.
    #[error("guest egress file path `{0}` has conflicting providers")]
    GuestFileConflict(PathBuf),
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn valid_mount_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_guest_egress_path(path: &Path) -> bool {
    path.is_absolute()
        && path != Path::new(GUEST_EGRESS_ROOT)
        && path.starts_with(GUEST_EGRESS_ROOT)
        && path
            .components()
            .all(|component| !matches!(component, Component::CurDir | Component::ParentDir))
}

fn paths_overlap(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn append_shell_command<'a>(
    script: &mut OsString,
    prefix: &str,
    arguments: impl IntoIterator<Item = &'a OsStr>,
) {
    script.push(prefix);
    let mut first = true;
    for argument in arguments {
        if !first {
            script.push(" ");
        }
        first = false;
        script.push(shell_single_quote(argument));
    }
}

fn shell_single_quote(value: &OsStr) -> OsString {
    let bytes = value.as_bytes();
    let mut quoted = Vec::with_capacity(bytes.len().saturating_add(2));
    quoted.push(b'\'');
    for byte in bytes {
        match *byte {
            b'\'' => quoted.extend_from_slice(b"'\\''"),
            // libkrun cannot carry a literal double quote in an argv entry.
            // Produce it at wrapper-shell evaluation time instead.
            b'"' => quoted.extend_from_slice(b"'$(printf '\\042')'"),
            byte => quoted.push(byte),
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cache_scope_tracks_the_complete_egress_policy() {
        let mut internet = EgressLease::internet();
        assert_eq!(
            internet.build_cache_scope(),
            Some(INTERNET_BUILD_CACHE_SCOPE)
        );
        assert_eq!(
            EgressLease::disabled().build_cache_scope(),
            Some(DISABLED_BUILD_CACHE_SCOPE)
        );
        assert!(
            EgressLease::new(Network::gvproxy("/tmp/gvproxy.sock"))
                .build_cache_scope()
                .is_none()
        );

        internet
            .insert_environment("HTTPS_PROXY", "http://proxy")
            .unwrap();
        assert!(internet.build_cache_scope().is_none());
        internet.set_build_cache_scope("proxy-policy-v1").unwrap();
        internet
            .insert_environment("HTTPS_PROXY", "http://proxy")
            .unwrap();
        assert_eq!(internet.build_cache_scope(), Some("proxy-policy-v1"));
        internet
            .insert_file(EgressFile::new(
                "/tmp/nanocodex/egress/ca.pem",
                Vec::new(),
                0o444,
            ))
            .unwrap();
        assert!(internet.build_cache_scope().is_none());
    }

    #[test]
    fn merged_egress_preserves_only_an_identical_cache_scope() {
        let mut empty = EgressLease::internet();
        let mut scoped = EgressLease::internet();
        scoped
            .insert_environment("HTTPS_PROXY", "http://proxy")
            .unwrap();
        scoped.set_build_cache_scope("scoped-policy").unwrap();
        empty.merge(scoped).unwrap();
        assert_eq!(empty.build_cache_scope(), Some("scoped-policy"));

        let mut first = EgressLease::internet();
        first
            .insert_environment("HTTPS_PROXY", "http://proxy")
            .unwrap();
        first.set_build_cache_scope("same-policy").unwrap();
        let mut identical = EgressLease::internet();
        identical
            .insert_environment("HTTPS_PROXY", "http://proxy")
            .unwrap();
        identical.set_build_cache_scope("same-policy").unwrap();
        first.merge(identical).unwrap();
        assert_eq!(first.build_cache_scope(), Some("same-policy"));

        first.merge(EgressLease::internet()).unwrap();
        assert_eq!(first.build_cache_scope(), Some("same-policy"));

        let mut additional = EgressLease::internet();
        additional
            .insert_environment("NO_PROXY", "localhost")
            .unwrap();
        additional.set_build_cache_scope("same-policy").unwrap();
        first.merge(additional).unwrap();
        assert!(first.build_cache_scope().is_none());

        first.set_build_cache_scope("same-policy").unwrap();
        let mut different = EgressLease::internet();
        different
            .insert_environment("HTTPS_PROXY", "http://proxy")
            .unwrap();
        different.set_build_cache_scope("different-policy").unwrap();
        first.merge(different).unwrap();
        assert!(first.build_cache_scope().is_none());
    }

    #[test]
    fn build_cache_scope_rejects_unsafe_identifiers() {
        let mut lease = EgressLease::internet();
        assert_eq!(
            lease.set_build_cache_scope(""),
            Err(EgressError::InvalidBuildCacheScope)
        );
        assert_eq!(
            lease.set_build_cache_scope("contains\0nul"),
            Err(EgressError::InvalidBuildCacheScope)
        );
        assert_eq!(
            lease.set_build_cache_scope("x".repeat(MAX_BUILD_CACHE_SCOPE_BYTES + 1)),
            Err(EgressError::InvalidBuildCacheScope)
        );
    }

    #[test]
    fn independently_provisioned_egress_fragments_compose() {
        let guard = Arc::new(());
        let mut secrets = EgressLease::internet();
        secrets
            .insert_environment("NANOCENTAUR_SECRET_BASE_URL", "https://secret-gateway/v1")
            .unwrap();
        secrets
            .insert_mount(EgressMount::read_only(
                "secret-ca",
                "/host/ca",
                "/tmp/nanocodex/egress/secrets/ca",
            ))
            .unwrap();
        secrets.retain(Arc::clone(&guard));

        let mut mpp = EgressLease::internet();
        mpp.insert_environment(
            "HTTPS_PROXY",
            "http://mpp-lease:credential@host.internal:8080",
        )
        .unwrap();
        let egress = EgressLease::internet()
            .with_layer(mpp)
            .unwrap()
            .with_layer(secrets)
            .unwrap();

        assert_eq!(egress.guest_environment().len(), 2);
        assert_eq!(egress.guest_mounts().count(), 1);
        assert_eq!(Arc::strong_count(&guard), 2);
        let debug = format!("{egress:?}");
        assert!(!debug.contains("credential"));
        assert!(!debug.contains("secret-gateway"));
    }

    #[test]
    fn conflicting_provider_values_fail_closed() {
        let mut secrets = EgressLease::internet();
        secrets
            .insert_environment("HTTPS_PROXY", "http://secret-gateway")
            .unwrap();
        let mut mpp = EgressLease::internet();
        mpp.insert_environment("HTTPS_PROXY", "http://mpp-gateway")
            .unwrap();

        assert_eq!(
            secrets.merge(mpp),
            Err(EgressError::EnvironmentConflict("HTTPS_PROXY".to_owned()))
        );
    }

    #[test]
    fn provider_files_compose_idempotently_and_conflicts_fail_closed() {
        let path = "/tmp/nanocodex/egress/mpp/ca.pem";
        let file = EgressFile::new(path, b"public ca".to_vec(), 0o444);
        let mut first = EgressLease::internet();
        first.insert_file(file.clone()).unwrap();
        let mut identical = EgressLease::internet();
        identical.insert_file(file).unwrap();
        first.merge(identical).unwrap();
        assert_eq!(first.guest_files().count(), 1);

        let mut conflicting = EgressLease::internet();
        conflicting
            .insert_file(EgressFile::new(path, b"different ca".to_vec(), 0o444))
            .unwrap();
        assert_eq!(
            first.merge(conflicting),
            Err(EgressError::GuestFileConflict(PathBuf::from(path)))
        );
    }

    #[test]
    fn provider_file_paths_must_be_absolute() {
        let mut lease = EgressLease::internet();
        assert_eq!(
            lease.insert_file(EgressFile::new("relative/ca.pem", Vec::new(), 0o444)),
            Err(EgressError::GuestFilePathOutsideRoot(PathBuf::from(
                "relative/ca.pem"
            )))
        );
    }

    #[test]
    fn lease_configures_network_mounts_and_proxy_environment() {
        let mut lease = EgressLease::internet();
        lease
            .insert_environment("HTTPS_PROXY", "http://host.internal:8080")
            .unwrap();
        lease
            .insert_mount(EgressMount::read_only(
                "mpp-ca",
                "/host/mpp",
                "/tmp/nanocodex/egress/mpp",
            ))
            .unwrap();
        let command = GuestCommand::new("/usr/local/bin/nanocodex-vm-guest")
            .arg("/workspace")
            .arg(r#"say "hello""#)
            .env("HTTPS_PROXY", "http://untrusted")
            .current_dir("/workspace");

        let (vm, command) = lease.configure(
            VmConfig::ext4("/tmp/rootfs").network(Network::Disabled),
            &command,
        );

        assert_eq!(vm.network_value(), &Network::Internet);
        assert_eq!(
            vm.shared_directories(),
            &[SharedDirectory::read_only("mpp-ca", "/host/mpp")]
        );
        assert_eq!(command.program(), std::path::Path::new("/bin/sh"));
        assert_eq!(command.arguments()[0], "-c");
        let script = command.arguments()[1].to_string_lossy();
        assert!(script.contains("mount -t virtiofs -o ro 'mpp-ca'"));
        assert!(script.contains("cd -- '/workspace'"));
        assert!(script.contains("exec '/usr/local/bin/nanocodex-vm-guest' '/workspace'"));
        assert!(script.contains("$(printf '\\042')"));
        assert!(!script.contains('"'));
        assert_eq!(
            command
                .environment()
                .get(&std::ffi::OsString::from("HTTPS_PROXY")),
            Some(&std::ffi::OsString::from("http://host.internal:8080"))
        );
    }

    #[test]
    fn mount_and_file_path_hierarchy_conflicts_fail_before_launch() {
        let mut lease = EgressLease::internet();
        lease
            .insert_mount(EgressMount::read_only(
                "provider",
                "/host/provider",
                "/tmp/nanocodex/egress/provider",
            ))
            .unwrap();

        assert_eq!(
            lease.insert_file(EgressFile::new(
                "/tmp/nanocodex/egress/provider/ca.pem",
                Vec::new(),
                0o444,
            )),
            Err(EgressError::GuestMountFileOverlap(PathBuf::from(
                "/tmp/nanocodex/egress/provider/ca.pem"
            )))
        );
        assert_eq!(
            lease.insert_mount(EgressMount::read_only(
                "nested",
                "/host/nested",
                "/tmp/nanocodex/egress/provider/nested",
            )),
            Err(EgressError::GuestMountConflict(PathBuf::from(
                "/tmp/nanocodex/egress/provider/nested"
            )))
        );
    }

    #[test]
    fn egress_rejects_unsafe_values_before_vmm_configuration() {
        let mut lease = EgressLease::internet();
        assert_eq!(
            lease.insert_environment("HTTPS_PROXY", "http://proxy\0hidden"),
            Err(EgressError::EnvironmentValueContainsNul(
                "HTTPS_PROXY".to_owned()
            ))
        );
        assert!(matches!(
            lease.insert_file(EgressFile::new(
                "/tmp/nanocodex/egress/mpp/ca.pem",
                vec![0; MAX_EGRESS_FILE_BYTES + 1],
                0o444,
            )),
            Err(EgressError::GuestFileTooLarge { .. })
        ));
        assert!(matches!(
            lease.insert_file(EgressFile::new(
                "/tmp/nanocodex/egress/mpp/ca.pem",
                Vec::new(),
                0o4755,
            )),
            Err(EgressError::InvalidGuestFileMode { .. })
        ));
        assert!(matches!(
            lease.insert_mount(EgressMount::read_only(
                "bad tag",
                "/host/provider",
                "/tmp/nanocodex/egress/provider",
            )),
            Err(EgressError::InvalidMountTag(_))
        ));
        assert!(matches!(
            lease.insert_mount(EgressMount::read_only(
                "provider",
                "relative",
                "/tmp/nanocodex/egress/provider",
            )),
            Err(EgressError::HostMountPathNotAbsolute(_))
        ));
    }

    #[test]
    fn mount_free_lease_preserves_the_direct_guest_command() {
        let command = GuestCommand::new("/usr/local/bin/nanocodex-vm-guest")
            .arg("/workspace")
            .current_dir("/workspace");

        let (vm, configured) =
            EgressLease::disabled().configure(VmConfig::new("/rootfs"), &command);

        assert_eq!(vm.network_value(), &Network::Disabled);
        assert_eq!(configured, command);
    }
}
