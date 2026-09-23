//! Disposable local startup caches. They never authorize a provider invocation.
//! A managed bundle fingerprint is checked before catalog registration; each
//! conversation still discovers and compares the live catalog before tools/call.
use crate::{ComputerConfig, ProviderTool};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const MAX_AGE_SECS: u64 = 3600;
const MAX_CACHE_BYTES: u64 = 1024 * 1024;
const CACHE_FORMAT: u32 = 1;

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn fresh(saved: u64, current: u64) -> bool {
    current
        .checked_sub(saved)
        .is_some_and(|age| age < MAX_AGE_SECS)
}

fn private_directory(path: &Path) -> Option<()> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.is_dir() && metadata.mode() & 0o077 == 0).then_some(())
}

pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Option<T> {
    private_directory(path.parent()?)?;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES || metadata.mode() & 0o077 != 0 {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let opened = file.metadata().ok()?;
    if (opened.dev(), opened.ino()) != (metadata.dev(), metadata.ino()) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Option<()> {
    let parent = path.parent()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .ok()?;
    private_directory(parent)?;
    let bytes = serde_json::to_vec(value).ok()?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stage = parent.join(format!(
        ".startup-{}-{}-{}",
        std::process::id(),
        now(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stage)
            .ok()?;
        file.write_all(&bytes).ok()?;
        fs::rename(&stage, path).ok()
    })();
    let _ = fs::remove_file(stage);
    result
}

fn part(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}
fn hex(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The whole tree participates, including directories, nested resources and
/// symlinks. ctime catches edits with a restored mtime; inode/device catch
/// replacement. Symlinks outside the tree and unusual filesystem entries are
/// deliberately uncacheable. No content digest or signature check is skipped
/// after any observed mutation. The scan is bounded even for a damaged bundle.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn fingerprint(root: &Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let mut pending = vec![(root.clone(), 0)];
    let mut count = 0;
    let mut digest = Sha256::new();
    part(&mut digest, root.as_os_str().as_bytes());
    while let Some((path, depth)) = pending.pop() {
        count += 1;
        if count > 65536 || depth > 128 {
            return None;
        }
        let metadata = fs::symlink_metadata(&path).ok()?;
        part(
            &mut digest,
            path.strip_prefix(&root).ok()?.as_os_str().as_bytes(),
        );
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.mode() as u64,
            metadata.nlink(),
            metadata.uid() as u64,
            metadata.gid() as u64,
            metadata.rdev(),
            metadata.len(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            digest.update(value.to_le_bytes());
        }
        if metadata.is_symlink() {
            let target = fs::read_link(&path).ok()?;
            let (mut resolved, relative) = if target.is_absolute() {
                (root.clone(), target.strip_prefix(&root).ok()?)
            } else {
                (path.parent()?.to_owned(), target.as_path())
            };
            // Reject even an external hop that resolves back into the bundle:
            // that intermediate symlink would not participate in this scan.
            for component in relative.components() {
                match component {
                    std::path::Component::Normal(name) => resolved.push(name),
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir if resolved != root => {
                        resolved.pop();
                    }
                    _ => return None,
                }
            }
            if !resolved.starts_with(&root) || !path.canonicalize().ok()?.starts_with(&root) {
                return None;
            }
            part(&mut digest, target.as_os_str().as_bytes());
        } else if metadata.is_dir() {
            let mut children = fs::read_dir(&path)
                .ok()?
                .take(65537usize.saturating_sub(count + pending.len()))
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            children.sort();
            if count + pending.len() + children.len() > 65536 {
                return None;
            }
            pending.extend(children.into_iter().rev().map(|path| (path, depth + 1)));
        } else if !metadata.is_file() {
            return None;
        }
    }
    Some(hex(digest))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CatalogCache {
    path: PathBuf,
    identity: String,
}

#[derive(Deserialize, Serialize)]
struct CatalogRecord {
    format: u32,
    saved_at: u64,
    identity: String,
    tools: Vec<ProviderTool>,
}

impl CatalogCache {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn managed(root: &Path, host: &Path, bundle: &str) -> Self {
        let mut digest = Sha256::new();
        part(&mut digest, host.as_os_str().as_bytes());
        part(&mut digest, bundle.as_bytes());
        Self {
            path: root.join(".startup-cache").join("catalog-v1.json"),
            identity: hex(digest),
        }
    }

    fn key(&self, config: &ComputerConfig) -> Option<String> {
        // Public launch configuration can be changed after discovery. Such a
        // change, including inherited OS environment, invalidates this catalog.
        if self.identity.len() != 64 || !self.identity.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        let mut digest = Sha256::new();
        part(&mut digest, self.identity.as_bytes());
        part(&mut digest, config.executable.as_os_str().as_bytes());
        digest.update((config.args.len() as u64).to_le_bytes());
        for arg in &config.args {
            part(&mut digest, arg.as_os_str().as_bytes());
        }
        for name in crate::PROVIDER_ENVIRONMENT {
            part(&mut digest, name.as_bytes());
            match std::env::var_os(name) {
                Some(value) => {
                    digest.update([1]);
                    part(&mut digest, value.as_os_str().as_bytes());
                }
                None => digest.update([0]),
            }
        }
        digest.update((config.environment.len() as u64).to_le_bytes());
        for (name, value) in &config.environment {
            part(&mut digest, name.as_os_str().as_bytes());
            part(&mut digest, value.as_os_str().as_bytes());
        }
        Some(hex(digest))
    }

    pub(crate) fn load(&self, config: &ComputerConfig) -> Option<Vec<ProviderTool>> {
        let record: CatalogRecord = read(&self.path)?;
        if record.format != CACHE_FORMAT
            || !fresh(record.saved_at, now())
            || record.identity != self.key(config)?
            || crate::validate_catalog(&record.tools).is_err()
        {
            return None;
        }
        Some(record.tools)
    }

    pub(crate) fn invalidate(&self, config: &ComputerConfig) {
        if let Some(record) = read::<CatalogRecord>(&self.path)
            && Some(record.identity) == self.key(config)
            && config.provider_catalog.as_ref() == Some(&record.tools)
        {
            // Losing a concurrent refresh here only causes rediscovery; a cache
            // is disposable. Never replace it with a mismatching live catalog.
            let _ = fs::remove_file(&self.path);
        }
    }

    pub(crate) fn save(&self, config: &ComputerConfig, tools: &[ProviderTool]) {
        if let Some(identity) = self.key(config) {
            let _ = write(
                &self.path,
                &CatalogRecord {
                    format: CACHE_FORMAT,
                    saved_at: now(),
                    identity,
                    tools: tools.to_vec(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
