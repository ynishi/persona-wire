//! Deprecated alias for the unified `pw-migrate` runner.
//!
//! v0.7.0 introduced the [`persona_wire::migrations`] framework. The old
//! task-specific binary (`migrate_id_to_ulid`) now forwards to
//! `pw-migrate up --db <path> [--apply] [--backup ...] [--mapping-out ...]`
//! and emits a deprecation warning so existing scripts keep working.
//!
//! Removal target: v0.8.0. Update callers to `pw-migrate up ...` directly.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;

use persona_wire::migrations::{Runner, ALL};

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
            "--mapping-out" => {
                mapping_out = Some(PathBuf::from(it.next().ok_or_else(|| anyhow!("--mapping-out requires path"))?))
            }
            "--force" => force = true,
            "-h" | "--help" => {
                eprintln!(
                    "migrate_id_to_ulid is deprecated. Use:\n  pw-migrate up --db <path> [--apply] [--backup <path>] [--force]\n  pw-migrate list / status / apply <id>"
                );
                std::process::exit(0);
            }
            other => bail!("unknown arg: {other} (try --help)"),
        }
    }

    let db = db.ok_or_else(|| anyhow!("--db <path> is required"))?;
    Ok(Args { db, apply, backup, mapping_out, force })
}

fn main() -> Result<()> {
    eprintln!(
        "warning: `migrate_id_to_ulid` is deprecated as of v0.7.0; use `pw-migrate up`.\n"
    );
    let args = parse_args()?;

    let conn = Connection::open(&args.db)
        .with_context(|| format!("open DB: {}", args.db.display()))?;
    let runner = Runner::new(&conn);
    let status = runner.status()?;

    println!("== schema migration status ==");
    for r in &status.applied {
        println!("  [applied] {}", r.version);
    }
    for m in &status.pending {
        println!("  [pending] {}  {}", m.id(), m.description());
    }

    if status.pending.is_empty() {
        println!("\n[skip] no pending migrations. Nothing to do.");
        return Ok(());
    }

    if !args.apply {
        println!("\n[dry-run] {} migration(s) would be applied. Re-run with --apply.", status.pending.len());
        if let Some(p) = args.mapping_out.as_ref() {
            // Backward-compat: dump a placeholder mapping file. The v0.7
            // framework no longer exposes per-row mapping at the CLI
            // boundary (each Migration::up handles its own mapping
            // internally). The file is kept as a marker for scripts that
            // check for its existence.
            let dump = serde_json::json!({
                "db": args.db.display().to_string(),
                "applied": false,
                "note": "v0.7+ migrations do not expose per-row id mapping at the CLI boundary; this file is a compatibility marker.",
                "pending_migrations": status.pending.iter().map(|m| m.id()).collect::<Vec<_>>(),
            });
            std::fs::write(p, serde_json::to_string_pretty(&dump)?)
                .with_context(|| format!("write {}", p.display()))?;
            println!("  [ok] compat marker written: {}", p.display());
        }
        return Ok(());
    }

    // --apply path: backup + run all pending.
    let backup = resolve_backup(&args)?;
    if backup.exists() && !args.force {
        bail!(
            "backup path already exists: {} (pass --force or pick a different --backup)",
            backup.display()
        );
    }
    std::fs::copy(&args.db, &backup)
        .with_context(|| format!("backup DB to {}", backup.display()))?;
    println!("  [ok] backup written: {}", backup.display());

    let applied = runner.up(None)?;
    println!("\n[apply] {} migration(s) applied:", applied.len());
    for a in &applied {
        println!("  + {}  ({})", a.version, a.description);
    }

    if let Some(p) = args.mapping_out.as_ref() {
        let dump = serde_json::json!({
            "db": args.db.display().to_string(),
            "applied": true,
            "note": "v0.7+ migrations do not expose per-row id mapping at the CLI boundary; this file is a compatibility marker.",
            "applied_now": applied.iter().map(|a| serde_json::json!({"version": a.version, "description": a.description})).collect::<Vec<_>>(),
        });
        std::fs::write(p, serde_json::to_string_pretty(&dump)?)
            .with_context(|| format!("write {}", p.display()))?;
        println!("  [ok] compat marker written: {}", p.display());
    }
    Ok(())
}

fn resolve_backup(args: &Args) -> Result<PathBuf> {
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

// Suppress "unused" lint for items that the v0.7 framework no longer needs
// here. Kept import paths anchored so future deprecation-tools find them.
#[allow(dead_code)]
fn _hold_runtime_refs() -> (HashMap<String, String>, &'static [&'static dyn persona_wire::migrations::Migration]) {
    (HashMap::new(), ALL)
}
