use std::path::Path;

use nanocodex_managed::ManagedError;
use nanocodex_tools::ToolsBuilder;

use super::Hand;

pub(crate) struct VmHand;

impl VmHand {
    pub(crate) async fn start(_config: &Hand) -> Result<Self, ManagedError> {
        Err(unsupported())
    }

    pub(crate) fn tools_builder(&self) -> ToolsBuilder {
        unreachable!("unsupported VM hand cannot be constructed")
    }

    pub(crate) async fn shutdown(self) -> Result<(), ManagedError> {
        Ok(())
    }
}

pub(crate) fn run_config(_path: &Path) -> Result<(), ManagedError> {
    Err(unsupported())
}

fn unsupported() -> ManagedError {
    ManagedError::Configuration(
        "VM hands require glibc Linux with /dev/kvm or Apple Silicon macOS".to_owned(),
    )
}
