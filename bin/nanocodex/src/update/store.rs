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
/// Hands keyed by their deterministic identity, shared by every CLI version.
const HAND_VERSIONS_DIR: &str = "hand-versions";
/// The identity of the Hand a version links (absent for older bundles).
const HAND_IDENTITY_FILE: &str = "hand-identity";
/// Present in every CLI that contains both command trees and selects one from
/// argv[0]: `nanocodex`/`nc`/`nanocodex2` managed, `ncl` (or `--local`) local.
/// Older CLIs lack it; their managed tree was the separate nanocodex2 binary.
/// This is a capability hint for entrypoint links, not an integrity check.
pub(crate) const UNIFIED_CLI_MARKER: &[u8] = b"NANOCODEX_UNIFIED_CLI_V1";

fn is_unified_cli(contents: &[u8]) -> bool {
    crate::launcher::contains_marker(contents, UNIFIED_CLI_MARKER)
}

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
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Serialize staging, service handover, and CLI activation across processes.
    pub(super) fn update_lock(&self) -> Result<fs::File> {
        fs::create_dir_all(&self.root)?;
        let path = self.root.join("update.lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            bail!("update lock must be a regular file");
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&file)
            .wrap_err("another Nanocodex update is already running")?;
        Ok(file)
    }

    pub(super) fn pending(&self) -> Result<Option<String>> {
        match fs::read_to_string(self.root.join("pending-update")) {
            Ok(key) => {
                let key = key.trim();
                validate_key(key)?;
                Ok(Some(key.to_owned()))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn stage_pending(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        if !self.is_cached_bundle(key, false)? {
            bail!("cannot stage an incomplete update bundle");
        }
        atomic_write(
            &self.root.join("pending-update"),
            format!("{key}\n").as_bytes(),
            false,
        )
    }

    pub(super) fn clear_pending(&self) -> Result<()> {
        match fs::remove_file(self.root.join("pending-update")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Record an explicit local/source selection that background updates must preserve.
    pub(super) fn record_explicit_selection(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        fs::create_dir_all(&self.root)?;
        atomic_write(
            &self.root.join("explicit-selection"),
            format!("{key}\n").as_bytes(),
            false,
        )
    }

    /// The explicit selection while it is still active or staged for activation.
    pub(super) fn held_explicit_selection(&self) -> Result<Option<String>> {
        let key = match fs::read_to_string(self.root.join("explicit-selection")) {
            Ok(key) => key.trim().to_owned(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        validate_key(&key)?;
        // A newer explicit pending choice supersedes the active selection.
        let selected = self.pending()?.or(self.active()?);
        Ok((selected.as_deref() == Some(key.as_str())).then_some(key))
    }

    pub(super) fn clear_explicit_selection(&self) -> Result<()> {
        match fs::remove_file(self.root.join("explicit-selection")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn discover() -> Result<Self> {
        std::hint::black_box(UNIFIED_CLI_MARKER);
        let root = if let Some(root) = std::env::var_os("NANOCODEX_DIR") {
            PathBuf::from(root)
        } else if let Some(root) = crate::launcher::running_install_root() {
            root
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

    #[cfg(unix)]
    pub(super) fn at_root(root: PathBuf) -> Self {
        Self { root }
    }

    #[cfg(test)]
    pub(super) fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(super) fn prepare(&self, manager_version: &str) -> Result<()> {
        let executable = std::env::current_exe()
            .wrap_err("failed to locate the running Nanocodex executable")?;
        let contents = fs::read(&executable)
            .wrap_err_with(|| format!("failed to read {}", executable.display()))?;
        #[cfg(windows)]
        self.retain_windows_baseline(manager_version, &executable, &contents)?;
        self.prepare_with_contents(manager_version, &contents)?;
        self.seed_running_updater_checksum(&executable, &contents)
    }

    #[cfg(windows)]
    fn retain_windows_baseline(
        &self,
        manager_version: &str,
        executable: &Path,
        contents: &[u8],
    ) -> Result<()> {
        let active = self.active()?;
        let key = active.as_deref().unwrap_or(manager_version);
        if self.is_cached_bundle(key, false)? {
            return Ok(());
        }
        let cli = if active.is_some() {
            if !self.is_cached(key)? {
                bail!("The previous Windows CLI failed verification; refusing update preparation");
            }
            fs::read(self.binary_path(key))?
        } else {
            contents.to_vec()
        };
        // The installer and older updater bootstrap cached only the CLI. Freeze
        // and probe the exact bytes we will retain before any service handover.
        // The updater directory has no companion, so also inspect stable bin.
        let frozen = tempfile::tempdir()?;
        let cli_path = frozen.path().join(BINARY_NAME);
        let hand_path = frozen.path().join(NANOCODEX2_BINARY_NAME);
        atomic_write(&cli_path, &cli, true)?;
        let mut failure = None;
        for candidate in [
            self.version_dir(key).join(NANOCODEX2_BINARY_NAME),
            executable.with_file_name(NANOCODEX2_BINARY_NAME),
            self.root.join("bin").join(NANOCODEX2_BINARY_NAME),
        ] {
            if !candidate.is_file() {
                continue;
            }
            let hand = fs::read(&candidate)?;
            atomic_write(&hand_path, &hand, true)?;
            // prepare is synchronous and is called inside the updater's Tokio
            // runtime. Reuse the bounded real-pair probes on a separate thread.
            let verified = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()?
                            .block_on(super::local::verify_pair(&cli_path, &hand_path))
                    })
                    .join()
                    .map_err(|_| eyre!("Windows rollback-pair verification panicked"))?
            });
            match verified {
                Ok(identity) => {
                    return self.install_bundle_with_hand(
                        key,
                        &cli,
                        &hand,
                        identity.as_deref(),
                        None,
                        None,
                    );
                }
                Err(error) => failure = Some(error),
            }
        }
        Err(failure.unwrap_or_else(|| eyre!("No previous Windows Hand companion was found")))
            .wrap_err("Cannot retain a verified previous Windows CLI/Hand pair; repair the matching installation before updating. No service handover was attempted")
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

    #[cfg(test)]
    pub(super) fn install_bundle(
        &self,
        key: &str,
        binary: &[u8],
        nanocodex2: &[u8],
        vm_guest: Option<&[u8]>,
        voice: Option<&[u8]>,
    ) -> Result<()> {
        self.install_bundle_with_hand(key, binary, nanocodex2, None, vm_guest, voice)
    }

    /// Install a CLI with its Hand. A Hand that reports an identity is stored
    /// once under `hand-versions/<identity>` and linked from the version, so
    /// every CLI version with an unchanged Hand runs the same Hand file.
    pub(super) fn install_bundle_with_hand(
        &self,
        key: &str,
        binary: &[u8],
        nanocodex2: &[u8],
        hand_identity: Option<&str>,
        vm_guest: Option<&[u8]>,
        voice: Option<&[u8]>,
    ) -> Result<()> {
        validate_key(key)?;
        fs::create_dir_all(self.versions_dir())
            .wrap_err("failed to create the Nanocodex version store")?;
        let voice_cached = match voice {
            Some(bytes) => self.is_cached_voice(key, Some(&hex::encode(Sha256::digest(bytes))))?,
            None => true,
        };
        if self.is_cached_bundle(key, vm_guest.is_some())? && voice_cached {
            return Ok(());
        }

        let directory = self.version_dir(key);
        if directory.exists() {
            let installed_binary = fs::read(self.binary_path(key))
                .wrap_err_with(|| format!("failed to read Nanocodex version {key}"))?;
            if self.is_cached(key)? && Sha256::digest(&installed_binary) == Sha256::digest(binary) {
                if let Some(voice) = voice {
                    super::voice::install(&directory, voice)?;
                }
                self.write_companion_files(&directory, nanocodex2, hand_identity, vm_guest)?;
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
        if let Some(voice) = voice {
            super::voice::install(staging.path(), voice)?;
        }
        atomic_write(&staging.path().join(BINARY_NAME), binary, true)?;
        atomic_write(
            &staging.path().join(CHECKSUM_FILE),
            format!("{}\n", hex::encode(Sha256::digest(binary))).as_bytes(),
            false,
        )?;
        self.write_companion_files(staging.path(), nanocodex2, hand_identity, vm_guest)?;
        fs::rename(staging.path(), &directory)
            .wrap_err_with(|| format!("failed to install {}", directory.display()))?;
        Ok(())
    }

    fn write_companion_files(
        &self,
        directory: &Path,
        nanocodex2: &[u8],
        hand_identity: Option<&str>,
        vm_guest: Option<&[u8]>,
    ) -> Result<()> {
        let hand = directory.join(NANOCODEX2_BINARY_NAME);
        let identity_file = directory.join(HAND_IDENTITY_FILE);
        let stored = match hand_identity {
            #[cfg(unix)]
            Some(identity) => {
                let stored = self.store_hand(identity, nanocodex2)?;
                // versions/<key>/nanocodex2 -> ../../hand-versions/<identity>/nanocodex2
                atomic_symlink(
                    &hand,
                    &Path::new("../..")
                        .join(HAND_VERSIONS_DIR)
                        .join(identity)
                        .join(NANOCODEX2_BINARY_NAME),
                )?;
                atomic_write(&identity_file, format!("{identity}\n").as_bytes(), false)?;
                stored
            }
            _ => {
                // Windows copies the Hand into each version (no links); the
                // identity still records that the Hand is unchanged.
                atomic_write(&hand, nanocodex2, true)?;
                match hand_identity {
                    Some(identity) => {
                        atomic_write(&identity_file, format!("{identity}\n").as_bytes(), false)?;
                    }
                    None => remove_if_present(&identity_file)?,
                }
                nanocodex2.to_vec()
            }
        };
        atomic_write(
            &directory.join(NANOCODEX2_CHECKSUM_FILE),
            format!("{}\n", hex::encode(Sha256::digest(&stored))).as_bytes(),
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

    /// The Hand identity recorded for an installed version.
    pub(super) fn hand_identity_of(&self, key: &str) -> Option<String> {
        let recorded = fs::read_to_string(self.version_dir(key).join(HAND_IDENTITY_FILE)).ok()?;
        super::local::hand_identity(&format!("Hand Identity: {}", recorded.trim()))
    }

    /// Store a Hand under its identity. The first verified bytes stored for an
    /// identity are kept, so an unchanged Hand keeps one executable path (and
    /// on macOS one code signature) across CLI versions. Returns those bytes.
    #[cfg(unix)]
    fn store_hand(&self, identity: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        if super::local::hand_identity(&format!("Hand Identity: {identity}")).is_none() {
            bail!("invalid Hand identity {identity}");
        }
        let hands = self.root.join(HAND_VERSIONS_DIR);
        let directory = hands.join(identity);
        let path = directory.join(NANOCODEX2_BINARY_NAME);
        if file_matches_checksum(&path, &directory.join(NANOCODEX2_CHECKSUM_FILE))? {
            return fs::read(&path).wrap_err_with(|| format!("failed to read {}", path.display()));
        }
        fs::create_dir_all(&hands).wrap_err("failed to create the Nanocodex Hand store")?;
        let staging = tempfile::Builder::new()
            .prefix(".hand-")
            .tempdir_in(&hands)
            .wrap_err("failed to stage the Nanocodex Hand")?;
        atomic_write(&staging.path().join(NANOCODEX2_BINARY_NAME), bytes, true)?;
        atomic_write(
            &staging.path().join(NANOCODEX2_CHECKSUM_FILE),
            format!("{}\n", hex::encode(Sha256::digest(bytes))).as_bytes(),
            false,
        )?;
        if directory.exists() {
            // Incomplete or corrupt: set it aside (a running Hand keeps its
            // open file) instead of deleting it.
            let aside = hands.join(format!(".corrupt-{identity}-{}", std::process::id()));
            fs::rename(&directory, &aside)
                .wrap_err_with(|| format!("failed to set aside {}", directory.display()))?;
        }
        let staged = staging.keep();
        fs::rename(&staged, &directory)
            .wrap_err_with(|| format!("failed to install {}", directory.display()))?;
        Ok(bytes.to_vec())
    }

    /// The Hand a version runs: its linked `Nanocodex.app` when present,
    /// otherwise the standalone `nanocodex2`. Resolved to the canonical
    /// `hand-versions/<identity>/...` file, so service records name the
    /// Hand's own stable path rather than a CLI version's link.
    pub(super) fn hand_executable(&self, key: &str) -> PathBuf {
        let linked = if self.links_hand_app(key) {
            self.version_dir(key).join(super::app::EXECUTABLE)
        } else {
            self.version_dir(key).join(NANOCODEX2_BINARY_NAME)
        };
        fs::canonicalize(&linked).unwrap_or(linked)
    }

    fn links_hand_app(&self, key: &str) -> bool {
        fs::symlink_metadata(self.version_dir(key).join(super::app::BUNDLE)).is_ok()
    }

    /// Whether a version links the complete, receipt-verified bundle stored
    /// for its Hand identity.
    pub(super) fn has_hand_app(&self, key: &str) -> Result<bool> {
        validate_key(key)?;
        let Some(identity) = self.hand_identity_of(key) else {
            return Ok(false);
        };
        let link = self.version_dir(key).join(super::app::BUNDLE);
        if fs::read_link(&link).ok() != Some(hand_app_link(&identity)) {
            return Ok(false);
        }
        super::app::cached(&self.root.join(HAND_VERSIONS_DIR).join(identity))
    }

    /// Store a released `Nanocodex.app` archive for this version's Hand
    /// identity and link the version to it. The caller has verified the
    /// archive checksum and that its Hand reports the same identity.
    #[cfg(unix)]
    pub(super) fn install_hand_app(&self, key: &str, archive: &[u8]) -> Result<()> {
        self.store_hand_app(key, |parent| super::app::extract(archive, parent))
    }

    /// Wrap this version's development Hand into a locally signed
    /// `Nanocodex.app` and link the version to it.
    #[cfg(unix)]
    pub(super) fn wrap_hand_app(&self, key: &str, version: &str) -> Result<()> {
        let hand = fs::read(self.version_dir(key).join(NANOCODEX2_BINARY_NAME))
            .wrap_err_with(|| format!("failed to read the Hand of Nanocodex version {key}"))?;
        self.store_hand_app(key, |parent| super::app::wrap(&hand, version, parent))
    }

    /// The first complete bundle stored for an identity is kept, exactly like
    /// `store_hand`: an unchanged Hand keeps one path and one signature, so
    /// installing it again never re-signs or moves the running Hand.
    #[cfg(unix)]
    fn store_hand_app(&self, key: &str, build: impl FnOnce(&Path) -> Result<String>) -> Result<()> {
        validate_key(key)?;
        let identity = self.hand_identity_of(key).ok_or_else(|| {
            eyre!("Nanocodex version {key} has no Hand identity; its Hand cannot be bundled")
        })?;
        let directory = self.root.join(HAND_VERSIONS_DIR).join(&identity);
        if !super::app::cached(&directory)? {
            fs::create_dir_all(&directory)
                .wrap_err_with(|| format!("failed to create {}", directory.display()))?;
            let staging = tempfile::Builder::new()
                .prefix(".app-")
                .tempdir_in(&directory)
                .wrap_err("failed to stage Nanocodex.app")?;
            let receipt = build(staging.path())?;
            super::app::verify_signature(&staging.path().join(super::app::BUNDLE))?;
            let bundle = directory.join(super::app::BUNDLE);
            remove_if_present(&directory.join(super::app::RECEIPT))?;
            if fs::symlink_metadata(&bundle).is_ok() {
                // Incomplete or corrupt: set it aside (a running Hand keeps
                // its open file) instead of deleting it.
                let aside = tempfile::Builder::new()
                    .prefix(".corrupt-app-")
                    .tempdir_in(&directory)?
                    .keep();
                fs::rename(&bundle, aside.join(super::app::BUNDLE))
                    .wrap_err_with(|| format!("failed to set aside {}", bundle.display()))?;
            }
            fs::rename(staging.path().join(super::app::BUNDLE), &bundle)
                .wrap_err_with(|| format!("failed to install {}", bundle.display()))?;
            // The receipt is written last; without it the bundle is incomplete.
            atomic_write(
                &directory.join(super::app::RECEIPT),
                receipt.as_bytes(),
                false,
            )?;
        }
        atomic_symlink(
            &self.version_dir(key).join(super::app::BUNDLE),
            &hand_app_link(&identity),
        )
    }

    pub(super) fn voice_repair_directory(
        &self,
        key: &str,
        executable: &Path,
    ) -> Result<Option<PathBuf>> {
        validate_key(key)?;
        let installed = self.binary_path(key);
        if !installed.is_file()
            || executable.canonicalize()? != installed.canonicalize()?
            || self.is_cached_voice(key, None)?
        {
            return Ok(None);
        }
        Ok(Some(self.version_dir(key)))
    }

    pub(super) fn is_cached_voice(
        &self,
        key: &str,
        expected_archive: Option<&str>,
    ) -> Result<bool> {
        validate_key(key)?;
        super::voice::cached(&self.version_dir(key), expected_archive)
    }

    pub(super) fn is_cached_bundle(&self, key: &str, requires_vm_guest: bool) -> Result<bool> {
        Ok(self.is_cached(key)?
            && (!self.links_hand_app(key) || self.has_hand_app(key)?)
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

    pub(super) fn validate_activation(&self, key: &str) -> Result<()> {
        if !self.is_cached(key)? {
            bail!("Nanocodex version {key} is not installed or its checksum is invalid");
        }
        if self.links_hand_app(key) && !self.has_hand_app(key)? {
            bail!("Nanocodex version {key} links an incomplete or corrupt Nanocodex.app");
        }
        if self
            .version_dir(key)
            .join("nanocodex-voice.sha256")
            .exists()
            && !self.is_cached_voice(key, None)?
        {
            bail!("Nanocodex version {key} has an incomplete or corrupt voice runtime");
        }

        // --apply can consume a bundle staged by an older updater without
        // prepare. Establish its previous-pair rollback invariant here too,
        // before the coordinator reaches any service handover.
        #[cfg(windows)]
        if let Some(previous) = self.active()?
            && !self.is_cached_bundle(&previous, false)?
        {
            self.retain_windows_baseline(&previous, &std::env::current_exe()?, &[])?;
        }

        Ok(())
    }

    /// Windows has no symlink launcher. Keep stable PATH entrypoints beside the
    /// version store while a running executable is replaced in place.
    pub(super) fn sync_windows_entrypoints(&self, key: &str) -> Result<()> {
        if !cfg!(windows) {
            return Ok(());
        }
        // A CLI-only version leaves the stable Hand copy alone; a present but
        // corrupt Hand still refuses publication.
        let selected = self.version_dir(key);
        let has_hand = selected.join(NANOCODEX2_BINARY_NAME).exists();
        if !self.is_cached(key)? || (has_hand && !self.is_cached_bundle(key, false)?) {
            bail!("cannot publish an incomplete Windows Nanocodex bundle");
        }
        let bin = self.root.join("bin");
        let running = std::env::current_exe()?.canonicalize()?;
        let entrypoint = bin.join(BINARY_NAME);
        let cli = fs::read(selected.join(BINARY_NAME))?;
        if entrypoint.canonicalize().ok().as_deref() != Some(running.as_path()) {
            atomic_write(&entrypoint, &cli, true)?;
        }
        // Windows has no argv[0] links. A unified CLI selects its local tree
        // with a leading --local; older CLIs kept the managed tree in the
        // separate nanocodex2.exe.
        let shims: [(&str, &str); 2] = if is_unified_cli(&cli) {
            [
                ("nc.cmd", "@\"%~dp0nanocodex.exe\" %*\r\n"),
                ("ncl.cmd", "@\"%~dp0nanocodex.exe\" --local %*\r\n"),
            ]
        } else {
            [
                ("nc.cmd", "@\"%~dp0nanocodex2.exe\" %*\r\n"),
                ("ncl.cmd", "@\"%~dp0nanocodex.exe\" %*\r\n"),
            ]
        };
        for (name, contents) in shims {
            let path = bin.join(name);
            if fs::read(&path).ok().as_deref() != Some(contents.as_bytes()) {
                atomic_write(&path, contents.as_bytes(), false)?;
            }
        }
        if !has_hand {
            return Ok(());
        }
        let companion = fs::read(selected.join(NANOCODEX2_BINARY_NAME))?;
        let stable_companion = bin.join(NANOCODEX2_BINARY_NAME);
        atomic_write(&stable_companion, &companion, true)?;

        // The signed Inno installer puts both commands beside each other on
        // PATH. Keep that companion in lockstep too; raw bootstrap executables
        // have no sibling and therefore only publish into the stable bin dir.
        let running_companion = running.with_file_name(NANOCODEX2_BINARY_NAME);
        if running_companion != stable_companion
            && fs::symlink_metadata(&running_companion).is_ok_and(|metadata| metadata.is_file())
        {
            atomic_write(&running_companion, &companion, true)?;
        }
        Ok(())
    }

    pub(super) fn activate(&self, key: &str) -> Result<()> {
        self.validate_activation(key)?;
        #[cfg(unix)]
        {
            self.activate_symlink(key)?;
            self.install_launcher()?;
            self.sync_nanocodex2_launcher(key)?;
            self.sync_hand_aliases(key)?;
            self.remove_retired_computer_launcher()?;
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

    /// Apply this version's entrypoint rules after an older updater activated
    /// it. Pre-unified updaters link `bin/nanocodex2` to `../current/nanocodex2`
    /// (in a unified version, the Hand daemon) and create no `nc`, `ncl`,
    /// `nanocodex-hand` or `nc-hand`; this version's own activation never
    /// links that name to the Hand. Only the running, active CLI repairs its
    /// own installation, once: afterwards the trigger no longer matches, so a
    /// normal start costs one readlink. Hand files, `current` and service
    /// records are untouched. Returns whether the entrypoints were repaired.
    #[cfg(unix)]
    pub(super) fn repair_legacy_activation(&self) -> Result<bool> {
        let entrypoint = self.root.join("bin").join(NANOCODEX2_BINARY_NAME);
        let legacy = Path::new("../current").join(NANOCODEX2_BINARY_NAME);
        let stale = || fs::read_link(&entrypoint).is_ok_and(|target| target == legacy);
        if !stale() {
            return Ok(false);
        }
        let Some(key) = self.active()? else {
            return Ok(false);
        };
        let running = std::env::current_exe()?.canonicalize()?;
        if self.binary_path(&key).canonicalize().ok() != Some(running) {
            return Ok(false);
        }
        // A concurrent update owns the entrypoints; a later start repairs them.
        let Ok(_lock) = self.update_lock() else {
            return Ok(false);
        };
        if !stale() || self.active()?.as_deref() != Some(key.as_str()) {
            return Ok(false);
        }
        self.install_launcher()?;
        self.sync_hand_aliases(&key)?;
        // Last: this clears the trigger, so an earlier failure retries next start.
        self.sync_nanocodex2_launcher(&key)?;
        Ok(true)
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

    pub(super) fn promote_running_manager(&self) -> Result<()> {
        let contents = fs::read(std::env::current_exe()?)?;
        self.publish_updater(&contents)
    }

    /// Publish `contents` as the updater. A repeated update that selects the
    /// same manager keeps the existing file (its inode, mtime and receipt)
    /// instead of rewriting the whole executable with identical bytes.
    fn publish_updater(&self, contents: &[u8]) -> Result<()> {
        let checksum = format!("{}\n", hex::encode(Sha256::digest(contents)));
        let path = self.updater_path();
        let executable = |metadata: &fs::Metadata| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 == 0o111
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                true
            }
        };
        let unchanged = fs::read_to_string(self.updater_checksum_path())
            .is_ok_and(|receipt| receipt == checksum)
            && fs::symlink_metadata(&path).is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata.len() == contents.len() as u64
                    && executable(&metadata)
            })
            && fs::read(&path).is_ok_and(|existing| existing == contents);
        if unchanged {
            return Ok(());
        }
        atomic_write(&path, contents, true)?;
        atomic_write(&self.updater_checksum_path(), checksum.as_bytes(), false)
    }

    pub(super) fn promote_manager(&self, key: &str) -> Result<()> {
        if !self.is_cached(key)? {
            bail!("cannot promote missing Nanocodex version {key} to updater");
        }

        #[cfg(unix)]
        {
            let contents = fs::read(self.binary_path(key))
                .wrap_err_with(|| format!("failed to read Nanocodex version {key}"))?;
            self.publish_updater(&contents)?;
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

    pub(super) fn version_dir(&self, key: &str) -> PathBuf {
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
        let path = self.root.join("bin").join(BINARY_NAME);
        let (native, unified) = match fs::read(self.root.join("current").join(BINARY_NAME)) {
            Ok(contents) if crate::launcher::supports_native_launcher(&contents) => {
                (true, is_unified_cli(&contents))
            }
            _ => (false, false),
        };
        self.sync_short_aliases(native, unified)?;
        if native {
            return atomic_symlink(&path, &Path::new("../current").join(BINARY_NAME));
        }
        const LAUNCHER: &str = r#"#!/bin/sh
set -eu

case "$0" in
    */*) launcher=$0 ;;
    *) launcher=$(command -v "$0") ;;
esac
case "$launcher" in
    */*) launcher_dir=${launcher%/*} ;;
    *) launcher_dir=. ;;
esac
bin_dir=$(CDPATH= cd -- "${launcher_dir:-/}" && pwd -P)
install_root=${bin_dir%/*}
install_root=${install_root:-/}
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

    /// Link `nc`/`ncl` only to binaries that discover their installation
    /// natively; a shell wrapper would replace the argv[0] that selects the
    /// tree. A unified CLI serves both (`ncl` selects its local tree). For an
    /// older pair, `nc` is its managed nanocodex2 and `ncl` its local CLI.
    /// Remove only our own links when the selected binaries cannot serve them.
    #[cfg(unix)]
    fn sync_short_aliases(&self, cli_native: bool, unified: bool) -> Result<()> {
        let managed = if unified {
            Some(BINARY_NAME)
        } else {
            fs::read(self.root.join("current").join(NANOCODEX2_BINARY_NAME))
                .is_ok_and(|contents| crate::launcher::supports_native_launcher(&contents))
                .then_some(NANOCODEX2_BINARY_NAME)
        };
        let local = cli_native.then_some(BINARY_NAME);
        for (alias, executable) in [("nc", managed), ("ncl", local)] {
            let path = self.root.join("bin").join(alias);
            if let Some(executable) = executable {
                atomic_symlink(&path, &Path::new("../current").join(executable))?;
            } else {
                self.remove_own_current_link(&path)?;
            }
        }
        Ok(())
    }

    /// Link `nanocodex-hand`/`nc-hand` to the selected Hand when it serves the
    /// `hand` command under those names. macOS runs the signed bundle Hand so
    /// its privacy grants apply. Otherwise remove only our own links: an older
    /// Hand (or a CLI-only version) would run the wrong command under them.
    #[cfg(unix)]
    fn sync_hand_aliases(&self, key: &str) -> Result<()> {
        use crate::hand_executable::{HAND_COMMAND_ALIASES, HAND_COMMAND_ALIASES_MARKER};
        const APP_HAND: &str = "Nanocodex.app/Contents/MacOS/nanocodex2";
        let selected = self.version_dir(key);
        let serves_aliases = |contents: &[u8]| {
            crate::launcher::contains_marker(contents, HAND_COMMAND_ALIASES_MARKER)
        };
        // Read each Hand once: its checksum and capability come from one copy.
        let checksummed_hand = || -> Result<Option<Vec<u8>>> {
            let expected = match fs::read_to_string(selected.join(NANOCODEX2_CHECKSUM_FILE)) {
                Ok(expected) => expected.trim().to_ascii_lowercase(),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).wrap_err("failed to read the Hand checksum"),
            };
            match fs::read(selected.join(NANOCODEX2_BINARY_NAME)) {
                Ok(contents) if hex::encode(Sha256::digest(&contents)) == expected => {
                    Ok(Some(contents))
                }
                Ok(_) => Ok(None),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error).wrap_err("failed to read the selected Hand"),
            }
        };
        let hand = if cfg!(target_os = "macos")
            && fs::read(selected.join(APP_HAND)).is_ok_and(|contents| serves_aliases(&contents))
        {
            Some(APP_HAND)
        } else if checksummed_hand()?.is_some_and(|contents| serves_aliases(&contents)) {
            Some(NANOCODEX2_BINARY_NAME)
        } else {
            None
        };
        for alias in HAND_COMMAND_ALIASES {
            let path = self.root.join("bin").join(alias);
            match hand {
                Some(hand) => atomic_symlink(&path, &Path::new("../current").join(hand))?,
                None => {
                    let ours = fs::read_link(&path).is_ok_and(|target| {
                        [NANOCODEX2_BINARY_NAME, APP_HAND].iter().any(|hand| {
                            target == Path::new("../current").join(hand)
                                || target == self.root.join("current").join(hand)
                        })
                    });
                    if ours {
                        fs::remove_file(&path)
                            .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Remove a launcher link only if it points into `current` (ours).
    #[cfg(unix)]
    fn remove_own_current_link(&self, path: &Path) -> Result<()> {
        let ours = fs::read_link(path).is_ok_and(|target| {
            [BINARY_NAME, NANOCODEX2_BINARY_NAME].iter().any(|name| {
                target == Path::new("../current").join(name)
                    || target == self.root.join("current").join(name)
            })
        });
        if ours {
            fs::remove_file(path)
                .wrap_err_with(|| format!("failed to remove {}", path.display()))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn remove_retired_computer_launcher(&self) -> Result<()> {
        const LAUNCHER: &str = r#"#!/bin/sh
set -eu
case "$0" in
    */*) launcher=$0 ;;
    *) launcher=$(command -v "$0") ;;
esac
bin_dir=$(CDPATH= cd -- "$(dirname -- "$launcher")" && pwd -P)
install_root=$(dirname -- "$bin_dir")
exec "$install_root/current/nanocodex-computer" "$@"
"#;
        let path = self.root.join("bin/nanocodex-computer");
        // Earlier installers used either this wrapper or a direct current link.
        // Inspect the link itself so dangling launchers are retired as well.
        let retired_link = fs::read_link(&path).is_ok_and(|target| {
            target == Path::new("../current/nanocodex-computer")
                || target == self.root.join("current/nanocodex-computer")
        });
        if retired_link
            || (!path.is_symlink()
                && fs::read(&path).is_ok_and(|bytes| bytes == LAUNCHER.as_bytes()))
        {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn sync_nanocodex2_launcher(&self, key: &str) -> Result<()> {
        const LAUNCHER: &str = r#"#!/bin/sh
set -eu

case "$0" in
    */*) launcher=$0 ;;
    *) launcher=$(command -v "$0") ;;
esac
case "$launcher" in
    */*) launcher_dir=${launcher%/*} ;;
    *) launcher_dir=. ;;
esac
bin_dir=$(CDPATH= cd -- "${launcher_dir:-/}" && pwd -P)
install_root=${bin_dir%/*}
install_root=${install_root:-/}
export NANOCODEX_DIR="$install_root"
exec "$install_root/current/nanocodex2" "$@"
"#;

        // Recognize wrappers installed before the builtin path setup as well.
        const LEGACY_LAUNCHER: &str = r#"#!/bin/sh
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
        // A unified CLI is the nanocodex2 command (managed tree by argv[0]);
        // the Hand file of the same name is for services only.
        if fs::read(self.version_dir(key).join(BINARY_NAME)).is_ok_and(|contents| {
            crate::launcher::supports_native_launcher(&contents) && is_unified_cli(&contents)
        }) {
            return atomic_symlink(&path, &Path::new("../current").join(BINARY_NAME));
        }
        if file_matches_checksum(
            &self.version_dir(key).join(NANOCODEX2_BINARY_NAME),
            &self.version_dir(key).join(NANOCODEX2_CHECKSUM_FILE),
        )? {
            let contents = fs::read(self.version_dir(key).join(NANOCODEX2_BINARY_NAME))?;
            if crate::launcher::supports_native_launcher(&contents) {
                return atomic_symlink(
                    &path,
                    &Path::new("../current").join(NANOCODEX2_BINARY_NAME),
                );
            }
            return atomic_write(&path, LAUNCHER.as_bytes(), true);
        }
        // Inspect the link itself, including a dangling link after activating a
        // legacy version without the companion. Never follow/remove custom links.
        if path.is_symlink() {
            return self.remove_own_current_link(&path);
        }
        match fs::read(&path) {
            Ok(contents)
                if contents == LAUNCHER.as_bytes() || contents == LEGACY_LAUNCHER.as_bytes() =>
            {
                fs::remove_file(&path)
                    .wrap_err_with(|| format!("failed to remove {}", path.display()))
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).wrap_err_with(|| format!("failed to read {}", path.display())),
        }
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).wrap_err_with(|| format!("failed to remove {}", path.display())),
    }
}

/// Replace either an older wrapper or link without exposing a missing launcher.
#[cfg(unix)]
fn atomic_symlink(path: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    if fs::read_link(path).is_ok_and(|existing| existing == target) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("launcher has no parent"))?;
    fs::create_dir_all(parent)?;
    // A private directory reserves a unique name without unlinking another
    // update's staging path. Rename occurs on the same filesystem.
    let staging = tempfile::Builder::new()
        .prefix(".launcher-")
        .tempdir_in(parent)?;
    let temporary = staging.path().join("link");
    symlink(target, &temporary)?;
    fs::rename(&temporary, path).wrap_err_with(|| format!("failed to install {}", path.display()))
}

/// versions/<key>/Nanocodex.app -> ../../hand-versions/<identity>/Nanocodex.app
fn hand_app_link(identity: &str) -> PathBuf {
    Path::new("../..")
        .join(HAND_VERSIONS_DIR)
        .join(identity)
        .join(super::app::BUNDLE)
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

pub(super) fn atomic_write(path: &Path, contents: &[u8], executable: bool) -> Result<()> {
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

    const HAND_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HAND_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A Nanocodex.app archived exactly as scripts/release/macos-sign-hand.sh
    /// does (minus codesign, which only macOS has).
    fn release_app_archive(hand: &[u8], format: Option<&str>) -> Vec<u8> {
        use std::os::unix::fs::PermissionsExt;
        let work = tempfile::tempdir().unwrap();
        let contents = work.path().join("Nanocodex.app/Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::create_dir_all(contents.join("_CodeSignature")).unwrap();
        fs::write(contents.join("Info.plist"), b"<plist/>").unwrap();
        fs::write(contents.join("_CodeSignature/CodeResources"), b"sealed").unwrap();
        fs::write(contents.join("MacOS/nanocodex2"), hand).unwrap();
        fs::set_permissions(
            contents.join("MacOS/nanocodex2"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let archive = work
            .path()
            .join("nanocodex-app-aarch64-apple-darwin.tar.gz");
        let mut tar = std::process::Command::new("tar");
        tar.env("COPYFILE_DISABLE", "1").arg("--no-xattrs");
        if let Some(format) = format {
            tar.arg(format!("--format={format}"));
        }
        let status = tar
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(work.path())
            .arg("Nanocodex.app")
            .status()
            .unwrap();
        assert!(status.success());
        fs::read(archive).unwrap()
    }

    fn crafted_app_archive(name: &str, kind: tar::EntryType, mode: u32) -> Vec<u8> {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, mode) in [
            ("Nanocodex.app/Contents/Info.plist", 0o644),
            ("Nanocodex.app/Contents/_CodeSignature/CodeResources", 0o644),
            ("Nanocodex.app/Contents/MacOS/nanocodex2", mode),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(mode);
            header.set_cksum();
            archive
                .append_data(&mut header, path, &b"hand"[..])
                .unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_entry_type(kind);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("/etc/passwd").unwrap();
        }
        header.set_cksum();
        // append_data rejects '..'; write the raw name like a hostile archive.
        let bytes = name.as_bytes();
        header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
        header.set_cksum();
        archive.append(&header, std::io::empty()).unwrap();
        archive.into_inner().unwrap().finish().unwrap()
    }

    fn install_release(store: &VersionStore, key: &str, identity: &str, hand: &[u8]) {
        store
            .install_bundle_with_hand(key, b"cli", hand, Some(identity), None, None)
            .unwrap();
        store
            .install_hand_app(key, &release_app_archive(hand, None))
            .unwrap();
    }

    #[test]
    fn release_app_is_shared_by_cli_versions_with_an_unchanged_hand() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        install_release(&store, "1.0.0", HAND_A, b"hand-a");
        store.activate("1.0.0").unwrap();
        assert!(store.is_cached_bundle("1.0.0", false).unwrap());
        let first = store.hand_executable("1.0.0").canonicalize().unwrap();
        assert_eq!(
            first,
            directory.path().canonicalize().unwrap().join(format!(
                "hand-versions/{HAND_A}/Nanocodex.app/Contents/MacOS/nanocodex2"
            ))
        );
        assert_eq!(
            directory
                .path()
                .join("current/Nanocodex.app/Contents/MacOS/nanocodex2")
                .canonicalize()
                .unwrap(),
            first
        );
        let inode = fs::metadata(&first).unwrap().ino();

        // A CLI-only release: a re-signed archive of the same Hand never
        // replaces the stored bundle, so path and signature stay put.
        store
            .install_bundle_with_hand("1.0.1", b"cli-2", b"hand-a", Some(HAND_A), None, None)
            .unwrap();
        store
            .install_hand_app(
                "1.0.1",
                &release_app_archive(b"hand-a-resigned", Some("pax")),
            )
            .unwrap();
        store.activate("1.0.1").unwrap();
        let second = store.hand_executable("1.0.1").canonicalize().unwrap();
        assert_eq!(second, first);
        assert_eq!(fs::metadata(&second).unwrap().ino(), inode);
        assert_eq!(fs::read(&second).unwrap(), b"hand-a");

        // A changed Hand gets its own bundle; the previous one stays intact
        // for rollback.
        install_release(&store, "2.0.0", HAND_B, b"hand-b");
        store.activate("2.0.0").unwrap();
        assert_ne!(
            store.hand_executable("2.0.0").canonicalize().unwrap(),
            first
        );
        store.activate("1.0.1").unwrap();
        assert!(store.is_cached_bundle("1.0.1", false).unwrap());
        assert_eq!(
            directory
                .path()
                .join("current/Nanocodex.app/Contents/MacOS/nanocodex2")
                .canonicalize()
                .unwrap(),
            first
        );
    }

    #[test]
    fn corrupt_release_app_blocks_activation_until_reinstalled() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        install_release(&store, "1.0.0", HAND_A, b"hand-a");
        let bundle = directory
            .path()
            .join(format!("hand-versions/{HAND_A}/Nanocodex.app"));
        fs::write(bundle.join("Contents/Info.plist"), b"tampered").unwrap();
        assert!(!store.has_hand_app("1.0.0").unwrap());
        assert!(!store.is_cached_bundle("1.0.0", false).unwrap());
        assert!(store.activate("1.0.0").is_err());
        assert!(fs::read_link(directory.path().join("current")).is_err());

        store
            .install_hand_app("1.0.0", &release_app_archive(b"hand-a", None))
            .unwrap();
        assert!(store.is_cached_bundle("1.0.0", false).unwrap());
        // The corrupt bundle was set aside, not deleted under a running Hand.
        let aside = fs::read_dir(bundle.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".corrupt-app-")
            })
            .count();
        assert_eq!(aside, 1);

        // Unlisted files inside the sealed bundle also invalidate it.
        fs::write(bundle.join("Contents/MacOS/extra"), b"x").unwrap();
        assert!(!store.is_cached_bundle("1.0.0", false).unwrap());
    }

    #[test]
    fn hostile_or_incomplete_app_archives_change_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        install_release(&store, "1.0.0", HAND_A, b"hand-a");
        store
            .install_bundle_with_hand("1.0.1", b"cli-2", b"hand-b", Some(HAND_B), None, None)
            .unwrap();
        for archive in [
            crafted_app_archive("Nanocodex.app/../escaped", tar::EntryType::Regular, 0o755),
            crafted_app_archive(
                "Nanocodex.app/Contents/link",
                tar::EntryType::Symlink,
                0o755,
            ),
            crafted_app_archive("Nanocodex.app/Contents/hard", tar::EntryType::Link, 0o755),
            crafted_app_archive(
                "Nanocodex.app/Contents/._Info.plist",
                tar::EntryType::Regular,
                0o755,
            ),
            crafted_app_archive("Other.app/Contents/x", tar::EntryType::Regular, 0o755),
            crafted_app_archive(
                "Nanocodex.app/Contents/info.plist",
                tar::EntryType::Regular,
                0o755,
            ),
            // The Hand inside the bundle must be executable.
            crafted_app_archive(
                "Nanocodex.app/Contents/Resources/x",
                tar::EntryType::Regular,
                0o644,
            ),
            b"not a gzip archive".to_vec(),
        ] {
            assert!(store.install_hand_app("1.0.1", &archive).is_err());
            assert!(!store.has_hand_app("1.0.1").unwrap());
            assert_eq!(
                store.hand_executable("1.0.1"),
                directory
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join(format!("hand-versions/{HAND_B}/nanocodex2"))
            );
        }
        assert!(!directory.path().join("hand-versions/escaped").exists());
        assert!(store.has_hand_app("1.0.0").unwrap());
        // A version without a Hand identity is never bundled.
        store
            .install_bundle_with_hand("0.9.0", b"cli-0", b"hand-0", None, None, None)
            .unwrap();
        assert!(
            store
                .install_hand_app("0.9.0", &release_app_archive(b"hand-0", None))
                .is_err()
        );
    }

    #[test]
    fn native_launchers_switch_atomically_and_fall_back_for_older_versions() {
        use std::os::unix::fs::{MetadataExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        let binary = crate::launcher::NATIVE_LAUNCHER_MARKER;
        store
            .install_bundle("native", binary, binary, None, None)
            .unwrap();
        store.activate("native").unwrap();
        for (name, target) in [
            (BINARY_NAME, BINARY_NAME),
            (NANOCODEX2_BINARY_NAME, NANOCODEX2_BINARY_NAME),
            ("nc", NANOCODEX2_BINARY_NAME),
            ("ncl", BINARY_NAME),
        ] {
            let link = directory.path().join("bin").join(name);
            assert_eq!(
                fs::read_link(&link).unwrap(),
                Path::new("../current").join(target)
            );
            assert_eq!(fs::read(&link).unwrap(), binary);
        }
        let launcher = directory.path().join("bin").join(BINARY_NAME);
        let inode = fs::symlink_metadata(&launcher).unwrap().ino();
        store.install_launcher().unwrap();
        assert_eq!(fs::symlink_metadata(&launcher).unwrap().ino(), inode);

        store.install("legacy", b"old binary").unwrap();
        store.activate("legacy").unwrap();
        assert!(!launcher.is_symlink());
        assert!(
            fs::read_to_string(&launcher)
                .unwrap()
                .contains("updater/nanocodex")
        );
        assert_eq!(fs::read(store.binary_path("native")).unwrap(), binary);
        let companion = directory.path().join("bin").join(NANOCODEX2_BINARY_NAME);
        assert!(fs::symlink_metadata(&companion).is_err());
        // Short aliases need argv[0]; older binaries get no wrapper for them.
        for alias in ["nc", "ncl"] {
            assert!(fs::symlink_metadata(directory.path().join("bin").join(alias)).is_err());
        }

        // An older bundle gets its compatible companion wrapper as well.
        store
            .install_bundle("older-bundle", b"old", b"old2", None, None)
            .unwrap();
        store.activate("older-bundle").unwrap();
        assert!(!companion.is_symlink());
        store.activate("native").unwrap();
        assert!(companion.is_symlink());
        fs::remove_file(&companion).unwrap();
        symlink("/custom/missing/companion", &companion).unwrap();
        store.activate("legacy").unwrap();
        assert_eq!(
            fs::read_link(&companion).unwrap(),
            Path::new("/custom/missing/companion")
        );
    }

    #[test]
    fn unified_cli_serves_every_entrypoint_while_older_pairs_keep_theirs() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        let bin = directory.path().join("bin");
        let cli = [crate::launcher::NATIVE_LAUNCHER_MARKER, UNIFIED_CLI_MARKER].concat();
        store
            .install_bundle("unified", &cli, b"hand", None, None)
            .unwrap();
        store.activate("unified").unwrap();
        for name in [BINARY_NAME, NANOCODEX2_BINARY_NAME, "nc", "ncl"] {
            assert_eq!(
                fs::read_link(bin.join(name)).unwrap(),
                Path::new("../current").join(BINARY_NAME),
                "{name}"
            );
        }
        // The Hand keeps its service file name inside the version directory.
        assert_eq!(
            fs::read(
                directory
                    .path()
                    .join("current")
                    .join(NANOCODEX2_BINARY_NAME)
            )
            .unwrap(),
            b"hand"
        );
        // A Hand without the alias capability gets no Hand command links.
        for alias in crate::hand_executable::HAND_COMMAND_ALIASES {
            assert!(fs::symlink_metadata(bin.join(alias)).is_err(), "{alias}");
        }
        let hand = [
            b"hand ".as_slice(),
            crate::hand_executable::HAND_COMMAND_ALIASES_MARKER,
        ]
        .concat();
        store
            .install_bundle("aliased", &cli, &hand, None, None)
            .unwrap();
        store.activate("aliased").unwrap();
        for alias in crate::hand_executable::HAND_COMMAND_ALIASES {
            assert_eq!(
                fs::read_link(bin.join(alias)).unwrap(),
                Path::new("../current").join(NANOCODEX2_BINARY_NAME),
                "{alias}"
            );
            assert_eq!(fs::read(bin.join(alias)).unwrap(), hand, "{alias}");
        }
        store.activate("unified").unwrap();
        for alias in crate::hand_executable::HAND_COMMAND_ALIASES {
            assert!(fs::symlink_metadata(bin.join(alias)).is_err(), "{alias}");
        }

        let old = crate::launcher::NATIVE_LAUNCHER_MARKER;
        store.install_bundle("older", old, old, None, None).unwrap();
        store.activate("older").unwrap();
        for (name, target) in [
            (BINARY_NAME, BINARY_NAME),
            (NANOCODEX2_BINARY_NAME, NANOCODEX2_BINARY_NAME),
            ("nc", NANOCODEX2_BINARY_NAME),
            ("ncl", BINARY_NAME),
        ] {
            assert_eq!(
                fs::read_link(bin.join(name)).unwrap(),
                Path::new("../current").join(target),
                "{name}"
            );
        }
        store.activate("unified").unwrap();
        assert_eq!(
            fs::read_link(bin.join("nc")).unwrap(),
            Path::new("../current").join(BINARY_NAME)
        );
    }

    #[test]
    fn launchers_preserve_paths_arguments_and_cwd_without_external_utilities() {
        use std::{os::unix::fs::symlink, process::Command};

        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().canonicalize().unwrap();
        let original = parent.join("original install");
        let store = VersionStore::at(&original);
        let script = b"#!/bin/sh\nprintf '%s\\n' \"$NANOCODEX_DIR\" \"$PWD\" \"$@\"\nexit 23\n";
        store
            .install_bundle("test", script, script, None, None)
            .unwrap();
        store.activate("test").unwrap();
        let root = parent.join("moved install");
        fs::rename(original, &root).unwrap();
        symlink(root.join("bin"), parent.join("linked bin")).unwrap();
        let bin = root.join("bin");
        let arguments = ["a b", "", "*.txt", "--flag", "line\nbreak"];

        for name in [BINARY_NAME, NANOCODEX2_BINARY_NAME] {
            let cases = [
                (bin.join(name), parent.clone(), String::new()),
                (
                    PathBuf::from(format!("moved install/bin/../bin//{name}")),
                    parent.clone(),
                    String::new(),
                ),
                (
                    parent.join("linked bin").join(name),
                    parent.clone(),
                    String::new(),
                ),
                (
                    PathBuf::from(name),
                    parent.clone(),
                    format!("{}/", bin.display()),
                ),
                (
                    PathBuf::from(name),
                    parent.clone(),
                    "moved install/bin/".to_owned(),
                ),
                (PathBuf::from(name), bin.clone(), String::new()),
            ];
            for (launcher, cwd, path) in cases {
                let output = Command::new("/bin/sh")
                    .args(["-c", "exec \"$@\"", "launcher-test"])
                    .arg(&launcher)
                    .args(arguments)
                    .current_dir(&cwd)
                    .env("PATH", path)
                    .env("CDPATH", &parent)
                    .env("NANOCODEX_DIR", "must be replaced")
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(23), "{launcher:?}: {output:?}");
                let expected = format!(
                    "{}\n{}\n{}\n",
                    root.display(),
                    cwd.display(),
                    arguments.join("\n")
                );
                assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            }
        }

        // A bare $0 and command -v result exercise the dirname(.) fallback.
        let launcher = fs::read_to_string(bin.join(BINARY_NAME)).unwrap();
        let output = Command::new("/bin/sh")
            .args(["-c", &launcher, BINARY_NAME])
            .current_dir(&bin)
            .env("PATH", "")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}\n{}\n", root.display(), bin.display())
        );
    }

    #[test]
    fn launcher_redirects_update_only_with_updater_marker() {
        use std::process::Command;

        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store
            .install("test", b"#!/bin/sh\nprintf 'current:%s\\n' \"$@\"\n")
            .unwrap();
        store.activate("test").unwrap();
        atomic_write(
            &store.updater_path(),
            b"#!/bin/sh\nprintf 'updater:%s\\n' \"$@\"\n",
            true,
        )
        .unwrap();
        let launch = |args: &[&str]| {
            let output = Command::new(directory.path().join("bin/nanocodex"))
                .args(args)
                .env("PATH", "")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!(launch(&["update", "a b"]), "current:update\ncurrent:a b\n");
        fs::write(store.updater_checksum_path(), b"present").unwrap();
        assert_eq!(launch(&["update", "a b"]), "updater:update\nupdater:a b\n");
        assert_eq!(launch(&["--version"]), "current:--version\n");
    }

    #[test]
    fn removes_legacy_companion_wrapper_but_preserves_custom_wrapper() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.install("stable", b"legacy").unwrap();
        let launcher = directory.path().join("bin/nanocodex2");
        let legacy = r#"#!/bin/sh
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
        atomic_write(&launcher, legacy.as_bytes(), true).unwrap();
        store.activate("stable").unwrap();
        assert!(!launcher.exists());
        atomic_write(&launcher, b"#!/bin/sh\n# custom wrapper\n", true).unwrap();
        store.activate("stable").unwrap();
        assert_eq!(
            fs::read(&launcher).unwrap(),
            b"#!/bin/sh\n# custom wrapper\n"
        );
    }

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
    fn activation_retires_dangling_cua_links_and_preserves_custom_launchers() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.prepare_with_contents("0.3.0", b"current").unwrap();
        let launcher = directory.path().join("bin/nanocodex-computer");
        for target in [
            PathBuf::from("../current/nanocodex-computer"),
            directory.path().join("current/nanocodex-computer"),
        ] {
            symlink(target, &launcher).unwrap();
            store.activate("0.3.0").unwrap();
            assert!(fs::symlink_metadata(&launcher).is_err());
        }
        let custom = directory.path().join("custom-provider");
        symlink(&custom, &launcher).unwrap();
        store.activate("0.3.0").unwrap();
        assert_eq!(fs::read_link(&launcher).unwrap(), custom);
        fs::remove_file(&launcher).unwrap();
        fs::write(&launcher, b"user-owned launcher").unwrap();
        store.activate("0.3.0").unwrap();
        assert_eq!(fs::read(&launcher).unwrap(), b"user-owned launcher");
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
            .install_bundle(
                "nightly-build",
                b"cli",
                b"managed-cli",
                Some(b"guest"),
                None,
            )
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
            .install_bundle("0.5.0", b"stable-cli", b"stable-managed-cli", None, None)
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
            .install_bundle("0.4.0", b"stable-cli", b"stable-managed-cli", None, None)
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

    #[test]
    fn first_voice_use_repairs_only_the_running_managed_version() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store
            .install_bundle("release", b"cli", b"managed", None, None)
            .unwrap();
        let executable = store.binary_path("release");
        assert_eq!(
            store
                .voice_repair_directory("release", &executable)
                .unwrap(),
            Some(store.version_dir("release"))
        );
        let custom = directory.path().join("custom-cli");
        fs::write(&custom, b"cli").unwrap();
        assert!(
            store
                .voice_repair_directory("release", &custom)
                .unwrap()
                .is_none()
        );
        super::super::voice::install(
            &store.version_dir("release"),
            &super::super::voice::fixture(None),
        )
        .unwrap();
        assert!(
            store
                .voice_repair_directory("release", &executable)
                .unwrap()
                .is_none()
        );
        fs::remove_file(
            store
                .version_dir("release")
                .join("nanocodex-resources/voice/runtime.json"),
        )
        .unwrap();
        assert!(
            store
                .voice_repair_directory("release", &executable)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn runtime_repairs_legacy_cache_and_invalid_runtime_keeps_previous_version_active() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store
            .install_bundle("old", b"cli", b"managed", None, None)
            .unwrap();
        store.activate("old").unwrap();
        assert!(!store.is_cached_voice("old", None).unwrap());
        let voice = super::super::voice::fixture(None);
        store
            .install_bundle("old", b"cli", b"managed", None, Some(&voice))
            .unwrap();
        assert!(store.is_cached_voice("old", None).unwrap());
        assert!(
            store
                .install_bundle("new", b"new", b"managed", None, Some(b"invalid"))
                .is_err()
        );
        assert_eq!(store.active().unwrap().as_deref(), Some("old"));
        assert!(!store.version_dir("new").exists());
        fs::remove_file(
            store
                .version_dir("old")
                .join("nanocodex-resources/voice/bin/nanocodex-voice-host"),
        )
        .unwrap();
        assert!(!store.is_cached_voice("old", None).unwrap());
        assert!(store.activate("old").is_err());
        store
            .install_bundle("old", b"cli", b"managed", None, Some(&voice))
            .unwrap();
        store.activate("old").unwrap();
    }

    #[test]
    #[ignore = "requires NANOCODEX_TEST_VOICE_ARCHIVE built by scripts/build-voice-release.py"]
    fn installs_and_launches_the_real_release_runtime() {
        let archive = fs::read(std::env::var_os("NANOCODEX_TEST_VOICE_ARCHIVE").unwrap()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store
            .install_bundle("release", b"cli", b"managed", None, Some(&archive))
            .unwrap();
        store.activate("release").unwrap();
        assert!(
            store
                .is_cached_voice("release", Some(&hex::encode(Sha256::digest(&archive))))
                .unwrap()
        );
        let runtime = directory.path().join("current/nanocodex-resources/voice");
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(runtime.join("runtime.json")).unwrap()).unwrap();
        assert_eq!(receipt["developmentOnly"], false);
        assert_eq!(receipt["distribution"], "publicRelease");
        let helper = std::process::Command::new(runtime.join("bin/nanocodex-voice-host"))
            .arg("--build-commit")
            .output()
            .unwrap();
        assert!(helper.status.success());
        assert!(!helper.stdout.is_empty());
    }
    #[test]
    fn pending_update_requires_complete_verified_bundle_and_preserves_active() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        store.prepare_with_contents("old", b"old").unwrap();
        store.install("incomplete", b"new").unwrap();
        assert!(store.stage_pending("incomplete").is_err());
        store
            .install_bundle("new", b"new", b"hand", None, None)
            .unwrap();
        store.stage_pending("new").unwrap();
        assert_eq!(store.active().unwrap().as_deref(), Some("old"));
        assert_eq!(store.pending().unwrap().as_deref(), Some("new"));
        fs::write(
            store.version_dir("new").join(NANOCODEX2_BINARY_NAME),
            b"corrupt",
        )
        .unwrap();
        assert!(store.stage_pending("new").is_err());
        store.clear_pending().unwrap();
        assert_eq!(store.pending().unwrap(), None);
    }

    #[test]
    fn update_lock_excludes_concurrent_activation_and_releases_on_exit() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        let lock = store.update_lock().unwrap();
        assert!(store.update_lock().is_err());
        drop(lock);
        assert!(store.update_lock().is_ok());
    }

    #[test]
    fn pending_update_rejects_path_traversal() {
        let directory = tempfile::tempdir().unwrap();
        let store = VersionStore::at(directory.path());
        fs::write(directory.path().join("pending-update"), "../../other").unwrap();
        assert!(store.pending().is_err());
    }
}
