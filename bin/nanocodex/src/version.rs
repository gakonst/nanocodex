//! Build and version information for the Nanocodex CLI.

/// SemVer-compatible build identity including commit, timestamp, and profile.
pub(crate) const SEMVER_VERSION: &str = env!("NANOCODEX_SEMVER_VERSION");

/// Compact CLI version displayed by `nanocodex --version`.
pub(crate) const SHORT_VERSION: &str = env!("NANOCODEX_SHORT_VERSION");

/// Detailed CLI version displayed by the long version flag.
pub(crate) const LONG_VERSION: &str = concat!(
    env!("NANOCODEX_LONG_VERSION_0"),
    "\n",
    env!("NANOCODEX_LONG_VERSION_1"),
    "\n",
    env!("NANOCODEX_LONG_VERSION_2"),
    "\n",
    env!("NANOCODEX_LONG_VERSION_3"),
    "\nHand Identity: ",
    env!("NANOCODEX_HAND_IDENTITY"),
);

/// Deterministic identity of the nanocodex-hand built from this source tree
/// (build_hand_identity.rs). Independent of Git revision and build time, so
/// CLI-only changes keep it stable and installed Hands keep running.
#[allow(dead_code)]
pub(crate) const HAND_IDENTITY: &str = env!("NANOCODEX_HAND_IDENTITY");

/// Whether this binary was produced by the nightly release channel.
pub(crate) const IS_NIGHTLY: bool = option_env!("NANOCODEX_IS_NIGHTLY").is_some();
