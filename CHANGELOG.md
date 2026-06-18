# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **P3b — `persona-wire-adapter-mini-app` external crate**: `MiniAppAdapter` + `mini-app://` URI parse + 関連 tests を core (`crates/persona-wire-core`) から外部 crate (`crates/persona-wire-adapter-mini-app`) へ物理 move。 core が `mini-app-core` dep に依存しない状態を達成 = single-binary OSS distribution の前提条件成立。 詳細は `docs/plugin-trait.md` §2.1 / §3 参照。
- **`PluginRegistry::default_builder_for_wire()`**: core 同梱 plugin (FileAdapter + HandlebarsEngine + StaticProjection) を pre-populate した `PluginRegistryBuilder` を返す convenience helper。 caller は `.with_adapter(MiniAppAdapter).build()` のように外部 adapter を chain する。

### Changed

- **`PluginRegistry::default_for_wire()`** から `MiniAppAdapter` を削除 (core 同梱 3 plugin = FileAdapter + HandlebarsEngine + StaticProjection のみ)。 mini-app scheme を含めたい consumer は `default_builder_for_wire()` + `MiniAppAdapter` chain form に切替。
- Workspace dep `persona-wire-core` / `persona-wire-mcp` の version pin を 0.2.1 → 0.2.2 に揃え (workspace.package.version と整合、 drift fix)。
- `crates/persona-wire-mcp/onboarding.md` を `docs/onboarding.md` と sync (build.rs guard 整合)。

### Deprecated

### Removed

- **`infrastructure::adapter::fetch_via_adapter`** (deprecated since 0.3.0) を削除。 後継は `PluginRegistry::adapter_for_uri(uri).fetch(uri)` (P3a Phase 2 で移行済)。

### Fixed

### Security

## [0.2.2] - 2026-06-18

### Added

- **P3a Plugin trait — Phase 1 (skeleton)**: three pluggable extension surfaces landed as trait + default impl pairs so external crates can extend the engine without forking core.
  - `infrastructure::adapter::Adapter` — gains a `scheme()` method for URI-based dispatch. `FileAdapter` (`file://`) and `MiniAppAdapter` (`mini-app://`) are now registered via this trait; external crates can ship additional schemes (e.g. `pg://`, `vector://`, `http://`).
  - `infrastructure::template::TemplateEngine` — new trait for render engines. `HandlebarsEngine` (`id = "handlebars"`) is the default impl; external engines (`jinja`, `tera`, etc.) can be added by implementing the trait.
  - `application::projection::Projection` + `ProjectionInput<'a>` — new trait for projection kinds. `StaticProjection` (`kind = "static"`) delegates to a `TemplateEngine` and is behaviour-equivalent to the existing `wire_init` / `wire_render` / `wire_prompt_context` path; external kinds (`llm`, `code`, `cache`, …) can be plugged in.
  - `application::plugin_registry::PluginRegistry` — immutable builder-based registry that aggregates all three axes. `build()` is fail-fast on duplicate scheme / engine id / projection kind.
- **Public design doc** `docs/plugin-trait.md` — canonical SoT for the Plugin trait surface (rationale, three-axis surface, registry, NamedProjection schema extension carry, plugin-author walkthrough for `wire-adapter-pg`, stability policy, done-criteria).
- **P3a Plugin trait — Phase 2 (a) — NamedProjection schema extension**: persisted dispatch hints so registered projections can opt into non-default plugins (carry: the use-case layer wiring will land in Phase 2 (b)).
  - `NamedProjection` gains three optional fields: `template_engine: Option<String>`, `projection_kind: Option<String>`, `projection_config: Option<serde_json::Value>`. When all are `None`, behaviour is identical to v0.2.1.
  - `projections` SQLite table gains three nullable `TEXT` columns. An idempotent migration (`PRAGMA table_info` + conditional `ALTER TABLE ADD COLUMN`) upgrades pre-existing stores on first open — no manual step required.
  - Storage round-trip test added covering the three hint fields.
- **P3a Phase 2 (d) — `wire_node_update` surface for in-place metadata patching**: wiring entries (e.g. `<persona>.<axis>` outline_nodes) can now have their `metadata.source_uri` (and other metadata fields) tuned without a delete + re-create cycle. Backs the "append `&limit=10` to mini-app:// source_uri" operational UC observed when `/wake mia` injected 30 mailbox rows past the useful horizon.
  - `wire_node_update(id, metadata_patch, mode)` use_case in `application::use_cases`, with `mode = Merge` (RFC 7396 shallow merge: top-level keys overwrite; `null` deletes the matching key) and `mode = Replace` (full metadata swap). Other node fields (`type` / `sot_ref` / lifecycle) intentionally remain immutable on this path.
  - MCP tool `wire_node_update` exposes the same surface (params: `id`, `metadata_patch`, optional `mode`).
  - CLI `persona-wire node update --id <id> --metadata-patch <json> [--mode merge|replace]`.
  - Storage primitive `SqliteStorage::update_node_metadata(id, &Value)` (executes `UPDATE nodes SET metadata = ?`).
  - 6 new unit tests covering merge / null-delete / replace / NotFound / non-object patch rejection / mode parse.
- **P3a Plugin trait — Phase 2 (c) — `projection_kind` field is now consumed by the async render path**: external Projection plugins (e.g. an LLM-driven summarizing projection) can now actually animate through `wire_prompt_context`.
  - `wire_prompt_context` now dispatches its render through `PluginRegistry`'s `Projection` axis. Each per-axis render call goes through `projection.render(ProjectionInput { … }).await`. `projection_kind = None | Some("static")` keeps behaviour identical to v0.2.x (delegates to the registered `TemplateEngine`). `projection_kind = Some("<other>")` resolves the matching `Projection` impl from the registry.
  - `CollectedAxis` carries the additional `projection_kind` / `projection_config` / `projection_name` hints from the registered `NamedProjection` through to render dispatch.
  - `wire_init` / `wire_render` (both sync) now reject non-`static` `projection_kind` values with a structured `WireError::Other("… non-static kinds require the async path; use wire_prompt_context instead")`. This fails fast instead of silently dropping the kind hint on the sync path.
  - 3 new unit tests in `use_cases.rs::tests`: non-static kind rejection on `wire_init`, non-static kind rejection on `wire_render`, explicit `Some("static")` behaves identically to `None`.
- **P3a Plugin trait — Phase 2 (b) — use-case layer dispatches through PluginRegistry**: the three render use cases now resolve their adapter and template engine through the Plugin Registry instead of calling the legacy free functions directly. External-plugin caller surface unblocked.
  - `PluginRegistry::default_for_wire()` convenience helper builds the standard 4-plugin set (`FileAdapter` + `MiniAppAdapter` + `HandlebarsEngine` + `StaticProjection`). Boot sites (MCP server + CLI bin + integration tests) call this when they have no external plugins to inject.
  - `wire_init`, `wire_render`, `wire_prompt_context` each gain a `registry: &PluginRegistry` argument and dispatch adapter / engine through it. Each consults `NamedProjection.template_engine` (Phase 2 (a) field, falling back to `"handlebars"`) when resolving the engine.
  - `WireServer` caches an `Arc<PluginRegistry>` built once at boot so every MCP tool call shares the same dispatch surface.
  - `wire_prompt_context` now surfaces a warning when no adapter is registered for a wiring entry's `source_uri` scheme (previously surfaced via free-fn fall-through).

### Deprecated

- `crate::infrastructure::adapter::fetch_via_adapter` and `crate::infrastructure::rendering::render` are marked `#[deprecated(since = "0.3.0")]`. They remain functional and behaviour-equivalent (the use-case layer no longer touches them); new callers should resolve plugins through `PluginRegistry` instead. Removal is scheduled for the end of P3a, after the external `wire-adapter-pg` proof-of-concept lands in Phase 3.

### Changed

- Workspace dependency `async-trait = "0.1"` added (used to make `Adapter` and `Projection` dyn-compatible so a single `PluginRegistry` can hold heterogeneous trait objects).

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
