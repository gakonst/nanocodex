use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::EvalError;

/// Native storage for one evaluator attempt.
#[derive(Clone, Debug)]
pub(crate) struct EvalJob {
    id: Uuid,
    directory: PathBuf,
    parent_directory: PathBuf,
}

impl EvalJob {
    pub(crate) fn create(parent: &Path) -> Result<Self, EvalError> {
        let parent_directory = prepare_parent_directory(parent)?;
        let id = Uuid::now_v7();
        let directory = create_durable_directory_all(&parent_directory.join(id.to_string()))?;
        Ok(Self {
            id,
            directory,
            parent_directory,
        })
    }

    pub(crate) const fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn parent_directory(&self) -> &Path {
        &self.parent_directory
    }
}

fn prepare_parent_directory(path: &Path) -> io::Result<PathBuf> {
    require_durable_directory_sync()?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    create_durable_directory_all(&absolute)
}

pub(crate) fn create_durable_directory_all(path: &Path) -> io::Result<PathBuf> {
    create_durable_directory_all_with_sync(path, sync_directory)
}

fn create_durable_directory_all_with_sync<F>(
    path: &Path,
    mut sync_directory: F,
) -> io::Result<PathBuf>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("path component is not a directory: {}", ancestor.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let component = ancestor.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("directory has no existing ancestor: {}", path.display()),
                    )
                })?;
                missing.push(component.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("directory has no existing ancestor: {}", path.display()),
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }

    let mut directory = fs::canonicalize(ancestor)?;
    let mut created = Vec::new();
    for component in missing.into_iter().rev() {
        directory.push(component);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&directory)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "new evaluation output component was replaced: {}",
                            directory.display()
                        ),
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        validate_created_directory(&directory)?;
        created.push(directory.clone());
    }
    for parent in directory.ancestors().skip(1) {
        validate_created_directories(&created)?;
        sync_directory(parent)?;
        validate_created_directories(&created)?;
    }
    Ok(directory)
}

fn validate_created_directories(directories: &[PathBuf]) -> io::Result<()> {
    for directory in directories {
        validate_created_directory(directory)?;
    }
    Ok(())
}

fn validate_created_directory(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "new evaluation output component was replaced: {}",
            directory.display()
        ),
    ))
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "durable evaluation jobs require directory fsync support: {}",
            path.display()
        ),
    ))
}

#[cfg(unix)]
const fn require_durable_directory_sync() -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn require_durable_directory_sync() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable evaluation jobs require directory fsync support",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, io};

    use tempfile::tempdir;

    use super::{EvalJob, create_durable_directory_all_with_sync};

    #[test]
    fn creates_a_unique_attempt_directory() {
        let output = tempdir().unwrap();
        let job = EvalJob::create(&output.path().join("nested")).unwrap();
        assert!(job.directory().is_dir());
        assert_eq!(
            job.parent_directory(),
            std::fs::canonicalize(output.path()).unwrap().join("nested")
        );
    }

    #[test]
    fn retries_sync_for_an_existing_directory() {
        let output = tempdir().unwrap();
        let target = output.path().join("created-before-sync-failure");
        let error = create_durable_directory_all_with_sync(&target, |_| {
            Err(io::Error::other("injected sync failure"))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);

        let mut synced = Vec::new();
        create_durable_directory_all_with_sync(&target, |directory| {
            synced.push(directory.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            synced.first(),
            Some(&fs::canonicalize(output.path()).unwrap())
        );
    }

    #[test]
    fn rejects_a_created_component_replaced_during_sync() {
        let trusted = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = trusted.path().join("new-output");
        let mut substituted = false;

        let error = create_durable_directory_all_with_sync(&target, |_| {
            if !substituted {
                fs::remove_dir(&target)?;
                std::os::unix::fs::symlink(outside.path(), &target)?;
                substituted = true;
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
