//! Hand device credentials for the VM factory child.
//!
//! A device-enrolled Hand never gives its factory the account API key. The
//! daemon keeps the current short-lived device credential in a private 0600
//! file it atomically replaces before expiry; the factory reads the file again
//! before every (re)connect. Only the file path crosses the process boundary.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use nanocodex_bin_shared::device_identity::{DeviceCredentials, DeviceError};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Names the credential file for the factory child; never the credential.
pub(crate) const CREDENTIAL_FILE_ENV: &str = "NANOCODEX_VM_HOST_CREDENTIAL_FILE";
const FILE_NAME: &str = "vm-host-credential";
const MAX_BYTES: u64 = 1024;
/// Well inside the credential source's refresh margin before expiry.
const REFRESH_INTERVAL: Duration = Duration::from_secs(20);

/// `ncxhd1.{owner}.{device}.{secret}` with UUID ids and a 32-byte base64url secret.
fn valid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    let uuid =
        |part: &str| part.len() == 36 && part.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    parts.len() == 4
        && parts[0] == "ncxhd1"
        && uuid(parts[1])
        && uuid(parts[2])
        && parts[3].len() == 43
        && parts[3]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Replaces a Hand device credential in log text with a fixed marker.
pub(crate) fn redact(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(index) = rest.find("ncxhd1.") {
        output.push_str(&rest[..index]);
        output.push_str("ncxhd1.[redacted]");
        let tail = &rest[index + "ncxhd1.".len()..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    output.push_str(rest);
    output
}

/// Atomic replace: exclusive 0600 temporary file, fsync, rename, fsync directory.
fn write(directory: &Path, credential: &str) -> io::Result<()> {
    let temporary = directory.join(format!(".{FILE_NAME}.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
        }
        let mut file = options.open(&temporary)?;
        io::Write::write_all(&mut file, credential.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, directory.join(FILE_NAME))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// Writes a changed credential. Returns false once the device is no longer
/// accepted, after removing the file.
async fn refresh(
    credentials: &DeviceCredentials,
    directory: &Path,
    written: &mut Option<Zeroizing<String>>,
) -> bool {
    match credentials.current().await {
        Ok(credential) => {
            if written.as_ref().map(|value| value.as_str()) != Some(credential.as_str()) {
                match write(directory, &credential) {
                    Ok(()) => *written = Some(credential),
                    Err(error) => {
                        tracing::warn!(target: "nanocodex2", stage = "vm.host.credential_write_failed", %error,
                        "Cannot write the VM factory device credential file")
                    }
                }
            }
            true
        }
        Err(DeviceError::Transient(message)) => {
            tracing::warn!(target: "nanocodex2", stage = "vm.host.credential_unavailable", error = %message,
                "VM factory device credential temporarily unavailable");
            true
        }
        Err(error) => {
            let _ = fs::remove_file(directory.join(FILE_NAME));
            tracing::error!(target: "nanocodex2", stage = "vm.host.credential_unavailable", %error,
                "VM factory device credential permanently unavailable");
            false
        }
    }
}

/// Writes the current credential file, then keeps it fresh until cancelled or
/// the device is no longer accepted. The task removes the file when it ends.
pub(crate) async fn start(
    credentials: Arc<DeviceCredentials>,
    directory: &Path,
    cancel: CancellationToken,
) -> (PathBuf, tokio::task::JoinHandle<()>) {
    let mut written = None;
    let mut live = refresh(&credentials, directory, &mut written).await;
    let owned = directory.to_owned();
    let task = tokio::spawn(async move {
        while live {
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(REFRESH_INTERVAL) => {}
            }
            live = refresh(&credentials, &owned, &mut written).await;
        }
        let _ = fs::remove_file(owned.join(FILE_NAME));
    });
    (directory.join(FILE_NAME), task)
}

/// Reads the factory's current device credential (factory child, per connect).
/// Refuses symlinks, non-regular or oversized files and, outside WSL's
/// Windows-owned drvfs, files not owned by this user with mode 0600.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn read(path: &Path) -> io::Result<Zeroizing<String>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the VM factory credential file is not a small regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // WSL reads a Windows-owned file through drvfs, protected by its ACL.
        let drvfs = std::env::var_os("WSL_DISTRO_NAME").is_some() && path.starts_with("/mnt/");
        if !drvfs
            && (metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the VM factory credential file must be owned by this user with mode 0600",
            ));
        }
    }
    let mut value = Zeroizing::new(String::new());
    io::Read::read_to_string(&mut io::Read::take(file, MAX_BYTES), &mut value)?;
    let trimmed = value.trim();
    if !valid(trimmed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the VM factory credential file does not hold a Hand device credential",
        ));
    }
    Ok(Zeroizing::new(trimmed.to_owned()))
}
