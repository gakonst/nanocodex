//! Code shared by the two role-split executables: the `nanocodex` CLI
//! (nanocodex-bin) and the `nanocodex-hand` daemon (nanocodex-hand-daemon).
//!
//! Neither executable depends on the other's implementation. This crate
//! holds only what both need: install-root and executable-role discovery,
//! the Hand's client-side IPC lease and state layout, Hand and VM-host
//! command definitions, startup timing, and small shared helpers.

pub mod computer;
pub mod ffmpeg;
pub mod hand_args;
pub mod hand_client;
pub mod hand_executable;
#[cfg(target_os = "macos")]
pub mod hand_keep_awake;
pub mod hand_observability;
pub mod host;
pub mod launcher;
pub mod screen_ice;
pub mod startup_timing;
pub mod version;
pub mod voice_recording;

// Opus (through nanocodex-remote) is built with GCC stack protection on
// windows-gnu. Link its runtime statically into every executable that uses this
// crate, so neither the CLI nor the standalone Hand needs an extra libssp DLL.
// The empty block only carries the link directive; -bundle defers it to the
// final link exactly like a build-script `rustc-link-lib`.
#[cfg(all(target_os = "windows", target_env = "gnu"))]
#[allow(
    unsafe_code,
    reason = "an empty extern block declares only a native link"
)]
#[link(name = "ssp", kind = "static", modifiers = "+whole-archive,-bundle")]
unsafe extern "C" {}

use nanocodex_managed::ManagedError;

/// Run a command future on a multi-threaded Tokio runtime and return without
/// waiting for disposable background work.
pub fn run_with_runtime(
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
