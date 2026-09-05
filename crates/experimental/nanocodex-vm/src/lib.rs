#![cfg_attr(feature = "host", doc = include_str!("../README.md"))]
#![cfg_attr(not(feature = "host"), doc = include_str!("../GUEST_RUNTIME.md"))]
#![deny(unsafe_code, missing_docs, rustdoc::broken_intra_doc_links)]

#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod capabilities;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod child_lifetime;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
pub use child_lifetime::terminate_child_with_parent;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod command;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod config;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod egress;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod gvproxy;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
pub mod image;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod krun;
#[cfg(any(
    all(feature = "guest-runtime", target_os = "linux"),
    all(
        feature = "host",
        any(
            all(target_os = "linux", not(target_env = "musl")),
            all(target_os = "macos", target_arch = "aarch64")
        )
    )
))]
mod overlay;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod process;
#[cfg(any(
    feature = "guest-runtime",
    all(
        feature = "host",
        any(
            all(target_os = "linux", not(target_env = "musl")),
            all(target_os = "macos", target_arch = "aarch64")
        )
    )
))]
pub mod tools;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod workspace;

/// Low-level host-side VM configuration and lifecycle components.
///
/// Most applications should start with [`crate::VmWorkspaceBuilder`]. This
/// module is for custom VMM entry points, network/egress policy, and direct
/// libkrun lifecycle ownership.
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
pub mod host {
    pub use crate::{
        capabilities::{Capabilities, KrunFeature},
        command::GuestCommand,
        config::{BlockDevice, Network, RootFilesystem, SharedDirectory, VmConfig},
        egress::{
            EgressError, EgressFile, EgressLease, EgressMount, GUEST_EGRESS_ROOT,
            MAX_EGRESS_FILE_BYTES,
        },
        gvproxy::{Gvproxy, GvproxyError},
        krun::{KrunVm, KrunVmControl, VmError},
        overlay::{OverlayDiskError, create_sparse_overlay_disk, overlay_guest_command},
        process::{PrivateVmProcessConfig, VmProcessConfig, VmProcessError},
    };
}

#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
pub use workspace::{VmWorkspace, VmWorkspaceBuilder, VmWorkspaceError};
