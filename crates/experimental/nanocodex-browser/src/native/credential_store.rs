use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use chromiumoxide::cdp::browser_protocol::web_authn::Credential;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::BrowserError;

const STORE_VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CREDENTIALS: usize = 1_024;

pub(super) type CredentialKey = (String, String);

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialStoreFile {
    version: u32,
    credentials: Vec<Credential>,
}

pub(super) struct VirtualCredentialStore {
    path: PathBuf,
    credentials: BTreeMap<CredentialKey, Credential>,
}

impl VirtualCredentialStore {
    pub(super) fn load(path: PathBuf) -> Result<Self, BrowserError> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    credentials: BTreeMap::new(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(store_error(&path, "must not be a symbolic link"));
        }
        if metadata.len() > MAX_STORE_BYTES {
            return Err(store_error(
                &path,
                format!("file exceeds the {MAX_STORE_BYTES}-byte limit"),
            ));
        }
        restrict_secret_permissions(&path)?;
        let contents = fs::read(&path)?;
        let stored: CredentialStoreFile = serde_json::from_slice(&contents)?;
        if stored.version != STORE_VERSION {
            return Err(store_error(
                &path,
                format!(
                    "unsupported version {}; expected {STORE_VERSION}",
                    stored.version
                ),
            ));
        }
        if stored.credentials.len() > MAX_CREDENTIALS {
            return Err(store_error(
                &path,
                format!("contains more than {MAX_CREDENTIALS} credentials"),
            ));
        }
        let credentials = credential_map(stored.credentials, &path)?;
        Ok(Self { path, credentials })
    }

    pub(super) fn credentials(&self) -> impl Iterator<Item = &Credential> {
        self.credentials.values()
    }

    pub(super) fn reconcile(
        &mut self,
        snapshots: &[Vec<Credential>],
        presented: &BTreeSet<CredentialKey>,
    ) -> Result<(), BrowserError> {
        if snapshots.is_empty() {
            return Ok(());
        }
        let snapshots = snapshots
            .iter()
            .cloned()
            .map(|credentials| credential_map(credentials, &self.path))
            .collect::<Result<Vec<_>, _>>()?;
        let deleted = self
            .credentials
            .keys()
            .filter(|key| presented.contains(*key))
            .filter(|key| {
                snapshots
                    .iter()
                    .any(|snapshot| !snapshot.contains_key(*key))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut next = self.credentials.clone();
        next.retain(|key, _| !deleted.contains(key));
        for snapshot in &snapshots {
            for (key, credential) in snapshot {
                if deleted.contains(key) {
                    continue;
                }
                match next.get(key) {
                    Some(current)
                        if current.sign_count > credential.sign_count || current == credential => {}
                    _ => {
                        next.insert(key.clone(), credential.clone());
                    }
                }
            }
        }
        if next.len() > MAX_CREDENTIALS {
            return Err(store_error(
                &self.path,
                format!("would contain more than {MAX_CREDENTIALS} credentials"),
            ));
        }
        if next != self.credentials {
            self.persist(&next)?;
            self.credentials = next;
        }
        Ok(())
    }

    pub(super) fn snapshot(&self) -> BTreeMap<(String, String), Credential> {
        self.credentials.clone()
    }

    fn persist(
        &self,
        credentials: &BTreeMap<CredentialKey, Credential>,
    ) -> Result<(), BrowserError> {
        let contents = serde_json::to_vec_pretty(&CredentialStoreFile {
            version: STORE_VERSION,
            credentials: credentials.values().cloned().collect(),
        })?;
        if contents.len() as u64 > MAX_STORE_BYTES {
            return Err(store_error(
                &self.path,
                format!("serialized file exceeds the {MAX_STORE_BYTES}-byte limit"),
            ));
        }
        atomic_write_secret(&self.path, &contents)?;
        Ok(())
    }
}

fn credential_map(
    credentials: Vec<Credential>,
    path: &Path,
) -> Result<BTreeMap<CredentialKey, Credential>, BrowserError> {
    let mut mapped = BTreeMap::new();
    for credential in credentials {
        let relying_party = credential
            .rp_id
            .as_ref()
            .filter(|relying_party| !relying_party.is_empty())
            .ok_or_else(|| store_error(path, "credential is missing its relying-party ID"))?;
        let credential_id = String::from(credential.credential_id.clone());
        if credential_id.is_empty() {
            return Err(store_error(path, "credential has an empty ID"));
        }
        let key = (relying_party.clone(), credential_id);
        if mapped.insert(key, credential).is_some() {
            return Err(store_error(path, "contains a duplicate credential"));
        }
    }
    Ok(mapped)
}

fn atomic_write_secret(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    let parent_existed = parent.exists();
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    if !parent_existed {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_secret_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_secret_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn store_error(path: &Path, message: impl Into<String>) -> BrowserError {
    BrowserError::VirtualCredentialStore {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chromiumoxide::{cdp::browser_protocol::web_authn::Credential, types::Binary};

    use super::VirtualCredentialStore;

    fn credential_with_id(credential_id: &str, sign_count: i64) -> Credential {
        let mut credential = Credential::new(
            Binary::from(credential_id.to_owned()),
            true,
            Binary::from("private-key".to_owned()),
            sign_count,
        );
        credential.rp_id = Some("wallet.example".to_owned());
        credential.user_name = Some("tester".to_owned());
        credential
    }

    fn credential(sign_count: i64) -> Credential {
        credential_with_id("credential-id", sign_count)
    }

    fn presented() -> BTreeSet<(String, String)> {
        BTreeSet::from([("wallet.example".to_owned(), "credential-id".to_owned())])
    }

    #[test]
    fn credentials_round_trip_across_store_instances() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("browser/passkeys.json");
        let mut first = VirtualCredentialStore::load(path.clone()).unwrap();
        first
            .reconcile(&[vec![credential(1)]], &BTreeSet::new())
            .unwrap();

        let second = VirtualCredentialStore::load(path).unwrap();
        assert_eq!(
            second.credentials().cloned().collect::<Vec<_>>(),
            [credential(1)]
        );
    }

    #[test]
    fn the_highest_observed_signature_counter_wins() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("passkeys.json");
        let mut store = VirtualCredentialStore::load(path).unwrap();
        store
            .reconcile(&[vec![credential(7)]], &BTreeSet::new())
            .unwrap();
        store
            .reconcile(&[vec![credential(6)], vec![credential(8)]], &presented())
            .unwrap();

        assert_eq!(store.credentials().next().unwrap().sign_count, 8);
    }

    #[test]
    fn deletion_from_one_synchronized_authenticator_removes_the_credential() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("passkeys.json");
        let mut store = VirtualCredentialStore::load(path.clone()).unwrap();
        store
            .reconcile(&[vec![credential(1)]], &BTreeSet::new())
            .unwrap();
        store
            .reconcile(&[Vec::new(), vec![credential(1)]], &presented())
            .unwrap();

        assert!(
            VirtualCredentialStore::load(path)
                .unwrap()
                .credentials()
                .next()
                .is_none()
        );
    }

    #[test]
    fn credentials_hidden_by_selection_are_not_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("passkeys.json");
        let selected = credential_with_id("selected", 1);
        let hidden = credential_with_id("hidden", 2);
        let mut store = VirtualCredentialStore::load(path.clone()).unwrap();
        store
            .reconcile(&[vec![selected.clone(), hidden.clone()]], &BTreeSet::new())
            .unwrap();
        store
            .reconcile(
                &[vec![selected.clone()]],
                &BTreeSet::from([("wallet.example".to_owned(), "selected".to_owned())]),
            )
            .unwrap();

        assert_eq!(
            VirtualCredentialStore::load(path)
                .unwrap()
                .credentials()
                .cloned()
                .collect::<Vec<_>>(),
            [hidden, selected]
        );
    }

    #[cfg(unix)]
    #[test]
    fn persisted_credentials_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("browser/passkeys.json");
        let mut store = VirtualCredentialStore::load(path.clone()).unwrap();
        store
            .reconcile(&[vec![credential(1)]], &BTreeSet::new())
            .unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
