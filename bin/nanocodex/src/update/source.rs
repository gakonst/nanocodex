//! Build an explicitly selected upstream revision before installing it.
//!
//! Current revisions build the `nanocodex` CLI and the `nanocodex-hand` daemon
//! from package nanocodex-bin; the Hand is installed under its service file name
//! `nanocodex2`. Historical revisions build their `nanocodex2-bin` pair, and the
//! brief single-binary revisions install their one binary under both names.

use std::{
    path::{Path, PathBuf},
    process::Output,
};

use eyre::{Context, Result, bail, eyre};
use serde::Deserialize;
use tokio::process::Command;

use super::{REPOSITORY, local};

const SOURCE_URL: &str = "https://github.com/gakonst/nanocodex.git";

#[derive(Clone, Copy)]
pub(super) enum Selection<'a> {
    Branch(&'a str),
    Pr(u64),
}

impl Selection<'_> {
    pub(super) fn key_prefix(self) -> String {
        match self {
            Self::Branch(_) => "branch".into(),
            Self::Pr(number) => format!("pr-{number}"),
        }
    }

    pub(super) fn description(self) -> String {
        match self {
            Self::Branch(name) => format!("branch {name}"),
            Self::Pr(number) => format!("PR #{number}"),
        }
    }

    fn reference(self) -> String {
        match self {
            Self::Branch(name) => format!("refs/heads/{name}"),
            Self::Pr(number) => format!("refs/pull/{number}/head"),
        }
    }
}

pub(super) struct Build {
    pub(super) sha: String,
    pub(super) cli: Vec<u8>,
    pub(super) hand: Vec<u8>,
    /// Identity the compiled Hand reports (absent for older source trees).
    pub(super) hand_identity: Option<String>,
}

/// How the fetched revision packages its CLI and Hand.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// nanocodex-bin's `nanocodex` CLI plus its `nanocodex-hand` daemon.
    Split,
    /// Historical `nanocodex2-bin` package beside nanocodex-bin.
    Pair,
    /// One `nanocodex` binary serving as both CLI and Hand.
    Single,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    head_ref_oid: String,
    state: String,
}

pub(super) async fn build(
    selection: Selection<'_>,
    checkout: &Path,
    target: &Path,
) -> Result<Build> {
    let expected_sha = match selection {
        Selection::Branch(name) => {
            if name.is_empty() || name.starts_with('-') {
                bail!("invalid branch name: {name}");
            }
            git(None, &["check-ref-format", "--branch", name]).await?;
            None
        }
        Selection::Pr(number) => {
            let output = Command::new("gh")
                .args([
                    "pr",
                    "view",
                    &number.to_string(),
                    "--repo",
                    REPOSITORY,
                    "--json",
                    "headRefOid,state",
                ])
                .output()
                .await
                .wrap_err("gh is required to inspect the pull request")?;
            let bytes = successful(output, "inspect the pull request")?;
            let pr: PullRequest = serde_json::from_slice(&bytes)
                .wrap_err("gh returned invalid pull request metadata")?;
            if pr.state != "OPEN" {
                bail!(
                    "pull request #{number} is {}; refusing to build a stale head",
                    pr.state
                );
            }
            Some(pr.head_ref_oid)
        }
    };

    // Cargo fingerprints include source paths. Keep this updater-owned checkout
    // stable across revisions, under the same update lock as installation.
    std::fs::create_dir_all(checkout).wrap_err("failed to create source checkout")?;
    let checkout = checkout.canonicalize()?;
    let root = checkout.as_path();
    if !root.join(".git").exists() {
        git(Some(root), &["init", "--quiet"]).await?;
    }
    eprintln!("fetching nanocodex {}...", selection.description());
    let reference = selection.reference();
    git(
        Some(root),
        &["fetch", "--depth", "1", SOURCE_URL, &reference],
    )
    .await?;
    let sha = String::from_utf8(git(Some(root), &["rev-parse", "FETCH_HEAD"]).await?)
        .wrap_err("git returned an invalid source revision")?
        .trim()
        .to_owned();
    if let Some(expected) = expected_sha
        && sha != expected
    {
        bail!(
            "pull request head changed while fetching: expected {expected}, fetched {sha}; retry the update"
        );
    }

    // Do not rewrite HEAD on an unchanged revision: build scripts track it.
    // A newly initialized checkout has no HEAD yet.
    let head = git(Some(root), &["rev-parse", "--verify", "HEAD"])
        .await
        .unwrap_or_default();
    let dirty = git(
        Some(root),
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .await?;
    if head != format!("{sha}\n").as_bytes() || !dirty.is_empty() {
        git(
            Some(root),
            &["checkout", "--quiet", "--force", "--detach", "FETCH_HEAD"],
        )
        .await?;
    }
    git(Some(root), &["clean", "--quiet", "-ffdx"]).await?;

    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    // Build helpers from the fetched revision's pinned inputs, never from the
    // updating CLI's checkout or an arbitrary inherited bundle. Keep them alive
    // until both the build and shipped-payload verification have completed.
    let helpers = tempfile::tempdir().wrap_err("failed to create helper build directory")?;
    let layout = layout(root).await?;
    let screen_bundle =
        prepare_screen_bundle(root, helpers.path(), &sha, layout == Layout::Pair).await?;
    // Resolve shared dependency features once and use the optimized profile
    // without release LTO, matching the nightly build's faster feedback.
    let what = match layout {
        Layout::Split => "nanocodex and nanocodex-hand",
        Layout::Pair => "nanocodex and nanocodex2",
        Layout::Single => "nanocodex",
    };
    eprintln!("compiling {what} at {sha}...");
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .env_remove("NANOCODEX_LINUX_SCREEN_BUNDLE")
        .env("CARGO_TARGET_DIR", &target)
        .env("VERGEN_GIT_SHA", &sha)
        .env("STABLE_GIT_COMMIT", &sha)
        .args([
            "build",
            "--locked",
            "--profile",
            "nightly",
            "--timings",
            "--package",
            "nanocodex-bin",
            "--bin",
            "nanocodex",
        ]);
    match layout {
        Layout::Split => {
            command.args(["--bin", "nanocodex-hand"]);
        }
        Layout::Pair => {
            command.args(["--package", "nanocodex2-bin", "--bin", "nanocodex2"]);
        }
        Layout::Single => {}
    }
    command.args(["--features", "nanocodex-bin/tempo"]);
    if let Some(bundle) = &screen_bundle {
        command.env("NANOCODEX_LINUX_SCREEN_BUNDLE", bundle);
    }
    let status = command
        .status()
        .await
        .wrap_err_with(|| format!("failed to start cargo while compiling {what}"))?;
    if !status.success() {
        bail!("cargo failed while compiling {what}: {status}");
    }
    let extension = if cfg!(windows) { ".exe" } else { "" };
    let cli_path = target.join("nightly").join(format!("nanocodex{extension}"));
    let hand_path = match layout {
        Layout::Split => target
            .join("nightly")
            .join(format!("nanocodex-hand{extension}")),
        Layout::Pair => target
            .join("nightly")
            .join(format!("nanocodex2{extension}")),
        Layout::Single => cli_path.clone(),
    };
    if cfg!(target_os = "macos") {
        let status = Command::new("codesign")
            .args(["--force", "--sign", "-", "--entitlements"])
            .arg(root.join("nanocodex-vm.entitlements"))
            .arg(&hand_path)
            .status()
            .await
            .wrap_err("failed to sign the locally compiled Hand")?;
        if !status.success() {
            bail!("failed to sign the locally compiled Hand: {status}");
        }
    }
    let hand_identity = if layout == Layout::Single {
        local::verify_single(&cli_path).await?;
        None
    } else {
        local::verify_pair(&cli_path, &hand_path).await?
    };
    if let Some(bundle) = screen_bundle {
        let status = Command::new("python3")
            .arg(root.join("scripts/tests/linux-screen-helpers-bundle.py"))
            .arg("--binary")
            .arg(&hand_path)
            .arg(&bundle)
            .status()
            .await
            .wrap_err(
                "python3 is required to verify the compiled Hand's embedded screen payload",
            )?;
        if !status.success() {
            bail!(
                "compiled Hand screen payload verification failed: {status}; nothing was installed"
            );
        }
    }
    let cli = std::fs::read(&cli_path).wrap_err("failed to read the compiled CLI")?;
    let hand = if layout == Layout::Single {
        cli.clone()
    } else {
        std::fs::read(&hand_path).wrap_err("failed to read the compiled Hand")?
    };
    Ok(Build {
        sha,
        cli,
        hand,
        hand_identity,
    })
}

async fn layout(root: &Path) -> Result<Layout> {
    if has_legacy_hand_package(root)? {
        return Ok(Layout::Pair);
    }
    let output = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .await
        .wrap_err("failed to start cargo while inspecting the fetched workspace")?;
    let metadata: serde_json::Value =
        serde_json::from_slice(&successful(output, "inspect the fetched Cargo workspace")?)
            .wrap_err("cargo returned invalid workspace metadata")?;
    let split = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|package| package["name"] == "nanocodex-bin")
        .flat_map(|package| package["targets"].as_array().into_iter().flatten())
        .any(|target| {
            target["name"] == "nanocodex-hand"
                && target["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        });
    Ok(if split { Layout::Split } else { Layout::Single })
}

/// Historical revisions declare a separate `nanocodex2-bin` workspace package;
/// every workspace package is recorded in the lockfile used by `--locked`.
fn has_legacy_hand_package(root: &Path) -> Result<bool> {
    let lockfile = match std::fs::read_to_string(root.join("Cargo.lock")) {
        Ok(lockfile) => lockfile,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).wrap_err("failed to read the fetched Cargo.lock"),
    };
    Ok(lockfile
        .lines()
        .any(|line| line.trim() == r#"name = "nanocodex2-bin""#))
}

async fn prepare_screen_bundle(
    root: &Path,
    work: &Path,
    sha: &str,
    legacy_pair: bool,
) -> Result<Option<PathBuf>> {
    if !cfg!(target_os = "linux") {
        return Ok(None);
    }
    if !cfg!(target_arch = "x86_64") {
        bail!(
            "self-contained Linux source updates require x86_64; this architecture has no supported Wayland helper payload"
        );
    }
    // The unified package's build script embeds the helpers; historical pairs
    // embedded them from the separate nanocodex2 package build script.
    let embedding_build_script = if legacy_pair {
        "bin/nanocodex/nanocodex2/build.rs"
    } else {
        "bin/nanocodex/build.rs"
    };
    for file in [
        "scripts/build-linux-screen-helpers.sh",
        "scripts/build-linux-screen-helpers.py",
        "scripts/build-linux-screen-helpers.Dockerfile",
        "scripts/tests/linux-screen-helpers-bundle.py",
        embedding_build_script,
        "bin/nanocodex/src/nanocodex2/screen_helpers.rs",
    ] {
        if !root.join(file).is_file() {
            bail!(
                "source revision {sha} predates the self-contained Linux screen-helper packaging contract (missing {file}); refusing to install a Hand without helpers. Use an explicitly selected historical release only if its legacy host-provisioned screen dependencies are acceptable"
            );
        }
    }
    let bundle = work.join("linux-screen-helpers.tar.gz");
    eprintln!("preparing embedded Linux Waymote/Grim helpers from source revision {sha}...");
    let status = Command::new("bash")
        .current_dir(root)
        .env_remove("NANOCODEX_LINUX_SCREEN_BUNDLE")
        .arg(root.join("scripts/build-linux-screen-helpers.sh"))
        .arg("--auto")
        .arg(&bundle)
        .arg(work.join("build"))
        .status()
        .await
        .wrap_err("bash is required to prepare Linux screen helpers")?;
    if !status.success() {
        bail!(
            "failed to prepare Linux screen helpers at {sha}: {status}; install the documented native build prerequisites or provide a working Docker daemon. No host packages were installed and no candidate binaries were activated"
        );
    }
    let size = std::fs::metadata(&bundle)
        .wrap_err("helper builder did not produce the requested Linux screen bundle")?
        .len();
    if size == 0 || size > 64 * 1024 * 1024 {
        bail!("helper builder produced an empty or oversized Linux screen bundle");
    }
    Ok(Some(bundle))
}

async fn git(cwd: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .args(args)
        .output()
        .await
        .wrap_err("git is required to fetch Nanocodex source")?;
    successful(output, "fetch Nanocodex source")
}

fn successful(output: Output, action: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        return Err(eyre!(
            "failed to {action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}
