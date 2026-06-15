//! P2c E2E integration test.
//!
//! Walks `wire_nodes_create_batch` + `wire_edges_create_batch` and verifies:
//! - happy-path batch insert (all rows succeed, counts match)
//! - partial insert on duplicate id (stops at the failing index, counts
//!   reflect what was committed so far, error_message surfaces)

use persona_wire_core::application::use_cases::{
    wire_doctor, wire_edges_create_batch, wire_nodes_create_batch, WireEdgesCreateBatchInput,
    WireNodesCreateBatchInput,
};
use persona_wire_core::domain::graph::{Edge, Node};
use persona_wire_core::infrastructure::storage::SqliteStorage;
use serde_json::json;

fn bare_node(id: &str, type_: &str) -> Node {
    Node {
        id: id.into(),
        r#type: type_.into(),
        sot_ref: None,
        confidence: None,
        applicability: None,
        last_verified_at: None,
        review_due: None,
        version: 1,
        prev_id: None,
        metadata: json!({}),
    }
}

fn bare_edge(id: &str, src: &str, tgt: &str, kind: &str) -> Edge {
    Edge {
        id: id.into(),
        src_node: src.into(),
        tgt_node: tgt.into(),
        kind: kind.into(),
        severity: None,
        metadata: json!({}),
        version: 1,
        prev_id: None,
    }
}

#[test]
fn batch_inserts_all_nodes_and_edges_happy_path() {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();

    let nodes = vec![
        bare_node("n1", "persona"),
        bare_node("n2", "persona"),
        bare_node("n3", "persona"),
    ];
    let node_out = wire_nodes_create_batch(WireNodesCreateBatchInput { nodes }, &s).unwrap();
    assert_eq!(node_out.inserted_count, 3);
    assert!(node_out.failed_at.is_none());
    assert!(node_out.error_message.is_none());

    let edges = vec![
        bare_edge("e1", "n1", "n2", "routes_to"),
        bare_edge("e2", "n1", "n3", "routes_to"),
    ];
    let edge_out = wire_edges_create_batch(WireEdgesCreateBatchInput { edges }, &s).unwrap();
    assert_eq!(edge_out.inserted_count, 2);
    assert!(edge_out.failed_at.is_none());

    // wire_doctor confirms the batched graph end-to-end.
    let doctor = wire_doctor(&s).unwrap();
    assert_eq!(doctor.total_node_count, 3);
    assert_eq!(doctor.total_edge_count, 2);
    assert_eq!(doctor.orphan_node_count, 0);
}

#[test]
fn batch_stops_at_first_duplicate_node() {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();

    // Pre-seed an existing node so the second batch entry collides.
    s.insert_node(&bare_node("n_existing", "persona")).unwrap();

    let nodes = vec![
        bare_node("n_fresh1", "persona"),
        bare_node("n_existing", "persona"), // duplicate -> stops here
        bare_node("n_fresh2", "persona"),   // never reached
    ];
    let out = wire_nodes_create_batch(WireNodesCreateBatchInput { nodes }, &s).unwrap();
    assert_eq!(out.inserted_count, 1);
    assert_eq!(out.failed_at, Some(1));
    let msg = out.error_message.expect("error_message must surface");
    assert!(
        msg.to_lowercase().contains("unique") || msg.to_lowercase().contains("constraint"),
        "expected duplicate-key hint, got: {msg}"
    );

    // wire_doctor reflects the partial state: pre-existing + 1 fresh inserted.
    let doctor = wire_doctor(&s).unwrap();
    assert_eq!(doctor.total_node_count, 2);
    assert_eq!(doctor.total_edge_count, 0);
    assert_eq!(doctor.orphan_node_count, 2);
}

#[test]
fn batch_stops_at_first_edge_missing_node() {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();

    // Seed only n1 / n2; e3 will dangle.
    let _ = wire_nodes_create_batch(
        WireNodesCreateBatchInput {
            nodes: vec![bare_node("n1", "persona"), bare_node("n2", "persona")],
        },
        &s,
    )
    .unwrap();

    let edges = vec![
        bare_edge("e1", "n1", "n2", "routes_to"),
        bare_edge("e2", "n2", "n1", "routes_to"),
        // e3 references a node that does not exist -> FK fails.
        bare_edge("e3", "n1", "nonexistent", "routes_to"),
        // never reached
        bare_edge("e4", "n2", "n1", "routes_to"),
    ];
    let out = wire_edges_create_batch(WireEdgesCreateBatchInput { edges }, &s).unwrap();
    assert_eq!(out.inserted_count, 2);
    assert_eq!(out.failed_at, Some(2));
    assert!(out.error_message.is_some());
}
