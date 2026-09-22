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

#[cfg(test)]
mod tests {
    use super::TaskTree;
    use crate::AgentId;

    fn insert(tree: &mut TaskTree, session_id: &str, parent: Option<AgentId>) -> AgentId {
        let id = tree.reserve(parent).unwrap();
        tree.insert(id, session_id.to_owned(), parent).unwrap();
        id
    }

    #[test]
    fn sessions_resolve_to_agents() {
        let mut tree = TaskTree::default();
        let parent = insert(&mut tree, "parent", None);
        let child = insert(&mut tree, "child", Some(parent));
        insert(&mut tree, "sibling", None);

        assert_eq!(tree.agent_for_session("child"), Some(child));
        assert_eq!(tree.agent_for_session("parent"), Some(parent));
    }

    #[test]
    fn agents_can_only_authorize_descendants() {
        let mut tree = TaskTree::default();
        let parent = insert(&mut tree, "parent", None);
        let child = insert(&mut tree, "child", Some(parent));
        let sibling = insert(&mut tree, "sibling", None);

        assert!(tree.authorize("parent", child).is_ok());
        assert!(tree.authorize("parent", sibling).is_err());
        assert!(tree.authorize("child", parent).is_err());
        assert!(tree.authorize("root", sibling).is_ok());
    }

    #[test]
    fn postorder_places_every_descendant_before_its_parent() {
        let mut tree = TaskTree::default();
        let parent = insert(&mut tree, "parent", None);
        let child = insert(&mut tree, "child", Some(parent));
        let grandchild = insert(&mut tree, "grandchild", Some(child));
        let sibling = insert(&mut tree, "sibling", None);

        assert_eq!(
            tree.subtree_postorder(parent).unwrap(),
            [grandchild, child, parent]
        );
        assert_eq!(tree.all_postorder(), [grandchild, child, parent, sibling]);
    }
}
