//! CRUD primitive — node/edge level commands routed to the storage port.

use crate::domain::graph::{Edge, EdgeId, Node, NodeId};

#[derive(Debug, Clone)]
pub enum CrudCommand {
    CreateNode(Node),
    UpdateNode(Node),
    DeleteNode(NodeId),
    CreateEdge(Edge),
    UpdateEdge(Edge),
    DeleteEdge(EdgeId),
}
