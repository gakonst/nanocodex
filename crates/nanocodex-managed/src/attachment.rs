//! Private owner for one background tool attachment.

use nanocodex_tools::{
    Tools,
    attachment::{Attachment, AttachmentError, AttachmentMetadata, AttachmentTarget},
};

#[derive(Clone)]
pub(crate) struct AttachmentSupervisor {
    attachment: Attachment,
}

impl AttachmentSupervisor {
    pub(crate) fn start(
        tools: Tools,
        target: AttachmentTarget,
        metadata: Option<AttachmentMetadata>,
    ) -> Result<Self, AttachmentError> {
        let connector = tools.attach(target);
        let connector = match metadata {
            Some(metadata) => connector.metadata(metadata),
            None => connector,
        };
        let (attachment, _events) = connector.start()?;
        Ok(Self { attachment })
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AttachmentError> {
        self.attachment.clone().detach().await
    }
}
