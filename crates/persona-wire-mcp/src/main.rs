//! persona-wire MCP server entry point (rmcp stdio transport).

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
    pnet_close, pnet_init, PnetCloseInput, PnetInitInput,
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
pub struct PnetInitParams {
    /// Persona id for which the context bundle is rendered.
    pub persona_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PnetCloseParams {
    /// Persona id for which the lifecycle scan is reported.
    pub persona_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WireNodeCreateParams {
    pub id: String,
    /// Node type — must be in type_registry (e.g. "persona", "outline_node").
    #[serde(rename = "type")]
    pub type_: String,
    /// Optional SoT ref like "pp://shi".
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
        name = "pnet_init",
        description = "Run pnet_init: render every registered NamedProjection against the current graph; returns the rendered context bundle (one entry per projection)."
    )]
    async fn pnet_init_tool(
        &self,
        Parameters(p): Parameters<PnetInitParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = pnet_init(
            PnetInitInput {
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
        name = "pnet_close",
        description = "Run pnet_close: minimal lifecycle scan reporting total nodes / edges / orphan-node count, in a Markdown report."
    )]
    async fn pnet_close_tool(
        &self,
        Parameters(p): Parameters<PnetCloseParams>,
    ) -> Result<String, String> {
        let s = self.storage.lock().map_err(|e| e.to_string())?;
        let out = pnet_close(
            PnetCloseInput {
                persona_id: p.persona_id,
            },
            &s,
        )
        .map_err(|e| e.to_string())?;
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
             context routing. Tools: pnet_init / pnet_close / wire_node_create / \
             wire_edge_create / wire_spec_register / wire_projection_register.",
        )
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let db_path =
        std::env::var("PERSONA_WIRE_DB").unwrap_or_else(|_| "./persona-wire.db".to_string());
    tracing::info!(db = %db_path, "persona-wire-mcp starting");

    let storage = SqliteStorage::open(&db_path)?;
    storage.migrate()?;
    storage.seed_default_types()?;

    let server = WireServer::new(storage);
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}
