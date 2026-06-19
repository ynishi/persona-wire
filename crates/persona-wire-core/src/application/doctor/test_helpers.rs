//! Test fixtures shared across probe unit tests.

#![cfg(test)]

use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::domain::error::WireResult;
use crate::domain::graph::{Edge, Node};
use crate::infrastructure::storage::SqliteStorage;
use serde_json::{json, Value};

pub fn setup() -> SqliteStorage {
    let s = SqliteStorage::open_in_memory().unwrap();
    s.migrate().unwrap();
    s.seed_default_types().unwrap();
    s
}

pub fn node(id: &str, ty: &str, metadata: Value) -> Node {
    Node {
        id: id.into(),
        r#type: ty.into(),
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

pub fn persona_node(id: &str, persona: &str) -> Node {
    node(id, "persona", json!({ "persona": persona }))
}

pub fn bare_persona_node(id: &str) -> Node {
    node(id, "persona", json!({}))
}

pub fn edge(id: &str, src: &str, tgt: &str) -> Edge {
    Edge {
        id: id.into(),
        src_node: src.into(),
        tgt_node: tgt.into(),
        kind: "routes_to".into(),
        severity: None,
        metadata: json!({}),
        version: 1,
        prev_id: None,
    }
}

/// Run a probe on the given storage and return all emitted findings.
pub fn scan<P: Probe>(
    probe: &P,
    storage: &SqliteStorage,
    persona: Option<&str>,
) -> WireResult<Vec<crate::application::doctor::finding::Finding>> {
    let ctx = ProbeCtx {
        storage,
        persona_filter: persona.map(|s| s.to_string()),
    };
    let mut sink = FindingSink::new();
    probe.scan(&ctx, &mut sink)?;
    Ok(sink.into_vec())
}

/// Build a workflow_def Node with given trigger/action JSON literals.
pub fn workflow_node(
    id: &str,
    persona: Option<&str>,
    trigger: Value,
    action: Value,
    enabled: bool,
) -> Node {
    let mut meta = serde_json::Map::new();
    if let Some(p) = persona {
        meta.insert("persona".into(), Value::String(p.into()));
    }
    meta.insert("trigger".into(), trigger);
    meta.insert("action".into(), action);
    meta.insert("enabled".into(), Value::Bool(enabled));
    node(id, "workflow_def", Value::Object(meta))
}
