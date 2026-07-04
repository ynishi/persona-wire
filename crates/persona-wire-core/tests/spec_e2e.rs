//! Specification read-tool E2E integration test.
//!
//! Round-trips a Specification through the public registry surface:
//! register → get by name → get by ULID id → list (contains) → delete →
//! get returns NotFound-equivalent `None`. Mirrors `bundle_e2e.rs`.

use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;

fn setup() -> SqliteStorage {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();
    s
}

#[test]
fn spec_register_get_list_delete_roundtrip() {
    let s = setup();
    let reg = SpecRegistry::new(&s);

    // ---- register ----
    let spec = Specification::TypeIs("persona".into());
    let id = reg.register("active_personas", &spec).unwrap();

    // ---- get by name ----
    let by_name = reg
        .get_full_by_name("active_personas")
        .unwrap()
        .expect("row by name");
    assert_eq!(by_name.id, id);
    assert_eq!(by_name.name, "active_personas");
    assert_eq!(by_name.json, r#"{"TypeIs":"persona"}"#);
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
    assert!(rows.iter().any(|r| r.name == "active_personas"));

    // ---- delete ----
    assert!(s.delete_specification(&id).unwrap());

    // ---- get after delete returns None (NotFound at the MCP boundary) ----
    assert!(reg.get_full_by_name("active_personas").unwrap().is_none());
    assert!(reg.get_full_by_ref(&id.to_string()).unwrap().is_none());
}

#[test]
fn spec_list_paginates_and_orders_by_created_at_desc() {
    let s = setup();
    let reg = SpecRegistry::new(&s);
    for (i, name) in ["s1", "s2", "s3"].into_iter().enumerate() {
        reg.register(name, &Specification::TypeIs(format!("t{i}")))
            .unwrap();
        // Force distinct created_at seconds so DESC ordering is observable.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    let all = reg.list_full(100, 0).unwrap();
    let names: Vec<_> = all.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, vec!["s3", "s2", "s1"]);

    let page = reg.list_full(1, 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].name, "s2");
}
