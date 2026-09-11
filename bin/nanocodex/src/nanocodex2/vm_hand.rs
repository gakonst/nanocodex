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
    VmWorkspace,
    docker::DockerWorkspace,
    host::{Capabilities, Gpu, KrunFeature, VmProcessConfig},
    tools::{GuestRuntimeDisk, VmCommand, VmCommandOutput, VmToolSessionError},
};
use tokio::time::sleep;

pub(crate) use super::vm_hand_config::VmHandConfig;
use super::{Hand, hand_workspace::HandWorkspace};

const DEFAULT_KRUNFW_DIRECTORY: &str = ".cache/libkrunfw/libkrunfw";
const FIRMWARE_LIBRARY: &str = if cfg!(target_os = "macos") {
    "libkrunfw.5.dylib"
} else {
    "libkrunfw.so.5"
};
const CAPABILITY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const CAPABILITY_DRAIN_INTERVAL: Duration = Duration::from_millis(10);
const DESKTOP_RUNTIME: &str = "/run/nanocodex-hand-desktop";

struct VmDesktop {
    publisher: Option<super::screen_publisher::ScreenPublisher>,
    executable: String,
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
    workspace: HandWorkspace,
    tools: Tools,
    machine: AttachmentMachine,
    _root_lock: Option<File>,
    desktop: Option<VmDesktop>,
}

impl VmHand {
    pub(crate) async fn preflight(config: &Hand) -> Result<(), ManagedError> {
        let config = VmHandConfig::from(config);
        validate_common_config(&config)?;
        attachment_machine(&config)?;
        if let Some(docker) = &config.docker {
            let mut builder = DockerWorkspace::builder(&docker.image, &docker.volume)
                .guest_workspace(&config.vm_workspace)
                .cpus(config.vm_cpus)
                .memory_mib(config.vm_memory_mib);
            if let Some(runtime) = &docker.runtime {
                builder = builder.runtime(runtime);
            }
            builder
                .preflight()
                .await
                .map_err(|error| configuration(error.to_string()))?;
        } else {
            let root = config.rootfs.metadata().map_err(|error| configuration(format!(
                "VM root {} is unavailable: {error}; --vm must point to an existing writable ext4 image or development root directory", config.rootfs.display()
            )))?;
            if root.is_file() && config.vm_guest_runtime.is_none() {
                return Err(configuration(
                    "an ext4 VM requires --guest-runtime ELF or NANOCODEX_VM_GUEST_RUNTIME; build the guest with `just build-vm-guest`",
                ));
            }
        }
        Ok(())
    }

    pub(crate) async fn start(config: &Hand) -> Result<Self, ManagedError> {
        Self::start_config(&VmHandConfig::from(config)).await
    }

    pub(crate) async fn start_config(config: &VmHandConfig) -> Result<Self, ManagedError> {
        validate_common_config(config)?;
        let machine = attachment_machine(config)?;
        let (workspace, root_lock) = if let Some(docker) = &config.docker {
            let mut builder = DockerWorkspace::builder(&docker.image, &docker.volume)
                .guest_workspace(&config.vm_workspace)
                .shell(&config.vm_shell)
                .cpus(config.vm_cpus)
                .memory_mib(config.vm_memory_mib);
            if docker.internet {
                builder = builder.internet();
            }
            if let Some(runtime) = &docker.runtime {
                builder = builder.runtime(runtime);
            }
            let workspace = builder
                .launch()
                .await
                .map_err(|error| configuration(error.to_string()))?;
            (HandWorkspace::Docker(workspace), None)
        } else {
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
            let executable = std::env::current_exe().map_err(|error| {
                configuration(format!("failed to resolve VMM executable: {error}"))
            })?;
            let mut builder = VmWorkspace::builder(&rootfs, executable)
                .vmm_argument("__vm-run-config")
                .vmm_argument("--config")
                .guest_workspace(&config.vm_workspace)
                .shell(&config.vm_shell)
                .cpus(config.vm_cpus)
                .memory_mib(config.vm_memory_mib)
                .gpu(if config.vm_gpu {
                    Gpu::Vulkan
                } else {
                    Gpu::Disabled
                });
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
            (HandWorkspace::Vm(workspace), root_lock)
        };
        let tools = match workspace.attachment_tools_builder().build() {
            Ok(tools) => tools,
            Err(error) => {
                let message = format!("failed to prepare Hand tools: {error}");
                return match workspace.shutdown().await {
                    Ok(()) => Err(configuration(message)),
                    Err(shutdown) => Err(configuration(format!(
                        "{message}; Hand shutdown also failed: {shutdown}"
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

    /// The host owns signaling; the Rust guest runtime owns its desktop.
    /// Account and allocation credentials never enter the guest filesystem.
    pub(crate) async fn start_desktop(
        &mut self,
        target: &AttachmentTarget,
    ) -> Result<(), ManagedError> {
        use super::screen_publisher::{ScreenBackend, ScreenPublisher};
        use std::sync::Arc;
        let control = self.workspace.control();
        let present = control
            .command(
                VmCommand::new("/bin/sh")
                    .arg("-c")
                    .arg("command -v Xvfb >/dev/null && command -v xterm >/dev/null")
                    .timeout(Duration::from_secs(5)),
            )
            .await
            .map_err(|_| configuration("failed to inspect VM desktop image"))?;
        if present.exit_code != 0 {
            tracing::info!(target: "nanocodex2", stage = "vm.screen.unavailable",
                "VM image has no desktop; install Xvfb, openbox, xterm, and fonts");
            return Ok(());
        }
        let runtime = control.command(VmCommand::new("/bin/sh").arg("-c")
            .arg("for p in /run/nanocodex/nanocodex-vm-guest /nanocodex-vm-guest /usr/local/bin/nanocodex-vm-guest; do if test -x \"$p\"; then printf '%s' \"$p\"; exit 0; fi; done; exit 1")
            .timeout(Duration::from_secs(5))).await.map_err(|_| configuration("failed to resolve Rust guest runtime"))?;
        if runtime.exit_code != 0 {
            return Err(configuration(
                "Rust guest runtime is unavailable for desktop startup",
            ));
        }
        let executable = String::from_utf8(runtime.stdout)
            .map_err(|_| configuration("invalid guest runtime path"))?;
        let runner = self.workspace.control();
        let command = VmCommand::new(&executable)
            .arg("--desktop")
            .arg(self.workspace.guest_workspace())
            .arg(DESKTOP_RUNTIME)
            .timeout(Duration::from_secs(365 * 24 * 60 * 60))
            .max_output_bytes(64 * 1024);
        self.desktop = Some(VmDesktop {
            publisher: None,
            executable: executable.clone(),
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
                let task = self
                    .desktop
                    .as_mut()
                    .and_then(|desktop| desktop.task.take())
                    .expect("finished desktop task");
                let detail = match task.await {
                    Ok(Ok(output)) => String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    _ => "guest command failed".to_owned(),
                };
                return Err(configuration(format!(
                    "Rust VM desktop exited before readiness: {detail}"
                )));
            }
            if control
                .read_file(format!("{DESKTOP_RUNTIME}/ready"))
                .await
                .is_ok()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(configuration(
                    "VM desktop did not become ready within 30 seconds",
                ));
            }
            sleep(Duration::from_millis(100)).await;
        }
        let runner = self.workspace.control();
        let backend: ScreenBackend = Arc::new(move |input| {
            let control = runner.clone();
            let executable = executable.clone();
            Box::pin(async move {
                let output = control
                    .command(
                        VmCommand::new(executable)
                            .arg("--desktop-request")
                            .arg(input.to_string())
                            .arg(DESKTOP_RUNTIME)
                            .timeout(Duration::from_secs(8))
                            .max_output_bytes(750_000),
                    )
                    .await
                    .map_err(|_| configuration("VM desktop request failed"))?;
                if output.exit_code != 0 {
                    return Err(configuration("VM desktop request was rejected"));
                }
                serde_json::from_slice(&output.stdout)
                    .map_err(|_| configuration("invalid VM desktop result"))
            })
        });
        let publisher = ScreenPublisher::start(target, &self.machine, backend).await?;
        self.desktop.as_mut().expect("desktop started").publisher = Some(publisher);
        tracing::info!(target: "nanocodex2", stage = "vm.screen.ready", "Rust VM screen is published");
        Ok(())
    }

    pub(crate) async fn refresh_desktop(
        &self,
        target: &AttachmentTarget,
    ) -> Result<(), ManagedError> {
        if let Some(publisher) = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.publisher.as_ref())
        {
            publisher.refresh(target).await?;
        }
        Ok(())
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), ManagedError> {
        if let Some(mut desktop) = self.desktop.take() {
            if let Some(publisher) = desktop.publisher.take() {
                let _ = publisher.shutdown().await;
            }
            let _ = self
                .workspace
                .control()
                .command(
                    VmCommand::new(&desktop.executable)
                        .arg("--desktop-request")
                        .arg(r#"{"action":"shutdown"}"#)
                        .arg(DESKTOP_RUNTIME)
                        .timeout(Duration::from_secs(5)),
                )
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
                Err(error)
                    if error.is_busy() && started_at.elapsed() < CAPABILITY_DRAIN_TIMEOUT =>
                {
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
    if config.docker.is_some() && config.vm_gpu {
        return Err(configuration(
            "--gpu is supported only with --vm; Docker Hands use software rendering",
        ));
    }
    if config.vm_gpu
        && !Capabilities::detect()
            .map_err(|error| configuration(error.to_string()))?
            .has(KrunFeature::Gpu)
    {
        return Err(configuration(
            "--gpu requires a host built with nanocodex-vm/gpu and a Vulkan renderer",
        ));
    }
    if !Path::new(&config.vm_workspace).is_absolute() {
        return Err(configuration(format!(
            "--workspace must be an absolute guest path, got {:?}",
            config.vm_workspace
        )));
    }
    #[cfg(target_os = "linux")]
    if config.docker.is_none() {
        preflight_kvm_device(Path::new("/dev/kvm"))?;
        let kvm = kvm_ioctls::Kvm::new().map_err(|error| unavailable_kvm(error.to_string()))?;
        if kvm.get_api_version() != 12 {
            return Err(unavailable_kvm("unsupported KVM API version".into()));
        }
        kvm.create_vm()
            .map_err(|error| unavailable_kvm(error.to_string()))?;
    }
    Ok(())
}

// Check before the host advertises a factory or prepares a guest disk. A Linux
// Hand can run ordinary processes inside a container without being able to host
// VMs; it needs a passed-through, accessible KVM character device as well.
#[cfg(any(target_os = "linux", test))]
fn unavailable_kvm(detail: String) -> ManagedError {
    configuration(format!(
        "VM backend is unavailable: Linux VM hosting requires working KVM at /dev/kvm: {detail}. Enable hardware/nested virtualization and grant this user read/write access to /dev/kvm. Alternatively, use `hand --docker IMAGE --volume NAME` with a Linux Docker daemon. No fallback was attempted"
    ))
}

#[cfg(any(target_os = "linux", test))]
fn preflight_kvm_device(path: &Path) -> Result<(), ManagedError> {
    use std::os::unix::fs::FileTypeExt as _;

    let unavailable = |detail: String| unavailable_kvm(format!("{}: {detail}", path.display()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| unavailable(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| unavailable(error.to_string()))?;
    if !metadata.file_type().is_char_device() {
        return Err(unavailable("the path is not a character device".to_owned()));
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
        if config.docker.is_some() {
            "container"
        } else {
            "vm"
        }
        .to_owned(),
        format!("cpu:{}", config.vm_cpus),
        format!("memory-mib:{}", config.vm_memory_mib),
    ];
    if config
        .docker
        .as_ref()
        .map_or(!config.vm_no_network, |docker| docker.internet)
    {
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

pub(crate) fn clone_image(source: &Path, destination: &Path) -> Result<(), ManagedError> {
    nanocodex_vm::image::reflink_or_sparse_copy(source, destination)
        .map(|_| ())
        .map_err(|error| configuration(format!("failed to create private VM disk: {error}")))
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
    fn docker_hand_skips_kvm_and_advertises_container_isolation() {
        let config = VmHandConfig {
            rootfs: PathBuf::new(),
            docker: Some(super::super::vm_hand_config::DockerHandConfig {
                image: "image".into(),
                volume: "workspace".into(),
                internet: false,
                runtime: None,
            }),
            vm_guest_runtime: None,
            vm_cache: PathBuf::new(),
            vm_firmware: None,
            vm_workspace: "/app".into(),
            vm_cpus: 2,
            vm_memory_mib: 1024,
            vm_gpu: false,
            vm_shell: "sh".into(),
            vm_no_network: false,
            machine_id: "docker-hand".into(),
            machine_name: "Docker Hand".into(),
        };
        validate_common_config(&config).unwrap();
        let machine = serde_json::to_value(attachment_machine(&config).unwrap()).unwrap();
        let capabilities = machine["capabilities"].as_array().unwrap();
        assert!(capabilities.iter().any(|value| value == "container"));
        assert!(
            !capabilities
                .iter()
                .any(|value| value == "vm" || value == "network")
        );
        let mut internet = config;
        internet.docker.as_mut().unwrap().internet = true;
        let machine = serde_json::to_value(attachment_machine(&internet).unwrap()).unwrap();
        assert!(
            machine["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "network")
        );
    }

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

    #[test]
    fn kvm_preflight_rejects_missing_devices_without_creating_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kvm");
        let error = preflight_kvm_device(&path).unwrap_err().to_string();
        assert!(error.contains("Linux VM hosting requires"));
        assert!(error.contains("nested virtualization"));
        assert!(!path.exists());
    }

    #[test]
    fn kvm_preflight_rejects_regular_files_without_modifying_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("kvm");
        std::fs::write(&path, b"not a hypervisor").unwrap();
        let error = preflight_kvm_device(&path).unwrap_err().to_string();
        assert!(error.contains("not a character device"));
        assert_eq!(std::fs::read(&path).unwrap(), b"not a hypervisor");
    }

    #[test]
    fn kvm_preflight_accepts_an_accessible_character_device() {
        // This checks device access only. The VMM still verifies the KVM API
        // when opening the real /dev/kvm during launch.
        preflight_kvm_device(Path::new("/dev/null")).unwrap();
    }
}
