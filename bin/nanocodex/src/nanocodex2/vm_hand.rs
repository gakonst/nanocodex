use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt as _;
use nanocodex_managed::ManagedError;
use nanocodex_tools::{
    Tools,
    attachment::{AttachmentMachine, AttachmentTarget},
};
use nanocodex_vm::{
    VmWorkspace, VmWorkspaceError,
    host::VmProcessConfig,
    tools::{GuestRuntimeDisk, VmCommand, VmCommandOutput, VmToolSessionError},
};
use tokio::time::sleep;

use super::Hand;
pub(crate) use super::vm_hand_config::VmHandConfig;

const DEFAULT_KRUNFW_DIRECTORY: &str = ".cache/libkrunfw/libkrunfw";
const FIRMWARE_LIBRARY: &str = if cfg!(target_os = "macos") {
    "libkrunfw.5.dylib"
} else {
    "libkrunfw.so.5"
};
const CAPABILITY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const CAPABILITY_DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const DESKTOP_CREDENTIAL: &str = "/run/nanocodex-remote/credential";
const DESKTOP_EXECUTABLE: &str = "/usr/local/bin/nanocodex-remote";

struct VmDesktop {
    task: Option<tokio::task::JoinHandle<Result<VmCommandOutput, VmToolSessionError>>>,
}

impl Drop for VmDesktop {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(crate) struct VmHand {
    workspace: VmWorkspace,
    tools: Tools,
    machine: AttachmentMachine,
    _root_lock: Option<File>,
    desktop: Option<VmDesktop>,
}

impl VmHand {
    pub(crate) async fn start(config: &Hand) -> Result<Self, ManagedError> {
        Self::start_config(&VmHandConfig::from(config)).await
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
            desktop: None,
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
            let library = firmware.join(FIRMWARE_LIBRARY);
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

    /// Images containing the companion opt into an owned interactive desktop.
    /// Existing shell-only images retain their previous startup contract.
    pub(crate) async fn start_desktop(
        &mut self,
        target: &AttachmentTarget,
    ) -> Result<(), ManagedError> {
        let control = self.workspace.control();
        let present = control
            .command(
                VmCommand::new("/bin/sh")
                    .arg("-c")
                    .arg("test -x /usr/local/bin/nanocodex-remote")
                    .timeout(Duration::from_secs(5)),
            )
            .await
            .map_err(|_| configuration("failed to inspect VM desktop image"))?;
        if present.exit_code != 0 {
            return Ok(());
        }
        let mut endpoint = target.endpoint().clone();
        let path = endpoint
            .path()
            .strip_suffix("/tool-host")
            .filter(|path| path.starts_with("/v1/vm-host-attachments/"))
            .ok_or_else(|| configuration("VM desktop requires an allocation attachment"))?;
        let path = format!("{path}/hands");
        let scheme = if endpoint.scheme() == "wss" {
            "https"
        } else {
            "http"
        };
        endpoint
            .set_scheme(scheme)
            .map_err(|()| configuration("invalid VM desktop endpoint"))?;
        endpoint.set_path(&path);
        control
            .create_directory("/run/nanocodex-remote", 0o700, None)
            .await
            .map_err(|_| configuration("failed to prepare private VM desktop directory"))?;
        control
            .write_file(
                DESKTOP_CREDENTIAL,
                target.bearer().as_bytes().to_vec(),
                0o600,
            )
            .await
            .map_err(|_| configuration("failed to deliver VM desktop credential"))?;
        let clean = control
            .command(
                VmCommand::new("/bin/rm")
                    .arg("-f")
                    .arg(format!("{DESKTOP_CREDENTIAL}.ready"))
                    .timeout(Duration::from_secs(5)),
            )
            .await
            .map_err(|_| configuration("failed to clear VM desktop readiness"))?;
        if clean.exit_code != 0 {
            return Err(configuration("failed to clear VM desktop readiness"));
        }
        let command = VmCommand::new(DESKTOP_EXECUTABLE)
            .arg("desktop-host")
            .arg("--url")
            .arg(endpoint.to_string())
            .arg("--credential-file")
            .arg(DESKTOP_CREDENTIAL)
            .arg("--machine-id")
            .arg(self.machine.id())
            .arg("--name")
            .arg(self.machine.name())
            .arg("--workspace")
            .arg(self.workspace.guest_workspace())
            .timeout(Duration::from_secs(365 * 24 * 60 * 60))
            .max_output_bytes(64 * 1024);
        let runner = self.workspace.control();
        self.desktop = Some(VmDesktop {
            task: Some(tokio::spawn(async move { runner.command(command).await })),
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self
                .desktop
                .as_ref()
                .and_then(|desktop| desktop.task.as_ref())
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                return Err(configuration("VM desktop exited before readiness"));
            }
            if control
                .read_file(format!("{DESKTOP_CREDENTIAL}.ready"))
                .await
                .is_ok_and(|value| value == b"ready\n")
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(configuration(
                    "VM desktop compositor did not become ready within 30 seconds",
                ));
            }
            sleep(Duration::from_millis(100)).await;
        }
    }

    pub(crate) async fn refresh_desktop(
        &self,
        target: &AttachmentTarget,
    ) -> Result<(), ManagedError> {
        if self.desktop.is_some() {
            self.workspace
                .control()
                .write_file(
                    DESKTOP_CREDENTIAL,
                    target.bearer().as_bytes().to_vec(),
                    0o600,
                )
                .await
                .map_err(|_| configuration("failed to refresh VM desktop credential"))?;
        }
        Ok(())
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), ManagedError> {
        if let Some(mut desktop) = self.desktop.take() {
            let _ = self
                .workspace
                .control()
                .write_file(DESKTOP_CREDENTIAL, Vec::new(), 0o600)
                .await;
            if let Some(mut task) = desktop.task.take()
                && tokio::time::timeout(Duration::from_secs(5), &mut task)
                    .await
                    .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
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
    // Installed VM assets travel together. Resolve relative to the guest
    // runtime, not the app's working directory or its signed helper cache.
    config
        .vm_guest_runtime
        .as_deref()
        .and_then(bundled_firmware_directory)
        .or_else(|| {
            let directory = PathBuf::from(DEFAULT_KRUNFW_DIRECTORY);
            directory
                .join(FIRMWARE_LIBRARY)
                .is_file()
                .then_some(directory)
        })
}

fn bundled_firmware_directory(runtime: &Path) -> Option<PathBuf> {
    let directory = runtime.parent()?.join("firmware");
    directory
        .join(FIRMWARE_LIBRARY)
        .is_file()
        .then_some(directory)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_guest_assets_resolve_firmware_without_a_working_directory() {
        let assets = tempfile::tempdir().unwrap();
        let runtime = assets.path().join("nanocodex-vm-guest");
        assert_eq!(bundled_firmware_directory(&runtime), None);
        let firmware = assets.path().join("firmware");
        std::fs::create_dir(&firmware).unwrap();
        std::fs::write(firmware.join(FIRMWARE_LIBRARY), b"firmware fixture").unwrap();
        assert_eq!(bundled_firmware_directory(&runtime), Some(firmware));
    }
}
