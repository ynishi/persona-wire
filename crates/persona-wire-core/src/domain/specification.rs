//! Specification primitive — first-class composable query object.
//!
//! BP reference: Specification pattern (Evans / Fowler / Greg Young).
//! `Specification` is the **domain object** representing a query predicate;
//! Application layer holds a registry that stores composed Specifications by name.

use crate::domain::graph::Node;
use serde::{Deserialize, Serialize};

/// Specification — composable query predicate over the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Specification {
    /// Match node type literal.
    TypeIs(String),
    /// Match metadata key/value (path expression, value JSON).
    MetadataEq {
        path: String,
        value: serde_json::Value,
    },
    /// Match nodes reachable from `from_node` within `depth` hops via `edge_kind` (None = any).
    Reachable {
        from_node: String,
        edge_kind: Option<String>,
        depth: u32,
    },
    And(Vec<Specification>),
    Or(Vec<Specification>),
    Not(Box<Specification>),
}

impl Specification {
    /// Compose with logical AND.
    pub fn and(self, other: Specification) -> Specification {
        Specification::And(vec![self, other])
    }

    /// Compose with logical OR.
    pub fn or(self, other: Specification) -> Specification {
        Specification::Or(vec![self, other])
    }

    /// Evaluate against a single node (in-memory predicate check).
    ///
    /// Traversal-bound variants (Reachable) require graph context and are
    /// handled by the `compute` primitive, not here.
    pub fn is_satisfied_by(&self, _node: &Node) -> bool {
        // TODO(P1): implement TypeIs / MetadataEq / And / Or / Not branches.
        false
    }
}

impl std::ops::Not for Specification {
    type Output = Specification;

    /// Negate via the `!` operator: `!spec` wraps the spec in `Specification::Not`.
    fn not(self) -> Self::Output {
        Specification::Not(Box::new(self))
    }
}
