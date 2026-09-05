use std::{fs, path::Path, process::Output};

use eyre::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::process::Command;

use super::{REPOSITORY, checksum_for};

const WORKFLOW: &str = "nightly.yml";

pub(super) struct Artifact {
    pub(super) contents: Vec<u8>,
    pub(super) head_sha: String,
    pub(super) run_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    head_ref_oid: String,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRun {
    conclusion: Option<String>,
    database_id: u64,
    display_title: String,
    status: String,
    url: String,
}

#[derive(Deserialize)]
struct Provenance {
    repository: String,
    pr: u64,
    sha: String,
    artifact: String,
    run_id: u64,
}

pub(super) async fn download(number: u64, asset_name: &str) -> Result<Artifact> {
    let pull_request = gh_json::<PullRequest>(
        &[
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            REPOSITORY,
            "--json",
            "headRefOid,state",
        ],
        "inspect the pull request",
    )
    .await?;
    if pull_request.state != "OPEN" {
        bail!(
            "pull request #{number} is {}; refusing to install a stale artifact",
            pull_request.state
        );
    }

    let runs = gh_json::<Vec<WorkflowRun>>(
        &[
            "run",
            "list",
            "--repo",
            REPOSITORY,
            "--workflow",
            WORKFLOW,
            "--event",
            "workflow_dispatch",
            "--limit",
            "100",
            "--json",
            "conclusion,databaseId,displayTitle,status,url",
        ],
        "list pull-request artifact runs",
    )
    .await?;
    let title = format!("PR #{number} artifacts");
    let run = runs
        .into_iter()
        .find(|run| run.display_title == title)
        .ok_or_else(|| {
            eyre::eyre!(
                "no on-demand artifact run exists for PR #{number}; dispatch it with \
                 `gh workflow run {WORKFLOW} --repo {REPOSITORY} --field pr={number}`"
            )
        })?;
    if run.status != "completed" || run.conclusion.as_deref() != Some("success") {
        bail!(
            "latest PR #{number} artifact run is {}/{}: {}",
            run.status,
            run.conclusion.as_deref().unwrap_or("pending"),
            run.url
        );
    }

    let directory = tempfile::tempdir().wrap_err("failed to create PR artifact directory")?;
    download_run_artifact(run.database_id, asset_name, &directory).await?;

    let manifest_path = directory.path().join("PR_BUILD.json");
    let checksum_path = directory.path().join(format!("{asset_name}.sha256"));
    let binary_path = directory.path().join(asset_name);
    require_file(&manifest_path)?;
    require_file(&checksum_path)?;
    require_file(&binary_path)?;

    let provenance: Provenance =
        serde_json::from_slice(&fs::read(&manifest_path).wrap_err("failed to read PR_BUILD.json")?)
            .wrap_err("PR_BUILD.json is invalid")?;
    if provenance.repository != REPOSITORY
        || provenance.pr != number
        || provenance.sha != pull_request.head_ref_oid
        || provenance.artifact != asset_name
        || provenance.run_id != run.database_id
    {
        bail!("artifact provenance does not match the current PR head");
    }

    let checksum_manifest =
        fs::read(&checksum_path).wrap_err("failed to read the PR artifact checksum")?;
    let expected = checksum_for(&checksum_manifest, asset_name)?;
    let contents = fs::read(&binary_path).wrap_err("failed to read the PR artifact")?;
    let actual = hex::encode(Sha256::digest(&contents));
    if actual != expected {
        bail!("checksum mismatch for {asset_name}: expected {expected}, downloaded {actual}");
    }

    Ok(Artifact {
        contents,
        head_sha: pull_request.head_ref_oid,
        run_url: run.url,
    })
}

async fn download_run_artifact(run_id: u64, asset_name: &str, directory: &TempDir) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "run",
            "download",
            &run_id.to_string(),
            "--repo",
            REPOSITORY,
            "--name",
            asset_name,
            "--dir",
        ])
        .arg(directory.path())
        .output()
        .await
        .wrap_err("gh is required to download PR artifacts")?;
    ensure_gh_success(output, "download the PR artifact")?;
    Ok(())
}

async fn gh_json<T>(args: &[&str], action: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let output = Command::new("gh")
        .args(args)
        .output()
        .await
        .wrap_err("gh is required to install PR artifacts")?;
    let stdout = ensure_gh_success(output, action)?;
    serde_json::from_slice(&stdout)
        .wrap_err_with(|| format!("gh returned invalid JSON while trying to {action}"))
}

fn ensure_gh_success(output: Output, action: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("failed to {action} with gh: {}", stderr.trim());
    }
    Ok(output.stdout)
}

fn require_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!(
            "PR artifact archive is missing {}",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into()
            )
        );
    }
    Ok(())
}
