//! P2b E2E integration test.
//!
//! Walks `wire_query` (ad-hoc Specification → slim node list) and verifies:
//! - inline `spec` returns matched nodes in slim form (id + type + metadata)
//! - `spec_ref` resolves a registered Specification by name
//! - `limit` / `offset` paginate the result correctly
//! - validation: spec + spec_ref are mutually exclusive; one is required

use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::application::projection_registry::{
    NamedProjection, ProjectionRegistry, TargetForm,
};
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    wire_query, wire_render, WireQueryInput, WireRenderInput,
};
use persona_wire_core::domain::graph::Node;
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

fn setup() -> SqliteStorage {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();
    // 4 personas + 1 outline_node, with `status` metadata for filtering.
    s.insert_node(&bare_node("p1", "persona", json!({"status": "active"})))
        .unwrap();
    s.insert_node(&bare_node("p2", "persona", json!({"status": "active"})))
        .unwrap();
    s.insert_node(&bare_node("p3", "persona", json!({"status": "active"})))
        .unwrap();
    s.insert_node(&bare_node("p4", "persona", json!({"status": "retired"})))
        .unwrap();
    s.insert_node(&bare_node(
        "note1",
        "outline_node",
        json!({"title": "review me"}),
    ))
    .unwrap();
    s
}

#[test]
fn inline_spec_returns_matched_nodes_in_slim_form() {
    let s = setup();
    let out = wire_query(
        WireQueryInput {
            spec: Some(Specification::TypeIs("persona".into())),
            spec_ref: None,
            limit: None,
            offset: None,
        },
        &s,
    )
    .unwrap();

    assert_eq!(out.total_count, 4);
    assert_eq!(out.returned_count, 4);
    let ids: Vec<&str> = out.matched.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"p1"));
    assert!(ids.contains(&"p2"));
    assert!(ids.contains(&"p3"));
    assert!(ids.contains(&"p4"));
    assert!(!ids.contains(&"note1"), "outline_node must be excluded");

    // Slim form: every matched node carries id + type + metadata (no sot_ref / version).
    for n in &out.matched {
        assert_eq!(n.r#type, "persona");
        assert!(n.metadata.is_object());
    }
}

#[test]
fn spec_ref_resolves_registered_specification() {
    let s = setup();
    SpecRegistry::new(&s)
        .register(
            "active_personas",
            &Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
                path: "status".into(),
                value: json!("active"),
            }),
        )
        .unwrap();

    let out = wire_query(
        WireQueryInput {
            spec: None,
            spec_ref: Some("active_personas".into()),
            limit: None,
            offset: None,
        },
        &s,
    )
    .unwrap();

    assert_eq!(out.total_count, 3);
    assert_eq!(out.returned_count, 3);
    let ids: Vec<&str> = out.matched.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"p1"));
    assert!(ids.contains(&"p2"));
    assert!(ids.contains(&"p3"));
    assert!(
        !ids.contains(&"p4"),
        "p4 is retired and must be filtered out"
    );
}

#[test]
fn limit_and_offset_paginate_results() {
    let s = setup();
    let spec = Specification::TypeIs("persona".into());

    // First page: 2 items, no offset.
    let page1 = wire_query(
        WireQueryInput {
            spec: Some(spec.clone()),
            spec_ref: None,
            limit: Some(2),
            offset: None,
        },
        &s,
    )
    .unwrap();
    assert_eq!(page1.total_count, 4);
    assert_eq!(page1.returned_count, 2);

    // Second page: 2 items, offset 2.
    let page2 = wire_query(
        WireQueryInput {
            spec: Some(spec.clone()),
            spec_ref: None,
            limit: Some(2),
            offset: Some(2),
        },
        &s,
    )
    .unwrap();
    assert_eq!(page2.total_count, 4);
    assert_eq!(page2.returned_count, 2);

    // Pages must be disjoint.
    let ids1: std::collections::HashSet<_> = page1.matched.iter().map(|n| n.id.clone()).collect();
    let ids2: std::collections::HashSet<_> = page2.matched.iter().map(|n| n.id.clone()).collect();
    assert!(ids1.is_disjoint(&ids2));

    // Offset past the end returns empty.
    let beyond = wire_query(
        WireQueryInput {
            spec: Some(spec),
            spec_ref: None,
            limit: Some(10),
            offset: Some(100),
        },
        &s,
    )
    .unwrap();
    assert_eq!(beyond.total_count, 4);
    assert_eq!(beyond.returned_count, 0);
}

#[test]
fn validation_errors_when_spec_and_spec_ref_both_or_neither() {
    let s = setup();

    // both set
    let err = wire_query(
        WireQueryInput {
            spec: Some(Specification::TypeIs("persona".into())),
            spec_ref: Some("foo".into()),
            limit: None,
            offset: None,
        },
        &s,
    )
    .expect_err("expected validation error");
    assert!(err
        .to_string()
        .to_lowercase()
        .contains("mutually exclusive"));

    // neither set
    let err = wire_query(
        WireQueryInput {
            spec: None,
            spec_ref: None,
            limit: None,
            offset: None,
        },
        &s,
    )
    .expect_err("expected validation error");
    assert!(err.to_string().to_lowercase().contains("required"));

    // spec_ref to a non-existent name
    let err = wire_query(
        WireQueryInput {
            spec: None,
            spec_ref: Some("does_not_exist".into()),
            limit: None,
            offset: None,
        },
        &s,
    )
    .expect_err("expected not-found error");
    assert!(err.to_string().to_lowercase().contains("not found"));
}

#[test]
fn wire_render_evaluates_registered_projection_by_name() {
    let s = setup();
    SpecRegistry::new(&s)
        .register(
            "active_personas",
            &Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
                path: "status".into(),
                value: json!("active"),
            }),
        )
        .unwrap();
    ProjectionRegistry::new(&s)
        .register(&NamedProjection {
            name: "_active".into(),
            spec_ref: "active_personas".into(),
            template: "Active personas ({{count}}): {{names}}".into(),
            target_form: TargetForm::Prompt,
            template_engine: None,
            projection_kind: None,
            projection_config: None,
        })
        .unwrap();

    let out = wire_render(
        WireRenderInput {
            projection_ref: "_active".into(),
        },
        &s,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .unwrap();

    assert_eq!(out.name, "_active");
    assert_eq!(out.target_form, TargetForm::Prompt);
    assert!(out.rendered.starts_with("Active personas (3):"));
    for id in ["p1", "p2", "p3"] {
        assert!(out.rendered.contains(id), "missing {id}: {}", out.rendered);
    }
    assert!(
        !out.rendered.contains("p4"),
        "p4 is retired and must be filtered out"
    );
}

#[test]
fn wire_render_errors_on_unknown_projection_and_dangling_spec() {
    let s = setup();

    // (a) Unknown projection name.
    let err = wire_render(
        WireRenderInput {
            projection_ref: "does_not_exist".into(),
        },
        &s,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .expect_err("expected not-found");
    assert!(err.to_string().to_lowercase().contains("projection"));

    // (b) Projection whose spec_ref dangles.
    ProjectionRegistry::new(&s)
        .register(&NamedProjection {
            name: "broken".into(),
            spec_ref: "missing_spec".into(),
            template: "x".into(),
            target_form: TargetForm::Prompt,
            template_engine: None,
            projection_kind: None,
            projection_config: None,
        })
        .unwrap();
    let err = wire_render(
        WireRenderInput {
            projection_ref: "broken".into(),
        },
        &s,
        &PluginRegistry::default_for_wire().unwrap(),
    )
    .expect_err("expected dangling spec_ref error");
    assert!(err
        .to_string()
        .to_lowercase()
        .contains("spec_ref (dangling)"));
}
