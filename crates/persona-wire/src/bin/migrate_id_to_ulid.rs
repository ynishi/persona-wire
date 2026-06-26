//! v0.6.x → v0.7.0 SQLite schema migration: stringly `id` → opaque ULID `id`
//! + human-readable `name` column.
//!
//! Run manually against an existing persona-wire SQLite store. Idempotent
//! at the schema-detection level (re-running on an already-migrated DB
//! exits cleanly).
//!
//! Usage:
//!
//! ```sh
//! # Dry-run (default, NO mutation). Reports counts + mapping plan to stdout.
//! cargo run -p persona-wire --bin migrate_id_to_ulid -- --db <path>
//!
//! # Real run. Backup happens automatically to `<db>.pre-ulid.bak` unless
//! # `--backup <path>` overrides the destination.
//! cargo run -p persona-wire --bin migrate_id_to_ulid -- --db <path> --apply
//!
//! # Real run with custom backup path + mapping dump.
//! cargo run -p persona-wire --bin migrate_id_to_ulid -- --db <path> --apply \
//!     --backup /tmp/store.before-ulid.db \
//!     --mapping-out /tmp/id-mapping.json
//! ```
//!
//! Safety:
//! - `--dry-run` is the default; `--apply` must be passed to mutate the DB.
//! - Backup is mandatory on `--apply`. Default destination is
//!   `<db>.pre-ulid.bak` (sibling of the source). Fails fast if the backup
//!   path exists and `--force` is not set.
//! - All schema + data writes happen in a single `BEGIN IMMEDIATE`
//!   transaction with `PRAGMA foreign_keys = OFF`. Any error rolls back.
//! - On success, `PRAGMA foreign_key_check` must report empty before commit.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use ulid::Ulid;

#[derive(Debug)]
struct Args {
    db: PathBuf,
    apply: bool,
    backup: Option<PathBuf>,
    mapping_out: Option<PathBuf>,
    force: bool,
}

fn parse_args() -> Result<Args> {
    let mut db: Option<PathBuf> = None;
    let mut apply = false;
    let mut backup: Option<PathBuf> = None;
    let mut mapping_out: Option<PathBuf> = None;
    let mut force = false;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--db" => db = Some(PathBuf::from(it.next().ok_or_else(|| anyhow!("--db requires path"))?)),
            "--apply" => apply = true,
            "--dry-run" => apply = false,
            "--backup" => backup = Some(PathBuf::from(it.next().ok_or_else(|| anyhow!("--backup requires path"))?)),
            "--mapping-out" => mapping_out = Some(PathBuf::from(it.next().ok_or_else(|| anyhow!("--mapping-out requires path"))?)),
            "--force" => force = true,
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => bail!("unknown arg: {other} (try --help)"),
        }
    }

    let db = db.ok_or_else(|| anyhow!("--db <path> is required (see --help)"))?;
    Ok(Args {
        db,
        apply,
        backup,
        mapping_out,
        force,
    })
}

fn print_usage() {
    println!("Usage: migrate_id_to_ulid --db <path> [--apply] [--backup <path>]");
    println!("                          [--mapping-out <path>] [--force]");
    println!();
    println!("Default mode is dry-run (no mutation). Pass --apply to mutate.");
    println!("Backup is mandatory on --apply (default: <db>.pre-ulid.bak).");
}

fn main() -> Result<()> {
    let args = parse_args()?;
    if !args.db.exists() {
        bail!("--db path does not exist: {}", args.db.display());
    }

    let conn = Connection::open(&args.db)
        .with_context(|| format!("open DB: {}", args.db.display()))?;

    let state = inspect_schema(&conn)?;
    println!("== schema inspection ==");
    println!("  nodes.name col          : {}", state.has_nodes_name);
    println!("  edges.name col          : {}", state.has_edges_name);
    println!("  specifications.id col   : {}", state.has_specs_id);
    println!("  projections.id col      : {}", state.has_projs_id);
    println!(
        "  rows: nodes={} edges={} versions={} specs={} projs={}",
        state.node_count, state.edge_count, state.version_count,
        state.spec_count, state.proj_count
    );

    let needs_phase_a = !state.has_nodes_name || !state.has_edges_name;
    let needs_phase_b = !state.has_specs_id || !state.has_projs_id;
    if !needs_phase_a && !needs_phase_b {
        println!("[skip] schema already migrated. Nothing to do.");
        return Ok(());
    }
    println!(
        "\n  phase A (nodes/edges → ULID) : {}",
        if needs_phase_a { "PENDING" } else { "skip" }
    );
    println!(
        "  phase B (registries → ULID)  : {}",
        if needs_phase_b { "PENDING" } else { "skip" }
    );

    let mode = if args.apply { "APPLY" } else { "DRY-RUN" };
    println!("\n== plan ({mode}) ==");

    if args.apply {
        // Resolve + assert backup destination.
        let backup = resolve_backup_path(&args)?;
        if backup.exists() && !args.force {
            bail!(
                "backup path already exists: {} (pass --force to overwrite, or pick a different --backup)",
                backup.display()
            );
        }
        std::fs::copy(&args.db, &backup)
            .with_context(|| format!("backup DB to {}", backup.display()))?;
        println!("  [ok] backup written: {}", backup.display());
    } else {
        println!("  (--dry-run) backup skipped — pass --apply to mutate.");
    }

    // Phase A — node/edge id mapping (skipped if columns already present).
    let (node_map, edge_map) = if needs_phase_a {
        let n = build_id_map(&conn, "nodes")?;
        let e = build_id_map(&conn, "edges")?;
        println!(
            "  phase A mapping: {} node ids → ULID, {} edge ids → ULID",
            n.len(),
            e.len()
        );
        (n, e)
    } else {
        (HashMap::new(), HashMap::new())
    };

    // Phase B — registry name → id mapping. Existing rows have no `id`
    // column yet, so we key the map by `name` (already UNIQUE PK).
    let (spec_map, proj_map) = if needs_phase_b {
        let s = build_id_map_keyed_by_name(&conn, "specifications")?;
        let p = build_id_map_keyed_by_name(&conn, "projections")?;
        println!(
            "  phase B mapping: {} spec names → ULID, {} projection names → ULID",
            s.len(),
            p.len()
        );
        (s, p)
    } else {
        (HashMap::new(), HashMap::new())
    };

    if let Some(path) = args.mapping_out.as_ref() {
        let dump = json!({
            "nodes": node_map.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<HashMap<_, _>>(),
            "edges": edge_map.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<HashMap<_, _>>(),
            "specifications": spec_map.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<HashMap<_, _>>(),
            "projections": proj_map.iter().map(|(k, v)| (k.clone(), v.to_string())).collect::<HashMap<_, _>>(),
            "db": args.db.display().to_string(),
            "applied": args.apply,
            "phases": json!({ "A": needs_phase_a, "B": needs_phase_b }),
        });
        std::fs::write(path, serde_json::to_string_pretty(&dump)?)
            .with_context(|| format!("write mapping to {}", path.display()))?;
        println!("  [ok] mapping dumped: {}", path.display());
    }

    if !args.apply {
        println!("\n[dry-run complete] no DB mutation. Re-run with --apply to commit.");
        return Ok(());
    }

    if needs_phase_a {
        apply_migration(&conn, &node_map, &edge_map)?;
    }
    if needs_phase_b {
        apply_registry_migration(&conn, &spec_map, &proj_map)?;
    }

    println!("\n[apply complete] migration committed. DB: {}", args.db.display());
    Ok(())
}

fn resolve_backup_path(args: &Args) -> Result<PathBuf> {
    if let Some(p) = args.backup.as_ref() {
        return Ok(p.clone());
    }
    let mut p = args.db.clone();
    let fname = p
        .file_name()
        .ok_or_else(|| anyhow!("--db has no file name component"))?
        .to_string_lossy()
        .into_owned();
    p.set_file_name(format!("{fname}.pre-ulid.bak"));
    Ok(p)
}

#[derive(Debug)]
struct SchemaState {
    has_nodes_name: bool,
    has_edges_name: bool,
    has_specs_id: bool,
    has_projs_id: bool,
    node_count: i64,
    edge_count: i64,
    version_count: i64,
    spec_count: i64,
    proj_count: i64,
}

fn inspect_schema(conn: &Connection) -> Result<SchemaState> {
    fn has_column(conn: &Connection, table: &str, col: &str) -> Result<bool> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .with_context(|| format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for n in rows {
            if n? == col {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn count(conn: &Connection, table: &str) -> Result<i64> {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .with_context(|| format!("count {table}"))
    }
    fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }
    fn count_if_exists(conn: &Connection, table: &str) -> Result<i64> {
        if table_exists(conn, table)? { count(conn, table) } else { Ok(0) }
    }
    fn has_col_if_exists(conn: &Connection, table: &str, col: &str) -> Result<bool> {
        if table_exists(conn, table)? { has_column(conn, table, col) } else { Ok(true) }
    }
    Ok(SchemaState {
        has_nodes_name: has_column(conn, "nodes", "name")?,
        has_edges_name: has_column(conn, "edges", "name")?,
        has_specs_id: has_col_if_exists(conn, "specifications", "id")?,
        has_projs_id: has_col_if_exists(conn, "projections", "id")?,
        node_count: count(conn, "nodes")?,
        edge_count: count(conn, "edges")?,
        version_count: count(conn, "versions")?,
        spec_count: count_if_exists(conn, "specifications")?,
        proj_count: count_if_exists(conn, "projections")?,
    })
}

/// Build a `name → fresh ULID` mapping for a table whose current PK is `name`
/// (specifications / projections in the legacy schema). Used by phase B.
fn build_id_map_keyed_by_name(conn: &Connection, table: &str) -> Result<HashMap<String, Ulid>> {
    let mut stmt = conn
        .prepare(&format!("SELECT name FROM {table}"))
        .with_context(|| format!("prepare select name from {table}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut map = HashMap::new();
    for r in rows {
        map.insert(r?, Ulid::new());
    }
    Ok(map)
}

/// Phase B: rebuild `specifications` / `projections` with `id TEXT PK + name UNIQUE`,
/// minting a fresh ULID per legacy row. SQLite cannot rename PK in place, so we
/// follow the canonical 12-step ALTER recipe (CREATE new + INSERT SELECT + DROP
/// old + RENAME). Inside one transaction.
fn apply_registry_migration(
    conn: &Connection,
    spec_map: &HashMap<String, Ulid>,
    proj_map: &HashMap<String, Ulid>,
) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")
        .context("open phase B transaction")?;

    let res: Result<()> = (|| {
        // ----- specifications -----
        if !column_exists(conn, "specifications", "id")? {
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
                for (name, id) in spec_map {
                    stmt.execute(params![id.to_string(), name])?;
                }
            }
            conn.execute("DROP TABLE specifications", [])?;
            conn.execute("ALTER TABLE specifications_new RENAME TO specifications", [])?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_specifications_name ON specifications(name)",
                [],
            )?;
            println!("  [step] specifications rebuilt with id PK ({} rows)", spec_map.len());
        }

        // ----- projections -----
        if !column_exists(conn, "projections", "id")? {
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
                for (name, id) in proj_map {
                    stmt.execute(params![id.to_string(), name])?;
                }
            }
            conn.execute("DROP TABLE projections", [])?;
            conn.execute("ALTER TABLE projections_new RENAME TO projections", [])?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_projections_name ON projections(name)",
                [],
            )?;
            println!("  [step] projections rebuilt with id PK ({} rows)", proj_map.len());
        }

        // Re-enable FK and run validation.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mut stmt = conn.prepare("PRAGMA foreign_key_check;")?;
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
            bail!("foreign_key_check reported {} violation(s): {:?}", violations.len(), violations);
        }

        // Sanity: every id is a 26-char ULID.
        let bad_specs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM specifications WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        let bad_projs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM projections WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        if bad_specs != 0 || bad_projs != 0 {
            bail!("phase B sanity failed: bad_specs={bad_specs} bad_projs={bad_projs}");
        }

        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute("COMMIT", []).context("commit phase B")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e.context("phase B aborted; transaction rolled back"))
        }
    }
}

fn build_id_map(conn: &Connection, table: &str) -> Result<HashMap<String, Ulid>> {
    let mut stmt = conn
        .prepare(&format!("SELECT id FROM {table}"))
        .with_context(|| format!("prepare select id from {table}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut map = HashMap::new();
    for r in rows {
        let old = r?;
        // Guard: if the row already holds a 26-char Crockford-base32 string,
        // reuse it as-is (idempotency for partial migrations). Otherwise mint.
        let new_id = match Ulid::from_string(&old) {
            Ok(u) => u,
            Err(_) => Ulid::new(),
        };
        map.insert(old, new_id);
    }
    Ok(map)
}

fn apply_migration(
    conn: &Connection,
    node_map: &HashMap<String, Ulid>,
    edge_map: &HashMap<String, Ulid>,
) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")
        .context("open transaction")?;

    let res: Result<()> = (|| {
        // Add missing columns (idempotent — wrapped because ALTER fails if
        // column already exists).
        if !column_exists(conn, "nodes", "name")? {
            conn.execute("ALTER TABLE nodes ADD COLUMN name TEXT NOT NULL DEFAULT ''", [])
                .context("ALTER nodes ADD name")?;
        }
        if !column_exists(conn, "edges", "name")? {
            conn.execute("ALTER TABLE edges ADD COLUMN name TEXT", [])
                .context("ALTER edges ADD name")?;
        }

        // Preserve old string id as `name`. Skip rows already populated
        // (re-run safety on a partial migration).
        let n_node_name = conn
            .execute("UPDATE nodes SET name = id WHERE name IS NULL OR name = ''", [])
            .context("UPDATE nodes name")?;
        let n_edge_name = conn
            .execute("UPDATE edges SET name = id WHERE name IS NULL", [])
            .context("UPDATE edges name")?;
        println!("  [step] copied old id → name: nodes={n_node_name} edges={n_edge_name}");

        // Stage mapping in temp tables so SQL UPDATEs can join against it.
        conn.execute_batch(
            "DROP TABLE IF EXISTS _node_id_map;
             CREATE TEMP TABLE _node_id_map(old_id TEXT PRIMARY KEY, new_id TEXT NOT NULL);
             DROP TABLE IF EXISTS _edge_id_map;
             CREATE TEMP TABLE _edge_id_map(old_id TEXT PRIMARY KEY, new_id TEXT NOT NULL);",
        )
        .context("create temp maps")?;

        {
            let mut stmt = conn.prepare("INSERT INTO _node_id_map(old_id, new_id) VALUES (?1, ?2)")?;
            for (old, new) in node_map {
                stmt.execute(params![old, new.to_string()])?;
            }
        }
        {
            let mut stmt = conn.prepare("INSERT INTO _edge_id_map(old_id, new_id) VALUES (?1, ?2)")?;
            for (old, new) in edge_map {
                stmt.execute(params![old, new.to_string()])?;
            }
        }

        // Rewrite FK columns FIRST (while node.id still holds the old string).
        let n_src = conn.execute(
            "UPDATE edges SET src_node = (SELECT new_id FROM _node_id_map WHERE old_id = edges.src_node) \
             WHERE EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = edges.src_node)",
            [],
        )?;
        let n_tgt = conn.execute(
            "UPDATE edges SET tgt_node = (SELECT new_id FROM _node_id_map WHERE old_id = edges.tgt_node) \
             WHERE EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = edges.tgt_node)",
            [],
        )?;
        let n_edge_prev = conn.execute(
            "UPDATE edges SET prev_id = (SELECT new_id FROM _edge_id_map WHERE old_id = edges.prev_id) \
             WHERE prev_id IS NOT NULL \
               AND EXISTS (SELECT 1 FROM _edge_id_map WHERE old_id = edges.prev_id)",
            [],
        )?;
        let n_node_prev = conn.execute(
            "UPDATE nodes SET prev_id = (SELECT new_id FROM _node_id_map WHERE old_id = nodes.prev_id) \
             WHERE prev_id IS NOT NULL \
               AND EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = nodes.prev_id)",
            [],
        )?;
        let n_v_node = conn.execute(
            "UPDATE versions SET target_id = (SELECT new_id FROM _node_id_map WHERE old_id = versions.target_id) \
             WHERE target_kind = 'node' \
               AND EXISTS (SELECT 1 FROM _node_id_map WHERE old_id = versions.target_id)",
            [],
        )?;
        let n_v_edge = conn.execute(
            "UPDATE versions SET target_id = (SELECT new_id FROM _edge_id_map WHERE old_id = versions.target_id) \
             WHERE target_kind = 'edge' \
               AND EXISTS (SELECT 1 FROM _edge_id_map WHERE old_id = versions.target_id)",
            [],
        )?;
        println!("  [step] rewrote FK / version refs: src={n_src} tgt={n_tgt} edge_prev={n_edge_prev} node_prev={n_node_prev} versions_node={n_v_node} versions_edge={n_v_edge}");

        // Now flip the PKs themselves.
        let n_nodes = conn.execute(
            "UPDATE nodes SET id = (SELECT new_id FROM _node_id_map WHERE old_id = nodes.id)",
            [],
        )?;
        let n_edges = conn.execute(
            "UPDATE edges SET id = (SELECT new_id FROM _edge_id_map WHERE old_id = edges.id)",
            [],
        )?;
        println!("  [step] flipped PKs: nodes={n_nodes} edges={n_edges}");

        // Secondary indexes — idempotent.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
             CREATE INDEX IF NOT EXISTS idx_edges_name ON edges(name);",
        )?;

        // Drop temp maps (no leak into the post-commit DB).
        conn.execute_batch("DROP TABLE _node_id_map; DROP TABLE _edge_id_map;")?;

        // Re-enable FK and validate.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mut stmt = conn.prepare("PRAGMA foreign_key_check;")?;
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
            bail!("foreign_key_check reported {} violation(s): {:?}", violations.len(), violations);
        }

        // Sanity: every PK row must now be a 26-char ULID.
        let bad_nodes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        let bad_edges: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE length(id) != 26",
            [],
            |r| r.get(0),
        )?;
        if bad_nodes != 0 || bad_edges != 0 {
            bail!("post-migration sanity failed: bad_nodes={bad_nodes} bad_edges={bad_edges}");
        }

        Ok(())
    })();

    match res {
        Ok(()) => {
            conn.execute("COMMIT", []).context("commit")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e.context("migration aborted; transaction rolled back"))
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_schema_seed(conn: &Connection) {
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
            INSERT INTO type_registry(name, kind) VALUES
                ('persona', 'node'), ('outline_node', 'node'), ('routes_to', 'edge');
            INSERT INTO nodes(id, type, version) VALUES
                ('alpha', 'persona', 1),
                ('alpha.active', 'outline_node', 1);
            INSERT INTO edges(id, src_node, tgt_node, kind, version) VALUES
                ('e.alpha.active', 'alpha', 'alpha.active', 'routes_to', 1);
            INSERT INTO versions(target_kind, target_id, version, ts) VALUES
                ('node', 'alpha', 1, 1700000000),
                ('edge', 'e.alpha.active', 1, 1700000000);
            "#,
        )
        .unwrap();
    }

    #[test]
    fn round_trip_migration_rewrites_ids_and_keeps_fks() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_schema_seed(&conn);

        let node_map = build_id_map(&conn, "nodes").unwrap();
        let edge_map = build_id_map(&conn, "edges").unwrap();
        assert_eq!(node_map.len(), 2);
        assert_eq!(edge_map.len(), 1);

        apply_migration(&conn, &node_map, &edge_map).unwrap();

        // Every PK is now a 26-char ULID.
        let bad: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes WHERE length(id) != 26", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bad, 0);

        // name preserves the old string identity.
        let names: Vec<String> = conn
            .prepare("SELECT name FROM nodes ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(names, vec!["alpha".to_string(), "alpha.active".to_string()]);

        // Edge FKs point at the rewritten node ids.
        let (src, tgt): (String, String) = conn
            .query_row("SELECT src_node, tgt_node FROM edges", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let alpha_new = node_map.get("alpha").unwrap().to_string();
        let active_new = node_map.get("alpha.active").unwrap().to_string();
        assert_eq!(src, alpha_new);
        assert_eq!(tgt, active_new);

        // versions.target_id rewritten too.
        let v_node: String = conn
            .query_row(
                "SELECT target_id FROM versions WHERE target_kind = 'node'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v_node, alpha_new);
    }

    #[test]
    fn rerun_on_migrated_db_exits_clean() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_schema_seed(&conn);
        let node_map = build_id_map(&conn, "nodes").unwrap();
        let edge_map = build_id_map(&conn, "edges").unwrap();
        apply_migration(&conn, &node_map, &edge_map).unwrap();

        let state = inspect_schema(&conn).unwrap();
        assert!(state.has_nodes_name);
        assert!(state.has_edges_name);
        // (driver code short-circuits before apply_migration on this state)
    }

    fn legacy_registry_schema(conn: &Connection) {
        conn.execute_batch(
            r#"
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
            INSERT INTO specifications(name, expr_json) VALUES
                ('active_personas', '{"TypeIs":"persona"}'),
                ('outline_only', '{"TypeIs":"outline_node"}');
            INSERT INTO projections(name, spec_ref, template, target_form) VALUES
                ('alpha.section.active', 'active_personas', '## {{name}}', 'markdown');
            "#,
        )
        .unwrap();
    }

    #[test]
    fn phase_b_rebuilds_registries_with_ulid_pk() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_registry_schema(&conn);

        let spec_map = build_id_map_keyed_by_name(&conn, "specifications").unwrap();
        let proj_map = build_id_map_keyed_by_name(&conn, "projections").unwrap();
        assert_eq!(spec_map.len(), 2);
        assert_eq!(proj_map.len(), 1);

        apply_registry_migration(&conn, &spec_map, &proj_map).unwrap();

        // id col is now PK + 26-char ULID.
        let bad: i64 = conn
            .query_row("SELECT COUNT(*) FROM specifications WHERE length(id) != 26", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bad, 0);
        let bad: i64 = conn
            .query_row("SELECT COUNT(*) FROM projections WHERE length(id) != 26", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bad, 0);

        // name preserved + UNIQUE.
        let names: Vec<String> = conn
            .prepare("SELECT name FROM specifications ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(names, vec!["active_personas".to_string(), "outline_only".to_string()]);

        // projections.spec_ref untouched (still by name).
        let spec_ref: String = conn
            .query_row("SELECT spec_ref FROM projections", [], |r| r.get(0))
            .unwrap();
        assert_eq!(spec_ref, "active_personas");
    }
}
