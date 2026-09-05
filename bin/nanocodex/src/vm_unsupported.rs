use std::path::PathBuf;

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Result, eyre};
use nanocodex::tools::ToolsBuilder;

/// VM arguments retained on builds whose host cannot run libkrun.
#[derive(Args, Default)]
pub(crate) struct VmArgs {
    /// Run workspace-mutating tools in this writable VM root filesystem.
    #[arg(long = "vm", visible_alias = "vm-rootfs", value_name = "ROOTFS")]
    rootfs: Option<PathBuf>,

    /// Statically linked Linux guest executable used with a raw ext4 root.
    #[arg(
        long,
        value_name = "ELF",
        env = "NANOCODEX_VM_GUEST_RUNTIME",
        requires = "rootfs"
    )]
    _vm_guest_runtime: Option<PathBuf>,

    /// Absolute working directory inside the VM.
    #[arg(
        long,
        value_name = "PATH",
        requires = "rootfs",
        value_parser = NonEmptyStringValueParser::new()
    )]
    _vm_workspace: Option<String>,

    /// Number of virtual CPUs assigned to the VM.
    #[arg(
        long,
        value_name = "COUNT",
        requires = "rootfs",
        value_parser = clap::value_parser!(u8).range(1..)
    )]
    _vm_cpus: Option<u8>,

    /// Guest memory in mebibytes.
    #[arg(
        long,
        value_name = "MIB",
        requires = "rootfs",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    _vm_memory_mib: Option<u32>,

    /// Shell name described to the model for the VM environment.
    #[arg(
        long,
        value_name = "SHELL",
        requires = "rootfs",
        value_parser = NonEmptyStringValueParser::new()
    )]
    _vm_shell: Option<String>,

    /// Disable guest internet socket proxying.
    #[arg(long, requires = "rootfs", action = ArgAction::SetTrue)]
    _vm_no_network: bool,
}

/// Uninhabited VM session placeholder for the shared application lifecycle.
pub(crate) struct ConfiguredVm {
    _private: (),
}

/// Placeholder for a provider-neutral VM route on unsupported hosts.
pub(crate) enum EgressLease {}

/// Private VMM entrypoint retained for command-line compatibility.
#[derive(Args)]
pub(crate) struct VmRunConfig {
    /// Mode-0600 launch record prepared by `nanocodex-vm`.
    #[arg(long)]
    config: PathBuf,
}

impl VmRunConfig {
    pub(crate) fn run(&self) -> Result<()> {
        Err(unsupported_error(Some(&self.config)))
    }
}

impl VmArgs {
    pub(crate) const fn is_enabled(&self) -> bool {
        self.rootfs.is_some()
    }

    pub(crate) async fn start(self, _egress: Option<EgressLease>) -> Result<Option<ConfiguredVm>> {
        match self.rootfs {
            Some(rootfs) => Err(unsupported_error(Some(&rootfs))),
            None => Ok(None),
        }
    }
}

impl ConfiguredVm {
    pub(crate) fn tools_builder(&self) -> ToolsBuilder {
        nanocodex::Tools::builder()
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        Ok(())
    }
}

fn unsupported_error(path: Option<&std::path::Path>) -> eyre::Report {
    let target = path.map_or_else(String::new, |path| format!(" ({})", path.display()));
    eyre!(
        "VM hosting is unsupported on {}; use Linux or Apple Silicon macOS{target}",
        std::env::consts::ARCH
    )
}
