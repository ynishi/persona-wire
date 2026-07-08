# persona-wire-mcp 0.12.0

persona-wire MCP server library — the transport layer that wraps
[`persona_wire_core`] for consumption by MCP clients (Claude Code,
`mcp://` peers, etc.).

This crate exposes [`serve_stdio`] for the unified
`persona-wire mcp` subcommand to dispatch into. Transport is rmcp
stdio (see [`ServiceExt`] plumbing); the tool surface is defined
by the [`WireServer`] struct, whose methods are annotated with
`#[rmcp::tool]` and enumerated into a [`ToolRouter`] at boot.

[`WireServer::new`] constructs a persistent [`SqliteStorage`] +
[`PluginRegistry`] pair once at startup. The registry combines
core defaults (FileAdapter + HandlebarsEngine + StaticProjection)
with the ten external adapter crates
(`persona-wire-adapter-{mini-app, sqlite-x, obsidian,
persona-pack, mcp, rss, github, todoist, notion, slack}`), so
every scheme-tagged URI a caller passes to `wire_prompt_context`,
`wire_render`, or `wire_workflow_fire` resolves through the same
pipeline.

No CLI parsing / entry-point code lives here — the `persona-wire`
binary in the sibling crate is the only intended caller, and its
`mcp` subcommand's job is to build a [`SqliteStorage`] pointing at
the operator's on-disk DB and hand it to `serve_stdio`.

