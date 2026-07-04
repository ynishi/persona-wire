//! NamedProjection read-tool E2E integration test.
//!
//! Round-trips a NamedProjection through the public registry surface:
//! register → get by name → get by ULID id → list (contains) → delete →
//! get returns NotFound-equivalent `None`. Mirrors `bundle_e2e.rs`.

use persona_wire_core::application::projection_registry::ProjectionRegistry;
use persona_wire_core::domain::entity::projection::{PluginDispatch, Projection};
use persona_wire_core::domain::entity::TargetForm;
use persona_wire_core::infrastructure::storage::SqliteStorage;

fn setup() -> SqliteStorage {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();
    s
}

#[test]
fn projection_register_get_list_delete_roundtrip() {
    let s = setup();
    let reg = ProjectionRegistry::new(&s);

    // ---- register ----
    let p = Projection::from_parts(
        "personas_overview",
        "active_personas",
        "Personas: {{count}}",
        TargetForm::Prompt,
        PluginDispatch::Default,
    )
    .unwrap();
    let id = reg.register(&p).unwrap();

    // ---- get by name ----
    let by_name = reg
        .get_full_by_name("personas_overview")
        .unwrap()
        .expect("row by name");
    assert_eq!(by_name.id, id);
    assert_eq!(by_name.name, "personas_overview");
    assert_eq!(by_name.spec_ref, "active_personas");
    assert_eq!(by_name.target_form, TargetForm::Prompt);
    assert_eq!(by_name.template, "Personas: {{count}}");
    assert!(by_name.created_at > 0);
    assert_eq!(by_name.created_at, by_name.updated_at);

    // ---- get by ULID id ----
    let by_id = reg
        .get_full_by_ref(&id.to_string())
        .unwrap()
        .expect("row by id");
    assert_eq!(by_id, by_name);

    // ---- list (contains) ----
    let rows = reg.list_full(100, 0).unwrap();
    assert!(rows.iter().any(|r| r.name == "personas_overview"));

    // ---- delete ----
    assert!(s.delete_projection(&id).unwrap());

    // ---- get after delete returns None (NotFound at the MCP boundary) ----
    assert!(reg.get_full_by_name("personas_overview").unwrap().is_none());
    assert!(reg.get_full_by_ref(&id.to_string()).unwrap().is_none());
}

#[test]
fn projection_list_paginates_and_orders_by_created_at_desc() {
    let s = setup();
    let reg = ProjectionRegistry::new(&s);
    for name in ["p1", "p2", "p3"] {
        let p = Projection::from_parts(
            name,
            "s",
            "t",
            TargetForm::Markdown,
            PluginDispatch::Default,
        )
        .unwrap();
        reg.register(&p).unwrap();
        // Force distinct created_at seconds so DESC ordering is observable.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    let all = reg.list_full(100, 0).unwrap();
    let names: Vec<_> = all.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, vec!["p3", "p2", "p1"]);

    let page = reg.list_full(1, 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].name, "p2");
}
