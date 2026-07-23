//! P1 E2E integration test.
//!
//! Walks the full pipeline through the public API of `persona-wire-core`:
//! migrate → seed → insert nodes/edges → register Specification → register
//! NamedProjection → wire_init renders → wire_close reports.

use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::application::projection_registry::ProjectionRegistry;
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    graph_scan_summary, wire_close, wire_init, WireCloseInput, WireInitInput,
};
use persona_wire_core::domain::entity::projection::{PluginDispatch, Projection};
use persona_wire_core::domain::entity::TargetForm;
use persona_wire_core::domain::graph::{ulid_from_seed, Edge, Node, Severity};
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

#[test]
fn full_pipeline_init_seed_register_render_close() {
    let storage = SqliteStorage::open_in_memory().expect("open in-memory");
    storage.migrate().expect("migrate");
    storage.seed_default_types().expect("seed");

    // Sanity: type vocabulary is in place.
    let nodes = storage.list_types_by_kind("node").unwrap();
    assert_eq!(nodes.len(), 11, "expected 11 seeded node types");

    // Insert a small persona-routing graph.
    // alpha -[routes_to]-> beta
    // alpha -[routes_to]-> gamma
    // beta -[triggers_review_of severity=hard]-> note1 (outline_node)
    for id in ["alpha", "beta", "gamma"] {
        storage
            .insert_node(&bare_node(id, "persona", json!({"display": id})))
            .unwrap();
    }
    storage
        .insert_node(&bare_node(
            "note1",
            "outline_node",
            json!({"title": "review me"}),
        ))
        .unwrap();

    storage
        .insert_edge(&Edge {
            id: ulid_from_seed("e_alice_carol"),
            name: Some("e_alice_carol".into()),
            src_node: ulid_from_seed("alpha"),
            tgt_node: ulid_from_seed("beta"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
    storage
        .insert_edge(&Edge {
            id: ulid_from_seed("e_alice_dave"),
            name: Some("e_alice_dave".into()),
            src_node: ulid_from_seed("alpha"),
            tgt_node: ulid_from_seed("gamma"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
    storage
        .insert_edge(&Edge {
            id: ulid_from_seed("e_carol_review"),
            name: Some("e_carol_review".into()),
            src_node: ulid_from_seed("beta"),
            tgt_node: ulid_from_seed("note1"),
            kind: "triggers_review_of".into(),
            severity: Some(Severity::Hard),
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();

    // Register Specifications.
    let spec_reg = SpecRegistry::new(&storage);
    spec_reg
        .register("active_personas", &Specification::TypeIs("persona".into()))
        .unwrap();
    spec_reg
        .register(
            "outline_review_targets",
            &Specification::TypeIs("outline_node".into()),
        )
        .unwrap();

    // Register NamedProjections.
    let proj_reg = ProjectionRegistry::new(&storage);
    proj_reg
        .register(
            &Projection::from_parts(
                "_persona_toc",
                "active_personas",
                "Personas ({{count}}): {{names}}",
                TargetForm::Prompt,
                PluginDispatch::Default,
            )
            .unwrap(),
        )
        .unwrap();
    proj_reg
        .register(
            &Projection::from_parts(
                "_review_targets",
                "outline_review_targets",
                "Review targets ({{count}}): {{names}}",
                TargetForm::Markdown,
                PluginDispatch::Default,
            )
            .unwrap(),
        )
        .unwrap();

    // wire_init renders both projections.
    let init_out = wire_init(
        WireInitInput {
            persona_id: "alpha".into(),
        },
        &storage,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .expect("wire_init");
    assert_eq!(init_out.projections.len(), 2);
    assert!(init_out.warnings.is_empty(), "no dangling spec_refs");

    // Projections come back in ProjectionRegistry::list() order (alphabetical).
    let by_name: std::collections::HashMap<_, _> = init_out
        .projections
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let toc = by_name["_persona_toc"];
    assert_eq!(toc.target_form, TargetForm::Prompt);
    assert!(toc.rendered.starts_with("Personas (3):"));
    for id in ["alpha", "beta", "gamma"] {
        assert!(
            toc.rendered.contains(id),
            "toc missing {id}: {}",
            toc.rendered
        );
    }

    let review = by_name["_review_targets"];
    assert_eq!(review.target_form, TargetForm::Markdown);
    assert_eq!(review.rendered, "Review targets (1): note1");

    // wire_close reports correct totals.
    let close_out = wire_close(
        WireCloseInput {
            persona_id: "alpha".into(),
        },
        &storage,
    )
    .expect("wire_close");
    let close_summary = graph_scan_summary(&storage).unwrap();
    assert_eq!(close_summary.total_node_count, 4);
    assert_eq!(close_summary.total_edge_count, 3);
    assert_eq!(
        close_summary.orphan_node_count, 0,
        "every node is touched by at least one edge"
    );
    assert!(close_out.report_markdown.contains("total nodes: 4"));
    assert!(close_out.report_markdown.contains("total edges: 3"));
}

#[test]
fn wire_init_warns_on_dangling_spec_ref() {
    let storage = SqliteStorage::open_in_memory().unwrap();
    storage.migrate().unwrap();
    storage.seed_default_types().unwrap();

    // Register a projection whose spec_ref doesn't exist.
    ProjectionRegistry::new(&storage)
        .register(
            &Projection::from_parts(
                "broken",
                "missing_spec",
                "shouldn't render",
                TargetForm::Prompt,
                PluginDispatch::Default,
            )
            .unwrap(),
        )
        .unwrap();

    let out = wire_init(
        WireInitInput {
            persona_id: "alpha".into(),
        },
        &storage,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .unwrap();
    assert!(out.projections.is_empty());
    assert_eq!(out.warnings.len(), 1);
    assert!(out.warnings[0].contains("missing_spec"));
}

#[test]
fn composed_specification_roundtrips_through_storage_and_evaluates() {
    let storage = SqliteStorage::open_in_memory().unwrap();
    storage.migrate().unwrap();
    storage.seed_default_types().unwrap();

    // Persona with `owner=alpha` metadata, persona without.
    storage
        .insert_node(&bare_node(
            "p1",
            "persona",
            json!({"owner": {"name": "alpha"}}),
        ))
        .unwrap();
    storage
        .insert_node(&bare_node(
            "p2",
            "persona",
            json!({"owner": {"name": "beta"}}),
        ))
        .unwrap();

    // Compose: TypeIs("persona") AND MetadataEq("owner.name", "alpha")
    let spec = Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
        path: "owner.name".into(),
        value: json!("alpha"),
    });
    SpecRegistry::new(&storage)
        .register("personas_owned_by_alpha", &spec)
        .unwrap();
    ProjectionRegistry::new(&storage)
        .register(
            &Projection::from_parts(
                "_owned",
                "personas_owned_by_alpha",
                "{{count}} matched: {{names}}",
                TargetForm::Prompt,
                PluginDispatch::Default,
            )
            .unwrap(),
        )
        .unwrap();

    let out = wire_init(
        WireInitInput {
            persona_id: "alpha".into(),
        },
        &storage,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .unwrap();
    assert_eq!(out.projections.len(), 1);
    // Only p1 matches.
    assert_eq!(out.projections[0].rendered, "1 matched: p1");
}
