//! NamedProjection registry — fixed named query + template + target form binding.
//!
//! BP reference: CQRS Read Model / Projection (Query side).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedProjection {
    pub name: String,
    /// Reference to a registered Specification (by name) or inline serialized spec.
    pub spec_ref: String,
    /// Template body (DSL TBD at P0 — Lua snippet / Tera / minimal mustache).
    pub template: String,
    pub target_form: TargetForm,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetForm {
    Prompt,
    Markdown,
    Json,
    Ascii,
}

#[derive(Debug, Default)]
pub struct ProjectionRegistry {
    inner: HashMap<String, NamedProjection>,
}

impl ProjectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, p: NamedProjection) {
        self.inner.insert(p.name.clone(), p);
    }

    pub fn get(&self, name: &str) -> Option<&NamedProjection> {
        self.inner.get(name)
    }

    pub fn list(&self) -> Vec<&NamedProjection> {
        self.inner.values().collect()
    }
}
