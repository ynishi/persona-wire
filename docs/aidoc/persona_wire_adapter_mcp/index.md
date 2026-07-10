# persona-wire-adapter-mcp 0.12.1

persona-wire Adapter for any MCP server (scheme `mcp://`).

Adapter type: [`McpAdapter`]. Wire into a
[`persona_wire_core::application::plugin_registry::PluginRegistry`]
via `.with_adapter(McpAdapter::new(resolver))` (see [`WireServer`]
in `persona-wire-mcp` for the production wiring).

Routes tool calls and resource reads to MCP servers via rmcp 1.x.
Supports stdio (`transport-child-process`) and streamable HTTP
(`transport-streamable-http-client`) transports.

## URI Grammar

```text
mcp://<server>/tools/<tool_name>[?<args>]
mcp://<server>/resources?uri=<resource_uri>
mcp://<server>/resources/<resource_uri_passthrough>
```

- `<server>` — server alias resolved via the [`McpEndpointResolver`]
  passed to [`McpAdapter::new`]. The production resolver
  ([`SqliteEndpointResolver`]) looks up a graph node with
  `type = "mcp_server"` (see [`MCP_SERVER_NODE_TYPE`]) and reads
  `metadata.endpoint` as a [`ServerEndpoint`].
- `path` 1st segment dispatches the RPC kind:
  - `tools/<tool_name>` → `tools/call`
  - `resources?uri=<encoded>` → `resources/read` (default form)
  - `resources/<passthrough>` → `resources/read` (sugar; only when the
    passthrough does not contain `:` after percent-decoding).
- tool args are passed as scalar query params (`?key=value`) or as a single
  JSON-encoded object via `?_args=<json>`. The `_args` form wins if both
  are present.

## Lifecycle

Each `fetch` is **stateless**: connect → call → disconnect. Connection
pooling is out of scope for the single-shot Wire use case.

