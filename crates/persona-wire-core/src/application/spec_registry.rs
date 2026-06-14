//! Specification registry — store dynamic / composable Specifications by name.
//!
//! Domain-neutral: callers register arbitrary Specifications (BP: Specification pattern).

use crate::domain::specification::Specification;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct SpecRegistry {
    inner: HashMap<String, Specification>,
}

impl SpecRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>, spec: Specification) {
        self.inner.insert(name.into(), spec);
    }

    pub fn get(&self, name: &str) -> Option<&Specification> {
        self.inner.get(name)
    }

    pub fn list(&self) -> Vec<&str> {
        self.inner.keys().map(String::as_str).collect()
    }
}
