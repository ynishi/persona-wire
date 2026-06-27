//! SQLite storage adapter.
//!
//! P1 scope: type_registry / nodes / edges / versions schema + CRUD primitive.
//! specifications / projections / workflow_runs tables are P2+ carry.

use std::path::PathBuf;

use crate::domain::autoversion::{VersionRecord, VersionTargetKind};
use crate::domain::error::{DomainError, WireError, WireResult};
use crate::domain::graph::{Edge, EdgeId, Node, NodeId, Severity, Ulid};
#[cfg(test)]
use crate::domain::graph::ulid_from_seed;
use rusqlite::{params, types::Type as SqlType, Connection, OptionalExtension, Row};

/// Decode a ULID stored as 26-char Crockford base32 TEXT. Used in `row_to_*`
/// helpers since `ulid::Ulid` does not implement `rusqlite::FromSql` directly.
fn text_to_ulid(s: &str) -> rusqlite::Result<Ulid> {
    Ulid::from_string(s)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, SqlType::Text, Box::new(e)))
}

fn opt_text_to_ulid(s: Option<String>) -> rusqlite::Result<Option<Ulid>> {
    s.map(|t| text_to_ulid(&t)).transpose()
}

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

/// Normalize node `metadata` at the storage boundary so callers cannot stash
/// a string-encoded JSON literal (or other non-object shapes) into the DB.
///
/// Accepted inputs:
/// - `Value::Object(_)` → pass-through unchanged.
/// - `Value::String(s)` → attempt `serde_json::from_str(&s)`. If the result is
///   `Value::Object(_)`, adopt it. Any parse failure or non-object result
///   returns `WireError::InvalidMetadata`.
///
/// All other variants (`Null` / `Bool` / `Number` / `Array`) are rejected with
/// `WireError::InvalidMetadata`. Node metadata semantics across the codebase
/// (handlebars rendering / `MetadataEq` spec evaluation / persona-pack overlay)
/// assume an object shape; this helper is the single enforcement point on the
/// write path. The read path (`row_to_node`) remains best-effort
/// (`from_str(...).unwrap_or(Value::Null)`) because legacy rows written before
/// this guard may still carry stringified payloads — those are healed by data
/// fix scripts on a case-by-case basis (see issue 22dcf208 axis (a)).
fn normalize_metadata_storage(metadata: &serde_json::Value) -> WireResult<serde_json::Value> {
    use serde_json::Value;
    match metadata {
        Value::Object(_) => Ok(metadata.clone()),
        Value::String(s) => {
            let parsed: Value = serde_json::from_str(s).map_err(|e| {
                WireError::Domain(DomainError::InvalidMetadata(format!(
                    "node metadata is a string but does not parse as JSON object: {}",
                    e
                )))
            })?;
            if matches!(parsed, Value::Object(_)) {
                Ok(parsed)
            } else {
                Err(WireError::Domain(DomainError::InvalidMetadata(format!(
                    "node metadata string parsed to non-object JSON: {}",
                    parsed
                ))))
            }
        }
        other => Err(WireError::Domain(DomainError::InvalidMetadata(format!(
            "node metadata must be a JSON object, got: {}",
            other
        )))),
    }
}

pub struct SqliteStorage {
    conn: Connection,
}

impl SqliteStorage {
    /// Borrow the underlying `rusqlite::Connection` for tests that need
    /// to verify SQL-level state (row counts, raw column reads) that the
    /// public API does not expose.
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }

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
        // P3a Phase 2 (a) — Idempotent ALTER for pre-existing DBs that were
        // created before the `template_engine` / `projection_kind` /
        // `projection_config` columns were introduced on `projections`.
        // SQLite has no `ADD COLUMN IF NOT EXISTS` — instead, probe via
        // `PRAGMA table_info(...)` and ADD only when missing.
        self.add_column_if_missing("projections", "template_engine", "TEXT")?;
        self.add_column_if_missing("projections", "projection_kind", "TEXT")?;
        self.add_column_if_missing("projections", "projection_config", "TEXT")?;
        Ok(())
    }

    /// Add a column iff `PRAGMA table_info(<table>)` does not list it. Idempotent.
    fn add_column_if_missing(&self, table: &str, column: &str, type_decl: &str) -> WireResult<()> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let names: Vec<String> = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        if names.iter().any(|n| n == column) {
            return Ok(());
        }
        self.conn
            .execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {type_decl}"),
                [],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Seed concept-doc §4.1/§4.2 type vocabulary (9 node + 9 edge).
    /// Idempotent via `INSERT OR IGNORE`.
    pub fn seed_default_types(&self) -> WireResult<()> {
        const SEED: &[(&str, &str, Option<&str>)] = &[
            ("outline_node", "node", None),
            ("actor_artifact", "node", None),
            ("pp_field", "node", None),
            ("ma_row", "node", None),
            ("pj_chapter", "node", None),
            ("persona", "node", None),
            ("channel", "node", None),
            ("workflow_def", "node", None),
            ("projection_def", "node", None),
            // P3b Layer 6 Adapter — endpoint vocabulary for `mcp://` Adapter.
            // Carries `metadata.endpoint` (ServerEndpoint JSON) +
            // `metadata.maintenance_exempt = true` so doctor's orphan_node
            // probe (`is_self_attached_wiring`) skips it cleanly.
            ("mcp_server", "node", None),
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
        let normalized = normalize_metadata_storage(&node.metadata)?;
        let metadata_str =
            serde_json::to_string(&normalized).map_err(|e| WireError::Storage(e.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO nodes (id, name, type, sot_ref, confidence, applicability, \
                 last_verified_at, review_due, version, prev_id, metadata) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    node.id.to_string(),
                    node.name,
                    node.r#type,
                    node.sot_ref,
                    node.confidence,
                    node.applicability,
                    node.last_verified_at,
                    node.review_due,
                    node.version,
                    node.prev_id.as_ref().map(|u| u.to_string()),
                    metadata_str,
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_node(&self, id: &NodeId) -> WireResult<Option<Node>> {
        self.conn
            .query_row(
                "SELECT id, name, type, sot_ref, confidence, applicability, last_verified_at, \
                 review_due, version, prev_id, metadata FROM nodes WHERE id = ?1",
                params![id.to_string()],
                row_to_node,
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// Fetch a node by its human-readable `name`. Returns `Ok(None)` for no
    /// match and `Err(WireError::AmbiguousName)` for multiple matches.
    /// Convenience wrapper over `lookup_node_id_by_name` + `get_node`.
    pub fn get_node_by_name(&self, name: &str) -> WireResult<Option<Node>> {
        match self.lookup_node_id_by_name(name)? {
            Some(id) => self.get_node(&id),
            None => Ok(None),
        }
    }

    /// Resolve a string that is either a 26-char ULID or a `name` to a
    /// concrete `NodeId`. Used by every MCP entry that exposes `id_or_name`
    /// at the boundary.
    pub fn resolve_node_id_or_name(&self, id_or_name: &str) -> WireResult<Option<NodeId>> {
        if let Ok(ulid) = Ulid::from_string(id_or_name) {
            return Ok(Some(ulid));
        }
        self.lookup_node_id_by_name(id_or_name)
    }

    /// Edge-side counterpart to `resolve_node_id_or_name`.
    pub fn resolve_edge_id_or_name(&self, id_or_name: &str) -> WireResult<Option<EdgeId>> {
        if let Ok(ulid) = Ulid::from_string(id_or_name) {
            return Ok(Some(ulid));
        }
        self.lookup_edge_id_by_name(id_or_name)
    }

    /// Resolve a `NodeId` by its human-readable `name`. Returns
    /// `Ok(Some(_))` when exactly one row matches, `Ok(None)` when zero,
    /// and `Err(WireError::AmbiguousName)` when more than one row shares
    /// the name (callers should fall back to ULID for disambiguation).
    pub fn lookup_node_id_by_name(&self, name: &str) -> WireResult<Option<NodeId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM nodes WHERE name = ?1")
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![name], |r| r.get::<_, String>(0))
            .map_err(|e| WireError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        match rows.len() {
            0 => Ok(None),
            1 => {
                let id = Ulid::from_string(&rows[0])
                    .map_err(|e| WireError::Storage(e.to_string()))?;
                Ok(Some(id))
            }
            n => Err(WireError::AmbiguousName {
                name: name.to_string(),
                count: n,
            }),
        }
    }

    /// Replace a node's `metadata` JSON object in place.
    ///
    /// Returns `Ok(true)` when an existing row was updated, `Ok(false)` when
    /// no row matched `id` (caller decides whether that is an error). The
    /// `metadata` argument is stored verbatim — callers performing a partial
    /// patch should compose the merged object beforehand (see
    /// [`merge_metadata_shallow`] / [`merge_metadata_deep`]).
    ///
    /// P3a Phase 2 (d) — primitive backing `wire_node_update`. Other node
    /// fields (`type` / `sot_ref` / lifecycle timestamps) intentionally stay
    /// immutable on this path; full-row replacement is out of scope for the
    /// metadata-patch UC (= wiring-entry `source_uri` tuning).
    pub fn update_node_metadata(&self, id: &NodeId, metadata: &serde_json::Value) -> WireResult<bool> {
        let normalized = normalize_metadata_storage(metadata)?;
        let metadata_str =
            serde_json::to_string(&normalized).map_err(|e| WireError::Storage(e.to_string()))?;
        let n = self
            .conn
            .execute(
                "UPDATE nodes SET metadata = ?1 WHERE id = ?2",
                params![metadata_str, id.to_string()],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    pub fn list_nodes_by_type(&self, type_name: &str) -> WireResult<Vec<Node>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, type, sot_ref, confidence, applicability, last_verified_at, \
                 review_due, version, prev_id, metadata FROM nodes WHERE type = ?1 ORDER BY name, id",
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
                "INSERT INTO edges (id, name, src_node, tgt_node, kind, severity, metadata, version, prev_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    edge.id.to_string(),
                    edge.name,
                    edge.src_node.to_string(),
                    edge.tgt_node.to_string(),
                    edge.kind,
                    sev,
                    metadata_str,
                    edge.version,
                    edge.prev_id.as_ref().map(|u| u.to_string()),
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_edge(&self, id: &EdgeId) -> WireResult<Option<Edge>> {
        self.conn
            .query_row(
                "SELECT id, name, src_node, tgt_node, kind, severity, metadata, version, prev_id \
                 FROM edges WHERE id = ?1",
                params![id.to_string()],
                row_to_edge,
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// Lookup an `EdgeId` by its optional human-readable `name`. Same
    /// semantics as `lookup_node_id_by_name`: 0/1/many → None/Some/Err.
    pub fn lookup_edge_id_by_name(&self, name: &str) -> WireResult<Option<EdgeId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM edges WHERE name = ?1")
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows: Vec<String> = stmt
            .query_map(params![name], |r| r.get::<_, String>(0))
            .map_err(|e| WireError::Storage(e.to_string()))?
            .collect::<Result<_, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        match rows.len() {
            0 => Ok(None),
            1 => {
                let id = Ulid::from_string(&rows[0])
                    .map_err(|e| WireError::Storage(e.to_string()))?;
                Ok(Some(id))
            }
            n => Err(WireError::AmbiguousName {
                name: name.to_string(),
                count: n,
            }),
        }
    }

    pub fn list_edges_from(&self, src_node: &NodeId) -> WireResult<Vec<Edge>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, src_node, tgt_node, kind, severity, metadata, version, prev_id \
                 FROM edges WHERE src_node = ?1 ORDER BY name, id",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![src_node.to_string()], row_to_edge)
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    pub fn list_edges_to(&self, tgt_node: &NodeId) -> WireResult<Vec<Edge>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, src_node, tgt_node, kind, severity, metadata, version, prev_id \
                 FROM edges WHERE tgt_node = ?1 ORDER BY name, id",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![tgt_node.to_string()], row_to_edge)
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

    /// Upsert a Specification by name. Returns the row's ULID `id` (newly
    /// minted on insert; preserved on update of an existing name).
    pub fn upsert_specification(
        &self,
        name: &str,
        expr_json: &str,
    ) -> WireResult<crate::domain::entity::projection::SpecificationId> {
        let existing = self.lookup_specification_id_by_name(name)?;
        let id = existing.unwrap_or_else(Ulid::new);
        self.conn
            .execute(
                "INSERT INTO specifications (id, name, expr_json, created_at) \
                 VALUES (?1, ?2, ?3, 0) \
                 ON CONFLICT(name) DO UPDATE SET expr_json = excluded.expr_json",
                params![id.to_string(), name, expr_json],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(id)
    }

    /// Read the `expr_json` body of a Specification by its human-readable
    /// `name`. Kept for caller compatibility; new code may prefer
    /// `get_specification_by_id`.
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

    /// Lookup a Specification's `id` from its `name`. Returns `Ok(None)`
    /// for no match; `name` is UNIQUE so multi-row resolution cannot occur.
    pub fn lookup_specification_id_by_name(
        &self,
        name: &str,
    ) -> WireResult<Option<crate::domain::entity::projection::SpecificationId>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM specifications WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        row.map(|s| Ulid::from_string(&s).map_err(|e| WireError::Storage(e.to_string())))
            .transpose()
    }

    /// Resolve a string that is either a 26-char ULID or a `name` to a
    /// concrete `SpecificationId`. Mirrors `resolve_node_id_or_name` so MCP
    /// `wire_spec_*` callers can pass whichever they have.
    pub fn resolve_specification_id_or_name(
        &self,
        id_or_name: &str,
    ) -> WireResult<Option<crate::domain::entity::projection::SpecificationId>> {
        if let Ok(ulid) = Ulid::from_string(id_or_name) {
            return Ok(Some(ulid));
        }
        self.lookup_specification_id_by_name(id_or_name)
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

    // ---- Bundles ----

    /// Upsert a Bundle by name. Returns the row's ULID `id` (newly minted
    /// on insert; preserved on update of an existing name). `created_at`
    /// is set only on insert; `updated_at` always to `now_secs`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_bundle(
        &self,
        name: &str,
        version: &str,
        description: Option<&str>,
        body: &str,
        now_secs: i64,
    ) -> WireResult<crate::domain::entity::bundle::BundleId> {
        let existing = self.lookup_bundle_id_by_name(name)?;
        let id = existing.unwrap_or_else(Ulid::new);
        self.conn
            .execute(
                "INSERT INTO bundles (id, name, version, description, body, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
                 ON CONFLICT(name) DO UPDATE SET \
                    version = excluded.version, \
                    description = excluded.description, \
                    body = excluded.body, \
                    updated_at = excluded.updated_at",
                params![id.to_string(), name, version, description, body, now_secs],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(id)
    }

    /// Lookup a Bundle's `id` from its `name`. Returns `Ok(None)` for no
    /// match; `name` is UNIQUE so multi-row resolution cannot occur.
    pub fn lookup_bundle_id_by_name(
        &self,
        name: &str,
    ) -> WireResult<Option<crate::domain::entity::bundle::BundleId>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM bundles WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        row.map(|s| Ulid::from_string(&s).map_err(|e| WireError::Storage(e.to_string())))
            .transpose()
    }

    /// Resolve a string that is either a 26-char ULID or a `name` to a
    /// concrete `BundleId`.
    pub fn resolve_bundle_id_or_name(
        &self,
        id_or_name: &str,
    ) -> WireResult<Option<crate::domain::entity::bundle::BundleId>> {
        if let Ok(ulid) = Ulid::from_string(id_or_name) {
            // Validate the id actually exists; otherwise treat as not-found.
            let exists: Option<String> = self
                .conn
                .query_row(
                    "SELECT id FROM bundles WHERE id = ?1",
                    params![ulid.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| WireError::Storage(e.to_string()))?;
            return Ok(exists.map(|_| ulid));
        }
        self.lookup_bundle_id_by_name(id_or_name)
    }

    /// Read a full Bundle row by `name`. Returns `Ok(None)` for no match.
    pub fn get_bundle_by_name(
        &self,
        name: &str,
    ) -> WireResult<Option<crate::domain::entity::bundle::Bundle>> {
        self.conn
            .query_row(
                "SELECT id, name, version, description, body, created_at, updated_at \
                 FROM bundles WHERE name = ?1",
                params![name],
                row_to_bundle,
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// Read a full Bundle row by `id`. Returns `Ok(None)` for no match.
    pub fn get_bundle_by_id(
        &self,
        id: crate::domain::entity::bundle::BundleId,
    ) -> WireResult<Option<crate::domain::entity::bundle::Bundle>> {
        self.conn
            .query_row(
                "SELECT id, name, version, description, body, created_at, updated_at \
                 FROM bundles WHERE id = ?1",
                params![id.to_string()],
                row_to_bundle,
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// List bundles in name-ascending order. Returns lightweight summary
    /// rows (id / name / version / description) — the full TOML body is
    /// fetched only via `get_bundle_by_*` to keep list payloads bounded.
    pub fn list_bundles(
        &self,
    ) -> WireResult<Vec<crate::domain::entity::bundle::Bundle>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, version, description, body, created_at, updated_at \
                 FROM bundles ORDER BY name",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], row_to_bundle)
            .map_err(|e| WireError::Storage(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// Delete a Bundle by `name`. Returns `true` if a row was removed,
    /// `false` if no row matched. Cascades on `bundle_installs` are not
    /// declared at the schema level — install history outlives the bundle
    /// row so a future History UI can still surface "this bundle was once
    /// installed" entries after deletion.
    pub fn delete_bundle_by_name(&self, name: &str) -> WireResult<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM bundles WHERE name = ?1", params![name])
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Delete a Bundle by `id`. Returns `true` if a row was removed.
    pub fn delete_bundle_by_id(
        &self,
        id: crate::domain::entity::bundle::BundleId,
    ) -> WireResult<bool> {
        let affected = self
            .conn
            .execute("DELETE FROM bundles WHERE id = ?1", params![id.to_string()])
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Append one install log entry to `bundle_installs`. Called from the
    /// install use case after dispatch completes (success or partial).
    pub fn append_bundle_install(
        &self,
        install_id: crate::domain::entity::bundle::BundleId,
        bundle_id: crate::domain::entity::bundle::BundleId,
        mode: &str,
        installed_at: i64,
        report_json: &str,
    ) -> WireResult<()> {
        self.conn
            .execute(
                "INSERT INTO bundle_installs (install_id, bundle_id, mode, installed_at, report) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    install_id.to_string(),
                    bundle_id.to_string(),
                    mode,
                    installed_at,
                    report_json,
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }

    // ---- Projections ----

    /// Upsert a NamedProjection row. `template_engine` / `projection_kind` /
    /// `projection_config` are stored as NULL when `None`, signalling that the
    /// use-case layer should fall back to `PluginRegistry` defaults at
    /// dispatch time.
    /// Upsert a NamedProjection by name. Returns the row's ULID `id`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_projection(
        &self,
        name: &str,
        spec_ref: &str,
        template: &str,
        target_form: &str,
        template_engine: Option<&str>,
        projection_kind: Option<&str>,
        projection_config: Option<&str>,
    ) -> WireResult<crate::domain::entity::projection::ProjectionId> {
        let existing = self.lookup_projection_id_by_name(name)?;
        let id = existing.unwrap_or_else(Ulid::new);
        self.conn
            .execute(
                "INSERT INTO projections (id, name, spec_ref, template, target_form, created_at, template_engine, projection_kind, projection_config) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8) \
                 ON CONFLICT(name) DO UPDATE SET \
                    spec_ref = excluded.spec_ref, \
                    template = excluded.template, \
                    target_form = excluded.target_form, \
                    template_engine = excluded.template_engine, \
                    projection_kind = excluded.projection_kind, \
                    projection_config = excluded.projection_config",
                params![
                    id.to_string(),
                    name,
                    spec_ref,
                    template,
                    target_form,
                    template_engine,
                    projection_kind,
                    projection_config
                ],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(id)
    }

    /// Lookup a Projection's `id` from its `name`. UNIQUE on `name` so 0/1.
    pub fn lookup_projection_id_by_name(
        &self,
        name: &str,
    ) -> WireResult<Option<crate::domain::entity::projection::ProjectionId>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM projections WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        row.map(|s| Ulid::from_string(&s).map_err(|e| WireError::Storage(e.to_string())))
            .transpose()
    }

    /// Resolve a string that is either a 26-char ULID or a `name` to a
    /// concrete `ProjectionId`.
    pub fn resolve_projection_id_or_name(
        &self,
        id_or_name: &str,
    ) -> WireResult<Option<crate::domain::entity::projection::ProjectionId>> {
        if let Ok(ulid) = Ulid::from_string(id_or_name) {
            return Ok(Some(ulid));
        }
        self.lookup_projection_id_by_name(id_or_name)
    }

    /// Reverse lookup: ULID → `name`. Used by `wire_render` etc to feed a
    /// resolved id back into the legacy name-keyed API (`ProjectionRegistry::get`).
    pub fn get_projection_name_by_id(
        &self,
        id: &crate::domain::entity::projection::ProjectionId,
    ) -> WireResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT name FROM projections WHERE id = ?1",
                params![id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// Reverse lookup: ULID → `name` (Specification).
    pub fn get_specification_name_by_id(
        &self,
        id: &crate::domain::entity::projection::SpecificationId,
    ) -> WireResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT name FROM specifications WHERE id = ?1",
                params![id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WireError::Storage(e.to_string()))
    }

    /// Row tuple returned by `get_projection`:
    /// `(spec_ref, template, target_form, template_engine?, projection_kind?, projection_config?)`.
    /// The last three are `None` for rows persisted before P3a Phase 2 (a).
    #[allow(clippy::type_complexity)]
    pub fn get_projection(
        &self,
        name: &str,
    ) -> WireResult<
        Option<(
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    > {
        self.conn
            .query_row(
                "SELECT spec_ref, template, target_form, template_engine, projection_kind, projection_config FROM projections WHERE name = ?1",
                params![name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
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

    // ---- delete surface (P2c-bis、 メンテ運用必須) ----

    /// Delete a node by id. Returns `true` if a row was deleted, `false` if
    /// no row matched. Edges referencing this node (as src or tgt) are
    /// **cascade-deleted** in the same transaction — schema has NOT-NULL FK
    /// from edges → nodes, so orphan edges are not representable; cascade is
    /// the only consistent option.
    pub fn delete_node(&self, id: &NodeId) -> WireResult<bool> {
        let id_str = id.to_string();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| WireError::Storage(e.to_string()))?;
        tx.execute(
            "DELETE FROM edges WHERE src_node = ?1 OR tgt_node = ?1",
            rusqlite::params![id_str],
        )
        .map_err(|e| WireError::Storage(e.to_string()))?;
        let n = tx
            .execute("DELETE FROM nodes WHERE id = ?1", rusqlite::params![id_str])
            .map_err(|e| WireError::Storage(e.to_string()))?;
        tx.commit().map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    /// Delete an edge by id. Returns `true` if a row was deleted.
    pub fn delete_edge(&self, id: &EdgeId) -> WireResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM edges WHERE id = ?1", rusqlite::params![id.to_string()])
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    /// Delete a Specification by ULID id. Returns `true` if a row was deleted.
    /// Projections referencing this spec via `spec_ref` will start returning
    /// dangling-spec errors at render time (existing wire_render contract).
    pub fn delete_specification(
        &self,
        id: &crate::domain::entity::projection::SpecificationId,
    ) -> WireResult<bool> {
        let n = self
            .conn
            .execute(
                "DELETE FROM specifications WHERE id = ?1",
                rusqlite::params![id.to_string()],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(n > 0)
    }

    /// Delete a NamedProjection by ULID id. Returns `true` if a row was deleted.
    pub fn delete_projection(
        &self,
        id: &crate::domain::entity::projection::ProjectionId,
    ) -> WireResult<bool> {
        let n = self
            .conn
            .execute(
                "DELETE FROM projections WHERE id = ?1",
                rusqlite::params![id.to_string()],
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(n > 0)
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
    // Column order: id, name, type, sot_ref, confidence, applicability,
    //               last_verified_at, review_due, version, prev_id, metadata
    let id_str: String = row.get(0)?;
    let prev_str: Option<String> = row.get(9)?;
    let metadata_str: String = row.get(10)?;
    let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
    Ok(Node {
        id: text_to_ulid(&id_str)?,
        name: row.get(1)?,
        r#type: row.get(2)?,
        sot_ref: row.get(3)?,
        confidence: row.get(4)?,
        applicability: row.get(5)?,
        last_verified_at: row.get(6)?,
        review_due: row.get(7)?,
        version: row.get::<_, i64>(8)? as u32,
        prev_id: opt_text_to_ulid(prev_str)?,
        metadata,
    })
}

fn row_to_edge(row: &Row) -> rusqlite::Result<Edge> {
    // Column order: id, name, src_node, tgt_node, kind, severity, metadata,
    //               version, prev_id
    let id_str: String = row.get(0)?;
    let src_str: String = row.get(2)?;
    let tgt_str: String = row.get(3)?;
    let sev_str: Option<String> = row.get(5)?;
    let severity = sev_str.and_then(|s| match s.as_str() {
        "hard" => Some(Severity::Hard),
        "soft" => Some(Severity::Soft),
        "advisory" => Some(Severity::Advisory),
        _ => None,
    });
    let metadata_str: String = row.get(6)?;
    let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);
    let prev_str: Option<String> = row.get(8)?;
    Ok(Edge {
        id: text_to_ulid(&id_str)?,
        name: row.get(1)?,
        src_node: text_to_ulid(&src_str)?,
        tgt_node: text_to_ulid(&tgt_str)?,
        kind: row.get(4)?,
        severity,
        metadata,
        version: row.get::<_, i64>(7)? as u32,
        prev_id: opt_text_to_ulid(prev_str)?,
    })
}

fn row_to_bundle(row: &Row<'_>) -> rusqlite::Result<crate::domain::entity::bundle::Bundle> {
    use crate::domain::entity::bundle::{Bundle, BundleName, BundleVersion};
    let id_str: String = row.get(0)?;
    let name_str: String = row.get(1)?;
    let version_str: String = row.get(2)?;
    let description: Option<String> = row.get(3)?;
    let body: String = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    let updated_at: i64 = row.get(6)?;
    let id = text_to_ulid(&id_str)?;
    // Domain VOs were already validated on insert, so non-empty is an
    // invariant. Re-validate defensively at the read boundary so a corrupt
    // row surfaces as a typed error rather than a downstream panic.
    let name = BundleName::new(name_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, SqlType::Text, Box::new(std::io::Error::other(e.to_string())))
    })?;
    let version = BundleVersion::new(version_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, SqlType::Text, Box::new(std::io::Error::other(e.to_string())))
    })?;
    Bundle::new(id, name, version, description, body, created_at, updated_at).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, SqlType::Text, Box::new(std::io::Error::other(e.to_string())))
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
    name              TEXT NOT NULL DEFAULT '',
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
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);

CREATE TABLE IF NOT EXISTS edges (
    id        TEXT PRIMARY KEY,
    name      TEXT,
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
CREATE INDEX IF NOT EXISTS idx_edges_name ON edges(name);

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
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    expr_json   TEXT NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_specifications_name ON specifications(name);

CREATE TABLE IF NOT EXISTS projections (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE,
    spec_ref          TEXT NOT NULL,
    template          TEXT NOT NULL,
    target_form       TEXT NOT NULL CHECK (target_form IN ('prompt', 'markdown', 'json', 'ascii')),
    created_at        INTEGER NOT NULL DEFAULT 0,
    -- P3a Phase 2 (a) — Plugin dispatch hints. NULL → server defaults ("handlebars" / "static" / null).
    template_engine   TEXT,
    projection_kind   TEXT,
    projection_config TEXT
);

CREATE INDEX IF NOT EXISTS idx_projections_name ON projections(name);

CREATE TABLE IF NOT EXISTS workflow_runs (
    id          TEXT PRIMARY KEY,
    def_node    TEXT NOT NULL,
    state       TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    metadata    TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS bundles (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    version     TEXT NOT NULL,
    description TEXT,
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_bundles_name ON bundles(name);

CREATE TABLE IF NOT EXISTS bundle_installs (
    install_id   TEXT PRIMARY KEY,
    bundle_id    TEXT NOT NULL REFERENCES bundles(id),
    mode         TEXT NOT NULL CHECK (mode IN ('increment', 'skip', 'error')),
    installed_at INTEGER NOT NULL DEFAULT 0,
    report       TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_bundle_installs_bundle ON bundle_installs(bundle_id);
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
            id: ulid_from_seed(id),
            name: id.into(),
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
                "bundle_installs",
                "bundles",
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
    fn seed_inserts_10_node_and_9_edge_types() {
        let s = setup();
        let nodes = s.list_types_by_kind("node").unwrap();
        let edges = s.list_types_by_kind("edge").unwrap();
        assert_eq!(nodes.len(), 10);
        assert_eq!(edges.len(), 9);
        assert!(nodes.contains(&"persona".to_string()));
        assert!(nodes.contains(&"mcp_server".to_string()));
        assert!(edges.contains(&"triggers_review_of".to_string()));
    }

    #[test]
    fn seed_is_idempotent() {
        let s = setup();
        s.seed_default_types().unwrap();
        s.seed_default_types().unwrap();
        assert_eq!(s.list_types().unwrap().len(), 19);
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

        let got = s.get_node(&ulid_from_seed("n1")).unwrap().expect("exists");
        assert_eq!(got.name, "n1");
        assert_eq!(got.r#type, "persona");
        assert_eq!(got.sot_ref.as_deref(), Some("pp://alpha"));
        assert_eq!(got.confidence, Some(0.95));
        assert_eq!(got.last_verified_at, Some(1_700_000_000));
        assert_eq!(got.metadata, json!({"name": "alpha", "tags": ["dev"]}));
    }

    #[test]
    fn get_node_returns_none_when_absent() {
        let s = setup();
        assert!(s.get_node(&ulid_from_seed("missing")).unwrap().is_none());
    }

    #[test]
    fn insert_node_rejects_unknown_type() {
        let s = setup();
        let n = bare_node("nx", "definitely_not_a_registered_type");
        let err = s.insert_node(&n);
        assert!(err.is_err(), "FK on nodes.type should reject unknown type");
    }

    // ----- issue 22dcf208: metadata shape normalization at storage boundary -----

    #[test]
    fn insert_node_normalizes_stringified_object_metadata() {
        let s = setup();
        let mut n = bare_node("shi_like", "persona");
        n.metadata =
            serde_json::Value::String(r#"{"display":"shi_like","first_person":"しー"}"#.into());
        s.insert_node(&n).unwrap();
        let got = s.get_node(&ulid_from_seed("shi_like")).unwrap().expect("exists");
        assert_eq!(
            got.metadata,
            json!({"display": "shi_like", "first_person": "しー"}),
            "string-encoded metadata should be parsed back into an object"
        );
    }

    #[test]
    fn insert_node_rejects_unparseable_string_metadata() {
        let s = setup();
        let mut n = bare_node("bad1", "persona");
        n.metadata = serde_json::Value::String("not json at all".into());
        let err = s.insert_node(&n);
        assert!(matches!(
            err,
            Err(WireError::Domain(DomainError::InvalidMetadata(_)))
        ));
    }

    #[test]
    fn insert_node_rejects_string_metadata_parsing_to_non_object() {
        let s = setup();
        let mut n = bare_node("bad2", "persona");
        // Valid JSON, but parses to an array — non-object shapes must be rejected.
        n.metadata = serde_json::Value::String(r#"[1, 2, 3]"#.into());
        let err = s.insert_node(&n);
        assert!(matches!(
            err,
            Err(WireError::Domain(DomainError::InvalidMetadata(_)))
        ));
    }

    #[test]
    fn insert_node_rejects_array_metadata() {
        let s = setup();
        let mut n = bare_node("bad3", "persona");
        n.metadata = json!([1, 2, 3]);
        let err = s.insert_node(&n);
        assert!(matches!(
            err,
            Err(WireError::Domain(DomainError::InvalidMetadata(_)))
        ));
    }

    #[test]
    fn insert_node_rejects_scalar_metadata() {
        let s = setup();
        let mut n = bare_node("bad4", "persona");
        n.metadata = json!(42);
        let err = s.insert_node(&n);
        assert!(matches!(
            err,
            Err(WireError::Domain(DomainError::InvalidMetadata(_)))
        ));
    }

    #[test]
    fn update_node_metadata_normalizes_stringified_object() {
        let s = setup();
        s.insert_node(&bare_node("p1", "persona")).unwrap();
        let patched = serde_json::Value::String(r#"{"display":"p1"}"#.into());
        let updated = s.update_node_metadata(&ulid_from_seed("p1"), &patched).unwrap();
        assert!(updated);
        let got = s.get_node(&ulid_from_seed("p1")).unwrap().expect("exists");
        assert_eq!(got.metadata, json!({"display": "p1"}));
    }

    #[test]
    fn update_node_metadata_rejects_non_object_input() {
        let s = setup();
        s.insert_node(&bare_node("p2", "persona")).unwrap();
        let err = s.update_node_metadata(&ulid_from_seed("p2"), &json!("plain string"));
        assert!(matches!(
            err,
            Err(WireError::Domain(DomainError::InvalidMetadata(_)))
        ));
    }

    // Mirrors the batch path: `wire_nodes_create_batch` iterates `insert_node`
    // 1 row at a time, so verifying the storage boundary covers both surfaces.
    #[test]
    fn insert_node_batch_path_normalizes_each_row() {
        let s = setup();
        let mut n1 = bare_node("b1", "persona");
        n1.metadata = serde_json::Value::String(r#"{"display":"b1"}"#.into());
        let mut n2 = bare_node("b2", "persona");
        n2.metadata = json!({"display": "b2"});
        for n in [&n1, &n2] {
            s.insert_node(n).unwrap();
        }
        let got1 = s.get_node(&ulid_from_seed("b1")).unwrap().expect("exists");
        let got2 = s.get_node(&ulid_from_seed("b2")).unwrap().expect("exists");
        assert_eq!(got1.metadata, json!({"display": "b1"}));
        assert_eq!(got2.metadata, json!({"display": "b2"}));
    }

    #[test]
    fn list_nodes_by_type_filters() {
        let s = setup();
        s.insert_node(&bare_node("p1", "persona")).unwrap();
        s.insert_node(&bare_node("p2", "persona")).unwrap();
        s.insert_node(&bare_node("c1", "channel")).unwrap();
        let personas = s.list_nodes_by_type("persona").unwrap();
        assert_eq!(personas.len(), 2);
        assert_eq!(personas[0].name, "p1");
    }

    #[test]
    fn insert_and_list_edges_from() {
        let s = setup();
        s.insert_node(&bare_node("p_alpha", "persona")).unwrap();
        s.insert_node(&bare_node("p_beta", "persona")).unwrap();
        let e = Edge {
            id: ulid_from_seed("e1"),
            name: Some("e1".into()),
            src_node: ulid_from_seed("p_alpha"),
            tgt_node: ulid_from_seed("p_beta"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({"weight": 1}),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&e).unwrap();

        let edges = s.list_edges_from(&ulid_from_seed("p_alpha")).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].name.as_deref(), Some("e1"));
        assert_eq!(edges[0].kind, "routes_to");
        assert_eq!(edges[0].metadata, json!({"weight": 1}));

        // reverse direction empty
        let back = s.list_edges_from(&ulid_from_seed("p_beta")).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn list_edges_to_finds_incoming() {
        let s = setup();
        s.insert_node(&bare_node("a", "persona")).unwrap();
        s.insert_node(&bare_node("b", "persona")).unwrap();
        let e = Edge {
            id: ulid_from_seed("e_ab"),
            name: Some("e_ab".into()),
            src_node: ulid_from_seed("a"),
            tgt_node: ulid_from_seed("b"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&e).unwrap();
        let in_b = s.list_edges_to(&ulid_from_seed("b")).unwrap();
        assert_eq!(in_b.len(), 1);
        assert_eq!(in_b[0].src_node, ulid_from_seed("a"));
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
                id: ulid_from_seed(id),
                name: Some(id.into()),
                src_node: ulid_from_seed("a"),
                tgt_node: ulid_from_seed("b"),
                kind: "triggers_review_of".into(),
                severity: Some(sev),
                metadata: json!({}),
                version: 1,
                prev_id: None,
            };
            s.insert_edge(&e).unwrap();
        }
        let edges = s.list_edges_from(&ulid_from_seed("a")).unwrap();
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
                target_id: ulid_from_seed("n1").to_string(),
                version: v,
                diff: json!({"step": v}),
                ts: 1_700_000_000 + v as i64,
                author: Some("alpha".into()),
            };
            s.insert_version_record(&rec).unwrap();
        }
        assert_eq!(s.count_versions(VersionTargetKind::Node, &ulid_from_seed("n1").to_string()).unwrap(), 3);
        assert_eq!(s.count_versions(VersionTargetKind::Edge, &ulid_from_seed("n1").to_string()).unwrap(), 0);
    }

    #[test]
    fn version_pk_rejects_duplicate_kind_id_version() {
        let s = setup();
        let rec = VersionRecord {
            target_kind: VersionTargetKind::Node,
            target_id: ulid_from_seed("dup").to_string(),
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
            None,
            None,
            None,
        )
        .unwrap();
        let got = s.get_projection("_persona_toc").unwrap().expect("exists");
        assert_eq!(got.0, "active_personas");
        assert_eq!(got.1, "Personas: {{count}}");
        assert_eq!(got.2, "prompt");
        assert!(got.3.is_none());
        assert!(got.4.is_none());
        assert!(got.5.is_none());

        assert!(s
            .list_projections()
            .unwrap()
            .contains(&"_persona_toc".into()));

        // Bad target_form is rejected
        assert!(s
            .upsert_projection("bad", "any", "tpl", "yaml", None, None, None)
            .is_err());
    }

    #[test]
    fn projection_upsert_roundtrips_plugin_hint_fields() {
        // P3a Phase 2 (a) — when `template_engine` / `projection_kind` /
        // `projection_config` are persisted, they round-trip through SQLite
        // unchanged. NULL ↔ None already covered by the test above.
        let s = setup();
        s.upsert_projection(
            "with_hints",
            "active_personas",
            "{{count}}",
            "prompt",
            Some("jinja"),
            Some("llm"),
            Some(r#"{"endpoint":"http://localhost:8080"}"#),
        )
        .unwrap();
        let got = s.get_projection("with_hints").unwrap().expect("exists");
        assert_eq!(got.3.as_deref(), Some("jinja"));
        assert_eq!(got.4.as_deref(), Some("llm"));
        assert_eq!(
            got.5.as_deref(),
            Some(r#"{"endpoint":"http://localhost:8080"}"#)
        );
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
                id: ulid_from_seed(id),
                name: Some(id.into()),
                src_node: ulid_from_seed(src),
                tgt_node: ulid_from_seed(tgt),
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
        let result = traverse(&ulid_from_seed("alpha"), &spec, 1, repo).unwrap();
        assert_eq!(result.nodes.len(), 3);
        assert_eq!(result.depth_reached, 1);
        let names: Vec<_> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"gamma"));
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
            id: ulid_from_seed("e1"),
            name: Some("e1".into()),
            src_node: ulid_from_seed("alpha"),
            tgt_node: ulid_from_seed("beta"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        let repo: &dyn Repository = &s;
        let result = traverse(
            &ulid_from_seed("alpha"),
            &Specification::TypeIs("persona".into()),
            0,
            repo,
        )
        .unwrap();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "alpha");
    }
}
