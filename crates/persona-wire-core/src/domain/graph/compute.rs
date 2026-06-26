//! Compute primitive — traversal + execution + constraint eval.
//!
//! Handles 3 axes within one primitive:
//! 1. Traversal     — BFS graph walk bounded by `max_depth` (P1: 1 hop default)
//! 2. Execution     — workflow run / step transition (P5 carry)
//! 3. Constraint    — bulk eval of constraint-kind edges (P3 carry)

use std::collections::{HashSet, VecDeque};

use crate::domain::error::WireResult;
use crate::domain::graph::{Node, NodeId};
use crate::domain::repository::Repository;
use crate::domain::specification::Specification;

#[derive(Debug, Clone)]
pub struct TraversalResult {
    pub nodes: Vec<Node>,
    pub depth_reached: u32,
}

/// BFS traverse from `start`, visiting each reachable node up to `max_depth`
/// hops via `repo.list_edges_from`. Each visited node is run through
/// `spec.is_satisfied_by` and collected if it matches.
///
/// Edges are followed in source→target direction only (1-direction reachability).
/// `Specification::Reachable` variants inside `spec` evaluate to `false` here
/// because predicate-level eval cannot recurse; compose `spec` with
/// `TypeIs` / `MetadataEq` instead.
pub fn traverse(
    start: &NodeId,
    spec: &Specification,
    max_depth: u32,
    repo: &dyn Repository,
) -> WireResult<TraversalResult> {
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<(NodeId, u32)> = VecDeque::new();
    let mut matched: Vec<Node> = Vec::new();
    let mut depth_reached: u32 = 0;

    queue.push_back((*start, 0));

    while let Some((id, depth)) = queue.pop_front() {
        if !visited.insert(id) {
            continue;
        }
        if depth > depth_reached {
            depth_reached = depth;
        }

        let Some(node) = repo.get_node(&id)? else {
            continue;
        };

        if spec.is_satisfied_by(&node) {
            matched.push(node);
        }

        if depth < max_depth {
            for edge in repo.list_edges_from(&id)? {
                if !visited.contains(&edge.tgt_node) {
                    queue.push_back((edge.tgt_node, depth + 1));
                }
            }
        }
    }

    Ok(TraversalResult {
        nodes: matched,
        depth_reached,
    })
}
