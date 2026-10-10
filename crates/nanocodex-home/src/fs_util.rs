use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

/// What occupies a path, without following a final symlink into a missing target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathState {
    Absent,
    /// A symlink whose target does not resolve.
    Dangling,
    File {
        canonical: PathBuf,
    },
    Dir {
        canonical: PathBuf,
    },
    /// A FIFO, socket, device or other non-regular entry.
    Other,
}

impl PathState {
    pub(crate) fn canonical(&self) -> Option<&Path> {
        match self {
            Self::File { canonical } | Self::Dir { canonical } => Some(canonical),
            _ => None,
        }
    }
}

pub(crate) fn path_state(path: &Path) -> io::Result<PathState> {
    let link = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(PathState::Absent),
        Err(error) => return Err(error),
    };
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if link.file_type().is_symlink() && is_missing(&error) => {
            return Ok(PathState::Dangling);
        }
        Err(error) => return Err(error),
    };
    let canonical = || fs::canonicalize(path);
    Ok(if metadata.is_file() {
        PathState::File {
            canonical: canonical()?,
        }
    } else if metadata.is_dir() {
        PathState::Dir {
            canonical: canonical()?,
        }
    } else {
        PathState::Other
    })
}

fn is_missing(error: &io::Error) -> bool {
    // ELOOP (symlink cycles) and ENOTDIR through a link are also unresolvable.
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    ) || error.raw_os_error() == Some(libc_eloop())
}

const fn libc_eloop() -> i32 {
    // ELOOP is 62 on macOS/BSD and 40 on Linux.
    if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd"
    )) {
        62
    } else {
        40
    }
}

/// Reads at most `limit` bytes as lossy UTF-8; reports truncation.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> io::Result<(String, bool)> {
    let file = File::open(path)?;
    let mut data = Vec::with_capacity(limit.saturating_add(1).min(8 * 1024));
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut data)?;
    let truncated = data.len() > limit;
    data.truncate(limit);
    let mut text = String::from_utf8_lossy(&data).into_owned();
    if truncated {
        // Drop a replacement character produced by a split multi-byte sequence.
        if text.ends_with('\u{fffd}') {
            text.pop();
        }
    }
    Ok((text, truncated))
}

/// Directory entries sorted by file name; hidden (`.`-prefixed) names skipped.
pub(crate) fn sorted_visible_entries(dir: &Path, limit: usize) -> io::Result<(Vec<PathBuf>, bool)> {
    let mut names = Vec::new();
    let mut truncated = false;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if names.len() == limit {
            truncated = true;
            break;
        }
        names.push(name);
    }
    names.sort();
    Ok((
        names.into_iter().map(|name| dir.join(name)).collect(),
        truncated,
    ))
}
