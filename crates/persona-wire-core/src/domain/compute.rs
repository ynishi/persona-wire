//! Compute primitive — traversal + execution + constraint eval.
//!
//! Handles 3 axes within one primitive:
//! 1. Traversal     — graph walk (BFS / DFS / multi-hop) bounded by Specification
//! 2. Execution     — workflow run / step transition (uses transitions_to edges)
//! 3. Constraint    — bulk evaluation of constraint-kind edges across a subgraph

use crate::domain::graph::{Node, NodeId};
use crate::domain::specification::Specification;

#[derive(Debug, Clone)]
pub struct TraversalResult {
    pub nodes: Vec<Node>,
    pub depth_reached: u32,
}

/// Traverse the graph from `start` while `spec` holds; bounded by `max_depth`.
/// Returns the matched node subset.
pub fn traverse(_start: &NodeId, _spec: &Specification, _max_depth: u32) -> TraversalResult {
    // TODO(P1): wire to graph port and walk.
    TraversalResult {
        nodes: Vec::new(),
        depth_reached: 0,
    }
}
