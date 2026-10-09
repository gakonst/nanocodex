//! Local subagent tree updates.
//!
//! Port of the legacy receive_subagent_update arm: every ScopedAgentUpdate
//! from the local child-agent registry feeds the shared subagent tree
//! (AppEvent::Subagent). The shared root component owns completion
//! continuation: when a direct child completes while the main agent is idle it
//! prompts the main agent once ("[Subagent N completed]"), exactly as for a
//! managed session, so this feature does not submit a second continuation.

use nanocodex_subagents::ScopedAgentUpdate;
use tokio::{sync::mpsc, task::JoinHandle};

use super::{Feature, FeatureContext, FeatureHost, FeatureUpdate};
use crate::nanocodex2::tui::local::agent::LocalParts;

#[derive(Default)]
pub(crate) struct Subagents {
    task: Option<JoinHandle<()>>,
}

impl Feature for Subagents {
    fn name(&self) -> &'static str {
        "subagents"
    }

    fn attach(&mut self, parts: &mut LocalParts, cx: &FeatureContext<'_>) {
        self.shutdown();
        if let Some(updates) = parts.subagent_updates.take() {
            self.task = Some(tokio::spawn(forward(updates, cx.host.clone())));
        }
    }

    fn shutdown(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn forward(mut updates: mpsc::UnboundedReceiver<ScopedAgentUpdate>, host: FeatureHost) {
    while let Some(scoped) = updates.recv().await {
        host.send(FeatureUpdate::Subagent(scoped.update));
    }
}
