//! Private owner for one background tool attachment and its preparation.

use std::{fmt, future::Future, pin::Pin};

use nanocodex_oai_tools::{
    Tools,
    attachment::{Attachment, AttachmentError, AttachmentMetadata, AttachmentTarget},
};
use tokio::{sync::watch, task::JoinHandle};

pub(crate) enum ToolPreparation {
    Ready(Tools),
    Deferred(Pin<Box<dyn Future<Output = Tools> + Send>>),
    Preparing(PreparingTools),
}

impl fmt::Debug for ToolPreparation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ready(_) => "ToolPreparation::Ready",
            Self::Deferred(_) => "ToolPreparation::Deferred",
            Self::Preparing(_) => "ToolPreparation::Preparing",
        })
    }
}

impl ToolPreparation {
    pub(crate) fn start(self) -> Self {
        match self {
            Self::Deferred(future) => Self::Preparing(PreparingTools(tokio::spawn(future))),
            other => other,
        }
    }
}

pub(crate) struct PreparingTools(JoinHandle<Tools>);

impl Drop for PreparingTools {
    fn drop(&mut self) {
        // Dropping a creation future or a failed admission must not orphan work.
        self.0.abort();
    }
}

pub(crate) enum AttachmentSupervisor {
    Ready(Attachment),
    Preparing(PreparingAttachment),
}

pub(crate) struct PreparingAttachment {
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<(), AttachmentError>>,
}

impl Drop for PreparingAttachment {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

impl AttachmentSupervisor {
    pub(crate) fn start(
        tools: ToolPreparation,
        target: AttachmentTarget,
        metadata: Option<AttachmentMetadata>,
    ) -> Result<Self, AttachmentError> {
        match tools.start() {
            ToolPreparation::Ready(tools) => Self::attach(tools, target, metadata).map(Self::Ready),
            ToolPreparation::Preparing(mut preparation) => {
                let (stop, mut stopped) = watch::channel(false);
                let task = tokio::spawn(async move {
                    let tools = tokio::select! {
                        biased;
                        _ = stopped.wait_for(|stop| *stop) => return Ok(()),
                        tools = &mut preparation.0 => tools.map_err(|_| AttachmentError::Catalog("tool preparation task failed".into()))?,
                    };
                    let attachment = Self::attach(tools, target, metadata)?;
                    let _ = stopped.wait_for(|stop| *stop).await;
                    attachment.detach().await
                });
                Ok(Self::Preparing(PreparingAttachment { stop, task }))
            }
            ToolPreparation::Deferred(_) => unreachable!("preparation was started"),
        }
    }

    fn attach(
        tools: Tools,
        target: AttachmentTarget,
        metadata: Option<AttachmentMetadata>,
    ) -> Result<Attachment, AttachmentError> {
        let connector = tools.attach(target);
        let connector = match metadata {
            Some(metadata) => connector.metadata(metadata),
            None => connector,
        };
        let (attachment, _events) = connector.start()?;
        Ok(attachment)
    }

    pub(crate) async fn shutdown(self) -> Result<(), AttachmentError> {
        match self {
            Self::Ready(attachment) => attachment.detach().await,
            Self::Preparing(mut pending) => {
                pending.stop.send_replace(true);
                (&mut pending.task)
                    .await
                    .map_err(|_| AttachmentError::Closed)?
            }
        }
    }
}
