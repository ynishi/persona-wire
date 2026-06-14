//! persona-wire CLI entry point.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "persona-wire", version, about = "persona-wire CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize a new wire store (SQLite) at the given path.
    Init {
        #[arg(long, default_value = "./persona-wire.db")]
        db: String,
    },
    /// Run a registered named projection.
    Project {
        name: String,
    },
    /// Node low-level CRUD.
    Node {
        #[command(subcommand)]
        op: NodeOp,
    },
    /// Edge low-level CRUD.
    Edge {
        #[command(subcommand)]
        op: EdgeOp,
    },
}

#[derive(Subcommand, Debug)]
enum NodeOp {
    Create { r#type: String },
    Get { id: String },
    List,
}

#[derive(Subcommand, Debug)]
enum EdgeOp {
    Create { src: String, tgt: String, kind: String },
    List,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Init { db } => {
            tracing::info!(?db, "init wire store (skeleton)");
            // TODO(P1): SqliteStorage::open(&db)?.migrate()?;
        }
        Command::Project { name } => {
            tracing::info!(?name, "run projection (skeleton)");
        }
        Command::Node { op } => {
            tracing::info!(?op, "node op (skeleton)");
        }
        Command::Edge { op } => {
            tracing::info!(?op, "edge op (skeleton)");
        }
    }

    Ok(())
}
