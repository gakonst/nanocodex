//! CLI composition over the reusable subagent extension.

mod simplify;

use nanocodex::{Tools, agent::AgentHandle, tools::ToolsBuildError};
use nanocodex_subagents::SubagentControl;
pub(crate) use nanocodex_subagents::{
    AgentId, AgentStatus, AgentUpdate, DEFAULT_MAX_SUBAGENTS, Registry, ScopedAgentUpdate, channel,
};
use std::sync::Arc;
use tokio::{sync::mpsc, task::JoinHandle};

use self::simplify::SimplifyReview;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubagentToolSet {
    Generic,
    Simplify,
    GenericAndSimplify,
}

impl SubagentToolSet {
    const fn generic(self) -> bool {
        matches!(self, Self::Generic | Self::GenericAndSimplify)
    }

    const fn simplify(self) -> bool {
        matches!(self, Self::Simplify | Self::GenericAndSimplify)
    }
}

pub(crate) fn install_tools(
    tools: Tools,
    parent: AgentHandle,
    registry: Arc<Registry>,
    tool_set: SubagentToolSet,
) -> Result<Tools, ToolsBuildError> {
    let tools = if tool_set.generic() {
        nanocodex_subagents::install_tools(tools, parent.clone(), Arc::clone(&registry))?
    } else {
        tools
    };
    if tool_set.simplify() {
        tools
            .into_builder()
            .tool(SimplifyReview::new(parent, Arc::downgrade(&registry)))
            .build()
    } else {
        Ok(tools)
    }
}

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

    pub(crate) async fn shutdown(&self) {
        drop(self.control.close_all(&self.root_session_id).await);
        if let Some(update_task) = self.update_task.lock().await.take() {
            update_task.abort();
            drop(update_task.await);
        }
    }
}
