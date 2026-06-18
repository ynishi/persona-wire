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

## [0.2.1] - 2026-06-18

### Added

### Changed

- **`docs/onboarding.md §6b`** rewritten — the “Forward-looking — `wire_workflow_*` (P5, not yet implemented)” framing is replaced with “Implemented — `wire_workflow_*` (P5-a/a')” now that both tools ship. The section gains a generic JSON example for `wire_workflow_fire` (event-fanout) and `wire_workflow_check` (coverage audit). Declarative cadence (`every 7d`) and `wire_update` remain carry under `concept-2026-06-14.md`.
- **`docs/onboarding.md §6c`** new section — “Migrating from a per-persona config layer”. A generic three-step recipe (register one wire Node per axis → call `wire_prompt_context` from wake → fire `wire_workflow_fire` from close), plus a `projection_names: ["axis"]` subset note for wake-vs-close inject sizing. Uses generic placeholder paths only (e.g. `~/my-personas/<id>/work-config.toml`).
- **`docs/runbook-verify.md §TC-011`** describes the path-resolution priority generically (`env (PERSONA_WIRE_DB) > CLI flag (--db) > OS data dir (XDG_DATA_HOME / HOME)`) instead of referencing an external pattern label, so the runbook stays self-contained.
- **`crates/persona-wire-mcp/onboarding.md`** synced with the canonical `docs/onboarding.md` (the `build.rs` sync-drift guard rejects drift at dev-build time).

### Deprecated

### Removed

### Fixed

### Security

## [0.2.0] - 2026-06-17

### Added

- **P5-a `wire_workflow_*` declarative trigger/action seed** — register / list / fire / delete tools backed by the existing `workflow_def` node type. Trigger kinds: `on_demand` / `on_event`. Action kinds: `no_op` / `emit_projection`. The `emit_projection` action invokes `wire_prompt_context` internally and returns the rendered Markdown in `result.prompt_context`. Designed in `docs/wire-workflow-spec.md` (new file, P5 design draft).
- **P5-a' `wire_workflow_check` — graph coverage audit** — classifies each Node into `declared_covered` / `declared_uncovered` / `undeclared` / `exempt` by comparing `metadata.maintained_by` (declared maintenance plan) against the set of enabled `workflow_def` nodes. Coverage semantic: workflow covers node iff `action.kind == "emit_projection"` AND `workflow.persona == node.persona` AND `node.axis ∈ workflow.action.projection_names`. Designed in `docs/wire-workflow-spec.md §6.5`.
- **`docs/wire-workflow-spec.md`** (new file, ~330 lines): P5 WorkflowEngine seed design — mental model, Workflow Node data model, Trigger / Action forms, Tool surface, `wire_update` outline (P5-b carry), UC mapping to `onboarding §6b`, `wire_workflow_check` audit sibling (§6.5), implementation order (P5-a / P5-a' / P5-b〜P5-e), open questions.
- **`docs/onboarding.md §6b Loop / review / update-check trigger pattern`** (~120 lines): UC1-3 (session-close review / wake-time pending list / stale node surfacing), recipe using current `Specification` / `NamedProjection` / Adapter primitives, generic trigger layer (Skill / Command / Hook / cron), forward-looking note for `wire_workflow_*` (P5).
- `MiniAppAdapter::fetch_via_alias` resolves aliases through the mini-app
  `GlobalAliasStorage` (`_global.db`) with scope-aware lookup:
  `?scope=user` hits the User-scope `_global.db` as a hard target,
  `?scope=<project>&root=<dir>` hits the Project-scope `_global.db`,
  and the legacy URI form (no `?scope=`) falls back from User-scope
  global to the per-table `_aliases` SQLite table for backward
  compatibility. Resolves issue `8904d808-cff2-4788-b047-a77b21981492`
  (mini-app issue tracker).
- New E2E test suite `crates/persona-wire/tests/e2e_alias_scope.rs`
  exercises the scope resolution matrix end-to-end through the real
  `persona-wire mcp` stdio binary (7 axes: scope=user hit / scope=user
  miss / scope=&lt;project&gt; hit / scope=&lt;project&gt; without root /
  legacy URI global hit / legacy URI per-table fallback hit / legacy
  URI double miss).

### Changed

- Refactored E2E test fixtures into a shared `tests/common/mod.rs`
  module so the per-table legacy suite (`e2e_alias_mcp.rs`) and the
  new scope suite (`e2e_alias_scope.rs`) share `McpClient`, `Layout`,
  `bootstrap_mini_app_table*`, and `wire_one_axis` helpers without
  duplication.
- `docs/onboarding.md §2b` rewritten to reflect the new resolution
  matrix: both `_global.db` and per-table `_aliases` storages are now
  resolved, the `?scope=` reserved key is documented as effective
  rather than dead-code, and the remaining wire-side scope-outs
  (aggregator / multi-source / pattern source) are listed as P3b carry.
- `docs/runbook-verify.md` line 3 rewritten for consumer readability — describes the file as the procedure SoT, contributor invocation pattern, and explicitly scopes execution log location to contributor's own choice. Replaces an earlier internal-doc reference that exposed the gitignored workspace layer.

### Fixed

- **`crates/persona-wire-mcp` publish path** — the `ONBOARDING_GUIDE` constant resolved `include_str!("../../../docs/onboarding.md")` against the workspace root, but `cargo publish` only packages files inside the crate's own tree, so `cargo publish --dry-run` failed at packaging-verify time. Fixed by bundling a synced copy at `crates/persona-wire-mcp/onboarding.md` and switching the path to `include_str!("../onboarding.md")` (in-crate). The synced copy is ship-only metadata; the canonical workspace `docs/onboarding.md` remains the human-navigable source.
- **Two-layer safety net for the bundled onboarding sync** — (1) `include_str!` resolution failure makes `cargo build` / `cargo publish` error out when the bundled copy is missing; (2) the new `crates/persona-wire-mcp/build.rs` byte-compares the workspace canonical copy against the bundled copy on every dev build and panics with a one-line `cp` fix command on drift. Published-tarball builds skip the byte compare (the workspace doc is absent there; only the bundled copy ships).

### Security

- Removed an internal-doc reference from `docs/runbook-verify.md` line 3 (public artifact exposed the gitignored workspace layer's existence; replaced with a consumer-readable description of the file's role).

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

[Unreleased]: https://github.com/ynishi/persona-wire/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/ynishi/persona-wire/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ynishi/persona-wire/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ynishi/persona-wire/compare/441a727...v0.1.0
