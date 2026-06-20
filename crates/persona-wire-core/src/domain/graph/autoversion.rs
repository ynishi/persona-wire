//! AutoVersion primitive — append-only version chain with diff/rollback.
//!
//! Every update to a node/edge inserts a new row with incremented `version`
//! and `prev_id` pointing to the previous version. Latest row wins for default
//! reads; historical rows kept for diff/rollback.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionRecord {
    pub target_kind: VersionTargetKind,
    pub target_id: String,
    pub version: u32,
    pub diff: serde_json::Value,
    pub ts: i64,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VersionTargetKind {
    Node,
    Edge,
}
