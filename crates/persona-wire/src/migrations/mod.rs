//! Schema migration framework — Diesel / sqlx style, scoped to persona-wire's
//! SQLite store. Each numbered migration declares an idempotent `up` step
//! that the [`Runner`] tracks in a `schema_migrations` table so re-running
//! is a no-op once applied.
//!
//! Migrations are listed in [`ALL`] in **execution order**. A new schema
//! change adds one module under this directory plus one entry at the end of
//! `ALL`. Down migrations are intentionally not modelled in v1 — the
//! framework leaves room for a future `Migration::down` extension once a
//! real rollback need shows up.

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
pub use ulid::Ulid;

pub mod m001_node_id_ulid;
pub mod m002_registry_id_ulid;

/// One immutable, monotonic schema change. `id` must be unique across the
/// whole [`ALL`] list and never change once shipped — it is the persistent
/// key the `schema_migrations` table records.
pub trait Migration: Sync {
    /// Stable identifier (e.g. `"001_node_id_ulid"`). Persisted to
    /// `schema_migrations.version`; renaming after release breaks
    /// idempotency for already-migrated stores.
    fn id(&self) -> &'static str;

    /// One-line human description (surfaced by `pw-migrate status` etc).
    fn description(&self) -> &'static str;

    /// Apply this migration to `conn`. The framework runs `up` inside an
    /// outer `BEGIN IMMEDIATE` transaction managed by [`Runner`]; the
    /// migration body is free to issue further `PRAGMA` / DDL / DML as
    /// long as it leaves the schema in the post-migration shape.
    fn up(&self, conn: &Connection) -> Result<()>;
}

/// Registry of all known migrations, in **execution order**. Append-only.
pub static ALL: &[&'static dyn Migration] = &[
    &m001_node_id_ulid::MIGRATION,
    &m002_registry_id_ulid::MIGRATION,
];

/// Driver around a `Connection` that knows how to read / write the
/// `schema_migrations` ledger and apply pending migrations.
pub struct Runner<'a> {
    conn: &'a Connection,
}

impl<'a> Runner<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Ensure the bookkeeping table exists. Idempotent — safe to call on
    /// every CLI invocation.
    pub fn ensure_table(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                     version     TEXT PRIMARY KEY,
                     description TEXT NOT NULL,
                     applied_at  INTEGER NOT NULL
                 );",
            )
            .context("create schema_migrations")?;
        Ok(())
    }

    /// Snapshot of (applied, pending) migrations vs the [`ALL`] registry.
    pub fn status(&self) -> Result<Status> {
        self.ensure_table()?;
        let mut stmt = self.conn.prepare(
            "SELECT version, description, applied_at FROM schema_migrations \
             ORDER BY applied_at",
        )?;
        let applied: Vec<AppliedRow> = stmt
            .query_map([], |r| {
                Ok(AppliedRow {
                    version: r.get(0)?,
                    description: r.get(1)?,
                    applied_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let applied_ids: std::collections::HashSet<String> =
            applied.iter().map(|r| r.version.clone()).collect();
        let pending: Vec<&'static dyn Migration> = ALL
            .iter()
            .copied()
            .filter(|m| !applied_ids.contains(m.id()))
            .collect();
        Ok(Status { applied, pending })
    }

    /// Apply every pending migration in [`ALL`] order, up to and including
    /// `target` (or all pending when `target` is `None`). Returns the list
    /// of migrations actually applied this call (skipped ones are not
    /// included).
    pub fn up(&self, target: Option<&str>) -> Result<Vec<AppliedNow>> {
        let status = self.status()?;
        if let Some(t) = target {
            if !ALL.iter().any(|m| m.id() == t) {
                return Err(anyhow!("unknown migration: {t}"));
            }
        }
        let mut applied_now = Vec::new();
        for m in status.pending {
            self.apply_one(m)?;
            applied_now.push(AppliedNow {
                version: m.id().to_string(),
                description: m.description().to_string(),
            });
            if target.is_some_and(|t| t == m.id()) {
                break;
            }
        }
        Ok(applied_now)
    }

    /// Apply exactly one migration by id (errors if already applied OR not
    /// known). Useful when an operator wants strict 1-step control.
    pub fn apply(&self, id: &str) -> Result<()> {
        let m = ALL
            .iter()
            .copied()
            .find(|m| m.id() == id)
            .ok_or_else(|| anyhow!("unknown migration: {id}"))?;
        let already = self.status()?.applied.iter().any(|r| r.version == id);
        if already {
            return Err(anyhow!("migration already applied: {id}"));
        }
        self.apply_one(m)
    }

    fn apply_one(&self, m: &'static dyn Migration) -> Result<()> {
        // FK guard off + outer transaction. Migration body may toggle further
        // pragmas but the commit / rollback is the framework's responsibility.
        self.conn
            .execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")
            .context("open migration transaction")?;
        let res: Result<()> = (|| {
            m.up(self.conn)
                .with_context(|| format!("migration {} body", m.id()))?;
            self.conn
                .execute(
                    "INSERT INTO schema_migrations(version, description, applied_at) \
                     VALUES (?1, ?2, ?3)",
                    params![m.id(), m.description(), epoch_ms()?],
                )
                .with_context(|| format!("record schema_migrations row for {}", m.id()))?;
            // Re-enable FK + validate.
            self.conn
                .execute_batch("PRAGMA foreign_keys = ON;")
                .context("re-enable foreign_keys")?;
            let mut stmt = self.conn.prepare("PRAGMA foreign_key_check;")?;
            let violations: Vec<(String, i64, String, i64)> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            if !violations.is_empty() {
                return Err(anyhow!(
                    "foreign_key_check violations after {}: {:?}",
                    m.id(),
                    violations
                ));
            }
            Ok(())
        })();
        match res {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e.context(format!("migration {} aborted; rolled back", m.id())))
            }
        }
    }
}

fn epoch_ms() -> Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time")?;
    Ok(d.as_millis() as i64)
}

#[derive(Clone)]
pub struct Status {
    pub applied: Vec<AppliedRow>,
    pub pending: Vec<&'static dyn Migration>,
}

impl std::fmt::Debug for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Status")
            .field("applied", &self.applied)
            .field("pending_ids", &self.pending.iter().map(|m| m.id()).collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AppliedRow {
    pub version: String,
    pub description: String,
    pub applied_at: i64,
}

#[derive(Debug, Clone)]
pub struct AppliedNow {
    pub version: String,
    pub description: String,
}

// Re-exported so individual migration modules and the bin agree on the
// type for ULID minting + mapping tables.
pub(crate) use Ulid as MigrationUlid;

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_v0_6(conn: &Connection) {
        conn.execute_batch(
            r#"
            CREATE TABLE type_registry (
                name TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                schema_json TEXT,
                severity_allowed TEXT
            );
            CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL REFERENCES type_registry(name),
                sot_ref TEXT, confidence REAL, applicability TEXT,
                last_verified_at INTEGER, review_due INTEGER,
                version INTEGER NOT NULL DEFAULT 1,
                prev_id TEXT,
                metadata TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE edges (
                id TEXT PRIMARY KEY,
                src_node TEXT NOT NULL REFERENCES nodes(id),
                tgt_node TEXT NOT NULL REFERENCES nodes(id),
                kind TEXT NOT NULL REFERENCES type_registry(name),
                severity TEXT,
                metadata TEXT NOT NULL DEFAULT '{}',
                version INTEGER NOT NULL DEFAULT 1,
                prev_id TEXT
            );
            CREATE TABLE versions (
                target_kind TEXT NOT NULL,
                target_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                diff TEXT NOT NULL DEFAULT '{}',
                ts INTEGER NOT NULL,
                author TEXT,
                PRIMARY KEY (target_kind, target_id, version)
            );
            CREATE TABLE specifications (
                name TEXT PRIMARY KEY,
                expr_json TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE projections (
                name TEXT PRIMARY KEY,
                spec_ref TEXT NOT NULL,
                template TEXT NOT NULL,
                target_form TEXT NOT NULL,
                created_at INTEGER NOT NULL DEFAULT 0,
                template_engine TEXT,
                projection_kind TEXT,
                projection_config TEXT
            );
            INSERT INTO type_registry(name, kind) VALUES
                ('persona', 'node'), ('outline_node', 'node'), ('routes_to', 'edge');
            INSERT INTO nodes(id, type, version) VALUES
                ('alpha', 'persona', 1),
                ('alpha.active', 'outline_node', 1);
            INSERT INTO edges(id, src_node, tgt_node, kind, version) VALUES
                ('e.alpha.active', 'alpha', 'alpha.active', 'routes_to', 1);
            INSERT INTO specifications(name, expr_json) VALUES
                ('active_personas', '{"TypeIs":"persona"}');
            INSERT INTO projections(name, spec_ref, template, target_form) VALUES
                ('alpha.section.active', 'active_personas', '## {{name}}', 'markdown');
            "#,
        )
        .unwrap();
    }

    #[test]
    fn runner_up_applies_all_pending_then_skips_on_rerun() {
        let conn = Connection::open_in_memory().unwrap();
        seed_v0_6(&conn);
        let runner = Runner::new(&conn);

        let s = runner.status().unwrap();
        assert_eq!(s.applied.len(), 0);
        assert_eq!(s.pending.len(), 2);

        let now = runner.up(None).unwrap();
        assert_eq!(now.len(), 2);

        let s2 = runner.status().unwrap();
        assert_eq!(s2.applied.len(), 2);
        assert!(s2.pending.is_empty());

        // Re-run is a no-op.
        let now2 = runner.up(None).unwrap();
        assert!(now2.is_empty());

        // Post-state sanity.
        let bad_nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes WHERE length(id) != 26", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bad_nodes, 0);
        let bad_specs: i64 = conn
            .query_row("SELECT COUNT(*) FROM specifications WHERE length(id) != 26", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bad_specs, 0);
    }

    #[test]
    fn runner_apply_specific_id_then_repeat_errors() {
        let conn = Connection::open_in_memory().unwrap();
        seed_v0_6(&conn);
        let runner = Runner::new(&conn);

        runner.apply("001_node_id_ulid").unwrap();
        let s = runner.status().unwrap();
        assert_eq!(s.applied.len(), 1);
        assert_eq!(s.pending.len(), 1);

        // Second time errors.
        assert!(runner.apply("001_node_id_ulid").is_err());

        // Unknown id errors.
        assert!(runner.apply("999_nope").is_err());
    }

    #[test]
    fn runner_target_stops_at_named_migration() {
        let conn = Connection::open_in_memory().unwrap();
        seed_v0_6(&conn);
        let runner = Runner::new(&conn);
        let now = runner.up(Some("001_node_id_ulid")).unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].version, "001_node_id_ulid");
        let s = runner.status().unwrap();
        assert_eq!(s.pending.len(), 1);
        assert_eq!(s.pending[0].id(), "002_registry_id_ulid");
    }
}
