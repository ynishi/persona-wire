//! persona-wire CLI entry point.
//!
//! Wraps the engine layer (persona-wire-core) for local invocation.

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use persona_wire_core::application::projection_registry::{
    NamedProjection, ProjectionRegistry, TargetForm,
};
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    wire_close, wire_doctor, wire_init, WireCloseInput, WireInitInput,
};
use persona_wire_core::domain::graph::{Edge, Node};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;

const DEFAULT_DB: &str = "./persona-wire.db";

#[derive(Parser, Debug)]
#[command(name = "persona-wire", version, about = "persona-wire CLI")]
struct Cli {
    /// Path to the SQLite db (created if absent, except for `init` which always creates).
    #[arg(long, global = true, default_value = DEFAULT_DB)]
    db: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize a fresh wire store (migrate + seed default 18 type vocabulary).
    Init,

    /// List registered type vocabulary.
    Types {
        /// Optional kind filter: `node` or `edge`.
        #[arg(long)]
        kind: Option<String>,
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

    /// Specification registry (dynamic-axis query objects).
    Spec {
        #[command(subcommand)]
        op: SpecOp,
    },

    /// NamedProjection registry (fixed-axis query + template).
    Projection {
        #[command(subcommand)]
        op: ProjectionOp,
    },

    /// `wire_init` use case — render every registered projection.
    WireInit(WireInitArgs),

    /// `wire_close` use case — lifecycle scan report.
    WireClose(WireCloseArgs),

    /// `wire_doctor` use case — graph-wide health diagnostic (orphan + totals).
    WireDoctor,

    /// Boot the stdio MCP server (delegates to persona-wire-mcp::serve_stdio).
    Mcp,
}

#[derive(Subcommand, Debug)]
enum NodeOp {
    /// Create a node.
    Create {
        #[arg(long)]
        id: String,
        #[arg(long = "type")]
        type_: String,
        /// Optional JSON metadata. Defaults to `{}`.
        #[arg(long, default_value = "{}")]
        metadata: String,
        /// Optional SoT ref (e.g. `pp://alpha`).
        #[arg(long)]
        sot_ref: Option<String>,
    },
    /// Get a node by id.
    Get {
        #[arg(long)]
        id: String,
    },
    /// List nodes of a given type.
    List {
        #[arg(long = "type")]
        type_: String,
    },
}

#[derive(Subcommand, Debug)]
enum EdgeOp {
    /// Create an edge.
    Create {
        #[arg(long)]
        id: String,
        #[arg(long)]
        src: String,
        #[arg(long)]
        tgt: String,
        #[arg(long)]
        kind: String,
        /// Optional severity {hard|soft|advisory} (edge `triggers_review_of` typically).
        #[arg(long)]
        severity: Option<String>,
        #[arg(long, default_value = "{}")]
        metadata: String,
    },
    /// List edges leaving `src`.
    From {
        #[arg(long)]
        src: String,
    },
}

#[derive(Subcommand, Debug)]
enum SpecOp {
    /// Register a Specification (JSON-serialised expression body).
    Register {
        #[arg(long)]
        name: String,
        /// Specification body (JSON-serialised). Example: `{"TypeIs":"persona"}`.
        #[arg(long = "spec", alias = "json")]
        spec: String,
    },
    /// Get a registered Specification (printed as JSON).
    Get {
        #[arg(long)]
        name: String,
    },
    /// List registered Specifications.
    List,
}

#[derive(Subcommand, Debug)]
enum ProjectionOp {
    /// Register a NamedProjection.
    Register {
        #[arg(long)]
        name: String,
        #[arg(long = "spec-ref")]
        spec_ref: String,
        #[arg(long)]
        template: String,
        /// Target form: prompt | markdown | json | ascii.
        #[arg(long = "target-form")]
        target_form: String,
    },
    /// Get a registered NamedProjection.
    Get {
        #[arg(long)]
        name: String,
    },
    /// List registered NamedProjections.
    List,
}

#[derive(Args, Debug)]
struct WireInitArgs {
    #[arg(long = "persona-id", alias = "persona")]
    persona_id: String,
}

#[derive(Args, Debug)]
struct WireCloseArgs {
    #[arg(long = "persona-id", alias = "persona")]
    persona_id: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // `mcp` subcommand uses stdout for JSON-RPC framing, so route tracing to
    // stderr in that mode. Other subcommands keep default (stdout).
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if matches!(cli.command, Command::Mcp) {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    match cli.command {
        Command::Init => {
            let s = SqliteStorage::open(&cli.db).with_context(|| format!("open db: {}", cli.db))?;
            s.migrate().context("migrate")?;
            s.seed_default_types().context("seed default types")?;
            println!("initialized: db={} (9 node + 9 edge types seeded)", cli.db);
        }

        Command::Types { kind } => {
            let s = SqliteStorage::open(&cli.db)?;
            let entries = match kind.as_deref() {
                Some("node") => s
                    .list_types_by_kind("node")?
                    .into_iter()
                    .map(|n| (n, "node".to_string()))
                    .collect(),
                Some("edge") => s
                    .list_types_by_kind("edge")?
                    .into_iter()
                    .map(|n| (n, "edge".to_string()))
                    .collect(),
                Some(other) => anyhow::bail!("unknown kind: {other}"),
                None => s.list_types()?,
            };
            for (name, kind) in entries {
                println!("{kind:6}  {name}");
            }
        }

        Command::Node { op } => match op {
            NodeOp::Create {
                id,
                type_,
                metadata,
                sot_ref,
            } => {
                let s = SqliteStorage::open(&cli.db)?;
                let meta: serde_json::Value =
                    serde_json::from_str(&metadata).context("parse --metadata as JSON")?;
                let node = Node {
                    id: id.clone(),
                    r#type: type_,
                    sot_ref,
                    confidence: None,
                    applicability: None,
                    last_verified_at: None,
                    review_due: None,
                    version: 1,
                    prev_id: None,
                    metadata: meta,
                };
                s.insert_node(&node)?;
                println!("created node: {id}");
            }
            NodeOp::Get { id } => {
                let s = SqliteStorage::open(&cli.db)?;
                match s.get_node(&id)? {
                    Some(n) => println!("{}", serde_json::to_string_pretty(&n)?),
                    None => {
                        eprintln!("not found: {id}");
                        std::process::exit(1);
                    }
                }
            }
            NodeOp::List { type_ } => {
                let s = SqliteStorage::open(&cli.db)?;
                let nodes = s.list_nodes_by_type(&type_)?;
                println!("{}", serde_json::to_string_pretty(&nodes)?);
            }
        },

        Command::Edge { op } => match op {
            EdgeOp::Create {
                id,
                src,
                tgt,
                kind,
                severity,
                metadata,
            } => {
                let s = SqliteStorage::open(&cli.db)?;
                let meta: serde_json::Value =
                    serde_json::from_str(&metadata).context("parse --metadata as JSON")?;
                let sev = severity.as_deref().map(parse_severity).transpose()?;
                let edge = Edge {
                    id: id.clone(),
                    src_node: src,
                    tgt_node: tgt,
                    kind,
                    severity: sev,
                    metadata: meta,
                    version: 1,
                    prev_id: None,
                };
                s.insert_edge(&edge)?;
                println!("created edge: {id}");
            }
            EdgeOp::From { src } => {
                let s = SqliteStorage::open(&cli.db)?;
                let edges = s.list_edges_from(&src)?;
                println!("{}", serde_json::to_string_pretty(&edges)?);
            }
        },

        Command::Spec { op } => match op {
            SpecOp::Register { name, spec } => {
                let s = SqliteStorage::open(&cli.db)?;
                let spec: Specification =
                    serde_json::from_str(&spec).context("parse --spec as Specification")?;
                SpecRegistry::new(&s).register(&name, &spec)?;
                println!("registered spec: {name}");
            }
            SpecOp::Get { name } => {
                let s = SqliteStorage::open(&cli.db)?;
                match SpecRegistry::new(&s).get(&name)? {
                    Some(spec) => println!("{}", serde_json::to_string_pretty(&spec)?),
                    None => {
                        eprintln!("not found: {name}");
                        std::process::exit(1);
                    }
                }
            }
            SpecOp::List => {
                let s = SqliteStorage::open(&cli.db)?;
                for n in SpecRegistry::new(&s).list()? {
                    println!("{n}");
                }
            }
        },

        Command::Projection { op } => match op {
            ProjectionOp::Register {
                name,
                spec_ref,
                template,
                target_form,
            } => {
                let s = SqliteStorage::open(&cli.db)?;
                let tf = TargetForm::parse(&target_form)?;
                ProjectionRegistry::new(&s).register(&NamedProjection {
                    name: name.clone(),
                    spec_ref,
                    template,
                    target_form: tf,
                })?;
                println!("registered projection: {name}");
            }
            ProjectionOp::Get { name } => {
                let s = SqliteStorage::open(&cli.db)?;
                match ProjectionRegistry::new(&s).get(&name)? {
                    Some(p) => println!("{}", serde_json::to_string_pretty(&p)?),
                    None => {
                        eprintln!("not found: {name}");
                        std::process::exit(1);
                    }
                }
            }
            ProjectionOp::List => {
                let s = SqliteStorage::open(&cli.db)?;
                for n in ProjectionRegistry::new(&s).list()? {
                    println!("{n}");
                }
            }
        },

        Command::WireInit(args) => {
            let s = SqliteStorage::open(&cli.db)?;
            let out = wire_init(
                WireInitInput {
                    persona_id: args.persona_id,
                },
                &s,
            )?;
            for w in &out.warnings {
                eprintln!("warn: {w}");
            }
            for p in &out.projections {
                println!("=== {} ({}) ===", p.name, p.target_form.as_str());
                println!("{}", p.rendered);
            }
        }

        Command::WireClose(args) => {
            let s = SqliteStorage::open(&cli.db)?;
            let out = wire_close(
                WireCloseInput {
                    persona_id: args.persona_id,
                },
                &s,
            )?;
            println!("{}", out.report_markdown);
        }

        Command::WireDoctor => {
            let s = SqliteStorage::open(&cli.db)?;
            let out = wire_doctor(&s)?;
            println!("{}", out.report_markdown);
        }

        Command::Mcp => {
            let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
            rt.block_on(persona_wire_mcp::serve_stdio(&cli.db))?;
        }
    }

    Ok(())
}

fn parse_severity(s: &str) -> Result<persona_wire_core::domain::graph::Severity> {
    use persona_wire_core::domain::graph::Severity;
    match s {
        "hard" => Ok(Severity::Hard),
        "soft" => Ok(Severity::Soft),
        "advisory" => Ok(Severity::Advisory),
        other => anyhow::bail!("unknown severity: {other} (expected hard|soft|advisory)"),
    }
}
