//! Private durable state for an independently owned browser route. No command
//! arguments, DOM contents, cookies, input values or endpoint credentials persist.
use crate::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const MAX_STATE: u64 = 8 * 1024 * 1024;
pub struct Store {
    directory: PathBuf,
    path: PathBuf,
    _lease: File,
}
impl Store {
    pub fn open(directory: &Path, key: &str) -> Result<Self> {
        private_directory(directory)?;
        let name = format!("{:x}", Sha256::digest(key.as_bytes()));
        let path = directory.join(format!("{name}.json"));
        let lease_path = directory.join(format!("{name}.lock"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        private_options(&mut options);
        let lease = options.open(&lease_path)?;
        private_file(&lease)?;
        lease.try_lock().map_err(|_| {
            Error::action("Browser route durable state is already owned by another host")
        })?;
        Ok(Self {
            directory: directory.into(),
            path,
            _lease: lease,
        })
    }
    pub fn load<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        let mut options = OpenOptions::new();
        options.read(true);
        private_options(&mut options);
        let file = match options.open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        private_file(&file)?;
        if file.metadata()?.len() > MAX_STATE {
            return Err(Error::action("Browser durable state exceeds 8 MiB"));
        }
        let mut bytes = vec![];
        file.take(MAX_STATE + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE {
            return Err(Error::action("Browser durable state exceeds 8 MiB"));
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
    pub fn save<T: Serialize>(&self, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() as u64 > MAX_STATE {
            return Err(Error::action("Browser durable state exceeds 8 MiB"));
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| Error::action("Cannot allocate browser state transaction"))?;
        let temporary = self.directory.join(format!(
            ".{}.tmp",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        let write = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            private_options(&mut options);
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if write.is_err() {
            let _ = fs::remove_file(temporary);
        }
        write
    }
}
#[cfg(unix)]
fn private_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
}
#[cfg(not(unix))]
fn private_options(_: &mut OpenOptions) {}
#[cfg(unix)]
fn private_file(file: &File) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(Error::action(
            "Browser durable files must be owned and private (0600)",
        ));
    }
    Ok(())
}
#[cfg(not(unix))]
fn private_file(_: &File) -> Result<()> {
    Err(Error::unsupported(
        "Private browser durable state requires an implemented platform ACL adapter",
    ))
}
#[cfg(unix)]
fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if !path.exists() {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(Error::action(
            "Browser durable directory must be owned and private (0700)",
        ));
    }
    Ok(())
}
#[cfg(not(unix))]
fn private_directory(_: &Path) -> Result<()> {
    Err(Error::unsupported(
        "Private browser durable state requires an implemented platform ACL adapter",
    ))
}

/// Read explicit host capability configuration without following a file symlink.
pub fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    private_options(&mut options);
    let file = options.open(path)?;
    private_file(&file)?;
    if file.metadata()?.len() > limit {
        return Err(Error::invalid(
            "Private host configuration exceeds its byte limit",
        ));
    }
    let mut bytes = vec![];
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::invalid(
            "Private host configuration exceeds its byte limit",
        ));
    }
    Ok(bytes)
}
