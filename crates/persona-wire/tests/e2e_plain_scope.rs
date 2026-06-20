//! E2E test for plain form (alias 不在) + `?scope=<project>&root=<dir>` path.
//!
//! Covers `MiniAppAdapter::fetch_table_via_spec` (= alias 不在 list-all path):
//!
//! - `?scope=<project>&root=<dir>` → Project-scope `<root>/<table>/<table>.db`
//!   list-all (no filter)
//!
//! Plain form lets callers wire a project-scoped mini-app table that lives
//! outside the User-scope `~/.mini-app/` base, without registering an alias.
//! Sibling: `e2e_alias_scope.rs` exercises the alias-routed scope path.

mod common;

use common::{
    bootstrap_mini_app_table_no_alias, each_title_template, make_layout_with_project, schema_for,
    wire_one_slot, McpClient, STATUS_TITLE_SCHEMA,
};
use serde_json::{json, Value};

fn rendered_of(pc: &Value) -> String {
    pc.get("prompt_context")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn warnings_of(pc: &Value) -> Vec<String> {
    pc.get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn e2e_plain_form_with_scope_project_lists_all_rows() {
    let layout = make_layout_with_project();
    let table = "wire_e2e_plain_scope_proj";

    // Project-scope mini-app table: rows live under project_dir, not user_dir.
    // No alias registration — exercises the `fetch_table_via_spec` list-all path.
    bootstrap_mini_app_table_no_alias(
        &layout.mini_app_project_dir,
        table,
        &schema_for(table, STATUS_TITLE_SCHEMA),
        vec![
            json!({"status": "active", "title": "PlainProj1"}),
            json!({"status": "closed", "title": "PlainProj2"}),
            json!({"status": "active", "title": "PlainProj3"}),
        ],
    )
    .await;

    let mut client = McpClient::spawn_with_project(
        &layout.wire_db,
        &layout.mini_app_user_dir,
        &layout.mini_app_project_dir,
    );
    let persona_id = "plain_scope_proj";
    let project_root_str = layout.mini_app_project_dir.to_string_lossy();

    // plain form: alias 不在、 `?scope=<project>&root=<dir>` だけで project-scope
    // mini-app table を直接 list-all する。
    wire_one_slot(
        &mut client,
        persona_id,
        "all",
        &format!("mini-app://{table}?scope=example-project&root={project_root_str}"),
        &each_title_template("PlainAll"),
    );

    let pc = client.call_tool_json("wire_prompt_context", json!({"persona_id": persona_id}));
    let rendered = rendered_of(&pc);
    let warnings = warnings_of(&pc);

    // plain form list-all は filter 無しで全 3 行が render される。
    assert!(
        rendered.contains("PlainProj1")
            && rendered.contains("PlainProj2")
            && rendered.contains("PlainProj3"),
        "plain form scope=<project> did not render all project-scope rows: \
         {rendered:?}\nFULL: {pc}"
    );
    assert!(
        warnings.is_empty(),
        "warnings emitted under plain form scope=<project> happy path: {warnings:?}"
    );
}
