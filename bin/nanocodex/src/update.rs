use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Args;
use eyre::{Context, Result, bail, eyre};
use reqwest::{Client, header};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::version;

const REPOSITORY: &str = "gakonst/nanocodex";
const STABLE_RELEASE_API: &str = "https://api.github.com/repos/gakonst/nanocodex/releases/latest";
const NIGHTLY_RELEASE_API: &str =
    "https://api.github.com/repos/gakonst/nanocodex/releases/tags/nightly";
const CHECKSUMS_ASSET: &str = "SHA256SUMS";

#[derive(Debug, Args)]
pub(crate) struct Update {
    /// Install the latest nightly build instead of the latest stable release.
    #[arg(long)]
    nightly: bool,

    /// Reinstall the latest release even when its version is not newer.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

impl Update {
    pub(crate) async fn run(self) -> Result<()> {
        let client = Client::builder()
            .user_agent(format!("nanocodex/{}", version::SEMVER_VERSION))
            .timeout(Duration::from_secs(60))
            .build()
            .wrap_err("failed to create the update client")?;
        let release = client
            .get(release_api(self.nightly))
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .wrap_err_with(|| format!("failed to query the {} Nanocodex release", self.channel()))?
            .error_for_status()
            .wrap_err_with(|| {
                format!(
                    "GitHub did not return a {} Nanocodex release",
                    self.channel()
                )
            })?
            .json::<Release>()
            .await
            .wrap_err("GitHub returned invalid release metadata")?;

        let current = Version::parse(env!("CARGO_PKG_VERSION"))
            .wrap_err("the installed Nanocodex version is invalid")?;
        let latest = if self.nightly {
            None
        } else {
            Some(parse_release_version(&release.tag_name)?)
        };
        if !should_install(
            self.nightly,
            self.force,
            version::IS_NIGHTLY,
            latest.as_ref(),
            &current,
        ) {
            println!("nanocodex {current} is already up to date");
            return Ok(());
        }

        let asset_name = release_asset_name()?;
        let binary = find_asset(&release, asset_name)?;
        let checksums = find_asset(&release, CHECKSUMS_ASSET)?;
        let checksum_manifest = download(&client, checksums).await?;
        let expected = checksum_for(&checksum_manifest, asset_name)?;
        let contents = download(&client, binary).await?;
        let actual = format!("{:x}", Sha256::digest(&contents));
        if actual != expected {
            bail!("checksum mismatch for {asset_name}: expected {expected}, downloaded {actual}");
        }

        let executable = std::env::current_exe()
            .wrap_err("failed to locate the running Nanocodex executable")?;
        let temporary = TemporaryBinary::write_next_to(&executable, &contents)?;
        self_replace::self_replace(temporary.path()).wrap_err_with(|| {
            format!(
                "failed to replace {}; check that it is writable",
                executable.display()
            )
        })?;

        if self.nightly {
            println!("updated nanocodex {current} -> nightly");
        } else if let Some(latest) = latest {
            println!("updated nanocodex {current} -> {latest}");
        }
        Ok(())
    }

    fn channel(&self) -> &'static str {
        if self.nightly { "nightly" } else { "stable" }
    }
}

fn release_api(nightly: bool) -> &'static str {
    if nightly {
        NIGHTLY_RELEASE_API
    } else {
        STABLE_RELEASE_API
    }
}

fn should_install(
    nightly: bool,
    force: bool,
    current_is_nightly: bool,
    latest: Option<&Version>,
    current: &Version,
) -> bool {
    force || nightly || current_is_nightly || latest.is_some_and(|latest| latest > current)
}

async fn download(client: &Client, asset: &ReleaseAsset) -> Result<Vec<u8>> {
    client
        .get(&asset.browser_download_url)
        .send()
        .await
        .wrap_err_with(|| format!("failed to download {}", asset.name))?
        .error_for_status()
        .wrap_err_with(|| format!("GitHub rejected the {} download", asset.name))?
        .bytes()
        .await
        .wrap_err_with(|| format!("failed to read {}", asset.name))
        .map(|bytes| bytes.to_vec())
}

fn parse_release_version(tag: &str) -> Result<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .wrap_err_with(|| format!("latest release tag {tag:?} is not a semantic version"))
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

fn release_asset_name() -> Result<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Ok("nanocodex-x86_64-unknown-linux-gnu");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Ok("nanocodex-aarch64-unknown-linux-gnu");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Ok("nanocodex-aarch64-apple-darwin");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Ok("nanocodex-x86_64-apple-darwin");
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Ok("nanocodex-x86_64-pc-windows-msvc.exe");

    #[allow(unreachable_code)]
    Err(eyre!(
        "self-update is not supported on {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

struct TemporaryBinary {
    path: PathBuf,
}

impl TemporaryBinary {
    fn write_next_to(executable: &Path, contents: &[u8]) -> Result<Self> {
        let parent = executable
            .parent()
            .ok_or_else(|| eyre!("the running executable has no parent directory"))?;
        let file_name = executable
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| eyre!("the running executable name is not valid UTF-8"))?;
        let path = parent.join(format!(".{file_name}.update-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .wrap_err_with(|| format!("failed to create {}", path.display()))?;
        let temporary = Self { path };
        file.write_all(contents)
            .wrap_err_with(|| format!("failed to write {}", temporary.path.display()))?;
        file.sync_all()
            .wrap_err_with(|| format!("failed to sync {}", temporary.path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(executable)
                .wrap_err("failed to read the current executable permissions")?
                .permissions()
                .mode();
            fs::set_permissions(&temporary.path, fs::Permissions::from_mode(mode))
                .wrap_err("failed to make the downloaded executable runnable")?;
        }

        Ok(temporary)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryBinary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(release_api(false), STABLE_RELEASE_API);
        assert_eq!(release_api(true), NIGHTLY_RELEASE_API);
    }

    #[test]
    fn nightly_channel_always_installs_and_stable_respects_versions() {
        let current = Version::new(1, 2, 3);
        assert!(should_install(true, false, false, None, &current));
        assert!(should_install(
            false,
            false,
            true,
            Some(&Version::new(1, 2, 3)),
            &current
        ));
        assert!(!should_install(
            false,
            false,
            false,
            Some(&Version::new(1, 2, 3)),
            &current
        ));
        assert!(should_install(
            false,
            false,
            false,
            Some(&Version::new(1, 2, 4)),
            &current
        ));
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
    }
}
