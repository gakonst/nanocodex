use std::path::Path;

use nanocodex_managed::ManagedError;
use nanocodex_tools::{Tools, attachment::AttachmentMachine};

use super::Hand;
pub(crate) use super::vm_hand_config::VmHandConfig;

pub(crate) struct VmHand;

impl VmHand {
    pub(crate) async fn preflight(config: &Hand) -> Result<(), ManagedError> {
        let backend = if config.docker.is_some() {
            "Docker"
        } else {
            "VM"
        };
        Err(ManagedError::Configuration(format!(
            "{backend} Hands are not supported by this {}/{} build; use a glibc Linux or Apple Silicon macOS build. Linux Docker Hands require a Linux Docker daemon; Linux VM Hands additionally require KVM. No fallback was attempted",
            std::env::consts::OS,
            std::env::consts::ARCH
        )))
    }

    pub(crate) async fn start(_config: &Hand) -> Result<Self, ManagedError> {
        Err(unsupported())
    }

    pub(crate) async fn start_config(_config: &VmHandConfig) -> Result<Self, ManagedError> {
        Err(unsupported())
    }

    pub(crate) fn machine(&self) -> &AttachmentMachine {
        unreachable!("unsupported VM hand cannot be constructed")
    }

    pub(crate) fn tools(&self) -> Tools {
        unreachable!("unsupported VM hand cannot be constructed")
    }

    pub(crate) async fn start_desktop(
        &mut self,
        _target: &nanocodex_tools::attachment::AttachmentTarget,
    ) -> Result<(), ManagedError> {
        Err(unsupported())
    }

    pub(crate) async fn shutdown(self) -> Result<(), ManagedError> {
        Ok(())
    }
}

pub(crate) fn run_config(_path: &Path) -> Result<(), ManagedError> {
    Err(unsupported())
}

pub(crate) fn clone_image(_source: &Path, _destination: &Path) -> Result<(), ManagedError> {
    Err(unsupported())
}

fn unsupported() -> ManagedError {
    ManagedError::Configuration(
        "VM hands require glibc Linux with /dev/kvm or Apple Silicon macOS".to_owned(),
    )
}
