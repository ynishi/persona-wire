//! persona-wire Adapter for any MCP server (scheme `mcp://`).
//!
//! Adapter type: [`McpAdapter`]. Wire into a
//! [`persona_wire_core::application::plugin_registry::PluginRegistry`]
//! via `.with_adapter(McpAdapter::new(resolver))` (see [`WireServer`]
//! in `persona-wire-mcp` for the production wiring).
//!
//! Routes tool calls and resource reads to MCP servers via rmcp 1.x.
//! Supports stdio (`transport-child-process`) and streamable HTTP
//! (`transport-streamable-http-client`) transports.
//!
//! ## URI Grammar
//!
//! ```text
//! mcp://<server>/tools/<tool_name>[?<args>]
//! mcp://<server>/resources?uri=<resource_uri>
//! mcp://<server>/resources/<resource_uri_passthrough>
//! ```
//!
//! - `<server>` — server alias resolved via the [`McpEndpointResolver`]
//!   passed to [`McpAdapter::new`]. The production resolver
//!   ([`SqliteEndpointResolver`]) looks up a graph node with
//!   `type = "mcp_server"` (see [`MCP_SERVER_NODE_TYPE`]) and reads
//!   `metadata.endpoint` as a [`ServerEndpoint`].
//! - `path` 1st segment dispatches the RPC kind:
//!   - `tools/<tool_name>` → `tools/call`
//!   - `resources?uri=<encoded>` → `resources/read` (default form)
//!   - `resources/<passthrough>` → `resources/read` (sugar; only when the
//!     passthrough does not contain `:` after percent-decoding).
//! - tool args are passed as scalar query params (`?key=value`) or as a single
//!   JSON-encoded object via `?_args=<json>`. The `_args` form wins if both
//!   are present.
//!
//! ## Lifecycle
//!
//! Each `fetch` is **stateless**: connect → call → disconnect. Connection
//! pooling is out of scope for the single-shot Wire use case.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use percent_encoding::percent_decode_str;
use persona_wire_core::infrastructure::storage::SqliteStorage;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use rmcp::{
    model::{CallToolRequestParams, ReadResourceRequestParams},
    transport::TokioChildProcess,
    ServiceExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::warn;

/// Default per-call RPC timeout. Matches `agent-block::mcp_client::DEFAULT_RPC_TIMEOUT`.
pub const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Node `type` literal that identifies an MCP server endpoint in the graph.
///
/// Shared between [`SqliteEndpointResolver`] (consumer) and callers that
/// register endpoint nodes via `wire_node_create(type = "mcp_server", ...)`.
/// `workflow_def` の `WORKFLOW_TYPE` 定数と同じ運用 — vocabulary 登録 layer は
/// 存在せず、 literal 一致と `maintenance_exempt: true` で orphan 判定を抑制する。
pub const MCP_SERVER_NODE_TYPE: &str = "mcp_server";

/// `metadata.endpoint` key holding the serialized [`ServerEndpoint`] payload.
pub const META_ENDPOINT: &str = "endpoint";

// ── Server endpoint config ────────────────────────────────────────────────────

/// Resolved endpoint for a server alias.
///
/// Caller (Wire runtime / `persona-wire-mcp` registry) supplies the
/// [`McpEndpointResolver`] that produces these values; for the production
/// graph-based path see [`SqliteEndpointResolver`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ServerEndpoint {
    /// Spawn an MCP server as a child process and talk over stdio.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    /// Talk to an MCP server over Streamable HTTP.
    Http { url: String },
}

// ── Endpoint resolver abstraction ─────────────────────────────────────────────

/// Resolve a server alias to a [`ServerEndpoint`] connectable by the adapter.
///
/// Production implementation: [`SqliteEndpointResolver`] reads `mcp_server`
/// nodes from the Wire graph. Test implementations typically use a static
/// `BTreeMap`-backed resolver so unit tests do not need a live storage.
///
/// Errors are returned with a hint about *why* resolution failed (alias
/// unknown, node type mismatch, metadata malformed) — callers do not need to
/// second-guess.
#[async_trait]
pub trait McpEndpointResolver: Send + Sync {
    async fn resolve(&self, alias: &str) -> WireResult<ServerEndpoint>;
}

/// Graph-backed resolver: looks up a node with `type = "mcp_server"` and
/// returns its `metadata.endpoint` deserialized as a [`ServerEndpoint`].
///
/// The shared `Arc<Mutex<SqliteStorage>>` is the same handle held by
/// `WireServer` — so endpoint changes go live the moment they are committed
/// via `wire_node_create` / `wire_node_update`.
pub struct SqliteEndpointResolver {
    storage: Arc<Mutex<SqliteStorage>>,
}

impl SqliteEndpointResolver {
    pub fn new(storage: Arc<Mutex<SqliteStorage>>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl McpEndpointResolver for SqliteEndpointResolver {
    async fn resolve(&self, alias: &str) -> WireResult<ServerEndpoint> {
        // Sync lock + sync get_node — guard is dropped before any await.
        let node_opt = {
            let guard = self.storage.lock().map_err(|e| {
                WireError::Storage(format!("mcp adapter: storage lock poisoned: {e}"))
            })?;
            guard.get_node_by_name(alias)?
        };
        let node = node_opt.ok_or_else(|| {
            WireError::Storage(format!(
                "mcp adapter: unknown server alias '{alias}' \
                 (no graph node with type='{MCP_SERVER_NODE_TYPE}' and id='{alias}')"
            ))
        })?;
        if node.r#type != MCP_SERVER_NODE_TYPE {
            return Err(WireError::Storage(format!(
                "mcp adapter: alias '{alias}' is not an {MCP_SERVER_NODE_TYPE} node \
                 (got type='{}')",
                node.r#type
            )));
        }
        let endpoint_value = node.metadata.get(META_ENDPOINT).ok_or_else(|| {
            WireError::Storage(format!(
                "mcp adapter: node '{alias}' missing metadata.{META_ENDPOINT} \
                 (expected ServerEndpoint JSON)"
            ))
        })?;
        serde_json::from_value::<ServerEndpoint>(endpoint_value.clone()).map_err(|e| {
            WireError::Storage(format!(
                "mcp adapter: node '{alias}' metadata.{META_ENDPOINT} malformed: {e}"
            ))
        })
    }
}

// ── Adapter struct ────────────────────────────────────────────────────────────

/// persona-wire Adapter for MCP servers (`mcp://` scheme).
pub struct McpAdapter {
    resolver: Arc<dyn McpEndpointResolver>,
    rpc_timeout: Duration,
}

impl McpAdapter {
    /// Construct an adapter with the given endpoint resolver.
    ///
    /// Uses [`DEFAULT_RPC_TIMEOUT`] (30s) for every RPC.
    pub fn new(resolver: Arc<dyn McpEndpointResolver>) -> Self {
        Self {
            resolver,
            rpc_timeout: DEFAULT_RPC_TIMEOUT,
        }
    }

    /// Construct an adapter with a caller-specified RPC timeout.
    ///
    /// `rpc_timeout` must be non-zero; otherwise every RPC would time out
    /// immediately and the failure mode would be indistinguishable from
    /// "everything is broken".
    pub fn with_rpc_timeout(
        resolver: Arc<dyn McpEndpointResolver>,
        rpc_timeout: Duration,
    ) -> WireResult<Self> {
        if rpc_timeout.is_zero() {
            return Err(WireError::Storage(
                "mcp adapter: rpc_timeout must be > 0".to_string(),
            ));
        }
        Ok(Self {
            resolver,
            rpc_timeout,
        })
    }
}

// ── URI parse ─────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum McpUriKind {
    /// `tools/<tool_name>` — `tools/call` RPC.
    Tool { tool_name: String },
    /// `resources?uri=<...>` or `resources/<passthrough>` — `resources/read` RPC.
    Resource { resource_uri: String },
}

#[derive(Debug)]
struct McpUriSpec {
    server: String,
    kind: McpUriKind,
    /// Tool arguments as a JSON object (empty for resource reads).
    args: Map<String, Value>,
}

/// Parse a `WireUri` (already split into typed components) into an `McpUriSpec`.
///
/// `WireUri::parse` has already validated the `<scheme>://<host>/<path>?<query>`
/// shape; here we enforce the `mcp://` grammar on top.
fn parse_mcp_uri(uri: &WireUri) -> WireResult<McpUriSpec> {
    if uri.scheme() != "mcp" {
        return Err(WireError::Storage(format!(
            "mcp adapter: scheme must be 'mcp' (got '{}')",
            uri.scheme()
        )));
    }
    let server = uri
        .host()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            WireError::Storage(format!(
                "mcp adapter: missing server alias in '{}'",
                uri.as_raw()
            ))
        })?
        .to_string();

    // Path always begins with `/` because WireUri preserves it after the
    // authority split. Strip the leading slash and dispatch on the first
    // segment.
    let path = uri.path().trim_start_matches('/');
    let (kind_seg, rest) = match path.split_once('/') {
        Some((k, r)) => (k, r),
        None => (path, ""),
    };

    let kind = match kind_seg {
        "tools" => {
            if rest.is_empty() {
                return Err(WireError::Storage(format!(
                    "mcp adapter: missing tool name in '{}'",
                    uri.as_raw()
                )));
            }
            // `rest` must be a single segment (no further `/`); MCP tool names
            // are flat identifiers.
            if rest.contains('/') {
                return Err(WireError::Storage(format!(
                    "mcp adapter: tool name must be a single segment (got '{}')",
                    rest
                )));
            }
            // Percent-decode tool name to allow `_` / `-` and tolerate
            // encoded variants.
            let tool_name = decode_segment(rest)?;
            McpUriKind::Tool { tool_name }
        }
        "resources" => {
            // Preferred: query form `?uri=<encoded>`. Sugar: passthrough
            // path segment `resources/<rest>`, valid only when `<rest>`
            // contains no `:` after decoding (no nested scheme URIs).
            //
            // WireUri preserves query values raw, so we percent-decode here
            // — caller writes `?uri=outline%3A%2F%2Fnode%2F12345` and the
            // adapter passes `outline://node/12345` to `resources/read`.
            let resource_uri = if let Some(q) = uri.query_get("uri") {
                decode_segment(q)?
            } else if !rest.is_empty() {
                let decoded = decode_segment(rest)?;
                if decoded.contains(':') {
                    return Err(WireError::Storage(format!(
                        "mcp adapter: resource passthrough must not contain ':' \
                         (use ?uri=<encoded> form for nested-scheme URIs): '{decoded}'"
                    )));
                }
                decoded
            } else {
                return Err(WireError::Storage(format!(
                    "mcp adapter: missing resource uri in '{}' \
                     (use ?uri=<encoded> or /resources/<passthrough>)",
                    uri.as_raw()
                )));
            };
            McpUriKind::Resource { resource_uri }
        }
        other => {
            return Err(WireError::Storage(format!(
                "mcp adapter: unknown path kind '{other}' \
                 (expected 'tools' or 'resources') in '{}'",
                uri.as_raw()
            )));
        }
    };

    let args = match &kind {
        McpUriKind::Tool { .. } => parse_tool_args(uri)?,
        McpUriKind::Resource { .. } => Map::new(),
    };

    Ok(McpUriSpec { server, kind, args })
}

/// Parse tool args from URI query.
///
/// Precedence: `?_args=<json_object>` wins if present and parseable.
/// Otherwise, all `?key=value` pairs (except `_args`) form a flat
/// `{"key": "value"}` map.
fn parse_tool_args(uri: &WireUri) -> WireResult<Map<String, Value>> {
    if let Some(raw) = uri.query_get("_args") {
        // Percent-decode first — callers SHOULD encode `{`, `}`, `:`, `,`
        // per RFC 3986 strictness, but the raw form (browser-tolerated)
        // is also accepted as long as it round-trips to valid JSON.
        let decoded = decode_segment(raw)?;
        let parsed: Value = serde_json::from_str(&decoded).map_err(|e| {
            WireError::Storage(format!(
                "mcp adapter: _args is not valid JSON: {e} (raw: {raw})"
            ))
        })?;
        match parsed {
            Value::Object(obj) => return Ok(obj),
            other => {
                return Err(WireError::Storage(format!(
                    "mcp adapter: _args must be a JSON object (got {})",
                    json_kind(&other)
                )));
            }
        }
    }
    // WireUri preserves query values raw, so percent-decode each value here.
    // Keys are left as-is (MCP tool arg names should be plain identifiers).
    let mut args = Map::new();
    for (k, v) in uri.query() {
        if k == "_args" {
            continue;
        }
        let decoded = decode_segment(v)?;
        args.insert(k.clone(), Value::String(decoded));
    }
    Ok(args)
}

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn decode_segment(s: &str) -> WireResult<String> {
    percent_decode_str(s)
        .decode_utf8()
        .map(|c| c.into_owned())
        .map_err(|e| {
            WireError::Storage(format!(
                "mcp adapter: invalid percent-encoded segment '{s}': {e}"
            ))
        })
}

// ── Adapter impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Adapter for McpAdapter {
    fn scheme(&self) -> &'static str {
        "mcp"
    }

    /// `mcp://` is a passthrough scheme: every `?key=value` pair becomes a
    /// tool argument (see [`parse_tool_args`]), so the filter-vocabulary keys
    /// (`?query=`, `?limit=`, ...) are addressing here, not filters. The wire
    /// layer must never strip them for post-filtering (GH #10 opt-out).
    fn post_filterable(&self) -> bool {
        false
    }

    async fn fetch(&self, uri: &WireUri) -> WireResult<Value> {
        let spec = parse_mcp_uri(uri)?;
        let endpoint = self.resolver.resolve(&spec.server).await?;

        // Connect (stateless: per-fetch). `()` is a minimal `ClientHandler`
        // with no-op notification handling — adequate for single-shot fetch.
        let running = connect(&endpoint, self.rpc_timeout).await?;

        // Execute the RPC.
        let result = match &spec.kind {
            McpUriKind::Tool { tool_name } => {
                let mut params = CallToolRequestParams::new(tool_name.clone());
                if !spec.args.is_empty() {
                    params = params.with_arguments(spec.args.clone());
                }
                timeout(self.rpc_timeout, running.call_tool(params))
                    .await
                    .map_err(|_| {
                        warn!(server = %spec.server, tool = %tool_name, "mcp adapter: call_tool timed out");
                        WireError::Storage(format!(
                            "mcp adapter: rpc timeout ({:?}) calling tool '{}' on '{}'",
                            self.rpc_timeout, tool_name, spec.server
                        ))
                    })?
                    .map_err(|e| {
                        WireError::Storage(format!(
                            "mcp adapter: rpc failed: call_tool '{}' on '{}': {e}",
                            tool_name, spec.server
                        ))
                    })
                    .and_then(|r| {
                        serde_json::to_value(&r).map_err(|e| {
                            WireError::Storage(format!(
                                "mcp adapter: serialize call_tool result: {e}"
                            ))
                        })
                    })
            }
            McpUriKind::Resource { resource_uri } => {
                let params = ReadResourceRequestParams::new(resource_uri.clone());
                timeout(self.rpc_timeout, running.read_resource(params))
                    .await
                    .map_err(|_| {
                        warn!(server = %spec.server, uri = %resource_uri, "mcp adapter: read_resource timed out");
                        WireError::Storage(format!(
                            "mcp adapter: rpc timeout ({:?}) reading resource '{}' on '{}'",
                            self.rpc_timeout, resource_uri, spec.server
                        ))
                    })?
                    .map_err(|e| {
                        WireError::Storage(format!(
                            "mcp adapter: rpc failed: read_resource '{}' on '{}': {e}",
                            resource_uri, spec.server
                        ))
                    })
                    .and_then(|r| {
                        serde_json::to_value(&r).map_err(|e| {
                            WireError::Storage(format!(
                                "mcp adapter: serialize read_resource result: {e}"
                            ))
                        })
                    })
            }
        };

        // Disconnect (best-effort; do not mask the primary result).
        if let Err(e) = timeout(self.rpc_timeout, running.cancel()).await {
            warn!(
                server = %spec.server,
                error = ?e,
                "mcp adapter: disconnect timed out (ignored)"
            );
        }

        result
    }
}

// ── Transport-specific connect helpers ────────────────────────────────────────

/// Resolve a server endpoint to a connected `RunningService<RoleClient, ()>`.
async fn connect(
    endpoint: &ServerEndpoint,
    rpc_timeout: Duration,
) -> WireResult<rmcp::service::RunningService<rmcp::service::RoleClient, ()>> {
    match endpoint {
        ServerEndpoint::Stdio { command, args, env } => {
            let mut cmd = Command::new(command);
            cmd.args(args).stderr(Stdio::inherit());
            for (k, v) in env {
                cmd.env(k, v);
            }
            let transport = TokioChildProcess::new(cmd).map_err(|e| {
                WireError::Storage(format!(
                    "mcp adapter: connect failed: spawn '{command}': {e}"
                ))
            })?;
            let running = timeout(rpc_timeout, ().serve(transport))
                .await
                .map_err(|_| {
                    WireError::Storage(format!(
                        "mcp adapter: rpc timeout ({rpc_timeout:?}) during MCP initialize"
                    ))
                })?
                .map_err(|e| {
                    WireError::Storage(format!("mcp adapter: connect failed: initialize: {e}"))
                })?;
            Ok(running)
        }
        ServerEndpoint::Http { url } => {
            use rmcp::transport::StreamableHttpClientTransport;
            let transport = StreamableHttpClientTransport::from_uri(url.clone());
            let running = timeout(rpc_timeout, ().serve(transport))
                .await
                .map_err(|_| {
                    WireError::Storage(format!(
                        "mcp adapter: rpc timeout ({rpc_timeout:?}) during HTTP MCP initialize"
                    ))
                })?
                .map_err(|e| {
                    WireError::Storage(format!(
                        "mcp adapter: connect failed: HTTP initialize on '{url}': {e}"
                    ))
                })?;
            Ok(running)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(uri: &str) -> WireResult<McpUriSpec> {
        let wire = WireUri::parse(uri)?;
        parse_mcp_uri(&wire)
    }

    struct NeverResolver;

    #[async_trait]
    impl McpEndpointResolver for NeverResolver {
        async fn resolve(&self, alias: &str) -> WireResult<ServerEndpoint> {
            Err(WireError::Storage(format!("test resolver: {alias}")))
        }
    }

    #[test]
    fn mcp_adapter_opts_out_of_wire_post_filtering() {
        // `?query=` etc. are tool arguments on this passthrough scheme — the
        // wire layer must never strip them (GH #10 opt-out contract).
        let adapter = McpAdapter::new(Arc::new(NeverResolver));
        assert!(!adapter.post_filterable());
    }

    #[test]
    fn parse_tool_scalar_args() {
        let spec = parse("mcp://outline/tools/node_query?slug=rust&q=ownership").unwrap();
        assert_eq!(spec.server, "outline");
        match spec.kind {
            McpUriKind::Tool { tool_name } => assert_eq!(tool_name, "node_query"),
            _ => panic!("expected Tool"),
        }
        assert_eq!(
            spec.args.get("slug").unwrap(),
            &Value::String("rust".into())
        );
        assert_eq!(
            spec.args.get("q").unwrap(),
            &Value::String("ownership".into())
        );
    }

    #[test]
    fn parse_tool_args_json() {
        let spec =
            parse(r#"mcp://mini-app/tools/list?_args={"table":"issue","filter":{"type":"eq"}}"#)
                .unwrap();
        assert_eq!(spec.server, "mini-app");
        let table = spec.args.get("table").unwrap();
        assert_eq!(table, &Value::String("issue".into()));
        let filter = spec.args.get("filter").unwrap();
        assert!(filter.is_object());
    }

    #[test]
    fn parse_tool_args_json_overrides_scalar() {
        let spec = parse(r#"mcp://x/tools/t?ignored=v&_args={"key":"val"}"#).unwrap();
        // `ignored` from scalar form must NOT leak in when _args wins.
        assert!(spec.args.get("ignored").is_none());
        assert_eq!(spec.args.get("key").unwrap(), &Value::String("val".into()));
    }

    #[test]
    fn parse_tool_args_no_args() {
        let spec = parse("mcp://x/tools/t").unwrap();
        assert!(spec.args.is_empty());
    }

    #[test]
    fn parse_resource_query_form() {
        let spec = parse("mcp://outline/resources?uri=outline%3A%2F%2Fnode%2F12345").unwrap();
        match spec.kind {
            McpUriKind::Resource { resource_uri } => {
                // Note: WireUri's parse_query already percent-decodes values,
                // so the resource_uri here should be the decoded form.
                assert_eq!(resource_uri, "outline://node/12345");
            }
            _ => panic!("expected Resource"),
        }
    }

    #[test]
    fn parse_resource_passthrough_sugar() {
        let spec = parse("mcp://hn/resources/item/123").unwrap();
        match spec.kind {
            McpUriKind::Resource { resource_uri } => {
                assert_eq!(resource_uri, "item/123");
            }
            _ => panic!("expected Resource"),
        }
    }

    #[test]
    fn parse_resource_passthrough_rejects_colon() {
        let err = parse("mcp://hn/resources/scheme%3Apath").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("must not contain ':'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        let err = parse("mcp://x/prompts/p").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown path kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_rejects_missing_tool_name() {
        let err = parse("mcp://x/tools/").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing tool name"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_rejects_multi_segment_tool() {
        let err = parse("mcp://x/tools/a/b").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("single segment"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_rejects_missing_resource_uri() {
        let err = parse("mcp://x/resources").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("missing resource uri"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_rejects_missing_server_alias() {
        let err = parse("mcp:///tools/t").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("missing server alias"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_rejects_wrong_scheme() {
        let wire = WireUri::parse("file:///etc/hosts").unwrap();
        let err = parse_mcp_uri(&wire).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("scheme must be 'mcp'"),
            "unexpected error: {msg}"
        );
    }

    /// In-test resolver: returns whatever the caller put in the map, or an
    /// `unknown server alias` error matching the production hint shape.
    struct StaticResolver(BTreeMap<String, ServerEndpoint>);

    #[async_trait]
    impl McpEndpointResolver for StaticResolver {
        async fn resolve(&self, alias: &str) -> WireResult<ServerEndpoint> {
            self.0.get(alias).cloned().ok_or_else(|| {
                WireError::Storage(format!(
                    "mcp adapter: unknown server alias '{alias}' \
                     (no graph node with type='{MCP_SERVER_NODE_TYPE}' and id='{alias}')"
                ))
            })
        }
    }

    #[test]
    fn adapter_unknown_alias_errors() {
        use tokio::runtime::Runtime;
        let resolver: Arc<dyn McpEndpointResolver> = Arc::new(StaticResolver(BTreeMap::new()));
        let adapter = McpAdapter::new(resolver);
        let rt = Runtime::new().unwrap();
        let uri = WireUri::parse("mcp://nowhere/tools/t").unwrap();
        let err = rt.block_on(adapter.fetch(&uri)).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown server alias 'nowhere'"),
            "unexpected error: {msg}"
        );
        // Graph-aware hint must be carried through verbatim by the production
        // SqliteEndpointResolver — guarded so the message form does not silently drift.
        assert!(
            msg.contains("type='mcp_server'"),
            "expected graph-aware hint, got: {msg}"
        );
    }

    // ── SqliteEndpointResolver — graph round-trip ──────────────────────────────

    fn sqlite_resolver_with_node(
        node_id: &str,
        node_type: &str,
        metadata: serde_json::Value,
    ) -> SqliteEndpointResolver {
        use persona_wire_core::domain::graph::{ulid_from_seed, Node};
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.migrate().unwrap();
        storage.seed_default_types().unwrap();
        let node = Node {
            id: ulid_from_seed(node_id),
            name: node_id.to_string(),
            r#type: node_type.to_string(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata,
        };
        storage.insert_node(&node).unwrap();
        SqliteEndpointResolver::new(Arc::new(Mutex::new(storage)))
    }

    #[test]
    fn sqlite_resolver_resolves_stdio_endpoint() {
        use tokio::runtime::Runtime;
        let resolver = sqlite_resolver_with_node(
            "outline",
            MCP_SERVER_NODE_TYPE,
            serde_json::json!({
                "endpoint": {"kind": "stdio", "command": "outline-mcp", "args": ["--foo"]},
                "maintenance_exempt": true,
            }),
        );
        let rt = Runtime::new().unwrap();
        let ep = rt.block_on(resolver.resolve("outline")).unwrap();
        match ep {
            ServerEndpoint::Stdio { command, args, .. } => {
                assert_eq!(command, "outline-mcp");
                assert_eq!(args, vec!["--foo".to_string()]);
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn sqlite_resolver_missing_node_errors() {
        use tokio::runtime::Runtime;
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.migrate().unwrap();
        storage.seed_default_types().unwrap();
        let resolver = SqliteEndpointResolver::new(Arc::new(Mutex::new(storage)));
        let rt = Runtime::new().unwrap();
        let err = rt.block_on(resolver.resolve("ghost")).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown server alias 'ghost'") && msg.contains("type='mcp_server'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn sqlite_resolver_wrong_type_errors() {
        use tokio::runtime::Runtime;
        let resolver = sqlite_resolver_with_node(
            "wrong",
            "persona",
            serde_json::json!({"endpoint": {"kind": "stdio", "command": "x"}}),
        );
        let rt = Runtime::new().unwrap();
        let err = rt.block_on(resolver.resolve("wrong")).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not an mcp_server node") && msg.contains("got type='persona'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn sqlite_resolver_missing_endpoint_field_errors() {
        use tokio::runtime::Runtime;
        let resolver = sqlite_resolver_with_node(
            "barebones",
            MCP_SERVER_NODE_TYPE,
            serde_json::json!({"maintenance_exempt": true}),
        );
        let rt = Runtime::new().unwrap();
        let err = rt.block_on(resolver.resolve("barebones")).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("missing metadata.endpoint"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn sqlite_resolver_malformed_endpoint_errors() {
        use tokio::runtime::Runtime;
        let resolver = sqlite_resolver_with_node(
            "bad",
            MCP_SERVER_NODE_TYPE,
            // Missing required `kind` discriminant → serde fails.
            serde_json::json!({"endpoint": {"command": "x"}}),
        );
        let rt = Runtime::new().unwrap();
        let err = rt.block_on(resolver.resolve("bad")).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("metadata.endpoint malformed"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn server_endpoint_serde_roundtrip_stdio() {
        let ep = ServerEndpoint::Stdio {
            command: "outline-mcp".to_string(),
            args: vec!["--config".into(), "conf.toml".into()],
            env: BTreeMap::from([("FOO".to_string(), "bar".to_string())]),
        };
        let json = serde_json::to_string(&ep).unwrap();
        let back: ServerEndpoint = serde_json::from_str(&json).unwrap();
        match back {
            ServerEndpoint::Stdio { command, args, env } => {
                assert_eq!(command, "outline-mcp");
                assert_eq!(args.len(), 2);
                assert_eq!(env.get("FOO").unwrap(), "bar");
            }
            _ => panic!("expected Stdio"),
        }
    }

    #[test]
    fn server_endpoint_serde_roundtrip_http() {
        let ep = ServerEndpoint::Http {
            url: "http://localhost:8000".to_string(),
        };
        let json = serde_json::to_string(&ep).unwrap();
        let back: ServerEndpoint = serde_json::from_str(&json).unwrap();
        match back {
            ServerEndpoint::Http { url } => assert_eq!(url, "http://localhost:8000"),
            _ => panic!("expected Http"),
        }
    }
}
