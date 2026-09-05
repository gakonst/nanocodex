use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use eyre::{Context, Result, bail, eyre};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

const CHECKSUM_FILE: &str = "nanocodex.sha256";
const NANOCODEX2_CHECKSUM_FILE: &str = "nanocodex2.sha256";
const VM_GUEST_BINARY_NAME: &str = "nanocodex-vm-guest";
const VM_GUEST_CHECKSUM_FILE: &str = "nanocodex-vm-guest.sha256";

#[cfg(windows)]
const BINARY_NAME: &str = "nanocodex.exe";
#[cfg(not(windows))]
const BINARY_NAME: &str = "nanocodex";

#[cfg(windows)]
const NANOCODEX2_BINARY_NAME: &str = "nanocodex2.exe";
#[cfg(not(windows))]
const NANOCODEX2_BINARY_NAME: &str = "nanocodex2";

pub(super) struct VersionStore {
    root: PathBuf,
}

impl VersionStore {
    pub(super) fn discover() -> Result<Self> {
        let root = if let Some(root) = std::env::var_os("NANOCODEX_DIR") {
            PathBuf::from(root)
        } else {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| eyre!("HOME is not set; set NANOCODEX_DIR explicitly"))?;
            PathBuf::from(home).join(".nanocodex")
        };
        if root.as_os_str().is_empty() {
            bail!("NANOCODEX_DIR cannot be empty");
        }
        Ok(Self { root })
    }

    #[cfg(test)]
    fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(super) fn prepare(&self, manager_version: &str) -> Result<()> {
        let executable = std::env::current_exe()
            .wrap_err("failed to locate the running Nanocodex executable")?;
        let contents = fs::read(&executable)
            .wrap_err_with(|| format!("failed to read {}", executable.display()))?;
        self.prepare_with_contents(manager_version, &contents)?;
        self.seed_running_updater_checksum(&executable, &contents)
    }

    fn prepare_with_contents(&self, manager_version: &str, contents: &[u8]) -> Result<()> {
        validate_key(manager_version)?;
        fs::create_dir_all(self.versions_dir())
            .wrap_err("failed to create the Nanocodex version store")?;
        fs::create_dir_all(self.root.join("updater"))
            .wrap_err("failed to create the Nanocodex updater directory")?;
        fs::create_dir_all(self.root.join("bin"))
            .wrap_err("failed to create the Nanocodex bin directory")?;

        let active = self.active()?;
        let updater_exists = self.updater_path().is_file();
        if (!updater_exists || active.is_none()) && !self.is_cached(manager_version)? {
            self.install(manager_version, contents)?;
        }
        if !updater_exists {
            atomic_write(&self.updater_path(), contents, true)?;
            self.write_updater_checksum(contents)?;
        }
        if active.is_none() {
            self.activate(manager_version)?;
        }

        #[cfg(unix)]
        self.install_launcher()?;

        Ok(())
    }

    pub(super) fn is_cached(&self, key: &str) -> Result<bool> {
        validate_key(key)?;
        file_matches_checksum(&self.binary_path(key), &self.checksum_path(key))
    }

    pub(super) fn install(&self, key: &str, contents: &[u8]) -> Result<()> {
        validate_key(key)?;
        let directory = self.version_dir(key);
        fs::create_dir_all(&directory)
            .wrap_err_with(|| format!("failed to create {}", directory.display()))?;
        atomic_write(&self.binary_path(key), contents, true)?;
        let checksum = hex::encode(Sha256::digest(contents));
        atomic_write(
            &self.checksum_path(key),
            format!("{checksum}\n").as_bytes(),
            false,
        )
    }

    pub(super) fn install_bundle(
        &self,
        key: &str,
        binary: &[u8],
        nanocodex2: &[u8],
        vm_guest: Option<&[u8]>,
    ) -> Result<()> {
        validate_key(key)?;
        fs::create_dir_all(self.versions_dir())
            .wrap_err("failed to create the Nanocodex version store")?;
        if self.is_cached_bundle(key, vm_guest.is_some())? {
            return Ok(());
        }

        let directory = self.version_dir(key);
        if directory.exists() {
            let installed_binary = fs::read(self.binary_path(key))
                .wrap_err_with(|| format!("failed to read Nanocodex version {key}"))?;
            if self.is_cached(key)? && Sha256::digest(&installed_binary) == Sha256::digest(binary) {
                self.write_companion_files(&directory, nanocodex2, vm_guest)?;
                return Ok(());
            }
            bail!(
                "cannot coherently replace incomplete Nanocodex version {}; remove {} and retry",
                key,
                directory.display()
            );
        }

        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(self.versions_dir())
            .wrap_err("failed to stage the Nanocodex version")?;
        atomic_write(&staging.path().join(BINARY_NAME), binary, true)?;
        atomic_write(
            &staging.path().join(CHECKSUM_FILE),
            format!("{}\n", hex::encode(Sha256::digest(binary))).as_bytes(),
            false,
        )?;
        self.write_companion_files(staging.path(), nanocodex2, vm_guest)?;
        fs::rename(staging.path(), &directory)
            .wrap_err_with(|| format!("failed to install {}", directory.display()))?;
        Ok(())
    }

    fn write_companion_files(
        &self,
        directory: &Path,
        nanocodex2: &[u8],
        vm_guest: Option<&[u8]>,
    ) -> Result<()> {
        atomic_write(&directory.join(NANOCODEX2_BINARY_NAME), nanocodex2, true)?;
        atomic_write(
            &directory.join(NANOCODEX2_CHECKSUM_FILE),
            format!("{}\n", hex::encode(Sha256::digest(nanocodex2))).as_bytes(),
            false,
        )?;
        if let Some(vm_guest) = vm_guest {
            atomic_write(&directory.join(VM_GUEST_BINARY_NAME), vm_guest, true)?;
            atomic_write(
                &directory.join(VM_GUEST_CHECKSUM_FILE),
                format!("{}\n", hex::encode(Sha256::digest(vm_guest))).as_bytes(),
                false,
            )?;
        }
        Ok(())
    }

    pub(super) fn is_cached_bundle(&self, key: &str, requires_vm_guest: bool) -> Result<bool> {
        Ok(self.is_cached(key)?
            && file_matches_checksum(
                &self.version_dir(key).join(NANOCODEX2_BINARY_NAME),
                &self.version_dir(key).join(NANOCODEX2_CHECKSUM_FILE),
            )?
            && (!requires_vm_guest
                || file_matches_checksum(
                    &self.version_dir(key).join(VM_GUEST_BINARY_NAME),
                    &self.version_dir(key).join(VM_GUEST_CHECKSUM_FILE),
                )?))
    }

    pub(super) fn activate(&self, key: &str) -> Result<()> {
        if !self.is_cached(key)? {
            bail!("Nanocodex version {key} is not installed or its checksum is invalid");
        }

        #[cfg(unix)]
        {
            self.activate_symlink(key)?;
            self.install_launcher()?;
            self.sync_nanocodex2_launcher(key)?;
        }

        #[cfg(not(unix))]
        {
            self_replace::self_replace(self.binary_path(key)).wrap_err(
                "failed to replace the running Nanocodex executable with the selected version",
            )?;
            atomic_write(
                &self.root.join("active-version"),
                format!("{key}\n").as_bytes(),
                false,
            )?;
        }

        Ok(())
    }

    pub(super) fn active(&self) -> Result<Option<String>> {
        #[cfg(unix)]
        {
            let target = match fs::read_link(self.root.join("current")) {
                Ok(target) => target,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(error).wrap_err("failed to read the active Nanocodex link");
                }
            };
            target
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| eyre!("the active Nanocodex link has an invalid target"))
                .map(Some)
        }

        #[cfg(not(unix))]
        {
            let path = self.root.join("active-version");
            match fs::read_to_string(&path) {
                Ok(key) => Ok(Some(key.trim().to_owned())),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => {
                    Err(error).wrap_err_with(|| format!("failed to read {}", path.display()))
                }
            }
        }
    }

    pub(super) fn promote_manager(&self, key: &str) -> Result<()> {
        if !self.is_cached(key)? {
            bail!("cannot promote missing Nanocodex version {key} to updater");
        }

        #[cfg(unix)]
        {
            let contents = fs::read(self.binary_path(key))
                .wrap_err_with(|| format!("failed to read Nanocodex version {key}"))?;
            atomic_write(&self.updater_path(), &contents, true)?;
            self.write_updater_checksum(&contents)?;
        }

        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn prepare_legacy_nightly_bootstrap() -> Result<bool> {
        let executable = std::env::current_exe()
            .wrap_err("failed to locate the running Nanocodex executable")?;
        let Some(store) = Self::legacy_nightly_store_for(&executable)? else {
            return Ok(false);
        };
        store.install_launcher()?;
        Ok(true)
    }

    #[cfg(not(unix))]
    pub(super) fn prepare_legacy_nightly_bootstrap() -> Result<bool> {
        Ok(false)
    }

    #[cfg(unix)]
    pub(super) fn promote_running_legacy_nightly_manager() -> Result<bool> {
        let executable = std::env::current_exe()
            .wrap_err("failed to locate the running Nanocodex executable")?;
        let Some(store) = Self::legacy_nightly_store_for(&executable)? else {
            return Ok(false);
        };
        store.promote_manager("nightly")?;
        Ok(true)
    }

    #[cfg(not(unix))]
    pub(super) fn promote_running_legacy_nightly_manager() -> Result<bool> {
        Ok(false)
    }

    #[cfg(unix)]
    fn legacy_nightly_store_for(executable: &Path) -> Result<Option<Self>> {
        let executable = executable
            .canonicalize()
            .wrap_err_with(|| format!("failed to resolve {}", executable.display()))?;
        let Some(version_directory) = executable.parent() else {
            return Ok(None);
        };
        let Some(versions_directory) = version_directory.parent() else {
            return Ok(None);
        };
        if versions_directory
            .file_name()
            .and_then(|name| name.to_str())
            != Some("versions")
        {
            return Ok(None);
        }
        let Some(root) = versions_directory.parent() else {
            return Ok(None);
        };
        let store = Self {
            root: root.to_path_buf(),
        };
        if store.active()?.as_deref() != Some("nightly") || store.updater_checksum_path().is_file()
        {
            return Ok(None);
        }
        let active_binary = match store.binary_path("nightly").canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).wrap_err("failed to resolve the active nightly Nanocodex");
            }
        };
        if executable != active_binary {
            return Ok(None);
        }

        Ok(Some(store))
    }

    fn write_updater_checksum(&self, contents: &[u8]) -> Result<()> {
        let checksum = hex::encode(Sha256::digest(contents));
        atomic_write(
            &self.updater_checksum_path(),
            format!("{checksum}\n").as_bytes(),
            false,
        )
    }

    fn seed_running_updater_checksum(&self, executable: &Path, contents: &[u8]) -> Result<()> {
        if self.updater_checksum_path().is_file() {
            return Ok(());
        }
        let executable = executable
            .canonicalize()
            .wrap_err_with(|| format!("failed to resolve {}", executable.display()))?;
        let updater = self
            .updater_path()
            .canonicalize()
            .wrap_err("failed to resolve the Nanocodex updater")?;
        if executable == updater {
            self.write_updater_checksum(contents)?;
        }
        Ok(())
    }

    fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    fn version_dir(&self, key: &str) -> PathBuf {
        self.versions_dir().join(key)
    }

    fn binary_path(&self, key: &str) -> PathBuf {
        self.version_dir(key).join(BINARY_NAME)
    }

    fn checksum_path(&self, key: &str) -> PathBuf {
        self.version_dir(key).join(CHECKSUM_FILE)
    }

    fn updater_path(&self) -> PathBuf {
        self.root.join("updater").join(BINARY_NAME)
    }

    fn updater_checksum_path(&self) -> PathBuf {
        self.root.join("updater").join(CHECKSUM_FILE)
    }

    #[cfg(unix)]
    fn activate_symlink(&self, key: &str) -> Result<()> {
        use std::os::unix::fs::symlink;

        let current = self.root.join("current");
        let temporary = self.root.join(format!(".current-{}", std::process::id()));
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .wrap_err_with(|| format!("failed to remove {}", temporary.display()));
            }
        }
        symlink(Path::new("versions").join(key), &temporary)
            .wrap_err("failed to create the active Nanocodex link")?;
        if let Err(error) = fs::rename(&temporary, &current) {
            let _ = fs::remove_file(&temporary);
            return Err(error).wrap_err("failed to activate the selected Nanocodex version");
        }
        Ok(())
    }

    #[cfg(unix)]
    fn install_launcher(&self) -> Result<()> {
        const LAUNCHER: &str = r#"#!/bin/sh
set -eu

case "$0" in
    */*) launcher=$0 ;;
    *) launcher=$(command -v "$0") ;;
esac
bin_dir=$(CDPATH= cd -- "$(dirname -- "$launcher")" && pwd -P)
install_root=$(dirname -- "$bin_dir")
export NANOCODEX_DIR="$install_root"

if [ "${1-}" = "update" ] && [ -f "$install_root/updater/nanocodex.sha256" ]; then
    exec "$install_root/updater/nanocodex" "$@"
fi
exec "$install_root/current/nanocodex" "$@"
"#;

        let path = self.root.join("bin").join(BINARY_NAME);
        if fs::read(&path).is_ok_and(|contents| contents == LAUNCHER.as_bytes()) {
            return Ok(());
        }
        atomic_write(&path, LAUNCHER.as_bytes(), true)
    }

    #[cfg(unix)]
    fn sync_nanocodex2_launcher(&self, key: &str) -> Result<()> {
        const LAUNCHER: &str = r#"#!/bin/sh
set -eu

case "$0" in
    */*) launcher=$0 ;;
    *) launcher=$(command -v "$0") ;;
esac
bin_dir=$(CDPATH= cd -- "$(dirname -- "$launcher")" && pwd -P)
install_root=$(dirname -- "$bin_dir")
export NANOCODEX_DIR="$install_root"
exec "$install_root/current/nanocodex2" "$@"
"#;

        let path = self.root.join("bin").join(NANOCODEX2_BINARY_NAME);
        if file_matches_checksum(
            &self.version_dir(key).join(NANOCODEX2_BINARY_NAME),
            &self.version_dir(key).join(NANOCODEX2_CHECKSUM_FILE),
        )? {
            return atomic_write(&path, LAUNCHER.as_bytes(), true);
        }
        match fs::read(&path) {
            Ok(contents) if contents == LAUNCHER.as_bytes() => fs::remove_file(&path)
                .wrap_err_with(|| format!("failed to remove {}", path.display())),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).wrap_err_with(|| format!("failed to read {}", path.display())),
        }
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.starts_with('.')
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        bail!("invalid Nanocodex version key {key:?}");
    }
    Ok(())
}

fn file_matches_checksum(path: &Path, checksum_path: &Path) -> Result<bool> {
    if !path.is_file() || !checksum_path.is_file() {
        return Ok(false);
    }
    let expected = fs::read_to_string(checksum_path)
        .wrap_err_with(|| format!("failed to read {}", checksum_path.display()))?;
    let expected = expected.trim();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(false);
    }
    let contents =
        fs::read(path).wrap_err_with(|| format!("failed to read cached {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(contents)) == expected.to_ascii_lowercase())
}

fn atomic_write(path: &Path, contents: &[u8], executable: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    let mut temporary =
        NamedTempFile::new_in(parent).wrap_err("failed to create a temporary install file")?;
    temporary
        .write_all(contents)
        .wrap_err_with(|| format!("failed to write {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .wrap_err_with(|| format!("failed to sync {}", path.display()))?;

    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;

        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))
            .wrap_err_with(|| format!("failed to make {} executable", path.display()))?;
    }

    #[cfg(not(unix))]
    let _ = executable;

    temporary
        .persist(path)
        .map_err(|error| error.error)
        .wrap_err_with(|| format!("failed to install {}", path.display()))?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn retains_versions_and_switches_the_active_link() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.prepare_with_contents("0.3.0", b"current").unwrap();

        assert_eq!(store.active().unwrap().as_deref(), Some("0.3.0"));
        assert_eq!(fs::read(store.binary_path("0.3.0")).unwrap(), b"current");
        assert_eq!(fs::read(store.updater_path()).unwrap(), b"current");
        assert!(
            file_matches_checksum(&store.updater_path(), &store.updater_checksum_path()).unwrap()
        );
        let launcher = fs::read_to_string(directory.path().join("bin/nanocodex")).unwrap();
        assert!(launcher.contains("updater/nanocodex"));
        assert!(launcher.contains("export NANOCODEX_DIR"));

        store.install("0.2.0", b"previous").unwrap();
        store.activate("0.2.0").unwrap();

        assert_eq!(store.active().unwrap().as_deref(), Some("0.2.0"));
        assert_eq!(fs::read(store.binary_path("0.2.0")).unwrap(), b"previous");
        assert_eq!(fs::read(store.binary_path("0.3.0")).unwrap(), b"current");
    }

    #[test]
    fn active_nightly_bootstraps_a_legacy_updater_without_copying_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.prepare_with_contents("0.3.0", b"legacy").unwrap();
        store.install("nightly", b"nightly").unwrap();
        store.activate("nightly").unwrap();
        fs::remove_file(store.updater_checksum_path()).unwrap();

        assert!(
            VersionStore::legacy_nightly_store_for(&store.binary_path("nightly"))
                .unwrap()
                .is_some()
        );
        store.install_launcher().unwrap();
        assert_eq!(fs::read(store.updater_path()).unwrap(), b"legacy");
        let launcher = fs::read_to_string(directory.path().join("bin/nanocodex")).unwrap();
        assert!(launcher.contains("updater/nanocodex.sha256"));
        assert!(launcher.contains("updater/nanocodex"));
        assert!(launcher.contains("current/nanocodex"));

        VersionStore::legacy_nightly_store_for(&store.binary_path("nightly"))
            .unwrap()
            .unwrap()
            .promote_manager("nightly")
            .unwrap();
        assert_eq!(fs::read(store.updater_path()).unwrap(), b"nightly");
        assert!(store.updater_checksum_path().is_file());

        store.install("local-build", b"local").unwrap();
        store.activate("local-build").unwrap();
        assert_eq!(fs::read(store.updater_path()).unwrap(), b"nightly");
        assert!(
            file_matches_checksum(&store.updater_path(), &store.updater_checksum_path()).unwrap()
        );
    }

    #[test]
    fn running_legacy_updater_seeds_its_checksum_marker() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.prepare_with_contents("0.3.0", b"legacy").unwrap();
        fs::remove_file(store.updater_checksum_path()).unwrap();

        store
            .seed_running_updater_checksum(&store.updater_path(), b"legacy")
            .unwrap();

        assert!(
            file_matches_checksum(&store.updater_path(), &store.updater_checksum_path()).unwrap()
        );
    }

    #[test]
    fn refuses_corrupted_cached_versions() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.install("0.2.0", b"original").unwrap();
        assert!(store.is_cached("0.2.0").unwrap());

        fs::write(store.binary_path("0.2.0"), b"corrupted").unwrap();

        assert!(!store.is_cached("0.2.0").unwrap());
        assert!(store.activate("0.2.0").is_err());
    }

    #[test]
    fn installs_both_binaries_and_optional_vm_guest_as_one_activatable_directory() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());

        store
            .install_bundle("nightly-build", b"cli", b"managed-cli", Some(b"guest"))
            .unwrap();
        store.activate("nightly-build").unwrap();

        assert!(store.is_cached_bundle("nightly-build", true).unwrap());
        assert_eq!(
            fs::read(directory.path().join("current/nanocodex2")).unwrap(),
            b"managed-cli"
        );
        let companion_launcher =
            fs::read_to_string(directory.path().join("bin/nanocodex2")).unwrap();
        assert!(companion_launcher.contains("current/nanocodex2"));
        assert_eq!(
            fs::read(directory.path().join("current/nanocodex-vm-guest")).unwrap(),
            b"guest"
        );

        fs::write(
            store
                .version_dir("nightly-build")
                .join(NANOCODEX2_BINARY_NAME),
            b"corrupted",
        )
        .unwrap();
        assert!(!store.is_cached_bundle("nightly-build", true).unwrap());
        fs::write(
            store
                .version_dir("nightly-build")
                .join(NANOCODEX2_BINARY_NAME),
            b"managed-cli",
        )
        .unwrap();

        fs::write(
            store
                .version_dir("nightly-build")
                .join(VM_GUEST_BINARY_NAME),
            b"corrupted",
        )
        .unwrap();
        assert!(!store.is_cached_bundle("nightly-build", true).unwrap());

        store.install("stable", b"stable").unwrap();
        store.activate("stable").unwrap();
        assert!(!directory.path().join("bin/nanocodex2").exists());
    }

    #[test]
    fn installs_stable_binaries_as_one_verified_activatable_directory() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());

        store
            .install_bundle("0.5.0", b"stable-cli", b"stable-managed-cli", None)
            .unwrap();
        store.activate("0.5.0").unwrap();

        assert!(store.is_cached_bundle("0.5.0", false).unwrap());
        assert_eq!(
            fs::read(directory.path().join("current/nanocodex")).unwrap(),
            b"stable-cli"
        );
        assert_eq!(
            fs::read(directory.path().join("current/nanocodex2")).unwrap(),
            b"stable-managed-cli"
        );
        assert!(directory.path().join("bin/nanocodex2").is_file());
        assert!(!directory.path().join("current/nanocodex-vm-guest").exists());

        fs::write(
            store.version_dir("0.5.0").join(NANOCODEX2_BINARY_NAME),
            b"corrupted",
        )
        .unwrap();
        assert!(!store.is_cached_bundle("0.5.0", false).unwrap());
    }

    #[test]
    fn completes_a_verified_legacy_stable_install_before_exposing_nanocodex2() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.install("0.4.0", b"stable-cli").unwrap();
        store.activate("0.4.0").unwrap();
        assert!(!directory.path().join("bin/nanocodex2").exists());

        store
            .install_bundle("0.4.0", b"stable-cli", b"stable-managed-cli", None)
            .unwrap();
        assert!(store.is_cached_bundle("0.4.0", false).unwrap());
        assert!(!directory.path().join("bin/nanocodex2").exists());

        store.activate("0.4.0").unwrap();
        assert!(directory.path().join("bin/nanocodex2").is_file());
        assert_eq!(
            fs::read(directory.path().join("current/nanocodex2")).unwrap(),
            b"stable-managed-cli"
        );
    }
}
