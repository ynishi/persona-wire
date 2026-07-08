//! persona-wire — CLI + migration hub for the 15-crate persona-wire
//! workspace. persona-wire is a graph engine over persona × SoT ×
//! workflow context routing; the workspace splits into (a) the pure
//! domain / application / infrastructure core (`persona-wire-core`),
//! (b) the MCP server library (`persona-wire-mcp`) that wraps the core
//! for `serve_stdio`, (c) ten `persona-wire-adapter-*` crates that
//! dispatch scheme-tagged URIs (`github://`, `mcp://`, `mini-app://`,
//! `notion://`, `obsidian://`, `persona-pack://`, `rss://`, `slack://`,
//! `sqlite://`, `todoist://`), and (d) shared infrastructure
//! (`persona-wire-transport-http` for reqwest+rustls, and
//! `persona-wire-credentials` for the Env→Keyring provider chain).
//!
//! This crate (the `persona-wire` binary itself) is the thin CLI
//! surface: it hosts three binary targets — `persona-wire` (the
//! unified subcommand entry point including `persona-wire mcp` which
//! dispatches into `persona-wire-mcp::serve_stdio`), `pw-migrate` (the
//! migration driver over the framework below), and
//! `migrate_id_to_ulid` (a deprecated alias of `pw-migrate` kept for
//! release-note continuity). Only modules that need to be shared
//! across those binaries (or used by external integration tests) live
//! in `lib.rs`; the CLI wiring proper lives in `main.rs` / `bin/`.
//!
//! The shared surface here is the [`migrations`] module, which holds
//! the numbered schema migration framework consumed by `pw-migrate`.

pub mod migrations;
