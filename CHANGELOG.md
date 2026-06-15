# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

## [0.1.0] - 2026-06-15

### Added

- **MCP Tool surface** (`persona-wire-mcp` crate):
  - `wire_init` / `wire_close` — graph lifecycle (P1)
  - `wire_node_create` / `wire_edge_create` — primitive CRUD (P1)
  - `wire_spec_register` / `wire_projection_register` — Specification + NamedProjection registry (P1)
  - `wire_doctor` — graph-wide health diagnostic (Orphan detection) (P2a)
  - `wire_nodes_create_batch` / `wire_edges_create_batch` — bulk import (P2c)
  - `wire_query` — ad-hoc Specification query without registration (P2b)
  - `wire_render` — individual NamedProjection render by name (P2b)
- **CLI** (`persona-wire` unified binary):
  - subcommands: `init` / `node` / `edge` / `spec` / `projection` / `wire-init` / `wire-close` / `wire-doctor` / `query` / `render` / `mcp`
  - `mcp` subcommand serves the stdio MCP server in-process (no separate binary)
- **Storage** (`persona-wire-core::infrastructure::storage`):
  - SQLite 4-table schema (nodes / edges / specs / projections) + 4 indexes
  - 18 seed Node types
  - `default_db_path()` resolution: `PERSONA_WIRE_DB` env > `--db <path>` flag > `~/.persona-wire/store.db`
- **Domain** (`persona-wire-core::domain`):
  - `Specification` pattern (Evans / Fowler / Greg Young) — composable predicate with `And` / `Or` / `Not` combinators + `impl std::ops::Not for Specification` (`!spec` syntax)
  - `Repository` trait + Compute BFS for Specification evaluation
- **Application** (`persona-wire-core::application`):
  - `SpecRegistry` + `ProjectionRegistry` (Use Case scoped registries)
  - `wire_init` / `wire_close` use cases with `WireInitInput` / `WireInitOutput` etc. typed boundaries
- **Rendering** (`persona-wire-core::infrastructure::rendering`):
  - Minimal mustache template engine with nested path support (`{{a.b.c}}`)
- **docs**:
  - `docs/concept-2026-06-14.md` — architecture / concept SoT (DDD + Hexagonal + Specification + NamedProjection layering)
  - `docs/runbook-verify.md` — TC-001〜TC-008 manual verification SoT
  - `docs/wire-query-spec.md` — `wire_query` Tool specification (9 chapters, 313 lines)

### Changed

- **Crate unification**: `persona-wire-mcp` (binary) + `persona-wire-cli` (binary) → `persona-wire` (unified binary with `mcp` subcommand). `persona-wire-mcp` crate becomes library-only and exposes `serve_stdio(db_path)`.
- **Tool naming**: `pnet_*` → `wire_*` (`pnet_init` → `wire_init` / `pnet_close` → `wire_close`). All MCP Tool surfaces now share the `wire_` prefix.
- **CLI flag semantic-first naming**:
  - `--persona` → `--persona-id` (canonical) + `persona` alias for backward compat
  - `--json` → `--spec` (canonical) + `json` alias for backward compat
- **Storage path resolution**: 3-tier priority (`PERSONA_WIRE_DB` env > `--db` flag > `~/.persona-wire/store.db`) — eliminates CWD-relative pollution caused by `.mcp.json`-driven startup.
- **Test fixtures**: persona-specific literals (`shi` / `mia` / `misaki` / `ytk`) → generic placeholders (`alpha` / `beta` / `gamma` / `user_a`) across 6 test / source files.
- **`Specification::not()` inherent method**: removed in favour of `impl std::ops::Not` (clippy `should_implement_trait` resolution).

### Fixed

- **DB path CWD pollution**: `.mcp.json`-driven startup auto-created `~/projects/persona-wire/persona-wire.db` under the project root, polluting the working tree. `storage::default_db_path()` now centralises the resolution under `~/.persona-wire/store.db` by default.

### Security

- **Internal token / persona-literal leak removal**: test fixtures, docs, and README were sanitised of persona-specific identifiers, internal issue IDs, and project labels. Each commit in the 7-commit chain leading to this release was verified by `publish-checker` + `secret-pre-commit-checker` + `content-hygiene-pre-commit-checker` (4-gate sweep).

[Unreleased]: https://github.com/ynishi/persona-wire/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ynishi/persona-wire/compare/441a727...v0.1.0
