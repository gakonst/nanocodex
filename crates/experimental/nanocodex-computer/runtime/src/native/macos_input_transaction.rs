//! Cooperative cross-process admission for an app's synthetic focus + input.
//! Never unlink lock files: doing so lets waiters lock different inodes.
use crate::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    time::{Duration, Instant},
};

pub struct Transaction(File);
impl Transaction {
    pub fn acquire(pid: i32) -> Result<Self> {
        if pid <= 0 {
            return Err(Error::invalid("Input target requires a positive PID"));
        }
        let uid = unsafe { libc::geteuid() };
        let directory = std::path::PathBuf::from(format!("/tmp/nanocodex-cua-input-{uid}"));
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(Error::action("Unsafe input transaction directory"));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join(format!("{pid}.lock")))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(Error::action("Unsafe input transaction file"));
        }
        Self::lock(file, Duration::from_secs(5))
    }
    fn lock(file: File, timeout: Duration) -> Result<Self> {
        let started = Instant::now();
        loop {
            crate::native::check_native_cancellation()?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock
                && error.kind() != std::io::ErrorKind::Interrupted
            {
                return Err(error.into());
            }
            if started.elapsed() >= timeout {
                return Err(Error::action(
                    "Application input is busy in another companion; no input was sent",
                ));
            }
            super::pump(Duration::from_millis(5));
        }
    }
}
impl Drop for Transaction {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_file_descriptions_exclude_same_target_and_release_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target");
        let open = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap()
        };
        let first = Transaction::lock(open(), Duration::ZERO).unwrap();
        assert!(Transaction::lock(open(), Duration::ZERO).is_err());
        let other = tempfile::tempfile().unwrap();
        let independent = Transaction::lock(other, Duration::ZERO).unwrap();
        drop(first);
        assert!(Transaction::lock(open(), Duration::ZERO).is_ok());
        drop(independent);
    }
    #[test]
    fn separate_process_is_excluded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let _held = Transaction::lock(file, Duration::ZERO).unwrap();
        let status = std::process::Command::new("/usr/bin/python3").args(["-c", "import fcntl,sys\nf=open(sys.argv[1], 'r+')\ntry: fcntl.flock(f, fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept BlockingIOError: sys.exit(0)\nsys.exit(1)"]).arg(path).status().unwrap();
        assert!(status.success());
    }
}

// Cursor overlays are process-global, while Desktop providers belong to engines.
// Only the last engine releasing a target may remove its shared visual.
fn cursor_owners() -> &'static std::sync::Mutex<std::collections::BTreeMap<(i32, u32), usize>> {
    static OWNERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<(i32, u32), usize>>,
    > = std::sync::OnceLock::new();
    OWNERS.get_or_init(Default::default)
}
pub fn retain_cursor_target(target: (i32, u32)) {
    *cursor_owners()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(target)
        .or_default() += 1;
}
pub fn release_cursor_targets(targets: impl IntoIterator<Item = (i32, u32)>) -> Vec<(i32, u32)> {
    let mut owners = cursor_owners().lock().unwrap_or_else(|e| e.into_inner());
    let mut released = Vec::new();
    for target in targets {
        if let Some(count) = owners.get_mut(&target) {
            *count -= 1;
            if *count == 0 {
                owners.remove(&target);
                released.push(target);
            }
        }
    }
    released
}
#[cfg(test)]
mod cursor_ownership_tests {
    use super::*;
    #[test]
    fn ending_one_session_keeps_a_shared_target_and_releases_only_its_exclusive_target() {
        let shared = (777_777, 41);
        let exclusive = (777_777, 42);
        retain_cursor_target(shared);
        retain_cursor_target(shared);
        retain_cursor_target(exclusive);
        assert_eq!(release_cursor_targets([shared, exclusive]), vec![exclusive]);
        assert_eq!(release_cursor_targets([shared]), vec![shared]);
        assert!(release_cursor_targets([shared]).is_empty());
    }
}
