//! Probe trait — wire_doctor の検査素子。
//!
//! 1 Probe = 1 kind が default、 1 Probe で複数 kind emit も許容。
//! 全 Probe は default で registry に埋め込まれる (registry::default)。

use crate::application::doctor::finding::{Axis, Finding};
use crate::domain::error::WireResult;
use crate::infrastructure::storage::SqliteStorage;

/// Probe 走査時の context。 `persona_filter` が Some なら persona-scoped mode。
pub struct ProbeCtx<'a> {
    pub storage: &'a SqliteStorage,
    pub persona_filter: Option<String>,
}

/// Finding の蓄積先。
#[derive(Default)]
pub struct FindingSink {
    items: Vec<Finding>,
}

impl FindingSink {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push(&mut self, f: Finding) {
        self.items.push(f);
    }

    pub fn into_vec(self) -> Vec<Finding> {
        self.items
    }
}

/// 検査素子。 doctor は registry から Vec<Box<dyn Probe>> を取り順次 scan を呼ぶ。
pub trait Probe: Send + Sync {
    fn axis(&self) -> Axis;
    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()>;
}
