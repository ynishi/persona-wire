//! # graph.edges_zero
//!
//! Detects the "fresh database" anomaly: a graph with zero edges has not
//! had its wire layer provisioned yet. Emitted as [`Severity::Error`] in
//! Full mode only.
//!
//! ## Persona-scoped mode skip (Phase A)
//!
//! When [`ProbeCtx::persona_filter`] is `Some`, [`GraphEdgesZero::scan`]
//! returns immediately without emitting any finding. In Phase A a single
//! persona is **not** the edge-based target of the graph axis — its
//! operation is closed over the persona-pack overlay, [Wiring entries],
//! and [Workflow triggers] — so reporting `edges=0` as a hard failure
//! for one persona is a design mismatch (cf. issue `9f70b493`).
//!
//! ## Lifting the Phase A skip (Phase β path)
//!
//! When persona-to-persona graph wiring is formalised, the early return
//! in [`GraphEdgesZero::scan`] should be removed. Phase β prerequisites:
//!
//! 1. A persona-to-persona edge type (e.g. `routes_to_persona`) is
//!    registered in `type_registry`.
//! 2. `persona-pack` declares an opt-in flag (under `[extra.persona_wire]`
//!    or its successor) for "subject to graph-axis health checks".
//! 3. A migration issue lands that removes the
//!    `ctx.persona_filter.is_some()` early-return.
//!
//! ## `workflow_def` exclusion (phase-invariant)
//!
//! [`workflow_def`] Nodes are excluded from the edge tally below. A
//! Workflow Entity completes its lifecycle via trigger/action and never
//! participates in edge-based wiring; counting its (always-zero) edges
//! would mask real "wire not provisioned" signals once a single workflow
//! is registered. This exclusion is invariant across Phase A/β.
//!
//! [`workflow_def`]: crate::application::workflow_mapper::WORKFLOW_TYPE
//! [Wiring entries]: crate::application::wiring_mapper
//! [Workflow triggers]: crate::domain::entity::workflow

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::workflow_mapper::WORKFLOW_TYPE;
use crate::domain::error::WireResult;

pub struct GraphEdgesZero;

impl Probe for GraphEdgesZero {
    fn axis(&self) -> Axis {
        Axis::Graph
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        // Phase A skip — see module-level docs (issue `9f70b493`).
        if ctx.persona_filter.is_some() {
            return Ok(());
        }
        let storage = ctx.storage;
        let mut total_edges = 0_usize;
        for t in storage.list_types_by_kind("node")? {
            // `workflow_def` exclusion — see module-level docs (issue `f3bb100e`).
            if t == WORKFLOW_TYPE {
                continue;
            }
            for n in storage.list_nodes_by_type(&t)? {
                total_edges += storage.list_edges_from(&n.id)?.len();
            }
        }
        if total_edges == 0 {
            let kind = Kind::GraphEdgesZero;
            sink.push(Finding {
                severity: Severity::Error,
                axis: kind.axis(),
                kind,
                location: Location::default(),
                description: "graph has zero edges (wire not provisioned)".to_string(),
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

    #[test]
    fn persona_scoped_mode_skips_probe() {
        // issue 9f70b493 regression: Phase A persona-scoped mode は graph axis
        // の applicable 対象外、 edges_zero finding を emit してはいけない
        // (= persona-scoped wire_doctor が edges=0 で BROKEN 化する false-positive 除去)。
        let s = setup();
        s.insert_node(&bare_persona_node("carol")).unwrap();
        let f = scan(&GraphEdgesZero, &s, Some("carol")).unwrap();
        assert!(
            f.is_empty(),
            "persona-scoped mode must not emit edges_zero finding: {f:?}"
        );
    }

    #[test]
    fn full_mode_still_emits_when_no_edges() {
        // Q1 fix の non-regression check: Full mode (persona_filter=None) は
        // 従来通り Error で emit (initial 状態 hard fail を維持)。
        let s = setup();
        s.insert_node(&bare_persona_node("a")).unwrap();
        let f = scan(&GraphEdgesZero, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn workflow_def_node_does_not_contribute_to_edge_count() {
        // issue f3bb100e sibling: workflow_def Node を edge count 集計対象から
        // 除外 (= workflow node に edge が無くても "wire 未投入" シグナルとして
        // 機能しない)。
        let s = setup();
        let wf = workflow_node(
            "carol.workflow.session_close",
            Some("carol"),
            serde_json::json!({"kind": "on_event", "event": "session_close"}),
            serde_json::json!({"kind": "no_op"}),
            true,
        );
        s.insert_node(&wf).unwrap();
        // workflow node 1 個のみ + edge 0 → Full mode で依然 edges_zero emit
        let f = scan(&GraphEdgesZero, &s, None).unwrap();
        assert_eq!(f.len(), 1, "workflow-only graph still has zero real edges");
    }
}
