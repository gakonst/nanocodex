// Derived from clabby/tact@1d9ccaefd1d8613dab020812af04a91cd9b4c52c (Apache-2.0).
// Modified for Nanocodex's reusable native/WASM extension runtime.

//! Synchronous subagent topology and descendant authorization.

use super::model::AgentId;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct TaskTree {
    next_id: u64,
    nodes: HashMap<AgentId, TaskNode>,
}

struct TaskNode {
    session_id: String,
    parent: Option<AgentId>,
}

impl TaskTree {
    pub(super) fn reserve(&mut self, parent: Option<AgentId>) -> std::io::Result<AgentId> {
        if let Some(parent) = parent
            && !self.nodes.contains_key(&parent)
        {
            return Err(std::io::Error::other(format!(
                "unknown parent_agent_id {parent}"
            )));
        }

        Ok(AgentId::next(&mut self.next_id))
    }

    pub(super) fn insert(
        &mut self,
        id: AgentId,
        session_id: String,
        parent: Option<AgentId>,
    ) -> std::io::Result<()> {
        if id.get() == 0 {
            return Err(std::io::Error::other("agent ID must be greater than zero"));
        }
        if id.get() == u64::MAX {
            return Err(std::io::Error::other(
                "agent ID must be less than the maximum u64 value",
            ));
        }
        if self.nodes.contains_key(&id) {
            return Err(std::io::Error::other(format!("duplicate agent_id {id}")));
        }
        if self.agent_for_session(&session_id).is_some() {
            return Err(std::io::Error::other(format!(
                "duplicate subagent session ID {session_id}"
            )));
        }
        if let Some(parent) = parent
            && !self.nodes.contains_key(&parent)
        {
            return Err(std::io::Error::other(format!(
                "unknown parent agent {parent}"
            )));
        }

        self.nodes.insert(id, TaskNode { session_id, parent });
        self.next_id = self.next_id.max(id.get());
        Ok(())
    }

    pub(super) fn contains(&self, id: AgentId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub(super) fn agent_for_session(&self, session_id: &str) -> Option<AgentId> {
        self.nodes
            .iter()
            .find_map(|(&id, node)| (node.session_id == session_id).then_some(id))
    }

    pub(super) fn authorize(&self, session_id: &str, id: AgentId) -> std::io::Result<()> {
        if !self.contains(id) {
            return Err(std::io::Error::other(format!("unknown agent_id {id}")));
        }

        if let Some(caller) = self.agent_for_session(session_id)
            && !self.is_descendant(id, caller)
        {
            return Err(std::io::Error::other(format!(
                "agent {caller} may only manage its descendants"
            )));
        }

        Ok(())
    }

    pub(super) fn ids(&self) -> Vec<AgentId> {
        self.nodes.keys().copied().collect()
    }

    pub(super) fn subtree_postorder(&self, id: AgentId) -> std::io::Result<Vec<AgentId>> {
        if !self.contains(id) {
            return Err(std::io::Error::other(format!("unknown agent_id {id}")));
        }

        let mut order = Vec::new();
        self.append_subtree_postorder(id, &mut order);
        Ok(order)
    }

    pub(super) fn all_postorder(&self) -> Vec<AgentId> {
        let mut roots = self
            .nodes
            .iter()
            .filter_map(|(&id, node)| node.parent.is_none().then_some(id))
            .collect::<Vec<_>>();
        roots.sort_unstable();

        let mut order = Vec::with_capacity(self.nodes.len());
        for root in roots {
            self.append_subtree_postorder(root, &mut order);
        }
        order
    }

    pub(super) fn is_descendant(&self, candidate: AgentId, ancestor: AgentId) -> bool {
        let mut parent = self.nodes.get(&candidate).and_then(|node| node.parent);
        while let Some(id) = parent {
            if id == ancestor {
                return true;
            }
            parent = self.nodes.get(&id).and_then(|node| node.parent);
        }
        false
    }

    fn append_subtree_postorder(&self, id: AgentId, order: &mut Vec<AgentId>) {
        let mut children = self
            .nodes
            .iter()
            .filter_map(|(&child_id, node)| (node.parent == Some(id)).then_some(child_id))
            .collect::<Vec<_>>();
        children.sort_unstable();

        for child in children {
            self.append_subtree_postorder(child, order);
        }
        order.push(id);
    }
}
