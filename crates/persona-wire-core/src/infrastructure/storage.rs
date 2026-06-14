//! SQLite storage adapter.
//!
//! Schema (P1 target):
//! - `type_registry(name, kind, schema_json, severity_allowed)`
//! - `nodes(...)` / `edges(...)` / `versions(...)`
//! - `specifications(spec_id, expr_json, ...)`
//! - `projections(name, spec_ref, template, target_form, ...)`
//! - `workflow_runs(...)`

use crate::domain::error::{WireError, WireResult};
use rusqlite::Connection;

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

    /// Apply minimal schema (P1 stub — full schema lands at P1 implementation).
    pub fn migrate(&self) -> WireResult<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS type_registry (
                    name TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    schema_json TEXT,
                    severity_allowed TEXT
                );",
            )
            .map_err(|e| WireError::Storage(e.to_string()))?;
        Ok(())
    }
}
