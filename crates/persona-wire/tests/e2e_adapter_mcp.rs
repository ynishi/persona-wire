//! E2E round-trip test for the `mcp://` Adapter (issue 3ca10673; parent
//! ea99f9e1 acceptance #3 closure).
//!
//! Strategy: dogfood `persona-wire mcp` as the MCP server endpoint. The test
//! spawns an Outer `persona-wire mcp` and, through its tool surface,
//!
//!   1. registers a `mcp_server` graph node `inner` whose
//!      `metadata.endpoint` points at the same `persona-wire` binary with a
//!      fresh `PERSONA_WIRE_DB` so it boots a second instance,
//!   2. registers a wiring slot whose `source_uri` is
//!      `mcp://inner/tools/wire_doctor`,
//!   3. calls `wire_prompt_context`, which makes the Outer's
//!      `McpAdapter` spawn the Inner over stdio, call `wire_doctor`, and
//!      return the resulting CallToolResult.
//!
//! The rendered prompt therefore embeds the Inner's `wire_doctor` markdown.
//! Asserting on the `verdict` literal proves the full round-trip
//! resolver → connect → call_tool → result deserialization actually fires.
//!
//! Inner runs against its own SQLite db (created by `serve_stdio`'s migrate
//! + seed on first open), so it does not contend with Outer for locks.

mod common;

use std::path::PathBuf;

use common::{make_layout, McpClient};
use serde_json::{json, Value};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_persona-wire");

#[test]
fn mcp_adapter_round_trips_through_inner_persona_wire_mcp() {
    let outer_layout = make_layout();

    // Inner gets its own db + mini-app dir to keep SQLite handles independent.
    let inner_tmp = TempDir::new().expect("inner tempdir");
    let inner_db: PathBuf = inner_tmp.path().join("inner-wire.db");
    let inner_mini_app: PathBuf = inner_tmp.path().join("inner-mini-app");
    std::fs::create_dir_all(&inner_mini_app).unwrap();

    let mut outer = McpClient::spawn(&outer_layout.wire_db, &outer_layout.mini_app_user_dir);

    // Endpoint payload that the production SqliteEndpointResolver will read
    // back when `mcp://inner/tools/wire_doctor` is fetched.
    let endpoint = json!({
        "kind": "stdio",
        "command": BIN,
        "args": ["mcp"],
        "env": {
            "PERSONA_WIRE_DB": inner_db.to_str().unwrap(),
            "MINI_APP_USER_DIR": inner_mini_app.to_str().unwrap(),
        },
    });

    // 1. Register the `mcp_server` endpoint node. `maintenance_exempt: true`
    //    keeps it out of doctor's orphan_node probe (issue 15a46ce6 path).
    outer.call_tool_text(
        "wire_node_create",
        json!({
            "name": "inner",
            "type": "mcp_server",
            "metadata": {
                "endpoint": endpoint,
                "maintenance_exempt": true,
            },
        }),
    );

    // 2. Register spec / projection / persona / wiring entry that points the
    //    `default` slot at the MCP fetch URI.
    let persona = "_e2e_mcp_adapter";
    let slot = "doctor";
    let spec_name = format!("{persona}.spec.{slot}");
    let projection_name = format!("{persona}.section.{slot}");
    let entry_id = format!("{persona}.{slot}");

    outer.call_tool_text(
        "wire_spec_register",
        json!({
            "name": spec_name,
            "json": "{\"TypeIs\":\"outline_node\"}",
        }),
    );

    // Template surfaces the round-trip evidence: pull the markdown out of
    // the CallToolResult shape `{content:[{type,text}], isError}`.
    let template = format!("## {slot}\n{{{{{{entries.[0].fetched_data.content.[0].text}}}}}}\n");
    outer.call_tool_text(
        "wire_projection_register",
        json!({
            "name": projection_name,
            "spec_ref": spec_name,
            "template": template,
            "target_form": "markdown",
        }),
    );

    outer.call_tool_text(
        "wire_node_create",
        json!({
            "name": persona,
            "type": "persona",
            "metadata": {},
        }),
    );
    outer.call_tool_text(
        "wire_node_create",
        json!({
            "name": entry_id,
            "type": "outline_node",
            "metadata": {
                "persona": persona,
                "axis": slot,
                "source_uri": format!("mcp://inner/tools/wire_doctor"),
            },
        }),
    );
    outer.call_tool_text(
        "wire_edge_create",
        json!({
            "name": format!("e.{persona}.{slot}"),
            "src": persona,
            "tgt": entry_id,
            "kind": "routes_to",
        }),
    );

    // 3. Round-trip via wire_prompt_context. If the McpAdapter fails to
    //    resolve / connect / call, `entries[0].fetched_data` would be Null
    //    and the rendered text would not contain `verdict`.
    let rendered = outer.call_tool_text("wire_prompt_context", json!({"persona_id": persona}));

    // wire_doctor's report_markdown always starts with a verdict line
    // (`# wire_doctor — verdict: HEALTHY / DEGRADED / BROKEN`). Treat the
    // literal as the round-trip canary.
    assert!(
        rendered.contains("verdict"),
        "expected wire_doctor markdown to flow through the McpAdapter; got: {rendered}"
    );

    // Sanity check: no adapter warnings about fetch failure should be embedded
    // in the JSON envelope returned by wire_prompt_context's tool path. The
    // `call_tool_text` helper returns the raw text content, so we additionally
    // inspect the structured form for `warnings`.
    let structured: Value =
        outer.call_tool_json("wire_prompt_context", json!({"persona_id": persona}));
    let warnings = structured
        .pointer("/warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let adapter_failures: Vec<&Value> = warnings
        .iter()
        .filter(|w| {
            w.as_str()
                .map(|s| s.contains("adapter fetch failed") || s.contains("registry route failed"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        adapter_failures.is_empty(),
        "unexpected adapter failure warnings: {adapter_failures:?}"
    );
}
