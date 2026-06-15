//! persona-wire MCP server library — exposes [`serve_stdio`] for the unified
//! `persona-wire mcp` subcommand to dispatch into. rmcp stdio transport.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use persona_wire_core::application::projection_registry::{
    NamedProjection, ProjectionRegistry, TargetForm,
};
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    wire_close, wire_doctor, wire_edges_create_batch, wire_init, wire_nodes_create_batch,
    wire_query, WireCloseInput, WireEdgesCreateBatchInput, WireInitInput,
    WireNodesCreateBatchInput, WireQueryInput,
};
use persona_wire_core::domain::graph::{Edge, Node, Severity};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;

/// MCP server wrapping persona-wire-core.
#[derive(Clone)]
pub struct WireServer {
    storage: Arc<Mutex<SqliteStorage>>,
    /// Consumed indirectly by `#[tool_handler]`-generated code.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl WireServer {
    pub fn new(storage: SqliteStorage) -> Self {
        Self {
            storage: Arc::new(Mutex::new(storage)),
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireNodeCreateParams {
    pub id: String,
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
    pub id: String,
    pub src: String,
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

    /// Graph-wide health diagnostic (orphan + totals, persona-agnostic).
    #[tool(
        name = "wire_doctor",
        description = "Run wire_doctor: graph-wide health diagnostic reporting total nodes / edges / orphan-node count in a Markdown report. Not persona-scoped."
    )]
    async fn wire_doctor_tool(&self) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_doctor(&s).map_err(|e| e.to_string())?;
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
        let node = Node {
            id: p.id.clone(),
            r#type: p.type_,
            sot_ref: p.sot_ref,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: p.metadata.unwrap_or(serde_json::json!({})),
        };
        s.insert_node(&node).map_err(|e| e.to_string())?;
        Ok(format!("created node: {}", p.id))
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
        let nodes: Vec<Node> = p
            .nodes
            .into_iter()
            .map(|np| Node {
                id: np.id,
                r#type: np.type_,
                sot_ref: np.sot_ref,
                confidence: None,
                applicability: None,
                last_verified_at: None,
                review_due: None,
                version: 1,
                prev_id: None,
                metadata: np.metadata.unwrap_or(serde_json::json!({})),
            })
            .collect();
        let out = wire_nodes_create_batch(WireNodesCreateBatchInput { nodes }, &s)
            .map_err(|e| e.to_string())?;
        let json = serde_json::json!({
            "inserted_count": out.inserted_count,
            "failed_at": out.failed_at,
            "error_message": out.error_message,
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
        for ep in p.edges {
            let sev = match ep.severity.as_deref() {
                None => None,
                Some("hard") => Some(Severity::Hard),
                Some("soft") => Some(Severity::Soft),
                Some("advisory") => Some(Severity::Advisory),
                Some(other) => return Err(format!("unknown severity: {other}")),
            };
            edges.push(Edge {
                id: ep.id,
                src_node: ep.src,
                tgt_node: ep.tgt,
                kind: ep.kind,
                severity: sev,
                metadata: ep.metadata.unwrap_or(serde_json::json!({})),
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
        let edge = Edge {
            id: p.id.clone(),
            src_node: p.src,
            tgt_node: p.tgt,
            kind: p.kind,
            severity: sev,
            metadata: p.metadata.unwrap_or(serde_json::json!({})),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&edge).map_err(|e| e.to_string())?;
        Ok(format!("created edge: {}", p.id))
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

    /// Register a Specification (dynamic-axis query object).
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
        SpecRegistry::new(&s)
            .register(&p.name, &spec)
            .map_err(|e| e.to_string())?;
        Ok(format!("registered spec: {}", p.name))
    }

    /// Register a NamedProjection (fixed-axis: spec + template + form).
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
        ProjectionRegistry::new(&s)
            .register(&NamedProjection {
                name: p.name.clone(),
                spec_ref: p.spec_ref,
                template: p.template,
                target_form: tf,
            })
            .map_err(|e| e.to_string())?;
        Ok(format!("registered projection: {}", p.name))
    }
}

#[tool_handler]
impl ServerHandler for WireServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "persona-wire-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "persona-wire MCP server. Graph engine over persona × SoT × workflow \
             context routing. Tools: wire_init / wire_close / wire_doctor / wire_query / \
             wire_node_create / wire_edge_create / wire_nodes_create_batch / \
             wire_edges_create_batch / wire_spec_register / wire_projection_register.",
        )
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
