//! persona-wire Adapter for any MCP server (scheme `mcp://`).
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
//! - `<server>` — server alias resolved against the `endpoints` map passed to
//!   [`McpAdapter::new`].
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
use std::time::Duration;

use async_trait::async_trait;
use percent_encoding::percent_decode_str;
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

// ── Server endpoint config ────────────────────────────────────────────────────

/// Resolved endpoint for a server alias.
///
/// Caller (Wire runtime / `persona-wire-mcp` registry) assembles the map.
/// File / env loading is out of scope for this adapter.
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

// ── Adapter struct ────────────────────────────────────────────────────────────

/// persona-wire Adapter for MCP servers (`mcp://` scheme).
pub struct McpAdapter {
    endpoints: BTreeMap<String, ServerEndpoint>,
    rpc_timeout: Duration,
}

impl McpAdapter {
    /// Construct an adapter with the given server alias map.
    ///
    /// Uses [`DEFAULT_RPC_TIMEOUT`] (30s) for every RPC.
    pub fn new(endpoints: BTreeMap<String, ServerEndpoint>) -> Self {
        Self {
            endpoints,
            rpc_timeout: DEFAULT_RPC_TIMEOUT,
        }
    }

    /// Construct an adapter with a caller-specified RPC timeout.
    ///
    /// `rpc_timeout` must be non-zero; otherwise every RPC would time out
    /// immediately and the failure mode would be indistinguishable from
    /// "everything is broken".
    pub fn with_rpc_timeout(
        endpoints: BTreeMap<String, ServerEndpoint>,
        rpc_timeout: Duration,
    ) -> WireResult<Self> {
        if rpc_timeout.is_zero() {
            return Err(WireError::Storage(
                "mcp adapter: rpc_timeout must be > 0".to_string(),
            ));
        }
        Ok(Self {
            endpoints,
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

    async fn fetch(&self, uri: &WireUri) -> WireResult<Value> {
        let spec = parse_mcp_uri(uri)?;
        let endpoint = self.endpoints.get(&spec.server).ok_or_else(|| {
            WireError::Storage(format!(
                "mcp adapter: unknown server alias '{}'",
                spec.server
            ))
        })?;

        // Connect (stateless: per-fetch). `()` is a minimal `ClientHandler`
        // with no-op notification handling — adequate for single-shot fetch.
        let running = connect(endpoint, self.rpc_timeout).await?;

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

    #[test]
    fn adapter_unknown_alias_errors() {
        use tokio::runtime::Runtime;
        let adapter = McpAdapter::new(BTreeMap::new());
        let rt = Runtime::new().unwrap();
        let uri = WireUri::parse("mcp://nowhere/tools/t").unwrap();
        let err = rt.block_on(adapter.fetch(&uri)).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown server alias 'nowhere'"),
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
