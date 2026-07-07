//! persona-wire MCP server library — exposes [`serve_stdio`] for the unified
//! `persona-wire mcp` subcommand to dispatch into. rmcp stdio transport.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use persona_wire_adapter_github::GithubAdapter;
use persona_wire_adapter_mcp::{McpAdapter, McpEndpointResolver, SqliteEndpointResolver};
use persona_wire_adapter_mini_app::MiniAppAdapter;
use persona_wire_adapter_notion::NotionAdapter;
use persona_wire_adapter_obsidian::ObsidianAdapter;
use persona_wire_adapter_persona_pack::PersonaPackAdapter;
use persona_wire_adapter_rss::RssAdapter;
use persona_wire_adapter_sqlite_x::SqliteAdapter;
use persona_wire_adapter_todoist::TodoistAdapter;
use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::application::projection_registry::ProjectionRegistry;
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    wire_close, wire_context_get, wire_doctor, wire_edge_delete, wire_edges_create_batch,
    wire_init, wire_node_delete, wire_node_update, wire_nodes_create_batch, wire_projection_delete,
    wire_prompt_context, wire_query, wire_render, wire_spec_delete, wire_workflow_fire,
    wire_workflow_list, wire_workflow_register, WireCloseInput, WireContextGetInput,
    WireDeleteInput, WireEdgesCreateBatchInput, WireInitInput, WireNodeUpdateInput,
    WireNodeUpdateMode, WireNodesCreateBatchInput, WirePromptContextInput, WireQueryInput,
    WireRenderInput, WireWorkflowFireInput, WireWorkflowListInput, WireWorkflowRegisterInput,
};
use persona_wire_core::domain::entity::projection::{PluginDispatch, Projection};
use persona_wire_core::domain::entity::TargetForm;
use persona_wire_core::domain::graph::{Edge, Node, Severity};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;

/// MCP server wrapping persona-wire-core.
#[derive(Clone)]
pub struct WireServer {
    storage: Arc<Mutex<SqliteStorage>>,
    /// P3a Phase 2 (b) / P3b — Plugin Registry built once at boot. Core defaults
    /// = FileAdapter + HandlebarsEngine + StaticProjection; `MiniAppAdapter`
    /// (mini-app schema-aware), `SqliteAdapter` (raw SQLite, suited for
    /// Fly.io / single-binary self-hosting), and `PersonaPackAdapter`
    /// (persona-pack overlay ACL Facade, scheme `persona-pack://`) are injected
    /// from external crates. Additional plugins (e.g. `wire-adapter-pg`) can be
    /// injected by replacing the `new()` constructor with a builder-aware one
    /// in a future Phase.
    registry: Arc<PluginRegistry>,
    /// Consumed indirectly by `#[tool_handler]`-generated code.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl WireServer {
    pub fn new(storage: SqliteStorage) -> Self {
        let persona_pack =
            PersonaPackAdapter::from_env().expect("PersonaPackAdapter::from_env (HOME unset?)");
        let storage_arc = Arc::new(Mutex::new(storage));
        // McpAdapter: graph-backed endpoint resolution. Every `mcp://<alias>/...`
        // fetch reads node `<alias>` from the shared SqliteStorage; the node
        // must have `type = "mcp_server"` and `metadata.endpoint = <ServerEndpoint>`.
        // See `wire-guide://onboarding` for the registration recipe.
        let mcp_resolver: Arc<dyn McpEndpointResolver> =
            Arc::new(SqliteEndpointResolver::new(storage_arc.clone()));
        Self {
            storage: storage_arc,
            registry: Arc::new(
                PluginRegistry::default_builder_for_wire()
                    .with_adapter(MiniAppAdapter)
                    .with_adapter(SqliteAdapter)
                    .with_adapter(ObsidianAdapter)
                    .with_adapter(persona_pack)
                    .with_adapter(McpAdapter::new(mcp_resolver))
                    .with_adapter(RssAdapter)
                    .with_adapter(GithubAdapter)
                    .with_adapter(TodoistAdapter)
                    .with_adapter(NotionAdapter)
                    .build()
                    .expect("default plugin registry build"),
            ),
            tool_router: Self::tool_router(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool parameter schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireInitParams {
    /// Persona id for which the context bundle is rendered.
    pub persona_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireCloseParams {
    /// Persona id for which the lifecycle scan is reported.
    pub persona_id: String,
}

/// Normalize a metadata argument that may arrive as a JSON-encoded string.
///
/// Some MCP clients (and the rmcp serde path the unified `Parameters<T>`
/// wrapper goes through for top-level fields) stringify `serde_json::Value`
/// inputs before deserialization. Without normalization, an object payload
/// like `{ "persona": "alpha" }` ends up stored as `Value::String("{ … }")`,
/// which breaks every downstream `MetadataEq` query.
///
/// The batch tools (`wire_nodes_create_batch` / `wire_edges_create_batch`)
/// avoid the issue because rmcp deserializes the outer `Vec<…>` first and
/// recursively unwraps each element. Single-row tools need to apply the
/// same recovery explicitly.
///
/// Rules:
/// - `None` / `Value::Null` → empty object `{}` (legacy default).
/// - `Value::String(s)` where `s` parses as JSON → parsed value.
/// - `Value::String(s)` where `s` is not JSON → kept as-is (caller intent).
/// - Anything else → returned unchanged.
fn normalize_metadata(raw: Option<serde_json::Value>) -> serde_json::Value {
    match raw {
        None | Some(serde_json::Value::Null) => serde_json::Value::Object(serde_json::Map::new()),
        Some(serde_json::Value::String(s)) => {
            serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
        }
        Some(other) => other,
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireNodeCreateParams {
    /// Human-readable label for the node (e.g. "alpha.workflow.review_close").
    /// Not required to be unique — the server mints a fresh ULID as the
    /// opaque `id` and returns it in the response. Subsequent operations
    /// (update / delete / get / src/tgt) accept either the ULID or this
    /// `name` via the `id_or_name` resolver.
    pub name: String,
    /// Node type — must be in type_registry (e.g. "persona", "outline_node").
    #[serde(rename = "type")]
    pub type_: String,
    /// Optional SoT ref like "pp://alpha".
    #[serde(default)]
    pub sot_ref: Option<String>,
    /// Optional metadata object (JSON), defaults to `{}`.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireEdgeCreateParams {
    /// Optional human-readable label for the edge. Server mints the opaque
    /// ULID `id` regardless and returns it. Omit for edges that have no
    /// natural caller-facing name (e.g. ad-hoc `routes_to` links).
    #[serde(default)]
    pub name: Option<String>,
    /// Source endpoint — accepts either the node's ULID or its `name`.
    pub src: String,
    /// Target endpoint — accepts either the node's ULID or its `name`.
    pub tgt: String,
    /// Edge kind — must be in type_registry (e.g. "routes_to", "cites").
    pub kind: String,
    /// Optional severity {hard|soft|advisory}, only for triggers_review_of.
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireNodesCreateBatchParams {
    /// Array of node entries; each entry mirrors `wire_node_create` params.
    pub nodes: Vec<WireNodeCreateParams>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireEdgesCreateBatchParams {
    /// Array of edge entries; each entry mirrors `wire_edge_create` params.
    pub edges: Vec<WireEdgeCreateParams>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireRenderParams {
    /// Name of a registered NamedProjection to evaluate + render.
    pub projection_ref: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireQueryParams {
    /// Inline Specification body (JSON-serialised). Mutually exclusive with `spec_ref`.
    /// Example: `{"TypeIs":"persona"}` or `{"And":[...]}`.
    #[serde(default)]
    pub spec: Option<String>,
    /// Name of a previously registered Specification. Mutually exclusive with `spec`.
    #[serde(default)]
    pub spec_ref: Option<String>,
    /// Maximum number of matched nodes to return. Omit for unlimited.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of leading matched nodes to skip. Omit for 0.
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireSpecRegisterParams {
    pub name: String,
    /// JSON body of a Specification (e.g. `{"TypeIs":"persona"}`).
    pub json: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireProjectionRegisterParams {
    pub name: String,
    /// Name of a previously registered Specification.
    pub spec_ref: String,
    /// Mustache-like template (e.g. `"Personas: {{count}}"`).
    pub template: String,
    /// One of prompt | markdown | json | ascii.
    pub target_form: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireDeleteParams {
    /// Node id (for wire_node_delete / wire_edge_delete) or registered name
    /// (for wire_spec_delete / wire_projection_delete). Also reused by
    /// wire_spec_get / wire_projection_get (same id-or-name resolution).
    pub id_or_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireListPageParams {
    /// Max rows to return. Defaults to 100, capped at 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Number of leading rows to skip. Defaults to 0.
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireNodeUpdateParams {
    /// Node id to update. NotFound error if the row does not exist.
    pub id: String,
    /// JSON object whose top-level keys patch the existing node metadata.
    /// In `merge` mode (default), `null` values DELETE the matching key
    /// (RFC 7396); other values overwrite. In `replace` mode the existing
    /// metadata is fully replaced by this object.
    pub metadata_patch: serde_json::Value,
    /// One of `"merge"` (default) or `"replace"`.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WirePromptContextParams {
    pub persona_id: String,
    /// Optional subset of slot names to render (e.g. `["active", "ng"]`).
    /// `None` (omitted) = render all slots registered in persona-pack
    /// `[extra.persona_wire.sections]`.
    #[serde(default)]
    pub projection_names: Option<Vec<String>>,
    /// Optional subset of slot names to exclude (e.g. `["mail", "news"]`).
    /// Combines with `projection_names` as AND NOT: `include \ exclude`.
    /// `None` (omitted) = no exclusion. Unknown names are ignored.
    #[serde(default)]
    pub projection_exclude_names: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireContextGetParams {
    /// The persona whose `ContextWiring` consistency boundary to read.
    pub persona_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireDoctorParams {
    /// Optional persona scope. `None` → Full mode (全 persona 横串)。
    /// `Some(id)` → Persona-scoped mode (当該 persona に紐づく Finding のみ列挙)。
    #[serde(default)]
    pub persona_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireWorkflowRegisterParams {
    /// Node id for the workflow (e.g. `"alpha.workflow.review_close"`).
    pub id: String,
    /// Optional persona scope (stored in `metadata.persona`).
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Trigger descriptor as JSON string — `{"kind":"on_demand"}` or
    /// `{"kind":"on_event","event":"<name>"}`. String form mirrors
    /// `wire_spec_register.json` for transport-friendly schema derivation.
    pub trigger: String,
    /// Action descriptor as JSON string — `{"kind":"no_op"}` or
    /// `{"kind":"emit_projection","projection_names":["..."]}`.
    pub action: String,
    /// Defaults to `true`.
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireWorkflowListParams {
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Filter by `trigger.kind` (e.g. `"on_demand"` / `"on_event"`).
    #[serde(default)]
    pub trigger_kind: Option<String>,
    /// Defaults to `true` (= exclude disabled).
    #[serde(default)]
    pub enabled_only: Option<bool>,
}

// ---- Bundle params --------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireBundleRegisterParams {
    /// TOML body of the bundle. Must include a `[bundle]` table with
    /// `name = "<unique>"` and `version = "<semver>"`. Section arrays
    /// (`[[specs]]` / `[[projections]]` / `[[nodes]]` / `[[edges]]` /
    /// `[[wirings]]` / `[[workflows]]`) are optional.
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireBundleRefParams {
    /// Bundle reference — accepts either a 26-char ULID `id` or the
    /// `name` value of a registered bundle. ULID is tried first; name
    /// fallback resolves through `bundles.name UNIQUE`.
    pub r#ref: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireBundleInstallParams {
    /// Bundle reference — ULID or `name`.
    pub r#ref: String,
    /// Conflict resolution mode for entity name collisions.
    /// `"increment"` (default, non-destructive auto-suffix) /
    /// `"skip"` (leave existing rows alone) / `"error"` (abort whole
    /// install on first collision). `"force"` (= overwrite) is not
    /// implemented in v1 — see Bundle v1 design docs §7.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireWorkflowFireParams {
    /// Fire a single workflow by id. Mutually exclusive with `event`.
    #[serde(default)]
    pub id: Option<String>,
    /// Event-name fan-out (matches every enabled `on_event` workflow whose
    /// `trigger.event` equals this value). Mutually exclusive with `id`.
    #[serde(default)]
    pub event: Option<String>,
    /// Optional persona scope for the event fan-out (matches `metadata.persona`).
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Defaults to `false`. When `true`, resolved fires are returned but no
    /// action is dispatched (= rendered output omitted).
    #[serde(default)]
    pub dry_run: Option<bool>,
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl WireServer {
    /// Render every registered NamedProjection as a Context bundle.
    #[tool(
        name = "wire_init",
        description = "Run wire_init: render every registered NamedProjection against the current graph; returns the rendered context bundle (one entry per projection)."
    )]
    async fn wire_init_tool(
        &self,
        Parameters(p): Parameters<WireInitParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_init(
            WireInitInput {
                persona_id: p.persona_id,
            },
            &s,
            &self.registry,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "persona_id": out.persona_id,
            "projections": out.projections.iter().map(|p| serde_json::json!({
                "name": p.name,
                "target_form": p.target_form.as_str(),
                "rendered": p.rendered,
            })).collect::<Vec<_>>(),
            "warnings": out.warnings,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Run lifecycle scan (orphan + totals).
    #[tool(
        name = "wire_close",
        description = "Run wire_close: minimal lifecycle scan reporting total nodes / edges / orphan-node count, in a Markdown report."
    )]
    async fn wire_close_tool(
        &self,
        Parameters(p): Parameters<WireCloseParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_close(
            WireCloseInput {
                persona_id: p.persona_id,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        Ok(out.report_markdown)
    }

    /// 2-axis integrated health report (graph connectivity + workflow coverage).
    #[tool(
        name = "wire_doctor",
        description = "Finding-driven 2-axis (graph / workflow) health diagnostic. Returns a Markdown report with verdict (HEALTHY / DEGRADED / BROKEN), per-finding severity (error / warn / info), location (固有名詞), description, and fix template (MCP tool call literal). persona_id=None → Full mode (全 persona 横串); persona_id=Some(id) → Persona-scoped mode.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    async fn wire_doctor_tool(
        &self,
        Parameters(p): Parameters<WireDoctorParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_doctor(&s, p.persona_id).map_err(|e| e.to_string())?;
        Ok(out.report_markdown)
    }

    /// Insert a node.
    #[tool(
        name = "wire_node_create",
        description = "Insert a node into the graph. Node `type` must already be registered in type_registry (call wire_type_list to inspect)."
    )]
    async fn wire_node_create(
        &self,
        Parameters(p): Parameters<WireNodeCreateParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let id = persona_wire_core::domain::graph::Ulid::new();
        let node = Node {
            id,
            name: p.name.clone(),
            r#type: p.type_,
            sot_ref: p.sot_ref,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: normalize_metadata(p.metadata),
        };
        s.insert_node(&node).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "id": id.to_string(), "name": p.name }).to_string())
    }

    /// Bulk-insert a batch of nodes (1-row-at-a-time loop, stops on first error).
    #[tool(
        name = "wire_nodes_create_batch",
        description = "Bulk-insert a batch of nodes. Iterates 1-row-at-a-time (non-atomic); stops on first failure and returns inserted_count + failed_at. Use when constructing a graph from many rows (e.g. mini-app row → Node mapping) to avoid per-row tool-call overhead."
    )]
    async fn wire_nodes_create_batch_tool(
        &self,
        Parameters(p): Parameters<WireNodesCreateBatchParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let mut minted: Vec<serde_json::Value> = Vec::with_capacity(p.nodes.len());
        let nodes: Vec<Node> = p
            .nodes
            .into_iter()
            .map(|np| {
                let id = persona_wire_core::domain::graph::Ulid::new();
                minted.push(serde_json::json!({ "id": id.to_string(), "name": np.name }));
                Node {
                    id,
                    name: np.name,
                    r#type: np.type_,
                    sot_ref: np.sot_ref,
                    confidence: None,
                    applicability: None,
                    last_verified_at: None,
                    review_due: None,
                    version: 1,
                    prev_id: None,
                    metadata: normalize_metadata(np.metadata),
                }
            })
            .collect();
        let out = wire_nodes_create_batch(WireNodesCreateBatchInput { nodes }, &s)
            .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "inserted_count": out.inserted_count,
            "failed_at": out.failed_at,
            "error_message": out.error_message,
            "minted": minted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Bulk-insert a batch of edges (1-row-at-a-time loop, stops on first error).
    #[tool(
        name = "wire_edges_create_batch",
        description = "Bulk-insert a batch of edges. Same non-atomic semantics as wire_nodes_create_batch: stops on first failure and returns inserted_count + failed_at."
    )]
    async fn wire_edges_create_batch_tool(
        &self,
        Parameters(p): Parameters<WireEdgesCreateBatchParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let mut edges = Vec::with_capacity(p.edges.len());
        let mut minted: Vec<serde_json::Value> = Vec::with_capacity(p.edges.len());
        for ep in p.edges {
            let sev = match ep.severity.as_deref() {
                None => None,
                Some("hard") => Some(Severity::Hard),
                Some("soft") => Some(Severity::Soft),
                Some("advisory") => Some(Severity::Advisory),
                Some(other) => return Err(format!("unknown severity: {other}")),
            };
            let src_id = s
                .resolve_node_id_or_name(&ep.src)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("edge src node not found: {}", ep.src))?;
            let tgt_id = s
                .resolve_node_id_or_name(&ep.tgt)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("edge tgt node not found: {}", ep.tgt))?;
            let id = persona_wire_core::domain::graph::Ulid::new();
            minted.push(serde_json::json!({
                "id": id.to_string(),
                "name": ep.name,
                "src": src_id.to_string(),
                "tgt": tgt_id.to_string(),
            }));
            edges.push(Edge {
                id,
                name: ep.name,
                src_node: src_id,
                tgt_node: tgt_id,
                kind: ep.kind,
                severity: sev,
                metadata: normalize_metadata(ep.metadata),
                version: 1,
                prev_id: None,
            });
        }
        let out = wire_edges_create_batch(WireEdgesCreateBatchInput { edges }, &s)
            .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "inserted_count": out.inserted_count,
            "failed_at": out.failed_at,
            "error_message": out.error_message,
            "minted": minted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Insert an edge.
    #[tool(
        name = "wire_edge_create",
        description = "Insert an edge into the graph. `kind` must be a registered edge type; `severity` is only valid for triggers_review_of."
    )]
    async fn wire_edge_create(
        &self,
        Parameters(p): Parameters<WireEdgeCreateParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let sev = match p.severity.as_deref() {
            None => None,
            Some("hard") => Some(Severity::Hard),
            Some("soft") => Some(Severity::Soft),
            Some("advisory") => Some(Severity::Advisory),
            Some(other) => {
                return Err(format!("unknown severity: {other}"));
            }
        };
        let src_id = s
            .resolve_node_id_or_name(&p.src)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("edge src node not found: {}", p.src))?;
        let tgt_id = s
            .resolve_node_id_or_name(&p.tgt)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("edge tgt node not found: {}", p.tgt))?;
        let id = persona_wire_core::domain::graph::Ulid::new();
        let edge = Edge {
            id,
            name: p.name.clone(),
            src_node: src_id,
            tgt_node: tgt_id,
            kind: p.kind,
            severity: sev,
            metadata: normalize_metadata(p.metadata),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&edge).map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "id": id.to_string(), "name": p.name }).to_string())
    }

    /// Render a single registered NamedProjection by name (counterpart to wire_init).
    #[tool(
        name = "wire_render",
        description = "Render a single registered NamedProjection by name. Counterpart to wire_init (which renders every registered projection at once): use wire_render when you want exactly one rendered context, identified by projection_ref."
    )]
    async fn wire_render_tool(
        &self,
        Parameters(p): Parameters<WireRenderParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_render(
            WireRenderInput {
                projection_ref: p.projection_ref,
            },
            &s,
            &self.registry,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "name": out.name,
            "target_form": out.target_form.as_str(),
            "rendered": out.rendered,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Ad-hoc query: run a Specification against the graph and return matched nodes.
    #[tool(
        name = "wire_query",
        description = "Ad-hoc query: evaluate either an inline `spec` (Specification JSON) or a registered `spec_ref` against the graph and return matched nodes (slim form: id + type + metadata). Supports `limit` / `offset` for pagination; both unset = unlimited. Mirrors mini-app `list(table, filter)` semantics on the graph."
    )]
    async fn wire_query_tool(
        &self,
        Parameters(p): Parameters<WireQueryParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let spec = match p.spec.as_deref() {
            Some(body) => Some(
                serde_json::from_str::<Specification>(body)
                    .map_err(|e| format!("parse spec JSON: {e}"))?,
            ),
            None => None,
        };
        let out = wire_query(
            WireQueryInput {
                spec,
                spec_ref: p.spec_ref,
                limit: p.limit,
                offset: p.offset,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "matched": out.matched.iter().map(|n| serde_json::json!({
                "id": n.id,
                "type": n.r#type,
                "metadata": n.metadata,
            })).collect::<Vec<_>>(),
            "total_count": out.total_count,
            "returned_count": out.returned_count,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Register a Specification (dynamic / composable selector).
    #[tool(
        name = "wire_spec_register",
        description = "Register a Specification by name. `json` is the serialised Specification body, e.g. `{\"TypeIs\":\"persona\"}` or `{\"And\":[...]}`."
    )]
    async fn wire_spec_register(
        &self,
        Parameters(p): Parameters<WireSpecRegisterParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let spec: Specification =
            serde_json::from_str(&p.json).map_err(|e| format!("parse Specification JSON: {e}"))?;
        let id = SpecRegistry::new(&s)
            .register(&p.name, &spec)
            .map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "id": id.to_string(), "name": p.name }).to_string())
    }

    /// Register a NamedProjection (fixed / named view: spec + template + form).
    #[tool(
        name = "wire_projection_register",
        description = "Register a NamedProjection. spec_ref must name a previously registered Specification. target_form ∈ {prompt|markdown|json|ascii}."
    )]
    async fn wire_projection_register(
        &self,
        Parameters(p): Parameters<WireProjectionRegisterParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let tf = TargetForm::parse(&p.target_form).map_err(|e| e.to_string())?;
        // P3a Phase 2 (a) — MCP wire_projection_register surface does not yet
        // accept the 3 Plugin hint fields; Phase 2 (c) will extend
        // `WireProjectionRegisterParams` to expose `PluginDispatch::Custom`.
        let entity = Projection::from_parts(
            p.name.clone(),
            p.spec_ref,
            p.template,
            tf,
            PluginDispatch::Default,
        )
        .map_err(|e| e.to_string())?;
        let id = ProjectionRegistry::new(&s)
            .register(&entity)
            .map_err(|e| e.to_string())?;
        Ok(serde_json::json!({ "id": id.to_string(), "name": p.name }).to_string())
    }

    /// List registered Specifications in created_at-descending order.
    #[tool(
        name = "wire_spec_list",
        description = "List registered Specifications in created_at-descending order. Default limit 100 / max 1000. Each row carries id / name / json (raw Specification body) / created_at / updated_at."
    )]
    async fn wire_spec_list(
        &self,
        Parameters(p): Parameters<WireListPageParams>,
    ) -> Result<String, String> {
        let limit = p.limit.unwrap_or(100).min(1000) as i64;
        let offset = p.offset.unwrap_or(0) as i64;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let rows = SpecRegistry::new(&s)
            .list_full(limit, offset)
            .map_err(|e| e.to_string())?;
        let out: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "name": r.name,
                    "json": r.json,
                    "created_at": r.created_at,
                    "updated_at": r.updated_at,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({ "specs": out }))
            .map_err(|e| e.to_string())
    }

    /// Get a Specification by name or id, including the raw JSON body.
    #[tool(
        name = "wire_spec_get",
        description = "Fetch a registered Specification by name or ULID id. Returns id / name / json (raw Specification body) / created_at / updated_at. Errors with NotFound if absent."
    )]
    async fn wire_spec_get(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let row = SpecRegistry::new(&s)
            .get_full_by_ref(&p.id_or_name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("spec not found: {}", p.id_or_name))?;
        Ok(serde_json::json!({
            "id": row.id.to_string(),
            "name": row.name,
            "json": row.json,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        })
        .to_string())
    }

    /// List registered NamedProjections in created_at-descending order.
    #[tool(
        name = "wire_projection_list",
        description = "List registered NamedProjections in created_at-descending order. Default limit 100 / max 1000. Each row carries id / name / spec_ref / target_form / template / created_at / updated_at."
    )]
    async fn wire_projection_list(
        &self,
        Parameters(p): Parameters<WireListPageParams>,
    ) -> Result<String, String> {
        let limit = p.limit.unwrap_or(100).min(1000) as i64;
        let offset = p.offset.unwrap_or(0) as i64;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let rows = ProjectionRegistry::new(&s)
            .list_full(limit, offset)
            .map_err(|e| e.to_string())?;
        let out: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id.to_string(),
                    "name": r.name,
                    "spec_ref": r.spec_ref,
                    "target_form": r.target_form.as_str(),
                    "template": r.template,
                    "created_at": r.created_at,
                    "updated_at": r.updated_at,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({ "projections": out }))
            .map_err(|e| e.to_string())
    }

    /// Get a NamedProjection by name or id.
    #[tool(
        name = "wire_projection_get",
        description = "Fetch a registered NamedProjection by name or ULID id. Returns id / name / spec_ref / target_form / template / created_at / updated_at. Errors with NotFound if absent."
    )]
    async fn wire_projection_get(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let row = ProjectionRegistry::new(&s)
            .get_full_by_ref(&p.id_or_name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("projection not found: {}", p.id_or_name))?;
        Ok(serde_json::json!({
            "id": row.id.to_string(),
            "name": row.name,
            "spec_ref": row.spec_ref,
            "target_form": row.target_form.as_str(),
            "template": row.template,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
        })
        .to_string())
    }

    /// Patch a node's metadata in place (merge or replace). Use for tuning
    /// wiring entries (e.g. appending `&limit=10` to `metadata.source_uri`)
    /// without delete + re-create cycles that would lose the node id.
    #[tool(
        name = "wire_node_update",
        description = "Patch a node's metadata in place. `mode=\"merge\"` (default) applies an RFC 7396 shallow merge — top-level keys in `metadata_patch` overwrite the existing metadata; `null` values delete the matching key. `mode=\"replace\"` swaps the metadata wholesale. Other node fields (type / sot_ref / lifecycle) are immutable on this path; delete + re-create to change them. Returns {id, mode, metadata}."
    )]
    async fn wire_node_update_tool(
        &self,
        Parameters(p): Parameters<WireNodeUpdateParams>,
    ) -> Result<String, String> {
        let mode_str = p.mode.as_deref().unwrap_or("merge");
        let mode = WireNodeUpdateMode::parse(mode_str).map_err(|e| e.to_string())?;
        // rmcp harness が top-level Value field を文字列化する挙動を吸収
        // (mia 2026-06-14 Finding 1 sibling — `normalize_metadata` 経由で recover)。
        let patch = normalize_metadata(Some(p.metadata_patch));
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: p.id,
                metadata_patch: patch,
                mode,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "id": out.id,
            "mode": out.mode.as_str(),
            "metadata": out.metadata,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Delete a node by id. Edges referencing the node are cascade-deleted
    /// in the same storage Tx (edges FK is NOT-NULL).
    #[tool(
        name = "wire_node_delete",
        description = "Delete a node by id. Returns {kind, id_or_name, deleted}. Edges referencing the node (as src or tgt) are cascade-deleted in the same storage transaction — edges table FK is NOT-NULL so dangling state is not representable in normal operation."
    )]
    async fn wire_node_delete_tool(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_node_delete(
            WireDeleteInput {
                id_or_name: p.id_or_name,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "kind": out.kind,
            "id_or_name": out.id_or_name,
            "deleted": out.deleted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Delete an edge by id.
    #[tool(
        name = "wire_edge_delete",
        description = "Delete an edge by id. Returns {kind, id_or_name, deleted}."
    )]
    async fn wire_edge_delete_tool(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_edge_delete(
            WireDeleteInput {
                id_or_name: p.id_or_name,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "kind": out.kind,
            "id_or_name": out.id_or_name,
            "deleted": out.deleted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Delete a Specification by name. Projections referencing this spec via
    /// `spec_ref` will start returning dangling-spec errors at render time.
    #[tool(
        name = "wire_spec_delete",
        description = "Delete a Specification by name. Returns {kind, id_or_name, deleted}. Projections referencing this spec via spec_ref will start returning dangling-spec errors at render time."
    )]
    async fn wire_spec_delete_tool(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_spec_delete(
            WireDeleteInput {
                id_or_name: p.id_or_name,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "kind": out.kind,
            "id_or_name": out.id_or_name,
            "deleted": out.deleted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// One-shot entry: discover persona-scoped wiring entries, fetch each slot
    /// via the Layer 6 Adapter, render with the registered NamedProjection
    /// (optionally merged with a persona-pack overlay), and concatenate the
    /// rendered blocks into a single PromptContext.
    #[tool(
        name = "wire_prompt_context",
        description = "Run every registered NamedProjection through the Layer 6 Adapter (mini-app:// / file:// schemes supported) to fresh-fetch each wiring entry's source_uri, render via handlebars, and return the concatenated PromptContext literal in one call. Used as the `/wake` auto-load entry — wire holds wiring metadata only, data lives in the SoT (mini-app / file / outline). Optional `projection_names` (include subset) and `projection_exclude_names` (exclude subset) compose as AND NOT (`include \\ exclude`); exclude wins on intersection, unknown names are ignored."
    )]
    async fn wire_prompt_context_tool(
        &self,
        Parameters(p): Parameters<WirePromptContextParams>,
    ) -> Result<String, String> {
        let storage = self.storage.clone();
        let out = wire_prompt_context(
            WirePromptContextInput {
                persona_id: p.persona_id,
                projection_names: p.projection_names,
                projection_exclude_names: p.projection_exclude_names,
            },
            storage,
            &self.registry,
        )
        .await
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "persona_id": out.persona_id,
            "prompt_context": out.prompt_context,
            "projections": out.projections.iter().map(|p| serde_json::json!({
                "name": p.name,
                "target_form": format!("{:?}", p.target_form),
                "rendered": p.rendered,
            })).collect::<Vec<_>>(),
            "warnings": out.warnings,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// One-shot structured read of a persona's `ContextWiring` boundary —
    /// returns the `Wiring` + `Workflow` set as summary DTOs in a single
    /// call (no rendering). Counterpart to `wire_prompt_context` (which
    /// returns rendered text).
    #[tool(
        name = "wire_context_get",
        description = "Return the per-persona ContextWiring read snapshot: {persona_id, wirings: [{slot, source_uri, projection_ref?, maintenance_exempt}], workflows: [{id, persona_id?, trigger, action, enabled}]}. 1-call structured aggregate (no rendering); use wire_prompt_context for the rendered surface."
    )]
    async fn wire_context_get_tool(
        &self,
        Parameters(p): Parameters<WireContextGetParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_context_get(
            WireContextGetInput {
                persona_id: p.persona_id,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let wirings: Vec<serde_json::Value> = out
            .wirings
            .into_iter()
            .map(|w| {
                serde_json::json!({
                    "slot": w.slot,
                    "source_uri": w.source_uri,
                    "projection_ref": w.projection_ref,
                    "maintenance_exempt": w.maintenance_exempt,
                })
            })
            .collect();
        let workflows: Vec<serde_json::Value> = out
            .workflows
            .into_iter()
            .map(|w| {
                serde_json::json!({
                    "id": w.id,
                    "persona_id": w.persona_id,
                    "trigger": w.trigger,
                    "action": w.action,
                    "enabled": w.enabled,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "persona_id": out.persona_id,
            "wirings": wirings,
            "workflows": workflows,
        }))
        .map_err(|e| e.to_string())
    }

    /// Delete a NamedProjection by name.
    #[tool(
        name = "wire_projection_delete",
        description = "Delete a NamedProjection by name. Returns {kind, id_or_name, deleted}."
    )]
    async fn wire_projection_delete_tool(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_projection_delete(
            WireDeleteInput {
                id_or_name: p.id_or_name,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "kind": out.kind,
            "id_or_name": out.id_or_name,
            "deleted": out.deleted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    // ---- wire_workflow_* (P5-a seed) ---------------------------------------

    /// Register a Workflow as a `workflow_def` Node (declarative trigger + action).
    #[tool(
        name = "wire_workflow_register",
        description = "Register a Workflow as a `workflow_def` Node. trigger.kind ∈ {on_demand, on_event}; action.kind ∈ {no_op, emit_projection}. See docs/wire-workflow-spec.md."
    )]
    async fn wire_workflow_register_tool(
        &self,
        Parameters(p): Parameters<WireWorkflowRegisterParams>,
    ) -> Result<String, String> {
        let trigger: serde_json::Value =
            serde_json::from_str(&p.trigger).map_err(|e| format!("parse trigger JSON: {e}"))?;
        let action: serde_json::Value =
            serde_json::from_str(&p.action).map_err(|e| format!("parse action JSON: {e}"))?;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_workflow_register(
            WireWorkflowRegisterInput {
                id: p.id,
                persona_id: p.persona_id,
                trigger,
                action,
                enabled: p.enabled,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        Ok(format!("registered workflow: {}", out.id))
    }

    /// List registered Workflows (= `workflow_def` Nodes).
    #[tool(
        name = "wire_workflow_list",
        description = "List registered Workflows with optional persona_id / trigger_kind filters (defaults: enabled_only=true)."
    )]
    async fn wire_workflow_list_tool(
        &self,
        Parameters(p): Parameters<WireWorkflowListParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_workflow_list(
            WireWorkflowListInput {
                persona_id: p.persona_id,
                trigger_kind: p.trigger_kind,
                enabled_only: p.enabled_only,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let workflows: Vec<serde_json::Value> = out
            .workflows
            .into_iter()
            .map(|w| {
                serde_json::json!({
                    "id": w.id,
                    "persona_id": w.persona_id,
                    "enabled": w.enabled,
                    "trigger": w.trigger,
                    "action": w.action,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({ "workflows": workflows }))
            .map_err(|e| e.to_string())
    }

    /// Delete a Workflow by id (thin alias of `wire_node_delete` for caller clarity).
    #[tool(
        name = "wire_workflow_delete",
        description = "Delete a Workflow by id. Returns {kind, id_or_name, deleted}. Equivalent to wire_node_delete for the workflow's Node id."
    )]
    async fn wire_workflow_delete_tool(
        &self,
        Parameters(p): Parameters<WireDeleteParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_node_delete(
            WireDeleteInput {
                id_or_name: p.id_or_name,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "kind": out.kind,
            "id_or_name": out.id_or_name,
            "deleted": out.deleted,
        });
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
    }

    /// Fire one or more Workflows. For `action.kind = emit_projection`,
    /// invokes `wire_prompt_context` for each fired workflow and includes the
    /// rendered output in the response (unless `dry_run = true`).
    #[tool(
        name = "wire_workflow_fire",
        description = "Fire a Workflow by `id` (single) or by `event` (fan-out across enabled on_event workflows). Resolves the action; for `emit_projection`, invokes wire_prompt_context and returns the rendered block per fire (unless dry_run=true)."
    )]
    async fn wire_workflow_fire_tool(
        &self,
        Parameters(p): Parameters<WireWorkflowFireParams>,
    ) -> Result<String, String> {
        // Phase 1: resolve under lock (sync core).
        let resolved = {
            let s = self.storage.lock().map_err(|e| e.to_string())?;
            wire_workflow_fire(
                WireWorkflowFireInput {
                    id: p.id,
                    event: p.event,
                    persona_id: p.persona_id,
                    dry_run: p.dry_run,
                },
                &s,
            )
            .map_err(|e| e.to_string())?
        };

        // Phase 2: dispatch async action per fire (currently emit_projection / no_op).
        let mut fired_out = Vec::with_capacity(resolved.fired.len());
        for f in resolved.fired {
            let rendered = if f.dry_run {
                serde_json::json!(null)
            } else if f.action_kind == "emit_projection" {
                match f.persona_id.clone() {
                    None => serde_json::json!({
                        "error": "emit_projection requires metadata.persona to render",
                    }),
                    Some(persona_id) => {
                        let names = f.action_emit_projection_names.clone().unwrap_or_default();
                        match wire_prompt_context(
                            WirePromptContextInput {
                                persona_id,
                                projection_names: Some(names),
                                projection_exclude_names: None,
                            },
                            self.storage.clone(),
                            &self.registry,
                        )
                        .await
                        {
                            Ok(pc) => serde_json::json!({
                                "persona_id": pc.persona_id,
                                "prompt_context": pc.prompt_context,
                                "warnings": pc.warnings,
                            }),
                            Err(e) => serde_json::json!({ "error": e.to_string() }),
                        }
                    }
                }
            } else {
                serde_json::json!({ "kind": "no_op" })
            };
            fired_out.push(serde_json::json!({
                "id": f.id,
                "persona_id": f.persona_id,
                "action_kind": f.action_kind,
                "dry_run": f.dry_run,
                "result": rendered,
            }));
        }

        let skipped_out: Vec<serde_json::Value> = resolved
            .skipped
            .into_iter()
            .map(|(id, reason)| serde_json::json!({ "id": id, "reason": reason }))
            .collect();

        serde_json::to_string_pretty(&serde_json::json!({
            "fired": fired_out,
            "skipped": skipped_out,
        }))
        .map_err(|e| e.to_string())
    }

    // ---- Bundle tools -----------------------------------------------------

    /// Register a Bundle by TOML literal. Returns `{id, name, version}`.
    #[tool(
        name = "wire_bundle_register",
        description = "Register a Bundle scaffolding template. `body` is a TOML literal containing a [bundle] table (name + version + optional description) and any subset of [[specs]] / [[projections]] / [[nodes]] / [[edges]] / [[wirings]] / [[workflows]] sections. The TOML body is stored verbatim; install-time parsing surfaces per-entity errors. Same-name register overwrites; install conflict resolution lives in `wire_bundle_install`."
    )]
    async fn wire_bundle_register(
        &self,
        Parameters(p): Parameters<WireBundleRegisterParams>,
    ) -> Result<String, String> {
        use persona_wire_core::application::bundle_registry::BundleRegistry;
        use persona_wire_core::domain::entity::bundle::{BundleName, BundleVersion};
        let (name, version, description) = parse_bundle_header(&p.body)?;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let bn = BundleName::new(name.clone()).map_err(|e| e.to_string())?;
        let bv = BundleVersion::new(version.clone()).map_err(|e| e.to_string())?;
        let id = BundleRegistry::new(&s)
            .register(&bn, &bv, description.as_deref(), &p.body)
            .map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "id": id.to_string(),
            "name": name,
            "version": version,
        })
        .to_string())
    }

    /// List registered Bundles in name-ascending order.
    #[tool(
        name = "wire_bundle_list",
        description = "List registered Bundles in name-ascending order. Each row carries id / name / version / description (full TOML body is omitted; fetch via `wire_bundle_get`)."
    )]
    async fn wire_bundle_list(&self) -> Result<String, String> {
        use persona_wire_core::application::bundle_registry::BundleRegistry;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let bundles = BundleRegistry::new(&s).list().map_err(|e| e.to_string())?;
        let rows: Vec<serde_json::Value> = bundles
            .into_iter()
            .map(|b| {
                serde_json::json!({
                    "id": b.id.to_string(),
                    "name": b.name.as_str(),
                    "version": b.version.as_str(),
                    "description": b.description,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({ "bundles": rows }))
            .map_err(|e| e.to_string())
    }

    /// Get a Bundle by name or id, including the full TOML body.
    #[tool(
        name = "wire_bundle_get",
        description = "Fetch a registered Bundle by name or ULID id. Returns id / name / version / description / body (raw TOML) / created_at / updated_at. Errors with NotFound if absent."
    )]
    async fn wire_bundle_get(
        &self,
        Parameters(p): Parameters<WireBundleRefParams>,
    ) -> Result<String, String> {
        use persona_wire_core::application::bundle_registry::BundleRegistry;
        use persona_wire_core::domain::entity::bundle::BundleRef;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let r = BundleRef::parse(&p.r#ref).map_err(|e| e.to_string())?;
        let b = BundleRegistry::new(&s)
            .resolve(&r)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("bundle not found: {}", p.r#ref))?;
        Ok(serde_json::json!({
            "id": b.id.to_string(),
            "name": b.name.as_str(),
            "version": b.version.as_str(),
            "description": b.description,
            "body": b.body,
            "created_at": b.created_at,
            "updated_at": b.updated_at,
        })
        .to_string())
    }

    /// Install a Bundle. Returns the structured report.
    #[tool(
        name = "wire_bundle_install",
        description = "Install a registered Bundle. `mode` ∈ {increment (default, auto-suffix collisions), skip (leave existing rows alone), error (abort whole install on first collision)}. Returns BundleInstallReport with per-entity installed / skipped / errors rows."
    )]
    async fn wire_bundle_install(
        &self,
        Parameters(p): Parameters<WireBundleInstallParams>,
    ) -> Result<String, String> {
        use persona_wire_core::application::bundle_install::install_bundle;
        use persona_wire_core::application::bundle_registry::BundleRegistry;
        use persona_wire_core::domain::entity::bundle::{BundleRef, ConflictMode};
        let mode = match p.mode.as_deref() {
            None => ConflictMode::default(),
            Some(s) => ConflictMode::parse(s).map_err(|e| e.to_string())?,
        };
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let r = BundleRef::parse(&p.r#ref).map_err(|e| e.to_string())?;
        let bundle = BundleRegistry::new(&s)
            .resolve(&r)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("bundle not found: {}", p.r#ref))?;
        let report = install_bundle(&bundle, mode, &s).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())
    }

    /// Delete a Bundle by name or id. Install history is preserved.
    #[tool(
        name = "wire_bundle_delete",
        description = "Delete a Bundle row by name or id. Install history (`bundle_installs`) is intentionally preserved across deletion. Returns {deleted: bool}."
    )]
    async fn wire_bundle_delete(
        &self,
        Parameters(p): Parameters<WireBundleRefParams>,
    ) -> Result<String, String> {
        use persona_wire_core::application::bundle_registry::BundleRegistry;
        use persona_wire_core::domain::entity::bundle::BundleRef;
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let reg = BundleRegistry::new(&s);
        let deleted = match BundleRef::parse(&p.r#ref).map_err(|e| e.to_string())? {
            BundleRef::Id(id) => reg.delete_by_id(id).map_err(|e| e.to_string())?,
            BundleRef::Name(name) => reg.delete(&name).map_err(|e| e.to_string())?,
        };
        Ok(serde_json::json!({ "deleted": deleted }).to_string())
    }
}

/// Pull `[bundle].name` / `version` / optional `description` out of a TOML
/// body without committing the full manifest schema. Used by
/// `wire_bundle_register` so a malformed install-time section (e.g.
/// `[[specs]].spec` shape) does not block the register call.
fn parse_bundle_header(body: &str) -> Result<(String, String, Option<String>), String> {
    let value: toml::Value =
        toml::from_str(body).map_err(|e| format!("bundle TOML parse: {}", e))?;
    let bundle = value
        .get("bundle")
        .and_then(|v| v.as_table())
        .ok_or_else(|| "missing [bundle] table".to_string())?;
    let name = bundle
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing [bundle].name".to_string())?
        .to_string();
    let version = bundle
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing [bundle].version".to_string())?
        .to_string();
    let description = bundle
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok((name, version, description))
}

// ---------------------------------------------------------------------------
// Onboarding guide — exposed both as a Rust constant (for embedding in tests
// or other crates) and as an MCP resource at `wire-guide://onboarding`.

/// Full end-to-end onboarding guide bundled with the MCP server.
///
/// # Sync policy (must not edit only one side)
///
/// - **Canonical**: `docs/onboarding.md` (workspace root, human-navigable
///   from project root, cross-referenced by other docs / READMEs).
/// - **Bundled copy**: `crates/persona-wire-mcp/onboarding.md` (= the file
///   `include_str!`-ed below). This copy exists because `cargo publish`
///   only packages files within the crate's own directory tree —
///   `include_str!("../../../docs/onboarding.md")` worked for local builds
///   but broke `cargo publish --dry-run` (= file outside the packaged
///   tarball). Hence the in-crate mirror.
///
/// **Editing rule**: always edit the canonical workspace copy
/// (`docs/onboarding.md`), then run `cp docs/onboarding.md
/// crates/persona-wire-mcp/onboarding.md` to refresh the bundled copy.
///
/// **Safety nets enforcing this rule**:
/// 1. `include_str!("../onboarding.md")` here → cargo build / publish
///    error out if the bundled copy is missing.
/// 2. `crates/persona-wire-mcp/build.rs` byte-compares the two copies on
///    every dev build and `panic!`s with a one-line fix command if they
///    diverge. Published-tarball builds (= workspace doc absent) skip
///    this check — they ship only the bundled copy.
///
/// Background: introduced in commit `37e7cec` with the original
/// `../../../docs/onboarding.md` path; the workspace-relative path silently
/// broke `cargo publish` until P5-a' work surfaced it via publish-checker
/// invoke (see `docs/wire-workflow-spec.md` §10).
pub const ONBOARDING_GUIDE: &str = include_str!("../onboarding.md");

const ONBOARDING_URI: &str = "wire-guide://onboarding";

#[tool_handler]
impl ServerHandler for WireServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "persona-wire-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "persona-wire MCP server. Graph engine over persona × SoT × workflow \
             context routing. Tools: wire_init / wire_close / wire_doctor / \
             wire_query / wire_render / wire_prompt_context / wire_node_create / \
             wire_edge_create / wire_nodes_create_batch / wire_edges_create_batch / \
             wire_spec_register / wire_projection_register / wire_node_delete / \
             wire_edge_delete / wire_spec_delete / wire_projection_delete / \
             wire_workflow_register / wire_workflow_list / wire_workflow_fire / \
             wire_workflow_delete. \
             For the full end-to-end onboarding walkthrough (setup → wire entries → \
             spec / projection → optional persona-pack overlay → wire_prompt_context \
             call → Skill / Prompt wiring) read the bundled resource at \
             `wire-guide://onboarding` via `read_resource`.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _ctx: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> std::result::Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        let raw = rmcp::model::RawResource {
            uri: ONBOARDING_URI.to_string(),
            name: "persona-wire onboarding guide".to_string(),
            title: Some("Onboarding — Wiring a new persona end-to-end".to_string()),
            description: Some(
                "Full walkthrough: install, register wiring entries, register \
                 Specification + NamedProjection, optional persona-pack overlay, \
                 smoke-test, and inline the rendered prompt_context into a Skill."
                    .to_string(),
            ),
            mime_type: Some("text/markdown".to_string()),
            size: Some(ONBOARDING_GUIDE.len() as u32),
            icons: None,
            meta: None,
        };
        let resource = rmcp::model::Resource {
            raw,
            annotations: None,
        };
        Ok(rmcp::model::ListResourcesResult::with_all_items(vec![
            resource,
        ]))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _ctx: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> std::result::Result<rmcp::model::ReadResourceResult, rmcp::ErrorData> {
        if request.uri == ONBOARDING_URI {
            Ok(rmcp::model::ReadResourceResult::new(vec![
                rmcp::model::ResourceContents::text(ONBOARDING_GUIDE, ONBOARDING_URI),
            ]))
        } else {
            Err(rmcp::ErrorData::resource_not_found(
                format!("unknown resource uri: {}", request.uri),
                None,
            ))
        }
    }
}

/// Run the MCP server over stdio against the given SQLite db path. Caller
/// (typically the unified `persona-wire mcp` subcommand) owns tokio runtime
/// setup and tracing init.
pub async fn serve_stdio(db_path: &str) -> Result<()> {
    tracing::info!(db = %db_path, "persona-wire mcp starting");

    let storage = SqliteStorage::open(db_path)?;
    storage.migrate()?;
    storage.seed_default_types()?;

    let server = WireServer::new(storage);
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_metadata_none_becomes_empty_object() {
        let got = normalize_metadata(None);
        assert_eq!(got, json!({}));
        assert!(got.is_object());
    }

    #[test]
    fn normalize_metadata_null_becomes_empty_object() {
        let got = normalize_metadata(Some(serde_json::Value::Null));
        assert_eq!(got, json!({}));
    }

    #[test]
    fn normalize_metadata_object_passes_through() {
        let got = normalize_metadata(Some(json!({"persona": "alpha", "axis": "active"})));
        assert_eq!(got, json!({"persona": "alpha", "axis": "active"}));
    }

    #[test]
    fn normalize_metadata_json_encoded_string_is_recovered_as_object() {
        // This is the core Finding 1 fix: a stringified JSON object payload
        // must round-trip back into an Object so downstream `MetadataEq`
        // queries against `metadata.persona` etc. continue to match.
        let stringified = r#"{"persona":"alpha","axis":"active","nested":{"k":1}}"#;
        let got = normalize_metadata(Some(serde_json::Value::String(stringified.into())));
        assert_eq!(
            got,
            json!({
                "persona": "alpha",
                "axis": "active",
                "nested": {"k": 1}
            })
        );
        assert!(got.is_object());
    }

    #[test]
    fn normalize_metadata_plain_string_is_preserved() {
        // A genuine string payload (not JSON-encoded) is kept as-is so the
        // caller's intent is not silently mutated.
        let got = normalize_metadata(Some(serde_json::Value::String("hello".into())));
        assert_eq!(got, json!("hello"));
        assert!(got.is_string());
    }

    #[test]
    fn normalize_metadata_json_array_string_is_recovered() {
        // Arrays survive the same way as objects.
        let got = normalize_metadata(Some(serde_json::Value::String("[1,2,3]".into())));
        assert_eq!(got, json!([1, 2, 3]));
        assert!(got.is_array());
    }
}
