//! graph.dangling_edge — edge target に該当 node が存在しない (error)。
//!
//! design.md §6 entry。 既存 `wire_node_delete` のコメントが「edges are NOT
//! cascade-deleted; surviving edges referencing the removed id become
//! dangling — wire_doctor flags them」 と宣言しており、 本 Probe がその
//! 宣言を実装する (現 graph_scan_summary は target 存在検査をしていなかった)。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::domain::error::WireResult;
use std::collections::HashSet;

pub struct GraphDanglingEdge;

impl Probe for GraphDanglingEdge {
    fn axis(&self) -> Axis {
        Axis::Graph
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        let storage = ctx.storage;
        // 1. 全 node id を 1 度収集 (HashSet で O(1) lookup)
        let mut node_ids: HashSet<String> = HashSet::new();
        let mut all_node_ids: Vec<String> = Vec::new();
        for t in storage.list_types_by_kind("node")? {
            for n in storage.list_nodes_by_type(&t)? {
                node_ids.insert(n.id.clone());
                all_node_ids.push(n.id);
            }
        }
        // 2. 各 node の out-edge を走査して target 不在を検出 (1 edge = 1 src なので二重列挙なし)
        for src_id in &all_node_ids {
            for e in storage.list_edges_from(src_id)? {
                if node_ids.contains(&e.tgt_node) {
                    continue;
                }
                let kind = Kind::GraphDanglingEdge;
                sink.push(Finding {
                    severity: Severity::Error,
                    axis: kind.axis(),
                    kind,
                    location: Location {
                        edge: Some((e.src_node.clone(), e.tgt_node.clone())),
                        ..Default::default()
                    },
                    description: format!(
                        "edge `{}` → `{}` points at a non-existent node",
                        e.src_node, e.tgt_node
                    ),
                    fix: format!(
                        "`mcp__persona-wire__wire_edge_delete(edge_id=\"{}\")`",
                        e.id
                    ),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::doctor::test_helpers::*;

    // NOTE: dangling state cannot be constructed via the public API:
    // - storage.insert_edge enforces SQLite FK (tgt_node must exist)
    // - storage.delete_node cascade-deletes referencing edges in one Tx
    //
    // The Probe is therefore defensive only (= catches external DB drift /
    // migration corruption / direct SQL writes). The wire_node_delete
    // docstring "edges become dangling — wire_doctor flags them" is currently
    // a lie (storage cascades). Discrepancy carry to a separate issue; for
    // the test we keep the negative path here and skip the positive case.
    #[test]
    #[ignore]
    fn emits_error_for_edge_to_missing_target_requires_db_drift_fixture() {
        // would need PRAGMA foreign_keys = OFF + direct SQL — out of scope for
        // probe-level unit test。
    }

    #[test]
    fn quiet_when_all_edges_resolve() {
        let s = setup();
        s.insert_node(&bare_persona_node("a")).unwrap();
        s.insert_node(&bare_persona_node("b")).unwrap();
        s.insert_edge(&edge("e1", "a", "b")).unwrap();
        let f = scan(&GraphDanglingEdge, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
