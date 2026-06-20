//! Graph primitive — Node + Edge entities (open vocabulary type system).

use serde::{Deserialize, Serialize};

pub type NodeId = String;
pub type EdgeId = String;
pub type TypeName = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub r#type: TypeName,
    pub sot_ref: Option<String>,
    pub confidence: Option<f64>,
    pub applicability: Option<String>,
    pub last_verified_at: Option<i64>,
    pub review_due: Option<i64>,
    pub version: u32,
    pub prev_id: Option<NodeId>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub src_node: NodeId,
    pub tgt_node: NodeId,
    pub kind: TypeName,
    pub severity: Option<Severity>,
    pub metadata: serde_json::Value,
    pub version: u32,
    pub prev_id: Option<EdgeId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Hard,
    Soft,
    Advisory,
}
