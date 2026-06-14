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
    /// Traversal-bound variants (`Reachable`) require graph context and are
    /// answered by `compute::traverse`; on a bare node they always return false.
    pub fn is_satisfied_by(&self, node: &Node) -> bool {
        match self {
            Specification::TypeIs(t) => node.r#type == *t,
            Specification::MetadataEq { path, value } => extract_path(&node.metadata, path)
                .map(|v| v == *value)
                .unwrap_or(false),
            Specification::Reachable { .. } => false,
            Specification::And(specs) => specs.iter().all(|s| s.is_satisfied_by(node)),
            Specification::Or(specs) => specs.iter().any(|s| s.is_satisfied_by(node)),
            Specification::Not(s) => !s.is_satisfied_by(node),
        }
    }
}

impl std::ops::Not for Specification {
    type Output = Specification;

    /// Negate via the `!` operator: `!spec` wraps the spec in `Specification::Not`.
    fn not(self) -> Self::Output {
        Specification::Not(Box::new(self))
    }
}

/// Walk a dotted path (`"a.b.c"`) into a JSON value. Empty path returns the root.
fn extract_path(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    if path.is_empty() {
        return Some(value.clone());
    }
    let mut current = value;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, type_: &str, metadata: serde_json::Value) -> Node {
        Node {
            id: id.into(),
            r#type: type_.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata,
        }
    }

    #[test]
    fn type_is_matches_when_equal() {
        let s = Specification::TypeIs("persona".into());
        assert!(s.is_satisfied_by(&node("n", "persona", json!({}))));
        assert!(!s.is_satisfied_by(&node("n", "channel", json!({}))));
    }

    #[test]
    fn metadata_eq_supports_nested_path() {
        let s = Specification::MetadataEq {
            path: "owner.name".into(),
            value: json!("shi"),
        };
        let yes = node("a", "persona", json!({"owner": {"name": "shi"}}));
        let no = node("a", "persona", json!({"owner": {"name": "mia"}}));
        let missing = node("a", "persona", json!({}));
        assert!(s.is_satisfied_by(&yes));
        assert!(!s.is_satisfied_by(&no));
        assert!(!s.is_satisfied_by(&missing));
    }

    #[test]
    fn and_short_circuits_on_false() {
        let s = Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
            path: "owner.name".into(),
            value: json!("shi"),
        });
        let yes = node("a", "persona", json!({"owner": {"name": "shi"}}));
        let no_type = node("a", "channel", json!({"owner": {"name": "shi"}}));
        let no_meta = node("a", "persona", json!({"owner": {"name": "mia"}}));
        assert!(s.is_satisfied_by(&yes));
        assert!(!s.is_satisfied_by(&no_type));
        assert!(!s.is_satisfied_by(&no_meta));
    }

    #[test]
    fn or_unions() {
        let s = Specification::TypeIs("persona".into()).or(Specification::TypeIs("channel".into()));
        assert!(s.is_satisfied_by(&node("a", "persona", json!({}))));
        assert!(s.is_satisfied_by(&node("a", "channel", json!({}))));
        assert!(!s.is_satisfied_by(&node("a", "workflow_def", json!({}))));
    }

    #[test]
    fn not_inverts_via_operator() {
        let s = !Specification::TypeIs("persona".into());
        assert!(!s.is_satisfied_by(&node("a", "persona", json!({}))));
        assert!(s.is_satisfied_by(&node("a", "channel", json!({}))));
    }

    #[test]
    fn reachable_returns_false_on_bare_node() {
        // Reachable needs graph traversal; the predicate alone is false.
        let s = Specification::Reachable {
            from_node: "n0".into(),
            edge_kind: Some("routes_to".into()),
            depth: 1,
        };
        assert!(!s.is_satisfied_by(&node("any", "persona", json!({}))));
    }
}
