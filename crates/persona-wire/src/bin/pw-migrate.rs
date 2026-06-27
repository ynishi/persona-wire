//! `pw-migrate` — persona-wire schema migration CLI.
//!
//! Diesel / sqlx style runner over [`persona_wire::migrations`]. Each
//! subcommand operates on a single SQLite DB pointed to by `--db <path>`.
//!
//! Usage:
//!
//! ```sh
//! pw-migrate list                            # show every known migration + applied-ness
//! pw-migrate status --db <path>              # applied / pending vs the registry
//! pw-migrate up     --db <path>              # apply all pending (dry-run default)
//! pw-migrate up     --db <path> --apply      # actually mutate
//! pw-migrate up     --db <path> --apply --target 002_registry_id_ulid
//! pw-migrate apply  --db <path> --apply 002_registry_id_ulid
//! ```
//!
//! Safety:
//! - `--dry-run` is the default (no mutation, prints the plan).
//! - `--apply` is the explicit opt-in for writes. On `--apply` the DB is
//!   automatically backed up to `<db>.pre-migrate.bak` (override with
//!   `--backup <path>`).
//! - Each migration runs in its own `BEGIN IMMEDIATE` transaction with
//!   `PRAGMA foreign_keys = OFF`; a body failure rolls back that one
//!   migration only, the rest of the run stops.
//! - A `schema_migrations(version, description, applied_at)` table tracks
//!   applied ids so repeated invocations are idempotent.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;

use persona_wire::migrations::{Runner, Status, ALL};

#[derive(Debug)]
enum Cmd {
    List,
    Status {
        db: PathBuf,
    },
    Up {
        db: PathBuf,
        apply: bool,
        backup: Option<PathBuf>,
        target: Option<String>,
        force: bool,
    },
    Apply {
        db: PathBuf,
        apply: bool,
        backup: Option<PathBuf>,
        id: String,
        force: bool,
    },
}

fn parse_args() -> Result<Cmd> {
    let mut it = std::env::args().skip(1);
    let sub = it
        .next()
        .ok_or_else(|| anyhow!("missing subcommand (try --help)"))?;
    let rest: Vec<String> = it.collect();

    #[derive(Default)]
    struct ParsedOpts {
        db: Option<PathBuf>,
        apply: bool,
        backup: Option<PathBuf>,
        target: Option<String>,
        force: bool,
        positional: Option<String>,
    }

    let parse_opts = |rest: &[String]| -> Result<ParsedOpts> {
        let mut o = ParsedOpts::default();
        let mut i = 0;
        while i < rest.len() {
            let a = &rest[i];
            match a.as_str() {
                "--db" => {
                    i += 1;
                    o.db = Some(PathBuf::from(
                        rest.get(i).ok_or_else(|| anyhow!("--db requires path"))?,
                    ));
                }
                "--apply" => o.apply = true,
                "--dry-run" => o.apply = false,
                "--backup" => {
                    i += 1;
                    o.backup = Some(PathBuf::from(
                        rest.get(i)
                            .ok_or_else(|| anyhow!("--backup requires path"))?,
                    ));
                }
                "--target" => {
                    i += 1;
                    o.target = Some(
                        rest.get(i)
                            .ok_or_else(|| anyhow!("--target requires id"))?
                            .clone(),
                    );
                }
                "--force" => o.force = true,
                "-h" | "--help" => {
                    print_usage();
                    std::process::exit(0);
                }
                other if other.starts_with("--") => bail!("unknown flag: {other}"),
                other => {
                    if o.positional.is_some() {
                        bail!("unexpected positional arg: {other}");
                    }
                    o.positional = Some(other.to_string());
                }
            }
            i += 1;
        }
        Ok(o)
    };

    match sub.as_str() {
        "list" => Ok(Cmd::List),
        "status" => {
            let o = parse_opts(&rest)?;
            Ok(Cmd::Status {
                db: o
                    .db
                    .ok_or_else(|| anyhow!("status: --db <path> required"))?,
            })
        }
        "up" => {
            let o = parse_opts(&rest)?;
            Ok(Cmd::Up {
                db: o.db.ok_or_else(|| anyhow!("up: --db <path> required"))?,
                apply: o.apply,
                backup: o.backup,
                target: o.target,
                force: o.force,
            })
        }
        "apply" => {
            let o = parse_opts(&rest)?;
            Ok(Cmd::Apply {
                db: o.db.ok_or_else(|| anyhow!("apply: --db <path> required"))?,
                apply: o.apply,
                backup: o.backup,
                id: o
                    .positional
                    .ok_or_else(|| anyhow!("apply: <migration-id> required"))?,
                force: o.force,
            })
        }
        "-h" | "--help" => {
            print_usage();
            std::process::exit(0);
        }
        other => bail!("unknown subcommand: {other} (try --help)"),
    }
}

fn print_usage() {
    println!("Usage: pw-migrate <subcommand> [options]");
    println!();
    println!("Subcommands:");
    println!("  list                                       — show every known migration");
    println!("  status --db <path>                         — applied / pending vs registry");
    println!("  up     --db <path> [--apply] [--target ID] [--backup <path>] [--force]");
    println!("  apply  --db <path> [--apply] [--backup <path>] [--force] <id>");
    println!();
    println!("Defaults to --dry-run; pass --apply to mutate. Backup on --apply is");
    println!("mandatory (default: <db>.pre-migrate.bak; existing backup needs --force).");
}

fn main() -> Result<()> {
    let cmd = parse_args()?;
    match cmd {
        Cmd::List => list(),
        Cmd::Status { db } => status(&db),
        Cmd::Up {
            db,
            apply,
            backup,
            target,
            force,
        } => up(&db, apply, backup, target.as_deref(), force),
        Cmd::Apply {
            db,
            apply,
            backup,
            id,
            force,
        } => apply_one(&db, apply, backup, &id, force),
    }
}

fn list() -> Result<()> {
    println!("== known migrations (in execution order) ==");
    for m in ALL {
        println!("  {}  {}", m.id(), m.description());
    }
    Ok(())
}

fn status(db: &PathBuf) -> Result<()> {
    let conn = open(db)?;
    let runner = Runner::new(&conn);
    let status = runner.status()?;
    print_status(&status);
    Ok(())
}

fn print_status(s: &Status) {
    println!("== applied ==");
    if s.applied.is_empty() {
        println!("  (none)");
    } else {
        for r in &s.applied {
            println!(
                "  [✓] {}  applied_at={}  ({})",
                r.version, r.applied_at, r.description
            );
        }
    }
    println!("\n== pending ==");
    if s.pending.is_empty() {
        println!("  (none — schema is up to date)");
    } else {
        for m in &s.pending {
            println!("  [ ] {}  ({})", m.id(), m.description());
        }
    }
}

fn up(
    db: &PathBuf,
    apply: bool,
    backup: Option<PathBuf>,
    target: Option<&str>,
    force: bool,
) -> Result<()> {
    let conn = open(db)?;
    let runner = Runner::new(&conn);
    let status = runner.status()?;
    print_status(&status);
    if status.pending.is_empty() {
        return Ok(());
    }
    if !apply {
        println!(
            "\n[dry-run] {} migration(s) would be applied. Re-run with --apply to commit.",
            status.pending.len()
        );
        return Ok(());
    }
    backup_db(db, backup.as_deref(), force)?;
    let applied = runner.up(target)?;
    println!("\n[apply] {} migration(s) applied:", applied.len());
    for a in &applied {
        println!("  + {}  ({})", a.version, a.description);
    }
    Ok(())
}

fn apply_one(
    db: &PathBuf,
    apply: bool,
    backup: Option<PathBuf>,
    id: &str,
    force: bool,
) -> Result<()> {
    let conn = open(db)?;
    let runner = Runner::new(&conn);
    if !apply {
        let s = runner.status()?;
        let pending = s.pending.iter().any(|m| m.id() == id);
        if !pending {
            println!("[dry-run] migration '{id}' is either already applied or not registered.");
        } else {
            println!("[dry-run] would apply: {id}. Re-run with --apply to commit.");
        }
        return Ok(());
    }
    backup_db(db, backup.as_deref(), force)?;
    runner.apply(id)?;
    println!("[apply] applied: {id}");
    Ok(())
}

fn open(db: &PathBuf) -> Result<Connection> {
    if !db.exists() {
        bail!("--db path does not exist: {}", db.display());
    }
    Connection::open(db).with_context(|| format!("open {}", db.display()))
}

fn backup_db(db: &PathBuf, backup: Option<&std::path::Path>, force: bool) -> Result<()> {
    let dest = match backup {
        Some(p) => p.to_path_buf(),
        None => {
            let mut p = db.clone();
            let fname = p
                .file_name()
                .ok_or_else(|| anyhow!("--db has no file name component"))?
                .to_string_lossy()
                .into_owned();
            p.set_file_name(format!("{fname}.pre-migrate.bak"));
            p
        }
    };
    if dest.exists() && !force {
        bail!(
            "backup already exists: {} (pass --force to overwrite, or pick a different --backup)",
            dest.display()
        );
    }
    std::fs::copy(db, &dest).with_context(|| format!("backup to {}", dest.display()))?;
    println!("  [ok] backup written: {}", dest.display());
    Ok(())
}
