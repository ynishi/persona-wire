//! 001 — `nodes` / `edges` stringly `id` → opaque ULID + `name` extraction.
//!
//! Idempotent at the column level: if both `nodes.name` and `edges.name`
//! already exist, `up()` is a no-op (the framework still records the
//! migration as applied so [`pw-migrate status`] reflects the current
//! shape correctly even on a DB that was migrated before the framework
//! existed).

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use super::{Migration, MigrationUlid as Ulid};

pub static MIGRATION: Mig = Mig;

pub struct Mig;

impl Migration for Mig {
    fn id(&self) -> &'static str {
        "001_node_id_ulid"
    }
    fn description(&self) -> &'static str {
        "nodes/edges: stringly id → ULID + name extraction"
    }
    fn up(&self, conn: &Connection) -> Result<()> {
        let nodes_done = column_exists(conn, "nodes", "name")?;
        let edges_done = column_exists(conn, "edges", "name")?;
        if nodes_done && edges_done {
            // Already migrated externally — framework records the row, body is no-op.
            return Ok(());
        }

        if !nodes_done {
            conn.execute(
                "ALTER TABLE nodes ADD COLUMN name TEXT NOT NULL DEFAULT ''",
                [],
            )
            .context("ALTER nodes ADD name")?;
        }
        if !edges_done {
            conn.execute("ALTER TABLE edges ADD COLUMN name TEXT", [])
                .context("ALTER edges ADD name")?;
        }

        conn.execute(
            "UPDATE nodes SET name = id WHERE name IS NULL OR name = ''",
            [],
        )?;
        conn.execute("UPDATE edges SET name = id WHERE name IS NULL", [])?;

        let node_map = build_id_map(conn, "nodes")?;
        let edge_map = build_id_map(conn, "edges")?;

        conn.execute_batch(
            "DROP TABLE IF EXISTS _node_id_map;
             CREATE TEMP TABLE _node_id_map(old_id TEXT PRIMARY KEY, new_id TEXT NOT NULL);
             DROP TABLE IF EXISTS _edge_id_map;
             CREATE TEMP TABLE _edge_id_map(old_id TEXT PRIMARY KEY, new_id TEXT NOT NULL);",
        )?;
        {
            let mut stmt =
                conn.prepare("INSERT INTO _node_id_map(old_id, new_id) VALUES (?1, ?2)")?;
            for (old, new) in &node_map {
                stmt.execute(params![old, new.to_string()])?;
            }
        }
        {
            let mut stmt =
                conn.prepare("INSERT INTO _edge_id_map(old_id, new_id) VALUES (?1, ?2)")?;
            for (old, new) in &edge_map {
                stmt.execute(params![old, new.to_string()])?;
            }
        }

        // FK / version refs first (while node.id still holds old string).
        conn.execute(
            "UPDATE edges SET src_node = (SELECT new_id FROM _node_id_map WHERE old_id = edges.src_node) \
             WHERE EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = edges.src_node)",
            [],
        )?;
        conn.execute(
            "UPDATE edges SET tgt_node = (SELECT new_id FROM _node_id_map WHERE old_id = edges.tgt_node) \
             WHERE EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = edges.tgt_node)",
            [],
        )?;
        conn.execute(
            "UPDATE edges SET prev_id = (SELECT new_id FROM _edge_id_map WHERE old_id = edges.prev_id) \
             WHERE prev_id IS NOT NULL \
               AND EXISTS (SELECT 1 FROM _edge_id_map WHERE old_id = edges.prev_id)",
            [],
        )?;
        conn.execute(
            "UPDATE nodes SET prev_id = (SELECT new_id FROM _node_id_map WHERE old_id = nodes.prev_id) \
             WHERE prev_id IS NOT NULL \
               AND EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = nodes.prev_id)",
            [],
        )?;
        conn.execute(
            "UPDATE versions SET target_id = (SELECT new_id FROM _node_id_map WHERE old_id = versions.target_id) \
             WHERE target_kind = 'node' \
               AND EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = versions.target_id)",
            [],
        )?;
        conn.execute(
            "UPDATE versions SET target_id = (SELECT new_id FROM _edge_id_map WHERE old_id = versions.target_id) \
             WHERE target_kind = 'edge' \
               AND EXISTS (SELECT 1 FROM _edge_id_map WHERE old_id = versions.target_id)",
            [],
        )?;

        // PKs.
        conn.execute(
            "UPDATE nodes SET id = (SELECT new_id FROM _node_id_map WHERE old_id = nodes.id)",
            [],
        )?;
        conn.execute(
            "UPDATE edges SET id = (SELECT new_id FROM _edge_id_map WHERE old_id = edges.id)",
            [],
        )?;

        // Indexes + cleanup.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
             CREATE INDEX IF NOT EXISTS idx_edges_name ON edges(name);
             DROP TABLE _node_id_map;
             DROP TABLE _edge_id_map;",
        )?;

        // Sanity: every PK is now 26-char ULID.
        let bad_n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        let bad_e: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        if bad_n != 0 || bad_e != 0 {
            anyhow::bail!("001_node_id_ulid sanity failed: bad_nodes={bad_n} bad_edges={bad_e}");
        }
        Ok(())
    }
}

fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for n in rows {
        if n? == col {
            return Ok(true);
        }
    }
    Ok(false)
}

fn build_id_map(conn: &Connection, table: &str) -> Result<HashMap<String, Ulid>> {
    let mut stmt = conn.prepare(&format!("SELECT id FROM {table}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut map = HashMap::new();
    for r in rows {
        let old = r?;
        let new_id = Ulid::from_string(&old).unwrap_or_else(|_| Ulid::new());
        map.insert(old, new_id);
    }
    Ok(map)
}
