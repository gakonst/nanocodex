//! Verified voice resources are installed and cached with their owning version.
use std::{collections::BTreeSet, fs, io::Read, path::Path};

use eyre::{Result, bail};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

const ROOT: &str = "nanocodex-resources/voice/";
const RECEIPT: &str = "nanocodex-voice.sha256";
const ARCHIVE_RECEIPT: &str = "nanocodex-voice.archive.sha256";
const MAX_BYTES: u64 = 256 * 1024 * 1024;

pub(super) fn asset_name(binary: &str) -> String {
    format!(
        "{}.tar.gz",
        binary.replacen("nanocodex-", "nanocodex-voice-", 1)
    )
}

fn valid_path(name: &str) -> bool {
    name.starts_with(ROOT)
        && name.len() > ROOT.len()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"/_+.-".contains(&c))
        && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

pub(super) fn cached(directory: &Path, expected_archive: Option<&str>) -> Result<bool> {
    let receipt_path = directory.join(RECEIPT);
    if receipt_path.is_symlink()
        || !receipt_path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024)
    {
        return Ok(false);
    }
    let Ok(receipt) = fs::read_to_string(directory.join(RECEIPT)) else {
        return Ok(false);
    };
    if receipt.len() > 64 * 1024 {
        return Ok(false);
    }
    let archive_path = directory.join(ARCHIVE_RECEIPT);
    if archive_path.is_symlink()
        || !archive_path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= 65)
    {
        return Ok(false);
    }
    let Ok(archive_digest) = fs::read_to_string(archive_path) else {
        return Ok(false);
    };
    let archive_digest = archive_digest.trim();
    if archive_digest.len() != 64
        || !archive_digest.bytes().all(|c| c.is_ascii_hexdigit())
        || expected_archive.is_some_and(|expected| expected != archive_digest)
    {
        return Ok(false);
    }
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    for line in receipt.lines() {
        let Some((digest, name)) = line.split_once("  ") else {
            return Ok(false);
        };
        if !valid_path(name) || names.len() >= 256 || !names.insert(name.to_owned()) {
            return Ok(false);
        }
        let path = directory.join(name);
        if path
            .ancestors()
            .take_while(|p| *p != directory)
            .any(|p| p.is_symlink())
        {
            return Ok(false);
        }
        let Ok(metadata) = path.metadata() else {
            return Ok(false);
        };
        total = total.saturating_add(metadata.len());
        if !metadata.is_file() || total > MAX_BYTES {
            return Ok(false);
        }
        #[cfg(unix)]
        if name.ends_with("/bin/nanocodex-voice-host") {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Ok(false);
            }
        }
        let Ok(bytes) = fs::read(path) else {
            return Ok(false);
        };
        if hex::encode(Sha256::digest(bytes)) != digest {
            return Ok(false);
        }
    }
    Ok(required_files(&names))
}

fn required_files(names: &BTreeSet<String>) -> bool {
    [
        "bin/nanocodex-voice-host",
        "runtime.json",
        "NOTICE.md",
        "sources.json",
        "manifest.json",
    ]
    .iter()
    .all(|name| names.contains(&format!("{ROOT}{name}")))
        && names
            .iter()
            .any(|name| name.starts_with(&format!("{ROOT}lib/")))
        && names
            .iter()
            .any(|name| name.starts_with(&format!("{ROOT}licenses/")))
}

pub(super) fn install(directory: &Path, bytes: &[u8]) -> Result<()> {
    let staging = tempfile::Builder::new()
        .prefix(".voice-")
        .tempdir_in(directory)?;
    let mut archive = tar::Archive::new(GzDecoder::new(bytes).take(MAX_BYTES + 1));
    let mut names = BTreeSet::new();
    let mut folded = BTreeSet::new();
    let mut total = 0_u64;
    let mut receipt = String::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = String::from_utf8(entry.path_bytes().into_owned())?;
        if !valid_path(&name)
            || !entry.header().entry_type().is_file()
            || !folded.insert(name.to_ascii_lowercase())
            || names.len() >= 256
        {
            bail!("invalid voice runtime archive entry");
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| eyre::eyre!("voice runtime too large"))?;
        if total > MAX_BYTES {
            bail!("voice runtime exceeds size limit");
        }
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents)?;
        if contents.len() as u64 != entry.size() {
            bail!("truncated voice runtime archive entry");
        }
        let path = staging.path().join(&name);
        fs::create_dir_all(path.parent().unwrap())?;
        super::store::atomic_write(
            &path,
            &contents,
            name.ends_with("/bin/nanocodex-voice-host"),
        )?;
        receipt.push_str(&format!(
            "{}  {name}\n",
            hex::encode(Sha256::digest(contents))
        ));
        names.insert(name);
    }
    if !required_files(&names) {
        bail!("voice runtime archive is incomplete");
    }
    let resources = directory.join("nanocodex-resources");
    if resources.is_symlink() {
        bail!("voice resources directory must not be a symbolic link");
    }
    fs::create_dir_all(&resources)?;
    let destination = resources.join("voice");
    let previous = staging.path().join("previous");
    if destination.try_exists()? || destination.is_symlink() {
        fs::rename(&destination, &previous)?;
    }
    if let Err(error) = fs::rename(
        staging.path().join("nanocodex-resources/voice"),
        &destination,
    ) {
        if previous.exists() {
            let _ = fs::rename(&previous, &destination);
        }
        return Err(error.into());
    }
    super::store::atomic_write(&directory.join(RECEIPT), receipt.as_bytes(), false)?;
    super::store::atomic_write(
        &directory.join(ARCHIVE_RECEIPT),
        format!("{}\n", hex::encode(Sha256::digest(bytes))).as_bytes(),
        false,
    )?;
    Ok(())
}

#[cfg(test)]
pub(super) fn fixture(extra: Option<(&str, tar::EntryType)>) -> Vec<u8> {
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for name in [
        "bin/nanocodex-voice-host",
        "runtime.json",
        "NOTICE.md",
        "sources.json",
        "manifest.json",
        "lib/libgstreamer-1.0.so.0",
        "licenses/LGPL-2.1.txt",
    ] {
        let mut header = tar::Header::new_ustar();
        header.set_size(7);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{ROOT}{name}"), &b"fixture"[..])
            .unwrap();
    }
    if let Some((name, kind)) = extra {
        let mut header = tar::Header::new_ustar();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_entry_type(kind);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("/tmp/outside").unwrap();
        }
        header.set_cksum();
        archive
            .append_data(&mut header, name, std::io::empty())
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_integrity_and_archive_identity_are_required_for_cache_hits() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = fixture(None);
        install(directory.path(), &bytes).unwrap();
        let digest = hex::encode(Sha256::digest(&bytes));
        assert!(cached(directory.path(), Some(&digest)).unwrap());
        assert!(!cached(directory.path(), Some("different-release")).unwrap());
        fs::write(
            directory
                .path()
                .join(format!("{ROOT}lib/libgstreamer-1.0.so.0")),
            b"corrupt",
        )
        .unwrap();
        assert!(!cached(directory.path(), Some(&digest)).unwrap());
        install(directory.path(), &bytes).unwrap();
        assert!(cached(directory.path(), Some(&digest)).unwrap());
    }

    #[test]
    fn invalid_archive_does_not_replace_the_installed_runtime() {
        let directory = tempfile::tempdir().unwrap();
        install(directory.path(), &fixture(None)).unwrap();
        for (name, kind) in [
            ("outside", tar::EntryType::Regular),
            ("nanocodex-resources/voice/link", tar::EntryType::Symlink),
            ("nanocodex-resources/voice/link", tar::EntryType::Link),
            (
                "nanocodex-resources/voice/runtime.json",
                tar::EntryType::Regular,
            ),
        ] {
            assert!(install(directory.path(), &fixture(Some((name, kind)))).is_err());
            assert!(cached(directory.path(), None).unwrap());
        }
        assert!(install(directory.path(), b"truncated").is_err());
        assert!(cached(directory.path(), None).unwrap());
        assert!(!directory.path().join("outside").exists());
    }
}
