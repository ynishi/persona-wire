//! persona-wire MCP server entry point.
//!
//! Wraps `persona-wire-core` use cases as MCP Tools via `rmcp`.
//! Tool surface (planned):
//! - `pnet_init` / `pnet_close` / `pnet_doctor` / `pnet_query`
//! - `pnet_update` / `pnet_net` / `pnet_spawn` / `pnet_retire`
//! - `wire_node_*` / `wire_edge_*` / `wire_type_register`
//! - `wire_spec_compose` / `wire_projection_register` / `wire_projection_run`
//! - `wire_friend_*` / `wire_workflow_*`

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("persona-wire-mcp starting (skeleton, no Tools wired yet)");

    // TODO(P1):
    //   1. instantiate persona_wire_core::infrastructure::storage::SqliteStorage
    //   2. build ServerHandler with Tools wired to use_cases
    //   3. rmcp transport-io stdio loop

    Ok(())
}
