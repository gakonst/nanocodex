//! Verify explicitly supplied local binaries before installing them.
//!
//! A local CLI with a Hand (`--hand-binary` or a sibling `nanocodex-hand`) must
//! come from one source tree: both report the same `Hand Identity` (the CLI
//! reports the identity of the Hand built beside it), or, for builds that
//! predate Hand identities, the same exact source revision. A CLI alone only
//! needs a working probe.

use std::{path::Path, time::Duration};

use eyre::{Context, Result, bail, eyre};
use tokio::process::Command;

const REBUILD_PAIR: &str = "Rebuild both executables from one checkout (cargo build) and pass target/<profile>/nanocodex with --path; the nanocodex-hand beside it (or --hand-binary) must come from the same checkout. Use a release update to install historical release bundles.";

/// A CLI installed without a Hand only needs to execute its version probe.
pub(super) async fn verify_single(binary: &Path) -> Result<()> {
    let binary = binary
        .canonicalize()
        .wrap_err_with(|| format!("failed to locate local binary {}", binary.display()))?;
    let version = version_output(&binary).await?;
    match source_revision(&version) {
        Some(revision) => eprintln!(
            "Verified local binary {} at source revision {}",
            binary.display(),
            revision.to_ascii_lowercase()
        ),
        None => eprintln!("Verified local binary {}", binary.display()),
    }
    Ok(())
}

/// Returns the pair's Hand identity when both binaries report one.
pub(super) async fn verify_pair(cli: &Path, companion: &Path) -> Result<Option<String>> {
    let cli = cli
        .canonicalize()
        .wrap_err_with(|| format!("failed to locate local CLI {}", cli.display()))?;
    let companion = companion
        .canonicalize()
        .wrap_err_with(|| format!("failed to locate local Hand {}", companion.display()))?;
    let (cli_version, companion_version) =
        tokio::try_join!(version_output(&cli), version_output(&companion))?;
    if let Some(identity) = hand_identity(&companion_version) {
        match hand_identity(&cli_version) {
            Some(expected) if expected == identity => {
                eprintln!(
                    "Verified local Hand identity {identity}: CLI {}, Hand {}",
                    cli.display(),
                    companion.display(),
                );
                return Ok(Some(identity));
            }
            Some(expected) => bail!(
                "local Hand identity {identity} differs from the Hand identity {expected} the CLI was built with. {REBUILD_PAIR}"
            ),
            None => bail!(
                "local CLI does not report the Hand identity it was built with. {REBUILD_PAIR}"
            ),
        }
    }
    if source_revision(&cli_version).is_none() && source_revision(&companion_version).is_none() {
        // Plain development builds record no release provenance. Verify the
        // runtime contract the pair actually shares instead: one package
        // version and the Hand service protocol. Without an identity, the
        // store keeps the exact installed bytes as the Hand's receipt.
        let version = package_version(&cli_version)
            .filter(|version| package_version(&companion_version) == Some(*version))
            .ok_or_else(|| {
                eyre!("local CLI and Hand report different package versions. {REBUILD_PAIR}")
            })?;
        crate::hand_service::validate_candidate(&companion).await?;
        eprintln!(
            "Verified local development pair {version} with Hand service protocol 1 (no recorded source revision): CLI {}, Hand {}",
            cli.display(),
            companion.display(),
        );
        return Ok(None);
    }
    let revision = matching_revision(&cli_version, &companion_version)?;
    eprintln!(
        "Verified local source revision {revision}: CLI {}, Hand {}",
        cli.display(),
        companion.display(),
    );
    Ok(None)
}

/// Package version (without channel suffix) from a `Version: X` line.
fn package_version(version: &str) -> Option<&str> {
    let value = version
        .lines()
        .find_map(|line| line.split_once("Version: ").map(|(_, value)| value.trim()))?;
    value.split(['-', '+', ' ']).next().filter(|value| !value.is_empty())
}

/// The deterministic Hand identity an executable reports, if any. Probe
/// failures read as unknown: callers then compare bytes instead.
pub(super) async fn probe_hand_identity(path: &Path) -> Option<String> {
    let output = version_output(path).await.ok()?;
    hand_identity(&output)
}

/// Exactly one `Hand Identity: <64 lowercase hex>` line.
pub(super) fn hand_identity(version: &str) -> Option<String> {
    let mut identities = version
        .lines()
        .filter_map(|line| line.strip_prefix("Hand Identity: "));
    let identity = identities.next()?.trim();
    (identities.next().is_none()
        && identity.len() == 64
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| identity.to_owned())
}

async fn version_output(path: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(path)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .wrap_err_with(|| {
        format!(
            "version probe timed out for {}. {REBUILD_PAIR}",
            path.display()
        )
    })?
    .wrap_err_with(|| {
        format!(
            "version probe failed for {}. {REBUILD_PAIR}",
            path.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "version probe failed for {}. {REBUILD_PAIR}",
            path.display()
        );
    }
    String::from_utf8(output.stdout).wrap_err_with(|| {
        format!(
            "invalid version output from {}. {REBUILD_PAIR}",
            path.display()
        )
    })
}

fn matching_revision(cli: &str, companion: &str) -> Result<String> {
    let cli = source_revision(cli).ok_or_else(|| {
        eyre!("local CLI does not report an exact source revision. {REBUILD_PAIR}")
    })?;
    let companion = source_revision(companion).ok_or_else(|| {
        eyre!("local Hand does not report an exact source revision. {REBUILD_PAIR}")
    })?;
    if !cli.eq_ignore_ascii_case(companion) {
        bail!("local CLI source revision {cli} differs from Hand {companion}. {REBUILD_PAIR}");
    }
    Ok(cli.to_ascii_lowercase())
}

fn source_revision(version: &str) -> Option<&str> {
    let mut revisions = version
        .lines()
        .filter_map(|line| line.strip_prefix("Commit SHA: "));
    let revision = revisions.next()?.trim();
    (revisions.next().is_none()
        && revision.len() == 40
        && revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    fn version(binary: &str, revision: &str, timestamp: &str, profile: &str) -> String {
        format!(
            "{binary} Version: 0.6.4-dev\nCommit SHA: {revision}\nBuild Timestamp: {timestamp}\nBuild Profile: {profile}\n"
        )
    }

    #[test]
    fn accepts_matching_revision_without_requiring_identical_build_settings() {
        let cli = version("nanocodex-bin", REVISION, "2026-01-01T00:00:00Z", "release");
        let companion = version(
            "nanocodex2",
            &REVISION.to_ascii_uppercase(),
            "2026-01-02T00:00:00Z",
            "nightly",
        );
        assert_eq!(matching_revision(&cli, &companion).unwrap(), REVISION);
    }

    #[test]
    fn rejects_mixed_sources_even_when_package_versions_match() {
        let cli = version("nanocodex-bin", REVISION, "now", "release");
        let companion = version(
            "nanocodex2",
            "fedcba9876543210fedcba9876543210fedcba98",
            "now",
            "release",
        );
        let error = matching_revision(&cli, &companion).unwrap_err().to_string();
        assert!(error.contains("differs from Hand"));
        assert!(error.contains("Rebuild both executables from one checkout (cargo build)"));
    }

    #[test]
    fn rejects_unidentified_historical_companions_for_local_pairing() {
        let cli = version("nanocodex-bin", REVISION, "now", "release");
        let error = matching_revision(&cli, "nanocodex2 0.6.4\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("local Hand does not report an exact source revision"));
        assert!(error.contains("Use a release update"));
        assert!(matching_revision("nanocodex 0.6.4\n", &cli).is_err());
    }

    #[test]
    fn rejects_missing_abbreviated_invalid_and_ambiguous_revisions() {
        for output in [
            String::new(),
            "Commit SHA: VERGEN_IDEMPOTENT_OUTPUT".into(),
            "Commit SHA: 0123456789".into(),
            format!("Commit SHA: {}", "g".repeat(40)),
            format!("Commit SHA: {REVISION}\nCommit SHA: {REVISION}"),
        ] {
            assert!(source_revision(&output).is_none(), "{output}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_bad_pairs_before_installing_any_candidate_files() {
        let directory = tempfile::Builder::new()
            .prefix("local pair ")
            .tempdir()
            .unwrap();
        let binary = |name: &str, output: &str| {
            let path = directory.path().join(name);
            super::super::store::atomic_write(
                &path,
                format!("#!/bin/sh\n[ \"$1\" = --version ] || exit 2\nprintf '%s\\n' '{output}'\n")
                    .as_bytes(),
                true,
            )
            .unwrap();
            path
        };
        let cli_version = version("nanocodex-bin", REVISION, "now", "release");
        let cli = binary("nanocodex", &cli_version);
        for companion_version in [
            String::from("nanocodex2 0.6.4"),
            version(
                "nanocodex2",
                "fedcba9876543210fedcba9876543210fedcba98",
                "now",
                "release",
            ),
        ] {
            let companion = binary("nanocodex2", &companion_version);
            let store = super::super::store::VersionStore::at(directory.path().join("install"));
            let error = super::super::install_local_binary(
                &cli,
                Some(&companion),
                None,
                &store,
                "previous",
                false,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(REBUILD_PAIR));
            assert!(!store.root().exists());
        }
        let companion = binary("nanocodex2", &cli_version);
        verify_pair(&cli, &companion).await.unwrap();
        super::super::store::atomic_write(&companion, b"#!/bin/sh\nexit 1\n", true).unwrap();
        assert!(verify_pair(&cli, &companion).await.is_err());
    }
}
