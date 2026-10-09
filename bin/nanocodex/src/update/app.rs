//! The macOS Hand as a signed `Nanocodex.app` bundle.
//!
//! Releases ship `nanocodex-app-<triple>.tar.gz` beside the standalone Hand. The
//! updater stores one bundle per Hand identity at
//! `hand-versions/<identity>/Nanocodex.app` and links every CLI version with
//! that identity to it, so an unchanged Hand keeps one path and one code
//! signature (and therefore its macOS privacy grants) across CLI updates.
//! Development pairs are wrapped into the same layout and signed locally with
//! the same bundle identifier.
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use eyre::{Context, Result, bail, eyre};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

pub(super) const BUNDLE: &str = "Nanocodex.app";
/// The Hand inside the bundle, relative to the bundle's parent.
pub(super) const EXECUTABLE: &str = "Nanocodex.app/Contents/MacOS/nanocodex2";
/// Per-file digests of a stored bundle, written beside it after it is in place.
pub(super) const RECEIPT: &str = "Nanocodex.app.sha256";
/// The code-signing identifier (and CFBundleIdentifier) of every Hand build.
#[cfg(unix)]
pub(super) const IDENTIFIER: &str = "com.nanocodex.hand";
/// The code-signing identity (certificate name or SHA-1) for development
/// bundles. Without it a single installed "Developer ID Application" identity
/// is used, and only without any such identity is the bundle signed ad hoc.
#[cfg(unix)]
pub(super) const SIGNING_IDENTITY_ENV: &str = "NANOCODEX_CODESIGN_IDENTITY";
/// Accepted alias of [`SIGNING_IDENTITY_ENV`].
#[cfg(unix)]
const SIGNING_IDENTITY_ALIAS_ENV: &str = "NANOCODEX_MACOS_SIGNING_IDENTITY";
#[cfg(unix)]
const ENTITLEMENTS: &str = include_str!("../../../../nanocodex-vm.entitlements");
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 64;

/// The release asset carrying the bundle for a target, if one is published.
pub(super) fn asset_name_for(os: &str, arch: &str) -> Option<&'static str> {
    (os == "macos" && arch == "aarch64").then_some("nanocodex-app-aarch64-apple-darwin.tar.gz")
}

pub(super) fn asset_name() -> Option<&'static str> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn valid_path(name: &str) -> bool {
    let name = name.strip_suffix('/').unwrap_or(name);
    (name == BUNDLE || name.starts_with("Nanocodex.app/"))
        && name.len() <= 512
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_+.- ".contains(&c))
        // No traversal, and no AppleDouble or Finder metadata that would break
        // the sealed bundle.
        && name
            .split('/')
            .all(|part| !matches!(part, "" | "." | "..") && !part.starts_with("._"))
        && !name.ends_with(".DS_Store")
}

fn has_required(files: &BTreeSet<String>) -> bool {
    [
        "Nanocodex.app/Contents/Info.plist",
        EXECUTABLE,
        "Nanocodex.app/Contents/_CodeSignature/CodeResources",
    ]
    .iter()
    .all(|required| files.contains(*required))
}

/// Extract a release archive into `parent/Nanocodex.app` and return the
/// receipt describing it. `parent` must be a fresh private directory. Only
/// directories and regular files are accepted; links, devices, absolute or
/// escaping names, case-folded duplicates and oversized archives fail closed.
pub(super) fn extract(archive: &[u8], parent: &Path) -> Result<String> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive).take(MAX_BYTES + 1));
    let mut files = BTreeSet::new();
    let mut folded = BTreeSet::new();
    let mut total = 0_u64;
    let mut receipt = String::new();
    let mut entries = 0_usize;
    for entry in archive
        .entries()
        .wrap_err("invalid Nanocodex.app archive")?
    {
        let mut entry = entry.wrap_err("invalid Nanocodex.app archive entry")?;
        let kind = entry.header().entry_type();
        if matches!(
            kind,
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader
        ) {
            continue;
        }
        entries += 1;
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .wrap_err("Nanocodex.app archive entry is not UTF-8")?;
        let trimmed = name.strip_suffix('/').unwrap_or(&name).to_owned();
        if entries > MAX_ENTRIES
            || !valid_path(&name)
            || !folded.insert(trimmed.to_ascii_lowercase())
        {
            bail!("invalid Nanocodex.app archive entry {name:?}");
        }
        let path = parent.join(&trimmed);
        if kind.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if !kind.is_file() {
            bail!("Nanocodex.app archive entry {name:?} is not a regular file");
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| eyre!("Nanocodex.app archive is too large"))?;
        if total > MAX_BYTES {
            bail!("Nanocodex.app archive exceeds the size limit");
        }
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents)?;
        if contents.len() as u64 != entry.size() {
            bail!("truncated Nanocodex.app archive entry {name:?}");
        }
        let executable = entry.header().mode().is_ok_and(|mode| mode & 0o111 != 0);
        if trimmed == EXECUTABLE && !executable {
            bail!("the Nanocodex.app Hand is not executable");
        }
        fs::create_dir_all(path.parent().unwrap())?;
        super::store::atomic_write(&path, &contents, executable)?;
        receipt.push_str(&format!(
            "{}  {trimmed}\n",
            hex::encode(Sha256::digest(&contents))
        ));
        files.insert(trimmed);
    }
    if !has_required(&files) {
        bail!("Nanocodex.app archive is incomplete");
    }
    Ok(receipt)
}

/// Every regular file under `parent/Nanocodex.app`, refusing links.
fn tree(parent: &Path) -> Result<Option<BTreeSet<String>>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![PathBuf::from(BUNDLE)];
    while let Some(relative) = pending.pop() {
        let path = parent.join(&relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Some(name) = relative.to_str() else {
            return Ok(None);
        };
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(relative.join(entry?.file_name()));
            }
        } else if metadata.is_file() && files.len() < MAX_ENTRIES {
            files.insert(name.to_owned());
        } else {
            return Ok(None);
        }
    }
    Ok(Some(files))
}

/// Whether `parent` holds a complete bundle exactly matching its receipt.
pub(super) fn cached(parent: &Path) -> Result<bool> {
    let receipt_path = parent.join(RECEIPT);
    if !fs::symlink_metadata(&receipt_path).is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024) {
        return Ok(false);
    }
    let receipt = fs::read_to_string(&receipt_path)?;
    let Some(files) = tree(parent)? else {
        return Ok(false);
    };
    let mut listed = BTreeSet::new();
    for line in receipt.lines() {
        let Some((digest, name)) = line.split_once("  ") else {
            return Ok(false);
        };
        if !valid_path(name) || !listed.insert(name.to_owned()) {
            return Ok(false);
        }
        let Ok(bytes) = fs::read(parent.join(name)) else {
            return Ok(false);
        };
        if hex::encode(Sha256::digest(bytes)) != digest {
            return Ok(false);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(parent.join(EXECUTABLE))?.permissions().mode() & 0o111 == 0 {
            return Ok(false);
        }
    }
    Ok(listed == files && has_required(&files))
}

/// Receipt of an already-complete bundle (used after local signing).
#[cfg(unix)]
pub(super) fn receipt_of(parent: &Path) -> Result<String> {
    let files = tree(parent)?.ok_or_else(|| eyre!("Nanocodex.app contains a link"))?;
    if !has_required(&files) {
        bail!("Nanocodex.app is incomplete");
    }
    let mut receipt = String::new();
    for name in files {
        let bytes = fs::read(parent.join(&name))?;
        receipt.push_str(&format!("{}  {name}\n", hex::encode(Sha256::digest(bytes))));
    }
    Ok(receipt)
}

#[cfg(unix)]
fn info_plist(version: &str) -> String {
    // CFBundleVersion accepts only numeric dotted versions.
    let numeric = version
        .split(['-', '+'])
        .next()
        .filter(|v| v.split('.').count() == 3 && v.split('.').all(|p| p.parse::<u32>().is_ok()))
        .unwrap_or("0.0.0");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>{IDENTIFIER}</string>
  <key>CFBundleName</key><string>Nanocodex</string>
  <key>CFBundleDisplayName</key><string>Nanocodex</string>
  <key>CFBundleExecutable</key><string>nanocodex2</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>{numeric}</string>
  <key>CFBundleVersion</key><string>{numeric}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
</dict>
</plist>
"#
    )
}

/// Wrap a development Hand into `parent/Nanocodex.app` exactly like the
/// release bundle and sign it with the release identifier and entitlements,
/// using the identity chosen by [`signing_identity`].
#[cfg(unix)]
pub(super) fn wrap(hand: &[u8], version: &str, parent: &Path) -> Result<String> {
    let contents = parent.join("Nanocodex.app/Contents");
    super::store::atomic_write(&contents.join("MacOS/nanocodex2"), hand, true)?;
    super::store::atomic_write(
        &contents.join("Info.plist"),
        info_plist(version).as_bytes(),
        false,
    )?;
    sign(&parent.join(BUNDLE))?;
    verify_signature(&parent.join(BUNDLE))?;
    receipt_of(parent)
}

fn codesign(arguments: &[&std::ffi::OsStr]) -> Result<std::process::Output> {
    std::process::Command::new("/usr/bin/codesign")
        .args(arguments)
        .output()
        .wrap_err("failed to run /usr/bin/codesign")
}

#[cfg(unix)]
fn sign(bundle: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("signing Nanocodex.app requires macOS");
    }
    let identity = signing_identity()?;
    if identity.is_none() {
        eprintln!(
            "warning: no Developer ID Application identity found; signing the development Nanocodex.app ad hoc. Set {SIGNING_IDENTITY_ENV} to a code-signing identity so macOS privacy grants survive Hand changes"
        );
    }
    let entitlements = tempfile::NamedTempFile::new()?;
    fs::write(entitlements.path(), ENTITLEMENTS)?;
    let identity = identity.unwrap_or_else(|| "-".to_owned());
    // Development signatures stay offline: no secure timestamp request.
    let output = codesign(&[
        "--force".as_ref(),
        "--timestamp=none".as_ref(),
        "--sign".as_ref(),
        identity.as_ref(),
        "--identifier".as_ref(),
        IDENTIFIER.as_ref(),
        "--entitlements".as_ref(),
        entitlements.path().as_os_str(),
        bundle.as_os_str(),
    ])?;
    if !output.status.success() {
        bail!(
            "codesign failed for {}: {}",
            bundle.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// The explicit identity from [`SIGNING_IDENTITY_ENV`] (or its alias), which
/// must be a valid installed code-signing identity; otherwise the one valid
/// "Developer ID Application" identity, if exactly one is installed. Several
/// candidates are ambiguous and fail rather than pick one silently.
#[cfg(unix)]
fn signing_identity() -> Result<Option<String>> {
    let explicit = [SIGNING_IDENTITY_ENV, SIGNING_IDENTITY_ALIAS_ENV]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name, value.trim().to_owned()))
                .filter(|(_, value)| !value.is_empty())
        });
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .wrap_err("failed to list code-signing identities with /usr/bin/security")?;
    // Lines look like:  1) <SHA-1> "Developer ID Application: Name (TEAMID)"
    let identities: Vec<(String, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.trim().split_once(") ")?;
            let (hash, name) = rest.split_once(' ')?;
            Some((hash.to_owned(), name.trim().trim_matches('"').to_owned()))
        })
        .collect();
    if let Some((variable, identity)) = explicit {
        if !identities
            .iter()
            .any(|(hash, name)| hash.eq_ignore_ascii_case(&identity) || *name == identity)
        {
            bail!("{variable}={identity:?} is not a valid code-signing identity in the keychain");
        }
        return Ok(Some(identity));
    }
    let developer_id: Vec<&(String, String)> = identities
        .iter()
        .filter(|(_, name)| name.starts_with("Developer ID Application:"))
        .collect();
    match developer_id.as_slice() {
        [] => Ok(None),
        [(hash, name)] => {
            eprintln!("Signing the development Nanocodex.app with {name}");
            Ok(Some(hash.clone()))
        }
        _ => bail!(
            "several Developer ID Application identities are installed; choose one with {SIGNING_IDENTITY_ENV}"
        ),
    }
}

/// On macOS, require an intact signature sealed with [`IDENTIFIER`]. Other
/// platforms never run the bundle and only check its structure.
#[cfg(unix)]
pub(super) fn verify_signature(bundle: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let output = codesign(&["--verify".as_ref(), "--strict".as_ref(), bundle.as_os_str()])?;
    if !output.status.success() {
        bail!(
            "Nanocodex.app signature is invalid: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let identifier = signing_detail(bundle, "Identifier")?;
    if identifier.as_deref() != Some(IDENTIFIER) {
        bail!("Nanocodex.app is signed as {identifier:?}, expected {IDENTIFIER}");
    }
    Ok(())
}

/// One `codesign --display --verbose=2` field (macOS only).
fn signing_detail(bundle: &Path, field: &str) -> Result<Option<String>> {
    let output = codesign(&[
        "--display".as_ref(),
        "--verbose=2".as_ref(),
        bundle.as_os_str(),
    ])?;
    // codesign prints the details on stderr.
    let text = String::from_utf8_lossy(&output.stderr);
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{field}=")))
        .map(|value| value.trim().to_owned()))
}

/// The signing team of the bundle containing `executable`, or None for ad
/// hoc, unsigned, non-bundle executables, or platforms without codesign.
pub(super) fn team_of(executable: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let bundle = bundle_of(executable)?;
    signing_detail(&bundle, "TeamIdentifier")
        .ok()
        .flatten()
        .filter(|team| team != "not set")
}

/// The `Nanocodex.app` directory that contains `executable`, if any.
pub(super) fn bundle_of(executable: &Path) -> Option<PathBuf> {
    executable
        .parent()
        .filter(|directory| directory.ends_with("Nanocodex.app/Contents/MacOS"))
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}
