//! graph.edges_zero — graph 全体で edge 0 件を error で emit。
//!
//! design.md §6 entry。 wire 配線が一括投入されていない初期状態を hard fail。
//! persona-scoped mode では persona の関与 edge 数で判定する。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::domain::error::WireResult;

pub struct GraphEdgesZero;

impl Probe for GraphEdgesZero {
    fn axis(&self) -> Axis {
        Axis::Graph
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        let storage = ctx.storage;
        let mut total_edges = 0_usize;
        for t in storage.list_types_by_kind("node")? {
            for n in storage.list_nodes_by_type(&t)? {
                if let Some(want) = ctx.persona_filter.as_deref() {
                    let persona = n
                        .metadata
                        .as_object()
                        .and_then(|m| m.get("persona"))
                        .and_then(|v| v.as_str());
                    if persona != Some(want) {
                        continue;
                    }
                }
                total_edges += storage.list_edges_from(&n.id)?.len();
            }
        }
        if total_edges == 0 {
            let kind = Kind::GraphEdgesZero;
            sink.push(Finding {
                severity: Severity::Error,
                axis: kind.axis(),
                kind,
                location: Location {
                    persona_id: ctx.persona_filter.clone(),
                    ..Default::default()
                },
                description: match ctx.persona_filter.as_deref() {
                    Some(p) => format!("persona `{p}` has zero edges (wire not provisioned)"),
                    None => "graph has zero edges (wire not provisioned)".to_string(),
                },
                fix: "bulk wire investment via `mcp__persona-wire__wire_edges_create_batch(...)`"
                    .to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::doctor::finding::Kind;
    use crate::application::doctor::test_helpers::*;

    #[test]
    fn emits_error_when_no_edges() {
        let s = setup();
        s.insert_node(&bare_persona_node("a")).unwrap();
        let f = scan(&GraphEdgesZero, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::GraphEdgesZero);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn quiet_when_at_least_one_edge_exists() {
        let s = setup();
        s.insert_node(&bare_persona_node("a")).unwrap();
        s.insert_node(&bare_persona_node("b")).unwrap();
        s.insert_edge(&edge("e1", "a", "b")).unwrap();
        let f = scan(&GraphEdgesZero, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
