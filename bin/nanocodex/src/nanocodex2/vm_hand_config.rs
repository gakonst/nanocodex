use std::path::PathBuf;

use super::Hand;

/// Complete single-VM launch recipe shared by every platform backend.
#[derive(Clone, Debug)]
pub(crate) struct VmHandConfig {
    pub(crate) rootfs: PathBuf,
    pub(crate) vm_guest_runtime: Option<PathBuf>,
    pub(crate) vm_cache: PathBuf,
    pub(crate) vm_firmware: Option<PathBuf>,
    pub(crate) vm_workspace: String,
    pub(crate) vm_cpus: u8,
    pub(crate) vm_memory_mib: u32,
    pub(crate) vm_shell: String,
    pub(crate) vm_no_network: bool,
    pub(crate) machine_id: String,
    pub(crate) machine_name: String,
}

impl From<&Hand> for VmHandConfig {
    fn from(config: &Hand) -> Self {
        Self {
            rootfs: config.rootfs.clone(),
            vm_guest_runtime: config.vm_guest_runtime.clone(),
            vm_cache: config.vm_cache.clone(),
            vm_firmware: config.vm_firmware.clone(),
            vm_workspace: config.vm_workspace.clone(),
            vm_cpus: config.vm_cpus,
            vm_memory_mib: config.vm_memory_mib,
            vm_shell: config.vm_shell.clone(),
            vm_no_network: config.vm_no_network,
            machine_id: config.machine_id.clone(),
            machine_name: config.machine_name.clone(),
        }
    }
}
