use std::path::PathBuf;

use super::Hand;

/// Complete single-Hand launch recipe shared by every supported backend.
#[derive(Clone, Debug)]
pub(crate) struct VmHandConfig {
    pub(crate) rootfs: PathBuf,
    pub(crate) docker: Option<DockerHandConfig>,
    pub(crate) vm_guest_runtime: Option<PathBuf>,
    pub(crate) vm_cache: PathBuf,
    pub(crate) vm_firmware: Option<PathBuf>,
    pub(crate) vm_workspace: String,
    pub(crate) vm_cpus: u8,
    pub(crate) vm_memory_mib: u32,
    pub(crate) vm_gpu: bool,
    pub(crate) vm_shell: String,
    pub(crate) vm_no_network: bool,
    pub(crate) machine_id: String,
    pub(crate) machine_name: String,
}

impl From<&Hand> for VmHandConfig {
    fn from(config: &Hand) -> Self {
        Self {
            rootfs: config.rootfs.clone().unwrap_or_default(),
            docker: config.docker.as_ref().map(|image| DockerHandConfig {
                image: image.clone(),
                volume: config.docker_volume.clone().unwrap_or_default(),
                internet: config.docker_internet,
                runtime: config.docker_runtime.clone(),
            }),
            vm_guest_runtime: config.vm_guest_runtime.clone(),
            vm_cache: config.vm_cache.clone(),
            vm_firmware: config.vm_firmware.clone(),
            vm_workspace: config.vm_workspace.clone(),
            vm_cpus: config.vm_cpus,
            vm_memory_mib: config.vm_memory_mib,
            vm_gpu: config.vm_gpu,
            vm_shell: config.vm_shell.clone(),
            vm_no_network: config.vm_no_network,
            machine_id: config.machine_id.clone(),
            machine_name: config.machine_name.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DockerHandConfig {
    pub(crate) image: String,
    pub(crate) volume: String,
    pub(crate) internet: bool,
    pub(crate) runtime: Option<String>,
}
