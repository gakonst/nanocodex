#![allow(
    unsafe_code,
    reason = "this module is one of the two audited libkrun FFI boundaries"
)]

use std::{
    ffi::{CString, NulError, OsStr, c_char},
    io,
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::PathBuf,
    ptr,
};

use thiserror::Error;

use crate::{
    command::GuestCommand,
    config::{BlockDevice, Network, RootFilesystem, SharedDirectory, VmConfig},
};

const ROOT_TAG: &std::ffi::CStr = c"/dev/root";
const ROOT_BLOCK_ID: &std::ffi::CStr = c"vda";
const ROOT_BLOCK_DEVICE: &std::ffi::CStr = c"/dev/vda";
const OVERLAY_LOWER_BLOCK_ID: &std::ffi::CStr = c"nanocodex-overlay-lower";
const OVERLAY_UPPER_BLOCK_ID: &std::ffi::CStr = c"nanocodex-overlay-upper";
const EXT4_FILESYSTEM: &std::ffi::CStr = c"ext4";
const READ_ONLY_MOUNT_OPTIONS: &std::ffi::CStr = c"ro";
const TSI_HIJACK_INET: u32 = 1;
const NET_FLAG_VFKIT: u32 = 1 << 0;
const NET_FLAG_DHCP_CLIENT: u32 = 1 << 1;
const NET_FEATURE_CSUM: u32 = 1 << 0;
const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
const NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
const NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
const NET_FEATURE_HOST_UFO: u32 = 1 << 14;

/// Failure to validate, configure, control, or enter a libkrun VM.
#[derive(Debug, Error)]
pub enum VmError {
    /// A typed VM policy value violates a libkrun precondition.
    #[error("invalid VM configuration: {0}")]
    InvalidConfig(&'static str),

    /// The root filesystem could not be canonicalized.
    #[error("failed to resolve root filesystem {path}: {source}")]
    ResolveRoot {
        /// Requested host path.
        path: PathBuf,
        /// Canonicalization failure.
        source: io::Error,
    },

    /// An additional block-device path could not be canonicalized.
    #[error("failed to resolve block device {path}: {source}")]
    ResolveBlockDevice {
        /// Requested host path.
        path: PathBuf,
        /// Canonicalization failure.
        source: io::Error,
    },

    /// A virtiofs share path could not be canonicalized.
    #[error("failed to resolve shared directory {path}: {source}")]
    ResolveSharedDirectory {
        /// Requested host path.
        path: PathBuf,
        /// Canonicalization failure.
        source: io::Error,
    },

    /// A gvproxy network socket could not be canonicalized.
    #[error("failed to resolve network socket {path}: {source}")]
    ResolveNetworkSocket {
        /// Requested socket path.
        path: PathBuf,
        /// Canonicalization failure.
        source: io::Error,
    },

    /// A directory root policy resolved to a non-directory.
    #[error("root filesystem is not a directory: {0}")]
    RootNotDirectory(PathBuf),

    /// An ext4 root policy resolved to a non-file.
    #[error("root disk is not a file: {0}")]
    RootNotFile(PathBuf),

    /// An additional block-device path resolved to a non-file.
    #[error("block device is not a file: {0}")]
    BlockDeviceNotFile(PathBuf),

    /// A virtiofs share resolved to a non-directory.
    #[error("shared directory is not a directory: {0}")]
    SharedDirectoryNotDirectory(PathBuf),

    /// The guest working directory is not absolute.
    #[error("guest working directory must be absolute: {0}")]
    WorkingDirectoryNotAbsolute(PathBuf),

    /// A path or command value contains a C-string terminator.
    #[error("{field} contains a NUL byte")]
    Nul {
        /// Configuration field that could not be represented.
        field: &'static str,
        /// C-string construction failure.
        source: NulError,
    },

    /// A command argument cannot survive libkrun's quoted kernel transport.
    #[error("{field} contains a double quote unsupported by libkrun's command-line transport")]
    UnsupportedDoubleQuote {
        /// Command field that could not be represented.
        field: &'static str,
    },

    /// A libkrun API operation returned a negative errno.
    #[error("libkrun {operation} failed with errno {errno}")]
    Libkrun {
        /// Stable description of the failed operation.
        operation: &'static str,
        /// Positive errno reported by libkrun.
        errno: i32,
    },

    /// The blocking VMM loop unexpectedly returned success.
    #[error("libkrun returned after starting the VM")]
    UnexpectedReturn,

    /// A caller reused a context already handed to the VMM loop.
    #[error("the libkrun context has already entered the VM")]
    ContextConsumed,
}

/// A configured libkrun VM which has not entered its blocking event loop yet.
///
/// This is the low-level VMM primitive. [`Self::run`] does not return after a
/// successful boot because libkrun owns the calling process until the guest
/// exits. The retained process-backed session builds on this primitive.
pub struct KrunVm {
    context: Option<u32>,
}

enum ResolvedRoot {
    Directory(PathBuf),
    Ext4(PathBuf),
    OverlayExt4 {
        runtime: PathBuf,
        lower: PathBuf,
        upper: PathBuf,
    },
}

impl KrunVm {
    /// Creates a libkrun configuration context.
    ///
    /// # Errors
    ///
    /// Returns an error when the root filesystem or VM configuration is
    /// invalid, or when libkrun rejects a requested device.
    pub fn new(config: &VmConfig) -> Result<Self, VmError> {
        if config.cpus_value() == 0 {
            return Err(VmError::InvalidConfig("CPU count must be nonzero"));
        }
        if config.memory_mib_value() == 0 {
            return Err(VmError::InvalidConfig("memory must be nonzero"));
        }

        let root = resolve_root(config.root_filesystem())?;

        let context = positive_context(krun::krun_create_ctx(), "create context")?;
        let vm = Self {
            context: Some(context),
        };

        check(
            krun::krun_set_vm_config(context, config.cpus_value(), config.memory_mib_value()),
            "configure VM",
        )?;

        match root {
            ResolvedRoot::Directory(root) => {
                let root = c_string(root.as_os_str(), "root filesystem path")?;
                // SAFETY: both C strings live through the call and libkrun copies their
                // contents into the context before returning.
                check(
                    unsafe {
                        krun::krun_add_virtiofs3(
                            context,
                            ROOT_TAG.as_ptr(),
                            root.as_ptr(),
                            0,
                            false,
                        )
                    },
                    "attach root filesystem",
                )?;
            }
            ResolvedRoot::Ext4(root) => {
                attach_root_disk(context, &root, false)?;
            }
            ResolvedRoot::OverlayExt4 {
                runtime,
                lower,
                upper,
            } => {
                attach_root_disk(context, &runtime, true)?;
                attach_resolved_disk(
                    context,
                    OVERLAY_LOWER_BLOCK_ID,
                    &lower,
                    true,
                    "attach immutable overlay lower disk",
                )?;
                attach_resolved_disk(
                    context,
                    OVERLAY_UPPER_BLOCK_ID,
                    &upper,
                    false,
                    "attach writable overlay upper disk",
                )?;
            }
        }

        attach_block_devices(context, config.block_devices())?;
        attach_shared_directories(context, config.shared_directories())?;

        let stdin = io::stdin();
        let stdout = io::stdout();
        let stderr = io::stderr();
        // SAFETY: the standard descriptors remain owned by this process for
        // the VM lifetime; libkrun duplicates the descriptors it needs.
        check(
            unsafe {
                krun::krun_add_virtio_console_default(
                    context,
                    stdin.as_raw_fd(),
                    stdout.as_raw_fd(),
                    stderr.as_raw_fd(),
                )
            },
            "attach console",
        )?;

        attach_network(context, config.network_value())?;
        check(
            krun::krun_split_irqchip(context, false),
            "configure interrupt controller",
        )?;

        Ok(vm)
    }

    /// Returns a thread-safe out-of-band pause/resume capability.
    ///
    /// # Errors
    ///
    /// Returns an error after this context has entered the VMM loop.
    pub fn control(&self) -> Result<KrunVmControl, VmError> {
        self.context
            .map(|context| KrunVmControl { context })
            .ok_or(VmError::ContextConsumed)
    }

    /// Configures the guest command and enters libkrun's blocking VMM loop.
    ///
    /// On successful boot this function does not return: libkrun exits the VMM
    /// process when the guest shuts down. Call this only in a dedicated VMM
    /// process.
    ///
    /// # Errors
    ///
    /// Returns an error when a command value contains a NUL byte, libkrun
    /// rejects the command, or the VMM loop unexpectedly returns.
    pub fn run(mut self, command: &GuestCommand) -> Result<(), VmError> {
        validate_guest_command(command)?;
        let executable = c_string(command.program().as_os_str(), "guest executable")?;
        let arguments = command
            .arguments()
            .iter()
            .map(|argument| array_string(argument, "guest argument"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut argument_pointers = arguments
            .iter()
            .map(|argument| argument.as_ptr())
            .collect::<Vec<_>>();
        argument_pointers.push(ptr::null());
        let environment = command
            .environment()
            .iter()
            .map(|(name, value)| {
                let mut entry = name.clone();
                entry.push("=");
                entry.push(value);
                array_string(&entry, "guest environment")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut environment_pointers = environment
            .iter()
            .map(|entry| entry.as_ptr())
            .collect::<Vec<_>>();
        environment_pointers.push(ptr::null::<c_char>());
        let context = self.context.ok_or(VmError::ContextConsumed)?;

        // SAFETY: executable, argument, and environment storage remains alive
        // through the call; each pointer list is NUL terminated. libkrun copies
        // the values into its owned context.
        check(
            unsafe {
                krun::krun_set_exec(
                    context,
                    executable.as_ptr(),
                    argument_pointers.as_ptr(),
                    environment_pointers.as_ptr(),
                )
            },
            "configure guest command",
        )?;
        let current_dir = c_string(
            command.current_directory().as_os_str(),
            "guest working directory",
        )?;
        // SAFETY: the C string remains valid for the call and libkrun copies it
        // into the context.
        check(
            unsafe { krun::krun_set_workdir(context, current_dir.as_ptr()) },
            "configure guest working directory",
        )?;

        let status = krun::krun_start_enter(context);
        if status < 0 {
            return check(status, "start VM");
        }
        self.context = None;
        Err(VmError::UnexpectedReturn)
    }
}

fn resolve_root(root: &RootFilesystem) -> Result<ResolvedRoot, VmError> {
    match root {
        RootFilesystem::Directory(path) => {
            let resolved = resolve_root_path(path)?;
            if !resolved.is_dir() {
                return Err(VmError::RootNotDirectory(resolved));
            }
            Ok(ResolvedRoot::Directory(resolved))
        }
        RootFilesystem::Ext4(path) => {
            let resolved = resolve_root_path(path)?;
            if !resolved.is_file() {
                return Err(VmError::RootNotFile(resolved));
            }
            Ok(ResolvedRoot::Ext4(resolved))
        }
        RootFilesystem::OverlayExt4 {
            runtime,
            lower,
            upper,
        } => {
            let runtime = resolve_root_path(runtime)?;
            if !runtime.is_file() {
                return Err(VmError::RootNotFile(runtime));
            }
            let lower = resolve_block_path(lower)?;
            if !lower.is_file() {
                return Err(VmError::BlockDeviceNotFile(lower));
            }
            let upper = resolve_block_path(upper)?;
            if !upper.is_file() {
                return Err(VmError::BlockDeviceNotFile(upper));
            }
            Ok(ResolvedRoot::OverlayExt4 {
                runtime,
                lower,
                upper,
            })
        }
    }
}

fn resolve_root_path(path: &std::path::Path) -> Result<PathBuf, VmError> {
    path.canonicalize().map_err(|source| VmError::ResolveRoot {
        path: path.to_path_buf(),
        source,
    })
}

fn resolve_block_path(path: &std::path::Path) -> Result<PathBuf, VmError> {
    path.canonicalize()
        .map_err(|source| VmError::ResolveBlockDevice {
            path: path.to_path_buf(),
            source,
        })
}

fn attach_root_disk(context: u32, path: &std::path::Path, read_only: bool) -> Result<(), VmError> {
    attach_resolved_disk(context, ROOT_BLOCK_ID, path, read_only, "attach root disk")?;
    let options = if read_only {
        READ_ONLY_MOUNT_OPTIONS.as_ptr()
    } else {
        ptr::null()
    };
    // SAFETY: all strings are static and NUL terminated.
    check(
        unsafe {
            krun::krun_set_root_disk_remount(
                context,
                ROOT_BLOCK_DEVICE.as_ptr(),
                EXT4_FILESYSTEM.as_ptr(),
                options,
            )
        },
        "select root disk",
    )
}

fn attach_resolved_disk(
    context: u32,
    id: &std::ffi::CStr,
    path: &std::path::Path,
    read_only: bool,
    operation: &'static str,
) -> Result<(), VmError> {
    let path = c_string(path.as_os_str(), "block device path")?;
    // SAFETY: both strings remain valid through the call and libkrun copies
    // their contents into its context before returning.
    check(
        unsafe { krun::krun_add_disk(context, id.as_ptr(), path.as_ptr(), read_only) },
        operation,
    )
}

fn attach_network(context: u32, network: &Network) -> Result<(), VmError> {
    match network {
        Network::Disabled => {}
        Network::Internet => {
            check(
                krun::krun_add_vsock(context, TSI_HIJACK_INET),
                "enable TSI networking",
            )?;
        }
        Network::Gvproxy {
            socket,
            mac_address,
        } => {
            let socket = socket
                .canonicalize()
                .map_err(|source| VmError::ResolveNetworkSocket {
                    path: socket.clone(),
                    source,
                })?;
            let socket = c_string(socket.as_os_str(), "gvproxy socket path")?;
            let mut mac_address = *mac_address;
            check(
                // SAFETY: the socket string and MAC array remain alive for the
                // call and libkrun copies both into its configuration.
                unsafe {
                    krun::krun_add_net_unixgram(
                        context,
                        socket.as_ptr(),
                        -1,
                        mac_address.as_mut_ptr(),
                        COMPATIBLE_NETWORK_FEATURES,
                        NET_FLAG_VFKIT | NET_FLAG_DHCP_CLIENT,
                    )
                },
                "attach gvproxy network",
            )?;
        }
    }
    Ok(())
}

const COMPATIBLE_NETWORK_FEATURES: u32 = NET_FEATURE_CSUM
    | NET_FEATURE_GUEST_CSUM
    | NET_FEATURE_GUEST_TSO4
    | NET_FEATURE_GUEST_UFO
    | NET_FEATURE_HOST_TSO4
    | NET_FEATURE_HOST_UFO;

fn attach_block_devices(context: u32, devices: &[BlockDevice]) -> Result<(), VmError> {
    for device in devices {
        let path = device
            .path()
            .canonicalize()
            .map_err(|source| VmError::ResolveBlockDevice {
                path: device.path().to_path_buf(),
                source,
            })?;
        if !path.is_file() {
            return Err(VmError::BlockDeviceNotFile(path));
        }
        let id = c_string(OsStr::new(device.id()), "block device ID")?;
        let path = c_string(path.as_os_str(), "block device path")?;
        // SAFETY: both strings remain valid through the call and libkrun
        // copies their contents into the context before returning.
        check(
            unsafe {
                krun::krun_add_disk(context, id.as_ptr(), path.as_ptr(), device.is_read_only())
            },
            "attach block device",
        )?;
    }
    Ok(())
}

fn attach_shared_directories(context: u32, directories: &[SharedDirectory]) -> Result<(), VmError> {
    for directory in directories {
        let path =
            directory
                .path()
                .canonicalize()
                .map_err(|source| VmError::ResolveSharedDirectory {
                    path: directory.path().to_path_buf(),
                    source,
                })?;
        if !path.is_dir() {
            return Err(VmError::SharedDirectoryNotDirectory(path));
        }
        let tag = c_string(OsStr::new(directory.tag()), "shared directory tag")?;
        let path = c_string(path.as_os_str(), "shared directory path")?;
        // SAFETY: both C strings remain valid through the call and libkrun
        // copies their contents into the context before returning.
        check(
            unsafe {
                krun::krun_add_virtiofs3(
                    context,
                    tag.as_ptr(),
                    path.as_ptr(),
                    0,
                    directory.is_read_only(),
                )
            },
            "attach shared directory",
        )?;
    }
    Ok(())
}

/// Out-of-band control for a VM running in libkrun's event loop.
///
/// This capability is valid only while the [`KrunVm`] context is alive in its
/// dedicated VMM process. Do not retain it after the VMM exits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KrunVmControl {
    context: u32,
}

impl KrunVmControl {
    /// Requests that every guest vCPU pause at an instruction boundary.
    ///
    /// libkrun currently implements this operation on macOS. The request is
    /// idempotent and completes asynchronously in the VMM event loop.
    ///
    /// # Errors
    ///
    /// Returns an OS error reported by libkrun, including unsupported-platform
    /// and not-yet-running errors.
    pub fn pause(self) -> Result<(), VmError> {
        check(krun::krun_vm_pause(self.context), "pause VM")
    }

    /// Resumes a VM previously paused with [`Self::pause`].
    ///
    /// # Errors
    ///
    /// Returns an OS error reported by libkrun.
    pub fn resume(self) -> Result<(), VmError> {
        check(krun::krun_vm_resume(self.context), "resume VM")
    }
}

impl Drop for KrunVm {
    fn drop(&mut self) {
        if let Some(context) = self.context.take() {
            let _ = krun::krun_free_ctx(context);
        }
    }
}

fn c_string(value: &OsStr, field: &'static str) -> Result<CString, VmError> {
    CString::new(value.as_bytes()).map_err(|source| VmError::Nul { field, source })
}

/// Validates values for libkrun's quoted command-line array serialization.
///
/// `krun_set_exec` does not preserve the supplied C array directly: it wraps
/// every entry in double quotes and forwards the resulting string through the
/// guest kernel command line. Its parser cannot represent a literal double
/// quote reliably, so fail closed instead of silently changing argv/envp.
fn array_string(value: &OsStr, field: &'static str) -> Result<CString, VmError> {
    let bytes = value.as_bytes();
    if bytes.contains(&b'"') {
        return Err(VmError::UnsupportedDoubleQuote { field });
    }
    CString::new(bytes).map_err(|source| VmError::Nul { field, source })
}

fn validate_guest_command(command: &GuestCommand) -> Result<(), VmError> {
    if !command.current_directory().is_absolute() {
        return Err(VmError::WorkingDirectoryNotAbsolute(
            command.current_directory().to_path_buf(),
        ));
    }
    Ok(())
}

fn positive_context(status: i32, operation: &'static str) -> Result<u32, VmError> {
    u32::try_from(status).map_err(|_| VmError::Libkrun {
        operation,
        errno: status.saturating_neg(),
    })
}

const fn check(status: i32, operation: &'static str) -> Result<(), VmError> {
    if status < 0 {
        Err(VmError::Libkrun {
            operation,
            errno: status.saturating_neg(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::Path};

    use super::{GuestCommand, VmError, array_string, validate_guest_command};

    #[test]
    fn libkrun_array_values_reject_unrepresentable_quotes() {
        assert!(matches!(
            array_string(OsStr::new(r#"printf "%s""#), "test"),
            Err(VmError::UnsupportedDoubleQuote { field: "test" })
        ));
        assert_eq!(
            array_string(OsStr::new(r"backslash\path"), "test")
                .unwrap()
                .as_c_str(),
            c"backslash\\path"
        );
    }

    #[test]
    fn guest_working_directory_must_be_absolute() {
        assert!(matches!(
            validate_guest_command(&GuestCommand::new("/bin/true").current_dir("workspace")),
            Err(VmError::WorkingDirectoryNotAbsolute(path))
                if path == Path::new("workspace")
        ));
        validate_guest_command(&GuestCommand::new("/bin/true").current_dir("/workspace")).unwrap();
    }
}
