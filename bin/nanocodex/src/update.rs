use std::{
    borrow::Cow,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Args, ValueHint};
use eyre::{Context, Result, bail, eyre};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, StatusCode, Url, header};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::version;

mod pr;
mod store;

use store::VersionStore;

const REPOSITORY: &str = "gakonst/nanocodex";
const STABLE_RELEASE_API: &str = "https://api.github.com/repos/gakonst/nanocodex/releases/latest";
const NIGHTLY_RELEASE_API: &str =
    "https://api.github.com/repos/gakonst/nanocodex/releases/tags/nightly";
const TAGGED_RELEASE_API: &str = "https://api.github.com/repos/gakonst/nanocodex/releases/tags";
const CHECKSUMS_ASSET: &str = "SHA256SUMS";
const NANOCODEX2_LINUX_ASSET: &str = "nanocodex2-x86_64-unknown-linux-gnu";
const NANOCODEX2_MACOS_ASSET: &str = "nanocodex2-aarch64-apple-darwin";
const VM_GUEST_ASSET: &str = "nanocodex-vm-guest-x86_64-unknown-linux-musl";
const DOWNLOAD_ATTEMPTS: usize = 5;
const DOWNLOAD_RETRY_DELAY: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn prepare_legacy_nightly_bootstrap() -> Result<()> {
    if version::IS_NIGHTLY {
        VersionStore::prepare_legacy_nightly_bootstrap()?;
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum DownloadError {
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error("download exceeds the 256 MiB limit")]
    TooLarge,
}

#[derive(Debug, Args)]
pub(crate) struct Update {
    /// Download or activate an exact release, such as 0.2.0.
    #[arg(
        value_name = "VERSION",
        value_parser = parse_requested_version,
        conflicts_with_all = ["nightly", "pr", "path"]
    )]
    version: Option<Version>,

    /// Download and activate the latest nightly build.
    #[arg(long, conflicts_with_all = ["version", "pr", "path"])]
    nightly: bool,

    /// Download and activate a verified on-demand pull-request artifact.
    #[arg(
        long,
        value_name = "NUMBER",
        value_parser = parse_pr_number,
        conflicts_with_all = ["version", "nightly", "path"]
    )]
    pr: Option<u64>,

    /// Cache and activate a trusted local Nanocodex binary.
    #[arg(
        long,
        value_name = "PATH",
        value_hint = ValueHint::FilePath,
        conflicts_with_all = ["version", "nightly", "pr"]
    )]
    path: Option<PathBuf>,

    /// Reinstall the selected release even when it is already installed.
    #[arg(long, conflicts_with_all = ["pr", "path"])]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    target_commitish: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReleaseAsset {
    id: u64,
    name: String,
    browser_download_url: String,
}

impl ReleaseAsset {
    fn download_url(&self) -> Result<Url> {
        let mut url = Url::parse(&self.browser_download_url)
            .wrap_err_with(|| format!("GitHub returned an invalid URL for {}", self.name))?;
        url.query_pairs_mut()
            .append_pair("asset_id", &self.id.to_string());
        Ok(url)
    }
}

impl Update {
    pub(crate) async fn run(self) -> Result<()> {
        let manager_version = Version::parse(env!("CARGO_PKG_VERSION"))
            .wrap_err("the installed Nanocodex version is invalid")?;
        let store = VersionStore::discover()?;
        let manager_key = manager_key(&manager_version);
        store.prepare(&manager_key)?;
        VersionStore::promote_running_legacy_nightly_manager()?;
        let previous = store.active()?.unwrap_or_else(|| manager_key.clone());

        if let Some(path) = &self.path {
            return install_local_binary(path, &store, &previous);
        }
        if let Some(pr) = self.pr {
            return install_pr_binary(pr, &store, &previous).await;
        }

        if let Some(requested) = &self.version {
            let key = requested.to_string();
            if !self.force && store.is_cached_bundle(&key, false)? {
                store.activate(&key)?;
                maybe_promote_manager(&store, &key, requested, &manager_version)?;
                report_activation(&previous, &key, false);
                return Ok(());
            }
        }

        let client = Client::builder()
            .user_agent(format!("nanocodex/{}", version::SEMVER_VERSION))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .wrap_err("failed to create the update client")?;
        let release_description = self.release_description();
        let release_api = release_api(self.nightly, self.version.as_ref());
        let mut release =
            fetch_release(&client, release_api.as_ref(), &release_description).await?;
        if self.nightly {
            release = fetch_immutable_nightly(&client, &release).await?;
        }

        let latest = if self.nightly {
            None
        } else {
            Some(parse_release_version(&release.tag_name)?)
        };
        if let (Some(requested), Some(released)) = (&self.version, &latest)
            && requested != released
        {
            bail!(
                "GitHub returned release {} for requested version {requested}",
                release.tag_name
            );
        }

        let key = latest
            .as_ref()
            .map_or_else(|| nightly_key(&release), |version| Ok(version.to_string()))?;
        let cached = if self.nightly {
            store.is_cached_bundle(&key, vm_guest_binary_asset_name().is_some())?
        } else {
            store.is_cached_bundle(&key, false)?
        };
        if !self.force && cached {
            store.activate(&key)?;
            if self.nightly {
                store.promote_manager(&key)?;
            } else if let Some(latest) = &latest {
                maybe_promote_manager(&store, &key, latest, &manager_version)?;
            }
            report_activation(&previous, &key, false);
            return Ok(());
        }

        let binary_name = binary_asset_name()?;
        let (binary, compressed) = find_preferred_asset(&release, binary_name)?;
        let (companion, companion_compressed) =
            find_preferred_asset(&release, nanocodex2_binary_asset_name()?)?;
        let checksums = find_asset(&release, CHECKSUMS_ASSET)?;
        let checksum_manifest = download(&client, checksums, false).await?;
        let archive = download_verified(&client, binary, &checksum_manifest, true).await?;
        let contents = unpack_release_asset(archive, &binary.name, compressed)?;
        let companion_archive =
            download_verified(&client, companion, &checksum_manifest, true).await?;
        let companion_contents =
            unpack_release_asset(companion_archive, &companion.name, companion_compressed)?;
        let guest_contents = if self.nightly {
            if let Some(guest_name) = vm_guest_binary_asset_name() {
                let (guest, compressed) = find_preferred_asset(&release, guest_name)?;
                let guest_archive =
                    download_verified(&client, guest, &checksum_manifest, true).await?;
                Some(unpack_release_asset(
                    guest_archive,
                    &guest.name,
                    compressed,
                )?)
            } else {
                None
            }
        } else {
            None
        };
        store.install_bundle(
            &key,
            &contents,
            &companion_contents,
            guest_contents.as_deref(),
        )?;
        store.activate(&key)?;
        if self.nightly {
            store.promote_manager(&key)?;
        } else if let Some(latest) = &latest {
            maybe_promote_manager(&store, &key, latest, &manager_version)?;
        }
        report_activation(&previous, &key, true);
        Ok(())
    }

    fn release_description(&self) -> Cow<'static, str> {
        if self.nightly {
            Cow::Borrowed("nightly Nanocodex release")
        } else if let Some(version) = &self.version {
            Cow::Owned(format!("Nanocodex {version} release"))
        } else {
            Cow::Borrowed("latest stable Nanocodex release")
        }
    }
}

async fn fetch_release(client: &Client, url: &str, description: &str) -> Result<Release> {
    client
        .get(url)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .wrap_err_with(|| format!("failed to query the {description}"))?
        .error_for_status()
        .wrap_err_with(|| format!("GitHub did not return the {description}"))?
        .json::<Release>()
        .await
        .wrap_err_with(|| format!("GitHub returned invalid {description} metadata"))
}

async fn fetch_immutable_nightly(client: &Client, pointer: &Release) -> Result<Release> {
    let tag = immutable_nightly_tag(pointer)?;
    let url = format!("{TAGGED_RELEASE_API}/{tag}");
    let release = fetch_release(client, &url, &format!("immutable {tag} release")).await?;
    validate_immutable_nightly(&release, &tag)?;
    Ok(release)
}

fn release_api(nightly: bool, version: Option<&Version>) -> Cow<'static, str> {
    if nightly {
        Cow::Borrowed(NIGHTLY_RELEASE_API)
    } else if let Some(version) = version {
        Cow::Owned(format!("{TAGGED_RELEASE_API}/v{version}"))
    } else {
        Cow::Borrowed(STABLE_RELEASE_API)
    }
}

fn parse_requested_version(value: &str) -> std::result::Result<Version, String> {
    Version::parse(value.strip_prefix('v').unwrap_or(value))
        .map_err(|_| format!("{value:?} is not a semantic version such as 0.2.0"))
}

fn parse_pr_number(value: &str) -> std::result::Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| "pull-request number must be a positive integer".to_owned())
}

fn manager_key(version_number: &Version) -> String {
    if version::IS_NIGHTLY {
        "nightly".to_owned()
    } else if version::SEMVER_VERSION.contains("-dev+") {
        format!("dev-{}", version::SEMVER_VERSION)
    } else {
        version_number.to_string()
    }
}

fn install_local_binary(path: &Path, store: &VersionStore, previous: &str) -> Result<()> {
    let contents = fs::read(path).wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let digest = hex::encode(Sha256::digest(&contents));
    let key = format!("local-{}", &digest[..12]);
    store.install(&key, &contents)?;
    store.activate(&key)?;
    println!(
        "installed and activated nanocodex {key} from {} (previously {previous})",
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
    );
    Ok(())
}

async fn install_pr_binary(number: u64, store: &VersionStore, previous: &str) -> Result<()> {
    let asset_name = binary_asset_name()?;
    let artifact = pr::download(number, asset_name).await?;
    let key = format!("pr-{number}-{}", artifact.head_sha);
    store.install(&key, &artifact.contents)?;
    store.activate(&key)?;
    println!(
        "installed and activated nanocodex PR #{number} at {} ({}, previously {previous})",
        artifact.head_sha, artifact.run_url,
    );
    Ok(())
}

fn maybe_promote_manager(
    store: &VersionStore,
    key: &str,
    selected: &Version,
    manager: &Version,
) -> Result<()> {
    if selected > manager {
        store.promote_manager(key)?;
    }
    Ok(())
}

fn report_activation(previous: &str, selected: &str, downloaded: bool) {
    if previous == selected {
        if downloaded {
            println!("reinstalled nanocodex {selected}");
        } else {
            println!("nanocodex {selected} is already active");
        }
    } else if downloaded {
        println!("installed and activated nanocodex {selected} (previously {previous})");
    } else {
        println!("switched nanocodex {previous} -> {selected}");
    }
}

async fn download(client: &Client, asset: &ReleaseAsset, show_progress: bool) -> Result<Vec<u8>> {
    let url = asset.download_url()?;
    if show_progress {
        eprintln!("downloading {}...", asset.name);
    }
    for attempt in 0..DOWNLOAD_ATTEMPTS {
        let result = download_once(client, url.clone(), show_progress).await;

        match result {
            Ok(contents) => return Ok(contents),
            Err(error) if attempt + 1 < DOWNLOAD_ATTEMPTS && retryable_download_error(&error) => {
                let delay = DOWNLOAD_RETRY_DELAY.saturating_mul(1 << attempt);
                if show_progress {
                    eprintln!(
                        "download interrupted ({error}); retrying {}/{} in {:.2}s...",
                        attempt + 2,
                        DOWNLOAD_ATTEMPTS,
                        delay.as_secs_f64()
                    );
                }
                tokio::time::sleep(delay).await;
            }
            Err(error) => {
                return Err(error).wrap_err_with(|| {
                    format!(
                        "failed to download {} after {} attempt{}",
                        asset.name,
                        attempt + 1,
                        if attempt == 0 { "" } else { "s" }
                    )
                });
            }
        }
    }

    unreachable!("the download attempt loop always returns")
}

async fn download_once(
    client: &Client,
    url: Url,
    show_progress: bool,
) -> std::result::Result<Vec<u8>, DownloadError> {
    let response = client
        .get(url)
        .header(header::ACCEPT, "application/octet-stream")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(DownloadError::TooLarge);
    }

    let progress = if show_progress {
        download_progress(response.content_length())
    } else {
        ProgressBar::hidden()
    };
    let mut contents = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .map(|length| length.min(MAX_ARCHIVE_BYTES as usize))
            .unwrap_or_default(),
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                if contents.len().saturating_add(chunk.len()) > MAX_ARCHIVE_BYTES as usize {
                    progress.finish_and_clear();
                    return Err(DownloadError::TooLarge);
                }
                progress.inc(chunk.len() as u64);
                contents.extend_from_slice(&chunk);
            }
            Err(error) => {
                progress.finish_and_clear();
                return Err(error.into());
            }
        }
    }
    progress.finish_and_clear();
    Ok(contents)
}

fn download_progress(total_size: Option<u64>) -> ProgressBar {
    let progress = total_size.map_or_else(ProgressBar::new_spinner, ProgressBar::new);
    let template = if total_size.is_some() {
        "{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})"
    } else {
        "{spinner:.green} {bytes} downloaded ({bytes_per_sec})"
    };
    if let Ok(style) = ProgressStyle::with_template(template) {
        progress.set_style(style.progress_chars("#>-"));
    }
    progress
}

async fn download_verified(
    client: &Client,
    asset: &ReleaseAsset,
    checksum_manifest: &[u8],
    show_progress: bool,
) -> Result<Vec<u8>> {
    let expected = checksum_for(checksum_manifest, &asset.name)?;
    let contents = download(client, asset, show_progress).await?;
    let actual = hex::encode(Sha256::digest(&contents));
    if actual != expected {
        bail!(
            "checksum mismatch for {}: expected {expected}, downloaded {actual}",
            asset.name
        );
    }
    Ok(contents)
}

fn retryable_download_error(error: &DownloadError) -> bool {
    match error {
        DownloadError::Request(error) => error.status().is_none_or(retryable_download_status),
        DownloadError::TooLarge => false,
    }
}

fn retryable_download_status(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
}

fn parse_release_version(tag: &str) -> Result<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .wrap_err_with(|| format!("release tag {tag:?} is not a semantic version"))
}

fn nightly_key(release: &Release) -> Result<String> {
    nightly_key_for(release, std::env::consts::OS, std::env::consts::ARCH)
}

fn nightly_key_for(release: &Release, os: &str, arch: &str) -> Result<String> {
    let sha = exact_release_commit(release)?;
    let (binary, _) = find_preferred_asset(release, binary_asset_name_for(os, arch)?)?;
    let (companion, _) =
        find_preferred_asset(release, nanocodex2_binary_asset_name_for(os, arch)?)?;
    let mut key = format!(
        "nightly-{}-{}-{}",
        sha.to_ascii_lowercase(),
        binary.id,
        companion.id
    );
    if let Some(guest_name) = vm_guest_binary_asset_name_for(os, arch) {
        let (guest, _) = find_preferred_asset(release, guest_name)?;
        key.push_str(&format!("-{}", guest.id));
    }
    Ok(key)
}

fn immutable_nightly_tag(pointer: &Release) -> Result<String> {
    if pointer.tag_name != "nightly" {
        bail!(
            "GitHub returned release {} for the nightly release pointer",
            pointer.tag_name
        );
    }
    Ok(format!(
        "nightly-{}",
        exact_release_commit(pointer)?.to_ascii_lowercase()
    ))
}

fn validate_immutable_nightly(release: &Release, expected_tag: &str) -> Result<()> {
    if release.tag_name != expected_tag {
        bail!(
            "GitHub returned release {} for immutable nightly {expected_tag}",
            release.tag_name
        );
    }
    let expected_sha = expected_tag
        .strip_prefix("nightly-")
        .ok_or_else(|| eyre!("invalid immutable nightly tag {expected_tag:?}"))?;
    let actual_sha = exact_release_commit(release)?;
    if !actual_sha.eq_ignore_ascii_case(expected_sha) {
        bail!("immutable nightly {expected_tag} targets {actual_sha}, expected {expected_sha}");
    }
    Ok(())
}

fn exact_release_commit(release: &Release) -> Result<&str> {
    let sha = release.target_commitish.as_str();
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "release {} target {sha:?} is not an exact Git commit",
            release.tag_name
        );
    }
    Ok(sha)
}

fn find_asset<'a>(release: &'a Release, name: &str) -> Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| {
            eyre!(
                "release {} does not contain {name}; see https://github.com/{REPOSITORY}/releases/tag/{}",
                release.tag_name,
                release.tag_name
            )
        })
}

fn find_preferred_asset<'a>(
    release: &'a Release,
    binary_name: &str,
) -> Result<(&'a ReleaseAsset, bool)> {
    let compressed_name = format!("{binary_name}.gz");
    if let Some(asset) = release
        .assets
        .iter()
        .find(|asset| asset.name == compressed_name)
    {
        return Ok((asset, true));
    }
    find_asset(release, binary_name).map(|asset| (asset, false))
}

fn checksum_for(manifest: &[u8], asset_name: &str) -> Result<String> {
    let manifest = std::str::from_utf8(manifest).wrap_err("SHA256SUMS is not UTF-8")?;
    for line in manifest.lines() {
        let mut fields = line.split_whitespace();
        let Some(checksum) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        if name.trim_start_matches('*') == asset_name {
            if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("SHA256SUMS contains an invalid checksum for {asset_name}");
            }
            return Ok(checksum.to_ascii_lowercase());
        }
    }
    bail!("SHA256SUMS does not contain {asset_name}")
}

#[cfg(test)]
fn release_asset_name_for(os: &str, arch: &str) -> Result<String> {
    Ok(format!("{}.gz", binary_asset_name_for(os, arch)?))
}

fn binary_asset_name() -> Result<&'static str> {
    binary_asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn nanocodex2_binary_asset_name() -> Result<&'static str> {
    nanocodex2_binary_asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn nanocodex2_binary_asset_name_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok(NANOCODEX2_LINUX_ASSET),
        ("macos", "aarch64") => Ok(NANOCODEX2_MACOS_ASSET),
        _ => Err(eyre!("self-update is not supported on {os} {arch}")),
    }
}

fn vm_guest_binary_asset_name() -> Option<&'static str> {
    vm_guest_binary_asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn vm_guest_binary_asset_name_for(os: &str, arch: &str) -> Option<&'static str> {
    matches!((os, arch), ("linux", "x86_64")).then_some(VM_GUEST_ASSET)
}

fn binary_asset_name_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("nanocodex-x86_64-unknown-linux-gnu"),
        ("macos", "aarch64") => Ok("nanocodex-aarch64-apple-darwin"),
        _ => Err(eyre!("self-update is not supported on {os} {arch}")),
    }
}

fn decompress_release_asset(archive: &[u8], asset_name: &str) -> Result<Vec<u8>> {
    let mut contents = Vec::new();
    GzDecoder::new(archive)
        .take(MAX_BINARY_BYTES + 1)
        .read_to_end(&mut contents)
        .wrap_err_with(|| format!("failed to decompress {asset_name}"))?;
    if contents.len() as u64 > MAX_BINARY_BYTES {
        bail!("decompressed {asset_name} exceeds the 256 MiB limit");
    }
    Ok(contents)
}

fn unpack_release_asset(archive: Vec<u8>, asset_name: &str, compressed: bool) -> Result<Vec<u8>> {
    if compressed {
        decompress_release_asset(&archive, asset_name)
    } else if archive.len() as u64 > MAX_BINARY_BYTES {
        bail!("{asset_name} exceeds the 256 MiB limit");
    } else {
        Ok(archive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Parser, Subcommand};
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TestCommand,
    }

    #[derive(Subcommand)]
    enum TestCommand {
        Update(Update),
    }

    #[test]
    fn accepts_prefixed_and_plain_release_versions() {
        assert_eq!(
            parse_release_version("v1.2.3").unwrap(),
            Version::new(1, 2, 3)
        );
        assert_eq!(
            parse_release_version("1.2.3").unwrap(),
            Version::new(1, 2, 3)
        );
        assert!(parse_release_version("latest").is_err());
    }

    #[test]
    fn selects_stable_and_nightly_release_channels() {
        assert_eq!(release_api(false, None), STABLE_RELEASE_API);
        assert_eq!(release_api(true, None), NIGHTLY_RELEASE_API);
        assert_eq!(
            release_api(false, Some(&Version::new(0, 2, 0))),
            format!("{TAGGED_RELEASE_API}/v0.2.0")
        );
    }

    #[test]
    fn publishes_both_binaries_only_for_linux_x86_64_and_apple_silicon() {
        assert_eq!(
            release_asset_name_for("linux", "x86_64").unwrap(),
            "nanocodex-x86_64-unknown-linux-gnu.gz"
        );
        assert_eq!(
            release_asset_name_for("macos", "aarch64").unwrap(),
            "nanocodex-aarch64-apple-darwin.gz"
        );
        assert_eq!(
            nanocodex2_binary_asset_name_for("linux", "x86_64").unwrap(),
            "nanocodex2-x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            nanocodex2_binary_asset_name_for("macos", "aarch64").unwrap(),
            "nanocodex2-aarch64-apple-darwin"
        );
        assert!(release_asset_name_for("linux", "aarch64").is_err());
        assert!(release_asset_name_for("macos", "x86_64").is_err());
        assert!(release_asset_name_for("windows", "x86_64").is_err());
        assert!(nanocodex2_binary_asset_name_for("linux", "aarch64").is_err());
        assert!(nanocodex2_binary_asset_name_for("macos", "x86_64").is_err());
        assert!(nanocodex2_binary_asset_name_for("windows", "x86_64").is_err());
    }

    #[test]
    fn parses_exact_pr_and_local_update_sources() {
        let TestCommand::Update(exact) = TestCli::try_parse_from(["nanocodex", "update", "v0.2.0"])
            .unwrap()
            .command;
        assert_eq!(exact.version, Some(Version::new(0, 2, 0)));

        let TestCommand::Update(pr) =
            TestCli::try_parse_from(["nanocodex", "update", "--pr", "50"])
                .unwrap()
                .command;
        assert_eq!(pr.pr, Some(50));

        let TestCommand::Update(path) =
            TestCli::try_parse_from(["nanocodex", "update", "--path", "/tmp/nanocodex"])
                .unwrap()
                .command;
        assert_eq!(path.path, Some(PathBuf::from("/tmp/nanocodex")));
    }

    #[test]
    fn rejects_conflicting_and_invalid_update_sources() {
        assert!(TestCli::try_parse_from(["nanocodex", "update", "0.2.0", "--nightly"]).is_err());
        assert!(TestCli::try_parse_from(["nanocodex", "update", "--pr", "0"]).is_err());
        assert!(TestCli::try_parse_from(["nanocodex", "update", "not-a-version"]).is_err());
    }

    #[test]
    fn selects_the_named_checksum() {
        let manifest = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other\n\
            ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789 *nanocodex-test\n";
        assert_eq!(
            checksum_for(manifest, "nanocodex-test").unwrap(),
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
    }

    #[test]
    fn rejects_missing_and_malformed_checksums() {
        assert!(checksum_for(b"abcd  nanocodex-test\n", "nanocodex-test").is_err());
        assert!(checksum_for(b"", "nanocodex-test").is_err());
        let manifest =
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  nanocodex-test\n";
        assert!(checksum_for(manifest, "nanocodex2-test").is_err());
    }

    #[test]
    fn decompresses_release_assets() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(b"nanocodex binary").unwrap();
        let archive = encoder.finish().unwrap();

        assert_eq!(
            decompress_release_asset(&archive, "nanocodex-test.gz").unwrap(),
            b"nanocodex binary"
        );
        assert!(decompress_release_asset(b"not gzip", "nanocodex-test.gz").is_err());
    }

    #[test]
    fn prefers_compressed_assets_and_falls_back_to_older_raw_releases() {
        let release = Release {
            tag_name: "v0.5.0".to_owned(),
            target_commitish: "master".to_owned(),
            assets: vec![
                ReleaseAsset {
                    id: 1,
                    name: "nanocodex-test".to_owned(),
                    browser_download_url: "https://example.invalid/raw".to_owned(),
                },
                ReleaseAsset {
                    id: 2,
                    name: "nanocodex-test.gz".to_owned(),
                    browser_download_url: "https://example.invalid/gzip".to_owned(),
                },
            ],
        };

        let (preferred, compressed) = find_preferred_asset(&release, "nanocodex-test").unwrap();
        assert_eq!(preferred.id, 2);
        assert!(compressed);

        let raw_release = Release {
            assets: release.assets[..1].to_vec(),
            ..release
        };
        let (raw, compressed) = find_preferred_asset(&raw_release, "nanocodex-test").unwrap();
        assert_eq!(raw.id, 1);
        assert!(!compressed);
    }

    #[test]
    fn requires_independently_selectable_assets_for_both_binaries() {
        let release = Release {
            tag_name: "v0.5.0".to_owned(),
            target_commitish: "master".to_owned(),
            assets: vec![ReleaseAsset {
                id: 1,
                name: "nanocodex-test.gz".to_owned(),
                browser_download_url: "https://example.invalid/nanocodex".to_owned(),
            }],
        };

        assert!(find_preferred_asset(&release, "nanocodex-test").is_ok());
        assert!(find_preferred_asset(&release, "nanocodex2-test").is_err());
    }

    #[test]
    fn cache_busts_mutable_release_assets_with_their_identity() {
        let asset = ReleaseAsset {
            id: 496_045_871,
            name: CHECKSUMS_ASSET.to_owned(),
            browser_download_url:
                "https://github.com/gakonst/nanocodex/releases/download/nightly/SHA256SUMS"
                    .to_owned(),
        };

        assert_eq!(
            asset.download_url().unwrap().as_str(),
            "https://github.com/gakonst/nanocodex/releases/download/nightly/SHA256SUMS?asset_id=496045871"
        );
    }

    #[test]
    fn retries_only_transient_download_statuses() {
        assert!(retryable_download_status(StatusCode::REQUEST_TIMEOUT));
        assert!(retryable_download_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_download_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!retryable_download_status(StatusCode::NOT_FOUND));
    }

    #[test]
    fn nightly_versions_are_bound_to_the_commit_and_every_asset() {
        let release = Release {
            tag_name: "nightly-0123456789abcdef0123456789abcdef01234567".to_owned(),
            target_commitish: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            assets: vec![
                ReleaseAsset {
                    id: 11,
                    name: "nanocodex-x86_64-unknown-linux-gnu.gz".to_owned(),
                    browser_download_url: "https://example.invalid/nanocodex".to_owned(),
                },
                ReleaseAsset {
                    id: 12,
                    name: format!("{NANOCODEX2_LINUX_ASSET}.gz"),
                    browser_download_url: "https://example.invalid/nanocodex2".to_owned(),
                },
                ReleaseAsset {
                    id: 13,
                    name: format!("{VM_GUEST_ASSET}.gz"),
                    browser_download_url: "https://example.invalid/nanocodex-vm-guest".to_owned(),
                },
            ],
        };

        assert_eq!(
            nightly_key_for(&release, "linux", "x86_64").unwrap(),
            "nightly-0123456789abcdef0123456789abcdef01234567-11-12-13"
        );
        assert_eq!(
            nanocodex2_binary_asset_name_for("macos", "aarch64").unwrap(),
            NANOCODEX2_MACOS_ASSET
        );
        assert_eq!(
            vm_guest_binary_asset_name_for("linux", "x86_64"),
            Some(VM_GUEST_ASSET)
        );
        assert_eq!(vm_guest_binary_asset_name_for("macos", "aarch64"), None);
    }

    #[test]
    fn resolves_and_validates_the_immutable_nightly_release() {
        let pointer = Release {
            tag_name: "nightly".to_owned(),
            target_commitish: "ABCDEF0123456789ABCDEF0123456789ABCDEF01".to_owned(),
            assets: Vec::new(),
        };
        let tag = immutable_nightly_tag(&pointer).unwrap();
        assert_eq!(tag, "nightly-abcdef0123456789abcdef0123456789abcdef01");

        let release = Release {
            tag_name: tag.clone(),
            target_commitish: "abcdef0123456789abcdef0123456789abcdef01".to_owned(),
            assets: Vec::new(),
        };
        validate_immutable_nightly(&release, &tag).unwrap();
    }

    #[test]
    fn rejects_misdirected_nightly_release_metadata() {
        let branch_target = Release {
            tag_name: "nightly".to_owned(),
            target_commitish: "master".to_owned(),
            assets: Vec::new(),
        };
        assert!(immutable_nightly_tag(&branch_target).is_err());

        let wrong_pointer = Release {
            tag_name: "nightly-other".to_owned(),
            target_commitish: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            assets: Vec::new(),
        };
        assert!(immutable_nightly_tag(&wrong_pointer).is_err());

        let wrong_target = Release {
            tag_name: "nightly-0123456789abcdef0123456789abcdef01234567".to_owned(),
            target_commitish: "fedcba9876543210fedcba9876543210fedcba98".to_owned(),
            assets: Vec::new(),
        };
        assert!(
            validate_immutable_nightly(
                &wrong_target,
                "nightly-0123456789abcdef0123456789abcdef01234567"
            )
            .is_err()
        );
    }
}
