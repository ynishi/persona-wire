//! E2E test for the GlobalAliasStorage `?scope=` path (issue 8904d808 fix).
//!
//! Covers the three resolve axes added in `MiniAppAdapter::fetch_via_alias`:
//!
//! - `?scope=user`           → User-scope `_global.db` hard target (no fallback)
//! - `?scope=<project-name>` → Project-scope `_global.db` hard target (no fallback)
//! - `?scope=` 不在 (legacy) → User-scope `_global.db` → per-table `_aliases` fallback
//!
//! Per-table `_aliases` legacy-only behaviour is exercised by the
//! sibling `e2e_alias_mcp.rs`. This file focuses on the resolve-path matrix
//! (hard target hit / hard target miss / project hard target / project
//! root 省略 / legacy global hit / legacy per-table fallback hit / legacy
//! both miss).

mod common;

use std::path::Path;

use common::{
    bootstrap_mini_app_table, bootstrap_mini_app_table_no_alias, each_title_template, make_layout,
    make_layout_with_project, schema_for, wire_one_slot, McpClient, STATUS_TITLE_SCHEMA,
};
use mini_app_core::aggregator::SourceSpec;
use mini_app_core::alias_storage::{AliasRecord, AliasScope, GlobalAliasStorage};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// GlobalAliasStorage seed helpers (Single-source, non-aggregator records).
// ---------------------------------------------------------------------------

/// Register a Single-source plain `Rows` alias in the **User scope**
/// `_global.db` under `<user_dir>/_global.db`.
async fn seed_global_alias_user(user_dir: &Path, name: &str, table: &str, filter_json: &str) {
    let storage = GlobalAliasStorage::open(None, Some(user_dir)).expect("open global user");
    let rec = AliasRecord::new(
        name,
        SourceSpec::Single(table.to_string()),
        None,
        filter_json,
        None,
        None,
        None,
    );
    storage
        .alias_create(AliasScope::User, rec)
        .await
        .expect("alias_create User scope");
}

/// Register a Single-source plain `Rows` alias in the **Project scope**
/// `_global.db` under `<project_dir>/_global.db`.
async fn seed_global_alias_project(project_dir: &Path, name: &str, table: &str, filter_json: &str) {
    let storage = GlobalAliasStorage::open(Some(project_dir), None).expect("open global project");
    let rec = AliasRecord::new(
        name,
        SourceSpec::Single(table.to_string()),
        None,
        filter_json,
        None,
        None,
        None,
    );
    storage
        .alias_create(AliasScope::Project, rec)
        .await
        .expect("alias_create Project scope");
}

// Pull warnings off the wire_prompt_context result body.
fn warnings_of(pc: &Value) -> Vec<String> {
    pc.get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn rendered_of(pc: &Value) -> String {
    pc.get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// E2E 1 — `?scope=user&alias=N` hits the User-scope `_global.db` hard target.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_scope_user_hits_global_user_storage() {
    let layout = make_layout();
    let table = "wire_e2e_scope_user_hit";

    bootstrap_mini_app_table_no_alias(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "SU1"}),
            json!({"status": "closed", "title": "SUC1"}),
            json!({"status": "active", "title": "SU2"}),
        ],
    )
    .await;

    seed_global_alias_user(
        &layout.mini_app_user_dir,
        "active_user",
        table,
        r#"{"type":"eq","field":"status","value":"active"}"#,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "scope_user_hit";
    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table}?scope=user&alias=active_user"),
        &each_title_template("ActiveUser"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        rendered.contains("SU1") && rendered.contains("SU2"),
        "scope=user did not render active rows: {rendered:?}\nFULL: {pc}"
    );
    assert!(
        !rendered.contains("SUC1"),
        "scope=user leaked closed row (filter not applied): {rendered:?}"
    );
    assert!(
        warnings.is_empty(),
        "warnings emitted under scope=user happy path: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E 2 — `?scope=user&alias=N` hard-fails when alias is absent from User
// `_global.db`, even if a same-name alias exists in per-table `_aliases`.
// The User-scope path does NOT fall back to per-table.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_scope_user_miss_does_not_fallback_to_per_table() {
    let layout = make_layout();
    let table = "wire_e2e_scope_user_miss";

    // Per-table alias under the same name exists, but `?scope=user` must NOT
    // fall back to it. Seeded via the legacy per-table bootstrap helper.
    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![json!({"status": "active", "title": "MISS1"})],
        "shadow_alias",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;

    // _global.db (User scope) is intentionally NOT seeded with `shadow_alias`.
    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "scope_user_miss";
    wire_one_slot(
        &mut client,
        persona_id,
        "ghost",
        &format!("mini-app://{table}?scope=user&alias=shadow_alias"),
        &each_title_template("UserMiss"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        !rendered.contains("MISS1"),
        "scope=user fell back to per-table _aliases (regression): {rendered:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("User scope") || w.contains("shadow_alias")),
        "expected a User-scope-miss warning, got: {warnings:?}\nFULL: {pc}"
    );
}

// ---------------------------------------------------------------------------
// E2E 3 — `?scope=<project>&root=<dir>&alias=N` hits the Project-scope
// `_global.db` hard target. The mini-app tables are bootstrapped under the
// project dir; the wire server's MINI_APP_PROJECT_DIR env is set so that
// the Store::open path (Step 3 of fetch_via_alias) can find the schema.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_scope_project_hits_global_project_storage() {
    let layout = make_layout_with_project();
    let table = "wire_e2e_scope_proj_hit";

    // Project-scope mini-app table layout: tables live under project_dir.
    bootstrap_mini_app_table_no_alias(
        &layout.mini_app_project_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "SP1"}),
            json!({"status": "closed", "title": "SPC1"}),
            json!({"status": "active", "title": "SP2"}),
        ],
    )
    .await;

    seed_global_alias_project(
        &layout.mini_app_project_dir,
        "active_proj",
        table,
        r#"{"type":"eq","field":"status","value":"active"}"#,
    )
    .await;

    let mut client = McpClient::spawn_with_project(
        &layout.wire_db,
        &layout.mini_app_user_dir,
        &layout.mini_app_project_dir,
    );
    let persona_id = "scope_proj_hit";
    let project_root_str = layout.mini_app_project_dir.to_string_lossy();
    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table}?scope=algocline&root={project_root_str}&alias=active_proj"),
        &each_title_template("ActiveProj"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        rendered.contains("SP1") && rendered.contains("SP2"),
        "scope=<project> did not render active rows: {rendered:?}\nFULL: {pc}"
    );
    assert!(
        !rendered.contains("SPC1"),
        "scope=<project> leaked closed row (filter not applied): {rendered:?}"
    );
    assert!(
        warnings.is_empty(),
        "warnings emitted under scope=<project> happy path: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E 4 — `?scope=<project>&alias=N` 省略 root fails at URI-parse time with a
// `requires ?root=<dir>` error, which surfaces as a wire warning. No rows
// are rendered.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_scope_project_without_root_parse_errors() {
    let layout = make_layout();
    let table = "wire_e2e_scope_proj_no_root";

    // Need a real per-table schema so the parse-stage failure is the ONLY
    // reason rows do not render (no schema-load noise).
    bootstrap_mini_app_table_no_alias(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![json!({"status": "active", "title": "NR1"})],
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "scope_proj_no_root";
    wire_one_slot(
        &mut client,
        persona_id,
        "ghost",
        &format!("mini-app://{table}?scope=algocline&alias=anything"),
        &each_title_template("NoRoot"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        !rendered.contains("NR1"),
        "rendered rows despite parse-stage error: {rendered:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("requires ?root")),
        "expected a 'requires ?root=' warning, got: {warnings:?}\nFULL: {pc}"
    );
}

// ---------------------------------------------------------------------------
// E2E 5 — legacy URI (no `?scope=`) hits the User-scope `_global.db` first.
// This is the new mini-app v0.12.1+ default path: aliases created via the
// global storage are picked up without any URI changes.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_legacy_uri_resolves_via_global_user_first() {
    let layout = make_layout();
    let table = "wire_e2e_legacy_global";

    bootstrap_mini_app_table_no_alias(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "LG1"}),
            json!({"status": "closed", "title": "LGC1"}),
        ],
    )
    .await;

    seed_global_alias_user(
        &layout.mini_app_user_dir,
        "active_legacy",
        table,
        r#"{"type":"eq","field":"status","value":"active"}"#,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "legacy_global";
    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        // No ?scope= — exercise the legacy URI form against global storage.
        &format!("mini-app://{table}?alias=active_legacy"),
        &each_title_template("LegacyGlobal"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        rendered.contains("LG1") && !rendered.contains("LGC1"),
        "legacy URI did not resolve via global User storage: {rendered:?}\nFULL: {pc}"
    );
    assert!(
        warnings.is_empty(),
        "warnings emitted under legacy URI + global hit: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E 6 — legacy URI (no `?scope=`) falls back to per-table `_aliases` when
// the User-scope `_global.db` does not have the alias. This is the backward
// compatibility path that keeps the existing per-table alias users working
// without migration.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_legacy_uri_falls_back_to_per_table_aliases() {
    let layout = make_layout();
    let table = "wire_e2e_legacy_per_table";

    // Per-table _aliases is the ONLY storage holding `legacy_active`.
    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "PT1"}),
            json!({"status": "closed", "title": "PTC1"}),
            json!({"status": "active", "title": "PT2"}),
        ],
        "legacy_active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;

    // _global.db (User scope) is intentionally empty.
    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "legacy_per_table";
    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table}?alias=legacy_active"),
        &each_title_template("LegacyPerTable"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        rendered.contains("PT1") && rendered.contains("PT2"),
        "per-table fallback did not render rows: {rendered:?}\nFULL: {pc}"
    );
    assert!(
        !rendered.contains("PTC1"),
        "per-table fallback leaked closed row: {rendered:?}"
    );
    assert!(
        warnings.is_empty(),
        "warnings emitted under per-table fallback: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E 7 — legacy URI hard-fails when neither User-scope `_global.db` nor
// per-table `_aliases` has the alias. The error message names both storages
// (canary against silent fallthrough).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_legacy_uri_double_miss_emits_combined_error() {
    let layout = make_layout();
    let table = "wire_e2e_legacy_double_miss";

    // Per-table store + schema exist (so the failure is alias-not-found,
    // not table-not-found), but neither _aliases nor _global.db carry the
    // alias name we ask for.
    bootstrap_mini_app_table_no_alias(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![json!({"status": "active", "title": "DM1"})],
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "legacy_double_miss";
    wire_one_slot(
        &mut client,
        persona_id,
        "ghost",
        &format!("mini-app://{table}?alias=nowhere_alias"),
        &each_title_template("DoubleMiss"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    assert!(
        !rendered.contains("DM1"),
        "rendered rows despite alias being unresolvable: {rendered:?}"
    );
    // The literal in adapter.rs is:
    //   "alias 'X' not found in _global.db (User scope) nor per-table T._aliases fallback: ..."
    // We assert on the substring that uniquely identifies the combined path.
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("_global.db") && w.contains("per-table")),
        "expected combined-storage miss warning, got: {warnings:?}\nFULL: {pc}"
    );
}
