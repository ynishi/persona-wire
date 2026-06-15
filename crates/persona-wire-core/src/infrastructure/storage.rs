//! SQLite storage adapter.
//!
//! P1 scope: type_registry / nodes / edges / versions schema + CRUD primitive.
//! specifications / projections / workflow_runs tables are P2+ carry.

use std::path::PathBuf;

use crate::domain::autoversion::{VersionRecord, VersionTargetKind};
use crate::domain::error::{WireError, WireResult};
use crate::domain::graph::{Edge, EdgeId, Node, NodeId, Severity};
use rusqlite::{params, Connection, OptionalExtension, Row};

/// Resolve the default DB path for persona-wire. Follows the persona-x family
/// convention (= persona-work `store-sqlite/src/lib.rs:96-109`):
///
/// 1. `$XDG_DATA_HOME/persona-wire/store.db` if `XDG_DATA_HOME` is set
/// 2. `$HOME/.persona-wire/store.db` otherwise
///
/// Env override (`PERSONA_WIRE_DB`) and CLI `--db` override are the caller's
/// responsibility (the CLI / MCP wrapper applies that precedence before
/// falling back to this helper).
pub fn default_db_path() -> WireResult<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("persona-wire");
        p.push("store.db");
        return Ok(p);
    }
    let home =
        std::env::var_os("HOME").ok_or_else(|| WireError::Storage("HOME not set".to_string()))?;
    let mut p = PathBuf::from(home);
    p.push(".persona-wire");
    p.push("store.db");
    Ok(p)
}

pub struct SqliteStorage {
    conn: Connection,
}

impl SqliteStorage {
    pub fn open(path: &str) -> WireResult<Self> {
        let conn = Connection::open(path).map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> WireResult<Self> {
        let conn = Connection::open_in_memory().map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(Self { conn })
    }

    pub fn migrate(&self) -> WireResult<()> {
        self.conn
            .execute_batch(SCHEMA)
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Seed concept-doc §4.1/§4.2 type vocabulary (9 node + 9 edge).
    /// Idempotent via `INSERT OR IGNORE`.
    pub fn seed_default_types(&self) -> WireResult<()> {
        const SEED: &[(&str, &str, Option<&str>)] = &[
            ("outline_node", "node", None),
            ("mia_artifact", "node", None),
            ("pp_field", "node", None),
            ("ma_row", "node", None),
            ("pj_chapter", "node", None),
            ("persona", "node", None),
            ("channel", "node", None),
            ("workflow_def", "node", None),
            ("projection_def", "node", None),
            ("triggers_review_of", "edge", Some("hard,soft,advisory")),
            ("cites", "edge", None),
            ("derives_from", "edge", None),
            ("routes_to", "edge", None),
            ("instance_of", "edge", None),
            ("versions_of", "edge", None),
            ("constraint", "edge", None),
            ("transitions_to", "edge", None),
            ("projects_into", "edge", None),
        ];
        let mut stmt = self
            .conn
            .prepare(
                "INSERT OR IGNORE INTO type_registry (name, kind, schema_json, severity_allowed) \
                 VALUES (?1, ?2, NULL, ?3)",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        for (name, kind, sev) in SEED {
            stmt.execute(params![name, kind, sev])
                .map_err(|e| WireError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    pub fn list_types(&self) -> WireResult<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, kind FROM type_registry ORDER BY kind, name")
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_types_by_kind(&self, kind: &str) -> WireResult<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM type_registry WHERE kind = ?1 ORDER BY name")
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![kind], |row| row.get::<_, String>(0))
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn insert_node(&self, node: &Node) -> WireResult<()> {
        let metadata_str =
            serde_json::to_string(&node.metadata).map_err(|e| WireError::Storage(e.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO nodes (id, type, sot_ref, confidence, applicability, \
                 last_verified_at, review_due, version, prev_id, metadata) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    node.id,
                    node.r#type,
                    node.sot_ref,
                    node.confidence,
                    node.applicability,
                    node.last_verified_at,
                    node.review_due,
                    node.version,
                    node.prev_id,
                    metadata_str,
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_node(&self, id: &NodeId) -> WireResult<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, type, sot_ref, confidence, applicability, last_verified_at, \
                 review_due, version, prev_id, metadata FROM nodes WHERE id = ?1",
                params![id],
                row_to_node,
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_nodes_by_type(&self, type_name: &str) -> WireResult<Vec<Node>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, type, sot_ref, confidence, applicability, last_verified_at, \
                 review_due, version, prev_id, metadata FROM nodes WHERE type = ?1 ORDER BY id",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![type_name], row_to_node)
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn insert_edge(&self, edge: &Edge) -> WireResult<()> {
        let sev = edge.severity.map(severity_to_str);
        let metadata_str =
            serde_json::to_string(&edge.metadata).map_err(|e| WireError::Storage(e.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO edges (id, src_node, tgt_node, kind, severity, metadata, version, prev_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    edge.id,
                    edge.src_node,
                    edge.tgt_node,
                    edge.kind,
                    sev,
                    metadata_str,
                    edge.version,
                    edge.prev_id,
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_edge(&self, id: &EdgeId) -> WireResult<Option<Edge>> {
        self.conn
            .query_row(
                "SELECT id, src_node, tgt_node, kind, severity, metadata, version, prev_id \
                 FROM edges WHERE id = ?1",
                params![id],
                row_to_edge,
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_edges_from(&self, src_node: &NodeId) -> WireResult<Vec<Edge>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, src_node, tgt_node, kind, severity, metadata, version, prev_id \
                 FROM edges WHERE src_node = ?1 ORDER BY id",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![src_node], row_to_edge)
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_edges_to(&self, tgt_node: &NodeId) -> WireResult<Vec<Edge>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, src_node, tgt_node, kind, severity, metadata, version, prev_id \
                 FROM edges WHERE tgt_node = ?1 ORDER BY id",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![tgt_node], row_to_edge)
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn insert_version_record(&self, rec: &VersionRecord) -> WireResult<()> {
        let kind = match rec.target_kind {
            VersionTargetKind::Node => "node",
            VersionTargetKind::Edge => "edge",
        };
        let diff_str =
            serde_json::to_string(&rec.diff).map_err(|e| WireError::Storage(e.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO versions (target_kind, target_id, version, diff, ts, author) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    kind,
                    rec.target_id,
                    rec.version,
                    diff_str,
                    rec.ts,
                    rec.author,
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn count_versions(
        &self,
        target_kind: VersionTargetKind,
        target_id: &str,
    ) -> WireResult<i64> {
        let kind = match target_kind {
            VersionTargetKind::Node => "node",
            VersionTargetKind::Edge => "edge",
        };
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM versions WHERE target_kind = ?1 AND target_id = ?2",
                params![kind, target_id],
                |row| row.get(0),
            )
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    // ---- Specifications ----

    pub fn upsert_specification(&self, name: &str, expr_json: &str) -> WireResult<()> {
        self.conn
            .execute(
                "INSERT INTO specifications (name, expr_json, created_at) \
                 VALUES (?1, ?2, 0) \
                 ON CONFLICT(name) DO UPDATE SET expr_json = excluded.expr_json",
                params![name, expr_json],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_specification(&self, name: &str) -> WireResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT expr_json FROM specifications WHERE name = ?1",
                params![name],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_specifications(&self) -> WireResult<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, expr_json FROM specifications ORDER BY name")
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    // ---- Projections ----

    pub fn upsert_projection(
        &self,
        name: &str,
        spec_ref: &str,
        template: &str,
        target_form: &str,
    ) -> WireResult<()> {
        self.conn
            .execute(
                "INSERT INTO projections (name, spec_ref, template, target_form, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 0) \
                 ON CONFLICT(name) DO UPDATE SET \
                    spec_ref = excluded.spec_ref, \
                    template = excluded.template, \
                    target_form = excluded.target_form",
                params![name, spec_ref, template, target_form],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_projection(&self, name: &str) -> WireResult<Option<(String, String, String)>> {
        self.conn
            .query_row(
                "SELECT spec_ref, template, target_form FROM projections WHERE name = ?1",
                params![name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_projections(&self) -> WireResult<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM projections ORDER BY name")
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }
}

impl crate::domain::repository::Repository for SqliteStorage {
    fn list_types_by_kind(&self, kind: &str) -> WireResult<Vec<String>> {
        SqliteStorage::list_types_by_kind(self, kind)
    }

    fn insert_node(&self, node: &Node) -> WireResult<()> {
        SqliteStorage::insert_node(self, node)
    }

    fn get_node(&self, id: &NodeId) -> WireResult<Option<Node>> {
        SqliteStorage::get_node(self, id)
    }

    fn list_nodes_by_type(&self, type_name: &str) -> WireResult<Vec<Node>> {
        SqliteStorage::list_nodes_by_type(self, type_name)
    }

    fn insert_edge(&self, edge: &Edge) -> WireResult<()> {
        SqliteStorage::insert_edge(self, edge)
    }

    fn get_edge(&self, id: &EdgeId) -> WireResult<Option<Edge>> {
        SqliteStorage::get_edge(self, id)
    }

    fn list_edges_from(&self, src_node: &NodeId) -> WireResult<Vec<Edge>> {
        SqliteStorage::list_edges_from(self, src_node)
    }

    fn list_edges_to(&self, tgt_node: &NodeId) -> WireResult<Vec<Edge>> {
        SqliteStorage::list_edges_to(self, tgt_node)
    }

    fn insert_version_record(&self, rec: &VersionRecord) -> WireResult<()> {
        SqliteStorage::insert_version_record(self, rec)
    }

    fn count_versions(&self, target_kind: VersionTargetKind, target_id: &str) -> WireResult<i64> {
        SqliteStorage::count_versions(self, target_kind, target_id)
    }
}

fn severity_to_str(s: Severity) -> &'static str {
    match s {
        Severity::Hard => "hard",
        Severity::Soft => "soft",
        Severity::Advisory => "advisory",
    }
}

fn row_to_node(row: &Row) -> rusqlite::Result<Node> {
    let metadata_str: String = row.get(9)?;
    let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
    Ok(Node {
        id: row.get(0)?,
        r#type: row.get(1)?,
        sot_ref: row.get(2)?,
        confidence: row.get(3)?,
        applicability: row.get(4)?,
        last_verified_at: row.get(5)?,
        review_due: row.get(6)?,
        version: row.get::<_, i64>(7)? as u32,
        prev_id: row.get(8)?,
        metadata,
    })
}

fn row_to_edge(row: &Row) -> rusqlite::Result<Edge> {
    let sev_str: Option<String> = row.get(4)?;
    let severity = sev_str.and_then(|s| match s.as_str() {
        "hard" => Some(Severity::Hard),
        "soft" => Some(Severity::Soft),
        "advisory" => Some(Severity::Advisory),
        _ => None,
    });
    let metadata_str: String = row.get(5)?;
    let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
    Ok(Edge {
        id: row.get(0)?,
        src_node: row.get(1)?,
        tgt_node: row.get(2)?,
        kind: row.get(3)?,
        severity,
        metadata,
        version: row.get::<_, i64>(6)? as u32,
        prev_id: row.get(7)?,
    })
}

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS type_registry (
    name             TEXT PRIMARY KEY,
    kind             TEXT NOT NULL CHECK (kind IN ('node', 'edge')),
    schema_json      TEXT,
    severity_allowed TEXT
);

CREATE TABLE IF NOT EXISTS nodes (
    id                TEXT PRIMARY KEY,
    type              TEXT NOT NULL REFERENCES type_registry(name),
    sot_ref           TEXT,
    confidence        REAL,
    applicability     TEXT,
    last_verified_at  INTEGER,
    review_due        INTEGER,
    version           INTEGER NOT NULL DEFAULT 1,
    prev_id           TEXT,
    metadata          TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(type);

CREATE TABLE IF NOT EXISTS edges (
    id        TEXT PRIMARY KEY,
    src_node  TEXT NOT NULL REFERENCES nodes(id),
    tgt_node  TEXT NOT NULL REFERENCES nodes(id),
    kind      TEXT NOT NULL REFERENCES type_registry(name),
    severity  TEXT CHECK (severity IS NULL OR severity IN ('hard', 'soft', 'advisory')),
    metadata  TEXT NOT NULL DEFAULT '{}',
    version   INTEGER NOT NULL DEFAULT 1,
    prev_id   TEXT
);

CREATE INDEX IF NOT EXISTS idx_edges_src  ON edges(src_node);
CREATE INDEX IF NOT EXISTS idx_edges_tgt  ON edges(tgt_node);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);

CREATE TABLE IF NOT EXISTS versions (
    target_kind  TEXT NOT NULL CHECK (target_kind IN ('node', 'edge')),
    target_id    TEXT NOT NULL,
    version      INTEGER NOT NULL,
    diff         TEXT NOT NULL DEFAULT '{}',
    ts           INTEGER NOT NULL,
    author       TEXT,
    PRIMARY KEY (target_kind, target_id, version)
);

CREATE TABLE IF NOT EXISTS specifications (
    name        TEXT PRIMARY KEY,
    expr_json   TEXT NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS projections (
    name         TEXT PRIMARY KEY,
    spec_ref     TEXT NOT NULL,
    template     TEXT NOT NULL,
    target_form  TEXT NOT NULL CHECK (target_form IN ('prompt', 'markdown', 'json', 'ascii')),
    created_at   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS workflow_runs (
    id          TEXT PRIMARY KEY,
    def_node    TEXT NOT NULL,
    state       TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    metadata    TEXT NOT NULL DEFAULT '{}'
);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
    }

    fn bare_node(id: &str, type_: &str) -> Node {
        Node {
            id: id.into(),
            r#type: type_.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: json!({}),
        }
    }

    #[test]
    fn migrate_creates_all_tables() {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        let names: Vec<String> = s
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            names,
            vec![
                "edges",
                "nodes",
                "projections",
                "specifications",
                "type_registry",
                "versions",
                "workflow_runs",
            ]
        );
    }

    #[test]
    fn migrate_is_idempotent() {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.migrate().unwrap();
        s.migrate().unwrap();
    }

    #[test]
    fn seed_inserts_9_node_and_9_edge_types() {
        let s = setup();
        let nodes = s.list_types_by_kind("node").unwrap();
        let edges = s.list_types_by_kind("edge").unwrap();
        assert_eq!(nodes.len(), 9);
        assert_eq!(edges.len(), 9);
        assert!(nodes.contains(&"persona".to_string()));
        assert!(edges.contains(&"triggers_review_of".to_string()));
    }

    #[test]
    fn seed_is_idempotent() {
        let s = setup();
        s.seed_default_types().unwrap();
        s.seed_default_types().unwrap();
        assert_eq!(s.list_types().unwrap().len(), 18);
    }

    #[test]
    fn list_types_orders_by_kind_then_name() {
        let s = setup();
        let all = s.list_types().unwrap();
        // edges come before nodes (kind ordering 'e' < 'n')
        assert_eq!(all[0].1, "edge");
        assert_eq!(all[9].1, "node");
    }

    #[test]
    fn insert_and_get_node_roundtrip() {
        let s = setup();
        let mut n = bare_node("n1", "persona");
        n.sot_ref = Some("pp://alpha".into());
        n.confidence = Some(0.95);
        n.last_verified_at = Some(1_700_000_000);
        n.metadata = json!({"name": "alpha", "tags": ["dev"]});
        s.insert_node(&n).unwrap();

        let got = s.get_node(&"n1".into()).unwrap().expect("exists");
        assert_eq!(got.id, "n1");
        assert_eq!(got.r#type, "persona");
        assert_eq!(got.sot_ref.as_deref(), Some("pp://alpha"));
        assert_eq!(got.confidence, Some(0.95));
        assert_eq!(got.last_verified_at, Some(1_700_000_000));
        assert_eq!(got.metadata, json!({"name": "alpha", "tags": ["dev"]}));
    }

    #[test]
    fn get_node_returns_none_when_absent() {
        let s = setup();
        assert!(s.get_node(&"missing".into()).unwrap().is_none());
    }

    #[test]
    fn insert_node_rejects_unknown_type() {
        let s = setup();
        let n = bare_node("nx", "definitely_not_a_registered_type");
        let err = s.insert_node(&n);
        assert!(err.is_err(), "FK on nodes.type should reject unknown type");
    }

    #[test]
    fn list_nodes_by_type_filters() {
        let s = setup();
        s.insert_node(&bare_node("p1", "persona")).unwrap();
        s.insert_node(&bare_node("p2", "persona")).unwrap();
        s.insert_node(&bare_node("c1", "channel")).unwrap();
        let personas = s.list_nodes_by_type("persona").unwrap();
        assert_eq!(personas.len(), 2);
        assert_eq!(personas[0].id, "p1");
    }

    #[test]
    fn insert_and_list_edges_from() {
        let s = setup();
        s.insert_node(&bare_node("p_shi", "persona")).unwrap();
        s.insert_node(&bare_node("p_mia", "persona")).unwrap();
        let e = Edge {
            id: "e1".into(),
            src_node: "p_shi".into(),
            tgt_node: "p_mia".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({"weight": 1}),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&e).unwrap();

        let edges = s.list_edges_from(&"p_shi".into()).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id, "e1");
        assert_eq!(edges[0].kind, "routes_to");
        assert_eq!(edges[0].metadata, json!({"weight": 1}));

        // reverse direction empty
        let back = s.list_edges_from(&"p_mia".into()).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn list_edges_to_finds_incoming() {
        let s = setup();
        s.insert_node(&bare_node("a", "persona")).unwrap();
        s.insert_node(&bare_node("b", "persona")).unwrap();
        let e = Edge {
            id: "e_ab".into(),
            src_node: "a".into(),
            tgt_node: "b".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&e).unwrap();
        let in_b = s.list_edges_to(&"b".into()).unwrap();
        assert_eq!(in_b.len(), 1);
        assert_eq!(in_b[0].src_node, "a");
    }

    #[test]
    fn edge_with_severity_roundtrip_all_three() {
        let s = setup();
        s.insert_node(&bare_node("a", "outline_node")).unwrap();
        s.insert_node(&bare_node("b", "outline_node")).unwrap();
        for (id, sev) in [
            ("e_h", Severity::Hard),
            ("e_s", Severity::Soft),
            ("e_a", Severity::Advisory),
        ] {
            let e = Edge {
                id: id.into(),
                src_node: "a".into(),
                tgt_node: "b".into(),
                kind: "triggers_review_of".into(),
                severity: Some(sev),
                metadata: json!({}),
                version: 1,
                prev_id: None,
            };
            s.insert_edge(&e).unwrap();
        }
        let edges = s.list_edges_from(&"a".into()).unwrap();
        assert_eq!(edges.len(), 3);
        let mut sevs: Vec<_> = edges.iter().filter_map(|e| e.severity).collect();
        sevs.sort_by_key(|s| match s {
            Severity::Hard => 0,
            Severity::Soft => 1,
            Severity::Advisory => 2,
        });
        assert_eq!(
            sevs,
            vec![Severity::Hard, Severity::Soft, Severity::Advisory]
        );
    }

    #[test]
    fn insert_edge_rejects_invalid_severity_via_check_constraint() {
        let s = setup();
        s.insert_node(&bare_node("a", "outline_node")).unwrap();
        s.insert_node(&bare_node("b", "outline_node")).unwrap();
        // bypass the typed API; raw insert with bad severity
        let r = s.conn.execute(
            "INSERT INTO edges (id, src_node, tgt_node, kind, severity, metadata, version, prev_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, '{}', 1, NULL)",
            params!["e_bad", "a", "b", "cites", "screaming"],
        );
        assert!(r.is_err(), "CHECK constraint should reject 'screaming'");
    }

    #[test]
    fn version_record_insert_and_count() {
        let s = setup();
        s.insert_node(&bare_node("n1", "persona")).unwrap();
        for v in 1..=3 {
            let rec = VersionRecord {
                target_kind: VersionTargetKind::Node,
                target_id: "n1".into(),
                version: v,
                diff: json!({"step": v}),
                ts: 1_700_000_000 + v as i64,
                author: Some("alpha".into()),
            };
            s.insert_version_record(&rec).unwrap();
        }
        assert_eq!(s.count_versions(VersionTargetKind::Node, "n1").unwrap(), 3);
        assert_eq!(s.count_versions(VersionTargetKind::Edge, "n1").unwrap(), 0);
    }

    #[test]
    fn version_pk_rejects_duplicate_kind_id_version() {
        let s = setup();
        let rec = VersionRecord {
            target_kind: VersionTargetKind::Node,
            target_id: "dup".into(),
            version: 1,
            diff: json!({}),
            ts: 1,
            author: None,
        };
        s.insert_version_record(&rec).unwrap();
        assert!(s.insert_version_record(&rec).is_err());
    }

    #[test]
    fn specification_upsert_roundtrip_and_overwrite() {
        let s = setup();
        s.upsert_specification("active_personas", r#"{"TypeIs":"persona"}"#)
            .unwrap();
        assert_eq!(
            s.get_specification("active_personas").unwrap().as_deref(),
            Some(r#"{"TypeIs":"persona"}"#)
        );

        // Overwrite under same name
        s.upsert_specification("active_personas", r#"{"TypeIs":"channel"}"#)
            .unwrap();
        assert_eq!(
            s.get_specification("active_personas").unwrap().as_deref(),
            Some(r#"{"TypeIs":"channel"}"#)
        );

        s.upsert_specification("workflow_defs", r#"{"TypeIs":"workflow_def"}"#)
            .unwrap();
        let all = s.list_specifications().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "active_personas");
        assert_eq!(all[1].0, "workflow_defs");
    }

    #[test]
    fn projection_upsert_roundtrip_and_form_check() {
        let s = setup();
        s.upsert_projection(
            "_persona_toc",
            "active_personas",
            "Personas: {{count}}",
            "prompt",
        )
        .unwrap();
        let got = s.get_projection("_persona_toc").unwrap().expect("exists");
        assert_eq!(got.0, "active_personas");
        assert_eq!(got.1, "Personas: {{count}}");
        assert_eq!(got.2, "prompt");

        assert!(s
            .list_projections()
            .unwrap()
            .contains(&"_persona_toc".into()));

        // Bad target_form is rejected
        assert!(s.upsert_projection("bad", "any", "tpl", "yaml").is_err());
    }

    #[test]
    fn workflow_runs_table_exists() {
        let s = setup();
        s.conn
            .execute(
                "INSERT INTO workflow_runs (id, def_node, state, started_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params!["r1", "wf_alpha", "ready", 100i64, 100i64],
            )
            .unwrap();
        let cnt: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM workflow_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn repository_trait_compute_traverse_one_hop() {
        use crate::domain::compute::traverse;
        use crate::domain::repository::Repository;
        use crate::domain::specification::Specification;

        let s = setup();
        // graph: alpha -[routes_to]-> beta, alpha -[routes_to]-> gamma
        for id in ["alpha", "beta", "gamma"] {
            s.insert_node(&bare_node(id, "persona")).unwrap();
        }
        for (id, src, tgt) in [("e1", "alpha", "beta"), ("e2", "alpha", "gamma")] {
            s.insert_edge(&Edge {
                id: id.into(),
                src_node: src.into(),
                tgt_node: tgt.into(),
                kind: "routes_to".into(),
                severity: None,
                metadata: json!({}),
                version: 1,
                prev_id: None,
            })
            .unwrap();
        }
        // include alpha (start) via TypeIs::persona match
        let spec = Specification::TypeIs("persona".into());
        let repo: &dyn Repository = &s;
        let result = traverse(&"alpha".into(), &spec, 1, repo).unwrap();
        assert_eq!(result.nodes.len(), 3);
        assert_eq!(result.depth_reached, 1);
        let ids: Vec<_> = result.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"alpha"));
        assert!(ids.contains(&"beta"));
        assert!(ids.contains(&"gamma"));
    }

    #[test]
    fn repository_trait_compute_traverse_depth_zero_only_start() {
        use crate::domain::compute::traverse;
        use crate::domain::repository::Repository;
        use crate::domain::specification::Specification;

        let s = setup();
        s.insert_node(&bare_node("alpha", "persona")).unwrap();
        s.insert_node(&bare_node("beta", "persona")).unwrap();
        s.insert_edge(&Edge {
            id: "e1".into(),
            src_node: "alpha".into(),
            tgt_node: "beta".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        let repo: &dyn Repository = &s;
        let result = traverse(
            &"alpha".into(),
            &Specification::TypeIs("persona".into()),
            0,
            repo,
        )
        .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id, "alpha");
    }
}
