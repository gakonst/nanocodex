use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Root filesystem exposed to one guest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RootFilesystem {
    /// A host directory shared through virtiofs.
    Directory(PathBuf),
    /// A raw ext4 image attached as the guest's writable root block device.
    Ext4(PathBuf),
    /// A guest OverlayFS with an immutable ext4 lower and writable ext4 upper.
    OverlayExt4 {
        /// Read-only disk containing the static Nanocodex guest runtime.
        runtime: PathBuf,
        /// Read-only prepared task root.
        lower: PathBuf,
        /// Writable sparse disk receiving all attempt mutations.
        upper: PathBuf,
    },
}

/// Network access supplied to the guest by libkrun.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Network {
    /// Do not attach a virtio-vsock device or proxy guest internet sockets.
    Disabled,
    /// Proxy guest internet sockets through libkrun TSI.
    #[default]
    Internet,
    /// A private virtio-net interface connected to a gvproxy unixgram socket.
    ///
    /// Unlike TSI, this gives the guest its own loopback and port namespace.
    Gvproxy {
        /// Host unixgram socket exposed by the owned gvproxy process.
        socket: PathBuf,
        /// Stable private MAC address assigned to the guest interface.
        mac_address: [u8; 6],
    },
}

impl Network {
    /// Creates a private virtio-net interface backed by gvproxy.
    #[must_use]
    pub fn gvproxy(socket: impl Into<PathBuf>) -> Self {
        Self::Gvproxy {
            socket: socket.into(),
            mac_address: [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee],
        }
    }
}

/// One additional block device attached after the root disk.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlockDevice {
    id: String,
    path: PathBuf,
    read_only: bool,
}

impl BlockDevice {
    /// Creates a writable block device.
    pub fn read_write(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            read_only: false,
        }
    }

    /// Creates an immutable block device.
    pub fn read_only(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            read_only: true,
        }
    }

    /// Returns the libkrun block-device identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the host path of the block image.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns whether guest writes to the device are prohibited.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

/// One narrowly scoped host directory exposed to the guest through virtiofs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SharedDirectory {
    tag: String,
    path: PathBuf,
    read_only: bool,
}

impl SharedDirectory {
    /// Creates a read-only share identified by `tag` inside the guest.
    pub fn read_only(tag: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            tag: tag.into(),
            path: path.into(),
            read_only: true,
        }
    }

    /// Creates a writable share identified by `tag` inside the guest.
    pub fn read_write(tag: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            tag: tag.into(),
            path: path.into(),
            read_only: false,
        }
    }

    /// Returns the virtiofs mount tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Returns the host directory shared through virtiofs.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns whether guest writes to the share are prohibited.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

/// Immutable configuration for one libkrun VM.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VmConfig {
    root: RootFilesystem,
    cpus: u8,
    memory_mib: u32,
    network: Network,
    block_devices: Vec<BlockDevice>,
    shared_directories: Vec<SharedDirectory>,
}

impl VmConfig {
    /// Creates a VM configuration with two vCPUs, 1 GiB RAM, and internet
    /// socket proxying enabled.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: RootFilesystem::Directory(root.into()),
            cpus: 2,
            memory_mib: 1_024,
            network: Network::Internet,
            block_devices: Vec::new(),
            shared_directories: Vec::new(),
        }
    }

    /// Creates a VM backed by a raw ext4 root disk.
    pub fn ext4(root: impl Into<PathBuf>) -> Self {
        Self {
            root: RootFilesystem::Ext4(root.into()),
            cpus: 2,
            memory_mib: 1_024,
            network: Network::Internet,
            block_devices: Vec::new(),
            shared_directories: Vec::new(),
        }
    }

    /// Creates a VM whose effective root is assembled by the guest.
    ///
    /// The runtime disk boots read-only as `/dev/vda`; the immutable lower is
    /// `/dev/vdb`; and the writable upper is `/dev/vdc`. Additional block
    /// devices are attached after those three devices.
    pub fn overlay_ext4(
        runtime: impl Into<PathBuf>,
        lower: impl Into<PathBuf>,
        upper: impl Into<PathBuf>,
    ) -> Self {
        Self {
            root: RootFilesystem::OverlayExt4 {
                runtime: runtime.into(),
                lower: lower.into(),
                upper: upper.into(),
            },
            cpus: 2,
            memory_mib: 1_024,
            network: Network::Internet,
            block_devices: Vec::new(),
            shared_directories: Vec::new(),
        }
    }

    /// Sets the number of virtual CPUs.
    #[must_use]
    pub const fn cpus(mut self, cpus: u8) -> Self {
        self.cpus = cpus;
        self
    }

    /// Sets guest memory in mebibytes.
    #[must_use]
    pub const fn memory_mib(mut self, memory_mib: u32) -> Self {
        self.memory_mib = memory_mib;
        self
    }

    /// Selects the guest network mode.
    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    /// Adds one virtiofs directory.
    #[must_use]
    pub fn shared_directory(mut self, directory: SharedDirectory) -> Self {
        self.shared_directories.push(directory);
        self
    }

    /// Adds one block device after the root disk.
    #[must_use]
    pub fn block_device(mut self, device: BlockDevice) -> Self {
        self.block_devices.push(device);
        self
    }

    /// Returns the host filesystem path that libkrun boots as the guest root.
    ///
    /// For an OverlayFS configuration this is the small runtime disk; inspect
    /// [`Self::root_filesystem`] for its lower and upper layers.
    #[must_use]
    pub fn root(&self) -> &Path {
        match &self.root {
            RootFilesystem::Directory(path) | RootFilesystem::Ext4(path) => path,
            RootFilesystem::OverlayExt4 { runtime, .. } => runtime,
        }
    }

    /// Returns the selected root filesystem kind.
    #[must_use]
    pub const fn root_filesystem(&self) -> &RootFilesystem {
        &self.root
    }

    /// Returns the configured virtual CPU count.
    #[must_use]
    pub const fn cpus_value(&self) -> u8 {
        self.cpus
    }

    /// Returns configured guest memory in mebibytes.
    #[must_use]
    pub const fn memory_mib_value(&self) -> u32 {
        self.memory_mib
    }

    /// Returns the selected guest network mode.
    #[must_use]
    pub const fn network_value(&self) -> &Network {
        &self.network
    }

    /// Returns additional virtiofs directories in attachment order.
    #[must_use]
    pub fn shared_directories(&self) -> &[SharedDirectory] {
        &self.shared_directories
    }

    /// Returns additional block devices in attachment order.
    #[must_use]
    pub fn block_devices(&self) -> &[BlockDevice] {
        &self.block_devices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_suitable_for_a_small_worker() {
        let config = VmConfig::new("rootfs");

        assert_eq!(config.root(), Path::new("rootfs"));
        assert_eq!(
            config.root_filesystem(),
            &RootFilesystem::Directory(PathBuf::from("rootfs"))
        );
        assert_eq!(config.cpus_value(), 2);
        assert_eq!(config.memory_mib_value(), 1_024);
        assert_eq!(config.network_value(), &Network::Internet);
    }

    #[test]
    fn policy_is_explicitly_overridable() {
        let config = VmConfig::new("rootfs")
            .cpus(8)
            .memory_mib(4_096)
            .network(Network::Disabled);

        assert_eq!(config.cpus_value(), 8);
        assert_eq!(config.memory_mib_value(), 4_096);
        assert_eq!(config.network_value(), &Network::Disabled);
    }

    #[test]
    fn gvproxy_network_has_an_isolated_default_identity() {
        let network = Network::gvproxy("/tmp/network.sock");

        assert_eq!(
            network,
            Network::Gvproxy {
                socket: PathBuf::from("/tmp/network.sock"),
                mac_address: [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee],
            }
        );
    }

    #[test]
    fn raw_ext4_is_an_explicit_root_kind() {
        let config = VmConfig::ext4("rootfs.ext4");

        assert_eq!(config.root(), Path::new("rootfs.ext4"));
        assert_eq!(
            config.root_filesystem(),
            &RootFilesystem::Ext4(PathBuf::from("rootfs.ext4"))
        );
    }

    #[test]
    fn overlay_root_owns_runtime_lower_and_upper_disks() {
        let config = VmConfig::overlay_ext4("runtime.ext4", "base.ext4", "upper.ext4");

        assert_eq!(config.root(), Path::new("runtime.ext4"));
        assert_eq!(
            config.root_filesystem(),
            &RootFilesystem::OverlayExt4 {
                runtime: PathBuf::from("runtime.ext4"),
                lower: PathBuf::from("base.ext4"),
                upper: PathBuf::from("upper.ext4"),
            }
        );
    }

    #[test]
    fn read_only_shares_are_explicit() {
        let config = VmConfig::ext4("rootfs.ext4").shared_directory(SharedDirectory::read_only(
            "nanocodex-tools",
            "target/guest-tools",
        ));

        assert_eq!(
            config.shared_directories(),
            &[SharedDirectory::read_only(
                "nanocodex-tools",
                "target/guest-tools"
            )]
        );
        assert!(config.shared_directories()[0].is_read_only());
    }

    #[test]
    fn block_device_mutability_is_explicit() {
        let config = VmConfig::ext4("rootfs.ext4")
            .block_device(BlockDevice::read_write("cache", "cache.ext4"))
            .block_device(BlockDevice::read_only("runtime", "runtime.ext4"));

        assert!(!config.block_devices()[0].is_read_only());
        assert!(config.block_devices()[1].is_read_only());
    }

    #[test]
    fn writable_shares_are_explicit() {
        let directory = SharedDirectory::read_write("nanocodex-cache", "cache");

        assert_eq!(directory.tag(), "nanocodex-cache");
        assert_eq!(directory.path(), Path::new("cache"));
        assert!(!directory.is_read_only());
    }

    #[test]
    fn read_only_block_devices_are_explicit() {
        let config = VmConfig::ext4("rootfs.ext4")
            .block_device(BlockDevice::read_only("runtime", "runtime.ext4"));

        assert_eq!(
            config.block_devices(),
            &[BlockDevice::read_only("runtime", "runtime.ext4")]
        );
        assert!(config.block_devices()[0].is_read_only());
    }
}
