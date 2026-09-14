use std::path::PathBuf;

use super::{Hand, HandNetwork};

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
                internet: config.network.map_or(config.docker_internet, |value| {
                    value == HandNetwork::Internet
                }),
                runtime: config.docker_runtime.clone(),
            }),
            vm_guest_runtime: config.vm_guest_runtime.clone().or_else(|| {
                config
                    .docker
                    .is_none()
                    .then(|| std::env::var_os("NANOCODEX_VM_GUEST_RUNTIME").map(PathBuf::from))
                    .flatten()
            }),
            vm_cache: config.vm_cache.clone(),
            vm_firmware: config.vm_firmware.clone().or_else(|| {
                config
                    .docker
                    .is_none()
                    .then(|| std::env::var_os("NANOCODEX_KRUNFW_DIR").map(PathBuf::from))
                    .flatten()
            }),
            vm_workspace: config.vm_workspace.clone(),
            vm_cpus: config.vm_cpus,
            vm_memory_mib: config.vm_memory_mib,
            vm_gpu: config.vm_gpu,
            vm_shell: config.vm_shell.clone(),
            vm_no_network: config
                .network
                .map_or(config.vm_no_network, |value| value == HandNetwork::Off),
            machine_id: config.machine_id().to_owned(),
            machine_name: config.machine_name.clone().unwrap_or_else(|| {
                if config.docker.is_some() {
                    "Nanocodex Docker Hand"
                } else {
                    "Nanocodex VM"
                }
                .to_owned()
            }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Command};
    use clap::Parser as _;

    fn config(args: &[&str]) -> VmHandConfig {
        let cli = Cli::try_parse_from(
            ["nanocodex2", "hand"]
                .into_iter()
                .chain(args.iter().copied()),
        )
        .unwrap();
        let Some(Command::Hand(hand)) = cli.command else {
            panic!("expected Hand")
        };
        VmHandConfig::from(&hand)
    }

    #[test]
    fn canonical_and_legacy_resource_flags_select_the_same_docker_config() {
        for args in [
            vec![
                "--docker",
                "image",
                "--volume",
                "work",
                "--cpus",
                "4",
                "--memory",
                "2048",
                "--workspace",
                "/workspace",
                "--network",
                "internet",
                "--runtime",
                "runsc",
            ],
            vec![
                "--docker",
                "image",
                "--docker-volume",
                "work",
                "--vm-cpus",
                "4",
                "--vm-memory-mib",
                "2048",
                "--vm-workspace",
                "/workspace",
                "--docker-internet",
                "--docker-runtime",
                "runsc",
            ],
        ] {
            let config = config(&args);
            let docker = config.docker.unwrap();
            assert_eq!(
                (docker.image.as_str(), docker.volume.as_str()),
                ("image", "work")
            );
            assert!(docker.internet);
            assert_eq!(docker.runtime.as_deref(), Some("runsc"));
            assert_eq!((config.vm_cpus, config.vm_memory_mib), (4, 2048));
            assert_eq!(config.vm_workspace, "/workspace");
            assert_eq!(config.machine_id, "docker");
            assert!(config.vm_guest_runtime.is_none());
        }
    }

    #[test]
    fn network_defaults_and_vm_identity_remain_compatible() {
        let docker = config(&["--docker", "image", "--volume", "work"]);
        assert!(!docker.docker.unwrap().internet);
        let vm = config(&["--vm", "root.ext4"]);
        assert!(!vm.vm_no_network);
        assert_eq!(vm.machine_id, "vm");
        assert_eq!(vm.machine_name, "Nanocodex VM");
        for flag in [vec!["--network", "off"], vec!["--vm-no-network"]] {
            let args = [vec!["--vm", "root.ext4"], flag].concat();
            assert!(config(&args).vm_no_network);
        }
    }
}
