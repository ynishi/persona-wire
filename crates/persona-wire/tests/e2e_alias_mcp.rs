//! E2E test for the `mini-app://table?alias=N` URI form through the real
//! `persona-wire mcp` stdio binary (per-table `_aliases` legacy path).
//!
//! Spawns the actual MCP server (`persona-wire mcp`) over stdio JSON-RPC
//! (same path Claude / any MCP client takes), points its DB and mini-app
//! storage at tempdirs, registers a wiring graph that points at a
//! mini-app alias URI, and asserts that `wire_prompt_context` renders
//! the alias-fetched rows.
//!
//! Pattern: written by writing one McpClient + handshake + 1 smoke
//! call, expanding step by step. Mirrors the mini-app-mcp `e2e_mcp.rs`
//! McpClient shape.
//!
//! Shared fixtures (McpClient / Layout / bootstrap / wire_one_slot) live
//! in `tests/common/mod.rs`. Scope-axis tests (`?scope=user` /
//! `?scope=<project>`) live in `tests/e2e_alias_scope.rs`.

mod common;

use common::{
    bootstrap_mini_app_table, bootstrap_mini_app_table_full, each_body_template,
    each_title_template, make_layout, schema_for, wire_one_slot, McpClient, STATUS_TITLE_SCHEMA,
    TO_BODY_SCHEMA,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Smoke 1 — tools/list advertises the wire surface we depend on.
// ---------------------------------------------------------------------------

#[test]
fn tools_list_includes_wire_surface() {
    let layout = make_layout();
    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let tools = client.list_tools();
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    for required in [
        "wire_init",
        "wire_prompt_context",
        "wire_spec_register",
        "wire_projection_register",
        "wire_node_create",
        "wire_edge_create",
        "wire_doctor",
    ] {
        assert!(
            names.contains(&required),
            "tools/list missing {required}: got {names:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// E2E 1 — plain Eq alias: wire_prompt_context renders rows fetched via
// `mini-app://<table>?alias=<name>` URI.
//
// Flow:
//   1. Bootstrap mini-app store in tempdir + register `active` alias
//      (Eq on status="active") with 3 seed rows (2 match).
//   2. Spawn `persona-wire mcp`, register spec/projection that injects
//      `count` + each `data.title`.
//   3. Insert a persona node + wiring_entry node (source_uri=
//      `mini-app://wire_e2e_eq?alias=active`) + routes_to edge.
//   4. Call wire_prompt_context and assert rendered string carries the
//      2 active titles and warnings is empty.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_plain_eq_alias_renders_active_rows() {
    let layout = make_layout();
    let table = "wire_e2e_eq";

    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "A1"}),
            json!({"status": "closed", "title": "C1"}),
            json!({"status": "active", "title": "A2"}),
        ],
        "active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_smoke";
    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table}?alias=active"),
        &each_title_template("Active"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing prompt_context: {pc:?}"));
    let warnings = pc
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert!(
        rendered.contains("A1") && rendered.contains("A2"),
        "rendered missing active titles: {rendered:?}\nFULL: {pc}",
    );
    assert!(
        !rendered.contains("C1"),
        "rendered leaked closed row (alias filter not applied): {rendered:?}"
    );
    assert!(
        warnings.is_empty(),
        "wire_prompt_context emitted warnings: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E 2 — template alias + params: `?alias=for_to&to=mia` should render
// MiniJinja-resolved `{"type":"eq","field":"to","value":"mia"}`.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_template_alias_renders_with_params() {
    let layout = make_layout();
    let table = "wire_e2e_tpl";

    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, TO_BODY_SCHEMA),
        vec![
            json!({"to": "mia", "body": "M1"}),
            json!({"to": "shi", "body": "S1"}),
            json!({"to": "mia", "body": "M2"}),
        ],
        "for_to",
        r#"{"type":"eq","field":"to","value":"{{ to }}"}"#,
        Some(r#"["to"]"#.to_string()),
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_tpl";
    wire_one_slot(
        &mut client,
        persona_id,
        "inbox",
        // `?to=mia` 以外の query は params 拡張なし。 wire の URI parse が
        // alias / limit reserved key 以外を params に集めて MiniJinja に渡す。
        &format!("mini-app://{table}?alias=for_to&to=mia"),
        &each_body_template("Inbox"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing prompt_context: {pc:?}"));
    let warnings = pc
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert!(
        rendered.contains("M1") && rendered.contains("M2"),
        "rendered missing matching rows: {rendered:?}\nFULL: {pc}"
    );
    assert!(
        !rendered.contains("S1"),
        "rendered leaked non-matching row: {rendered:?}"
    );
    assert!(warnings.is_empty(), "warnings emitted: {warnings:?}");
}

// ---------------------------------------------------------------------------
// E2E 3 — limit override: `?alias=active&limit=1` should cap rows to 1
// even though 2 rows would otherwise match.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_alias_limit_override_caps_rows() {
    let layout = make_layout();
    let table = "wire_e2e_limit";

    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "A1"}),
            json!({"status": "active", "title": "A2"}),
            json!({"status": "active", "title": "A3"}),
        ],
        "active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_limit";
    wire_one_slot(
        &mut client,
        persona_id,
        "capped",
        &format!("mini-app://{table}?alias=active&limit=1"),
        &each_title_template("Capped"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let match_count = ["A1", "A2", "A3"]
        .iter()
        .filter(|t| rendered.contains(*t))
        .count();
    assert_eq!(
        match_count, 1,
        "expected exactly 1 row under limit=1, got {match_count} in {rendered:?}"
    );
}

// ---------------------------------------------------------------------------
// E2E 4 — alias_not_found: malformed URI pointing at an unregistered alias
// surfaces a warning, slot is skipped, prompt_context is empty (no panic).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_alias_not_found_warns_and_skips_slot() {
    let layout = make_layout();
    let table = "wire_e2e_missing";

    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![json!({"status": "active", "title": "A1"})],
        "active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_missing";
    wire_one_slot(
        &mut client,
        persona_id,
        "ghost",
        &format!("mini-app://{table}?alias=does_not_exist"),
        &each_title_template("Ghost"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let warnings = pc
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        warnings
            .iter()
            .filter_map(|v| v.as_str())
            .any(|w| w.contains("does_not_exist") || w.contains("alias")),
        "expected a not-found warning, got: {warnings:?}\nFULL: {pc}"
    );
}

// ---------------------------------------------------------------------------
// E2E 5 — plain table fetch (alias absent): `mini-app://<table>` without
// `?alias=` keeps the list-all backward-compat path. Verifies that the
// alias dispatch did not break the original URI form.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_plain_table_fetch_lists_all_rows() {
    let layout = make_layout();
    let table = "wire_e2e_plain";

    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "PA1"}),
            json!({"status": "closed", "title": "PC1"}),
        ],
        // The bootstrap helper still creates one alias, but the test URI does
        // not reference it — list-all path is exercised.
        "noop",
        r#"{"type":"eq","field":"status","value":"never"}"#,
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_plain";
    wire_one_slot(
        &mut client,
        persona_id,
        "all",
        &format!("mini-app://{table}"),
        &each_title_template("All"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        rendered.contains("PA1") && rendered.contains("PC1"),
        "plain list-all path did not render all rows: {rendered:?}\nFULL: {pc}"
    );
}

// ---------------------------------------------------------------------------
// E2E 6 — multi-slot: one persona wired across two different alias URIs
// against two different mini-app tables, plus one plain (alias-free) slot.
// Verifies that `wire_prompt_context` walks the wiring graph and renders
// every slot through the Adapter dispatch.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_multi_slot_alias_and_plain_coexist() {
    let layout = make_layout();
    let table_active = "wire_e2e_multi_active";
    let table_inbox = "wire_e2e_multi_inbox";

    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table_active,
        &schema_for(table_active, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "MA1"}),
            json!({"status": "closed", "title": "MC1"}),
        ],
        "active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;
    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table_inbox,
        &schema_for(table_inbox, TO_BODY_SCHEMA),
        vec![
            json!({"to": "mia", "body": "MX1"}),
            json!({"to": "shi", "body": "SX1"}),
        ],
        "for_mia",
        r#"{"type":"eq","field":"to","value":"mia"}"#,
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_multi";

    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table_active}?alias=active"),
        &each_title_template("Active"),
    );
    wire_one_slot(
        &mut client,
        persona_id,
        "inbox",
        &format!("mini-app://{table_inbox}?alias=for_mia"),
        &each_body_template("Inbox"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let warnings = pc
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(warnings.is_empty(), "warnings emitted: {warnings:?}");
    assert!(
        rendered.contains("MA1") && !rendered.contains("MC1"),
        "active slot missing or leaked: {rendered:?}"
    );
    assert!(
        rendered.contains("MX1") && !rendered.contains("SX1"),
        "inbox slot missing or leaked: {rendered:?}"
    );
}

// ---------------------------------------------------------------------------
// Regression — Finding 1 (commit 37e7cec):
// Claude Code MCP stdio transport stringifies serde_json::Value-typed argument
// fields, so `metadata: {"persona":"X"}` arrives at the wire server as
// `Value::String("{\"persona\":\"X\"}")`. The server-side `normalize_metadata`
// helper re-parses JSON-encoded strings back into objects, so downstream
// `wire_query` / `wire_prompt_context` can still resolve `MetadataEq` paths.
//
// This regression test reproduces the stringified path explicitly: the
// `metadata` field is passed as a JSON-encoded string (not an object) and
// the test asserts that:
//   1. `wire_node_create` accepts the stringified form without erroring.
//   2. `wire_query` with `MetadataEq{path:"persona", value:"<id>"}` matches
//      the node — i.e. the metadata was re-parsed to an object before storage.
//   3. End-to-end: `wire_prompt_context` still walks the wiring graph and
//      renders rows through the Adapter when the wiring_entry's metadata
//      arrived stringified.
//
// Covers both `wire_node_create` (the original Finding 1 trigger) and
// `wire_edge_create` (sibling fix in the same commit).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn regression_stringified_metadata_recovered_by_normalize() {
    let layout = make_layout();
    let table = "wire_e2e_str_meta";
    bootstrap_mini_app_table(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![json!({"status": "active", "title": "STR1"})],
        "active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_strmeta";

    client.call_tool_text(
        "wire_spec_register",
        json!({
            "name": format!("{persona_id}.spec.active"),
            "json": "{\"TypeIs\":\"outline_node\"}",
        }),
    );
    client.call_tool_text(
        "wire_projection_register",
        json!({
            "name": format!("{persona_id}.section.active"),
            "spec_ref": format!("{persona_id}.spec.active"),
            "template": each_title_template("Active"),
            "target_form": "markdown",
        }),
    );
    client.call_tool_text(
        "wire_node_create",
        json!({
            "name": persona_id,
            "type": "persona",
            "metadata": {},
        }),
    );

    // STRINGIFIED metadata (the Finding 1 transport form). If
    // `normalize_metadata` does not run, this stores a Value::String for
    // metadata and every subsequent MetadataEq lookup misses.
    let entry_id = format!("{persona_id}.active");
    let stringified_meta = format!(
        r#"{{"persona":"{persona_id}","axis":"active","source_uri":"mini-app://{table}?alias=active"}}"#,
    );
    client.call_tool_text(
        "wire_node_create",
        json!({
            "name": entry_id,
            "type": "outline_node",
            "metadata": stringified_meta,
        }),
    );
    // Sibling: wire_edge_create also takes metadata through normalize_metadata.
    let stringified_edge_meta = r#"{"note":"regression-edge"}"#;
    client.call_tool_text(
        "wire_edge_create",
        json!({
            "name": format!("e.{persona_id}.active"),
            "src": persona_id,
            "tgt": entry_id,
            "kind": "routes_to",
            "metadata": stringified_edge_meta,
        }),
    );

    // (a) wire_query asserts that metadata was re-parsed to an object: a
    // MetadataEq path lookup only matches when storage holds the object form.
    let q = client.call_tool_json(
        "wire_query",
        json!({
            "spec": format!(
                "{{\"And\":[{{\"TypeIs\":\"outline_node\"}},{{\"MetadataEq\":{{\"path\":\"persona\",\"value\":\"{persona_id}\"}}}}]}}",
            ),
        }),
    );
    let matched = q
        .get("matched")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("wire_query returned no matched array: {q:?}"));
    assert_eq!(
        matched.len(),
        1,
        "MetadataEq lookup missed — stringified metadata not normalized: {q:?}"
    );
    let meta = &matched[0]["metadata"];
    assert!(
        meta.is_object(),
        "stored metadata is not an object — normalize_metadata did not run: {meta:?}"
    );
    assert_eq!(
        meta.get("source_uri").and_then(Value::as_str),
        Some(format!("mini-app://{table}?alias=active").as_str()),
        "source_uri missing or wrong after normalize: {meta:?}"
    );

    // (b) end-to-end: wire_prompt_context walks the graph and renders the row.
    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let warnings = pc
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        rendered.contains("STR1"),
        "rendered missing STR1 — end-to-end broke: {rendered:?}\nFULL: {pc}"
    );
    assert!(warnings.is_empty(), "warnings emitted: {warnings:?}");
}

// ---------------------------------------------------------------------------
// E2E 7 — alias default_limit is respected when URI omits `?limit=`.
// Bootstrap an alias with default_limit=2 against 3 matching rows; expect
// 2 rendered without a URI override.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_alias_default_limit_applied_without_uri_override() {
    let layout = make_layout();
    let table = "wire_e2e_default_limit";

    bootstrap_mini_app_table_full(
        &layout.mini_app_user_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "DL1"}),
            json!({"status": "active", "title": "DL2"}),
            json!({"status": "active", "title": "DL3"}),
        ],
        "active",
        r#"{"type":"eq","field":"status","value":"active"}"#,
        None,
        Some(2),
        None,
    )
    .await;

    let mut client = McpClient::spawn(&layout.wire_db, &layout.mini_app_user_dir);
    let persona_id = "alias_default_limit";
    wire_one_slot(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table}?alias=active"),
        &each_title_template("Default-limit"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = pc
        .get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let count = ["DL1", "DL2", "DL3"]
        .iter()
        .filter(|t| rendered.contains(*t))
        .count();
    assert_eq!(
        count, 2,
        "expected 2 rows under default_limit=2, got {count} in {rendered:?}"
    );
}
