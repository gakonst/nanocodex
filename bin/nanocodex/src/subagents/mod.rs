//! CLI lifecycle ownership over the reusable subagent extension.
use nanocodex_subagents::SubagentControl;
pub(crate) use nanocodex_subagents::{DEFAULT_MAX_SUBAGENTS, ScopedAgentUpdate, channel};
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};

pub(crate) struct ChildAgents {
    root_session_id: String,
    control: SubagentControl,
    update_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl ChildAgents {
    pub(crate) fn new(
        root_session_id: String,
        control: SubagentControl,
        updates: Option<mpsc::UnboundedReceiver<ScopedAgentUpdate>>,
    ) -> Arc<Self> {
        let update_task = updates.map(|mut updates| {
            tokio::spawn(async move { while updates.recv().await.is_some() {} })
        });
        Arc::new(Self {
            root_session_id,
            control,
            update_task: tokio::sync::Mutex::new(update_task),
        })
    }

    /// Changes how many child agents may run at once for this session tree.
    pub(crate) fn set_max_concurrency(&self, limit: usize) {
        self.control.set_max_concurrency(limit);
    }

    pub(crate) async fn shutdown(&self) {
        drop(self.control.close_all(&self.root_session_id).await);
        if let Some(update_task) = self.update_task.lock().await.take() {
            update_task.abort();
            drop(update_task.await);
        }
    }
}
