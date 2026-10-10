//! Daemon-only entry of the nanocodex-hand executable.
//!
//! This tree contains only Hand serving and daemon-side entrypoints. It never
//! reaches the managed or local CLI command trees, the terminal UI, or the
//! updater, so the linker keeps only Hand code in the Hand executable. Every
//! other command is forwarded by crate::hand_main to the sibling CLI.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{CommandFactory as _, FromArgMatches as _, Parser, Subcommand};
use nanocodex_cli_auth::client_from_environment;
use nanocodex_managed::ManagedError;

#[cfg(target_os = "linux")]
use super::screen_host;
use nanocodex_bin_shared::hand_args::HandRecordingArgs;

use super::{
    Hand, Host, VmRunConfig, device_hand, launcher, native_hand, screen_native, startup_timing,
    vm_hand, vm_host,
};

/// Version reported by the Hand: its package version and the content
/// identity of its source and dependency closure. It deliberately carries no
/// repository commit or build timestamp, which change with CLI-only commits.
fn hand_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        let mut version = format!("Version: {}", env!("CARGO_PKG_VERSION"));
        // The release reuse identity, when the build recorded one; otherwise
        // the line is omitted and an updater falls back to byte comparison.
        if let Some(identity) = crate::version::hand_identity() {
            version.push_str(
                "
Hand Identity: ",
            );
            version.push_str(identity);
        } else if let Some(sha) = crate::version::git_sha() {
            // Without an identity, local pair verification matches the exact
            // source revision of the CLI and Hand instead.
            version.push_str("\nCommit SHA: ");
            version.push_str(sha);
        }
        version
    })
}

#[derive(Parser)]
#[command(
    name = "nanocodex-hand",
    about = "Nanocodex Hand daemon; user commands are provided by the nanocodex CLI",
    disable_help_subcommand = true
)]
pub(crate) struct DaemonCli {
    #[command(subcommand)]
    pub(crate) command: DaemonCommand,
}

#[derive(Subcommand)]
pub(crate) enum DaemonCommand {
    /// Serve this computer as a Hand; --vm or --docker serve an isolated Hand.
    Hand(Hand),
    #[command(name = "__device-hand", hide = true)]
    DeviceHand(device_hand::DeviceHand),
    #[command(name = "__hand-screen", hide = true)]
    HandScreen(screen_native::ScreenCommand),
    #[cfg(target_os = "linux")]
    #[command(name = "__hand-desktop", hide = true)]
    HandDesktop(screen_native::DesktopCommand),
    #[cfg(target_os = "linux")]
    #[command(name = "__install-hand", hide = true)]
    InstallHand,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[command(name = "__update-hand", hide = true)]
    UpdateHand,
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
    #[command(name = "__vm-run-config", hide = true)]
    VmRunConfig(VmRunConfig),
    /// Historical spelling used by VMM children of older Hands.
    #[command(name = "vm-run-config", hide = true)]
    LegacyVmRunConfig(VmRunConfig),
    #[command(name = "__vm-clone-image", hide = true)]
    VmCloneImage {
        source: PathBuf,
        destination: PathBuf,
    },
    /// Control this Hand's recorder locally, without account authentication.
    HandRecording(HandRecordingArgs),
}

const LOCAL_RECORDING_CONTROL_FAILED: &str = "local recording control failed";

/// Run one daemon command of the Hand executable.
pub(crate) fn main(arguments: Vec<OsString>) -> ExitCode {
    match try_main(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if matches!(&error, ManagedError::Configuration(message) if message == LOCAL_RECORDING_CONTROL_FAILED)
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
    #[cfg(target_os = "linux")]
    if std::env::var(super::screen_wayland_encoder::HELPER_ENV).as_deref() == Ok("1") {
        return super::run_with_runtime(async {
            super::screen_wayland_encoder::run(std::env::args().skip(1).collect())
                .await
                .map_err(|error| ManagedError::Configuration(error.to_string()))
        });
    }
    // A service validator must probe the Hand itself. A CLI path forwards
    // here marked as forwarded; refusing it keeps a CLI from passing as a Hand.
    if crate::hand_executable::forwarded()
        && arguments
            .get(1)
            .is_some_and(|argument| argument == "__device-hand")
        && arguments
            .iter()
            .any(|argument| argument == "--service-protocol")
    {
        return Err(ManagedError::Configuration(
            "the Hand service protocol is answered only by the Hand executable itself, not through the nanocodex CLI".into(),
        ));
    }
    let _ = dotenvy::dotenv();
    // Report the Hand's own name whatever file name it is installed under
    // (historically `nanocodex2`).
    let command = DaemonCli::command()
        .display_name("nanocodex-hand")
        .version(hand_version())
        .long_version(hand_version());
    let cli = DaemonCli::from_arg_matches(&command.get_matches_from(arguments))
        .unwrap_or_else(|error| error.exit());
    // VMM children are synchronous and must not start Tokio; standalone
    // screen hosts set their environment before any thread starts.
    match cli.command {
        DaemonCommand::VmRunConfig(command) | DaemonCommand::LegacyVmRunConfig(command) => {
            vm_hand::run_config(&command.config)
        }
        DaemonCommand::VmCloneImage {
            source,
            destination,
        } => vm_hand::clone_image(&source, &destination),
        #[cfg(target_os = "linux")]
        DaemonCommand::WaylandHost(args) => serve_screen_host(args, screen_host::Mode::Wayland),
        #[cfg(target_os = "linux")]
        DaemonCommand::DesktopHost(args) => serve_screen_host(args, screen_host::Mode::Desktop),
        #[cfg(target_os = "linux")]
        DaemonCommand::ServerHost(args) => serve_screen_host(args, screen_host::Mode::Server),
        command => super::run_with_runtime(run(command)),
    }
}

#[cfg(target_os = "linux")]
fn serve_screen_host(
    args: screen_host::HostCommand,
    mode: screen_host::Mode,
) -> Result<(), ManagedError> {
    let (prepared, environment) = args.prepare(mode)?;
    // SAFETY: only standalone process startup reaches this point. No Tokio,
    // capture, audio, or provider threads have been started yet.
    for (key, value) in environment {
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(key, value);
        }
    }
    super::run_with_runtime(screen_host::serve(prepared))
}

async fn run(command: DaemonCommand) -> Result<(), ManagedError> {
    match command {
        DaemonCommand::HandRecording(command) => super::hand_recording_control::run(&command)
            .await
            .map_err(|_| ManagedError::Configuration(LOCAL_RECORDING_CONTROL_FAILED.into())),
        DaemonCommand::Hand(command) if command.rootfs.is_none() && command.docker.is_none() => {
            native_hand::serve_hand(command).await
        }
        DaemonCommand::Hand(command) => super::serve_isolated_hand(command).await,
        DaemonCommand::DeviceHand(command) => device_hand::serve(command).await,
        DaemonCommand::HandScreen(command) => {
            let client = client_from_environment(None)?;
            screen_native::serve(&client, command).await
        }
        #[cfg(target_os = "linux")]
        DaemonCommand::HandDesktop(command) => screen_native::serve_desktop(command).await,
        #[cfg(target_os = "linux")]
        DaemonCommand::InstallHand => super::linux_hand_install::run().await,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        DaemonCommand::UpdateHand => super::linux_hand_update::run().await,
        DaemonCommand::Host(command) => {
            let _observability = command
                .observability
                .install()
                .map_err(|error| ManagedError::Configuration(error.to_string()))?;
            vm_host::serve(command).await
        }
        #[cfg(target_os = "linux")]
        DaemonCommand::WaylandHost(_)
        | DaemonCommand::DesktopHost(_)
        | DaemonCommand::ServerHost(_) => unreachable!("handled before runtime startup"),
        DaemonCommand::VmRunConfig(_)
        | DaemonCommand::LegacyVmRunConfig(_)
        | DaemonCommand::VmCloneImage { .. } => unreachable!("handled before runtime startup"),
    }
}
