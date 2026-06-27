//! Bundle E2E integration test.
//!
//! Round-trips the quickstart bundle through the public registry surface:
//! register → install → verify-via-registries → re-install (increment) →
//! verify auto-suffix did not duplicate the original rows.
//!
//! Mirrors the workflow a CLI / MCP caller follows. The in-process test
//! uses `SqliteStorage::open_in_memory()` so no temp files are needed.

use persona_wire_core::application::bundle_install::install_bundle;
use persona_wire_core::application::bundle_registry::BundleRegistry;
use persona_wire_core::application::projection_registry::ProjectionRegistry;
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::domain::entity::bundle::{
    BundleName, BundleRef, BundleVersion, ConflictMode,
};
use persona_wire_core::infrastructure::storage::SqliteStorage;

const QUICKSTART_BODY: &str = r#"
[bundle]
name = "quickstart"
version = "0.1.0"
description = "E2E sample"

[[nodes]]
name = "shi"
node_type = "persona"
metadata = { owner = "ytk" }

[[specs]]
name = "active_personas"
spec = { TypeIs = "persona" }

[[projections]]
name = "personas_overview"
spec_ref = "active_personas"
template = "Personas: {{count}}"
target_form = "prompt"
"#;

fn setup() -> SqliteStorage {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();
    s
}

#[test]
fn bundle_register_install_roundtrip() {
    let s = setup();

    // ---- register ----
    let reg = BundleRegistry::new(&s);
    let bundle_id = reg
        .register(
            &BundleName::new("quickstart").unwrap(),
            &BundleVersion::new("0.1.0").unwrap(),
            Some("E2E sample"),
            QUICKSTART_BODY,
        )
        .expect("register");

    // BundleRef::parse should round-trip through either form.
    let by_id = reg
        .resolve(&BundleRef::parse(&bundle_id.to_string()).unwrap())
        .unwrap()
        .expect("by id");
    let by_name = reg
        .resolve(&BundleRef::parse("quickstart").unwrap())
        .unwrap()
        .expect("by name");
    assert_eq!(by_id, by_name);
    assert_eq!(by_id.description.as_deref(), Some("E2E sample"));
    assert!(by_id.body.contains("[[specs]]"));

    // ---- install (increment, default) ----
    let report = install_bundle(&by_id, ConflictMode::Increment, &s).expect("install");
    assert_eq!(report.installed.len(), 3, "report: {:?}", report);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert!(report.skipped.is_empty());

    // ---- verify via the underlying registries ----
    let spec_names = SpecRegistry::new(&s).list().unwrap();
    assert!(
        spec_names.iter().any(|n| n == "active_personas"),
        "spec list: {:?}",
        spec_names
    );

    let proj_names = ProjectionRegistry::new(&s).list().unwrap();
    assert!(
        proj_names.iter().any(|n| n == "personas_overview"),
        "projection list: {:?}",
        proj_names
    );

    // Projection.spec_ref points at the registered spec by name.
    let proj = ProjectionRegistry::new(&s)
        .get("personas_overview")
        .unwrap()
        .expect("projection row");
    assert_eq!(proj.spec_ref().as_str(), "active_personas");

    // Node persists with the declared name + type.
    let node_id = s.lookup_node_id_by_name("shi").unwrap().expect("node row");
    let node = s.get_node(&node_id).unwrap().expect("get_node");
    assert_eq!(node.r#type, "persona");
    assert_eq!(
        node.metadata.get("owner").and_then(|v| v.as_str()),
        Some("ytk")
    );
}

#[test]
fn bundle_reinstall_increments_names_without_duplicating_originals() {
    let s = setup();
    let reg = BundleRegistry::new(&s);
    let _ = reg
        .register(
            &BundleName::new("quickstart").unwrap(),
            &BundleVersion::new("0.1.0").unwrap(),
            None,
            QUICKSTART_BODY,
        )
        .unwrap();
    let bundle = reg
        .resolve(&BundleRef::parse("quickstart").unwrap())
        .unwrap()
        .unwrap();

    // First install: originals.
    let r1 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
    assert_eq!(r1.installed.len(), 3);

    // Second install: every name auto-suffixes to `-1`.
    let r2 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
    let final_names: Vec<_> = r2.installed.iter().map(|i| i.final_name.clone()).collect();
    assert!(final_names.contains(&"active_personas-1".to_string()));
    assert!(final_names.contains(&"personas_overview-1".to_string()));
    assert!(final_names.contains(&"shi-1".to_string()));

    // The original rows are still intact — increment did not overwrite.
    assert!(SpecRegistry::new(&s)
        .get("active_personas")
        .unwrap()
        .is_some());
    assert!(ProjectionRegistry::new(&s)
        .get("personas_overview")
        .unwrap()
        .is_some());
    assert!(s.lookup_node_id_by_name("shi").unwrap().is_some());

    // The internal spec_ref of the suffixed projection points at the
    // suffixed spec, not the original — internal references are rewritten
    // through the rename map.
    let p_suffix = ProjectionRegistry::new(&s)
        .get("personas_overview-1")
        .unwrap()
        .expect("suffixed projection");
    assert_eq!(p_suffix.spec_ref().as_str(), "active_personas-1");
}

#[test]
fn bundle_delete_after_install_succeeds_and_preserves_install_log() {
    // Regression test for /jikki Phase 2 smoke finding: pre-fix
    // `bundle_installs.bundle_id` was `NOT NULL REFERENCES bundles(id)`
    // (default RESTRICT), so deleting a bundle whose install log had
    // any rows failed with FOREIGN KEY constraint failed — even though
    // the registry and onboarding doc claim install history is
    // preserved across deletion.
    //
    // After the storage SCHEMA fix (bundle_id nullable + ON DELETE SET
    // NULL), `wire_bundle_delete` must succeed AND the historical
    // install_id rows must survive.
    let s = setup();
    let reg = BundleRegistry::new(&s);
    reg.register(
        &BundleName::new("delete-me").unwrap(),
        &BundleVersion::new("0.1.0").unwrap(),
        None,
        QUICKSTART_BODY,
    )
    .unwrap();
    let bundle = reg
        .resolve(&BundleRef::parse("delete-me").unwrap())
        .unwrap()
        .unwrap();
    let bundle_id = bundle.id;
    // Install once so bundle_installs has a row tied to this bundle.
    let report = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
    assert!(report.errors.is_empty());

    // Delete the parent row — pre-fix this returned FOREIGN KEY
    // constraint failed.
    let deleted = reg
        .delete(&BundleName::new("delete-me").unwrap())
        .expect("delete after install");
    assert!(deleted);
    assert!(reg
        .get(&BundleName::new("delete-me").unwrap())
        .unwrap()
        .is_none());

    // The install log row survived; bundle_id is now NULL because of
    // ON DELETE SET NULL.
    let (count, bundle_id_after): (i64, Option<String>) = s
        .conn_for_test()
        .query_row(
            "SELECT COUNT(*), MAX(bundle_id) FROM bundle_installs WHERE install_id = ?1",
            rusqlite::params![report.install_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "install log row should survive bundle delete");
    assert!(
        bundle_id_after.is_none(),
        "bundle_id should be SET NULL after parent delete, got {:?} (was bundle_id={:?})",
        bundle_id_after,
        bundle_id
    );
}

#[test]
fn bundle_skip_mode_is_idempotent_for_fixed_names() {
    let s = setup();
    let reg = BundleRegistry::new(&s);
    reg.register(
        &BundleName::new("quickstart").unwrap(),
        &BundleVersion::new("0.1.0").unwrap(),
        None,
        QUICKSTART_BODY,
    )
    .unwrap();
    let bundle = reg
        .resolve(&BundleRef::parse("quickstart").unwrap())
        .unwrap()
        .unwrap();

    // Seed.
    install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();

    // Re-install under skip — everything already exists, nothing installed,
    // every section row goes to `skipped[]`.
    let r = install_bundle(&bundle, ConflictMode::Skip, &s).unwrap();
    assert!(r.installed.is_empty(), "installed: {:?}", r.installed);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    assert_eq!(r.skipped.len(), 3);
}
