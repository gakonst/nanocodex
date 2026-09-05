use std::{fs, io, os::unix::fs::PermissionsExt as _, path::Path};

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

/// Copies a disk with reflink semantics when available and a sparse fallback.
///
/// The destination must not exist. The copy is materialized under a private
/// temporary directory next to the destination and published without replacing
/// any path that appeared concurrently. A failed attempt never modifies the
/// destination or leaves a partial disk there.
///
/// # Errors
///
/// Returns an I/O error when the source cannot be read, the destination
/// cannot be created, or neither reflink nor sparse copying succeeds.
pub fn reflink_or_sparse_copy(source: &Path, destination: &Path) -> io::Result<u64> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("disk copy destination has no parent directory"))?;
    let temporary_directory = tempfile::Builder::new()
        .prefix(".nanocodex-disk-copy.")
        .tempdir_in(parent)?;
    let temporary = temporary_directory.path().join("disk.ext4");
    copy_into_owned_path(source, &temporary)?;
    let metadata = fs::metadata(&temporary)?;
    let bytes = metadata.len();
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&temporary, permissions)?;
    fs::hard_link(&temporary, destination)?;
    Ok(bytes)
}

fn copy_into_owned_path(source: &Path, destination: &Path) -> io::Result<()> {
    match reflink_copy::reflink(source, destination) {
        Ok(()) => return Ok(()),
        Err(_) => remove_partial_copy(destination)?,
    }

    #[cfg(target_os = "linux")]
    {
        let status = Command::new("cp")
            .args(["--reflink=never", "--sparse=always", "--"])
            .arg(source)
            .arg(destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            return Ok(());
        }
        remove_partial_copy(destination)?;
        Err(io::Error::other(format!(
            "sparse disk copy failed with {status}"
        )))
    }

    #[cfg(not(target_os = "linux"))]
    fs::copy(source, destination).map(|_| ())
}

fn remove_partial_copy(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    #[cfg(target_os = "linux")]
    use std::{
        fs::File,
        io::{Seek, SeekFrom, Write},
        os::unix::fs::MetadataExt as _,
    };

    use super::reflink_or_sparse_copy;

    #[cfg(target_os = "linux")]
    #[test]
    fn fallback_preserves_sparse_disk_allocation() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.ext4");
        let destination = directory.path().join("destination.ext4");
        let mut file = File::create(&source).unwrap();
        file.seek(SeekFrom::Start(64 * 1024 * 1024)).unwrap();
        file.write_all(b"disk-tail").unwrap();
        drop(file);

        reflink_or_sparse_copy(&source, &destination).unwrap();

        let source_metadata = fs::metadata(&source).unwrap();
        let destination_metadata = fs::metadata(&destination).unwrap();
        assert_eq!(destination_metadata.len(), source_metadata.len());
        assert!(destination_metadata.blocks().saturating_mul(512) < destination_metadata.len() / 4);
    }

    #[test]
    fn existing_destination_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.ext4");
        let destination = directory.path().join("destination.ext4");
        fs::write(&source, b"new disk").unwrap();
        fs::write(&destination, b"existing disk").unwrap();

        let error = reflink_or_sparse_copy(&source, &destination).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), b"existing disk");
    }

    #[test]
    fn published_disk_is_private_and_writable() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.ext4");
        let destination = directory.path().join("destination.ext4");
        fs::write(&source, b"disk").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();

        reflink_or_sparse_copy(&source, &destination).unwrap();

        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
