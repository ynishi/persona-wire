//! E2E test for the `mini-app://table?alias=N` URI form through the real
//! `persona-wire mcp` stdio binary.
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

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_persona-wire");

// ---------------------------------------------------------------------------
// McpClient — JSON-RPC over stdio against a spawned `persona-wire mcp`.
// ---------------------------------------------------------------------------

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn(wire_db: &Path, mini_app_user_dir: &Path) -> Self {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .env("PERSONA_WIRE_DB", wire_db)
            .env("MINI_APP_USER_DIR", mini_app_user_dir)
            .env_remove("MINI_APP_PROJECT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn persona-wire mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut c = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        c.handshake();
        c
    }

    fn send_line(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).unwrap();
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv_for(&mut self, wanted_id: u64) -> Value {
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .expect("read mcp stdout line");
            assert!(n > 0, "mcp server closed stdout before response");
            let v: Value = serde_json::from_str(line.trim()).unwrap_or_else(|e| {
                panic!("mcp emitted non-JSON line: {line:?} ({e})");
            });
            match v.get("id").and_then(Value::as_u64) {
                Some(id) if id == wanted_id => return v,
                _ => continue,
            }
        }
    }

    fn handshake(&mut self) {
        let id = self.next_id();
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "persona-wire-e2e", "version": "0"},
            }
        }));
        let resp = self.recv_for(id);
        assert!(resp.get("error").is_none(), "initialize failed: {resp:?}");
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }));
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id();
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let resp = self.recv_for(id);
        assert!(resp.get("error").is_none(), "rpc {method} error: {resp:?}");
        resp.get("result").cloned().expect("result missing")
    }

    fn list_tools(&mut self) -> Vec<Value> {
        let result = self.rpc("tools/list", json!({}));
        result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .expect("tools array")
    }

    /// Invoke an MCP tool and return its single text content payload as a
    /// raw string. persona-wire-mcp emits either plain text (register
    /// commands, "registered spec: ...") or JSON-encoded bodies (query /
    /// prompt_context). Callers JSON-parse the result when they expect
    /// a structured body.
    fn call_tool_text(&mut self, name: &str, arguments: Value) -> String {
        let result = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        assert_ne!(
            result.get("isError").and_then(Value::as_bool),
            Some(true),
            "tool {name} returned error: {result:?}"
        );
        result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("tool {name} produced no text content: {result:?}"))
            .to_string()
    }

    /// Same as [`call_tool_text`] but JSON-parses the result body.
    fn call_tool_json(&mut self, name: &str, arguments: Value) -> Value {
        let text = self.call_tool_text(name, arguments);
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("tool text was not JSON: {text:?} ({e})");
        })
    }

    /// Best-effort variant: invoke an MCP tool, return Ok(text) on success
    /// and Err(message) on tool error. Used for idempotent setups (e.g.
    /// persona node creation that may already exist).
    fn try_call_tool_text(&mut self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!("{result:?}"));
        }
        Ok(result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Layout — tempdir for wire DB + mini-app store side by side.
// ---------------------------------------------------------------------------

struct Layout {
    _tmp: TempDir,
    wire_db: PathBuf,
    mini_app_user_dir: PathBuf,
}

fn make_layout() -> Layout {
    let tmp = TempDir::new().expect("tempdir");
    let wire_db = tmp.path().join("wire.db");
    let mini_app_user_dir = tmp.path().join("mini-app");
    std::fs::create_dir_all(&mini_app_user_dir).unwrap();
    Layout {
        _tmp: tmp,
        wire_db,
        mini_app_user_dir,
    }
}

/// Bootstrap one mini-app table under `<user_dir>/<table>/{<table>.db, schema.yaml}`
/// with the supplied schema YAML and seed rows, then register a single alias.
async fn bootstrap_mini_app_table(
    user_dir: &Path,
    table: &str,
    schema_yaml: &str,
    rows: Vec<serde_json::Value>,
    alias_name: &str,
    filter_json: &str,
    params_schema: Option<String>,
) {
    bootstrap_mini_app_table_full(
        user_dir,
        table,
        schema_yaml,
        rows,
        alias_name,
        filter_json,
        params_schema,
        None,
        None,
    )
    .await;
}

/// Extended bootstrap helper allowing an explicit `default_limit` on the alias
/// and an optional second alias for tests that need two aliases on one table.
///
/// `extra_alias`: Optional second alias as `(name, filter_json, params_schema,
/// default_limit)` for tests that exercise two aliases on the same table.
#[allow(clippy::too_many_arguments)]
async fn bootstrap_mini_app_table_full(
    user_dir: &Path,
    table: &str,
    schema_yaml: &str,
    rows: Vec<serde_json::Value>,
    alias_name: &str,
    filter_json: &str,
    params_schema: Option<String>,
    default_limit: Option<u32>,
    extra_alias: Option<(String, String, Option<String>, Option<u32>)>,
) {
    let table_dir = user_dir.join(table);
    std::fs::create_dir_all(&table_dir).unwrap();
    let schema_path = table_dir.join("schema.yaml");
    std::fs::write(&schema_path, schema_yaml).unwrap();
    let db_path = table_dir.join(format!("{table}.db"));
    let schema = mini_app_core::schema::load_from_path(&schema_path).unwrap();
    let store = mini_app_core::store::Store::open(&db_path, schema)
        .await
        .unwrap();
    for row in rows {
        store.create(row).await.unwrap();
    }
    store
        .alias_create(alias_name, filter_json, default_limit, None, params_schema)
        .await
        .unwrap();
    if let Some((n, f, ps, dl)) = extra_alias {
        store.alias_create(&n, &f, dl, None, ps).await.unwrap();
    }
}

/// Register spec + projection + persona + wiring_entry + edge for one axis,
/// wiring `<persona>.<axis>` at the supplied `source_uri`. Returns once the
/// graph is ready for `wire_prompt_context`.
fn wire_one_axis(
    client: &mut McpClient,
    persona_id: &str,
    axis: &str,
    source_uri: &str,
    template: &str,
) {
    let spec_name = format!("{persona_id}.spec.{axis}");
    let projection_name = format!("{persona_id}.section.{axis}");
    let entry_id = format!("{persona_id}.{axis}");

    client.call_tool_text(
        "wire_spec_register",
        json!({
            "name": spec_name,
            "json": "{\"TypeIs\":\"outline_node\"}",
        }),
    );
    client.call_tool_text(
        "wire_projection_register",
        json!({
            "name": projection_name,
            "spec_ref": spec_name,
            "template": template,
            "target_form": "markdown",
        }),
    );
    // Persona node may already exist (multi-axis tests). Tolerate dup via the
    // best-effort path (UNIQUE violation is silently ignored).
    let _ = client.try_call_tool_text(
        "wire_node_create",
        json!({
            "id": persona_id,
            "type": "persona",
            "metadata": {},
        }),
    );
    client.call_tool_text(
        "wire_node_create",
        json!({
            "id": entry_id,
            "type": "outline_node",
            "metadata": {
                "persona": persona_id,
                "axis": axis,
                "source_uri": source_uri,
            },
        }),
    );
    client.call_tool_text(
        "wire_edge_create",
        json!({
            "id": format!("e.{persona_id}.{axis}"),
            "src": persona_id,
            "tgt": entry_id,
            "kind": "routes_to",
        }),
    );
}

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

const STATUS_TITLE_SCHEMA: &str =
    "table: T\nfields:\n- name: status\n  type: string\n  required: true\n- name: title\n  type: string\n  required: true\n";

const TO_BODY_SCHEMA: &str =
    "table: T\nfields:\n- name: to\n  type: string\n  required: true\n- name: body\n  type: string\n  required: true\n";

fn each_title_template(axis: &str) -> String {
    format!(
        "## {axis}\n{{{{#each entries.[0].fetched_data.rows}}}}- {{{{this.data.title}}}}\n{{{{/each}}}}",
    )
}

fn each_body_template(axis: &str) -> String {
    format!(
        "## {axis}\n{{{{#each entries.[0].fetched_data.rows}}}}- {{{{this.data.body}}}}\n{{{{/each}}}}",
    )
}

fn schema_for(table: &str, base: &str) -> String {
    base.replacen("table: T", &format!("table: {table}"), 1)
}

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
    wire_one_axis(
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
    wire_one_axis(
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
    wire_one_axis(
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
// surfaces a warning, axis is skipped, prompt_context is empty (no panic).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_alias_not_found_warns_and_skips_axis() {
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
    wire_one_axis(
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
    wire_one_axis(
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
// E2E 6 — multi-axis: one persona wired across two different alias URIs
// against two different mini-app tables, plus one plain (alias-free) axis.
// Verifies that `wire_prompt_context` walks the wiring graph and renders
// every axis through the Adapter dispatch.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn e2e_multi_axis_alias_and_plain_coexist() {
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

    wire_one_axis(
        &mut client,
        persona_id,
        "active",
        &format!("mini-app://{table_active}?alias=active"),
        &each_title_template("Active"),
    );
    wire_one_axis(
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
        "active axis missing or leaked: {rendered:?}"
    );
    assert!(
        rendered.contains("MX1") && !rendered.contains("SX1"),
        "inbox axis missing or leaked: {rendered:?}"
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
            "id": persona_id,
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
            "id": entry_id,
            "type": "outline_node",
            "metadata": stringified_meta,
        }),
    );
    // Sibling: wire_edge_create also takes metadata through normalize_metadata.
    let stringified_edge_meta = r#"{"note":"regression-edge"}"#;
    client.call_tool_text(
        "wire_edge_create",
        json!({
            "id": format!("e.{persona_id}.active"),
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
    wire_one_axis(
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
