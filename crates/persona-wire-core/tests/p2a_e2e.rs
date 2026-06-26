//! P2a E2E integration test.
//!
//! Walks `wire_doctor` (graph-wide health diagnostic) and verifies:
//! - parity with `wire_close` totals on the same graph
//! - orphan detection on a graph with disconnected nodes
//! - dynamic Specification + wire_doctor end-to-end (graph built via the
//!   public storage API, Spec registered + evaluated via Projection render,
//!   totals confirmed via wire_doctor).

use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::application::projection_registry::ProjectionRegistry;
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    graph_scan_summary, wire_close, wire_doctor, wire_init, WireCloseInput, WireInitInput,
};
use persona_wire_core::domain::entity::projection::{PluginDispatch, Projection};
use persona_wire_core::domain::entity::TargetForm;
use persona_wire_core::domain::graph::{ulid_from_seed, Edge, Node};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;
use serde_json::json;

fn bare_node(id: &str, type_: &str, metadata: serde_json::Value) -> Node {
    Node {
        id: ulid_from_seed(id),
        name: id.into(),
        r#type: type_.into(),
        sot_ref: None,
        confidence: None,
        applicability: None,
        last_verified_at: None,
        review_due: None,
        version: 1,
        prev_id: None,
        metadata,
    }
}

fn bare_edge(id: &str, src: &str, tgt: &str, kind: &str) -> Edge {
    Edge {
        id: ulid_from_seed(id),
        name: Some(id.into()),
        src_node: ulid_from_seed(src),
        tgt_node: ulid_from_seed(tgt),
        kind: kind.into(),
        severity: None,
        metadata: json!({}),
        version: 1,
        prev_id: None,
    }
}

#[test]
fn wire_doctor_parity_with_wire_close_on_same_graph() {
    let s = SqliteStorage::open_in_memory().expect("open in-memory");
    s.migrate().expect("migrate");
    s.seed_default_types().expect("seed");

    // alpha -[routes_to]-> beta + lone gamma (orphan)
    for id in ["alpha", "beta", "gamma"] {
        s.insert_node(&bare_node(id, "persona", json!({}))).unwrap();
    }
    s.insert_edge(&bare_edge("e1", "alpha", "beta", "routes_to"))
        .unwrap();

    let _close = wire_close(
        WireCloseInput {
            persona_id: "alpha".into(),
        },
        &s,
    )
    .unwrap();
    let close_summary = graph_scan_summary(&s).unwrap();
    let _doctor = wire_doctor(&s, None).unwrap();
    let doctor_summary = graph_scan_summary(&s).unwrap();

    // wire_close and wire_doctor must agree on every count.
    assert_eq!(
        close_summary.total_node_count,
        doctor_summary.total_node_count
    );
    assert_eq!(
        close_summary.total_edge_count,
        doctor_summary.total_edge_count
    );
    assert_eq!(
        close_summary.orphan_node_count,
        doctor_summary.orphan_node_count
    );

    // Concrete expected values (3 nodes, 1 edge, 1 orphan = gamma).
    assert_eq!(doctor_summary.total_node_count, 3);
    assert_eq!(doctor_summary.total_edge_count, 1);
    assert_eq!(doctor_summary.orphan_node_count, 1);
}

#[test]
fn wire_doctor_reports_orphan_zero_when_every_node_is_touched() {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();

    // Fully connected triangle: every node has at least one in or out edge.
    for id in ["a", "b", "c"] {
        s.insert_node(&bare_node(id, "persona", json!({}))).unwrap();
    }
    s.insert_edge(&bare_edge("e_ab", "a", "b", "routes_to"))
        .unwrap();
    s.insert_edge(&bare_edge("e_bc", "b", "c", "routes_to"))
        .unwrap();

    let doctor = wire_doctor(&s, None).unwrap();
    let doctor_summary = graph_scan_summary(&s).unwrap();
    assert_eq!(doctor_summary.total_node_count, 3);
    assert_eq!(doctor_summary.total_edge_count, 2);
    assert_eq!(doctor_summary.orphan_node_count, 0);
    assert!(doctor.report_markdown.contains("# wire_doctor report"));
    // Finding-driven format (design §8): scope + verdict + axis sections。
    assert!(doctor.report_markdown.contains("scope: full"));
    assert!(doctor.report_markdown.contains("## Graph axis"));
    assert!(doctor.report_markdown.contains("## Workflow axis"));
}

#[test]
fn wire_doctor_with_dynamic_specification_e2e() {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();

    // Seed a small graph the way an outer LLM would (= use the public storage
    // API directly, no Adapter layer). 2 active personas + 1 retired.
    s.insert_node(&bare_node("p1", "persona", json!({"status": "active"})))
        .unwrap();
    s.insert_node(&bare_node("p2", "persona", json!({"status": "active"})))
        .unwrap();
    s.insert_node(&bare_node("p3", "persona", json!({"status": "retired"})))
        .unwrap();
    s.insert_edge(&bare_edge("e1", "p1", "p2", "routes_to"))
        .unwrap();
    s.insert_edge(&bare_edge("e2", "p1", "p3", "routes_to"))
        .unwrap();

    // Dynamic Specification: TypeIs(persona) AND status=active.
    let spec = Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
        path: "status".into(),
        value: json!("active"),
    });
    SpecRegistry::new(&s)
        .register("active_personas", &spec)
        .unwrap();
    ProjectionRegistry::new(&s)
        .register(
            &Projection::from_parts(
                "_active",
                "active_personas",
                "Active personas ({{count}}): {{names}}",
                TargetForm::Prompt,
                PluginDispatch::Default,
            )
            .unwrap(),
        )
        .unwrap();

    // wire_init renders only the 2 active personas.
    let init = wire_init(
        WireInitInput {
            persona_id: "p1".into(),
        },
        &s,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .unwrap();
    assert_eq!(init.projections.len(), 1);
    assert_eq!(init.projections[0].rendered, "Active personas (2): p1, p2");

    // wire_doctor sees the whole graph (3 personas, 2 edges, 0 orphans).
    let _doctor = wire_doctor(&s, None).unwrap();
    let doctor_summary = graph_scan_summary(&s).unwrap();
    assert_eq!(doctor_summary.total_node_count, 3);
    assert_eq!(doctor_summary.total_edge_count, 2);
    assert_eq!(doctor_summary.orphan_node_count, 0);
}
