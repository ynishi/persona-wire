//! 002 — `specifications` / `projections` rebuild: `name TEXT PK` →
//! `id TEXT PK + name TEXT NOT NULL UNIQUE`.
//!
//! SQLite cannot rename a PK in place, so this follows the canonical
//! 12-step ALTER recipe (CREATE new + INSERT SELECT + DROP old + RENAME).
//! Idempotent: if both tables already have an `id` column, body is a no-op.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use super::{Migration, MigrationUlid as Ulid};

/// Registry entry for migration 002 — referenced from
/// [`super::ALL`] in execution order.
pub static MIGRATION: Mig = Mig;

/// Zero-sized [`Migration`] impl for the `002_registry_id_ulid`
/// schema change. Instances carry no state; the [`MIGRATION`]
/// singleton is the intended handle.
pub struct Mig;

impl Migration for Mig {
    fn id(&self) -> &'static str {
        "002_registry_id_ulid"
    }
    fn description(&self) -> &'static str {
        "specifications/projections: name PK → id PK + name UNIQUE"
    }
    fn up(&self, conn: &Connection) -> Result<()> {
        let specs_done = column_exists(conn, "specifications", "id")?;
        let projs_done = column_exists(conn, "projections", "id")?;
        if specs_done && projs_done {
            return Ok(());
        }

        if !specs_done {
            let map = build_name_map(conn, "specifications")?;
            conn.execute(
                "CREATE TABLE specifications_new (
                    id          TEXT PRIMARY KEY,
                    name        TEXT NOT NULL UNIQUE,
                    expr_json   TEXT NOT NULL,
                    created_at  INTEGER NOT NULL DEFAULT 0
                )",
                [],
            )?;
            {
                let mut stmt = conn.prepare(
                    "INSERT INTO specifications_new(id, name, expr_json, created_at) \
                     SELECT ?1, name, expr_json, created_at FROM specifications WHERE name = ?2",
                )?;
                for (name, id) in &map {
                    stmt.execute(params![id.to_string(), name])?;
                }
            }
            conn.execute("DROP TABLE specifications", [])
                .context("DROP old specifications")?;
            conn.execute(
                "ALTER TABLE specifications_new RENAME TO specifications",
                [],
            )?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_specifications_name ON specifications(name)",
                [],
            )?;
        }

        if !projs_done {
            let map = build_name_map(conn, "projections")?;
            conn.execute(
                "CREATE TABLE projections_new (
                    id                TEXT PRIMARY KEY,
                    name              TEXT NOT NULL UNIQUE,
                    spec_ref          TEXT NOT NULL,
                    template          TEXT NOT NULL,
                    target_form       TEXT NOT NULL CHECK (target_form IN ('prompt', 'markdown', 'json', 'ascii')),
                    created_at        INTEGER NOT NULL DEFAULT 0,
                    template_engine   TEXT,
                    projection_kind   TEXT,
                    projection_config TEXT
                )",
                [],
            )?;
            {
                let mut stmt = conn.prepare(
                    "INSERT INTO projections_new(id, name, spec_ref, template, target_form, created_at, \
                     template_engine, projection_kind, projection_config) \
                     SELECT ?1, name, spec_ref, template, target_form, created_at, \
                       template_engine, projection_kind, projection_config \
                     FROM projections WHERE name = ?2",
                )?;
                for (name, id) in &map {
                    stmt.execute(params![id.to_string(), name])?;
                }
            }
            conn.execute("DROP TABLE projections", [])?;
            conn.execute("ALTER TABLE projections_new RENAME TO projections", [])?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_projections_name ON projections(name)",
                [],
            )?;
        }

        // Sanity: every id is 26-char ULID.
        let bad_s: i64 = conn.query_row(
            "SELECT COUNT(*) FROM specifications WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        let bad_p: i64 = conn.query_row(
            "SELECT COUNT(*) FROM projections WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        if bad_s != 0 || bad_p != 0 {
            anyhow::bail!(
                "002_registry_id_ulid sanity failed: bad_specs={bad_s} bad_projs={bad_p}"
            );
        }
        Ok(())
    }
}

fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    // Tolerate missing tables (= already migrated state or fresh shape).
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(true);
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for n in rows {
        if n? == col {
            return Ok(true);
        }
    }
    Ok(false)
}

fn build_name_map(conn: &Connection, table: &str) -> Result<HashMap<String, Ulid>> {
    let mut stmt = conn.prepare(&format!("SELECT name FROM {table}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut map = HashMap::new();
    for r in rows {
        map.insert(r?, Ulid::new());
    }
    Ok(map)
}
