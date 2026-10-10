//! Command-line definitions of Hand serving and the VM host. The CLI shows
//! them in its help and forwards these commands to the Hand executable, which
//! parses and serves them.
use std::path::PathBuf;

use clap::{Args, ValueEnum};
use nanocodex_managed::{ManagedError, validate_vm_factory_name};

use crate::hand_observability::HandObservabilityArgs;

/// Bearer of the system-scope VM host registration.
pub const SYSTEM_HOST_TOKEN_ENV: &str = "NANOCODEX_SYSTEM_HOST_TOKEN";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum HandNetwork {
    Off,
    Internet,
}

#[derive(Args)]
#[command(
    group(clap::ArgGroup::new("backend").args(["rootfs", "docker"])),
    after_help = "Without a subcommand, serve this computer as a Hand. Use --vm or --docker for an isolated Hand.\n\nExamples:\n  nanocodex hand --docker nanocodex-hand:local --volume my-workspace\n  nanocodex hand --vm root.ext4 --guest-runtime /path/to/nanocodex-vm-guest\n\nUse --network internet to give a Docker Hand internet access."
)]
pub struct HandServe {
    /// Private identity directory for an explicitly selected native workspace.
    #[arg(long, conflicts_with_all = ["rootfs", "docker"])]
    pub state_dir: Option<PathBuf>,
    /// VM with a persistent ext4 root (Linux KVM or Apple Silicon Hypervisor.framework).
    #[arg(
        long = "vm",
        alias = "vm-rootfs",
        value_name = "ROOTFS",
        help_heading = "Backend"
    )]
    pub rootfs: Option<PathBuf>,

    /// Container using an existing Linux Docker image; no KVM required.
    #[arg(long, value_name = "IMAGE", requires = "docker_volume", conflicts_with_all = ["vm_guest_runtime", "vm_firmware", "vm_gpu"], help_heading = "Backend")]
    pub docker: Option<String>,

    /// Persistent named Docker workspace volume (required with --docker).
    #[arg(
        long = "volume",
        alias = "docker-volume",
        value_name = "VOLUME",
        requires = "docker",
        help_heading = "Workspace"
    )]
    pub docker_volume: Option<String>,

    /// Guest network access [default: off for Docker, internet for VM].
    #[arg(long, value_enum, conflicts_with_all = ["docker_internet", "vm_no_network"], help_heading = "Workspace")]
    pub network: Option<HandNetwork>,

    #[arg(
        long,
        hide = true,
        requires = "docker",
        conflicts_with = "vm_no_network"
    )]
    pub docker_internet: bool,

    #[arg(long, hide = true, requires = "rootfs")]
    pub vm_no_network: bool,

    /// Absolute workspace directory inside the Hand.
    #[arg(
        long = "workspace",
        alias = "vm-workspace",
        value_name = "PATH",
        help_heading = "Workspace"
    )]
    pub vm_workspace: Option<String>,

    /// CPU limit.
    #[arg(long = "cpus", alias = "vm-cpus", value_name = "COUNT", default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..), help_heading = "Resources")]
    pub vm_cpus: u8,

    /// Memory limit in MiB.
    #[arg(long = "memory", alias = "vm-memory-mib", value_name = "MIB", default_value_t = 1_024, value_parser = clap::value_parser!(u32).range(1..), help_heading = "Resources")]
    pub vm_memory_mib: u32,

    /// Share the host GPU with a VM; requires a GPU-enabled build and Vulkan renderer.
    #[arg(
        long = "gpu",
        alias = "vm-gpu",
        requires = "rootfs",
        help_heading = "Resources"
    )]
    pub vm_gpu: bool,

    /// Stable account-local identifier [default: docker or vm, matching the backend].
    #[arg(long, help_heading = "Identity")]
    pub machine_id: Option<String>,

    /// Display name [default: Nanocodex Docker Hand or Nanocodex VM].
    #[arg(long, help_heading = "Identity")]
    pub machine_name: Option<String>,

    /// VM factory hosted by this native computer (managed separately, e.g. systemd).
    #[arg(long, conflicts_with_all = ["rootfs", "docker"], help_heading = "Identity")]
    pub vm_provider: Option<String>,

    /// Legacy option (disabled); use the Hand's CUA tools for browser interactions.
    #[arg(long, help_heading = "Browser")]
    pub browser: bool,

    /// Legacy browser executable option (disabled); use the Hand's CUA tools.
    #[arg(
        long,
        value_name = "PATH",
        env = "NANOCODEX_BROWSER_EXECUTABLE",
        requires = "browser",
        help_heading = "Browser"
    )]
    pub browser_executable: Option<PathBuf>,

    /// Static Linux guest executable for an ext4 VM (or NANOCODEX_VM_GUEST_RUNTIME).
    #[arg(
        long = "guest-runtime",
        alias = "vm-guest-runtime",
        value_name = "ELF",
        requires = "rootfs",
        help_heading = "VM setup"
    )]
    pub vm_guest_runtime: Option<PathBuf>,

    /// Installed Docker OCI runtime, e.g. runsc; fails if unavailable.
    #[arg(
        long = "runtime",
        alias = "docker-runtime",
        value_name = "RUNTIME",
        requires = "docker",
        help_heading = "Advanced"
    )]
    pub docker_runtime: Option<String>,

    /// Prepared VM guest disk cache.
    #[arg(
        long = "cache",
        alias = "vm-cache",
        value_name = "PATH",
        default_value = ".cache/vm",
        requires = "rootfs",
        help_heading = "Advanced"
    )]
    pub vm_cache: PathBuf,

    /// libkrun firmware directory (or NANOCODEX_KRUNFW_DIR).
    #[arg(
        long = "firmware",
        alias = "vm-firmware",
        value_name = "PATH",
        requires = "rootfs",
        help_heading = "Advanced"
    )]
    pub vm_firmware: Option<PathBuf>,

    /// Shell described to the managed brain.
    #[arg(
        long = "shell",
        alias = "vm-shell",
        value_name = "SHELL",
        default_value = "sh",
        help_heading = "Advanced"
    )]
    pub vm_shell: String,

    #[command(flatten, next_help_heading = "Logging")]
    pub observability: HandObservabilityArgs,
}

impl HandServe {
    pub fn machine_id(&self) -> &str {
        self.machine_id
            .as_deref()
            .unwrap_or(if self.docker.is_some() {
                "docker"
            } else {
                "vm"
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum HostScope {
    User,
    Agent,
    System,
}

#[derive(Args)]
pub struct Host {
    #[command(flatten)]
    pub observability: HandObservabilityArgs,

    /// Authority scope that may provision VMs from this host.
    #[arg(long, value_enum, default_value_t = HostScope::User)]
    pub scope: HostScope,

    /// Exact /mount provider selector, unique within the selected scope.
    #[arg(long, value_name = "FACTORY_NAME")]
    pub factory_name: String,

    /// Managed agent ID. Required only with --scope agent.
    #[arg(long, value_name = "AGENT_ID", required_if_eq("scope", "agent"))]
    pub agent: Option<String>,

    /// Immutable raw ext4 image cloned privately for every allocation.
    #[arg(long, value_name = "ROOTFS")]
    pub vm_template: PathBuf,

    /// Durable private host state and per-allocation VM roots.
    #[arg(long, value_name = "PATH")]
    pub state_dir: PathBuf,

    /// Maximum number of provisioning, live, or releasing VMs.
    #[arg(long, value_name = "COUNT", default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
    pub max_vms: u16,

    /// Keep one never-assigned VM ready within --max-vms capacity.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub warm_spare: bool,

    /// Stable host UUID. Generated and persisted under --state-dir when omitted.
    #[arg(long, value_name = "UUID")]
    pub host_id: Option<uuid::Uuid>,

    /// Statically linked Linux guest executable used with the raw ext4 roots.
    #[arg(long, value_name = "ELF", env = "NANOCODEX_VM_GUEST_RUNTIME")]
    pub vm_guest_runtime: PathBuf,

    /// Cache for the prepared read-only guest runtime disk.
    #[arg(long, value_name = "PATH", default_value = ".cache/vm")]
    pub vm_cache: PathBuf,

    /// Directory containing the platform libkrun firmware library.
    #[arg(long, value_name = "PATH", env = "NANOCODEX_KRUNFW_DIR")]
    pub vm_firmware: Option<PathBuf>,

    /// Absolute working directory inside every provisioned VM.
    #[arg(long, value_name = "PATH", default_value = "/app")]
    pub vm_workspace: String,

    /// Number of virtual CPUs assigned to each VM.
    #[arg(long, value_name = "COUNT", default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub vm_cpus: u8,

    /// Guest memory in mebibytes assigned to each VM.
    #[arg(long, value_name = "MIB", default_value_t = 1_024, value_parser = clap::value_parser!(u32).range(128..=262_144))]
    pub vm_memory_mib: u32,

    /// Expose shared host Vulkan through virtio-gpu Venus.
    #[arg(long)]
    pub vm_gpu: bool,

    /// Shell name described to the managed brain.
    #[arg(long, value_name = "SHELL", default_value = "sh")]
    pub vm_shell: String,

    /// Disable guest internet socket proxying.
    #[arg(long)]
    pub vm_no_network: bool,
}

#[derive(Args)]
pub struct VmRunConfig {
    /// Mode-0600 launch record prepared by nanocodex-vm.
    #[arg(long)]
    pub config: PathBuf,
}

impl Host {
    pub fn validate(&self) -> Result<(), ManagedError> {
        validate_vm_factory_name(&self.factory_name)?;
        if self.host_id.is_some_and(|id| {
            id.get_version_num() != 4 || id.get_variant() != uuid::Variant::RFC4122
        }) {
            return Err(ManagedError::Configuration(
                "--host-id must be a UUID v4".to_owned(),
            ));
        }
        match (self.scope, self.agent.as_deref()) {
            (HostScope::Agent, Some(agent)) if valid_managed_agent_id(agent) => Ok(()),
            (HostScope::Agent, Some(_)) => Err(ManagedError::Configuration(
                "--agent must be a safe managed agent identifier".to_owned(),
            )),
            (HostScope::Agent, None) => Err(ManagedError::Configuration(
                "--agent is required with --scope agent".to_owned(),
            )),
            (HostScope::User | HostScope::System, Some(_)) => Err(ManagedError::Configuration(
                "--agent is only valid with --scope agent".to_owned(),
            )),
            (HostScope::User | HostScope::System, None) => Ok(()),
        }
    }
}

/// Whether a value is a safe managed agent identifier.
pub fn valid_managed_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// Owner-local recorder control of a Hand (served by the Hand executable).
#[derive(clap::Args)]
pub struct HandRecordingArgs {
    /// Owner-private recording storage directory used by this Hand.
    #[arg(long)]
    pub state_dir: PathBuf,
    /// Run this Hand's recorder locally until interrupted.
    #[arg(long, conflicts_with = "request")]
    pub serve: bool,
    /// Native desktop observer runtime, when required by this platform.
    #[arg(long, requires = "serve")]
    pub desktop_runtime: Option<PathBuf>,
    /// JSON control request, such as '{"operation":"status"}'.
    #[arg(required_unless_present = "serve")]
    pub request: Option<String>,
}
