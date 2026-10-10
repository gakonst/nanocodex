//! Build and version information for the Nanocodex executables.
//!
//! The libraries carry no build-time provenance. Each executable crate
//! expands [`build_info!`](crate::build_info) at its thin entry point and passes
//! the result to `nanocodex_cli::cli_main` or `nanocodex_hand_daemon::hand_main`, so commit,
//! release-tag and Hand-identity environment variables are compile inputs of
//! the two tiny binary targets only. Commits, branch switches and release
//! metadata therefore relink the executables instead of recompiling the
//! library and every unit that depends on it.

use std::sync::OnceLock;

/// Release provenance captured by an executable crate.
#[derive(Clone, Copy, Debug)]
pub struct BuildInfo {
    /// Package version of the executable.
    pub version: &'static str,
    /// Full Git commit (`VERGEN_GIT_SHA`), set by release and source-update builds.
    pub git_sha: Option<&'static str>,
    /// Release tag (`TAG_NAME`); nightly tags select the nightly channel.
    pub tag: Option<&'static str>,
    /// Release reuse identity of the Hand (`NANOCODEX_HAND_IDENTITY`).
    pub hand_identity: Option<&'static str>,
    /// `release` for optimized builds, `debug` otherwise.
    pub profile: &'static str,
}

/// Captures [`BuildInfo`] from the calling crate's compile environment.
#[macro_export]
macro_rules! build_info {
    () => {
        $crate::version::BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            git_sha: option_env!("VERGEN_GIT_SHA"),
            tag: option_env!("TAG_NAME"),
            hand_identity: option_env!("NANOCODEX_HAND_IDENTITY"),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        }
    };
}

/// Full revision reported by unit tests, which run without an executable.
#[cfg(test)]
const TEST_GIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

static BUILD: OnceLock<BuildInfo> = OnceLock::new();

/// Records the executable's provenance; the first call wins.
pub fn init(build: BuildInfo) {
    let _ = BUILD.set(build);
}

fn build() -> &'static BuildInfo {
    BUILD.get_or_init(|| BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        #[cfg(test)]
        git_sha: Some(TEST_GIT_SHA),
        #[cfg(not(test))]
        git_sha: None,
        tag: None,
        hand_identity: None,
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    })
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Full Git commit of this build, when the build recorded one.
pub fn git_sha() -> Option<&'static str> {
    non_empty(build().git_sha)
}

/// Release reuse identity of the Hand built with this CLI, when recorded.
pub fn hand_identity() -> Option<&'static str> {
    non_empty(build().hand_identity)
}

/// Whether this binary was produced by the nightly release channel.
pub fn is_nightly() -> bool {
    non_empty(build().tag).is_some_and(|tag| tag.contains("nightly"))
}

/// Package version plus channel suffix: none for a stable tag, `-nightly`
/// for nightly tags and `-dev` otherwise.
fn channel_version() -> String {
    let build = build();
    let suffix = match non_empty(build.tag) {
        Some(tag) if tag.contains("nightly") => "-nightly",
        Some(tag) if tag == build.version || tag.strip_prefix('v') == Some(build.version) => "",
        _ => "-dev",
    };
    format!("{}{suffix}", build.version)
}

/// `debug` for unoptimized builds, `nightly` for optimized nightly-tagged
/// builds and `release` otherwise.
fn profile() -> &'static str {
    match build().profile {
        "release" if is_nightly() => "nightly",
        profile => profile,
    }
}

fn short_sha() -> &'static str {
    git_sha().map_or("unknown", |sha| sha.get(..sha.len().min(10)).unwrap_or(sha))
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

/// SemVer-compatible build identity including commit and profile.
pub fn semver() -> &'static str {
    static VALUE: OnceLock<&'static str> = OnceLock::new();
    VALUE.get_or_init(|| {
        leak(format!(
            "{}+{}.{}",
            channel_version(),
            short_sha(),
            profile()
        ))
    })
}

/// Compact CLI version displayed by `nanocodex --version`.
pub fn short() -> &'static str {
    static VALUE: OnceLock<&'static str> = OnceLock::new();
    VALUE.get_or_init(|| leak(format!("{} ({})", channel_version(), short_sha())))
}

/// Detailed CLI version displayed by the long version flag. The Commit SHA
/// and Hand Identity lines appear only when the build recorded them.
pub fn long() -> &'static str {
    static VALUE: OnceLock<&'static str> = OnceLock::new();
    VALUE.get_or_init(|| {
        let mut lines = vec![format!("Version: {}", channel_version())];
        if let Some(sha) = git_sha() {
            lines.push(format!("Commit SHA: {sha}"));
        }
        lines.push(format!("Build Profile: {}", profile()));
        if let Some(identity) = hand_identity() {
            lines.push(format!("Hand Identity: {identity}"));
        }
        leak(lines.join("\n"))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_version_reports_the_recorded_source_revision() {
        assert!(super::long().contains(&format!("Commit SHA: {}", super::TEST_GIT_SHA)));
        assert!(super::long().contains("Build Profile: "));
    }
}
