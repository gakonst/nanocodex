use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use nanocodex_tools::ToolsBuilder;
use tokio::process::Command;

use crate::{
    command::GuestCommand,
    config::{BlockDevice, Network, VmConfig},
    egress::EgressLease,
    image::{host_resolver_configuration, reflink_or_sparse_copy},
    tools::{
        DEFAULT_SHUTDOWN_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, VmToolSession, VmToolSessionError,
        VmTools,
    },
};

const DEFAULT_CPUS: u8 = 2;
const DEFAULT_MEMORY_MIB: u32 = 1_024;
const DEFAULT_WORKSPACE: &str = "/app";
const DEFAULT_SHELL: &str = "sh";
const RUNTIME_BLOCK_ID: &str = "nanocodex-runtime";
const RUNTIME_DEVICE: &str = "/dev/vdb";
const RUNTIME_MOUNT: &str = "/run/nanocodex";
const RUNTIME_EXECUTABLE: &str = "/run/nanocodex/nanocodex-vm-guest";

/// High-level builder for one private retained VM workspace.
///
/// The normal portable path uses [`Self::private_from`] to materialize a
/// writable raw-ext4 root from an immutable prepared image. The VMM executable
/// is a small application-owned process that reads the private launch record
/// appended by this crate and calls [`crate::host::VmProcessConfig::run`].
pub struct VmWorkspaceBuilder {
    rootfs: PathBuf,
    vmm_executable: PathBuf,
    vmm_arguments: Vec<OsString>,
    guest_runtime_disk: Option<PathBuf>,
    firmware_directory: Option<PathBuf>,
    workspace: String,
    shell: String,
    environment: BTreeMap<String, String>,
    cpus: u8,
    memory_mib: u32,
    egress: EgressLease,
    startup_timeout: Duration,
    shutdown_timeout: Duration,
}

/// One retained VM and the standard workspace tools routed into it.
///
/// Keep this owner alive for the complete Nanocodex session or root agent
/// tree. Clone-cheap tool capabilities may be captured by a
/// `NanocodexBuilder::tools_factory`; drop those agents and tools before
/// calling [`Self::shutdown`].
pub struct VmWorkspace {
    session: VmToolSession,
    rootfs: PathBuf,
    workspace: String,
    shell: String,
}

/// Failure to materialize, configure, start, or stop a high-level VM workspace.
#[derive(Debug, thiserror::Error)]
pub enum VmWorkspaceError {
    /// A private root or loader path could not be prepared.
    #[error("failed to prepare VM workspace files: {0}")]
    Io(#[from] std::io::Error),

    /// The source root was not a raw ext4 image.
    #[error("portable private VM workspaces require a raw ext4 source image: {0}")]
    PrivateRootSource(PathBuf),

    /// A required path was absent or had the wrong kind.
    #[error("invalid VM workspace path {path}: {reason}")]
    InvalidPath {
        /// Rejected path.
        path: PathBuf,
        /// Stable reason for rejection.
        reason: &'static str,
    },

    /// The guest working directory was not absolute.
    #[error("guest workspace must be absolute: {0}")]
    RelativeWorkspace(String),

    /// A raw ext4 root was configured without the companion runtime disk.
    #[error("raw ext4 VM workspace requires a prepared guest runtime disk")]
    MissingGuestRuntime,

    /// The retained guest session failed.
    #[error(transparent)]
    Session(#[from] VmToolSessionError),
}

impl VmWorkspace {
    /// Starts a builder around an already-private VM root.
    ///
    /// Prefer [`VmWorkspaceBuilder::private_from`] for immutable prepared ext4
    /// images. Passing a directory root is a low-level, non-confining escape
    /// hatch because libkrun's direct virtiofs access does not isolate the host
    /// mount namespace.
    #[must_use]
    pub fn builder(
        private_rootfs: impl Into<PathBuf>,
        vmm_executable: impl Into<PathBuf>,
    ) -> VmWorkspaceBuilder {
        VmWorkspaceBuilder::new(private_rootfs, vmm_executable)
    }

    /// Returns the private host root attached to this VM.
    #[must_use]
    pub fn rootfs(&self) -> &Path {
        &self.rootfs
    }

    /// Returns the absolute guest working directory used by the tool builder.
    #[must_use]
    pub fn guest_workspace(&self) -> &str {
        &self.workspace
    }

    /// Returns clone-cheap VM-backed standard workspace tools.
    #[must_use]
    pub fn tools(&self) -> VmTools {
        self.session.tools()
    }

    /// Returns a normal Nanocodex tool builder with workspace effects routed
    /// into this retained VM.
    #[must_use]
    pub fn tools_builder(&self) -> ToolsBuilder {
        self.configure_tools(self.tools().tools_builder())
    }

    /// Returns an attachment-safe builder with only VM-backed workspace tools.
    #[must_use]
    pub fn attachment_tools_builder(&self) -> ToolsBuilder {
        self.configure_tools(self.tools().attachment_tools_builder())
    }

    fn configure_tools(&self, builder: ToolsBuilder) -> ToolsBuilder {
        builder
            .working_directory(self.workspace.clone())
            .default_shell(self.shell.clone())
    }

    /// Gracefully cancels guest work, syncs filesystems, and stops the VMM.
    ///
    /// # Errors
    ///
    /// Returns an error when clone-cheap sibling capabilities or owner-borrowed
    /// requests are still alive, or the guest/VMM does not complete its
    /// bounded shutdown.
    pub async fn shutdown(&self) -> Result<(), VmWorkspaceError> {
        self.session.shutdown().await?;
        Ok(())
    }
}

impl VmWorkspaceBuilder {
    /// Configures an already-private root and application-owned VMM executable.
    #[must_use]
    pub fn new(private_rootfs: impl Into<PathBuf>, vmm_executable: impl Into<PathBuf>) -> Self {
        Self {
            rootfs: private_rootfs.into(),
            vmm_executable: vmm_executable.into(),
            vmm_arguments: Vec::new(),
            guest_runtime_disk: None,
            firmware_directory: None,
            workspace: DEFAULT_WORKSPACE.to_owned(),
            shell: DEFAULT_SHELL.to_owned(),
            environment: BTreeMap::new(),
            cpus: DEFAULT_CPUS,
            memory_mib: DEFAULT_MEMORY_MIB,
            egress: EgressLease::internet(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// Materializes one private writable ext4 root from an immutable image.
    ///
    /// The destination must not exist. Reflinks are used when available, with
    /// a sparse-copy fallback.
    ///
    /// # Errors
    ///
    /// Returns an error when the source is not a file, the destination exists,
    /// or the private disk cannot be materialized.
    pub fn private_from(
        immutable_rootfs: impl AsRef<Path>,
        private_rootfs: impl Into<PathBuf>,
        vmm_executable: impl Into<PathBuf>,
    ) -> Result<Self, VmWorkspaceError> {
        let source = immutable_rootfs.as_ref();
        if !source.is_file() {
            return Err(VmWorkspaceError::PrivateRootSource(source.to_path_buf()));
        }
        let destination = private_rootfs.into();
        if destination.exists() {
            return Err(VmWorkspaceError::InvalidPath {
                path: destination,
                reason: "private destination already exists",
            });
        }
        let parent = destination
            .parent()
            .ok_or_else(|| VmWorkspaceError::InvalidPath {
                path: destination.clone(),
                reason: "private destination has no parent",
            })?;
        fs::create_dir_all(parent)?;
        reflink_or_sparse_copy(source, &destination)?;
        Ok(Self::new(destination, vmm_executable))
    }

    /// Appends one argument understood by the application-owned VMM process.
    #[must_use]
    pub fn vmm_argument(mut self, argument: impl Into<OsString>) -> Self {
        self.vmm_arguments.push(argument.into());
        self
    }

    /// Selects the prepared read-only guest-runtime ext4 disk.
    #[must_use]
    pub fn guest_runtime_disk(mut self, disk: impl Into<PathBuf>) -> Self {
        self.guest_runtime_disk = Some(disk.into());
        self
    }

    /// Selects the directory containing platform libkrun firmware.
    ///
    /// Linux expects `libkrunfw.so.5`; macOS expects
    /// `libkrunfw.5.dylib`. The directory is placed on the corresponding
    /// loader path only for the VMM child.
    #[must_use]
    pub fn firmware_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.firmware_directory = Some(directory.into());
        self
    }

    /// Sets the absolute guest working directory.
    #[must_use]
    pub fn guest_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// Sets the shell description exposed to the model.
    #[must_use]
    pub fn shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// Replaces the environment inherited by guest workspace commands.
    ///
    /// The egress lease is applied after this environment and therefore wins
    /// when both provide the same name.
    #[must_use]
    pub fn environment(mut self, environment: impl IntoIterator<Item = (String, String)>) -> Self {
        self.environment = environment.into_iter().collect();
        self
    }

    /// Sets the virtual CPU count.
    #[must_use]
    pub const fn cpus(mut self, cpus: u8) -> Self {
        self.cpus = cpus;
        self
    }

    /// Sets guest memory in mebibytes.
    #[must_use]
    pub const fn memory_mib(mut self, memory_mib: u32) -> Self {
        self.memory_mib = memory_mib;
        self
    }

    /// Sets the complete deadline for guest readiness and egress provisioning.
    #[must_use]
    pub const fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Sets the complete deadline for guest sync and VMM exit.
    #[must_use]
    pub const fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Disables guest networking.
    #[must_use]
    pub fn offline(mut self) -> Self {
        self.egress = EgressLease::disabled();
        self
    }

    /// Installs an application-owned provider-neutral egress lease.
    #[must_use]
    pub fn egress(mut self, egress: EgressLease) -> Self {
        self.egress = egress;
        self
    }

    /// Starts the VM, waits for typed guest readiness, and returns its owner.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths, missing platform firmware/runtime,
    /// process startup, guest readiness, or egress provisioning.
    pub async fn launch(self) -> Result<VmWorkspace, VmWorkspaceError> {
        if !Path::new(&self.workspace).is_absolute() {
            return Err(VmWorkspaceError::RelativeWorkspace(self.workspace));
        }
        if !self.rootfs.exists() {
            return Err(VmWorkspaceError::InvalidPath {
                path: self.rootfs,
                reason: "private root does not exist",
            });
        }
        if !self.vmm_executable.is_file() {
            return Err(VmWorkspaceError::InvalidPath {
                path: self.vmm_executable,
                reason: "VMM executable is not a file",
            });
        }

        let ext4 = self.rootfs.is_file();
        let resolver = if ext4 && matches!(self.egress.network(), Network::Internet) {
            Some(host_resolver_configuration()?)
        } else {
            None
        };
        let (config, mut guest) = if ext4 {
            let runtime = self
                .guest_runtime_disk
                .ok_or(VmWorkspaceError::MissingGuestRuntime)?;
            if !runtime.is_file() {
                return Err(VmWorkspaceError::InvalidPath {
                    path: runtime,
                    reason: "guest runtime disk is not a file",
                });
            }
            let config = VmConfig::ext4(&self.rootfs)
                .cpus(self.cpus)
                .memory_mib(self.memory_mib)
                .block_device(BlockDevice::read_only(RUNTIME_BLOCK_ID, runtime));
            let guest = GuestCommand::new("/bin/sh")
                .arg("-c")
                .arg(ext4_bootstrap(&self.workspace, resolver.as_deref()));
            (config, guest)
        } else if self.rootfs.is_dir() {
            let runtime = self.rootfs.join("usr/local/bin/nanocodex-vm-guest");
            if !runtime.is_file() {
                return Err(VmWorkspaceError::InvalidPath {
                    path: runtime,
                    reason: "directory root is missing the guest runtime",
                });
            }
            (
                VmConfig::new(&self.rootfs)
                    .cpus(self.cpus)
                    .memory_mib(self.memory_mib),
                GuestCommand::new("/usr/local/bin/nanocodex-vm-guest").arg(&self.workspace),
            )
        } else {
            return Err(VmWorkspaceError::InvalidPath {
                path: self.rootfs,
                reason: "private root is neither a raw ext4 image nor a directory",
            });
        };
        for (name, value) in &self.environment {
            guest = guest.env(name, value);
        }

        let mut command = isolated_vmm_command(&self.vmm_executable, self.vmm_arguments);
        if let Some(firmware) = self.firmware_directory {
            let firmware = firmware.canonicalize()?;
            #[cfg(target_os = "linux")]
            {
                let library = firmware.join("libkrunfw.so.5");
                if !library.is_file() {
                    return Err(VmWorkspaceError::InvalidPath {
                        path: library,
                        reason: "Linux libkrun firmware is missing",
                    });
                }
                command.env("LD_LIBRARY_PATH", firmware);
            }
            #[cfg(target_os = "macos")]
            {
                let library = firmware.join("libkrunfw.5.dylib");
                if !library.is_file() {
                    return Err(VmWorkspaceError::InvalidPath {
                        path: library,
                        reason: "macOS libkrun firmware is missing",
                    });
                }
                command.env("DYLD_LIBRARY_PATH", firmware);
            }
        }

        let session = VmToolSession::spawn_configured_with_timeouts(
            command,
            config,
            guest,
            self.egress,
            self.startup_timeout,
            self.shutdown_timeout,
        )
        .await?;
        Ok(VmWorkspace {
            session,
            rootfs: self.rootfs,
            workspace: self.workspace,
            shell: self.shell,
        })
    }
}

fn ext4_bootstrap(workspace: &str, resolver: Option<&str>) -> String {
    let workspace = shell_word(workspace);
    let resolver = resolver_bootstrap(resolver);
    format!(
        "set -eu; {resolver}mkdir -p -- {workspace} {RUNTIME_MOUNT}; \
         mount -t ext4 -o ro {RUNTIME_DEVICE} {RUNTIME_MOUNT}; \
         exec {RUNTIME_EXECUTABLE} {workspace}"
    )
}

fn resolver_bootstrap(resolver: Option<&str>) -> String {
    resolver.map_or_else(String::new, |resolver| {
        // Guest argv is carried in libkrun's kernel command line and therefore
        // must remain printable ASCII. The resolver is already represented with
        // escaped newlines; let the guest's printf materialize them after boot.
        let resolver = shell_word(resolver);
        format!("rm -f /etc/resolv.conf; printf '%b' {resolver} > /etc/resolv.conf; ")
    })
}

fn isolated_vmm_command(
    executable: &Path,
    arguments: impl IntoIterator<Item = OsString>,
) -> Command {
    let mut command = Command::new(executable);
    command.args(arguments);
    // The owner handles terminal interrupts and asks the guest to sync before
    // stopping the VMM. Keep Ctrl-C from reaching the VMM independently.
    command.process_group(0);
    command
}

fn shell_word(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn materializes_a_private_sparse_root_without_overwriting() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("base.ext4");
        let destination = directory.path().join("attempt/root.ext4");
        let vmm = directory.path().join("vmm");
        fs::write(&source, [0_u8; 4096]).unwrap();
        fs::write(&vmm, b"vmm").unwrap();
        fs::set_permissions(&vmm, fs::Permissions::from_mode(0o755)).unwrap();

        let builder = VmWorkspaceBuilder::private_from(&source, &destination, &vmm).unwrap();

        assert_eq!(builder.rootfs, destination);
        assert_eq!(fs::read(builder.rootfs).unwrap(), fs::read(source).unwrap());
    }

    #[test]
    fn resolver_injection_is_limited_to_private_ext4_roots() {
        let resolver = "nameserver 192.0.2.1\\n";
        let ext4 = ext4_bootstrap("/workspace", Some(resolver));
        let offline = ext4_bootstrap("/workspace", None);

        assert!(ext4.contains("192.0.2.1"));
        assert!(ext4.contains("printf '%b'"));
        assert!(!ext4.contains('\n'));
        assert!(
            ext4.bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        );
        assert!(ext4.contains("> /etc/resolv.conf"));
        assert!(!offline.contains("resolv.conf"));
    }

    #[test]
    fn builder_retains_explicit_image_environment() {
        let builder = VmWorkspaceBuilder::new("/root.ext4", "/vmm").environment([
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("APP_MODE".to_owned(), "test".to_owned()),
        ]);

        assert_eq!(builder.environment["LANG"], "C.UTF-8");
        assert_eq!(builder.environment["APP_MODE"], "test");
    }

    #[tokio::test]
    async fn vmm_runs_outside_the_owners_terminal_process_group() {
        let directory = tempdir().unwrap();
        let vmm = directory.path().join("vmm");
        let record = directory.path().join("process-group");
        fs::write(
            &vmm,
            "#!/bin/sh\n\
             pid=$$\n\
             pgid=$(ps -o pgid= -p \"$pid\" | tr -d ' ')\n\
             printf '%s %s\\n' \"$pid\" \"$pgid\" > \"$RECORD\"\n",
        )
        .unwrap();
        fs::set_permissions(&vmm, fs::Permissions::from_mode(0o700)).unwrap();

        let mut command = isolated_vmm_command(&vmm, []);
        command.env("RECORD", &record);
        assert!(command.status().await.unwrap().success());

        let values = fs::read_to_string(record)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], values[1]);
    }

    #[tokio::test]
    async fn rejects_relative_guest_workspace_before_process_start() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        let vmm = directory.path().join("vmm");
        fs::create_dir(&root).unwrap();
        fs::write(&vmm, b"vmm").unwrap();

        let error = match VmWorkspace::builder(root, vmm)
            .guest_workspace("relative")
            .launch()
            .await
        {
            Ok(_) => panic!("relative workspace unexpectedly launched"),
            Err(error) => error,
        };

        assert!(matches!(error, VmWorkspaceError::RelativeWorkspace(_)));
    }
}
