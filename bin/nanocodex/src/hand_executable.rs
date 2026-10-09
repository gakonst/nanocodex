//! The two role-split executables and how each finds the other.
//!
//! `nanocodex` is the user-facing CLI. `nanocodex-hand` is the Hand daemon. An
//! installation stores the Hand under the historical file name `nanocodex2`
//! (`versions/<key>/nanocodex2`, or later `Nanocodex.app/Contents/MacOS/nanocodex2`),
//! which service records (launchd, systemd, Windows tasks) keep naming.

use std::{
    ffi::OsString,
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
    process::ExitCode,
};

/// Explicit Hand executable for development and tests.
pub(crate) const HAND_EXECUTABLE_ENV: &str = "NANOCODEX_HAND_EXECUTABLE";
/// Set on a process the CLI forwarded to the Hand, so the Hand never forwards back.
const FORWARDED_ENV: &str = "NANOCODEX_ROLE_FORWARDED";

const EXE_SUFFIX: &str = std::env::consts::EXE_SUFFIX;

fn file_name(stem: &str) -> String {
    format!("{stem}{EXE_SUFFIX}")
}

/// Whether `path` names an executable a Hand service record may run:
/// `nanocodex2` (the installed Hand), `nanocodex-hand` (a build output), or
/// `nanocodex` (the CLI, which forwards `hand` to the Hand).
pub(crate) fn is_hand_file_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let stem = if cfg!(windows) {
        name.len()
            .checked_sub(4)
            .filter(|split| {
                name.is_char_boundary(*split) && name[*split..].eq_ignore_ascii_case(".exe")
            })
            .map_or(name, |split| &name[..split])
    } else {
        name
    };
    ["nanocodex2", "nanocodex-hand", "nanocodex"]
        .iter()
        .any(|accepted| {
            if cfg!(windows) {
                stem.eq_ignore_ascii_case(accepted)
            } else {
                stem == *accepted
            }
        })
}

/// Paths under an installation's `current` link that run a Nanocodex executable.
pub(crate) fn current_executables(install: &Path) -> [PathBuf; 3] {
    let current = install.join("current");
    [
        current.join("nanocodex2"),
        current.join("nanocodex"),
        current.join("Nanocodex.app/Contents/MacOS/nanocodex2"),
    ]
}

fn running_paths() -> Vec<PathBuf> {
    let Ok(running) = std::env::current_exe() else {
        return Vec::new();
    };
    let mut paths = vec![running.clone()];
    if let Ok(canonical) = running.canonicalize()
        && canonical != running
    {
        paths.push(canonical);
    }
    paths
}

/// A candidate distinct from this running executable (never a self-loop).
fn other_executable(candidate: &Path) -> bool {
    candidate.is_file()
        && running_paths()
            .first()
            .is_none_or(|running| !same_file_contents(running, candidate).unwrap_or(false))
}

/// Locate the installed Hand daemon from the CLI.
pub(crate) fn hand_binary() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os(HAND_EXECUTABLE_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let mut candidates = Vec::new();
    for running in running_paths() {
        // A build directory or bundle holds nanocodex-hand; an installed
        // version holds the Hand as nanocodex2 beside the CLI.
        candidates.push(running.with_file_name(file_name("nanocodex-hand")));
        candidates.push(running.with_file_name(file_name("nanocodex2")));
    }
    if let Some(root) = crate::launcher::running_install_root()
        .or_else(|| std::env::var_os("NANOCODEX_DIR").map(PathBuf::from))
    {
        candidates.push(root.join("hand/current/Nanocodex.app/Contents/MacOS/nanocodex2"));
        candidates.push(root.join(format!("hand/current/{}", file_name("nanocodex2"))));
        candidates.push(root.join("current/Nanocodex.app/Contents/MacOS/nanocodex2"));
        candidates.push(root.join(format!("current/{}", file_name("nanocodex2"))));
    }
    candidates
        .into_iter()
        .find(|candidate| other_executable(candidate))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "the Nanocodex Hand executable is not installed beside this CLI; run `nanocodex update`, or build nanocodex-hand and set {HAND_EXECUTABLE_ENV}"
                ),
            )
        })
}

/// Locate the user-facing CLI from the Hand (installed beside it as `nanocodex`).
pub(crate) fn cli_binary() -> io::Result<PathBuf> {
    let mut candidates = Vec::new();
    for running in running_paths() {
        candidates.push(running.with_file_name(file_name("nanocodex")));
        // versions/<key>/Nanocodex.app/Contents/MacOS/nanocodex2 -> versions/<key>/nanocodex
        if let Some(version) = running
            .parent()
            .filter(|directory| directory.ends_with("Nanocodex.app/Contents/MacOS"))
            .and_then(Path::parent)
            .and_then(Path::parent)
            .and_then(Path::parent)
        {
            candidates.push(version.join(file_name("nanocodex")));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| other_executable(candidate))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the Nanocodex CLI is not installed beside this Hand executable",
            )
        })
}

static HAND_ROLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that this process is the `nanocodex-hand` executable.
pub(crate) fn set_hand_role() {
    HAND_ROLE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this process is the Hand executable rather than the CLI.
pub(crate) fn is_hand_role() -> bool {
    HAND_ROLE.load(std::sync::atomic::Ordering::Relaxed)
}

static FORWARDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record whether the other role forwarded this process here, then remove the
/// marker from this process's environment so no child (Hand services, tool
/// commands, terminals, or a later forward) inherits it. Call first thing in
/// `main`, before any thread starts.
pub(crate) fn take_forwarded() {
    if std::env::var_os(FORWARDED_ENV).is_some() {
        FORWARDED.store(true, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: called once at process start, before Tokio or any other
        // thread exists, so no concurrent environment access is possible.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(FORWARDED_ENV);
        }
    }
}

/// Whether this process was forwarded here by the other role.
pub(crate) fn forwarded() -> bool {
    FORWARDED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Run `executable` with `arguments` (argv without argv\[0\]) in place of this
/// process, preserving the exit status. Unix replaces the process image.
pub(crate) fn forward(
    executable: &Path,
    argv0: Option<OsString>,
    arguments: &[OsString],
) -> ExitCode {
    let mut command = std::process::Command::new(executable);
    command.args(arguments).env(FORWARDED_ENV, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        if let Some(argv0) = argv0 {
            command.arg0(argv0);
        }
        let error = command.exec();
        eprintln!("Error: could not start {}: {error}", executable.display());
        ExitCode::FAILURE
    }
    #[cfg(not(unix))]
    {
        let _ = argv0;
        match command.status() {
            Ok(status) => status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .map_or(ExitCode::FAILURE, ExitCode::from),
            Err(error) => {
                eprintln!("Error: could not start {}: {error}", executable.display());
                ExitCode::FAILURE
            }
        }
    }
}

/// Byte equality of two regular files (or one file reached through links).
pub(crate) fn same_file_contents(left: &Path, right: &Path) -> io::Result<bool> {
    let (Ok(left_meta), Ok(right_meta)) = (fs::metadata(left), fs::metadata(right)) else {
        return Ok(false);
    };
    if !left_meta.is_file() || !right_meta.is_file() || left_meta.len() != right_meta.len() {
        return Ok(false);
    }
    if fs::canonicalize(left)? == fs::canonicalize(right)? {
        return Ok(true);
    }
    let (mut left, mut right) = (fs::File::open(left)?, fs::File::open(right)?);
    let (mut a, mut b) = (vec![0; 1 << 16], vec![0; 1 << 16]);
    loop {
        let read = left.read(&mut a)?;
        if read == 0 {
            return Ok(true);
        }
        right.read_exact(&mut b[..read])?;
        if a[..read] != b[..read] {
            return Ok(false);
        }
    }
}
