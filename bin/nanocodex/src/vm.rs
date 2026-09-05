use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
use fs2::FileExt as _;
use nanocodex::tools::ToolsBuilder;
use nanocodex_vm::{
    VmWorkspace, VmWorkspaceError,
    host::VmProcessConfig,
    tools::{GuestRuntimeDisk, VmToolSessionError},
};
use tokio::time::sleep;
use tracing::info;

pub(crate) use nanocodex_vm::host::EgressLease;

const DEFAULT_CPUS: u8 = 2;
const DEFAULT_MEMORY_MIB: u32 = 1_024;
const DEFAULT_EXT4_WORKSPACE: &str = "/app";
const DEFAULT_DIRECTORY_WORKSPACE: &str = "/workspace";
const DEFAULT_SHELL: &str = "sh";
const DEFAULT_KRUNFW_DIRECTORY: &str = ".cache/libkrunfw/libkrunfw";
const DEFAULT_VM_CACHE: &str = ".cache/vm";
const CAPABILITY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const CAPABILITY_DRAIN_INTERVAL: Duration = Duration::from_millis(10);

/// Opt-in VM workspace-tool configuration for normal agent sessions.
#[derive(Args, Default)]
pub(crate) struct VmArgs {
    /// Run workspace-mutating tools in this writable VM root filesystem.
    ///
    /// The path may be a raw ext4 image or a directory rootfs. The selected
    /// root is modified in place for the lifetime of the agent session.
    #[arg(long = "vm", visible_alias = "vm-rootfs", value_name = "ROOTFS")]
    rootfs: Option<PathBuf>,

    /// Statically linked Linux guest executable used with a raw ext4 root.
    #[arg(
        long,
        value_name = "ELF",
        env = "NANOCODEX_VM_GUEST_RUNTIME",
        requires = "rootfs"
    )]
    vm_guest_runtime: Option<PathBuf>,

    /// Absolute working directory inside the VM.
    #[arg(
        long,
        value_name = "PATH",
        requires = "rootfs",
        value_parser = NonEmptyStringValueParser::new()
    )]
    vm_workspace: Option<String>,

    /// Number of virtual CPUs assigned to the VM.
    #[arg(
        long,
        value_name = "COUNT",
        requires = "rootfs",
        value_parser = clap::value_parser!(u8).range(1..)
    )]
    vm_cpus: Option<u8>,

    /// Guest memory in mebibytes.
    #[arg(
        long,
        value_name = "MIB",
        requires = "rootfs",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    vm_memory_mib: Option<u32>,

    /// Shell name described to the model for the VM environment.
    #[arg(
        long,
        value_name = "SHELL",
        requires = "rootfs",
        value_parser = NonEmptyStringValueParser::new()
    )]
    vm_shell: Option<String>,

    /// Disable guest internet socket proxying.
    #[arg(long, requires = "rootfs", action = ArgAction::SetTrue)]
    vm_no_network: bool,
}

pub(crate) struct ConfiguredVm {
    session: VmWorkspace,
    _root_lock: Option<File>,
}

/// Private synchronous entrypoint used by a spawned VMM child.
#[derive(Args)]
pub(crate) struct VmRunConfig {
    /// Mode-0600 launch record prepared by `nanocodex-vm`.
    #[arg(long)]
    config: PathBuf,
}

impl VmRunConfig {
    pub(crate) fn run(&self) -> Result<()> {
        VmProcessConfig::read(&self.config)?.run()?;
        Ok(())
    }
}

impl VmArgs {
    pub(crate) const fn is_enabled(&self) -> bool {
        self.rootfs.is_some()
    }

    pub(crate) async fn start(self, egress: Option<EgressLease>) -> Result<Option<ConfiguredVm>> {
        let Some(rootfs) = self.rootfs else {
            return Ok(None);
        };
        if self.vm_no_network && egress.is_some() {
            return Err(eyre!(
                "--vm-no-network cannot be combined with provider-backed VM egress"
            ));
        }
        let rootfs = rootfs
            .canonicalize()
            .wrap_err_with(|| format!("failed to resolve VM rootfs {}", rootfs.display()))?;
        let ext4 = rootfs.is_file();
        if !ext4 && !rootfs.is_dir() {
            return Err(eyre!(
                "VM rootfs is neither a raw ext4 image nor a directory: {}",
                rootfs.display()
            ));
        }
        let workspace = self.vm_workspace.unwrap_or_else(|| {
            if ext4 {
                DEFAULT_EXT4_WORKSPACE.to_owned()
            } else {
                DEFAULT_DIRECTORY_WORKSPACE.to_owned()
            }
        });
        if !Path::new(&workspace).is_absolute() {
            return Err(eyre!(
                "--vm-workspace must be an absolute guest path, got {workspace:?}"
            ));
        }
        let root_lock = ext4.then(|| lock_writable_rootfs(&rootfs)).transpose()?;
        let executable =
            std::env::current_exe().wrap_err("failed to resolve the VMM executable")?;
        let mut vm = VmWorkspace::builder(&rootfs, executable)
            .vmm_argument("vm-run-config")
            .vmm_argument("--config")
            .guest_workspace(&workspace)
            .shell(
                self.vm_shell
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SHELL.to_owned()),
            )
            .cpus(self.vm_cpus.unwrap_or(DEFAULT_CPUS))
            .memory_mib(self.vm_memory_mib.unwrap_or(DEFAULT_MEMORY_MIB));
        if ext4 {
            let runtime = self.vm_guest_runtime.ok_or_else(|| {
                eyre!(
                    "raw ext4 VM roots require --vm-guest-runtime ELF; build it with \
                     `just build-vm-guest` or set NANOCODEX_VM_GUEST_RUNTIME"
                )
            })?;
            let runtime = GuestRuntimeDisk::prepare(runtime, DEFAULT_VM_CACHE)
                .wrap_err("failed to prepare the read-only VM guest runtime disk")?;
            vm = vm.guest_runtime_disk(runtime.path().to_path_buf());
        } else if self.vm_guest_runtime.is_some() {
            return Err(eyre!(
                "--vm-guest-runtime is only used with raw ext4 roots; directory roots must \
                 contain /usr/local/bin/nanocodex-vm-guest"
            ));
        }
        let network = if self.vm_no_network {
            vm = vm.offline();
            "disabled"
        } else if let Some(egress) = egress {
            vm = vm.egress(egress);
            "provider_proxy"
        } else {
            "internet"
        };
        let firmware = Path::new(DEFAULT_KRUNFW_DIRECTORY);
        let platform_firmware = if cfg!(target_os = "macos") {
            firmware.join("libkrunfw.5.dylib")
        } else {
            firmware.join("libkrunfw.so.5")
        };
        if platform_firmware.is_file() {
            vm = vm.firmware_directory(firmware);
        }
        let session = vm
            .launch()
            .await
            .wrap_err("failed to start tool VM and reach guest readiness")?;
        info!(
            target: "nanocodex_vm",
            vm_rootfs = %rootfs.display(),
            vm_workspace = workspace,
            vm_cpu_count = self.vm_cpus.unwrap_or(DEFAULT_CPUS),
            vm_memory_mib = self.vm_memory_mib.unwrap_or(DEFAULT_MEMORY_MIB),
            vm_network = network,
            "normal agent workspace tools are running in a VM"
        );
        Ok(Some(ConfiguredVm {
            session,
            _root_lock: root_lock,
        }))
    }
}

impl ConfiguredVm {
    pub(crate) fn tools_builder(&self) -> ToolsBuilder {
        self.session.tools_builder()
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        let started_at = Instant::now();
        loop {
            match self.session.shutdown().await {
                Ok(()) => return Ok(()),
                Err(VmWorkspaceError::Session(
                    VmToolSessionError::ActiveCapabilities(_)
                    | VmToolSessionError::ActiveRequests(_),
                )) if started_at.elapsed() < CAPABILITY_DRAIN_TIMEOUT => {
                    sleep(CAPABILITY_DRAIN_INTERVAL).await;
                }
                Err(error) => return Err(error).wrap_err("failed to shut down the tool VM"),
            }
        }
    }
}

fn lock_writable_rootfs(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("failed to open writable VM rootfs {}", path.display()))?;
    file.try_lock_exclusive()
        .wrap_err_with(|| format!("VM rootfs is already in use: {}", path.display()))?;
    Ok(file)
}
