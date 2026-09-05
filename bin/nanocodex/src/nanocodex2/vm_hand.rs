use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt as _;
use nanocodex_managed::ManagedError;
use nanocodex_tools::{Tools, attachment::AttachmentMachine};
use nanocodex_vm::{
    VmWorkspace, VmWorkspaceError,
    host::VmProcessConfig,
    tools::{GuestRuntimeDisk, VmToolSessionError},
};
use tokio::time::sleep;

use super::Hand;
pub(crate) use super::vm_hand_config::VmHandConfig;

const DEFAULT_KRUNFW_DIRECTORY: &str = ".cache/libkrunfw/libkrunfw";
const CAPABILITY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const CAPABILITY_DRAIN_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct VmHand {
    workspace: VmWorkspace,
    tools: Tools,
    machine: AttachmentMachine,
    _root_lock: Option<File>,
}

impl VmHand {
    pub(crate) async fn start(config: &Hand) -> Result<Self, ManagedError> {
        Self::start_config(&VmHandConfig::try_from(config)?).await
    }

    pub(crate) async fn start_config(config: &VmHandConfig) -> Result<Self, ManagedError> {
        validate_common_config(config)?;
        let machine = attachment_machine(config)?;
        let rootfs = config.rootfs.canonicalize().map_err(|error| {
            configuration(format!(
                "failed to resolve VM rootfs {}: {error}",
                config.rootfs.display()
            ))
        })?;
        let ext4 = rootfs.is_file();
        if !ext4 && !rootfs.is_dir() {
            return Err(configuration(format!(
                "VM rootfs is neither a raw ext4 image nor a directory: {}",
                rootfs.display()
            )));
        }
        let root_lock = ext4.then(|| lock_writable_rootfs(&rootfs)).transpose()?;
        let executable = std::env::current_exe()
            .map_err(|error| configuration(format!("failed to resolve VMM executable: {error}")))?;
        let mut builder = VmWorkspace::builder(&rootfs, executable)
            .vmm_argument("__vm-run-config")
            .vmm_argument("--config")
            .guest_workspace(&config.vm_workspace)
            .shell(&config.vm_shell)
            .cpus(config.vm_cpus)
            .memory_mib(config.vm_memory_mib);
        if ext4 {
            let runtime = prepare_guest_runtime(config)?;
            builder = builder.guest_runtime_disk(runtime.path().to_path_buf());
        } else if config.vm_guest_runtime.is_some() {
            return Err(configuration(
                "--vm-guest-runtime is only used with raw ext4 roots; directory roots must contain /usr/local/bin/nanocodex-vm-guest",
            ));
        }
        if config.vm_no_network {
            builder = builder.offline();
        }
        if let Some(firmware) = firmware_directory(config) {
            builder = builder.firmware_directory(firmware);
        }
        let workspace = builder.launch().await.map_err(|error| {
            configuration(format!(
                "failed to start VM hand and reach guest readiness: {error}"
            ))
        })?;
        let tools = match workspace.attachment_tools_builder().build() {
            Ok(tools) => tools,
            Err(error) => {
                let message = format!("failed to prepare VM hand tools: {error}");
                return match workspace.shutdown().await {
                    Ok(()) => Err(configuration(message)),
                    Err(shutdown) => Err(configuration(format!(
                        "{message}; VM shutdown also failed: {shutdown}"
                    ))),
                };
            }
        };
        Ok(Self {
            workspace,
            tools,
            machine,
            _root_lock: root_lock,
        })
    }

    /// Validates and prepares every host-wide input which does not depend on
    /// an allocation root. Running this before the control lease is acquired
    /// keeps deterministic guest/runtime failures out of the redrive loop.
    pub(crate) fn preflight_host_config(config: &VmHandConfig) -> Result<(), ManagedError> {
        validate_common_config(config)?;
        attachment_machine(config)?;
        prepare_guest_runtime(config)?;
        if let Some(firmware) = &config.vm_firmware {
            let firmware = firmware.canonicalize().map_err(|error| {
                configuration(format!(
                    "failed to resolve VM firmware directory {}: {error}",
                    firmware.display()
                ))
            })?;
            let library = if cfg!(target_os = "macos") {
                firmware.join("libkrunfw.5.dylib")
            } else {
                firmware.join("libkrunfw.so.5")
            };
            if !library.is_file() {
                return Err(configuration(format!(
                    "VM firmware library is missing: {}",
                    library.display()
                )));
            }
        }
        Ok(())
    }

    pub(crate) const fn machine(&self) -> &AttachmentMachine {
        &self.machine
    }

    pub(crate) fn tools(&self) -> Tools {
        self.tools.clone()
    }

    pub(crate) async fn shutdown(self) -> Result<(), ManagedError> {
        drop(self.tools);
        let started_at = Instant::now();
        loop {
            match self.workspace.shutdown().await {
                Ok(()) => return Ok(()),
                Err(VmWorkspaceError::Session(
                    VmToolSessionError::ActiveCapabilities(_)
                    | VmToolSessionError::ActiveRequests(_),
                )) if started_at.elapsed() < CAPABILITY_DRAIN_TIMEOUT => {
                    sleep(CAPABILITY_DRAIN_INTERVAL).await;
                }
                Err(error) => {
                    return Err(configuration(format!(
                        "failed to shut down VM hand: {error}"
                    )));
                }
            }
        }
    }
}

fn validate_common_config(config: &VmHandConfig) -> Result<(), ManagedError> {
    if !Path::new(&config.vm_workspace).is_absolute() {
        return Err(configuration(format!(
            "--vm-workspace must be an absolute guest path, got {:?}",
            config.vm_workspace
        )));
    }
    Ok(())
}

fn attachment_machine(config: &VmHandConfig) -> Result<AttachmentMachine, ManagedError> {
    let mut capabilities = vec![
        "filesystem".to_owned(),
        "linux".to_owned(),
        "process".to_owned(),
        "pty".to_owned(),
        "shell".to_owned(),
        "vm".to_owned(),
        format!("cpu:{}", config.vm_cpus),
        format!("memory-mib:{}", config.vm_memory_mib),
    ];
    if !config.vm_no_network {
        capabilities.push("network".to_owned());
    }
    capabilities.sort_unstable();
    AttachmentMachine::new(
        &config.machine_id,
        &config.machine_name,
        &config.vm_workspace,
        capabilities,
    )
    .map_err(|error| configuration(error.to_string()))
}

fn prepare_guest_runtime(config: &VmHandConfig) -> Result<GuestRuntimeDisk, ManagedError> {
    let runtime = config.vm_guest_runtime.as_ref().ok_or_else(|| {
        configuration(
            "raw ext4 VM roots require --vm-guest-runtime ELF; build it with `just build-vm-guest` or set NANOCODEX_VM_GUEST_RUNTIME",
        )
    })?;
    GuestRuntimeDisk::prepare(runtime, &config.vm_cache).map_err(|error| {
        configuration(format!(
            "failed to prepare the read-only VM guest runtime disk: {error}"
        ))
    })
}

pub(crate) fn run_config(path: &Path) -> Result<(), ManagedError> {
    let config = VmProcessConfig::read(path)
        .map_err(|error| configuration(format!("failed to read VM launch record: {error}")))?;
    config
        .run()
        .map_err(|error| configuration(format!("VM process failed: {error}")))
}

fn firmware_directory(config: &VmHandConfig) -> Option<PathBuf> {
    if let Some(directory) = &config.vm_firmware {
        return Some(directory.clone());
    }
    let directory = PathBuf::from(DEFAULT_KRUNFW_DIRECTORY);
    let library = if cfg!(target_os = "macos") {
        directory.join("libkrunfw.5.dylib")
    } else {
        directory.join("libkrunfw.so.5")
    };
    library.is_file().then_some(directory)
}

fn lock_writable_rootfs(path: &Path) -> Result<File, ManagedError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            configuration(format!(
                "failed to open writable VM rootfs {}: {error}",
                path.display()
            ))
        })?;
    file.try_lock_exclusive().map_err(|error| {
        configuration(format!(
            "VM rootfs is already in use ({}): {error}",
            path.display()
        ))
    })?;
    Ok(file)
}

fn configuration(message: impl Into<String>) -> ManagedError {
    ManagedError::Configuration(message.into())
}
