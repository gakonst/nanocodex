//! Managed-agent CLI with a Tact-derived local terminal interface.
#![allow(
    clippy::missing_const_for_fn,
    clippy::too_many_arguments,
    clippy::use_self,
    reason = "preserve the reviewed Tact component ownership while adapting its engine boundary"
)]

#[allow(dead_code)]
mod config;
mod connectors;
mod continue_auth;
mod continue_sessions;
mod control;
mod device_hand;
mod hand_observability;
mod hand_recording;
mod hand_recording_control;
mod hand_share;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod hand_workspace;
mod host;
#[allow(dead_code)]
mod installation;
#[cfg(any(target_os = "linux", test))]
mod linux_hand_install;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
mod linux_hand_update;
mod native_hand;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native_secure_input;
mod observation_providers;
mod reload;
mod screen_audio;
mod screen_broadcast;
#[cfg(target_os = "linux")]
mod screen_gamepad;
#[cfg(target_os = "linux")]
mod screen_helpers;
mod screen_hls;
#[cfg(target_os = "linux")]
mod screen_host;
mod screen_ice;
#[cfg(target_os = "linux")]
mod screen_linux_session;
#[cfg(target_os = "macos")]
mod screen_macos;
mod screen_native;
mod screen_publisher;
mod screen_supervisor;
mod screen_video;
#[cfg(target_os = "linux")]
mod screen_wayland;
#[cfg(target_os = "linux")]
mod screen_wayland_encoder;
#[cfg(target_os = "linux")]
mod screen_wayland_input;
mod service;
#[allow(dead_code)]
mod skill;
#[allow(dead_code, unused_imports)]
mod tui;
mod vault;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod vm_hand;
#[cfg(not(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
)))]
#[path = "vm_hand_unsupported.rs"]
mod vm_hand;
mod vm_hand_config;
mod vm_host;
mod voice;
mod voice_recording;
mod voice_state;

// Modules shared with the local (`ncl`) command tree are compiled once at the
// crate root; keep their historical paths inside this managed tree.
#[cfg(target_os = "macos")]
pub(crate) use crate::hand_keep_awake;
pub(crate) use crate::{computer, hand_login, launcher, startup_timing, version};

use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    time::Instant,
};

use clap::{
    Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum,
    builder::NonEmptyStringValueParser,
};
use hand_observability::HandObservabilityArgs;
use host::HostConfig;
use nanocodex_agent::{AgentEvents, Nanocodex, NanocodexError, PromptRequest, Turn, TurnResult};
use nanocodex_cli_auth::client_from_environment;
use nanocodex_managed::{
    AgentSettings, AgentState, EventCursor, Managed, ManagedClient, ManagedError, ManagedEvent,
    PromptInput, validate_vm_factory_name,
};
use nanocodex_oai_tools::attachment::{Attachment, AttachmentMetadata, AttachmentTarget};
use percent_encoding::percent_decode_str;
use tracing::Instrument as _;
use url::Url;

const SYSTEM_HOST_TOKEN_ENV: &str = "NANOCODEX_SYSTEM_HOST_TOKEN";

#[derive(Parser)]
#[command(
    name = "nanocodex",
    version = version::SHORT_VERSION,
    long_version = version::LONG_VERSION,
    about = "Nanocodex terminal client connected to the background machine Hand"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Discover and control a running interactive terminal.
    Tui(nanocodex_tui_control::Cli),
    /// Install or refresh the upstream computer-use runtime.
    Computer(computer::Computer),
    /// Sign in with an SMS code, or import an account API key from stdin.
    Login(nanocodex_cli_auth::Login),
    /// Verify the selected account credential without displaying secrets.
    Status(nanocodex_cli_auth::Options),
    /// Remove the saved account credential on this machine.
    Logout(nanocodex_cli_auth::Options),
    /// Manage account credentials (also available as login, status, and logout).
    #[command(visible_alias = "auth")]
    Account(nanocodex_cli_auth::Account),
    /// Use saved Vault items through broker-owned HTTP requests.
    Vault(vault::Vault),
    /// Manage connected accounts directly.
    Connectors(connectors::Connectors),
    /// Create, list, revoke, or redeem account Hand sharing links.
    HandShare(hand_share::HandShare),
    /// Attach a terminal session to an existing managed agent.
    Attach(Attach),
    /// Connect this computer as a Hand; optionally run a VM or Docker Hand.
    Hand(Hand),
    #[command(name = "__device-hand", hide = true)]
    DeviceHand(device_hand::DeviceHand),
    /// Publish this Hand's native screen; owned by the desktop runtime.
    #[command(name = "__hand-screen", hide = true)]
    HandScreen(screen_native::ScreenCommand),
    #[cfg(target_os = "linux")]
    #[command(name = "__hand-desktop", hide = true)]
    HandDesktop(screen_native::DesktopCommand),
    /// Install this binary as a native Linux Hand from a private stdin request.
    #[cfg(any(target_os = "linux", test))]
    #[command(name = "__install-hand", hide = true)]
    InstallHand,
    /// Update an existing native Linux Hand without enrollment or factory migration.
    #[cfg(any(target_os = "linux", target_os = "macos", test))]
    #[command(name = "__update-hand", hide = true)]
    UpdateHand,
    /// Share an existing Wayland session through the shared Rust publisher.
    #[cfg(target_os = "linux")]
    #[command(name = "wayland-host", hide = true)]
    WaylandHost(screen_host::HostCommand),
    #[cfg(target_os = "linux")]
    #[command(name = "desktop-host", hide = true)]
    DesktopHost(screen_host::HostCommand),
    #[cfg(target_os = "linux")]
    #[command(name = "server-host", hide = true)]
    ServerHost(screen_host::HostCommand),
    /// Serve a bounded pool of on-demand libkrun VM hands.
    Host(Host),
    /// Create a managed agent and print its receipt as JSON.
    New(control::InitialSettings),
    /// Read or update an agent's model and reasoning settings.
    Settings(control::Settings),
    /// Manage durable scheduled prompts.
    Cron(control::Cron),
    /// Continue running and recently used sessions in named tmux windows.
    Continue(continue_sessions::Options),
    #[command(name = "__continue-attach", hide = true)]
    ContinueAttach(continue_auth::Args),
    /// Mark a session done (hide it from continue; retain history and running work).
    Done(AgentId),
    /// Restore a session to continue.
    Undone(AgentId),
    /// List account-owned managed agents as JSON.
    List,
    /// Read owner-only rolling 24-hour Hand tool statistics as JSON.
    HandStats,
    /// Control this Hand’s recorder locally, without account authentication.
    HandRecording(hand_recording_control::Args),
    /// Read one managed agent's durable state as JSON.
    State(AgentId),
    /// Read one managed turn's durable state as JSON.
    Turn(TurnId),
    /// Delete one managed agent and its retained state.
    Delete(AgentId),
    /// Submit one prompt and stream durable managed events as JSONL.
    Run(Run),
    /// Talk to a managed agent using native microphone and speaker audio.
    Voice(voice::Args),
    /// Stream an owned agent's durable events from a cursor.
    Watch(Watch),
    /// Read one backward page of retained events.
    History(History),
    /// Steer an active managed turn.
    Steer(Steer),
    /// Cancel an active managed turn.
    Cancel(TurnId),
    /// Private synchronous entrypoint used by the VM hand's VMM child.
    #[command(name = "__vm-run-config", hide = true)]
    VmRunConfig(VmRunConfig),
    /// Create a private VM disk through the shared Rust image lifecycle.
    #[command(name = "__vm-clone-image", hide = true)]
    VmCloneImage {
        source: PathBuf,
        destination: PathBuf,
    },
}

#[derive(Args)]
struct Attach {
    /// Agent ID, owner URL, or full shared-thread URL. Choose from a list when omitted.
    // Validate after Clap so errors never echo a bearer URL.
    #[arg(value_name = "AGENT_URL_OR_ID")]
    agent: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentReference {
    agent_id: String,
    managed_origin: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum HandNetwork {
    Off,
    Internet,
}

#[derive(Args)]
#[command(
    args_conflicts_with_subcommands = true,
    group(clap::ArgGroup::new("backend").args(["rootfs", "docker"])),
    after_help = "Without a subcommand, serve this computer as a Hand. Use --vm or --docker for an isolated Hand.\n\nExamples:\n  nanocodex hand --docker nanocodex-hand:local --volume my-workspace\n  nanocodex hand --vm root.ext4 --guest-runtime /path/to/nanocodex-vm-guest\n\nUse --network internet to give a Docker Hand internet access."
)]
struct Hand {
    /// Manage the installed Hand service; omit to serve this computer.
    #[command(subcommand)]
    management: Option<crate::hand_setup::HandCommand>,
    /// Private identity directory for an explicitly selected native workspace.
    #[arg(long, conflicts_with_all = ["rootfs", "docker"])]
    state_dir: Option<PathBuf>,
    /// VM with a persistent ext4 root (Linux KVM or Apple Silicon Hypervisor.framework).
    #[arg(
        long = "vm",
        alias = "vm-rootfs",
        value_name = "ROOTFS",
        help_heading = "Backend"
    )]
    rootfs: Option<PathBuf>,

    /// Container using an existing Linux Docker image; no KVM required.
    #[arg(long, value_name = "IMAGE", requires = "docker_volume", conflicts_with_all = ["vm_guest_runtime", "vm_firmware", "vm_gpu"], help_heading = "Backend")]
    docker: Option<String>,

    /// Persistent named Docker workspace volume (required with --docker).
    #[arg(
        long = "volume",
        alias = "docker-volume",
        value_name = "VOLUME",
        requires = "docker",
        help_heading = "Workspace"
    )]
    docker_volume: Option<String>,

    /// Guest network access [default: off for Docker, internet for VM].
    #[arg(long, value_enum, conflicts_with_all = ["docker_internet", "vm_no_network"], help_heading = "Workspace")]
    network: Option<HandNetwork>,

    #[arg(
        long,
        hide = true,
        requires = "docker",
        conflicts_with = "vm_no_network"
    )]
    docker_internet: bool,

    #[arg(long, hide = true, requires = "rootfs")]
    vm_no_network: bool,

    /// Absolute workspace directory inside the Hand.
    #[arg(
        long = "workspace",
        alias = "vm-workspace",
        value_name = "PATH",
        help_heading = "Workspace"
    )]
    vm_workspace: Option<String>,

    /// CPU limit.
    #[arg(long = "cpus", alias = "vm-cpus", value_name = "COUNT", default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..), help_heading = "Resources")]
    vm_cpus: u8,

    /// Memory limit in MiB.
    #[arg(long = "memory", alias = "vm-memory-mib", value_name = "MIB", default_value_t = 1_024, value_parser = clap::value_parser!(u32).range(1..), help_heading = "Resources")]
    vm_memory_mib: u32,

    /// Share the host GPU with a VM; requires a GPU-enabled build and Vulkan renderer.
    #[arg(
        long = "gpu",
        alias = "vm-gpu",
        requires = "rootfs",
        help_heading = "Resources"
    )]
    vm_gpu: bool,

    /// Stable account-local identifier [default: docker or vm, matching the backend].
    #[arg(long, help_heading = "Identity")]
    machine_id: Option<String>,

    /// Display name [default: Nanocodex Docker Hand or Nanocodex VM].
    #[arg(long, help_heading = "Identity")]
    machine_name: Option<String>,

    /// VM factory hosted by this native computer (managed separately, e.g. systemd).
    #[arg(long, conflicts_with_all = ["rootfs", "docker"], help_heading = "Identity")]
    vm_provider: Option<String>,

    /// Legacy option (disabled); use the Hand's CUA tools for browser interactions.
    #[arg(long, help_heading = "Browser")]
    browser: bool,

    /// Legacy browser executable option (disabled); use the Hand's CUA tools.
    #[arg(
        long,
        value_name = "PATH",
        env = "NANOCODEX_BROWSER_EXECUTABLE",
        requires = "browser",
        help_heading = "Browser"
    )]
    browser_executable: Option<PathBuf>,

    /// Static Linux guest executable for an ext4 VM (or NANOCODEX_VM_GUEST_RUNTIME).
    #[arg(
        long = "guest-runtime",
        alias = "vm-guest-runtime",
        value_name = "ELF",
        requires = "rootfs",
        help_heading = "VM setup"
    )]
    vm_guest_runtime: Option<PathBuf>,

    /// Installed Docker OCI runtime, e.g. runsc; fails if unavailable.
    #[arg(
        long = "runtime",
        alias = "docker-runtime",
        value_name = "RUNTIME",
        requires = "docker",
        help_heading = "Advanced"
    )]
    docker_runtime: Option<String>,

    /// Prepared VM guest disk cache.
    #[arg(
        long = "cache",
        alias = "vm-cache",
        value_name = "PATH",
        default_value = ".cache/vm",
        requires = "rootfs",
        help_heading = "Advanced"
    )]
    vm_cache: PathBuf,

    /// libkrun firmware directory (or NANOCODEX_KRUNFW_DIR).
    #[arg(
        long = "firmware",
        alias = "vm-firmware",
        value_name = "PATH",
        requires = "rootfs",
        help_heading = "Advanced"
    )]
    vm_firmware: Option<PathBuf>,

    /// Shell described to the managed brain.
    #[arg(
        long = "shell",
        alias = "vm-shell",
        value_name = "SHELL",
        default_value = "sh",
        help_heading = "Advanced"
    )]
    vm_shell: String,

    #[command(flatten, next_help_heading = "Logging")]
    observability: HandObservabilityArgs,
}

impl Hand {
    fn machine_id(&self) -> &str {
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
enum HostScope {
    User,
    Agent,
    System,
}

#[derive(Args)]
struct Host {
    #[command(flatten)]
    observability: HandObservabilityArgs,

    /// Authority scope that may provision VMs from this host.
    #[arg(long, value_enum, default_value_t = HostScope::User)]
    scope: HostScope,

    /// Exact /mount provider selector, unique within the selected scope.
    #[arg(long, value_name = "FACTORY_NAME")]
    factory_name: String,

    /// Managed agent ID. Required only with --scope agent.
    #[arg(long, value_name = "AGENT_ID", required_if_eq("scope", "agent"))]
    agent: Option<String>,

    /// Immutable raw ext4 image cloned privately for every allocation.
    #[arg(long, value_name = "ROOTFS")]
    vm_template: PathBuf,

    /// Durable private host state and per-allocation VM roots.
    #[arg(long, value_name = "PATH")]
    state_dir: PathBuf,

    /// Maximum number of provisioning, live, or releasing VMs.
    #[arg(long, value_name = "COUNT", default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
    max_vms: u16,

    /// Keep one never-assigned VM ready within --max-vms capacity.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    warm_spare: bool,

    /// Stable host UUID. Generated and persisted under --state-dir when omitted.
    #[arg(long, value_name = "UUID")]
    host_id: Option<uuid::Uuid>,

    /// Statically linked Linux guest executable used with the raw ext4 roots.
    #[arg(long, value_name = "ELF", env = "NANOCODEX_VM_GUEST_RUNTIME")]
    vm_guest_runtime: PathBuf,

    /// Cache for the prepared read-only guest runtime disk.
    #[arg(long, value_name = "PATH", default_value = ".cache/vm")]
    vm_cache: PathBuf,

    /// Directory containing the platform libkrun firmware library.
    #[arg(long, value_name = "PATH", env = "NANOCODEX_KRUNFW_DIR")]
    vm_firmware: Option<PathBuf>,

    /// Absolute working directory inside every provisioned VM.
    #[arg(long, value_name = "PATH", default_value = "/app")]
    vm_workspace: String,

    /// Number of virtual CPUs assigned to each VM.
    #[arg(long, value_name = "COUNT", default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=64))]
    vm_cpus: u8,

    /// Guest memory in mebibytes assigned to each VM.
    #[arg(long, value_name = "MIB", default_value_t = 1_024, value_parser = clap::value_parser!(u32).range(128..=262_144))]
    vm_memory_mib: u32,

    /// Expose shared host Vulkan through virtio-gpu Venus.
    #[arg(long)]
    vm_gpu: bool,

    /// Shell name described to the managed brain.
    #[arg(long, value_name = "SHELL", default_value = "sh")]
    vm_shell: String,

    /// Disable guest internet socket proxying.
    #[arg(long)]
    vm_no_network: bool,
}

#[derive(Args)]
struct VmRunConfig {
    /// Mode-0600 launch record prepared by nanocodex-vm.
    #[arg(long)]
    config: PathBuf,
}

impl Host {
    fn validate(&self) -> Result<(), ManagedError> {
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

#[derive(Args)]
struct AgentId {
    /// Account-owned managed agent ID.
    agent_id: String,
}

#[derive(Args)]
struct TurnId {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Managed turn ID.
    turn_id: String,
}

#[derive(Args)]
struct Run {
    #[command(flatten)]
    settings: control::InitialSettings,
    /// Prompt text.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,
    /// Resume this account-owned agent. A new one is created when omitted.
    #[arg(long, conflicts_with_all = ["model", "thinking", "reasoning_mode", "fast_mode", "chatgpt_account"])]
    agent: Option<String>,
    /// Stable idempotency key. The managed backend generates one when omitted.
    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Args)]
struct Watch {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Resume strictly after this decimal cursor, or tail from `latest`.
    #[arg(long, default_value = "0")]
    cursor: String,
}

#[derive(Args)]
struct History {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Return rows strictly before this positive decimal cursor.
    #[arg(long)]
    before: Option<String>,
    /// Page size from 1 through 256.
    #[arg(long, default_value_t = 128)]
    limit: u16,
}

#[derive(Args)]
struct Steer {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Active managed turn ID.
    turn_id: String,
    /// Additional prompt text.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,
}

/// The managed command tree, with the local tree's unique commands listed for help.
pub(crate) fn command() -> clap::Command {
    Cli::command()
}

/// Whether this process is an internal helper selected by environment rather
/// than by its command line; it bypasses command-tree selection.
pub(crate) fn is_helper_process() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var(screen_wayland_encoder::HELPER_ENV).as_deref() == Ok("1") {
        return true;
    }
    false
}

fn parse(arguments: Vec<OsString>) -> Cli {
    let mut command = crate::with_foreign_commands(Cli::command(), &crate::Cli::command());
    let matches = command
        .try_get_matches_from_mut(arguments)
        .unwrap_or_else(|error| error.exit());
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

pub(crate) fn main(arguments: Vec<OsString>) -> ExitCode {
    match try_main(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if matches!(&error, ManagedError::Configuration(message) if message == "local recording control failed")
            {
                println!(
                    "{}",
                    serde_json::json!({"ok": false, "error": "local_recording_control_failed"})
                );
            } else {
                eprintln!("Error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn try_main(arguments: Vec<OsString>) -> Result<(), ManagedError> {
    let _startup = startup_timing::Stage::new("process");
    launcher::initialize_install_root();
    let _ = dotenvy::dotenv();
    #[cfg(target_os = "linux")]
    if std::env::var(screen_wayland_encoder::HELPER_ENV).as_deref() == Ok("1") {
        return run_with_runtime(async {
            screen_wayland_encoder::run(std::env::args().skip(1).collect())
                .await
                .map_err(|error| ManagedError::Configuration(error.to_string()))
        });
    }
    let cli = parse(arguments);
    #[cfg(target_os = "linux")]
    let (cli, prepared) = {
        let mut cli = cli;
        let host = match cli.command.take() {
            Some(Command::WaylandHost(args)) => Some(args.prepare(screen_host::Mode::Wayland)?),
            Some(Command::DesktopHost(args)) => Some(args.prepare(screen_host::Mode::Desktop)?),
            Some(Command::ServerHost(args)) => Some(args.prepare(screen_host::Mode::Server)?),
            other => {
                cli.command = other;
                None
            }
        };
        let prepared = if let Some((prepared, environment)) = host {
            // SAFETY: only standalone process startup reaches this point. No
            // Tokio, capture, audio, or provider threads have been started yet.
            for (key, value) in environment {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var(key, value);
                }
            }
            Some(prepared)
        } else {
            None
        };
        (cli, prepared)
    };
    run_with_runtime(async move {
        #[cfg(target_os = "linux")]
        if let Some(prepared) = prepared {
            return screen_host::serve(prepared).await;
        }
        run(cli).await
    })
}

fn run_with_runtime(
    future: impl std::future::Future<Output = Result<(), ManagedError>>,
) -> Result<(), ManagedError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ManagedError::Configuration(format!("failed to start Tokio: {error}")))?;
    let result = runtime.block_on(future);
    // Application cleanup has completed. Optional presentation discovery or DNS
    // can still own blocking work that Tokio cannot cancel. Foreground work and
    // its owned cleanup were awaited above; give no extra exit grace period to
    // these disposable background tasks.
    runtime.shutdown_background();
    result
}

async fn run(cli: Cli) -> Result<(), ManagedError> {
    if let Some(Command::HandRecording(command)) = &cli.command {
        return command
            .run()
            .await
            .map_err(|_| ManagedError::Configuration("local recording control failed".into()));
    }
    // Shared links carry their own narrowly scoped authority. Never load an
    // account credential or start a local Hand for a guest attachment.
    let attach_reference = match &cli.command {
        Some(Command::Attach(Attach { agent: Some(value) })) => {
            if value.contains("/share/") || value.contains("#token=") {
                return tui::run_shared(value).await;
            }
            Some(parse_agent_reference(value).map_err(ManagedError::Configuration)?)
        }
        _ => None,
    };
    let command = match cli.command {
        Some(Command::Tui(command)) => {
            return command
                .run()
                .await
                .map_err(|error| ManagedError::Configuration(error.to_string()));
        }
        #[cfg(target_os = "linux")]
        Some(Command::WaylandHost(_) | Command::DesktopHost(_) | Command::ServerHost(_)) => {
            return Err(ManagedError::Configuration(
                "standalone host must initialize before runtime startup".into(),
            ));
        }
        Some(Command::ContinueAttach(command)) => return continue_auth::attach(command).await,
        Some(Command::Computer(command)) => {
            return command.run().await.map_err(ManagedError::Configuration);
        }
        Some(Command::Login(command)) => {
            let receipt = command.run_with_receipt().await.map_err(auth_error)?;
            hand_login::connect_after_login(&receipt).await;
            return Ok(());
        }
        Some(Command::Status(command)) => {
            return nanocodex_cli_auth::AccountCommand::Status(command)
                .run()
                .await
                .map_err(auth_error);
        }
        Some(Command::Logout(command)) => {
            return nanocodex_cli_auth::AccountCommand::Logout(command)
                .run()
                .await
                .map_err(auth_error);
        }
        Some(Command::Account(command)) => {
            if let Some(receipt) = command.run_with_receipt().await.map_err(auth_error)? {
                hand_login::connect_after_login(&receipt).await;
            }
            return Ok(());
        }
        Some(Command::VmRunConfig(command)) => return vm_hand::run_config(&command.config),
        Some(Command::VmCloneImage {
            source,
            destination,
        }) => {
            return vm_hand::clone_image(&source, &destination);
        }
        #[cfg(target_os = "linux")]
        Some(Command::HandDesktop(command)) => return screen_native::serve_desktop(command).await,
        #[cfg(any(target_os = "linux", test))]
        Some(Command::InstallHand) => return linux_hand_install::run().await,
        #[cfg(any(target_os = "linux", target_os = "macos", test))]
        Some(Command::UpdateHand) => return linux_hand_update::run().await,
        Some(Command::Hand(Hand {
            management: Some(management),
            ..
        })) => {
            // Mode selection sends management subcommands to the local tree;
            // keep any other route to them working identically.
            return crate::hand_setup::Hand::from(management)
                .run()
                .await
                .map_err(|error| ManagedError::Configuration(format!("{error:#}")));
        }
        Some(Command::Hand(command)) if command.rootfs.is_none() && command.docker.is_none() => {
            return native_hand::serve_hand(command).await;
        }
        Some(Command::DeviceHand(command)) => return device_hand::serve(command).await,
        Some(Command::Hand(command)) => {
            let _observability = command
                .observability
                .install()
                .map_err(|error| ManagedError::Configuration(error.to_string()))?;
            tracing::info!(target: "nanocodex2", stage = "hand.preflight",
                machine.id = command.machine_id(),
                hand.backend = if command.docker.is_some() { "docker" } else { "vm" },
                vm.cpu.count = command.vm_cpus,
                vm.memory.limit_mib = command.vm_memory_mib,
                vm.root.kind = command.rootfs.as_ref().map_or("container", |root| if root.exists() { "existing" } else { "missing" }),
                "checking Hand backend support");
            if let Err(error) = vm_hand::VmHand::preflight(&command).await {
                tracing::error!(target: "nanocodex2", stage = "hand.preflight.failed", "Hand backend preflight failed");
                return Err(error);
            }
            let client = client_from_environment(None)?;
            return serve_vm_hand(&client, command).await;
        }
        Some(Command::Host(command)) => {
            let _observability = command
                .observability
                .install()
                .map_err(|error| ManagedError::Configuration(error.to_string()))?;
            return vm_host::serve(command).await;
        }
        command => command,
    };
    let managed_origin = attach_reference
        .as_ref()
        .and_then(|agent| agent.managed_origin.as_deref());
    let client = {
        let _timing = startup_timing::Stage::new("managed_client");
        client_from_environment(managed_origin)?
    };
    // The OS-owned Hand publishes independently. Its local observer must not
    // hold the terminal or inference behind service startup or IPC readiness.
    let device = matches!(
        &command,
        None | Some(Command::Attach(_) | Command::Run(_) | Command::Voice(_))
    )
    .then(|| device_hand::BackgroundHandTask::start(client.clone()));
    let result = match command {
        Some(
            Command::Tui(_)
            | Command::Login(_)
            | Command::Status(_)
            | Command::Logout(_)
            | Command::Account(_),
        ) => {
            unreachable!("handled before managed client setup")
        }
        Some(Command::Vault(command)) => command.run(&client).await,
        Some(Command::Connectors(command)) => command.run(&client).await,
        Some(Command::HandShare(command)) => command.run(&client).await,
        Some(Command::Voice(command)) => voice::run(&client, command).await,
        Some(Command::Attach(_)) => {
            attach_tui(&client, attach_reference.map(|agent| agent.agent_id)).await
        }
        Some(Command::ContinueAttach(_)) => unreachable!("handled before managed client setup"),
        Some(Command::Computer(_)) => unreachable!("handled before managed client setup"),
        Some(Command::DeviceHand(_)) => unreachable!("handled before managed client setup"),
        Some(Command::Hand(_)) => unreachable!("handled before managed client setup"),
        Some(Command::HandScreen(command)) => screen_native::serve(&client, command).await,
        #[cfg(target_os = "linux")]
        Some(Command::HandDesktop(_)) => unreachable!("handled before managed client setup"),
        #[cfg(any(target_os = "linux", test))]
        Some(Command::InstallHand) => unreachable!("handled before managed client setup"),
        #[cfg(any(target_os = "linux", target_os = "macos", test))]
        Some(Command::UpdateHand) => unreachable!("handled before managed client setup"),
        Some(Command::Host(_)) => unreachable!("handled before managed client setup"),
        Some(Command::New(settings)) if !settings.is_explicit() => {
            // Omitted settings let the service choose its default (Claude Opus
            // 5.5 at medium effort, with an OpenAI fallback when unavailable).
            write_json(&client.create().await?)
        }
        Some(Command::New(settings)) => {
            let account = settings.chatgpt_account.clone();
            let settings = settings.resolve_validated()?;
            let receipt = match account {
                Some(account) => {
                    client
                        .create_with_chatgpt_account(settings, &account)
                        .await?
                }
                None => client.create_with_settings(settings).await?,
            };
            write_json(&receipt)
        }
        Some(Command::Settings(command)) => command.run(&client).await,
        Some(Command::Cron(command)) => command.run(&client).await,
        Some(Command::Continue(command)) => continue_sessions::run(&client, command).await,
        Some(Command::Done(command)) => {
            write_json(&client.set_done(&command.agent_id, true).await?)
        }
        Some(Command::Undone(command)) => {
            write_json(&client.set_done(&command.agent_id, false).await?)
        }
        Some(Command::List) => write_json(&client.list().await?),
        Some(Command::HandStats) => write_json(&client.hosted_tool_stats().await?),
        Some(Command::HandRecording(_)) => unreachable!("handled before managed client setup"),
        Some(Command::State(command)) => write_json(&client.state(&command.agent_id).await?),
        Some(Command::Turn(command)) => write_json(
            &client
                .turn_state(&command.agent_id, &command.turn_id)
                .await?,
        ),
        Some(Command::Delete(command)) => client.delete(&command.agent_id).await,
        Some(Command::Run(command)) => run_turn(&client, command).await,
        Some(Command::Watch(command)) => watch(&client, command).await,
        Some(Command::History(command)) => write_json(
            &client
                .history(&command.agent_id, command.before.as_deref(), command.limit)
                .await?,
        ),
        Some(Command::Steer(command)) => write_json(
            &client
                .steer(
                    &command.agent_id,
                    &command.turn_id,
                    &PromptInput::Text(command.prompt),
                )
                .await?,
        ),
        Some(Command::Cancel(command)) => {
            write_json(&client.cancel(&command.agent_id, &command.turn_id).await?)
        }
        #[cfg(target_os = "linux")]
        Some(Command::WaylandHost(_) | Command::DesktopHost(_) | Command::ServerHost(_)) => {
            unreachable!("handled before runtime startup")
        }
        Some(Command::VmRunConfig(_)) => unreachable!("handled before managed client setup"),
        Some(Command::VmCloneImage { .. }) => unreachable!("handled before managed client setup"),
        None => new_tui(&client).await,
    };
    if let Some(device) = device {
        device.stop().await;
    }
    result
}

async fn launch_vm_hand(command: &Hand) -> Result<vm_hand::VmHand, ManagedError> {
    let (root_kind, root_bytes) = match command.rootfs.as_ref().map(std::fs::metadata) {
        Some(Ok(metadata)) if metadata.is_file() => ("file", metadata.len()),
        Some(Ok(metadata)) if metadata.is_dir() => ("directory", 0),
        Some(Ok(_)) => ("other", 0),
        Some(Err(_)) => ("missing", 0),
        None => ("docker", 0),
    };
    let span = tracing::info_span!(
        target: "nanocodex2",
        "vm.launch",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        machine.id = command.machine_id(),
        vm.cpu.count = command.vm_cpus,
        vm.memory.limit_mib = command.vm_memory_mib,
        vm.root.kind = root_kind,
        vm.root.bytes = root_bytes,
        network.enabled = command.network.map_or(if command.docker.is_some() { command.docker_internet } else { !command.vm_no_network }, |network| network == HandNetwork::Internet),
        hand.backend = if command.docker.is_some() { "docker" } else { "libkrun" },
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let started = Instant::now();
    async {
        tracing::info!(
            target: "nanocodex2",
            stage = "vm.launch.starting",
            "starting Hand workspace"
        );
        let result = vm_hand::VmHand::start(command).await;
        span.record(
            "duration_ns",
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        match &result {
            Ok(_) => {
                span.record("status", "ready");
                span.record("otel.status_code", "OK");
                tracing::info!(
                    target: "nanocodex2",
                    stage = "vm.launch.ready",
                    "Hand guest is ready"
                );
            }
            Err(_) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                tracing::error!(
                    target: "nanocodex2",
                    stage = "vm.launch.failed",
                    "Hand guest failed to start"
                );
            }
        }
        result
    }
    .instrument(span.clone())
    .await
}

async fn serve_vm_hand(client: &ManagedClient, command: Hand) -> Result<(), ManagedError> {
    let target = client.account_attachment_target()?;
    let mut hand = launch_vm_hand(&command).await?;
    drop(command);
    let connected = async {
        hand.start_desktop(&target).await?;
        connect_vm_hand(&hand, target).await
    }
    .await;
    let attachment = match connected {
        Ok(Some(attachment)) => attachment,
        Ok(None) => {
            shutdown_vm_hand(hand).await?;
            return Ok(());
        }
        Err(error) => {
            return match shutdown_vm_hand(hand).await {
                Ok(()) => Err(error),
                Err(shutdown) => Err(ManagedError::Configuration(format!(
                    "{error}; VM shutdown also failed: {shutdown}"
                ))),
            };
        }
    };
    tracing::info!(
        target: "nanocodex2",
        stage = "vm.hand.ready",
        "Hand is ready; press Ctrl-C to detach"
    );
    let closed = attachment.clone();
    let attachment_result = tokio::select! {
        signal = service::shutdown_signal() => {
            signal?;
            attachment.clone().detach().await
        }
        result = closed.closed() => result,
    };
    drop(attachment);
    drop(closed);
    let shutdown = shutdown_vm_hand(hand).await;
    match (attachment_result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(ManagedError::Configuration(error.to_string())),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(ManagedError::Configuration(format!(
            "{error}; VM shutdown also failed: {shutdown}"
        ))),
    }
}

async fn shutdown_vm_hand(hand: vm_hand::VmHand) -> Result<(), ManagedError> {
    let span = tracing::info_span!(
        target: "nanocodex2",
        "vm.shutdown",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let started = Instant::now();
    async {
        tracing::info!(
            target: "nanocodex2",
            stage = "vm.shutdown.starting",
            "stopping Hand guest"
        );
        let result = hand.shutdown().await;
        span.record(
            "duration_ns",
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        if result.is_ok() {
            span.record("status", "completed");
            span.record("otel.status_code", "OK");
            tracing::info!(
                target: "nanocodex2",
                stage = "vm.shutdown.completed",
                "Hand guest stopped"
            );
        } else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            tracing::error!(
                target: "nanocodex2",
                stage = "vm.shutdown.failed",
                "Hand guest failed to stop cleanly"
            );
        }
        result
    }
    .instrument(span.clone())
    .await
}

async fn connect_vm_hand(
    hand: &vm_hand::VmHand,
    target: AttachmentTarget,
) -> Result<Option<Attachment>, ManagedError> {
    let connector = hand
        .tools()
        .attach(target)
        .metadata(AttachmentMetadata::machine(hand.machine().clone()));
    let connected = tokio::select! {
        signal = service::shutdown_signal() => {
            signal?;
            Ok(None)
        }
        connected = connector.connect() => connected
            .map(Some)
            .map_err(|error| ManagedError::Configuration(error.to_string())),
    };
    connected.map(|connected| connected.map(|(attachment, _events)| attachment))
}

fn auth_error(error: nanocodex_cli_auth::Error) -> ManagedError {
    ManagedError::Configuration(error.to_string())
}

fn managed_url_from_environment(fallback: Option<&str>) -> Result<String, ManagedError> {
    nanocodex_cli_auth::managed_url_from_environment(fallback).map_err(auth_error)
}

fn parse_agent_reference(value: &str) -> Result<AgentReference, String> {
    if valid_managed_agent_id(value) {
        return Ok(AgentReference {
            agent_id: value.to_owned(),
            managed_origin: None,
        });
    }
    let url = Url::parse(value).map_err(|_| {
        "agent must be a managed agent ID or a Nanocodex /agent/<agent-id> URL".to_owned()
    })?;
    if !supported_agent_page_origin(&url) || !url.username().is_empty() || url.password().is_some()
    {
        return Err("agent URL must use a Nanocodex web origin without credentials".to_owned());
    }
    let segments = url
        .path_segments()
        .ok_or_else(|| "agent URL must have the path /agent/<agent-id>".to_owned())?
        .collect::<Vec<_>>();
    let encoded = match segments.as_slice() {
        ["agent", encoded] if !encoded.is_empty() => *encoded,
        ["agent", encoded, ""] if !encoded.is_empty() => *encoded,
        _ => return Err("agent URL must have the path /agent/<agent-id>".to_owned()),
    };
    let agent_id = percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|_| "agent URL contains an invalid UTF-8 path segment".to_owned())?;
    if !valid_managed_agent_id(&agent_id) {
        return Err("agent URL contains an invalid managed agent ID".to_owned());
    }
    Ok(AgentReference {
        agent_id: agent_id.into_owned(),
        managed_origin: Some(url.origin().ascii_serialization()),
    })
}

fn valid_managed_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn supported_agent_page_origin(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if url.scheme() == "https" && host == "nanocodex.gakonst.workers.dev" && url.port().is_none() {
        return true;
    }
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    host == "nanocodex.localhost"
        || host
            .strip_suffix(".nanocodex.localhost")
            .is_some_and(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
}

async fn run_turn(client: &ManagedClient, command: Run) -> Result<(), ManagedError> {
    let created = command.agent.is_none();
    let account = command.settings.chatgpt_account.clone();
    let settings = if command.settings.is_explicit() || command.agent.is_some() {
        command.settings.resolve_validated()?
    } else {
        // The account catalog default: Claude Opus 5.5 at medium when available.
        client.default_settings().await?
    };
    let requested_agent = command.agent;
    let request_id = command
        .idempotency_key
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (agent, mut events, agent_id, _, initial_turn) = if requested_agent.is_none() {
        build_workspace_agent_with_settings(
            client,
            None,
            None,
            settings,
            None,
            Some((command.prompt.clone(), request_id.clone())),
            account,
        )
        .await?
    } else {
        build_workspace_agent_with_settings(
            client,
            requested_agent,
            None,
            settings,
            None,
            None,
            None,
        )
        .await?
    };
    if created {
        eprintln!("Managed agent: {agent_id}");
    }
    let turn = match initial_turn {
        Some(turn) => turn,
        None => agent
            .prompt(PromptRequest::new(command.prompt).request_id(request_id))
            .await
            .map_err(agent_error)?,
    };
    let outcome = await_turn(turn, &mut events).await;
    let shutdown = agent.shutdown().await.map_err(agent_error);
    match (outcome, shutdown) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(Some(result)), Ok(())) => {
            eprintln!("{}", result.final_message());
            Ok(())
        }
        (Ok(None), Ok(())) => Ok(()),
    }
}

async fn attach_tui(
    client: &ManagedClient,
    requested_agent_id: Option<String>,
) -> Result<(), ManagedError> {
    tui::run(client, requested_agent_id).await
}

async fn new_tui(client: &ManagedClient) -> Result<(), ManagedError> {
    tui::run_new(client).await
}

async fn open_workspace_agent_from(
    client: &ManagedClient,
    agent_id: Option<String>,
    state: Option<AgentState>,
    event_observer: Option<tokio::sync::mpsc::UnboundedSender<ManagedEvent>>,
) -> Result<(Nanocodex, AgentEvents, String, std::path::PathBuf), ManagedError> {
    let settings = AgentSettings::default();
    open_workspace_agent_with_settings(client, agent_id, state, settings, event_observer).await
}

async fn open_workspace_agent_with_settings(
    client: &ManagedClient,
    agent_id: Option<String>,
    state: Option<AgentState>,
    settings: AgentSettings,
    event_observer: Option<tokio::sync::mpsc::UnboundedSender<ManagedEvent>>,
) -> Result<(Nanocodex, AgentEvents, String, std::path::PathBuf), ManagedError> {
    let (agent, events, id, workspace, _) = build_workspace_agent_with_settings(
        client,
        agent_id,
        state,
        settings,
        event_observer,
        None,
        None,
    )
    .await?;
    Ok((agent, events, id, workspace))
}

async fn build_workspace_agent_with_settings(
    client: &ManagedClient,
    agent_id: Option<String>,
    state: Option<AgentState>,
    settings: AgentSettings,
    event_observer: Option<tokio::sync::mpsc::UnboundedSender<ManagedEvent>>,
    initial_prompt: Option<(String, String)>,
    chatgpt_account: Option<String>,
) -> Result<
    (
        Nanocodex,
        AgentEvents,
        String,
        std::path::PathBuf,
        Option<Turn>,
    ),
    ManagedError,
> {
    let _opening = startup_timing::Stage::new("workspace_open");
    let config =
        HostConfig::load().map_err(|error| ManagedError::Configuration(error.to_string()))?;
    let workspace = config.workspace().to_path_buf();
    // A terminal is a client of the account's persistent computer Hand. Its
    // directory is turn context, never another machine or tool publisher.
    let client = device_hand::with_client_context(client.clone(), &workspace)?;
    let backend = match (agent_id, state) {
        (None, None) if initial_prompt.is_some() => {
            Managed::create(client.clone()).with_settings(settings)
        }
        (None, None) => Managed::create_live(client.clone()).with_settings(settings),
        (Some(agent_id), Some(state)) => {
            Managed::open_live_from_state(client.clone(), agent_id, state)
        }
        (Some(agent_id), None) => Managed::open_live(client.clone(), agent_id),
        (None, Some(_)) => {
            return Err(ManagedError::Configuration(
                "managed state requires an agent identifier".to_owned(),
            ));
        }
    };
    let mut builder = Nanocodex::builder(backend);
    if let Some(account) = chatgpt_account {
        builder = builder.chatgpt_account(account);
    }
    let builder = match event_observer {
        Some(observer) => builder.event_observer(observer),
        None => builder,
    };
    let (agent, events, turn) = {
        let _timing = startup_timing::Stage::new("managed_backend");
        match initial_prompt {
            Some((prompt, key)) => {
                let (agent, events, turn) = builder
                    .build_with_prompt(prompt, key)
                    .await
                    .map_err(agent_error)?;
                (agent, events, Some(turn))
            }
            None => {
                let (agent, events) = builder.build().await.map_err(agent_error)?;
                (agent, events, None)
            }
        }
    };
    let agent_id = agent.agent_id().to_owned();
    Ok((agent, events, agent_id, workspace, turn))
}

async fn await_turn(
    turn: Turn,
    events: &mut AgentEvents,
) -> Result<Option<TurnResult>, ManagedError> {
    tokio::pin!(turn);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    loop {
        tokio::select! {
            biased;
            result = &mut turn => {
                let result = result.map_err(agent_error)?;
                while let Some(event) = events.try_recv_timed() {
                    write_json_line(&event.event)?;
                }
                return Ok(Some(result));
            }
            signal = &mut interrupt => {
                signal.map_err(|error| ManagedError::Configuration(
                    format!("failed to listen for Ctrl-C: {error}")
                ))?;
                return Ok(None);
            },
            event = events.recv() => match event {
                Some(event) => {
                    write_json_line(&event)?;
                }
                None => return tokio::select! {
                    result = &mut turn => result.map(Some).map_err(agent_error),
                    signal = &mut interrupt => {
                        signal.map_err(|error| ManagedError::Configuration(
                            format!("failed to listen for Ctrl-C: {error}")
                        ))?;
                        Ok(None)
                    },
                },
            },
        }
    }
}

fn agent_error(error: NanocodexError) -> ManagedError {
    ManagedError::Configuration(error.to_string())
}

async fn watch(client: &ManagedClient, command: Watch) -> Result<(), ManagedError> {
    let mut events = client.events(&command.agent_id, EventCursor::parse(command.cursor)?)?;
    loop {
        write_json_line(&events.next().await?)?;
    }
}

fn write_json<T: serde::Serialize>(value: &T) -> Result<(), ManagedError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|_| ManagedError::InvalidResponse("failed to encode output"))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|_| ManagedError::InvalidResponse("failed to write output"))
}

fn write_json_line<T: serde::Serialize>(value: &T) -> Result<(), ManagedError> {
    write_json(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_reports_full_source_revision_for_local_updates() {
        use clap::CommandFactory;

        let output = Cli::command()
            .try_get_matches_from(["nanocodex2", "--version"])
            .unwrap_err();
        assert_eq!(output.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(
            output
                .to_string()
                .contains(concat!("Commit SHA: ", env!("VERGEN_GIT_SHA"),))
        );
        assert!(output.to_string().contains("Build Profile: "));
    }

    #[test]
    fn runtime_waits_for_foreground_cleanup_before_success_or_error() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        for fails in [false, true] {
            let cleaned = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&cleaned);
            let result = run_with_runtime(async move {
                // The application future owns and awaits this cleanup, even on
                // its error path. Runtime background shutdown must follow it.
                let cleanup = tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    observed.store(true, Ordering::SeqCst);
                });
                cleanup.await.unwrap();
                if fails {
                    Err(ManagedError::Configuration(
                        "synthetic runtime failure".into(),
                    ))
                } else {
                    Ok(())
                }
            });
            assert!(cleaned.load(Ordering::SeqCst));
            assert_eq!(result.is_err(), fails);
        }
    }

    #[test]
    fn runtime_shutdown_does_not_wait_for_background_blocking_work() {
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };

        for fails in [false, true] {
            let (release, blocked) = mpsc::channel();
            let (finished, completion) = mpsc::channel();
            let started = Instant::now();
            let result = run_with_runtime(async move {
                let (ready, received) = tokio::sync::oneshot::channel();
                drop(tokio::task::spawn_blocking(move || {
                    let _ = ready.send(());
                    let _ = blocked.recv_timeout(Duration::from_secs(5));
                    let _ = finished.send(());
                }));
                received.await.unwrap();
                if fails {
                    Err(ManagedError::Configuration(
                        "synthetic runtime failure".into(),
                    ))
                } else {
                    Ok(())
                }
            });
            let elapsed = started.elapsed();
            // Release our synthetic blocking task even if the timing assertion fails.
            let _ = release.send(());
            completion.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(
                elapsed < Duration::from_secs(1),
                "shutdown took {elapsed:?}"
            );
            assert_eq!(result.is_err(), fails);
            if let Err(error) = result {
                assert!(matches!(
                    error,
                    ManagedError::Configuration(message) if message == "synthetic runtime failure"
                ));
            }
        }
    }

    #[test]
    fn hand_stats_is_a_read_only_standard_managed_command() {
        let cli = Cli::try_parse_from(["nanocodex2", "hand-stats"]).unwrap();
        assert!(matches!(cli.command, Some(Command::HandStats)));
    }

    #[test]
    fn parses_attach_url_into_its_agent_id() {
        let cli = Cli::try_parse_from([
            "nanocodex2",
            "attach",
            "https://named-workspace-fabric.nanocodex.localhost:2443/agent/77777777-7777-4777-8777-777777777777?thread=ignored#top",
        ])
        .expect("attach URL must parse");
        let Some(Command::Attach(Attach { agent })) = cli.command else {
            panic!("attach command parsed into the wrong variant");
        };
        assert_eq!(
            agent
                .as_deref()
                .map(parse_agent_reference)
                .transpose()
                .unwrap(),
            Some(AgentReference {
                agent_id: "77777777-7777-4777-8777-777777777777".to_owned(),
                managed_origin: Some(
                    "https://named-workspace-fabric.nanocodex.localhost:2443".to_owned()
                ),
            })
        );
    }

    #[test]
    fn parses_raw_agent_ids_and_optional_picker() {
        assert_eq!(
            parse_agent_reference("agent:v1_test-id").unwrap(),
            AgentReference {
                agent_id: "agent:v1_test-id".to_owned(),
                managed_origin: None,
            }
        );
        let picker = Cli::try_parse_from(["nanocodex2", "attach"])
            .expect("attach without an agent must open the picker");
        assert!(matches!(
            picker.command,
            Some(Command::Attach(Attach { agent: None }))
        ));
    }

    #[test]
    fn parses_supported_agent_urls() {
        for (url, expected) in [
            (
                "https://nanocodex.gakonst.workers.dev/agent/agent-1",
                "agent-1",
            ),
            ("https://nanocodex.localhost/agent/a%3Ab/", "a:b"),
            ("http://nanocodex.localhost:5173/agent/local", "local"),
            ("https://branch-1.nanocodex.localhost/agent/id", "id"),
        ] {
            assert_eq!(
                parse_agent_reference(url).unwrap().agent_id,
                expected,
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_non_agent_and_unsafe_urls() {
        for value in [
            "https://example.com/agent/id",
            "ftp://nanocodex.localhost/agent/id",
            "https://user@nanocodex.localhost/agent/id",
            "https://nanocodex.localhost/agent",
            "https://nanocodex.localhost/v1/agents/id",
            "https://nanocodex.localhost/agent/id/turns",
            "https://nanocodex.localhost/agent/a%2Fb",
            "https://nanocodex.localhost/agent/a%252Fb",
        ] {
            assert!(parse_agent_reference(value).is_err(), "{value}");
        }
    }

    #[test]
    fn host_scope_requires_agent_exactly_for_agent_scope() {
        let common = [
            "--factory-name",
            "garage-mac",
            "--vm-template",
            "/tmp/template.ext4",
            "--state-dir",
            "/tmp/host-state",
            "--vm-guest-runtime",
            "/tmp/guest",
        ];
        let user = Cli::try_parse_from(["nanocodex2", "host"].into_iter().chain(common)).unwrap();
        let Some(Command::Host(user)) = user.command else {
            panic!("host parsed into the wrong command")
        };
        assert_eq!(user.scope, HostScope::User);
        assert_eq!(user.factory_name, "garage-mac");
        user.validate().unwrap();

        for invalid_name in ["host", "cloudflare", "cf_sandbox", "Garage-Mac", "bad/name"] {
            let invalid = Cli::try_parse_from(
                ["nanocodex2", "host", "--factory-name", invalid_name]
                    .into_iter()
                    .chain(common[2..].iter().copied()),
            )
            .unwrap();
            let Some(Command::Host(invalid)) = invalid.command else {
                panic!("invalid factory host parsed into the wrong command")
            };
            assert!(invalid.validate().is_err(), "accepted {invalid_name:?}");
        }

        assert!(
            Cli::try_parse_from(
                ["nanocodex2", "host", "--scope", "agent"]
                    .into_iter()
                    .chain(common),
            )
            .is_err()
        );
        let agent = Cli::try_parse_from(
            [
                "nanocodex2",
                "host",
                "--scope",
                "agent",
                "--agent",
                "agent-1",
            ]
            .into_iter()
            .chain(common),
        )
        .unwrap();
        let Some(Command::Host(agent)) = agent.command else {
            panic!("agent host parsed into the wrong command")
        };
        agent.validate().unwrap();

        let system_with_agent = Cli::try_parse_from(
            [
                "nanocodex2",
                "host",
                "--scope",
                "system",
                "--agent",
                "agent-1",
            ]
            .into_iter()
            .chain(common),
        )
        .unwrap();
        let Some(Command::Host(system_with_agent)) = system_with_agent.command else {
            panic!("system host parsed into the wrong command")
        };
        assert!(system_with_agent.validate().is_err());
    }
}
