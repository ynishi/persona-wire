//! Constraint primitive — evaluate edges of `kind="constraint"` against current graph state.

use crate::domain::graph::Edge;

#[derive(Debug, Clone)]
pub struct ConstraintViolation {
    pub edge_id: String,
    pub message: String,
}

pub fn evaluate(_edge: &Edge) -> Option<ConstraintViolation> {
    // TODO(P3): evaluate metadata.expr (Lua snippet or DSL) per design.
    None
}
