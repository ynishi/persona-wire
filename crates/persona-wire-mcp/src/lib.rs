//! persona-wire MCP server library — exposes [`serve_stdio`] for the unified
//! `persona-wire mcp` subcommand to dispatch into. rmcp stdio transport.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use persona_wire_adapter_mini_app::MiniAppAdapter;
use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::application::projection_registry::{
    NamedProjection, ProjectionRegistry, TargetForm,
};
use persona_wire_core::application::spec_registry::SpecRegistry;
use persona_wire_core::application::use_cases::{
    wire_close, wire_doctor, wire_edge_delete, wire_edges_create_batch, wire_init,
    wire_node_delete, wire_node_update, wire_nodes_create_batch, wire_projection_delete,
    wire_prompt_context, wire_query, wire_render, wire_spec_delete, wire_workflow_check,
    wire_workflow_fire, wire_workflow_list, wire_workflow_register, WireCloseInput,
    WireDeleteInput, WireEdgesCreateBatchInput, WireInitInput, WireNodeUpdateInput,
    WireNodeUpdateMode, WireNodesCreateBatchInput, WirePromptContextInput, WireQueryInput,
    WireRenderInput, WireWorkflowCheckInput, WireWorkflowFireInput, WireWorkflowListInput,
    WireWorkflowRegisterInput,
};
use persona_wire_core::domain::graph::{Edge, Node, Severity};
use persona_wire_core::domain::specification::Specification;
use persona_wire_core::infrastructure::storage::SqliteStorage;

/// MCP server wrapping persona-wire-core.
#[derive(Clone)]
pub struct WireServer {
    storage: Arc<Mutex<SqliteStorage>>,
    /// P3a Phase 2 (b) / P3b — Plugin Registry built once at boot. Core defaults
    /// = FileAdapter + HandlebarsEngine + StaticProjection; `MiniAppAdapter` is
    /// injected from the external `persona-wire-adapter-mini-app` crate.
    /// Additional plugins (e.g. `wire-adapter-pg`) can be injected by replacing
    /// the `new()` constructor with a builder-aware one in a future Phase.
    registry: Arc<PluginRegistry>,
    /// Consumed indirectly by `#[tool_handler]`-generated code.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl WireServer {
    pub fn new(storage: SqliteStorage) -> Self {
        Self {
            storage: Arc::new(Mutex::new(storage)),
            registry: Arc::new(
                PluginRegistry::default_builder_for_wire()
                    .with_adapter(MiniAppAdapter)
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
    /// (for wire_spec_delete / wire_projection_delete).
    pub id_or_name: String,
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
    /// Optional subset of axis names to render (e.g. `["active", "ng"]`).
    /// `None` (omitted) = render all axes registered in persona-pack
    /// `[extra.persona_wire.sections]`.
    #[serde(default)]
    pub projection_names: Option<Vec<String>>,
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireWorkflowCheckParams {
    /// Optional persona scope (filters by Node `metadata.persona`).
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Include `exempt` nodes (`metadata.maintenance_exempt=true`) in the
    /// response. Defaults to `false` (= count only).
    #[serde(default)]
    pub include_exempt: Option<bool>,
    /// Include the full `declared_covered` list in the response. Defaults
    /// to `false` (= count only; uncovered + undeclared are always listed).
    #[serde(default)]
    pub include_covered: Option<bool>,
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
            metadata: normalize_metadata(p.metadata),
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
                metadata: normalize_metadata(np.metadata),
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
            metadata: normalize_metadata(p.metadata),
            version: 1,
            prev_id: None,
        };
        s.insert_edge(&edge).map_err(|e| e.to_string())?;
        Ok(format!("created edge: {}", p.id))
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
                // P3a Phase 2 (a) — MCP wire_projection_register surface does
                // not yet accept the 3 new Plugin hint fields; Phase 2 (c) will
                // extend `WireProjectionRegisterParams` to expose them.
                template_engine: None,
                projection_kind: None,
                projection_config: None,
            })
            .map_err(|e| e.to_string())?;
        Ok(format!("registered projection: {}", p.name))
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

    /// Delete a node by id. Edges are not cascade-deleted (wire_doctor surfaces
    /// surviving dangling references on the next scan).
    #[tool(
        name = "wire_node_delete",
        description = "Delete a node by id. Returns {kind, id_or_name, deleted}. Edges are NOT cascade-deleted; surviving edges referencing the removed id become dangling — wire_doctor flags them."
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

    /// One-shot entry: discover persona-scoped wiring entries, fetch each axis
    /// via the Layer 6 Adapter, render with the registered NamedProjection
    /// (optionally merged with a persona-pack overlay), and concatenate the
    /// rendered blocks into a single PromptContext.
    #[tool(
        name = "wire_prompt_context",
        description = "Run every registered NamedProjection through the Layer 6 Adapter (mini-app:// / file:// schemes supported) to fresh-fetch each wiring entry's source_uri, render via handlebars, and return the concatenated PromptContext literal in one call. Used as the `/wake` auto-load entry — wire holds wiring metadata only, data lives in the SoT (mini-app / file / outline)."
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

    /// Audit graph coverage — classify Nodes into declared_covered /
    /// declared_uncovered / undeclared / exempt based on
    /// `metadata.maintained_by` / `metadata.maintenance_exempt` and the set
    /// of enabled workflow_def Nodes.
    #[tool(
        name = "wire_workflow_check",
        description = "Audit graph coverage: classify each Node (excluding workflow_def) into declared_covered / declared_uncovered / undeclared / exempt by comparing metadata.maintained_by (declared maintenance plan) against the set of enabled workflow_def Nodes. See docs/wire-workflow-spec.md §6.5."
    )]
    async fn wire_workflow_check_tool(
        &self,
        Parameters(p): Parameters<WireWorkflowCheckParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: p.persona_id,
                include_exempt: p.include_exempt,
                include_covered: p.include_covered,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
        let declared_uncovered: Vec<serde_json::Value> = out
            .declared_uncovered
            .into_iter()
            .map(|u| {
                serde_json::json!({
                    "node_id": u.node_id,
                    "type": u.r#type,
                    "persona": u.persona,
                    "axis": u.axis,
                    "reasons": u.reasons,
                })
            })
            .collect();
        let undeclared: Vec<serde_json::Value> = out
            .undeclared
            .into_iter()
            .map(|u| {
                serde_json::json!({
                    "node_id": u.node_id,
                    "type": u.r#type,
                    "persona": u.persona,
                    "axis": u.axis,
                })
            })
            .collect();
        let exempt: Vec<serde_json::Value> = out
            .exempt
            .into_iter()
            .map(|e| serde_json::json!({ "node_id": e.node_id, "reason": e.reason }))
            .collect();
        let declared_covered: Vec<serde_json::Value> = out
            .declared_covered
            .into_iter()
            .map(|c| {
                serde_json::json!({
                    "node_id": c.node_id,
                    "axis": c.axis,
                    "covering_workflow_id": c.covering_workflow_id,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "total_nodes": out.total_nodes,
            "declared_covered_count": out.declared_covered_count,
            "declared_covered": declared_covered,
            "declared_uncovered": declared_uncovered,
            "undeclared": undeclared,
            "exempt": exempt,
            "workflows_observed": out.workflows_observed,
        }))
        .map_err(|e| e.to_string())
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
             wire_workflow_delete / wire_workflow_check. \
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
