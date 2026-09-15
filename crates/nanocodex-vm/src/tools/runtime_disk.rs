use std::{
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};

use arcbox_ext4::{
    Formatter, Reader,
    constants::{file_mode, make_mode},
    error::{FormatError, ReadError},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, info_span};

const BLOCK_SIZE: u32 = 4_096;
const DISK_BYTES: u64 = 128 * 1024 * 1024;
const GUEST_PATH: &str = "/nanocodex-vm-guest";
const RUNTIME_ROOT_DIRECTORIES: [&str; 10] = [
    "/dev",
    "/dev/pts",
    "/dev/shm",
    "/proc",
    "/sys",
    "/sys/fs",
    "/sys/fs/cgroup",
    "/mnt",
    "/run",
    "/tmp",
];
const IDENTITY_VERSION: &[u8] = b"nanocodex-vm-guest-runtime-v2-overlay-root\0";
const RECORD_VERSION: u32 = 1;

/// Whether preparing a guest runtime disk reused or created its cache entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestRuntimeDiskStatus {
    /// A validated content-addressed disk already existed.
    Hit,
    /// This call formatted and atomically published the disk.
    Created,
}

impl GuestRuntimeDiskStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Created => "created",
        }
    }
}

/// A content-addressed ext4 disk containing the Nanocodex VM guest runtime.
///
/// The disk remains in the caller-selected cache after this value is dropped,
/// so it can be mounted read-only by many VM attempts. Clones only copy the
/// path, digest, and preparation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestRuntimeDisk {
    path: PathBuf,
    digest: String,
    status: GuestRuntimeDiskStatus,
}

impl GuestRuntimeDisk {
    /// Stages a Linux guest ELF into a reusable ext4 disk.
    ///
    /// `cache` is the VM cache root, not its `runtimes` subdirectory. For
    /// example:
    ///
    /// ```no_run
    /// use nanocodex_vm::tools::{GuestRuntimeDisk, GuestRuntimeDiskStatus};
    ///
    /// # fn prepare() -> Result<(), Box<dyn std::error::Error>> {
    /// let runtime = GuestRuntimeDisk::prepare(
    ///     "target/aarch64-unknown-linux-musl/debug/nanocodex-vm-guest",
    ///     ".cache/vm",
    /// )?;
    /// assert!(matches!(
    ///     runtime.status(),
    ///     GuestRuntimeDiskStatus::Hit | GuestRuntimeDiskStatus::Created
    /// ));
    /// let read_only_ext4 = runtime.path();
    /// # let _ = read_only_ext4;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Equal binary bytes produce the same SHA-256 digest and cache path. A
    /// healthy warm call validates an atomic size/mtime record rather than
    /// rereading the binary or opening ext4. A changed source, disk, or record
    /// falls back to a complete byte-for-byte validation. Concurrent callers
    /// serialize on a per-digest filesystem lock and publish through unique
    /// temporary files. The caller-selected cache root and every managed
    /// descendant must be real directories or regular files; descendant
    /// symlinks are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error if the binary cannot be read, is not an ELF, the cache
    /// cannot be accessed, or a new ext4 disk cannot be formatted or validated.
    pub fn prepare(
        binary: impl AsRef<Path>,
        cache: impl AsRef<Path>,
    ) -> Result<Self, GuestRuntimeDiskError> {
        let binary = binary.as_ref();
        let cache = cache.as_ref();
        let span = info_span!(
            target: "nanocodex_vm",
            "vm.guest_runtime.prepare",
            otel.kind = "internal",
            otel.status_code = tracing::field::Empty,
            runtime.binary.path = %binary.display(),
            runtime.binary.bytes = tracing::field::Empty,
            runtime.cache.path = %cache.display(),
            runtime.digest = tracing::field::Empty,
            runtime.cache.status = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
            status = tracing::field::Empty,
            error.message = tracing::field::Empty,
        );
        let started = Instant::now();
        let result = span.in_scope(|| Self::prepare_inner(binary, cache));
        span.record("duration_ms", started.elapsed().as_secs_f64() * 1_000.0);
        match &result {
            Ok(runtime) => {
                span.record("otel.status_code", "OK");
                span.record("status", "completed");
                span.record("runtime.digest", runtime.digest());
                span.record("runtime.cache.status", runtime.status().as_str());
                if let Ok(metadata) = fs::metadata(binary) {
                    span.record("runtime.binary.bytes", metadata.len());
                }
            }
            Err(error) => {
                span.record("otel.status_code", "ERROR");
                span.record("status", "failed");
                span.record("error.message", error.to_string());
            }
        }
        result
    }

    /// Returns the prepared ext4 disk path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the lowercase SHA-256 cache identity.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Returns whether this call reused or created the disk.
    #[must_use]
    pub const fn status(&self) -> GuestRuntimeDiskStatus {
        self.status
    }

    fn prepare_inner(binary: &Path, cache: &Path) -> Result<Self, GuestRuntimeDiskError> {
        let binary =
            fs::canonicalize(binary).map_err(|source| GuestRuntimeDiskError::ReadBinary {
                path: binary.to_path_buf(),
                source,
            })?;
        let cache = RuntimeCache::open(cache)?;
        let source_snapshot = binary_snapshot(&binary)?;
        let record_path = runtime_record_path(&cache, &binary)?;
        if let Some((digest, path)) = recorded_runtime_disk(&record_path, source_snapshot, &cache)?
        {
            return Ok(Self {
                path,
                digest,
                status: GuestRuntimeDiskStatus::Hit,
            });
        }

        let bytes = fs::read(&binary).map_err(|source| GuestRuntimeDiskError::ReadBinary {
            path: binary.clone(),
            source,
        })?;
        if !bytes.starts_with(b"\x7fELF") {
            return Err(GuestRuntimeDiskError::NotElf(binary));
        }
        if binary_snapshot(&binary)? != source_snapshot {
            return Err(GuestRuntimeDiskError::BinaryChanged(binary));
        }

        let digest = runtime_digest(&bytes);
        let directory = cache.directory(Path::new("runtimes").join(&digest))?;
        let path = directory.join("runtime.ext4");
        if valid_cached_disk(&path, &bytes)? {
            write_runtime_record(&record_path, source_snapshot, &digest, &path)?;
            return Ok(Self {
                path,
                digest,
                status: GuestRuntimeDiskStatus::Hit,
            });
        }

        let _lock = CacheLock::acquire(&cache, &digest)?;
        if valid_cached_disk(&path, &bytes)? {
            write_runtime_record(&record_path, source_snapshot, &digest, &path)?;
            return Ok(Self {
                path,
                digest,
                status: GuestRuntimeDiskStatus::Hit,
            });
        }

        let temporary = tempfile::Builder::new()
            .prefix(".runtime.")
            .tempfile_in(&directory)
            .map_err(|source| cache_error(directory.clone(), source))?
            .into_temp_path();
        let mut contents = bytes.as_slice();
        let mut formatter = Formatter::new(&temporary, BLOCK_SIZE, DISK_BYTES)?;
        for directory in RUNTIME_ROOT_DIRECTORIES {
            formatter.create(
                directory,
                make_mode(file_mode::S_IFDIR, 0o755),
                None,
                None,
                None,
                Some(0),
                Some(0),
                None,
            )?;
        }
        formatter.create(
            GUEST_PATH,
            make_mode(file_mode::S_IFREG, 0o755),
            None,
            None,
            Some(&mut contents),
            Some(0),
            Some(0),
            None,
        )?;
        formatter.close()?;
        validate_prepared_disk(&temporary, &bytes)?;
        temporary
            .persist(&path)
            .map_err(|error| cache_error(path.clone(), error.error))?;
        write_runtime_record(&record_path, source_snapshot, &digest, &path)?;

        Ok(Self {
            path,
            digest,
            status: GuestRuntimeDiskStatus::Created,
        })
    }
}

/// Failure while preparing a content-addressed VM guest runtime disk.
#[derive(Debug, thiserror::Error)]
pub enum GuestRuntimeDiskError {
    /// The guest runtime binary could not be read.
    #[error("failed to read VM guest runtime binary {}", path.display())]
    ReadBinary {
        /// Binary path supplied by the caller.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// The runtime binary changed while it was being indexed.
    #[error("VM guest runtime binary {} changed while it was being indexed", .0.display())]
    BinaryChanged(PathBuf),
    /// The supplied runtime is not a Linux ELF executable.
    #[error("VM guest runtime {} is not an ELF executable", .0.display())]
    NotElf(PathBuf),
    /// A cache directory, lock, temporary file, or publication failed.
    #[error("failed to access VM guest runtime cache at {}", path.display())]
    Cache {
        /// Cache path being accessed.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Formatting the ext4 disk failed.
    #[error("failed to format VM guest runtime disk")]
    Format(#[from] FormatError),
    /// A newly formatted ext4 disk did not contain the expected runtime.
    #[error("prepared VM guest runtime disk {} failed validation", path.display())]
    InvalidPreparedDisk {
        /// Temporary disk path that failed validation.
        path: PathBuf,
        /// Underlying ext4 read error, when one was available.
        #[source]
        source: Option<ReadError>,
    },
}

struct RuntimeCache {
    root: PathBuf,
}

impl RuntimeCache {
    fn open(root: &Path) -> Result<Self, GuestRuntimeDiskError> {
        fs::create_dir_all(root).map_err(|source| cache_error(root.to_path_buf(), source))?;
        ensure_cache_directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn directory(&self, relative: impl AsRef<Path>) -> Result<PathBuf, GuestRuntimeDiskError> {
        let relative = relative.as_ref();
        let mut directory = self.root.clone();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(cache_error(
                    self.root.join(relative),
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "runtime cache path contains a non-normal component",
                    ),
                ));
            };
            directory.push(component);
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(cache_error(directory, source)),
            }
            ensure_cache_directory(&directory)?;
        }
        Ok(directory)
    }
}

fn ensure_cache_directory(path: &Path) -> Result<(), GuestRuntimeDiskError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| cache_error(path.to_path_buf(), source))?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(cache_error(
            path.to_path_buf(),
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime cache path component is not a directory",
            ),
        ))
    }
}

fn cache_file_metadata(path: &Path) -> Result<Option<fs::Metadata>, GuestRuntimeDiskError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(metadata)),
        Ok(_) => Err(cache_error(
            path.to_path_buf(),
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime cache file is not a regular file",
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(cache_error(path.to_path_buf(), source)),
    }
}

fn ensure_cache_file(path: &Path) -> Result<(), GuestRuntimeDiskError> {
    cache_file_metadata(path)?.map_or_else(
        || {
            Err(cache_error(
                path.to_path_buf(),
                io::Error::new(io::ErrorKind::NotFound, "runtime cache file does not exist"),
            ))
        },
        |_| Ok(()),
    )
}

struct CacheLock(File);

impl CacheLock {
    fn acquire(cache: &RuntimeCache, digest: &str) -> Result<Self, GuestRuntimeDiskError> {
        let directory = cache.directory("locks/runtimes")?;
        let path = directory.join(format!("{digest}.lock"));
        let file = open_cache_lock(&path)?;
        fs2::FileExt::lock_exclusive(&file).map_err(|source| cache_error(path.clone(), source))?;
        Ok(Self(file))
    }
}

fn open_cache_lock(path: &Path) -> Result<File, GuestRuntimeDiskError> {
    match fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            ensure_cache_file(path)?;
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|source| cache_error(path.to_path_buf(), source))
        }
        Err(source) => Err(cache_error(path.to_path_buf(), source)),
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn runtime_digest(bytes: &[u8]) -> String {
    let mut identity = Sha256::new();
    identity.update(IDENTITY_VERSION);
    identity.update(bytes);
    hex::encode(identity.finalize())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshot {
    bytes: u64,
    modified_unix_ns: u64,
}

impl FileSnapshot {
    fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?;
        Ok(Self {
            bytes: metadata.len(),
            modified_unix_ns: u64::try_from(modified.as_nanos()).map_err(io::Error::other)?,
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GuestRuntimeRecord {
    version: u32,
    binary_bytes: u64,
    binary_modified_unix_ns: u64,
    digest: String,
    disk_bytes: u64,
    disk_modified_unix_ns: u64,
}

fn binary_snapshot(path: &Path) -> Result<FileSnapshot, GuestRuntimeDiskError> {
    let metadata = fs::metadata(path).map_err(|source| GuestRuntimeDiskError::ReadBinary {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(GuestRuntimeDiskError::ReadBinary {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "runtime binary is not a file"),
        });
    }
    FileSnapshot::from_metadata(&metadata).map_err(|source| GuestRuntimeDiskError::ReadBinary {
        path: path.to_path_buf(),
        source,
    })
}

fn runtime_record_path(
    cache: &RuntimeCache,
    binary: &Path,
) -> Result<PathBuf, GuestRuntimeDiskError> {
    let mut identity = Sha256::new();
    identity.update(b"nanocodex-vm-runtime-record-v1\0");
    identity.update(binary.as_os_str().as_encoded_bytes());
    Ok(cache
        .directory("runtime-records")?
        .join(format!("{}.json", hex::encode(identity.finalize()))))
}

fn recorded_runtime_disk(
    record_path: &Path,
    source: FileSnapshot,
    cache: &RuntimeCache,
) -> Result<Option<(String, PathBuf)>, GuestRuntimeDiskError> {
    if cache_file_metadata(record_path)?.is_none() {
        return Ok(None);
    }
    let contents = match fs::read(record_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(cache_error(record_path.to_path_buf(), source)),
    };
    let record = match serde_json::from_slice::<GuestRuntimeRecord>(&contents) {
        Ok(record) => record,
        Err(error) => {
            info!(
                target: "nanocodex_vm",
                cache_record_path = %record_path.display(),
                error = %error,
                "ignoring invalid VM guest runtime cache record"
            );
            return Ok(None);
        }
    };
    if record.version != RECORD_VERSION
        || record.binary_bytes != source.bytes
        || record.binary_modified_unix_ns != source.modified_unix_ns
        || !is_sha256_digest(&record.digest)
        || record.disk_bytes != DISK_BYTES
    {
        return Ok(None);
    }

    let path = cache
        .directory(Path::new("runtimes").join(&record.digest))?
        .join("runtime.ext4");
    let Some(metadata) = cache_file_metadata(&path)? else {
        return Ok(None);
    };
    let disk = FileSnapshot::from_metadata(&metadata)
        .map_err(|source| cache_error(path.clone(), source))?;
    if disk.bytes != record.disk_bytes || disk.modified_unix_ns != record.disk_modified_unix_ns {
        return Ok(None);
    }
    Ok(Some((record.digest, path)))
}

fn write_runtime_record(
    record_path: &Path,
    source: FileSnapshot,
    digest: &str,
    disk_path: &Path,
) -> Result<(), GuestRuntimeDiskError> {
    let metadata =
        fs::metadata(disk_path).map_err(|source| cache_error(disk_path.to_path_buf(), source))?;
    let disk = FileSnapshot::from_metadata(&metadata)
        .map_err(|source| cache_error(disk_path.to_path_buf(), source))?;
    let record = GuestRuntimeRecord {
        version: RECORD_VERSION,
        binary_bytes: source.bytes,
        binary_modified_unix_ns: source.modified_unix_ns,
        digest: digest.to_owned(),
        disk_bytes: disk.bytes,
        disk_modified_unix_ns: disk.modified_unix_ns,
    };
    let directory = record_path
        .parent()
        .ok_or_else(|| {
            cache_error(
                record_path.to_path_buf(),
                io::Error::other("runtime cache record has no parent directory"),
            )
        })?
        .to_path_buf();
    ensure_cache_directory(&directory)?;
    let _ = cache_file_metadata(record_path)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".runtime-record.")
        .tempfile_in(&directory)
        .map_err(|source| cache_error(directory, source))?;
    serde_json::to_writer(temporary.as_file_mut(), &record)
        .map_err(|source| cache_error(record_path.to_path_buf(), io::Error::other(source)))?;
    temporary
        .into_temp_path()
        .persist(record_path)
        .map_err(|error| cache_error(record_path.to_path_buf(), error.error))?;
    Ok(())
}

fn is_sha256_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_cached_disk(path: &Path, binary: &[u8]) -> Result<bool, GuestRuntimeDiskError> {
    let Some(metadata) = cache_file_metadata(path)? else {
        return Ok(false);
    };
    if metadata.len() != DISK_BYTES {
        return Ok(false);
    }
    let Ok(mut reader) = Reader::new(path) else {
        return Ok(false);
    };
    if RUNTIME_ROOT_DIRECTORIES.iter().any(|directory| {
        !reader
            .stat(directory)
            .is_ok_and(|(_, inode)| inode.is_dir())
    }) {
        return Ok(false);
    }
    let Ok((_, inode)) = reader.stat(GUEST_PATH) else {
        return Ok(false);
    };
    if !inode.is_reg() || inode.file_size() != binary.len() as u64 || inode.mode & 0o777 != 0o755 {
        return Ok(false);
    }
    Ok(reader
        .read_file(GUEST_PATH, 0, None)
        .is_ok_and(|cached| cached == binary))
}

fn validate_prepared_disk(path: &Path, binary: &[u8]) -> Result<(), GuestRuntimeDiskError> {
    let metadata = fs::metadata(path).map_err(|source| cache_error(path.to_path_buf(), source))?;
    if !metadata.is_file() || metadata.len() != DISK_BYTES {
        return Err(GuestRuntimeDiskError::InvalidPreparedDisk {
            path: path.to_path_buf(),
            source: None,
        });
    }
    let mut reader =
        Reader::new(path).map_err(|source| GuestRuntimeDiskError::InvalidPreparedDisk {
            path: path.to_path_buf(),
            source: Some(source),
        })?;
    if RUNTIME_ROOT_DIRECTORIES.iter().any(|directory| {
        !reader
            .stat(directory)
            .is_ok_and(|(_, inode)| inode.is_dir())
    }) {
        return Err(GuestRuntimeDiskError::InvalidPreparedDisk {
            path: path.to_path_buf(),
            source: None,
        });
    }
    let (_, inode) =
        reader
            .stat(GUEST_PATH)
            .map_err(|source| GuestRuntimeDiskError::InvalidPreparedDisk {
                path: path.to_path_buf(),
                source: Some(source),
            })?;
    if !inode.is_reg() || inode.file_size() != binary.len() as u64 || inode.mode & 0o777 != 0o755 {
        return Err(GuestRuntimeDiskError::InvalidPreparedDisk {
            path: path.to_path_buf(),
            source: None,
        });
    }
    let contents = reader.read_file(GUEST_PATH, 0, None).map_err(|source| {
        GuestRuntimeDiskError::InvalidPreparedDisk {
            path: path.to_path_buf(),
            source: Some(source),
        }
    })?;
    if contents != binary {
        return Err(GuestRuntimeDiskError::InvalidPreparedDisk {
            path: path.to_path_buf(),
            source: None,
        });
    }
    Ok(())
}

const fn cache_error(path: PathBuf, source: io::Error) -> GuestRuntimeDiskError {
    GuestRuntimeDiskError::Cache { path, source }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use arcbox_ext4::Reader;

    use super::{
        GUEST_PATH, GuestRuntimeDisk, GuestRuntimeDiskStatus, RUNTIME_ROOT_DIRECTORIES,
        runtime_digest,
    };

    #[test]
    fn prepares_valid_content_addressed_disk_and_reuses_it() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("nanocodex-vm-guest");
        let bytes = b"\x7fELF deterministic guest runtime";
        fs::write(&binary, bytes).unwrap();
        let cache = directory.path().join("cache");

        let created = GuestRuntimeDisk::prepare(&binary, &cache).unwrap();
        assert_eq!(created.status(), GuestRuntimeDiskStatus::Created);
        assert_eq!(created.digest(), runtime_digest(bytes));
        assert_eq!(
            created.path(),
            cache
                .join("runtimes")
                .join(created.digest())
                .join("runtime.ext4")
        );

        let mut reader = Reader::new(created.path()).unwrap();
        for directory in RUNTIME_ROOT_DIRECTORIES {
            assert!(reader.stat(directory).unwrap().1.is_dir());
        }
        let (_, inode) = reader.stat(GUEST_PATH).unwrap();
        assert!(inode.is_reg());
        assert_eq!(inode.file_size(), bytes.len() as u64);
        assert_eq!(inode.mode & 0o777, 0o755);
        assert_eq!(reader.read_file(GUEST_PATH, 0, None).unwrap(), bytes);

        let reused = GuestRuntimeDisk::prepare(&binary, &cache).unwrap();
        assert_eq!(reused.status(), GuestRuntimeDiskStatus::Hit);
        assert_eq!(reused.path(), created.path());
        assert_eq!(reused.digest(), created.digest());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_nested_runtime_directory_symlinks_without_external_writes() {
        let bytes = b"\x7fELF nested runtime cache escape";
        let digest = runtime_digest(bytes);
        for relative in [
            std::path::PathBuf::from("runtimes"),
            std::path::PathBuf::from("runtimes").join(&digest),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let binary = directory.path().join("nanocodex-vm-guest");
            let cache = directory.path().join("cache");
            let outside = directory.path().join("outside");
            fs::write(&binary, bytes).unwrap();
            fs::create_dir(&cache).unwrap();
            fs::create_dir(&outside).unwrap();
            let link = cache.join(&relative);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&outside, &link).unwrap();

            let error = GuestRuntimeDisk::prepare(&binary, &cache).unwrap_err();

            assert!(error.to_string().contains(&link.display().to_string()));
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_lock_and_record_directory_symlinks_without_external_writes() {
        let bytes = b"\x7fELF nested lock and record escape";
        for relative in ["runtime-records", "locks", "locks/runtimes"] {
            let directory = tempfile::tempdir().unwrap();
            let binary = directory.path().join("nanocodex-vm-guest");
            let cache = directory.path().join("cache");
            let outside = directory.path().join("outside");
            fs::write(&binary, bytes).unwrap();
            fs::create_dir(&cache).unwrap();
            fs::create_dir(&outside).unwrap();
            let link = cache.join(relative);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&outside, &link).unwrap();

            let error = GuestRuntimeDisk::prepare(&binary, &cache).unwrap_err();

            assert!(error.to_string().contains(&link.display().to_string()));
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_managed_cache_file_symlinks_without_external_writes() {
        let bytes = b"\x7fELF managed cache file escape";
        let digest = runtime_digest(bytes);
        for managed_file in ["record", "runtime", "lock"] {
            let directory = tempfile::tempdir().unwrap();
            let binary = directory.path().join("nanocodex-vm-guest");
            let cache = directory.path().join("cache");
            let outside = directory.path().join("outside-file");
            fs::write(&binary, bytes).unwrap();
            fs::write(&outside, b"outside sentinel").unwrap();
            let path = match managed_file {
                "record" => {
                    let cache = super::RuntimeCache::open(&cache).unwrap();
                    let binary = fs::canonicalize(&binary).unwrap();
                    super::runtime_record_path(&cache, &binary).unwrap()
                }
                "runtime" => {
                    let parent = cache.join("runtimes").join(&digest);
                    fs::create_dir_all(&parent).unwrap();
                    parent.join("runtime.ext4")
                }
                "lock" => {
                    let parent = cache.join("locks/runtimes");
                    fs::create_dir_all(&parent).unwrap();
                    parent.join(format!("{digest}.lock"))
                }
                _ => unreachable!(),
            };
            std::os::unix::fs::symlink(&outside, &path).unwrap();

            let error = GuestRuntimeDisk::prepare(&binary, &cache).unwrap_err();

            assert!(error.to_string().contains(&path.display().to_string()));
            assert_eq!(fs::read(&outside).unwrap(), b"outside sentinel");
        }
    }

    #[test]
    fn repairs_an_invalid_cache_entry_under_the_same_identity() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("nanocodex-vm-guest");
        let bytes = b"\x7fELF repaired guest runtime";
        fs::write(&binary, bytes).unwrap();
        let cache = directory.path().join("cache");
        let first = GuestRuntimeDisk::prepare(&binary, &cache).unwrap();
        fs::write(first.path(), b"corrupt").unwrap();

        let repaired = GuestRuntimeDisk::prepare(&binary, &cache).unwrap();

        assert_eq!(repaired.status(), GuestRuntimeDiskStatus::Created);
        let mut reader = Reader::new(repaired.path()).unwrap();
        assert_eq!(reader.read_file(GUEST_PATH, 0, None).unwrap(), bytes);
    }

    #[test]
    fn repairs_same_sized_runtime_content_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("nanocodex-vm-guest");
        let bytes = b"\x7fELF original guest runtime";
        fs::write(&binary, bytes).unwrap();
        let cache = directory.path().join("cache");
        let first = GuestRuntimeDisk::prepare(&binary, &cache).unwrap();

        let mut replacement = bytes.to_vec();
        let last = replacement.last_mut().unwrap();
        *last ^= 1;
        let mut formatter =
            arcbox_ext4::Formatter::new(first.path(), 4_096, 128 * 1024 * 1024).unwrap();
        let mut replacement = replacement.as_slice();
        formatter
            .create(
                GUEST_PATH,
                arcbox_ext4::constants::make_mode(
                    arcbox_ext4::constants::file_mode::S_IFREG,
                    0o755,
                ),
                None,
                None,
                Some(&mut replacement),
                Some(0),
                Some(0),
                None,
            )
            .unwrap();
        formatter.close().unwrap();

        let repaired = GuestRuntimeDisk::prepare(&binary, &cache).unwrap();

        assert_eq!(repaired.status(), GuestRuntimeDiskStatus::Created);
        let mut reader = Reader::new(repaired.path()).unwrap();
        assert_eq!(reader.read_file(GUEST_PATH, 0, None).unwrap(), bytes);
    }
}
