// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Deterministic world-space layout for the subagent hierarchy.

use nanocodex_subagents::AgentId;
use std::collections::{HashMap, HashSet};

pub(super) const NODE_WIDTH: i32 = 24;
pub(super) const NODE_HEIGHT: i32 = 4;
pub(super) const HORIZONTAL_GAP: i32 = 6;
pub(super) const VERTICAL_GAP: i32 = 5;

#[derive(Clone, Copy)]
pub(super) struct LayoutNode {
    pub(super) id: AgentId,
    pub(super) parent: Option<AgentId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct WorldPoint {
    pub(super) x: f64,
    pub(super) y: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NodePosition {
    pub(super) center_x: i32,
    pub(super) top: i32,
}

pub(super) struct TreeLayout {
    positions: HashMap<AgentId, NodePosition>,
    parents: HashMap<AgentId, AgentId>,
    children: HashMap<AgentId, Vec<AgentId>>,
    roots: Vec<AgentId>,
}

impl TreeLayout {
    pub(super) fn new(nodes: &[LayoutNode]) -> Self {
        let ids = nodes.iter().map(|node| node.id).collect::<HashSet<_>>();
        let mut parents = HashMap::new();
        let mut children = HashMap::<AgentId, Vec<AgentId>>::new();

        for node in nodes {
            let parent = node.parent.filter(|parent| ids.contains(parent));
            if let Some(parent) = parent {
                parents.insert(node.id, parent);
                children.entry(parent).or_default().push(node.id);
            }
            children.entry(node.id).or_default();
        }
        for descendants in children.values_mut() {
            descendants.sort_unstable();
        }
        break_parent_cycles(&ids, &mut parents, &mut children);
        let mut roots = ids
            .iter()
            .copied()
            .filter(|id| !parents.contains_key(id))
            .collect::<Vec<_>>();
        roots.sort_unstable();

        let mut measured = HashMap::new();
        let mut visiting = HashSet::new();
        for &root in &roots {
            measure_subtree(root, &children, &mut measured, &mut visiting);
        }
        for node in nodes {
            if !measured.contains_key(&node.id) {
                roots.push(node.id);
                parents.remove(&node.id);
                measure_subtree(node.id, &children, &mut measured, &mut visiting);
            }
        }

        let forest_width = roots
            .iter()
            .map(|id| measured.get(id).copied().unwrap_or(NODE_WIDTH))
            .sum::<i32>()
            + HORIZONTAL_GAP * i32::try_from(roots.len().saturating_sub(1)).unwrap_or(i32::MAX);
        let mut left = -forest_width / 2;
        let mut positions = HashMap::new();
        let mut placed = HashSet::new();
        for &root in &roots {
            let width = measured.get(&root).copied().unwrap_or(NODE_WIDTH);
            place_subtree(
                root,
                left,
                0,
                &children,
                &measured,
                &mut positions,
                &mut placed,
            );
            left += width + HORIZONTAL_GAP;
        }

        Self {
            positions,
            parents,
            children,
            roots,
        }
    }

    pub(super) fn position(&self, id: AgentId) -> Option<NodePosition> {
        self.positions.get(&id).copied()
    }

    pub(super) fn center(&self, id: AgentId) -> Option<WorldPoint> {
        self.position(id).map(|position| WorldPoint {
            x: f64::from(position.center_x),
            y: f64::from(position.top + NODE_HEIGHT / 2),
        })
    }

    pub(super) fn parent(&self, id: AgentId) -> Option<AgentId> {
        self.parents.get(&id).copied()
    }

    pub(super) fn children(&self, id: AgentId) -> &[AgentId] {
        self.children.get(&id).map_or(&[], Vec::as_slice)
    }

    pub(super) fn roots(&self) -> &[AgentId] {
        &self.roots
    }

    pub(super) fn positioned_nodes(&self) -> impl Iterator<Item = (AgentId, NodePosition)> + '_ {
        self.positions.iter().map(|(&id, &position)| (id, position))
    }
}

fn break_parent_cycles(
    ids: &HashSet<AgentId>,
    parents: &mut HashMap<AgentId, AgentId>,
    children: &mut HashMap<AgentId, Vec<AgentId>>,
) {
    let mut ordered = ids.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    for start in ordered {
        let mut seen = HashSet::new();
        let mut current = start;
        seen.insert(current);
        while let Some(parent) = parents.get(&current).copied() {
            if !seen.insert(parent) {
                parents.remove(&current);
                if let Some(siblings) = children.get_mut(&parent) {
                    siblings.retain(|&child| child != current);
                }
                break;
            }
            current = parent;
        }
    }
}

fn measure_subtree(
    id: AgentId,
    children: &HashMap<AgentId, Vec<AgentId>>,
    measured: &mut HashMap<AgentId, i32>,
    visiting: &mut HashSet<AgentId>,
) -> i32 {
    if let Some(&width) = measured.get(&id) {
        return width;
    }
    if !visiting.insert(id) {
        return NODE_WIDTH;
    }

    let descendants = children.get(&id).map_or(&[][..], Vec::as_slice);
    let children_width = descendants
        .iter()
        .map(|&child| measure_subtree(child, children, measured, visiting))
        .sum::<i32>()
        + HORIZONTAL_GAP * i32::try_from(descendants.len().saturating_sub(1)).unwrap_or(i32::MAX);
    let width = NODE_WIDTH.max(children_width);
    visiting.remove(&id);
    measured.insert(id, width);
    width
}

#[allow(clippy::too_many_arguments)]
fn place_subtree(
    id: AgentId,
    left: i32,
    depth: i32,
    children: &HashMap<AgentId, Vec<AgentId>>,
    measured: &HashMap<AgentId, i32>,
    positions: &mut HashMap<AgentId, NodePosition>,
    placed: &mut HashSet<AgentId>,
) {
    if !placed.insert(id) {
        return;
    }
    let width = measured.get(&id).copied().unwrap_or(NODE_WIDTH);
    positions.insert(
        id,
        NodePosition {
            center_x: left + width / 2,
            top: depth * (NODE_HEIGHT + VERTICAL_GAP),
        },
    );

    let descendants = children.get(&id).map_or(&[][..], Vec::as_slice);
    let children_width = descendants
        .iter()
        .map(|child| measured.get(child).copied().unwrap_or(NODE_WIDTH))
        .sum::<i32>()
        + HORIZONTAL_GAP * i32::try_from(descendants.len().saturating_sub(1)).unwrap_or(i32::MAX);
    let mut child_left = left + (width - children_width) / 2;
    for &child in descendants {
        let child_width = measured.get(&child).copied().unwrap_or(NODE_WIDTH);
        place_subtree(
            child,
            child_left,
            depth + 1,
            children,
            measured,
            positions,
            placed,
        );
        child_left += child_width + HORIZONTAL_GAP;
    }
}
