use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{Error, Result, canonical_origin, validate_key};

const MAX_STORE_BYTES: u64 = 64 * 1024;

#[derive(Deserialize, Serialize)]
pub(crate) struct Credential {
    pub(crate) api_key: String,
}

impl Drop for Credential {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Store {
    version: u32,
    pub(crate) accounts: BTreeMap<String, Credential>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: BTreeMap::new(),
        }
    }
}

pub(crate) fn default_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NANOCODEX_ACCOUNT_FILE") {
        if path.is_empty() {
            return Err(Error::message("NANOCODEX_ACCOUNT_FILE must not be empty"));
        }
        return Ok(path.into());
    }
    let home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".codex"))
        })
        .ok_or_else(|| {
            Error::message("Set CODEX_HOME or NANOCODEX_ACCOUNT_FILE to store account credentials")
        })?;
    Ok(home.join("nanocodex-account.json"))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

// Check before opening so a misplaced symlink, device, or public credential
// file cannot be consumed as an account record.
fn private_file(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(Error::message("Cannot inspect the account credential file")),
    };
    if !metadata.is_file() {
        return Err(Error::message(
            "Account credential files must be regular files, not symlinks",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::message(
                "Account credential file must be private; use chmod 600 on it",
            ));
        }
    }
    Ok(true)
}

pub(crate) fn load(path: &Path) -> Result<Store> {
    if !private_file(path)? {
        return Ok(Store::default());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    File::open(path)
        .and_then(|file| file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| Error::message("Cannot read the account credential file"))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(Error::message("Account credential file is too large"));
    }
    let store: Store = serde_json::from_slice(&bytes)
        .map_err(|_| Error::message("Account credential file is malformed"))?;
    if store.version != 1 {
        return Err(Error::message(
            "Unsupported account credential file version",
        ));
    }
    for (origin, credential) in &store.accounts {
        if canonical_origin(origin)? != *origin {
            return Err(Error::message("Stored account origin is not canonical"));
        }
        validate_key(&credential.api_key)?;
    }
    Ok(store)
}

/// Keep the lock alive across login and logout so concurrent CLI processes
/// cannot overwrite another origin or restore credentials after logout.
pub(crate) struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // A descriptor inherited by a concurrent fork may outlive this
        // transaction. Release ownership explicitly instead of only closing.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub(crate) fn lock(path: &Path) -> Result<Lock> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent(path))
        .map_err(|_| Error::message("Cannot create the account credential directory"))?;
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    private_file(&lock_path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(lock_path)
        .map_err(|_| Error::message("Cannot open the account credential lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
        Error::message("Another account login or logout is running; finish it first")
    })?;
    Ok(Lock(lock))
}

pub(crate) fn save(path: &Path, store: &Store) -> Result<()> {
    private_file(path)?;
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec_pretty(store)
            .map_err(|_| Error::message("Cannot encode account credentials"))?,
    );
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(Error::message("Account credential file is too large"));
    }
    let mut file = tempfile::NamedTempFile::new_in(parent(path))
        .map_err(|_| Error::message("Cannot prepare the account credential file"))?;
    // NamedTempFile is mode 0600 on Unix. Rename publishes a complete record
    // and never truncates the last working credential on a failed write.
    file.write_all(&bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| Error::message("Cannot write the account credential file"))?;
    file.persist(path)
        .map_err(|_| Error::message("Cannot save the account credential file"))?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn finished_transaction_releases_inherited_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let transaction = super::lock(&path).unwrap();
        // dup and fork share the same open file description and lock lifetime.
        let inherited = transaction.0.try_clone().unwrap();
        assert!(super::lock(&path).is_err());
        drop(transaction);
        let next = super::lock(&path).unwrap();
        drop(inherited);
        assert!(super::lock(&path).is_err());
        drop(next);
        assert!(super::lock(&path).is_ok());
    }
}
