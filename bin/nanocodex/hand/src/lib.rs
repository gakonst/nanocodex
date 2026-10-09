//! The Nanocodex Hand daemon (`nanocodex-hand`): native computer, screen, VM and
//! Docker Hands and their daemon-side entrypoints.
//!
//! This crate never depends on the CLI command trees or the terminal UI
//! (nanocodex-bin). Every user command is forwarded to the sibling CLI.
#![recursion_limit = "256"]
#![allow(
    clippy::missing_const_for_fn,
    clippy::too_many_arguments,
    clippy::use_self,
    reason = "preserve the reviewed Tact component ownership while adapting its engine boundary"
)]

pub mod daemon;
mod device_hand;
mod hand_recording;
mod hand_recording_control;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod hand_workspace;
mod isolated;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux_hand_install;
#[cfg(any(target_os = "linux", target_os = "macos", all(test, unix)))]
mod linux_hand_update;
mod native_hand;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native_secure_input;
mod observation_providers;
mod screen_audio;
mod screen_broadcast;
#[cfg(target_os = "linux")]
mod screen_gamepad;
#[cfg(target_os = "linux")]
mod screen_helpers;
mod screen_hls;
#[cfg(target_os = "linux")]
mod screen_host;
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
mod vm_factory_credential;
mod vm_hand_config;
mod vm_host;

// Shared with the CLI; keep their historical module paths in this tree.
pub(crate) use nanocodex_bin_shared::hand_args::{
    HandNetwork, HandServe as Hand, Host, HostScope, SYSTEM_HOST_TOKEN_ENV, VmRunConfig,
};
#[cfg(target_os = "macos")]
pub(crate) use nanocodex_bin_shared::hand_keep_awake;
#[cfg(target_os = "macos")]
pub(crate) use nanocodex_bin_shared::voice_recording;
pub(crate) use nanocodex_bin_shared::{
    computer, hand_executable, hand_observability, host, launcher, run_with_runtime,
    startup_timing, version,
};
pub(crate) use nanocodex_cli_auth::client_from_environment;
pub(crate) use nanocodex_managed::validate_vm_factory_name;

use std::{ffi::OsString, process::ExitCode};

use isolated::serve_isolated_hand;
use nanocodex_managed::ManagedError;

fn auth_error(error: nanocodex_cli_auth::Error) -> ManagedError {
    ManagedError::Configuration(error.to_string())
}

fn managed_url_from_environment(fallback: Option<&str>) -> Result<String, ManagedError> {
    nanocodex_cli_auth::managed_url_from_environment(fallback).map_err(auth_error)
}

/// Whether this process is an internal helper selected by environment rather
/// than by its command line.
fn is_helper_process() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::var(screen_wayland_encoder::HELPER_ENV).as_deref() == Ok("1") {
        return true;
    }
    false
}

/// Entry point of the `nanocodex-hand` daemon executable.
///
/// Only daemon commands (and the Hand's own `--version`/`--help`) run here;
/// this crate does not contain the CLI command trees or the terminal UI.
/// Older installations may point `bin/nanocodex2` at this file, so every other
/// invocation, including a leading `--local`, is forwarded unchanged to the
/// CLI installed beside it.
pub fn hand_main(build: nanocodex_bin_shared::version::BuildInfo) -> ExitCode {
    version::init(build);
    hand_executable::set_hand_role();
    hand_executable::take_forwarded();
    let mut arguments: Vec<OsString> = std::env::args_os().collect();
    imply_hand_command(&mut arguments);
    if is_helper_process() || is_hand_daemon_invocation(&arguments) {
        return daemon::main(arguments);
    }
    if hand_executable::forwarded() {
        // The CLI forwarded this here; never bounce it back.
        eprintln!("Error: this command is provided by the nanocodex CLI, not the Hand executable");
        return ExitCode::FAILURE;
    }
    match hand_executable::cli_binary() {
        Ok(cli) => hand_executable::forward(&cli, arguments.first().cloned(), &arguments[1..]),
        Err(error) => {
            eprintln!("Error: {error}; user commands are provided by the nanocodex CLI");
            ExitCode::FAILURE
        }
    }
}

/// Names under which this executable is the `hand` command itself: the build
/// output and the installed `bin/nanocodex-hand` and `bin/nc-hand` links. Bare
/// `nc-hand` (or with serving flags) serves this computer like `nanocodex hand`,
/// `nc-hand --help` is the `hand` help, and `nc-hand status` is
/// `nanocodex hand status`. Every other invocation keeps its meaning: internal
/// entrypoints, explicit `hand`, bare `--version`, a leading `--local`, and CLI
/// commands such as the `computer setup --background` this executable starts.
fn imply_hand_command(arguments: &mut Vec<OsString>) {
    // Retained in stripped builds: the updater links the aliases only to a Hand
    // that contains it.
    std::hint::black_box(hand_executable::HAND_COMMAND_ALIASES_MARKER);
    let Some(name) = arguments
        .first()
        .and_then(|argv0| std::path::Path::new(argv0).file_stem())
        .and_then(|stem| stem.to_str())
    else {
        return;
    };
    if !hand_executable::HAND_COMMAND_ALIASES
        .iter()
        .any(|alias| name.eq_ignore_ascii_case(alias))
    {
        return;
    }
    let implied = match arguments.get(1).map(|argument| argument.to_str()) {
        None => true,
        Some(Some("--version" | "-V")) => arguments.len() > 2,
        Some(Some("--local")) => false,
        Some(Some(first)) => {
            first.starts_with('-') || hand_executable::HAND_SUBCOMMANDS.contains(&first)
        }
        Some(None) => false,
    };
    if implied {
        arguments.insert(1, "hand".into());
    }
}

/// Invocations the Hand executable serves itself, decided without the CLI
/// command trees: daemon entrypoints, `hand` serving (no management
/// subcommand, no help flag), and the Hand's bare `--version`/`--help`.
fn is_hand_daemon_invocation(arguments: &[OsString]) -> bool {
    let argument = |index: usize| arguments.get(index).and_then(|argument| argument.to_str());
    match argument(1) {
        Some("--version" | "-V" | "--help" | "-h") => arguments.len() == 2,
        Some(
            "__device-hand" | "__hand-screen" | "__hand-desktop" | "__install-hand"
            | "__update-hand" | "wayland-host" | "desktop-host" | "server-host" | "__vm-run-config"
            | "vm-run-config" | "__vm-clone-image" | "host" | "hand-recording",
        ) => true,
        // Serving flags start with `-`; a word is a CLI management subcommand.
        Some("hand") => {
            argument(2).is_none_or(|next| next.starts_with('-'))
                && !arguments[2..]
                    .iter()
                    .any(|argument| argument == "-h" || argument == "--help")
        }
        _ => false,
    }
}
