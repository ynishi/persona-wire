//! Repository port — abstract graph persistence operations.
//!
//! Implemented by `infrastructure::storage::SqliteStorage`, and can be
//! mocked for in-memory tests of Domain Core / Application use cases.

use crate::domain::autoversion::{VersionRecord, VersionTargetKind};
use crate::domain::error::WireResult;
use crate::domain::graph::{Edge, EdgeId, Node, NodeId};

pub trait Repository {
    fn list_types_by_kind(&self, kind: &str) -> WireResult<Vec<String>>;

    fn insert_node(&self, node: &Node) -> WireResult<()>;
    fn get_node(&self, id: &NodeId) -> WireResult<Option<Node>>;
    fn list_nodes_by_type(&self, type_name: &str) -> WireResult<Vec<Node>>;

    fn insert_edge(&self, edge: &Edge) -> WireResult<()>;
    fn get_edge(&self, id: &EdgeId) -> WireResult<Option<Edge>>;
    fn list_edges_from(&self, src_node: &NodeId) -> WireResult<Vec<Edge>>;
    fn list_edges_to(&self, tgt_node: &NodeId) -> WireResult<Vec<Edge>>;

    fn insert_version_record(&self, rec: &VersionRecord) -> WireResult<()>;
    fn count_versions(&self, target_kind: VersionTargetKind, target_id: &str) -> WireResult<i64>;
}
