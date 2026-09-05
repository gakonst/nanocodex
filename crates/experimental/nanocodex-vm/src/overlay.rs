#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
use std::ffi::OsString;
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
use std::os::unix::fs::PermissionsExt as _;
use std::{fs, io, path::Path};

#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
use arcbox_ext4::{
    Formatter,
    constants::{file_mode, make_mode},
    error::FormatError,
};
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
use nix::{
    errno::Errno,
    mount::{MntFlags, MsFlags, mount, umount2},
    unistd::{chdir, pivot_root},
};
use thiserror::Error;

#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
use crate::command::GuestCommand;

#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
const BLOCK_SIZE: u32 = 4_096;
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const LOWER_DEVICE: &str = "/dev/vdb";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const UPPER_DEVICE: &str = "/dev/vdc";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const STAGING_ROOT: &str = "/mnt";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const LOWER_MOUNT: &str = "/mnt/nanocodex-lower";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const UPPER_MOUNT: &str = "/mnt/nanocodex-upper";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const MERGED_MOUNT: &str = "/mnt/nanocodex-root";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const UPPER_DIRECTORY: &str = "/mnt/nanocodex-upper/upper";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const WORK_DIRECTORY: &str = "/mnt/nanocodex-upper/work";
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
const OLD_ROOT_DIRECTORY: &str = ".nanocodex-old-root";

/// Failure to create one sparse writable OverlayFS disk.
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
#[derive(Debug, Error)]
pub enum OverlayDiskError {
    /// The destination path or its parent could not be accessed.
    #[error("failed to create sparse OverlayFS disk: {0}")]
    Io(#[from] io::Error),
    /// The empty ext4 filesystem could not be formatted.
    #[error("failed to format sparse OverlayFS disk: {0}")]
    Format(#[from] FormatError),
}

/// Creates an empty sparse ext4 disk suitable for an OverlayFS upper layer.
///
/// The destination must not exist. Formatting happens through a private file
/// in the destination directory and is published atomically, so interruption
/// never exposes a partial attempt disk. The returned value is the logical
/// disk size in bytes; physical allocation remains limited to ext4 metadata
/// until the guest writes into the upper layer.
///
/// # Errors
///
/// Returns an error when the destination exists, its parent is unavailable,
/// or the ext4 image cannot be formatted and published.
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
pub fn create_sparse_overlay_disk(
    destination: impl AsRef<Path>,
    bytes: u64,
) -> Result<u64, OverlayDiskError> {
    let destination = destination.as_ref();
    if fs::symlink_metadata(destination).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("overlay disk already exists: {}", destination.display()),
        )
        .into());
    }
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "overlay disk destination has no parent directory",
        )
    })?;
    let temporary = tempfile::Builder::new()
        .prefix(".nanocodex-overlay.")
        .tempfile_in(parent)?
        .into_temp_path();
    let mut formatter = Formatter::new(&temporary, BLOCK_SIZE, bytes)?;
    for directory in ["/upper", "/work"] {
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
    formatter.close()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::hard_link(&temporary, destination)?;
    Ok(fs::metadata(destination)?.len())
}

/// Builds the initial command for a guest configured with
/// [`crate::host::VmConfig::overlay_ext4`].
///
/// The static runtime performs the mount and root transition before exposing
/// the normal typed VM-tool protocol. An empty resolver string preserves the
/// lower image's resolver configuration.
#[cfg(all(
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
pub fn overlay_guest_command(
    workspace: impl Into<OsString>,
    resolver_configuration: impl Into<OsString>,
) -> GuestCommand {
    GuestCommand::new("/nanocodex-vm-guest").args([
        OsString::from("--overlay-root"),
        workspace.into(),
        resolver_configuration.into(),
    ])
}

/// Failure while replacing the tiny guest runtime root with the attempt's
/// immutable-lower, writable-upper OverlayFS root.
#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
#[derive(Debug, Error)]
#[error("guest OverlayFS setup failed while {operation}: {source}")]
pub struct GuestOverlayError {
    operation: &'static str,
    #[source]
    source: io::Error,
}

#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
impl GuestOverlayError {
    const fn io(operation: &'static str, source: io::Error) -> Self {
        Self { operation, source }
    }

    fn errno(operation: &'static str, source: Errno) -> Self {
        Self::io(operation, io::Error::from_raw_os_error(source as i32))
    }
}

#[cfg(all(feature = "guest-runtime", target_os = "linux"))]
pub(crate) fn enter_guest_overlay_root(
    resolver_configuration: Option<&str>,
) -> Result<(), GuestOverlayError> {
    mount(
        Some("tmpfs"),
        STAGING_ROOT,
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some("mode=0755"),
    )
    .map_err(|error| GuestOverlayError::errno("mounting the staging tmpfs", error))?;
    for directory in [LOWER_MOUNT, UPPER_MOUNT, MERGED_MOUNT] {
        fs::create_dir_all(directory)
            .map_err(|error| GuestOverlayError::io("creating overlay mount points", error))?;
    }
    mount(
        Some(LOWER_DEVICE),
        LOWER_MOUNT,
        Some("ext4"),
        MsFlags::MS_RDONLY | MsFlags::MS_RELATIME,
        None::<&str>,
    )
    .map_err(|error| GuestOverlayError::errno("mounting the immutable lower disk", error))?;
    mount(
        Some(UPPER_DEVICE),
        UPPER_MOUNT,
        Some("ext4"),
        MsFlags::MS_RELATIME,
        None::<&str>,
    )
    .map_err(|error| GuestOverlayError::errno("mounting the writable upper disk", error))?;
    for directory in [UPPER_DIRECTORY, WORK_DIRECTORY] {
        fs::create_dir_all(directory)
            .map_err(|error| GuestOverlayError::io("creating upper-layer directories", error))?;
    }
    let options =
        format!("lowerdir={LOWER_MOUNT},upperdir={UPPER_DIRECTORY},workdir={WORK_DIRECTORY}");
    mount(
        Some("overlay"),
        MERGED_MOUNT,
        Some("overlay"),
        MsFlags::MS_RELATIME,
        Some(options.as_str()),
    )
    .map_err(|error| GuestOverlayError::errno("mounting the merged OverlayFS root", error))?;

    for source in ["/dev", "/proc", "/sys"] {
        let target = Path::new(MERGED_MOUNT).join(source.trim_start_matches('/'));
        fs::create_dir_all(&target)
            .map_err(|error| GuestOverlayError::io("creating virtual filesystem targets", error))?;
        mount(
            Some(source),
            &target,
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None::<&str>,
        )
        .map_err(|error| GuestOverlayError::errno("binding virtual filesystems", error))?;
    }
    let run = Path::new(MERGED_MOUNT).join("run");
    fs::create_dir_all(&run)
        .map_err(|error| GuestOverlayError::io("creating the guest runtime directory", error))?;
    mount(
        Some("tmpfs"),
        &run,
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some("mode=0755"),
    )
    .map_err(|error| GuestOverlayError::errno("mounting the guest runtime tmpfs", error))?;

    let old_root = Path::new(MERGED_MOUNT).join(OLD_ROOT_DIRECTORY);
    fs::create_dir_all(&old_root)
        .map_err(|error| GuestOverlayError::io("creating the old-root mount point", error))?;
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .map_err(|error| GuestOverlayError::errno("privatizing the original root mount", error))?;
    chdir(MERGED_MOUNT)
        .map_err(|error| GuestOverlayError::errno("entering the merged root", error))?;
    pivot_root(".", OLD_ROOT_DIRECTORY)
        .map_err(|error| GuestOverlayError::errno("pivoting into the merged root", error))?;
    chdir("/").map_err(|error| GuestOverlayError::errno("entering the guest root", error))?;
    let old_root = Path::new("/").join(OLD_ROOT_DIRECTORY);
    umount2(&old_root, MntFlags::MNT_DETACH)
        .map_err(|error| GuestOverlayError::errno("detaching the original root", error))?;
    fs::remove_dir(&old_root)
        .map_err(|error| GuestOverlayError::io("removing the old-root mount point", error))?;
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_SHARED,
        None::<&str>,
    )
    .map_err(|error| GuestOverlayError::errno("sharing the merged root mount", error))?;

    if let Some(resolver) = resolver_configuration.filter(|resolver| !resolver.is_empty()) {
        let resolver = resolver.replace("\\n", "\n");
        match fs::remove_file("/etc/resolv.conf") {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(GuestOverlayError::io(
                    "replacing the guest resolver configuration",
                    error,
                ));
            }
        }
        fs::write("/etc/resolv.conf", resolver)
            .map_err(|error| GuestOverlayError::io("writing the guest resolver", error))?;
    }
    Ok(())
}

#[cfg(all(
    test,
    feature = "host",
    any(
        all(target_os = "linux", not(target_env = "musl")),
        all(target_os = "macos", target_arch = "aarch64")
    )
))]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    };

    use arcbox_ext4::Reader;

    use super::create_sparse_overlay_disk;

    #[test]
    fn sparse_overlay_disk_is_private_and_contains_upper_and_work() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("upper.ext4");
        let bytes = 512 * 1024 * 1024;

        assert_eq!(
            create_sparse_overlay_disk(&destination, bytes).unwrap(),
            bytes
        );

        let metadata = destination.metadata().unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert!(metadata.blocks().saturating_mul(512) < bytes / 4);
        let mut reader = Reader::new(&destination).unwrap();
        assert!(reader.stat("/upper").unwrap().1.is_dir());
        assert!(reader.stat("/work").unwrap().1.is_dir());
    }

    #[test]
    fn sparse_overlay_disk_never_replaces_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("upper.ext4");
        fs::write(&destination, b"sentinel").unwrap();

        let error = create_sparse_overlay_disk(&destination, 512 * 1024 * 1024).unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "failed to create sparse OverlayFS disk: overlay disk already exists: {}",
                destination.display()
            )
        );
        assert_eq!(fs::read(destination).unwrap(), b"sentinel");
    }
}
