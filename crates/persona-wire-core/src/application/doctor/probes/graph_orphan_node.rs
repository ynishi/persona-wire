//! graph.orphan_node — in/out edge ゼロ + 自己参照なしの node を warn で emit。
//!
//! design.md §6 entry。 既存 `use_cases::graph_scan_summary` の orphan 判定
//! ロジック (`is_self_attached_wiring`) を node 単位で再走査する。

use crate::application::doctor::finding::{Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::use_cases;
use crate::domain::error::WireResult;
use crate::domain::graph::Node;
use crate::application::doctor::finding::Axis;

pub struct GraphOrphanNode;

impl Probe for GraphOrphanNode {
    fn axis(&self) -> Axis {
        Axis::Graph
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        let storage = ctx.storage;
        for t in storage.list_types_by_kind("node")? {
            for n in storage.list_nodes_by_type(&t)? {
                if !matches_persona_filter(&n, ctx.persona_filter.as_deref()) {
                    continue;
                }
                let out_edges = storage.list_edges_from(&n.id)?;
                let in_edges = storage.list_edges_to(&n.id)?;
                if !out_edges.is_empty() || !in_edges.is_empty() {
                    continue;
                }
                if use_cases::is_self_attached_wiring(&n) {
                    continue;
                }
                let persona_id = node_persona(&n);
                let kind = Kind::GraphOrphanNode;
                sink.push(Finding {
                    severity: Severity::Warn,
                    axis: kind.axis(),
                    kind,
                    location: Location {
                        node_id: Some(n.id.clone()),
                        persona_id,
                        ..Default::default()
                    },
                    description: format!(
                        "node `{}` has zero in/out edges and is not self-attached",
                        n.id
                    ),
                    fix: format!(
                        "`mcp__persona-wire__wire_edge_create(...)` to wire it, or \
                         `mcp__persona-wire__wire_node_delete(node_id=\"{}\")`",
                        n.id
                    ),
                });
            }
        }
        Ok(())
    }
}

fn node_persona(n: &Node) -> Option<String> {
    n.metadata
        .as_object()
        .and_then(|m| m.get("persona"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn matches_persona_filter(n: &Node, filter: Option<&str>) -> bool {
    let Some(want) = filter else {
        return true;
    };
    node_persona(n).as_deref() == Some(want)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::doctor::finding::Kind;
    use crate::application::doctor::test_helpers::*;

    #[test]
    fn emits_warn_for_bare_orphan_persona() {
        let s = setup();
        s.insert_node(&bare_persona_node("solo")).unwrap();
        let f = scan(&GraphOrphanNode, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::GraphOrphanNode);
        assert_eq!(f[0].severity, Severity::Warn);
        assert_eq!(f[0].location.node_id.as_deref(), Some("solo"));
    }

    #[test]
    fn quiet_when_node_has_edge() {
        let s = setup();
        s.insert_node(&bare_persona_node("a")).unwrap();
        s.insert_node(&bare_persona_node("b")).unwrap();
        s.insert_edge(&edge("e1", "a", "b")).unwrap();
        let f = scan(&GraphOrphanNode, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
