//! The single Linux Hand executable carries its Wayland helpers and ELF loader.
//! No downloads, administrator actions, or global loader environment occur at
//! screen startup. Cache contents are owner-private and verified before use.
use nanocodex_managed::ManagedError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::Command,
};

/// The verified Wayland helper bundle (scripts/build-linux-screen-helpers.sh)
/// named by NANOCODEX_LINUX_SCREEN_BUNDLE. Distributable builds enable the
/// feature; other builds report the missing payload when screen sharing starts.
#[cfg(feature = "embedded-screen-helpers")]
const BUNDLE: &[u8] = include_bytes!(env!("NANOCODEX_LINUX_SCREEN_BUNDLE"));
#[cfg(not(feature = "embedded-screen-helpers"))]
const BUNDLE: &[u8] = &[];
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 512;
fn error(e: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(format!("Linux screen helpers: {e}"))
}
#[derive(Deserialize)]
struct Manifest {
    version: u32,
    architecture: String,
    files: Vec<ManifestFile>,
}
#[derive(Deserialize)]
struct ManifestFile {
    path: String,
    sha256: String,
    bytes: u64,
    mode: u32,
}

fn loader_path() -> Result<&'static str, ManagedError> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("lib/ld-linux-x86-64.so.2"),
        "aarch64" => Ok("lib/ld-linux-aarch64.so.1"),
        _ => Err(error("unsupported helper architecture")),
    }
}

pub(crate) fn command(name: &str) -> Result<Command, ManagedError> {
    if !matches!(name, "waymote-streamd" | "grim") {
        return Err(error("unknown helper"));
    }
    // Managed desktop images explicitly provision their architecture-specific
    // helpers as image assets. This is a configured executable, never a silent
    // response to a missing/tampered embedded bundle or an encoder failure.
    if name == "grim"
        && let Some(executable) = std::env::var_os("NANOCODEX_GRIM")
    {
        if executable.is_empty() {
            return Err(error("NANOCODEX_GRIM must not be empty"));
        }
        let mut command = Command::new(executable);
        command
            .env_remove("LD_PRELOAD")
            .env_remove("LD_AUDIT")
            .env_remove("LD_LIBRARY_PATH");
        return Ok(command);
    }
    if BUNDLE.is_empty() {
        return Err(error(
            "this build lacks the embedded Wayland bundle; rebuild with scripts/build-linux-screen-helpers.sh, NANOCODEX_LINUX_SCREEN_BUNDLE and --features nanocodex-hand-daemon/embedded-screen-helpers",
        ));
    }
    let uid = nix::unistd::geteuid().as_raw();
    let root = PathBuf::from(format!("/tmp/nanocodex-screen-helpers-{uid}"));
    let directory = install(BUNDLE, &root, uid)?;
    let mut command = Command::new(directory.join(loader_path()?));
    command
        .arg("--library-path")
        .arg(directory.join("lib"))
        .arg(directory.join("bin").join(name))
        // Never let inherited dynamic loader injection cross this boundary.
        .env_remove("LD_PRELOAD")
        .env_remove("LD_AUDIT")
        .env_remove("LD_LIBRARY_PATH");
    Ok(command)
}

fn private_directory(path: &Path, uid: u32) -> Result<(), ManagedError> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(error(e)),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(error)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
        return Err(error(
            "cache directory must be a same-owner, non-symlink directory with mode 0700",
        ));
    }
    Ok(())
}
fn private_file(path: &Path, uid: u32, mode: u32) -> Result<File, ManagedError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != mode
    {
        return Err(error("cache file ownership, type, links, or mode changed"));
    }
    Ok(file)
}
fn relative(value: &str) -> Result<PathBuf, ManagedError> {
    let value = value.strip_prefix("./").unwrap_or(value);
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || value.len() > 512
        || value.bytes().any(|b| b.is_ascii_control())
        || path.components().count() > 8
        || value.contains('\\')
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error("archive path must be normal and relative"));
    }
    Ok(path.to_owned())
}
fn manifest(bytes: &[u8]) -> Result<Manifest, ManagedError> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("manifest exceeds limit"));
    }
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(error)?;
    if manifest.version != 1 || manifest.architecture != std::env::consts::ARCH {
        return Err(error("unsupported manifest version or architecture"));
    }
    if manifest.files.len() > MAX_FILES {
        return Err(error("too many files"));
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        let path = relative(&file.path)?;
        if path == Path::new("manifest.json")
            || !names.insert(path)
            || !matches!(file.mode, 0o644 | 0o755)
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(error("invalid or duplicate manifest file"));
        }
        total = total
            .checked_add(file.bytes)
            .ok_or_else(|| error("size overflow"))?;
        if total > MAX_BYTES {
            return Err(error("expanded bundle exceeds limit"));
        }
    }
    for name in ["bin/waymote-streamd", "bin/grim", loader_path()?] {
        if !manifest
            .files
            .iter()
            .any(|f| f.path == name && f.mode == 0o755)
        {
            return Err(error(format!("required executable {name} missing")));
        }
    }
    Ok(manifest)
}
fn install(bytes: &[u8], root: &Path, uid: u32) -> Result<PathBuf, ManagedError> {
    use fs2::FileExt;
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err(error("compressed bundle exceeds limit or is empty"));
    }
    let expected = bundled_manifest(bytes)?;
    manifest(&expected)?;
    private_directory(root, uid)?;
    let lock_path = root.join("install.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&lock_path)
        .map_err(error)?;
    // Check the actual opened inode, not only a path before opening.
    let metadata = lock.metadata().map_err(error)?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(error("invalid cache lock"));
    }
    lock.lock_exclusive().map_err(error)?;
    let destination = root.join(hex::encode(Sha256::digest(bytes)));
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => {
            verify(&destination, uid, &expected)?;
            return Ok(destination);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(error(e)),
    }
    // tempfile creates directories as 0777 & umask (0755 under the usual 022),
    // which verify() rightly rejects; request the private mode explicitly.
    let temporary = tempfile::Builder::new()
        .prefix("extract-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir_in(root)
        .map_err(error)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    let mut entries = 0usize;
    for entry in archive.entries().map_err(error)? {
        let mut entry = entry.map_err(error)?;
        entries += 1;
        if entries > MAX_FILES * 2 {
            return Err(error("too many archive entries"));
        }
        let raw = entry.path().map_err(error)?.to_string_lossy().into_owned();
        if entry.header().entry_type().is_dir() && matches!(raw.as_str(), "." | "./") {
            continue;
        }
        let relative = relative(&raw)?;
        let path = temporary.path().join(&relative);
        if entry.header().entry_type().is_dir() {
            create_parents(temporary.path(), &relative, uid)?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(error(
                "archive symlinks, hardlinks, and special entries are forbidden",
            ));
        }
        let size = entry.size();
        total = total
            .checked_add(size)
            .ok_or_else(|| error("size overflow"))?;
        if total > MAX_BYTES || files.len() >= MAX_FILES || files.contains_key(&relative) {
            return Err(error("duplicate file or expanded archive exceeds limit"));
        }
        if let Some(parent) = relative.parent() {
            create_parents(temporary.path(), parent, uid)?;
        }
        let mode = entry.header().mode().map_err(error)?;
        if !matches!(mode, 0o644 | 0o755) {
            return Err(error("archive file mode must be 0644 or 0755"));
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode & 0o700)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&path)
            .map_err(error)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut copied = 0u64;
        loop {
            let count = entry.read(&mut buffer).map_err(error)?;
            if count == 0 {
                break;
            }
            copied += count as u64;
            if copied > size {
                return Err(error("archive file exceeds declaration"));
            }
            hasher.update(&buffer[..count]);
            output.write_all(&buffer[..count]).map_err(error)?;
        }
        output.sync_all().map_err(error)?;
        if copied != size {
            return Err(error("truncated archive file"));
        }
        files.insert(relative, (hex::encode(hasher.finalize()), size, mode));
    }
    let manifest_path = temporary.path().join("manifest.json");
    let data = std::fs::read(manifest_path).map_err(error)?;
    let parsed = manifest(&data)?;
    if files.len() != parsed.files.len() + 1 {
        return Err(error("archive and manifest file inventory differ"));
    }
    for file in &parsed.files {
        if files.get(&relative(&file.path)?) != Some(&(file.sha256.clone(), file.bytes, file.mode))
        {
            return Err(error(
                "archive digest, length, or mode differs from manifest",
            ));
        }
    }
    verify(temporary.path(), uid, &expected)?;
    std::fs::rename(temporary.path(), &destination).map_err(error)?;
    // TempDir cleanup sees the old, now nonexistent path. No cache deletion.
    Ok(destination)
}
fn create_parents(root: &Path, relative: &Path, uid: u32) -> Result<(), ManagedError> {
    let mut parent = root.to_owned();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(error("invalid parent path"));
        }
        parent.push(component.as_os_str());
        private_directory(&parent, uid)?;
    }
    Ok(())
}
fn bundled_manifest(bytes: &[u8]) -> Result<Vec<u8>, ManagedError> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut total = 0u64;
    for (index, entry) in archive.entries().map_err(error)?.enumerate() {
        let entry = entry.map_err(error)?;
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| error("size overflow"))?;
        if index >= MAX_FILES * 2 || total > MAX_BYTES {
            return Err(error("archive exceeds limits"));
        }
        if entry.path().map_err(error)?.as_ref() == Path::new("manifest.json") {
            if !entry.header().entry_type().is_file() || entry.size() > 1024 * 1024 {
                return Err(error("invalid manifest entry"));
            }
            let mut data = Vec::new();
            entry
                .take(1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map_err(error)?;
            return Ok(data);
        }
    }
    Err(error("embedded manifest missing"))
}
fn inventory(
    root: &Path,
    at: &Path,
    names: &mut BTreeSet<PathBuf>,
    uid: u32,
) -> Result<(), ManagedError> {
    for entry in std::fs::read_dir(at).map_err(error)? {
        let entry = entry.map_err(error)?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(error)?;
        if metadata.uid() != uid || metadata.file_type().is_symlink() {
            return Err(error("foreign or symlink cache entry"));
        }
        if metadata.is_dir() {
            if metadata.mode() & 0o7777 != 0o700 {
                return Err(error("invalid cache directory mode"));
            }
            inventory(root, &path, names, uid)?;
        } else if metadata.is_file() {
            names.insert(path.strip_prefix(root).map_err(error)?.to_owned());
            if names.len() > MAX_FILES {
                return Err(error("unexpected cache entries"));
            }
        } else {
            return Err(error("special cache entry"));
        }
    }
    Ok(())
}
fn verify(root: &Path, uid: u32, expected: &[u8]) -> Result<(), ManagedError> {
    let metadata = std::fs::symlink_metadata(root).map_err(error)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700 {
        return Err(error("invalid extracted cache directory"));
    }
    let mut data = Vec::new();
    private_file(&root.join("manifest.json"), uid, 0o600)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(error)?;
    if data != expected {
        return Err(error("cache manifest differs from embedded bundle"));
    }
    let manifest = manifest(&data)?;
    let mut expected_names: BTreeSet<_> = manifest
        .files
        .iter()
        .map(|f| relative(&f.path))
        .collect::<Result<_, _>>()?;
    expected_names.insert(PathBuf::from("manifest.json"));
    let mut actual_names = BTreeSet::new();
    inventory(root, root, &mut actual_names, uid)?;
    if expected_names != actual_names {
        return Err(error("cache inventory changed"));
    }
    for file in manifest.files {
        let relative = relative(&file.path)?;
        if let Some(parent) = relative.parent() {
            let mut path = root.to_owned();
            for part in parent.components() {
                path.push(part);
                let metadata = std::fs::symlink_metadata(&path).map_err(error)?;
                if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != 0o700
                {
                    return Err(error("invalid extracted parent directory"));
                }
            }
        }
        let mut opened = private_file(&root.join(&relative), uid, file.mode & 0o700)?;
        if opened.metadata().map_err(error)?.len() != file.bytes {
            return Err(error("cache length changed"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = opened.read(&mut buffer).map_err(error)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if hex::encode(hash.finalize()) != file.sha256 {
            return Err(error("cache digest changed"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    /// A cache root that satisfies the production 0700 contract regardless of
    /// the runner's umask (tempdirs are 0777 & umask, i.e. 0755 under 022).
    fn private_root() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }
    fn bundle(extra: Option<(&str, tar::EntryType)>) -> Vec<u8> {
        let content = b"test helper";
        let names = ["bin/waymote-streamd", "bin/grim", loader_path().unwrap()];
        let files: Vec<_> = names.iter().map(|path| serde_json::json!({"path":path,"sha256":hex::encode(Sha256::digest(content)),"bytes":content.len(),"mode":0o755})).collect();
        let manifest = serde_json::to_vec(
            &serde_json::json!({"version":1,"architecture":std::env::consts::ARCH,"files":files}),
        )
        .unwrap();
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (name, data, mode) in
            names
                .iter()
                .map(|s| (*s, content.as_slice(), 0o755))
                .chain(std::iter::once((
                    "manifest.json",
                    manifest.as_slice(),
                    0o644,
                )))
        {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive.append_data(&mut header, name, data).unwrap();
        }
        if let Some((name, kind)) = extra {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o644);
            header.set_entry_type(kind);
            header.set_link_name("/etc/passwd").unwrap();
            header.set_cksum();
            archive
                .append_data(&mut header, name, std::io::empty())
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }
    #[test]
    fn extract_reuse_and_tamper_fail_closed() {
        let root = private_root();
        let uid = nix::unistd::geteuid().as_raw();
        let data = bundle(None);
        let destination = install(&data, root.path(), uid).unwrap();
        assert_eq!(install(&data, root.path(), uid).unwrap(), destination);
        std::fs::write(destination.join("bin/grim"), b"tampered").unwrap();
        assert!(install(&data, root.path(), uid).is_err());
    }
    #[test]
    fn cache_symlink_and_open_permissions_fail_closed() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("cache");
        std::os::unix::fs::symlink(parent.path(), &path).unwrap();
        assert!(install(&bundle(None), &path, nix::unistd::geteuid().as_raw()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(install(&bundle(None), &path, nix::unistd::geteuid().as_raw()).is_err());
    }
    #[test]
    fn link_duplicate_and_traversal_entries_fail_closed() {
        for kind in [tar::EntryType::Symlink, tar::EntryType::Link] {
            let root = private_root();
            assert!(
                install(
                    &bundle(Some(("bin/evil", kind))),
                    root.path(),
                    nix::unistd::geteuid().as_raw()
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read_dir(root.path()).unwrap().count(),
                1,
                "no partial extraction survives"
            );
        }
        assert!(relative("/etc/passwd").is_err());
        assert!(relative("../escape").is_err());
        assert!(relative("bin/../../escape").is_err());
        assert!(relative("bin\\evil").is_err());
    }
    #[test]
    fn changed_manifest_unlisted_library_and_links_are_rejected() {
        let uid = nix::unistd::geteuid().as_raw();
        let data = bundle(None);
        for variant in 0..4 {
            let root = private_root();
            let destination = install(&data, root.path(), uid).unwrap();
            match variant {
                0 => {
                    let path = destination.join("manifest.json");
                    let mut value: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    std::fs::write(destination.join("bin/grim"), b"altered").unwrap();
                    value["files"][1]["bytes"] = serde_json::json!(7);
                    value["files"][1]["sha256"] =
                        serde_json::json!(hex::encode(Sha256::digest(b"altered")));
                    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                1 => {
                    std::fs::write(destination.join("lib/injected.so"), b"extra").unwrap();
                }
                2 => {
                    std::fs::remove_file(destination.join("bin/grim")).unwrap();
                    std::os::unix::fs::symlink(
                        destination.join("bin/waymote-streamd"),
                        destination.join("bin/grim"),
                    )
                    .unwrap();
                }
                _ => {
                    std::fs::hard_link(
                        destination.join("bin/grim"),
                        root.path().join("external-link"),
                    )
                    .unwrap();
                }
            }
            assert!(install(&data, root.path(), uid).is_err());
        }
        let root = private_root();
        assert!(
            install(
                &bundle(Some(("bin/grim", tar::EntryType::Regular))),
                root.path(),
                uid
            )
            .is_err()
        );
    }
    #[test]
    #[ignore = "requires NANOCODEX_LINUX_SCREEN_BUNDLE at build time"]
    fn embedded_helpers_run_with_empty_path() {
        assert!(
            !BUNDLE.is_empty(),
            "acceptance requires a real embedded bundle"
        );
        for (name, argument) in [("waymote-streamd", "--help"), ("grim", "-h")] {
            let output = command(name)
                .unwrap()
                .arg(argument)
                .env("PATH", "")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!output.stdout.is_empty() || !output.stderr.is_empty());
        }
    }
    #[test]
    fn owner_and_manifest_limits_are_enforced() {
        let root = private_root();
        let uid = nix::unistd::geteuid().as_raw();
        assert!(install(&bundle(None), root.path(), uid.wrapping_add(1)).is_err());
        assert!(manifest(b"{}").is_err());
        assert!(manifest(&vec![b' '; 1024 * 1024 + 1]).is_err());
        assert!(command("anything").is_err());
    }
}
