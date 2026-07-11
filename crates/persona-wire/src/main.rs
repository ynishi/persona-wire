//! persona-wire CLI entry point.
//!
//! Wraps the engine layer (persona-wire-core) for local invocation.

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use persona_wire_adapter_mini_app::MiniAppAdapter;
use persona_wire_adapter_obsidian::ObsidianAdapter;
use persona_wire_adapter_persona_pack::PersonaPackAdapter;
use persona_wire_adapter_sqlite_x::SqliteAdapter;
use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::application::projection_mapper::projection_to_dto;
use persona_wire_core::application::projection_registry::ProjectionRegistry;
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    wire_close, wire_doctor, wire_init, wire_query, wire_render, WireCloseInput, WireInitInput,
    WireQueryInput, WireRenderInput,
};
use persona_wire_core::domain::entity::projection::{PluginDispatch, Projection};
use persona_wire_core::domain::entity::TargetForm;
use persona_wire_core::domain::graph::{Edge, Node};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::{default_db_path, SqliteStorage};
use persona_wire_credentials::{
    Credentials, KeyringTokenProvider, MutableTokenProvider, ALIAS_ENV_VARS,
};
use std::io::IsTerminal;

#[derive(Parser, Debug)]
#[command(name = "persona-wire", version, about = "persona-wire CLI")]
struct Cli {
    /// Path to the SQLite db (created if absent, except for `init` which always creates).
    /// Resolution order: env `PERSONA_WIRE_DB` > this `--db` flag > OS data dir
    /// fallback (`$XDG_DATA_HOME/persona-wire/store.db` or `$HOME/.persona-wire/store.db`).
    #[arg(long, global = true)]
    db: Option<String>,

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

    /// Specification registry (dynamic / composable selector).
    Spec {
        #[command(subcommand)]
        op: SpecOp,
    },

    /// NamedProjection registry (fixed / named view: query + template).
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

    /// `wire_query` use case — ad-hoc Specification query (slim node list).
    /// Provide either `--spec <json>` (inline) or `--spec-ref <name>` (registered).
    Query(QueryArgs),

    /// `wire_render` use case — render a single registered NamedProjection by name.
    Render(RenderArgs),

    /// Boot the stdio MCP server (delegates to persona-wire-mcp::serve_stdio).
    Mcp,

    /// Bundle scaffolding installer — register / list / get / install / delete
    /// a TOML bundle of Spec / Projection / Wiring / Workflow / Node / Edge.
    Bundle {
        #[command(subcommand)]
        op: BundleOp,
    },

    /// Service API token management (keyring-backed; env vars take precedence).
    Token {
        #[command(subcommand)]
        op: TokenOp,
    },
}

#[derive(Subcommand, Debug)]
enum TokenOp {
    /// Store a token in the OS keyring. Reads the token from stdin
    /// (pipe it in, or paste at the prompt — input echoes on a TTY).
    Set {
        /// Service name (e.g. `github`).
        service: String,
    },
    /// Remove a token from the OS keyring (idempotent).
    Rm {
        /// Service name (e.g. `github`).
        service: String,
    },
    /// Show which provider (env / keyring / none) supplies each service's
    /// token. Never prints token values.
    Status {
        /// Optional service name filter. Omit to list every known service.
        service: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum BundleOp {
    /// Register a bundle from a TOML file. The [bundle] table inside the
    /// TOML is the source-of-truth for `name` / `version` / `description`.
    Register {
        /// Path to the bundle TOML file.
        #[arg(long)]
        file: String,
    },
    /// List registered bundles (name-ascending, summary rows).
    List,
    /// Get a registered bundle (full TOML body) by name or ULID.
    Get {
        #[arg(long = "ref", alias = "ref_")]
        r#ref: String,
    },
    /// Install a registered bundle.
    Install {
        #[arg(long = "ref", alias = "ref_")]
        r#ref: String,
        /// Conflict mode: increment (default, auto-suffix) / skip / error.
        #[arg(long, default_value = "increment")]
        mode: String,
    },
    /// Delete a registered bundle by name or ULID. Install history is
    /// preserved.
    Delete {
        #[arg(long = "ref", alias = "ref_")]
        r#ref: String,
    },
}

#[derive(Subcommand, Debug)]
enum NodeOp {
    /// Create a node. The server mints the opaque ULID id and returns it.
    Create {
        /// Human-readable label (no uniqueness constraint).
        #[arg(long)]
        name: String,
        #[arg(long = "type")]
        type_: String,
        /// Optional JSON metadata. Defaults to `{}`.
        #[arg(long, default_value = "{}")]
        metadata: String,
        /// Optional SoT ref (e.g. `pp://alpha`).
        #[arg(long)]
        sot_ref: Option<String>,
    },
    /// Get a node by ULID id or by name.
    Get {
        /// Accepts either the 26-char ULID or the human-readable name.
        #[arg(long = "id-or-name", alias = "id")]
        id_or_name: String,
    },
    /// List nodes of a given type.
    List {
        #[arg(long = "type")]
        type_: String,
    },
    /// Patch a node's metadata in place (merge or replace).
    Update {
        #[arg(long = "id-or-name", alias = "id")]
        id_or_name: String,
        /// JSON object patch. In `merge` mode (default), top-level keys
        /// overwrite existing metadata; `null` deletes the matching key
        /// (RFC 7396). In `replace` mode, the existing metadata is replaced
        /// wholesale.
        #[arg(long = "metadata-patch")]
        metadata_patch: String,
        /// One of `merge` (default) or `replace`.
        #[arg(long, default_value = "merge")]
        mode: String,
    },
}

#[derive(Subcommand, Debug)]
enum EdgeOp {
    /// Create an edge. The server mints the opaque ULID id.
    Create {
        /// Optional human-readable label.
        #[arg(long)]
        name: Option<String>,
        /// Source endpoint — ULID or node name.
        #[arg(long)]
        src: String,
        /// Target endpoint — ULID or node name.
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
    /// List edges leaving `src` (ULID or node name).
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

#[derive(Args, Debug)]
struct RenderArgs {
    /// Name of a registered NamedProjection to render.
    #[arg(long = "projection-ref")]
    projection_ref: String,
}

#[derive(Args, Debug)]
struct QueryArgs {
    /// Inline Specification body (JSON). Mutually exclusive with `--spec-ref`.
    /// Example: `--spec '{"TypeIs":"persona"}'`.
    #[arg(long, conflicts_with = "spec_ref")]
    spec: Option<String>,
    /// Name of a previously registered Specification. Mutually exclusive with `--spec`.
    #[arg(long = "spec-ref")]
    spec_ref: Option<String>,
    /// Maximum matches to return. Default: env `PERSONA_WIRE_QUERY_LIMIT` if set, else unlimited.
    #[arg(long)]
    limit: Option<usize>,
    /// Number of leading matches to skip (pagination).
    #[arg(long)]
    offset: Option<usize>,
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

    // Resolve DB path: env > CLI flag > OS data dir fallback.
    let db: String = if let Ok(env_path) = std::env::var("PERSONA_WIRE_DB") {
        env_path
    } else if let Some(flag) = cli.db.clone() {
        flag
    } else {
        let p = default_db_path().context("resolve default db path")?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).context("create db parent dir")?;
        }
        p.to_string_lossy().into_owned()
    };

    match cli.command {
        Command::Init => {
            let s = SqliteStorage::open(&db).with_context(|| format!("open db: {db}"))?;
            s.migrate().context("migrate")?;
            s.seed_default_types().context("seed default types")?;
            println!("initialized: db={db} (9 node + 9 edge types seeded)");
        }

        Command::Types { kind } => {
            let s = SqliteStorage::open(&db)?;
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
                name,
                type_,
                metadata,
                sot_ref,
            } => {
                let s = SqliteStorage::open(&db)?;
                let meta: serde_json::Value =
                    serde_json::from_str(&metadata).context("parse --metadata as JSON")?;
                let id = persona_wire_core::domain::graph::Ulid::new();
                let node = Node {
                    id,
                    name: name.clone(),
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
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": id.to_string(),
                        "name": name,
                    }))?
                );
            }
            NodeOp::Get { id_or_name } => {
                let s = SqliteStorage::open(&db)?;
                let resolved = s
                    .resolve_node_id_or_name(&id_or_name)?
                    .ok_or_else(|| anyhow::anyhow!("not found: {id_or_name}"))?;
                match s.get_node(&resolved)? {
                    Some(n) => println!("{}", serde_json::to_string_pretty(&n)?),
                    None => {
                        eprintln!("not found: {id_or_name}");
                        std::process::exit(1);
                    }
                }
            }
            NodeOp::List { type_ } => {
                let s = SqliteStorage::open(&db)?;
                let nodes = s.list_nodes_by_type(&type_)?;
                println!("{}", serde_json::to_string_pretty(&nodes)?);
            }
            NodeOp::Update {
                id_or_name,
                metadata_patch,
                mode,
            } => {
                use persona_wire_core::application::use_cases::{
                    wire_node_update, WireNodeUpdateInput, WireNodeUpdateMode,
                };
                let s = SqliteStorage::open(&db)?;
                let mode = WireNodeUpdateMode::parse(&mode)?;
                let patch: serde_json::Value = serde_json::from_str(&metadata_patch)
                    .context("parse --metadata-patch as JSON")?;
                let out = wire_node_update(
                    WireNodeUpdateInput {
                        id: id_or_name,
                        metadata_patch: patch,
                        mode,
                    },
                    &s,
                )?;
                let json = serde_json::json!({
                    "id": out.id,
                    "mode": out.mode.as_str(),
                    "metadata": out.metadata,
                });
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
        },

        Command::Edge { op } => match op {
            EdgeOp::Create {
                name,
                src,
                tgt,
                kind,
                severity,
                metadata,
            } => {
                let s = SqliteStorage::open(&db)?;
                let meta: serde_json::Value =
                    serde_json::from_str(&metadata).context("parse --metadata as JSON")?;
                let sev = severity.as_deref().map(parse_severity).transpose()?;
                let src_id = s
                    .resolve_node_id_or_name(&src)?
                    .ok_or_else(|| anyhow::anyhow!("edge src node not found: {src}"))?;
                let tgt_id = s
                    .resolve_node_id_or_name(&tgt)?
                    .ok_or_else(|| anyhow::anyhow!("edge tgt node not found: {tgt}"))?;
                let id = persona_wire_core::domain::graph::Ulid::new();
                let edge = Edge {
                    id,
                    name: name.clone(),
                    src_node: src_id,
                    tgt_node: tgt_id,
                    kind,
                    severity: sev,
                    metadata: meta,
                    version: 1,
                    prev_id: None,
                };
                s.insert_edge(&edge)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "id": id.to_string(),
                        "name": name,
                    }))?
                );
            }
            EdgeOp::From { src } => {
                let s = SqliteStorage::open(&db)?;
                let src_id = s
                    .resolve_node_id_or_name(&src)?
                    .ok_or_else(|| anyhow::anyhow!("node not found: {src}"))?;
                let edges = s.list_edges_from(&src_id)?;
                println!("{}", serde_json::to_string_pretty(&edges)?);
            }
        },

        Command::Spec { op } => match op {
            SpecOp::Register { name, spec } => {
                let s = SqliteStorage::open(&db)?;
                let spec: Specification =
                    serde_json::from_str(&spec).context("parse --spec as Specification")?;
                SpecRegistry::new(&s).register(&name, &spec)?;
                println!("registered spec: {name}");
            }
            SpecOp::Get { name } => {
                let s = SqliteStorage::open(&db)?;
                match SpecRegistry::new(&s).get(&name)? {
                    Some(spec) => println!("{}", serde_json::to_string_pretty(&spec)?),
                    None => {
                        eprintln!("not found: {name}");
                        std::process::exit(1);
                    }
                }
            }
            SpecOp::List => {
                let s = SqliteStorage::open(&db)?;
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
                let s = SqliteStorage::open(&db)?;
                let tf = TargetForm::parse(&target_form)?;
                // P3a Phase 2 (a) — CLI `projection register` does not yet
                // accept the 3 Plugin hint fields; Phase 2 (c) will expose
                // them via additional flags (PluginDispatch::Custom).
                let entity = Projection::from_parts(
                    name.clone(),
                    spec_ref,
                    template,
                    tf,
                    PluginDispatch::Default,
                )?;
                ProjectionRegistry::new(&s).register(&entity)?;
                println!("registered projection: {name}");
            }
            ProjectionOp::Get { name } => {
                let s = SqliteStorage::open(&db)?;
                match ProjectionRegistry::new(&s).get(&name)? {
                    Some(p) => {
                        println!("{}", serde_json::to_string_pretty(&projection_to_dto(&p))?)
                    }
                    None => {
                        eprintln!("not found: {name}");
                        std::process::exit(1);
                    }
                }
            }
            ProjectionOp::List => {
                let s = SqliteStorage::open(&db)?;
                for n in ProjectionRegistry::new(&s).list()? {
                    println!("{n}");
                }
            }
        },

        Command::WireInit(args) => {
            let s = SqliteStorage::open(&db)?;
            let registry = PluginRegistry::default_builder_for_wire()
                .with_adapter(MiniAppAdapter)
                .with_adapter(SqliteAdapter)
                .with_adapter(ObsidianAdapter)
                .with_adapter(PersonaPackAdapter::from_env()?)
                .build()?;
            let out = wire_init(
                WireInitInput {
                    persona_id: args.persona_id,
                },
                &s,
                &registry,
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
            let s = SqliteStorage::open(&db)?;
            let out = wire_close(
                WireCloseInput {
                    persona_id: args.persona_id,
                },
                &s,
            )?;
            println!("{}", out.report_markdown);
        }

        Command::WireDoctor => {
            let s = SqliteStorage::open(&db)?;
            let registry = PluginRegistry::default_builder_for_wire()
                .with_adapter(MiniAppAdapter)
                .with_adapter(SqliteAdapter)
                .with_adapter(ObsidianAdapter)
                .with_adapter(PersonaPackAdapter::from_env()?)
                .build()?;
            let out = wire_doctor(&s, None, &registry)?;
            println!("{}", out.report_markdown);
        }

        Command::Query(args) => {
            let s = SqliteStorage::open(&db)?;
            // env precedence for `limit`: --limit flag > env PERSONA_WIRE_QUERY_LIMIT > None (unlimited).
            let limit: Option<usize> = match args.limit {
                Some(n) => Some(n),
                None => std::env::var("PERSONA_WIRE_QUERY_LIMIT")
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok()),
            };
            let spec = match args.spec.as_deref() {
                Some(body) => Some(
                    serde_json::from_str::<Specification>(body)
                        .context("parse --spec as Specification")?,
                ),
                None => None,
            };
            let out = wire_query(
                WireQueryInput {
                    spec,
                    spec_ref: args.spec_ref,
                    limit,
                    offset: args.offset,
                },
                &s,
            )?;
            let json = serde_json::json!({
                "matched": out.matched.iter().map(|n| serde_json::json!({
                    "id": n.id,
                    "type": n.r#type,
                    "metadata": n.metadata,
                })).collect::<Vec<_>>(),
                "total_count": out.total_count,
                "returned_count": out.returned_count,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }

        Command::Render(args) => {
            let s = SqliteStorage::open(&db)?;
            let registry = PluginRegistry::default_builder_for_wire()
                .with_adapter(MiniAppAdapter)
                .with_adapter(SqliteAdapter)
                .with_adapter(ObsidianAdapter)
                .with_adapter(PersonaPackAdapter::from_env()?)
                .build()?;
            let out = wire_render(
                WireRenderInput {
                    projection_ref: args.projection_ref,
                },
                &s,
                &registry,
            )?;
            let json = serde_json::json!({
                "name": out.name,
                "target_form": out.target_form.as_str(),
                "rendered": out.rendered,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
        }

        Command::Mcp => {
            let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
            rt.block_on(persona_wire_mcp::serve_stdio(&db))?;
        }

        Command::Bundle { op } => bundle_op(op, &db)?,

        Command::Token { op } => token_op(op)?,
    }

    Ok(())
}

/// Dispatch for `persona-wire token <op>`. Never touches the wire store —
/// token management is orthogonal to the graph db.
fn token_op(op: TokenOp) -> Result<()> {
    match op {
        TokenOp::Set { service } => {
            let stdin = std::io::stdin();
            let raw = if stdin.is_terminal() {
                rpassword::prompt_password(format!("Token for {service}: "))
                    .with_context(|| format!("read token for '{service}' from tty"))?
            } else {
                read_token(stdin.lock())
                    .with_context(|| format!("read token for '{service}' from stdin"))?
            };
            let token = raw.trim().to_string();
            if token.is_empty() {
                anyhow::bail!("empty token");
            }
            KeyringTokenProvider
                .set(&service, &token)
                .with_context(|| format!("store token for '{service}' in OS keyring"))?;
            println!("stored token for '{service}' in OS keyring");
        }
        TokenOp::Rm { service } => {
            KeyringTokenProvider
                .delete(&service)
                .with_context(|| format!("delete token for '{service}' from OS keyring"))?;
            println!("removed token for '{service}' from OS keyring (if present)");
        }
        TokenOp::Status { service } => {
            let creds = Credentials::default_chain();
            let services: Vec<String> = match service {
                Some(s) => vec![s],
                None => ALIAS_ENV_VARS
                    .iter()
                    .map(|(svc, _)| svc.to_string())
                    .collect(),
            };
            for svc in services {
                let source = creds
                    .resolve_source(&svc)
                    .with_context(|| format!("resolve token source for '{svc}'"))?;
                println!("{}", format_token_status(&svc, source));
            }
        }
    }
    Ok(())
}

/// Read one line from `r`, trim trailing newline/whitespace, and reject an
/// empty result. Never logs or echoes the value itself (caller controls the
/// TTY prompt, if any).
fn read_token<R: std::io::BufRead>(mut r: R) -> Result<String> {
    let mut line = String::new();
    r.read_line(&mut line).context("read line from stdin")?;
    let token = line.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("empty token (nothing read from stdin)");
    }
    Ok(token)
}

/// Render one `token status` line for `service`, given the resolved provider
/// name (or `None` if no provider supplies a token). Never includes the
/// token value.
fn format_token_status(service: &str, source: Option<&str>) -> String {
    format!("{service}: {}", source.unwrap_or("none"))
}

fn bundle_op(op: BundleOp, db: &str) -> Result<()> {
    use persona_wire_core::application::bundle_install::install_bundle;
    use persona_wire_core::application::bundle_registry::BundleRegistry;
    use persona_wire_core::domain::entity::bundle::{
        BundleName, BundleRef, BundleVersion, ConflictMode,
    };
    let s = SqliteStorage::open(db)?;
    let reg = BundleRegistry::new(&s);
    match op {
        BundleOp::Register { file } => {
            let body = std::fs::read_to_string(&file)
                .with_context(|| format!("read bundle file: {file}"))?;
            let value: toml::Value = toml::from_str(&body).context("parse bundle TOML")?;
            let bundle_tbl = value
                .get("bundle")
                .and_then(|v| v.as_table())
                .ok_or_else(|| anyhow::anyhow!("missing [bundle] table"))?;
            let name = bundle_tbl
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing [bundle].name"))?;
            let version = bundle_tbl
                .get("version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing [bundle].version"))?;
            let description = bundle_tbl
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let bn = BundleName::new(name.to_string())?;
            let bv = BundleVersion::new(version.to_string())?;
            let id = reg.register(&bn, &bv, description.as_deref(), &body)?;
            println!("registered bundle: {} (id={})", name, id);
        }
        BundleOp::List => {
            for b in reg.list()? {
                println!(
                    "{}\t{}\t{}\t{}",
                    b.id,
                    b.name,
                    b.version,
                    b.description.unwrap_or_default()
                );
            }
        }
        BundleOp::Get { r#ref } => {
            let r = BundleRef::parse(&r#ref)?;
            match reg.resolve(&r)? {
                Some(b) => {
                    let json = serde_json::json!({
                        "id": b.id.to_string(),
                        "name": b.name.as_str(),
                        "version": b.version.as_str(),
                        "description": b.description,
                        "body": b.body,
                        "created_at": b.created_at,
                        "updated_at": b.updated_at,
                    });
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                None => {
                    eprintln!("not found: {ref_}", ref_ = r#ref);
                    std::process::exit(1);
                }
            }
        }
        BundleOp::Install { r#ref, mode } => {
            let m = ConflictMode::parse(&mode)?;
            let r = BundleRef::parse(&r#ref)?;
            let bundle = reg
                .resolve(&r)?
                .ok_or_else(|| anyhow::anyhow!("bundle not found: {}", r#ref))?;
            let report = install_bundle(&bundle, m, &s)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        BundleOp::Delete { r#ref } => {
            let r = BundleRef::parse(&r#ref)?;
            let deleted = match r {
                BundleRef::Id(id) => reg.delete_by_id(id)?,
                BundleRef::Name(name) => reg.delete(&name)?,
            };
            println!("{}", serde_json::json!({ "deleted": deleted }));
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

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn read_token_trims_trailing_newline() {
        let got = read_token("secret-tok\n".as_bytes()).unwrap();
        assert_eq!(got, "secret-tok");
    }

    #[test]
    fn read_token_trims_surrounding_whitespace() {
        let got = read_token("  secret-tok  \n".as_bytes()).unwrap();
        assert_eq!(got, "secret-tok");
    }

    #[test]
    fn read_token_rejects_empty_input() {
        let err = read_token("\n".as_bytes()).unwrap_err();
        assert!(err.to_string().contains("empty token"));
    }

    #[test]
    fn read_token_rejects_whitespace_only_input() {
        let err = read_token("   \n".as_bytes()).unwrap_err();
        assert!(err.to_string().contains("empty token"));
    }

    #[test]
    fn read_token_rejects_eof_with_no_data() {
        let err = read_token(&b""[..]).unwrap_err();
        assert!(err.to_string().contains("empty token"));
    }

    #[test]
    fn format_token_status_with_source() {
        assert_eq!(format_token_status("github", Some("env")), "github: env");
    }

    #[test]
    fn format_token_status_without_source() {
        assert_eq!(format_token_status("github", None), "github: none");
    }
}
