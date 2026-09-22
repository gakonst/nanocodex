//! Same-user reload broadcasts. OS file locks, never PIDs, identify live TUIs.
//!
//! Register before entering the TUI and keep the registration alive until teardown.
//! `requested` is cancellation safe and may be used directly in `tokio::select!`.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

static LAUNCH_EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
const LAUNCH_PATH_ENV: &str = "NANOCODEX2_RELOAD_EXECUTABLE";

pub struct Registration {
    directory: PathBuf,
    path: PathBuf,
    // Keep the exclusive lock alive for precisely the lifetime of this registration.
    _lease: File,
    baseline: Option<String>,
    interval: Option<tokio::time::Interval>,
}

impl Registration {
    /// Check before leaving the event loop so failures remain visible in the TUI.
    pub fn preflight(&self) -> Result<(), String> {
        preflight()
    }

    /// Replace this TUI after caller teardown, retaining an explicit attach URL origin.
    pub fn restart(self, agent_id: &str) -> Result<(), String> {
        let arguments: Vec<_> = std::env::args_os().collect();
        let origin = arguments.windows(2).find_map(|pair| {
            if pair[0] != "attach" {
                return None;
            }
            attach_origin(pair[1].to_str()?)
        });
        self.preflight()?;
        drop(self);
        exec(agent_id, origin.as_deref())
    }

    /// Wait for a newer broadcast. Multiple broadcasts between polls coalesce.
    pub async fn requested(&mut self) -> Result<(), String> {
        loop {
            self.interval
                .get_or_insert_with(|| tokio::time::interval(Duration::from_millis(250)))
                .tick()
                .await;
            let generation = read_generation(&self.directory).map_err(message)?;
            if generation != self.baseline {
                self.baseline = generation;
                return Ok(());
            }
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // UUID names are never reused. Removing while holding the lease is safe.
        let _ = fs::remove_file(&self.path);
    }
}

/// Join broadcasts and remember the launch path before an installer replaces it.
pub fn register() -> Result<Registration, String> {
    launch_executable()?;
    register_in(&private_directory()?)
}

fn register_in(directory: &Path) -> Result<Registration, String> {
    let _guard = coordination_lock(directory).map_err(message)?;
    let baseline = read_generation(directory).map_err(message)?;
    let path = directory.join(format!("{}.lease", uuid::Uuid::new_v4()));
    let lease = private_options()
        .create_new(true)
        .open(&path)
        .map_err(message)?;
    if let Err(error) = try_lock(&lease) {
        let _ = fs::remove_file(&path);
        return Err(message(error));
    }
    Ok(Registration {
        directory: directory.to_owned(),
        path,
        _lease: lease,
        baseline,
        interval: None,
    })
}

/// Broadcast to registrations present at this instant and return their live count.
/// The count means notified registrations, not completed process replacements.
pub fn request_all() -> Result<usize, String> {
    preflight()?;
    request_in(&private_directory()?)
}

fn request_in(directory: &Path) -> Result<usize, String> {
    let _guard = coordination_lock(directory).map_err(message)?;
    let mut count = 0;
    for entry in fs::read_dir(directory).map_err(message)? {
        let entry = entry.map_err(message)?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("lease") {
            continue;
        }
        // A crashed process releases its kernel lock. Never interpret file contents as a PID.
        let lease = match private_options().open(entry.path()) {
            Ok(lease) => lease,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(message(error)),
        };
        match try_lock(&lease) {
            Ok(()) => match fs::remove_file(entry.path()) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(message(error)),
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => count += 1,
            Err(error) => return Err(message(error)),
        }
    }
    // Atomic rename makes every reader see either the complete old or new UUID.
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(message)?;
    writeln!(temporary, "{}", uuid::Uuid::new_v4()).map_err(message)?;
    temporary
        .persist(directory.join("generation"))
        .map_err(|error| message(error.error))?;
    Ok(count)
}

fn read_generation(directory: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(directory.join("generation")) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    options
}

fn try_lock(file: &File) -> io::Result<()> {
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
        std::fs::TryLockError::Error(error) => error,
    })
}

fn coordination_lock(directory: &Path) -> io::Result<File> {
    let file = private_options()
        .create(true)
        .open(directory.join("coordination"))?;
    file.lock()?;
    Ok(file)
}

#[cfg(unix)]
fn private_directory() -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // Avoid a home-directory registry: HOME can be shared by several machines.
    let uid = nix::unistd::geteuid().as_raw();
    let directory = match std::env::var_os("NANOCODEX_RELOAD_DIR") {
        Some(path) => PathBuf::from(path),
        None => std::env::temp_dir().join(format!("nanocodex2-reload-{uid}")),
    };
    if !directory.is_absolute() {
        return Err("NANOCODEX_RELOAD_DIR must be an absolute path".to_owned());
    }
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(message(error)),
    }
    let metadata = fs::symlink_metadata(&directory).map_err(message)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(format!(
            "reload directory must be owned by this user and private (0700): {}",
            directory.display()
        ));
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn private_directory() -> Result<PathBuf, String> {
    Err("local TUI reload requires Unix private-directory permissions".to_owned())
}

fn launch_executable() -> Result<&'static Path, String> {
    if let Some(path) = LAUNCH_EXECUTABLE.get() {
        return Ok(path);
    }
    // Persist the original pathname through exec. On Linux current_exe points to
    // a deleted inode after an atomic update, so it must be captured beforehand.
    let path = match std::env::var_os(LAUNCH_PATH_ENV) {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe().map_err(message)?,
    };
    if !path.is_absolute() {
        return Err("reload executable path must be absolute".to_owned());
    }
    let _ = LAUNCH_EXECUTABLE.set(path);
    Ok(LAUNCH_EXECUTABLE
        .get()
        .expect("launch executable was initialized"))
}

/// Validate this process's replacement before requesting reload or tearing down.
/// Every recipient must also check its own executable before terminal teardown.
/// A later installer race or loader failure can still make exec fail.
pub fn preflight() -> Result<(), String> {
    check_executable(launch_executable()?)
}

#[cfg(unix)]
fn check_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "reload executable {} is unavailable: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "reload executable is not an executable file: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_executable(_path: &Path) -> Result<(), String> {
    Err("in-place TUI reload requires Unix exec".to_owned())
}

/// Replace the calling process after terminal restoration, preserving cwd and env.
/// The current agent is attached again; a managed origin is encoded as its URL.
#[cfg(unix)]
pub fn exec(agent_id: &str, managed_origin: Option<&str>) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let executable = launch_executable()?;
    check_executable(executable)?;
    let mut command = std::process::Command::new(executable);
    command.args(restart_arguments(
        agent_id,
        managed_origin,
        std::env::args_os().skip(1),
    ));
    command.env(LAUNCH_PATH_ENV, executable);
    // Command inherits both the environment and current working directory.
    Err(format!(
        "could not reload {}: {}",
        executable.display(),
        command.exec()
    ))
}

#[cfg(not(unix))]
pub fn exec(_agent_id: &str, _managed_origin: Option<&str>) -> Result<(), String> {
    Err("in-place TUI reload requires Unix exec".to_owned())
}

fn restart_arguments(
    agent_id: &str,
    managed_origin: Option<&str>,
    original: impl IntoIterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    if agent_id.is_empty() {
        original.into_iter().collect()
    } else {
        vec![
            "attach".into(),
            agent_reference(agent_id, managed_origin).into(),
        ]
    }
}

fn attach_origin(argument: &str) -> Option<String> {
    crate::parse_agent_reference(argument).ok()?.managed_origin
}

fn agent_reference(agent_id: &str, managed_origin: Option<&str>) -> String {
    match managed_origin {
        Some(origin) => format!(
            "{}/agent/{}",
            origin.trim_end_matches('/'),
            percent_encoding::utf8_percent_encode(agent_id, percent_encoding::NON_ALPHANUMERIC)
        ),
        None => agent_id.to_owned(),
    }
}

fn message(error: io::Error) -> String {
    format!("local TUI reload: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcasts_to_all_live_registrations_without_replaying_on_restart() {
        let directory = tempfile::tempdir().unwrap();
        let mut first = register_in(directory.path()).unwrap();
        let mut second = register_in(directory.path()).unwrap();
        assert_eq!(request_in(directory.path()).unwrap(), 2);
        tokio::time::timeout(Duration::from_secs(1), first.requested())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), second.requested())
            .await
            .unwrap()
            .unwrap();
        let mut replacement = register_in(directory.path()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), replacement.requested())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), first.requested())
                .await
                .is_err()
        );
        assert_eq!(request_in(directory.path()).unwrap(), 3);
        tokio::time::timeout(Duration::from_secs(1), replacement.requested())
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn removes_crashed_leases_and_does_not_count_dropped_registrations() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("abandoned.lease"), "not a pid").unwrap();
        let registration = register_in(directory.path()).unwrap();
        assert_eq!(request_in(directory.path()).unwrap(), 1);
        assert!(!directory.path().join("abandoned.lease").exists());
        drop(registration);
        assert_eq!(request_in(directory.path()).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_missing_non_executable_and_directory_paths() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("replacement");
        assert!(check_executable(&path).is_err());
        assert!(check_executable(directory.path()).is_err());
        fs::write(&path, "replacement").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_executable(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_executable(&path).is_ok());
    }

    #[test]
    fn picker_restart_preserves_original_arguments() {
        let original: Vec<std::ffi::OsString> = vec!["attach".into()];
        assert_eq!(restart_arguments("", None, original.clone()), original);
        assert_eq!(
            restart_arguments("agent:v1_test", None, original),
            vec![std::ffi::OsString::from("attach"), "agent:v1_test".into()]
        );
        assert!(restart_arguments("", None, Vec::new()).is_empty());
    }

    #[test]
    fn bare_agent_with_colon_is_not_an_explicit_origin() {
        assert_eq!(attach_origin("agent:v1_test"), None);
        assert_eq!(attach_origin("plain-agent"), None);
        assert_eq!(
            attach_origin("https://nanocodex.localhost/agent/agent%3Av1_test"),
            Some("https://nanocodex.localhost".into())
        );
    }

    #[test]
    fn retains_agent_and_origin() {
        assert_eq!(agent_reference("agent:v1_test", None), "agent:v1_test");
        assert_eq!(
            agent_reference("agent:v1_test", Some("https://nanocodex.example/")),
            "https://nanocodex.example/agent/agent%3Av1%5Ftest"
        );
    }
}
