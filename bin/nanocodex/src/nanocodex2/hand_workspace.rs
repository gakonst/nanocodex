//! Shared Hand operations over explicitly selected execution backends.

use nanocodex_tools::ToolsBuilder;
use nanocodex_vm::{
    VmWorkspace, VmWorkspaceError,
    docker::{DockerWorkspace, DockerWorkspaceError},
    tools::{VmToolSessionError, VmToolSessionHandle},
};

pub(crate) enum HandWorkspace {
    Vm(VmWorkspace),
    Docker(DockerWorkspace),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ShutdownError {
    #[error(transparent)]
    Vm(#[from] VmWorkspaceError),
    #[error(transparent)]
    Docker(#[from] DockerWorkspaceError),
}

impl ShutdownError {
    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Vm(VmWorkspaceError::Session(
                VmToolSessionError::ActiveCapabilities(_) | VmToolSessionError::ActiveRequests(_)
            )) | Self::Docker(DockerWorkspaceError::Session(
                VmToolSessionError::ActiveCapabilities(_) | VmToolSessionError::ActiveRequests(_)
            ))
        )
    }
}

impl HandWorkspace {
    pub(crate) fn control(&self) -> VmToolSessionHandle {
        match self {
            Self::Vm(workspace) => workspace.control(),
            Self::Docker(workspace) => workspace.control(),
        }
    }

    pub(crate) fn guest_workspace(&self) -> &str {
        match self {
            Self::Vm(workspace) => workspace.guest_workspace(),
            Self::Docker(workspace) => workspace.guest_workspace(),
        }
    }

    pub(crate) fn attachment_tools_builder(&self) -> ToolsBuilder {
        match self {
            Self::Vm(workspace) => workspace.attachment_tools_builder(),
            Self::Docker(workspace) => workspace.attachment_tools_builder(),
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ShutdownError> {
        match self {
            Self::Vm(workspace) => workspace.shutdown().await.map_err(Into::into),
            Self::Docker(workspace) => workspace.shutdown().await.map_err(Into::into),
        }
    }
}
