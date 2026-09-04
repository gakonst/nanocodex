use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

use filetime::FileTime;
use sha2::{Digest, Sha256};

const PACKAGE_FILES: [&str; 5] = [
    "task.toml",
    "instruction.md",
    "transcript.json",
    "README.md",
    "pre_artifacts.sh",
];
const PACKAGE_DIRECTORIES: [&str; 4] = ["environment", "tests", "solution", "steps"];
const PACKAGE_DIGEST_DOMAIN: &[u8] = b"nanocodex-task-package-v1\0";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct TaskPackage {
    root: PathBuf,
    entries: Vec<TaskPackageEntry>,
    digest: String,
}

#[derive(Debug, Eq, PartialEq)]
struct TaskPackageEntry {
    relative: PathBuf,
    normalized: String,
    mode: u32,
    kind: TaskPackageEntryKind,
}

#[derive(Debug, Eq, PartialEq)]
enum TaskPackageEntryKind {
    Directory,
    File { bytes: u64, digest: [u8; 32] },
    Symlink { target: PathBuf },
}

impl TaskPackage {
    pub(crate) fn load(root: &Path) -> io::Result<Self> {
        let mut entries = Vec::new();
        for name in PACKAGE_FILES {
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(_) => collect_package_entry(root, &path, &mut entries)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        for name in PACKAGE_DIRECTORIES {
            let directory = root.join(name);
            let metadata = match fs::symlink_metadata(&directory) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "task package root must be a directory, not a symlink or file: {}",
                        directory.display()
                    ),
                ));
            }
            collect_package_entry(root, &directory, &mut entries)?;
        }
        entries.sort_by(|left, right| left.normalized.cmp(&right.normalized));
        let digest = package_digest(&entries);
        Ok(Self {
            root: root.to_path_buf(),
            entries,
            digest,
        })
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    pub(crate) const fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn file_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                TaskPackageEntryKind::File { bytes, .. } => Some(*bytes),
                TaskPackageEntryKind::Directory | TaskPackageEntryKind::Symlink { .. } => None,
            })
            .fold(0_u64, u64::saturating_add)
    }

    pub(crate) fn read_file(&self, relative: &Path) -> io::Result<Option<Vec<u8>>> {
        let Some((bytes, digest)) = self
            .entries
            .iter()
            .find(|entry| entry.relative == relative)
            .and_then(|entry| match &entry.kind {
                TaskPackageEntryKind::File { bytes, digest } => Some((*bytes, *digest)),
                TaskPackageEntryKind::Directory | TaskPackageEntryKind::Symlink { .. } => None,
            })
        else {
            return Ok(None);
        };
        let source = self.root.join(relative);
        let contents = fs::read(&source)?;
        validate_file_contents(&source, bytes, digest, &contents)?;
        Ok(Some(contents))
    }

    pub(crate) fn contains_directory(&self, relative: &Path) -> bool {
        self.entries.iter().any(|entry| {
            entry.relative == relative && matches!(&entry.kind, TaskPackageEntryKind::Directory)
        })
    }

    pub(crate) fn materialize_directory(
        &self,
        package_directory: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        let mut found = false;
        let mut directory_modes = Vec::new();
        for entry in &self.entries {
            let Ok(relative) = entry.relative.strip_prefix(package_directory) else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                found = matches!(&entry.kind, TaskPackageEntryKind::Directory);
                if found {
                    directory_modes.push((destination.to_path_buf(), entry.mode));
                }
                continue;
            }
            let target = destination.join(relative);
            match &entry.kind {
                TaskPackageEntryKind::Directory => {
                    fs::create_dir(&target)?;
                    directory_modes.push((target, entry.mode));
                }
                TaskPackageEntryKind::File { bytes, digest } => {
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)?;
                    copy_file_verified(
                        &self.root.join(&entry.relative),
                        &mut file,
                        *bytes,
                        *digest,
                    )?;
                    set_mode(&target, entry.mode)?;
                    normalize_times(&target)?;
                }
                TaskPackageEntryKind::Symlink {
                    target: link_target,
                } => std::os::unix::fs::symlink(link_target, &target)?,
            }
        }
        if !found {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "task package has no directory {}",
                    package_directory.display()
                ),
            ));
        }
        for (directory, mode) in directory_modes.into_iter().rev() {
            set_mode(&directory, mode)?;
            normalize_times(&directory)?;
        }
        Ok(())
    }

    pub(crate) fn materialize(&self, destination: &Path) -> io::Result<()> {
        fs::create_dir(destination)?;
        let mut directory_modes = vec![(destination.to_path_buf(), 0o755)];
        for entry in &self.entries {
            let target = destination.join(&entry.relative);
            match &entry.kind {
                TaskPackageEntryKind::Directory => {
                    fs::create_dir(&target)?;
                    directory_modes.push((target, entry.mode));
                }
                TaskPackageEntryKind::File { bytes, digest } => {
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&target)?;
                    copy_file_verified(
                        &self.root.join(&entry.relative),
                        &mut file,
                        *bytes,
                        *digest,
                    )?;
                    set_mode(&target, entry.mode)?;
                    normalize_times(&target)?;
                }
                TaskPackageEntryKind::Symlink {
                    target: link_target,
                } => std::os::unix::fs::symlink(link_target, &target)?,
            }
        }
        for (directory, mode) in directory_modes.into_iter().rev() {
            set_mode(&directory, mode)?;
            normalize_times(&directory)?;
        }
        Ok(())
    }
}

fn collect_package_entry(
    root: &Path,
    path: &Path,
    entries: &mut Vec<TaskPackageEntry>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let relative = path
        .strip_prefix(root)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        .to_path_buf();
    let normalized = normalized_relative_path(&relative)?;
    let mode = metadata_mode(&metadata);
    let kind = if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        validate_symlink_target(root, path, &target)?;
        TaskPackageEntryKind::Symlink { target }
    } else if metadata.is_dir() {
        TaskPackageEntryKind::Directory
    } else if metadata.is_file() {
        let (bytes, digest) = hash_file(path)?;
        TaskPackageEntryKind::File { bytes, digest }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported task package entry: {}", path.display()),
        ));
    };
    entries.push(TaskPackageEntry {
        relative,
        normalized,
        mode,
        kind,
    });

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            collect_package_entry(root, &entry?.path(), entries)?;
        }
    }
    Ok(())
}

fn package_digest(entries: &[TaskPackageEntry]) -> String {
    let mut digest = Sha256::new();
    digest.update(PACKAGE_DIGEST_DOMAIN);
    for entry in entries {
        update_field(&mut digest, entry.normalized.as_bytes());
        digest.update(entry.mode.to_le_bytes());
        match &entry.kind {
            TaskPackageEntryKind::Directory => digest.update(b"d"),
            TaskPackageEntryKind::Symlink { target } => {
                digest.update(b"l");
                update_field(&mut digest, target.as_os_str().as_encoded_bytes());
            }
            TaskPackageEntryKind::File {
                bytes,
                digest: file,
            } => {
                digest.update(b"f");
                digest.update(bytes.to_le_bytes());
                digest.update(file);
            }
        }
    }
    hex::encode(digest.finalize())
}

fn validate_symlink_target(root: &Path, path: &Path, target: &Path) -> io::Result<()> {
    if target.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "task package symlink target must be relative: {} -> {}",
                path.display(),
                target.display()
            ),
        ));
    }
    let resolved = fs::canonicalize(path)?;
    let source_relative = path.strip_prefix(root).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("task package symlink is outside its root: {error}"),
        )
    })?;
    let resolved_relative = resolved.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "task package symlink escapes its root: {} -> {}",
                path.display(),
                target.display()
            ),
        )
    })?;
    if source_relative.components().next() != resolved_relative.components().next() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "task package symlink crosses execution-input boundaries: {} -> {}",
                path.display(),
                target.display()
            ),
        ));
    }
    Ok(())
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(bytes);
}

fn hash_file(path: &Path) -> io::Result<(u64, [u8; 32])> {
    let mut file = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        digest.update(&buffer[..read]);
    }
    Ok((bytes, digest.finalize().into()))
}

fn copy_file_verified(
    source: &Path,
    destination: &mut File,
    expected_bytes: u64,
    expected_digest: [u8; 32],
) -> io::Result<()> {
    let mut source_file = BufReader::new(File::open(source)?);
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source_file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read])?;
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        digest.update(&buffer[..read]);
    }
    validate_file_identity(
        source,
        expected_bytes,
        expected_digest,
        bytes,
        digest.finalize().into(),
    )
}

fn validate_file_contents(
    path: &Path,
    expected_bytes: u64,
    expected_digest: [u8; 32],
    contents: &[u8],
) -> io::Result<()> {
    validate_file_identity(
        path,
        expected_bytes,
        expected_digest,
        u64::try_from(contents.len()).unwrap_or(u64::MAX),
        Sha256::digest(contents).into(),
    )
}

fn validate_file_identity(
    path: &Path,
    expected_bytes: u64,
    expected_digest: [u8; 32],
    actual_bytes: u64,
    actual_digest: [u8; 32],
) -> io::Result<()> {
    if actual_bytes == expected_bytes && actual_digest == expected_digest {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "task package file changed while being read: {}",
            path.display()
        ),
    ))
}

fn normalized_relative_path(path: &Path) -> io::Result<String> {
    let mut normalized = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("task package path is not normalized: {}", path.display()),
            ));
        };
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("task package path is not UTF-8: {}", path.display()),
            )
        })?);
    }
    Ok(normalized)
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn normalize_times(path: &Path) -> io::Result<()> {
    let epoch = FileTime::from_unix_time(0, 0);
    filetime::set_file_times(path, epoch, epoch)
}

#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::TaskPackage;

    #[test]
    fn package_digest_is_deterministic_for_the_fixture() {
        let task = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks/write-greeting");
        let first = TaskPackage::load(&task).unwrap();
        let second = TaskPackage::load(&task).unwrap();

        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.digest().len(), 64);
    }

    #[test]
    fn ignored_files_are_still_execution_inputs() {
        let task = package();
        fs::write(task.path().join(".gitignore"), "environment/ignored\n").unwrap();
        let ignored = task.path().join("environment/ignored");
        fs::write(&ignored, "first\n").unwrap();
        let first = TaskPackage::load(task.path()).unwrap();

        fs::write(ignored, "second\n").unwrap();
        let second = TaskPackage::load(task.path()).unwrap();

        assert_ne!(first.digest(), second.digest());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_absolute_symlinks_in_the_task_package() {
        let task = package();
        let link = task.path().join("environment/current");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();

        let error = TaskPackage::load(task.path()).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("symlink target must be relative")
        );
        assert!(error.to_string().contains("/etc/passwd"));
    }

    #[cfg(unix)]
    #[test]
    fn executable_modes_are_execution_inputs() {
        use std::os::unix::fs::PermissionsExt as _;

        let task = package();
        let executable = task.path().join("environment/run");
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
        let first = TaskPackage::load(task.path()).unwrap();

        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let second = TaskPackage::load(task.path()).unwrap();

        assert_ne!(first.digest(), second.digest());
    }

    #[cfg(unix)]
    #[test]
    fn package_materialization_preserves_files_directories_and_modes() {
        use std::os::unix::fs::PermissionsExt as _;

        let task = package();
        fs::set_permissions(
            task.path().join("environment"),
            fs::Permissions::from_mode(0o711),
        )
        .unwrap();
        let nested = task.path().join("environment/bin");
        fs::create_dir(&nested).unwrap();
        let executable = nested.join("run");
        fs::write(&executable, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let package = TaskPackage::load(task.path()).unwrap();
        let destination = tempdir().unwrap();

        package
            .materialize_directory(Path::new("environment"), destination.path())
            .unwrap();

        assert_eq!(
            fs::read_to_string(destination.path().join("bin/run")).unwrap(),
            "#!/bin/sh\n"
        );
        assert_eq!(
            fs::metadata(destination.path().join("bin/run"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(destination.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o711
        );
        assert_eq!(
            fs::metadata(destination.path().join("bin/run"))
                .unwrap()
                .modified()
                .unwrap(),
            std::time::UNIX_EPOCH
        );
        assert_eq!(
            fs::metadata(destination.path())
                .unwrap()
                .modified()
                .unwrap(),
            std::time::UNIX_EPOCH
        );
    }

    fn package() -> tempfile::TempDir {
        let task = tempdir().unwrap();
        fs::create_dir(task.path().join("environment")).unwrap();
        fs::create_dir(task.path().join("tests")).unwrap();
        fs::write(task.path().join("task.toml"), "schema_version = \"1.1\"\n").unwrap();
        fs::write(task.path().join("instruction.md"), "Do the work.\n").unwrap();
        task
    }
}
