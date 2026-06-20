//! Shared E2E test fixtures used by both `e2e_alias_mcp.rs` (per-table
//! `_aliases` path) and `e2e_alias_scope.rs` (GlobalAliasStorage scope
//! path). Each integration-test binary compiles this module independently,
//! so `#![allow(dead_code)]` keeps unused-from-this-binary helpers from
//! producing per-binary warnings.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_persona-wire");

// ---------------------------------------------------------------------------
// McpClient — JSON-RPC over stdio against a spawned `persona-wire mcp`.
// ---------------------------------------------------------------------------

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    pub fn spawn(wire_db: &Path, mini_app_user_dir: &Path) -> Self {
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

    /// Like `spawn`, but also sets `MINI_APP_PROJECT_DIR` so the
    /// `?scope=<project-name>` Project-scope path can resolve its
    /// `_global.db`.
    pub fn spawn_with_project(
        wire_db: &Path,
        mini_app_user_dir: &Path,
        mini_app_project_dir: &Path,
    ) -> Self {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .env("PERSONA_WIRE_DB", wire_db)
            .env("MINI_APP_USER_DIR", mini_app_user_dir)
            .env("MINI_APP_PROJECT_DIR", mini_app_project_dir)
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

    pub fn list_tools(&mut self) -> Vec<Value> {
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
    pub fn call_tool_text(&mut self, name: &str, arguments: Value) -> String {
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
    pub fn call_tool_json(&mut self, name: &str, arguments: Value) -> Value {
        let text = self.call_tool_text(name, arguments);
        serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!("tool text was not JSON: {text:?} ({e})");
        })
    }

    /// Best-effort variant: invoke an MCP tool, return Ok(text) on success
    /// and Err(message) on tool error. Used for idempotent setups (e.g.
    /// persona node creation that may already exist).
    pub fn try_call_tool_text(&mut self, name: &str, arguments: Value) -> Result<String, String> {
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

pub struct Layout {
    pub _tmp: TempDir,
    pub wire_db: PathBuf,
    pub mini_app_user_dir: PathBuf,
}

pub fn make_layout() -> Layout {
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

/// Extended layout: User scope mini-app dir + Project scope mini-app dir
/// side by side. Used by tests that exercise `?scope=<project-name>`
/// GlobalAliasStorage path.
pub struct LayoutWithProject {
    pub _tmp: TempDir,
    pub wire_db: PathBuf,
    pub mini_app_user_dir: PathBuf,
    pub mini_app_project_dir: PathBuf,
}

pub fn make_layout_with_project() -> LayoutWithProject {
    let tmp = TempDir::new().expect("tempdir");
    let wire_db = tmp.path().join("wire.db");
    let mini_app_user_dir = tmp.path().join("mini-app-user");
    let mini_app_project_dir = tmp.path().join("mini-app-project");
    std::fs::create_dir_all(&mini_app_user_dir).unwrap();
    std::fs::create_dir_all(&mini_app_project_dir).unwrap();
    LayoutWithProject {
        _tmp: tmp,
        wire_db,
        mini_app_user_dir,
        mini_app_project_dir,
    }
}

// ---------------------------------------------------------------------------
// mini-app bootstrap helpers.
// ---------------------------------------------------------------------------

/// Bootstrap one mini-app table under `<user_dir>/<table>/{<table>.db, schema.yaml}`
/// with the supplied schema YAML and seed rows, then register a single alias
/// via the per-table `_aliases` path (legacy compat path).
pub async fn bootstrap_mini_app_table(
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
pub async fn bootstrap_mini_app_table_full(
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

/// Schema-only bootstrap: create the `<table>/{<table>.db, schema.yaml}`
/// layout + seed rows, **without** registering any per-table alias. Used by
/// scope tests that register their alias via `GlobalAliasStorage` instead.
pub async fn bootstrap_mini_app_table_no_alias(
    base_dir: &Path,
    table: &str,
    schema_yaml: &str,
    rows: Vec<serde_json::Value>,
) {
    let table_dir = base_dir.join(table);
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
}

// ---------------------------------------------------------------------------
// Wiring helpers.
// ---------------------------------------------------------------------------

/// Register spec + projection + persona + wiring_entry + edge for one slot,
/// wiring `<persona>.<slot>` at the supplied `source_uri`. Returns once the
/// graph is ready for `wire_prompt_context`.
pub fn wire_one_slot(
    client: &mut McpClient,
    persona_id: &str,
    slot: &str,
    source_uri: &str,
    template: &str,
) {
    let spec_name = format!("{persona_id}.spec.{slot}");
    let projection_name = format!("{persona_id}.section.{slot}");
    let entry_id = format!("{persona_id}.{slot}");

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
    // Persona node may already exist (multi-slot tests). Tolerate dup via the
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
                // `axis` is the legacy storage-compat key for Slot
                // (see docs/design/render-trinity-domain-entity.md Appendix B).
                "axis": slot,
                "source_uri": source_uri,
            },
        }),
    );
    client.call_tool_text(
        "wire_edge_create",
        json!({
            "id": format!("e.{persona_id}.{slot}"),
            "src": persona_id,
            "tgt": entry_id,
            "kind": "routes_to",
        }),
    );
}

// ---------------------------------------------------------------------------
// Schemas + templates reused across alias tests.
// ---------------------------------------------------------------------------

pub const STATUS_TITLE_SCHEMA: &str =
    "table: T\nfields:\n- name: status\n  type: string\n  required: true\n- name: title\n  type: string\n  required: true\n";

pub const TO_BODY_SCHEMA: &str =
    "table: T\nfields:\n- name: to\n  type: string\n  required: true\n- name: body\n  type: string\n  required: true\n";

pub fn each_title_template(slot: &str) -> String {
    format!(
        "## {slot}\n{{{{#each entries.[0].fetched_data.rows}}}}- {{{{this.data.title}}}}\n{{{{/each}}}}",
    )
}

pub fn each_body_template(slot: &str) -> String {
    format!(
        "## {slot}\n{{{{#each entries.[0].fetched_data.rows}}}}- {{{{this.data.body}}}}\n{{{{/each}}}}",
    )
}

pub fn schema_for(table: &str, base: &str) -> String {
    base.replacen("table: T", &format!("table: {table}"), 1)
}
