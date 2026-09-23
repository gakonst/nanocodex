//! Native setup shared by the two installed Unix entrypoints.
//!
//! Call initialization before starting threads or loading a workspace `.env`.
//! Relative bin -> current links pin the selected executable at exec time; setup
//! must discover its installation without changing argv, PATH, or the cwd.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

// Retained by initialize_install_root, including optimized/stripped builds. The
// updater uses this capability marker when activating an older cached binary:
// those versions still need the portable shell wrapper for update dispatch and
// NANOCODEX_DIR. This is a capability hint, not an integrity check.
pub(crate) const NATIVE_LAUNCHER_MARKER: &[u8] = b"NANOCODEX_NATIVE_LAUNCHER_V1";

#[allow(dead_code)] // Only the legacy binary installs launchers.
pub(crate) fn supports_native_launcher(contents: &[u8]) -> bool {
    contents
        .windows(NATIVE_LAUNCHER_MARKER.len())
        .any(|window| window == NATIVE_LAUNCHER_MARKER)
}

pub(crate) fn initialize_install_root() {
    std::hint::black_box(NATIVE_LAUNCHER_MARKER);
    #[cfg(unix)]
    if let Some(root) = running_install_root() {
        // Match the old installed wrapper: this executable's installation wins
        // over inherited config. Development binaries retain explicit env/HOME.
        // SAFETY: both entrypoints call this before any runtime/thread starts.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("NANOCODEX_DIR", root);
        }
    }
}

/// Fallback for callers which discover the store without entrypoint setup.
pub(crate) fn running_install_root() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        installed_root_for(&std::env::current_exe().ok()?)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
fn installed_root_for(executable: &Path) -> Option<PathBuf> {
    let executable = executable.canonicalize().ok()?;
    if !matches!(
        executable.file_name()?.to_str()?,
        "nanocodex" | "nanocodex2"
    ) {
        return None;
    }
    let directory = executable.parent()?;
    let root = if directory.file_name()? == "updater" {
        directory.parent()?
    } else {
        let versions = directory.parent()?;
        if versions.file_name()? != "versions" {
            return None;
        }
        versions.parent()?
    };
    // Do not mistake similarly named directories in a source checkout for an
    // installation. The current target need not equal this pinned version.
    if !root.join("current").is_symlink() || !root.join("versions").is_dir() {
        return None;
    }
    Some(root.to_path_buf())
}

/// Keep `nanocodex update ...` on the separate update manager, even when the
/// active application is version-pinned. Other commands never spawn a process.
#[allow(dead_code)] // nanocodex2 has no update command.
pub(crate) fn dispatch_update() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let mut args = std::env::args_os();
        let argv0 = args.next().unwrap_or_default();
        if args.next().as_deref() != Some(std::ffi::OsStr::new("update")) {
            return Ok(());
        }
        let executable = std::env::current_exe()?;
        if let Some(updater) = updater_for(&executable)? {
            return Err(std::process::Command::new(updater)
                .arg0(argv0)
                .args(std::env::args_os().skip(1))
                .exec());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn updater_for(executable: &Path) -> std::io::Result<Option<PathBuf>> {
    use std::os::unix::fs::MetadataExt;

    let Some(root) = installed_root_for(executable) else {
        return Ok(None);
    };
    if !root.join("updater/nanocodex.sha256").is_file() {
        return Ok(None);
    }
    let updater = root.join("updater/nanocodex");
    let running = std::fs::metadata(executable)?;
    let manager = std::fs::metadata(&updater)?;
    // Inode identity also handles hard links and avoids an environment sentinel
    // that would accidentally bypass dispatch in child CLI invocations.
    if running.dev() == manager.dev() && running.ino() == manager.ino() {
        return Ok(None);
    }
    Ok(Some(updater))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Store(PathBuf);
    impl Store {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nanocodex-launcher-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::create_dir_all(path.join("versions/pinned")).unwrap();
            fs::create_dir(path.join("updater")).unwrap();
            fs::create_dir(path.join("bin")).unwrap();
            fs::write(path.join("versions/pinned/nanocodex"), b"application").unwrap();
            symlink("versions/pinned", path.join("current")).unwrap();
            symlink("../current/nanocodex", path.join("bin/nanocodex")).unwrap();
            Self(path)
        }
        fn executable(&self) -> PathBuf {
            self.0.join("versions/pinned/nanocodex")
        }
    }
    impl Drop for Store {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn managed_root_survives_relocation_and_current_switch() {
        let mut store = Store::new();
        let moved = store.0.with_extension("moved install");
        fs::rename(&store.0, &moved).unwrap();
        store.0 = moved;
        let root = store.0.canonicalize().unwrap();
        assert_eq!(
            installed_root_for(&store.0.join("bin/nanocodex")),
            Some(root.clone())
        );
        fs::create_dir(store.0.join("versions/new")).unwrap();
        fs::remove_file(store.0.join("current")).unwrap();
        symlink("versions/new", store.0.join("current")).unwrap();
        assert_eq!(installed_root_for(&store.executable()), Some(root));
    }

    #[test]
    fn standalone_binaries_do_not_infer_an_installation() {
        let store = Store::new();
        let standalone = store.0.join("nanocodex");
        fs::write(&standalone, b"development").unwrap();
        assert_eq!(installed_root_for(&standalone), None);
        fs::remove_file(store.0.join("current")).unwrap();
        assert_eq!(installed_root_for(&store.executable()), None);
    }

    #[test]
    fn updater_requires_marker_and_does_not_recurse_through_hardlinks() {
        let store = Store::new();
        let updater = store.0.join("updater/nanocodex");
        fs::write(&updater, b"manager").unwrap();
        assert_eq!(updater_for(&store.executable()).unwrap(), None);
        fs::write(store.0.join("updater/nanocodex.sha256"), b"marker").unwrap();
        assert_eq!(
            updater_for(&store.executable()).unwrap(),
            Some(updater.canonicalize().unwrap())
        );
        assert_eq!(updater_for(&updater).unwrap(), None);
        fs::remove_file(&updater).unwrap();
        fs::hard_link(store.executable(), &updater).unwrap();
        assert_eq!(updater_for(&store.executable()).unwrap(), None);
        fs::remove_file(&updater).unwrap();
        assert!(updater_for(&store.executable()).is_err());
    }

    #[test]
    fn capability_detection_keeps_older_binaries_on_the_compatible_wrapper() {
        assert!(!supports_native_launcher(b"old binary"));
        let mut binary = b"header".to_vec();
        binary.extend_from_slice(NATIVE_LAUNCHER_MARKER);
        binary.extend_from_slice(b"trailer");
        assert!(supports_native_launcher(&binary));
    }
}
