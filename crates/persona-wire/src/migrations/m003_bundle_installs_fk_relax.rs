//! 003 — relax `bundle_installs.bundle_id` to nullable + ON DELETE SET NULL.
//!
//! Bundle v1's `bundle_registry` documentation contract states "install
//! history is intentionally preserved across bundle deletion", but the
//! initial 0.7.0 schema declared
//! `bundle_id TEXT NOT NULL REFERENCES bundles(id)` (= default RESTRICT),
//! which the SQLite engine enforces as a foreign_key_check on DELETE.
//! Result: `wire_bundle_delete` failed with `FOREIGN KEY constraint
//! failed` whenever an install log entry referenced the bundle (= every
//! installed bundle).
//!
//! This migration applies the classic SQLite ALTER recipe to rebuild the
//! table with the corrected FK shape while preserving existing install
//! rows verbatim. Idempotent — re-running once `bundle_id` already
//! accepts NULL leaves the schema untouched.

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::Migration;

/// Registry entry for migration 003 — referenced from
/// [`super::ALL`] in execution order.
pub static MIGRATION: Mig = Mig;

/// Zero-sized [`Migration`] impl for the
/// `003_bundle_installs_fk_relax` schema change. Instances carry no
/// state; the [`MIGRATION`] singleton is the intended handle.
pub struct Mig;

impl Migration for Mig {
    fn id(&self) -> &'static str {
        "003_bundle_installs_fk_relax"
    }
    fn description(&self) -> &'static str {
        "bundle_installs: bundle_id NOT NULL → nullable + ON DELETE SET NULL"
    }
    fn up(&self, conn: &Connection) -> Result<()> {
        // Bundles feature might not exist yet on a DB initially provisioned
        // by storage::migrate() against an older binary — guard so the
        // migration is also safe to apply on pre-0.7 stores that have not
        // yet seen `bundle_installs` materialized.
        if !table_exists(conn, "bundle_installs")? {
            return Ok(());
        }
        if column_is_nullable(conn, "bundle_installs", "bundle_id")? {
            return Ok(());
        }

        // Classic 12-step rebuild. Outer transaction + FK guards are
        // owned by the framework (Runner::apply_one).
        conn.execute_batch(
            r#"
            CREATE TABLE bundle_installs_new (
                install_id   TEXT PRIMARY KEY,
                bundle_id    TEXT REFERENCES bundles(id) ON DELETE SET NULL,
                mode         TEXT NOT NULL CHECK (mode IN ('increment', 'skip', 'error')),
                installed_at INTEGER NOT NULL DEFAULT 0,
                report       TEXT NOT NULL DEFAULT '{}'
            );

            INSERT INTO bundle_installs_new(install_id, bundle_id, mode, installed_at, report)
            SELECT install_id, bundle_id, mode, installed_at, report FROM bundle_installs;

            DROP TABLE bundle_installs;
            ALTER TABLE bundle_installs_new RENAME TO bundle_installs;
            CREATE INDEX IF NOT EXISTS idx_bundle_installs_bundle ON bundle_installs(bundle_id);
            "#,
        )
        .context("rebuild bundle_installs with nullable bundle_id")?;
        Ok(())
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let found: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get(0),
        )
        .ok();
    Ok(found.is_some())
}

/// Read `PRAGMA table_info(<table>)` and return whether the given column
/// is declared NULL-permissive. SQLite reports `notnull = 0` for nullable
/// columns. Returns `false` if the column is absent (callers gate on
/// `table_exists` first).
fn column_is_nullable(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?)))?;
    for row in rows {
        let (name, notnull) = row?;
        if name == column {
            return Ok(notnull == 0);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn seed_pre_m003(conn: &Connection) {
        // Replicate the pre-fix schema (id NOT NULL + RESTRICT FK).
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE bundles (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                version     TEXT NOT NULL,
                description TEXT,
                body        TEXT NOT NULL,
                created_at  INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE bundle_installs (
                install_id   TEXT PRIMARY KEY,
                bundle_id    TEXT NOT NULL REFERENCES bundles(id),
                mode         TEXT NOT NULL CHECK (mode IN ('increment', 'skip', 'error')),
                installed_at INTEGER NOT NULL DEFAULT 0,
                report       TEXT NOT NULL DEFAULT '{}'
            );
            INSERT INTO bundles(id, name, version, body) VALUES
                ('bid-1', 'demo', '0.1.0', '# header only');
            INSERT INTO bundle_installs(install_id, bundle_id, mode, installed_at, report) VALUES
                ('iid-1', 'bid-1', 'increment', 1, '{}');
            "#,
        )
        .unwrap();
    }

    #[test]
    fn up_preserves_rows_and_allows_bundle_delete() {
        let conn = Connection::open_in_memory().unwrap();
        seed_pre_m003(&conn);

        // Before migration: delete fails due to RESTRICT FK.
        assert!(conn
            .execute("DELETE FROM bundles WHERE id = ?1", params!["bid-1"])
            .is_err());

        MIGRATION.up(&conn).unwrap();

        // Existing install row survived.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bundle_installs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // bundle_id is now nullable.
        assert!(column_is_nullable(&conn, "bundle_installs", "bundle_id").unwrap());

        // Delete now succeeds; FK SET NULL fires.
        conn.execute("DELETE FROM bundles WHERE id = ?1", params!["bid-1"])
            .expect("delete after migration");
        let bundle_id: Option<String> = conn
            .query_row(
                "SELECT bundle_id FROM bundle_installs WHERE install_id = ?1",
                params!["iid-1"],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            bundle_id.is_none(),
            "bundle_id should be NULL after parent delete"
        );
    }

    #[test]
    fn up_is_idempotent_when_already_relaxed() {
        let conn = Connection::open_in_memory().unwrap();
        seed_pre_m003(&conn);
        MIGRATION.up(&conn).unwrap();
        // Second run is a no-op.
        MIGRATION.up(&conn).unwrap();
        assert!(column_is_nullable(&conn, "bundle_installs", "bundle_id").unwrap());
    }

    #[test]
    fn up_is_noop_when_bundle_installs_absent() {
        let conn = Connection::open_in_memory().unwrap();
        // No bundle_installs table at all — pre-0.7 store.
        conn.execute_batch("CREATE TABLE placeholder (x INTEGER)")
            .unwrap();
        MIGRATION.up(&conn).expect("no-op on missing table");
    }
}
