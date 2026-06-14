//! P1 E2E integration test.
//!
//! Walks the full pipeline through the public API of `persona-wire-core`:
//! migrate → seed → insert nodes/edges → register Specification → register
//! NamedProjection → pnet_init renders → pnet_close reports.

use persona_wire_core::application::projection_registry::{
    NamedProjection, ProjectionRegistry, TargetForm,
};
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    pnet_close, pnet_init, PnetCloseInput, PnetInitInput,
};
use persona_wire_core::domain::graph::{Edge, Node, Severity};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;
use serde_json::json;

fn bare_node(id: &str, type_: &str, metadata: serde_json::Value) -> Node {
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
    assert_eq!(nodes.len(), 9, "expected 9 seeded node types");

    // Insert a small persona-routing graph.
    // shi -[routes_to]-> mia
    // shi -[routes_to]-> misaki
    // mia -[triggers_review_of severity=hard]-> note1 (outline_node)
    for id in ["shi", "mia", "misaki"] {
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
            id: "e_shi_mia".into(),
            src_node: "shi".into(),
            tgt_node: "mia".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
    storage
        .insert_edge(&Edge {
            id: "e_shi_misaki".into(),
            src_node: "shi".into(),
            tgt_node: "misaki".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
    storage
        .insert_edge(&Edge {
            id: "e_mia_review".into(),
            src_node: "mia".into(),
            tgt_node: "note1".into(),
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
        .register(&NamedProjection {
            name: "_persona_toc".into(),
            spec_ref: "active_personas".into(),
            template: "Personas ({{count}}): {{names}}".into(),
            target_form: TargetForm::Prompt,
        })
        .unwrap();
    proj_reg
        .register(&NamedProjection {
            name: "_review_targets".into(),
            spec_ref: "outline_review_targets".into(),
            template: "Review targets ({{count}}): {{names}}".into(),
            target_form: TargetForm::Markdown,
        })
        .unwrap();

    // pnet_init renders both projections.
    let init_out = pnet_init(
        PnetInitInput {
            persona_id: "shi".into(),
        },
        &storage,
    )
    .expect("pnet_init");
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
    for id in ["shi", "mia", "misaki"] {
        assert!(
            toc.rendered.contains(id),
            "toc missing {id}: {}",
            toc.rendered
        );
    }

    let review = by_name["_review_targets"];
    assert_eq!(review.target_form, TargetForm::Markdown);
    assert_eq!(review.rendered, "Review targets (1): note1");

    // pnet_close reports correct totals.
    let close_out = pnet_close(
        PnetCloseInput {
            persona_id: "shi".into(),
        },
        &storage,
    )
    .expect("pnet_close");
    assert_eq!(close_out.total_node_count, 4);
    assert_eq!(close_out.total_edge_count, 3);
    assert_eq!(
        close_out.orphan_node_count, 0,
        "every node is touched by at least one edge"
    );
    assert!(close_out.report_markdown.contains("total nodes: 4"));
    assert!(close_out.report_markdown.contains("total edges: 3"));
}

#[test]
fn pnet_init_warns_on_dangling_spec_ref() {
    let storage = SqliteStorage::open_in_memory().unwrap();
    storage.migrate().unwrap();
    storage.seed_default_types().unwrap();

    // Register a projection whose spec_ref doesn't exist.
    ProjectionRegistry::new(&storage)
        .register(&NamedProjection {
            name: "broken".into(),
            spec_ref: "missing_spec".into(),
            template: "shouldn't render".into(),
            target_form: TargetForm::Prompt,
        })
        .unwrap();

    let out = pnet_init(
        PnetInitInput {
            persona_id: "shi".into(),
        },
        &storage,
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

    // Persona with `owner=shi` metadata, persona without.
    storage
        .insert_node(&bare_node(
            "p1",
            "persona",
            json!({"owner": {"name": "shi"}}),
        ))
        .unwrap();
    storage
        .insert_node(&bare_node(
            "p2",
            "persona",
            json!({"owner": {"name": "mia"}}),
        ))
        .unwrap();

    // Compose: TypeIs("persona") AND MetadataEq("owner.name", "shi")
    let spec = Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
        path: "owner.name".into(),
        value: json!("shi"),
    });
    SpecRegistry::new(&storage)
        .register("personas_owned_by_shi", &spec)
        .unwrap();
    ProjectionRegistry::new(&storage)
        .register(&NamedProjection {
            name: "_owned".into(),
            spec_ref: "personas_owned_by_shi".into(),
            template: "{{count}} matched: {{names}}".into(),
            target_form: TargetForm::Prompt,
        })
        .unwrap();

    let out = pnet_init(
        PnetInitInput {
            persona_id: "shi".into(),
        },
        &storage,
    )
    .unwrap();
    assert_eq!(out.projections.len(), 1);
    // Only p1 matches.
    assert_eq!(out.projections[0].rendered, "1 matched: p1");
}
