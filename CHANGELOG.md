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

## [0.10.0] - 2026-07-04

### Added

- **Read tools for `wire_spec_*` and `wire_projection_*` MCP surface**
  (sibling of the `wire_wiring_create` / onboarding-guide drift work).
  `wire_spec_get` / `wire_spec_list` / `wire_projection_get` /
  `wire_projection_list` mirror the existing `wire_bundle_get` /
  `wire_bundle_list` pattern so that Specifications and NamedProjections
  gain symmetric read surfaces alongside their existing register / delete
  operations. Previously, inspecting registry contents (for example
  during onboarding troubleshoot or persona wire audit) required SQLite
  direct reads because the MCP surface only exposed register + delete.
  - `wire_spec_get(id_or_name)` / `wire_projection_get(id_or_name)` use
    the same name-or-ULID resolution as `wire_bundle_get`, returning
    full row shape (id / name / body / created_at / updated_at).
  - `wire_spec_list` / `wire_projection_list` share a new
    `WireListPageParams { limit, offset }` with default limit 100 / max
    1000 in `created_at`-descending order.
  - `SpecRegistry` / `ProjectionRegistry` gain `get_by_ref` /
    `get_by_name` / `get_by_id` / `list_all` methods returning full
    `SpecRow` / `ProjectionRow` shape backed by the storage layer.

### Changed

- **`specifications` and `named_projections` tables gain an
  `updated_at` column** via additive `add_column_if_missing` migration
  (idempotent, mirrors the existing `bundles` table shape).
  `upsert_specification` / `upsert_projection` now take `now_secs: i64`
  to persist `created_at` / `updated_at` alongside existing rows.
  Legacy rows registered before this migration keep
  `created_at = updated_at = 0` until the next re-register.

## [0.9.1] - 2026-06-29

### Fixed

- **Onboarding resource missed `projection_exclude_names` documentation**
  (commit `f78d9ae`). The v0.9.0 release added
  `projection_exclude_names` but `docs/onboarding.md` and its
  `include_str!`-bundled copy at
  `crates/persona-wire-mcp/onboarding.md` (served as MCP resource
  `wire-guide://onboarding`) still described only the include-side
  `projection_names`. First-time integrators discovering the wire
  surface through the resource had no documented path to the new
  filter.
  - §0 mental model now notes that `projection_names` and
    `projection_exclude_names` compose as AND NOT (`include \ exclude`),
    with exclude winning on intersection and unknown names ignored.
  - §5 smoke-test gains two JSONC samples (exclude-only and
    include ∧ ¬exclude) alongside the existing include-only and
    render-everything examples.
  - §6 Skill body gains the exclude-only signature snippet for the
    "render everything except a few noisy slots" use case.
  - §6c Migration extends the `projection_names: ["axis"]` subset note
    with the symmetric exclude usage (e.g. `["tick_log", "friend_map"]`
    at work-mode wake).

## [0.9.0] - 2026-06-29

### Added

- **`wire_prompt_context` accepts `projection_exclude_names` for AND NOT
  slot filtering** (issue `a3b4ef24` Phase 1). The new argument composes
  with the existing `projection_names` as `include \ exclude`, so callers
  that want "render everything except a few slots" no longer need to
  enumerate the full remainder on the include side.
  - `WirePromptContextInput` gains
    `projection_exclude_names: Option<Vec<String>>` with rustdoc
    documenting the 4-case semantics table (both-None / include-only /
    exclude-only / both = AND NOT).
  - `enumerate_slot_names` extends its signature with an `exclude`
    argument. Exclude wins on intersection, unknown names are silently
    ignored for forward compatibility, and an empty `Vec` is a noop
    equivalent to `None`.
  - MCP `WirePromptContextParams` passes the argument through to
    `wire_prompt_context`; the tool description documents the AND NOT
    semantics so clients see the option at the dispatch site.
  - 8 spec cases cover both-None / include-only / exclude-only / both
    paths plus intersection-exclude-wins / unknown-name-ignored /
    empty-result / empty-exclude-noop edges. Existing callers
    (`projection_names`-only or both `None`) keep their prior behavior;
    the new argument defaults to `None`.

## [0.8.1] - 2026-06-27

### Fixed

- **Bundle `[[wirings]]` section dropped `maintenance_exempt = true`**
  (issue `e8b444a6`) — `WiringEntry` did not declare the field, so
  serde silently discarded the top-level value at TOML deserialize
  time and the installed wiring node persisted as
  `metadata.maintenance_exempt = false` (the `wiring_mapper` read-side
  default). The `wire_doctor` graph axis then surfaced these nodes as
  orphans even though the bundle author had asked them to be treated
  as self-attached. Surface caught by the 2026-06-27 mia.anchor_files
  configuration round-trip (post-v0.8.0 self-detect).
  - `WiringEntry` gains a first-class `maintenance_exempt:
    Option<bool>` field. When the caller opts in, the dispatch loop
    inserts `metadata.maintenance_exempt = true`; absent / `Some(false)
    `+ omitted leave the metadata key untouched, preserving behaviour
    for existing graph callers that gate on `Value::Bool` presence.
  - Free-form `metadata = { … }` table input still wins on key
    collision through the existing `for (k, v) in extra { meta.insert
    (...) }` loop, so the field-level opt-in is additive on top.
  - 2 regression unit tests (`install_wiring_persists_maintenance_exempt
    _flag` for the round-trip + `install_wiring_omits_maintenance_exempt
    _when_absent` for the default branch). Bundle suite 24 → 26 tests.

## [0.8.0] - 2026-06-27

### Added

- **Bundle scaffolding installer** — a Bundle layer that packages
  Spec / Projection / Wiring / Workflow (+ optional Node / Edge) into
  a single TOML manifest and installs them through the existing
  registries in one shot. Same PoEAA Registry stance as
  Spec / Projection: domain entity (`Bundle` aggregate with
  `BundleName` / `BundleVersion` Value Objects, `BundleId` ULID alias,
  `ConflictMode` enum, `BundleRef` parse), `BundleRegistry` CRUD,
  `bundle_install` use case (TOML parse → name resolution → section
  dispatch → install report → install log append).
- **`bundles` + `bundle_installs` SQLite tables** — `bundles` carries
  the verbatim TOML body keyed by ULID id + UNIQUE name + version;
  `bundle_installs` provisions per-install audit rows for the future
  History / Force / Undo carry. Both materialized by
  `SqliteStorage::migrate()` for new stores.
- **6 install section dispatchers** (`bundle_install`): `[[specs]]` →
  `SpecRegistry`, `[[projections]]` → `ProjectionRegistry`,
  `[[nodes]]` → `SqliteStorage::insert_node`, `[[edges]]` →
  `SqliteStorage::insert_edge` with name-based src/tgt resolution,
  `[[wirings]]` → `outline_node` Node keyed by
  `format!("{persona}.{slot}")` + canonical metadata
  (`persona` / `axis` / `source_uri` / optional `projection_ref`),
  `[[workflows]]` → existing `wire_workflow_register` use case (trigger
  / action invariants stay owned by the Workflow entity).
- **Conflict resolution modes** — `Increment` (default,
  non-destructive auto-suffix `-1` / `-2` ...; internal references
  inside the same manifest are rewritten through a per-section rename
  map so re-install never produces a half-broken graph),
  `Skip` (idempotent for fixed-name bundles), `Error` (abort whole
  install on first collision). Force / overwrite is intentionally
  deferred to v2 — install history is already populated to support
  it.
- **5 MCP tools** (`persona-wire-mcp`):
  `wire_bundle_register` (TOML body in, header parsed at register time
  so a malformed install-time section does not block registration) /
  `wire_bundle_list` (name-ascending summary) / `wire_bundle_get`
  (ULID or name, returns verbatim body + timestamps) /
  `wire_bundle_install` (`mode` defaults to `increment`) /
  `wire_bundle_delete` (install history preserved).
- **CLI subcommand** `persona-wire bundle <register|list|get|install
  |delete>` — same five operations exposed through `clap`. Sample
  workflow: `persona-wire bundle register --file
  bundles/quickstart.toml` → `persona-wire bundle install --ref
  quickstart`.
- **Sample bundle `bundles/quickstart.toml`** — minimal persona + spec
  + projection scaffold demonstrating the documented TOML shape;
  re-installing under the default `increment` mode is non-destructive.
- **Onboarding §8 "Bundle — scaffolding installer"** (canonical
  `docs/onboarding.md` + bundled MCP resource copy
  `crates/persona-wire-mcp/onboarding.md` served at
  `wire-guide://onboarding`) — TOML manifest shape, CLI ↔ MCP surface
  map, conflict-resolution semantics, install report structure.
- **3 Bundle E2E integration tests** (`crates/persona-wire-core/tests/
  bundle_e2e.rs`) — register / install / verify-via-registries
  round-trip, increment re-install + rename-map rewrite check,
  skip-mode idempotency. A fourth regression test
  (`bundle_delete_after_install_succeeds_and_preserves_install_log`)
  covers the `wire_bundle_delete` post-fix path; see Fixed below.

### Changed

- **Workspace dep `toml = "0.8"` consolidated** into
  `[workspace.dependencies]`. `persona-wire-core` / `persona-wire-mcp`
  / `persona-wire` now declare `toml.workspace = true`, satisfying the
  `rust-architecture-baseline.md §Dependency` rule.
- **`SqliteStorage::conn_for_test`** widened from
  `#[cfg(test)] pub(crate)` to `#[doc(hidden)] pub` so out-of-crate
  integration tests under `tests/` can verify raw SQL state. The
  doc-hidden marker keeps the surface off the published rustdoc;
  production callers still go through the typed registry / repository
  APIs.

### Fixed

- **`wire_bundle_delete` FOREIGN KEY constraint failed** — pre-fix
  schema declared `bundle_installs.bundle_id TEXT NOT NULL REFERENCES
  bundles(id)` (= default RESTRICT on delete), so deleting a bundle
  whose install log had any rows failed with `FOREIGN KEY constraint
  failed` — contradicting the design's "install history is
  intentionally preserved across bundle deletion" stance.
  - Storage SCHEMA: `bundle_installs.bundle_id` is now nullable + `ON
    DELETE SET NULL`. New stores get the corrected shape directly.
  - Migration `m003_bundle_installs_fk_relax` — classic SQLite ALTER
    recipe (CREATE _new with the corrected FK shape → INSERT SELECT
    preserving rows → DROP old → RENAME → re-create the bundle_id
    index). Idempotent on three axes (table-absent, already-relaxed,
    `schema_migrations` re-run); 3 unit tests cover all three branches
    plus delete-then-SET-NULL post-state.
  - Surface caught by real-MCP smoke (`/jikki` Phase 2), not by the
    initial Bundle E2E test set. The new regression test
    (`bundle_delete_after_install_succeeds_and_preserves_install_log`)
    closes that gap end-to-end through the public registry surface.

## [0.7.0] - 2026-06-27

### Added

- `pw-migrate` binary + `persona_wire::migrations` library framework
  (Diesel / sqlx style runner). Subcommands `list` / `status` /
  `up [--target ID]` / `apply <id>` over a `--db <path>` SQLite store.
  `--dry-run` default; `--apply` opts in and auto-writes a
  `<db>.pre-migrate.bak` (override with `--backup`, overwrite with
  `--force`). Each migration runs inside a `BEGIN IMMEDIATE`
  transaction with `PRAGMA foreign_keys = OFF` + `foreign_key_check`
  post-validation; a `schema_migrations(version, description,
  applied_at)` ledger tracks applied ids for idempotent re-runs.
- `crates/persona-wire/src/migrations/m001_node_id_ulid.rs` —
  v0.6.x→v0.7.0 nodes/edges ULID rewrite (was phase A of the legacy
  `migrate_id_to_ulid` bin).
- `crates/persona-wire/src/migrations/m002_registry_id_ulid.rs` —
  specifications/projections rebuild (was phase B).

### Changed (extended)

- `migrate_id_to_ulid` binary is now a deprecated alias that forwards
  to `pw-migrate up --db <path>` (warns on stderr; the
  `--mapping-out` flag still writes a compatibility marker file).
  Removal target: v0.8.0.

- `Node.name` / `Edge.name` fields carry the human-readable label that
  used to live in `Node.id` / `Edge.id`. `name` has no uniqueness
  constraint; duplicates are allowed and surface as `WireError::AmbiguousName`
  on `id_or_name` lookups.
- `SqliteStorage::get_node_by_name`, `lookup_node_id_by_name`,
  `lookup_edge_id_by_name`, `resolve_node_id_or_name`,
  `resolve_edge_id_or_name` helpers for the new identity model.
- `WireQueryNode.name` field — slim-form query results now expose both
  the ULID `id` and the human-readable `name`.
- `migrate_id_to_ulid` binary (`crates/persona-wire/src/bin/`) —
  v0.6.x → v0.7.0 SQLite data migration. Dry-run by default, requires
  `--apply` to mutate; auto-backs up to `<db>.pre-ulid.bak` (override
  with `--backup`), optional `--mapping-out <json>` dumps the old→new
  id map. Idempotent at schema detection. Now runs in two phases:
  phase A (`nodes` / `edges` → ULID + `name` extraction) and phase B
  (`specifications` / `projections` table rebuild with `id` PK + `name`
  UNIQUE). Each phase short-circuits independently when its columns
  are already in the new shape.
- `scripts/migrate_id_to_ulid.sql` — pointer + validation-query stub
  for the migration binary.
- `SpecificationId` / `ProjectionId` type aliases over `ulid::Ulid`
  (`domain/entity/projection`). Registry rows now carry both the ULID
  `id` (server-minted, sortable, immutable, what Persona-Share /
  Templating workflows should pin to) and the human-readable `name`
  (UNIQUE within the registry, the existing CLI / MCP surface key).
- `SqliteStorage::lookup_specification_id_by_name`,
  `lookup_projection_id_by_name`, `resolve_specification_id_or_name`,
  `resolve_projection_id_or_name`, `get_specification_name_by_id`,
  `get_projection_name_by_id` — name ↔ id ↔ row resolver helpers
  mirroring the node/edge surface.

### Changed (extended)

- **BREAKING**: `SqliteStorage::delete_specification` /
  `delete_projection` now take `&SpecificationId` / `&ProjectionId`
  (were `&str` name). Use-case-layer `wire_spec_delete` /
  `wire_projection_delete` accept `id_or_name` and resolve internally.
- **BREAKING**: `SpecRegistry::register` / `ProjectionRegistry::register`
  now return the row's `Id` (were `()`).
- `wire_spec_register` / `wire_projection_register` MCP responses now
  return `{"id": "<ULID>", "name": "..."}` (were a plain status string).
- `wire_render.projection_ref` / `wire_query.spec_ref` accept either a
  ULID or the human-readable name (resolved via the new
  `resolve_*_id_or_name` helpers).
- `specifications` / `projections` SQLite schema gains `id TEXT PRIMARY KEY`
  and `name TEXT NOT NULL UNIQUE` + matching `idx_*_name` index.
  Existing rows are migrated by the `migrate_id_to_ulid` binary
  (phase B — table rebuild via canonical SQLite ALTER recipe).

### Changed

- **BREAKING**: `NodeId` and `EdgeId` are now `ulid::Ulid` aliases (were
  `String`). The server mints opaque ULIDs on row creation; the
  human-readable label moves to the new `name` field.
- **BREAKING**: `wire_node_create` / `wire_edge_create` / batch variants
  accept `name` instead of `id`. The response now returns
  `{"id": "<26-char ULID>", "name": "..."}`.
- **BREAKING**: `wire_node_update` / `wire_node_delete` / `wire_edge_delete`
  / `wire_edge_create.src` / `.tgt` / CLI `--id` flags accept either a
  ULID or a `name` (resolved internally via `resolve_*_id_or_name`).
  `AmbiguousName` is returned when a `name` resolves to multiple rows.
- **BREAKING**: `SqliteStorage::delete_node`, `delete_edge`,
  `update_node_metadata` now take `&NodeId` / `&EdgeId` instead of `&str`.
- `list_nodes_by_type` / `list_edges_*` now `ORDER BY name, id` instead
  of `ORDER BY id` so callers see a deterministic human-readable order.
- SQLite schema gains `nodes.name` / `edges.name` columns and matching
  `idx_nodes_name` / `idx_edges_name` indexes (compatible ALTER via the
  manual migration script).

### Deprecated

### Removed

### Fixed

### Security

## [0.6.0] - 2026-06-22

### Added

- **`mcp://` Source Adapter (`persona-wire-adapter-mcp` crate)** —
  Wire の Layer 6 Adapter として `mcp://<server>/tools/<tool>` /
  `mcp://<server>/resources?uri=<encoded>` の 2 grammar を dispatch、 stdio
  (rmcp `transport-child-process`) と Streamable HTTP の両 transport を
  サポート。 tool args は scalar query (`?k=v`) と JSON 一括 (`?_args=<json>`)
  の 2 系統、 後者が precedence。 connect / call / disconnect は per-fetch
  stateless 設計。 (issue ea99f9e1 Phase A、 commit `53fd351`)
- **`McpEndpointResolver` trait + `SqliteEndpointResolver`** —
  `McpAdapter` が endpoint を解決する経路を trait 抽象化、 production 経路は
  `SqliteEndpointResolver` が graph node (`type="mcp_server"`,
  `metadata.endpoint=<ServerEndpoint JSON>`, `metadata.maintenance_exempt=true`)
  を `SqliteStorage::get_node` で resolve。 type 不一致 / endpoint 欠落 /
  malformed JSON はそれぞれ hint 付き `WireError::Storage` で返す。
  (issue 3ca10673、 commit `0c62f90`)
- **`mcp_server` node type vocabulary** — `seed_default_types` の SEED に
  追加、 `wire init` 時に登録される (10 node + 9 edge types に増加)。
  consumer は `wire_node_create(type="mcp_server", ...)` で endpoint node を
  登録可能。 `is_self_attached_wiring` 経路で `maintenance_exempt=true` flag
  に対応し doctor の `graph.orphan_node` warn を抑制。 (commit `0c62f90`)
- **E2E round-trip test (`crates/persona-wire/tests/e2e_adapter_mcp.rs`)** —
  `persona-wire mcp` 自身を MCP endpoint として dogfood する dual-process
  scenario。 Outer の `wire_prompt_context` が `McpAdapter` を経由して Inner
  `persona-wire mcp` を stdio で spawn、 `wire_doctor` を call、 markdown を
  prompt に embed する round-trip を検証。 (親 issue ea99f9e1 acceptance #3
  closure、 commit `0c62f90`)

### Changed

- **`McpAdapter::new` / `McpAdapter::with_rpc_timeout` signature を変更
  (BREAKING)** — `BTreeMap<String, ServerEndpoint>` 受け取りを
  `Arc<dyn McpEndpointResolver>` に置換。 `WireServer::new` は
  `SqliteEndpointResolver(storage_arc)` を渡す経路に更新済 (consumer 側
  対応)。 (commit `0c62f90`)
- **`SqliteStorage::seed_default_types` SEED 件数 9 → 10** — `mcp_server`
  追加に伴い `seed_inserts_*_node_and_*_edge_types` / `seed_is_idempotent` /
  `full_pipeline_init_seed_register_render_close` の count assertion を 10/19
  に更新。 (commit `0c62f90`)

## [0.5.2] - 2026-06-22

### Added

- **`file://` Source Adapter — File metadata expose (R4、 issue `7998607b`)** —
  `FileAdapter::fetch` が file body と一緒に `metadata` JSON object を返す。
  Fields: `filename` (basename) / `full_path` (absolute) / `last_modified`
  (UNIX epoch u64、 `std::time::SystemTime` 経由、 chrono 非依存) / `size_bytes` /
  `age_days`。 単一 file path / newest-in-dir 両 path で metadata 同梱。
  Non-existent path は `WireError::Storage` を返さず `{ body: null, metadata: null }`
  で graceful fail。 handlebars template から `{{metadata.last_modified}}` /
  `{{metadata.age_days}}` 等で参照可。 backward-compat: 未参照時の emit ゼロ。
- **`file://` Source Adapter — tail / tail_n query param 拡張 (R5、 issue `eb62ebdb`)** —
  `?tail=last_section` で markdown `## ` h2 boundary の最後の section 以降を
  部分 fetch。 `?tail_n=<N>` で末尾 N 行を部分 fetch (`TAIL_N_MAX = 1000` 行で
  clamp、 context size guard)。 unknown / unparseable query param は full fetch
  に graceful fail。 R4 metadata は両 tail mode で preserve (= 同 source_uri で
  `?tail=last_section` + metadata 参照を併用可)。 `resolve_file_path` は `?query`
  suffix も strip (`#fragment` と並列)。 25 file-adapter tests + 287 core tests
  全 PASS。

### Changed

- **`crates/persona-wire-mcp/onboarding.md`** を `docs/onboarding.md` と再 sync
  (build.rs gate baseline 整合)。

## [0.5.1] - 2026-06-21

### Fixed

- **`wire_doctor` graph axis false-positive 2 件除去** (issues `9f70b493` / `f3bb100e`) — persona-scoped mode (`wire_doctor(persona_id=Some(_))`) で `graph.edges_zero` probe を skip するよう変更 (Phase A 時点で個別 persona は graph axis の edge-based 接続対象でない設計判断、 mia 等が `BROKEN` verdict を取る false-positive を構造除去)。 加えて `workflow_def` Node (= Workflow Entity は trigger/action で動作完結し edge を持たないのが正常) を graph axis の検知対象集合から phase-invariant に除外 — `graph.orphan_node` / `graph.edges_zero` / `graph_scan_summary` (= `wire_close` 経路) すべてで sweep。 `mia.workflow.session_close` 等が orphan 警告で報告される現象を解消。
- **`wire_doctor(persona="mia")` が `HEALTHY` 着地** — 上記 2 件の sibling fix により、 wiring を意図的に持たない Phase A persona の doctor は `error=0 warn=0 info=0` を返す。 旧挙動 (`BROKEN` verdict + `graph.edges_zero` error + `mia.workflow.session_close` orphan warn) は仕様判断の outdated に起因した false-positive。

### Changed

- **`graph_edges_zero` probe module rustdoc 整備** — Phase A applicability scope / Phase β 切替前提 3 条件 / `workflow_def` phase-invariant exclusion を module-level doc に集約 (rustdoc-as-SoT、 intra-doc link で `crate::application::workflow_mapper::WORKFLOW_TYPE` 等を参照)。

## [0.5.0] - 2026-06-21

### Added

- **Domain Entity layer 新設** (`domain/entity/`) — `Projection` / `Wiring` / `Workflow` / `ContextWiring` の 4 Aggregate Root + `Slot` / `PersonaId` / `Source` を Domain Entity として land (commits `a5d6fcb` / `faa9054` / `f7aacfb` / `ade40c7`)。 各 Entity は immutable、 mutator を持たず、 構築時に VO invariant を強制 (Vernon IDDD Rule 2 Small Aggregates / Make Illegal States Unrepresentable)。
- **Value Object 群** — `PersonaId` / `Source` / `ProjectionName` / `SpecName` / `SpecRef` / `ProjectionTemplate` / `TargetForm` enum / `PluginDispatch` enum を新設。 すべて `Display` / `AsRef<str>` / `Serialize` / `Deserialize` / `TryFrom<&str>` / `TryFrom<String>` を備える対称 surface。 `PluginDispatch` は 3 Optional field の組合せ 8 状態を `Default | Custom { engine, kind, config }` の 2 状態に discriminant 化、 illegal combination 6 種を型から排除 (`projection.rs`)。
- **Hexagonal Driven Port** — `ProjectionRenderer` trait を `domain/port/` 配下に新設 (commit `976cfe5`)。 application 層からの依存方向を逆転、 `StaticProjection` adapter は `infrastructure/projection/` 配下に配置して trait を impl。 `ProjectionInput<'a>` は技術依存ゼロの borrowed view (`TemplateEngine` 参照を除去、 Hole-1 解消)。
- **Data Mapper Pattern** (`application/{projection,wiring,workflow}_mapper.rs`) — Fowler PoEAA Ch.10 Data Mapper を 3 mapper で land (commits `f7aacfb` / `735f600` / `28112b2` / `25bf20b`)。 Persistence DTO (`NamedProjection` 等) と Domain Entity 間の `Entity ⇄ DTO` round-trip を保証、 不正 DTO は mapper boundary で `DomainError::InvalidProjection` 等で reject。 Registry が Mapper 役を兼任する narrow 解釈 (`docs/design/projection-data-mapper.md`)。
- **DomainError** を `WireError` から分離 (commit `1d9b977`) — `InvalidPersonaId` / `InvalidSource` / `InvalidMetadata` / `InvalidProjection` / `InvalidTargetForm` variant で Domain invariant 違反を型で表現。 `WireError::Domain(DomainError)` で wrap。
- **Test coverage +24** — `domain::entity::` (65 → 79 lib test) / `application::` (96 → 102 lib test) / `infrastructure::projection::` (2 → 6 lib test)。 workspace 全体 310 → 334 pass / 0 failed (refactor-verify Step 2-5 で land)。

### Changed

- **`domain/*` を `domain/graph/` subdir に整理** (commit `334a0a`) — graph 系 (autoversion / compute / constraint / crud / node / repository / specification) を namespace 化。 Domain Entity 層と分離して module 構造を整える Step A。
- **application Projection trait を `ProjectionRenderer` に rename + Domain Port 化** (commits `afdf0d2` / `976cfe5`) — Data Mapper land 2/3 で trait rename、 続いて Hexagonal Port として Domain layer に移行。
- **application callsite を Projection Domain Entity 経由に migrate** (commit `b855d8a`) — Data Mapper land 3/3。 raw `NamedProjection` DTO を直接扱う callsite を Mapper 経路に統一。
- **axis → slot vocabulary 統一** (commit `b467bcf`) — storage column / API field / Wiring Entity の語彙を `slot` に sweep。 Wiring Slot を first-class concept として表現。
- **`wire_init` / `wire_render` 薄化** (commit `72166e2`) — Step C-6 Phase 2 で 2 helper に分解、 use_case 本体を読みやすくする。
- **`wire_prompt_context` per-slot 薄化** (commit `28112b2`) — Workflow mapper 経由で per-slot 単位に分解。 `SpecName` VO も同時 land。
- **`WiringMetadata` mapper land + `Source::scheme` panic-free 化** (commit `735f600`) — Source の `scheme_len` を構築時 cache、 runtime panic 不能な型保証に再構築。
- **`projection_mapper` を wiring/workflow mapper の sibling として抽出** (commit `25bf20b`) — application/ 配下の 3 mapper 配置統一。
- **rustdoc-as-SoT for projection persistence pattern selection** (commit `368f458`) — design 経緯を rustdoc 化、 `docs/design/projection-data-mapper.md` を impermanent な setup doc として残置。

### Removed

- 旧 monolithic `domain/graph.rs` — subdir 化 (`domain/graph/`) で物理分割。
- 旧 application 層 `Projection` trait — `ProjectionRenderer` rename + Domain Port 化で削除。

## [0.4.0] - 2026-06-20

### Added

- **`wire_graph_check` — new standalone use_case + MCP tool (axis 1: graph connectivity)** — `pub fn wire_graph_check(input: WireGraphCheckInput, storage: &SqliteStorage)` exposed in `persona-wire-core` and as MCP tool `wire_graph_check`. Returns `WireGraphCheckOutput { orphan_count, total_nodes, total_edges, report_markdown }`. Callable independently or as a sub-step of `wire_doctor` (Crux #1 / #2 — genuine separate fn, not a rename of `graph_scan_summary`).

### Changed

- **`wire_doctor` refactored to 2-axis integrated health report** — now delegates axis 1 (graph connectivity) to `wire_graph_check` and axis 2 (workflow coverage) to `wire_workflow_check`. `WireDoctorOutput` gains nested `graph_check: WireGraphCheckOutput` and `workflow_check: WireWorkflowCheckOutput` fields. Backward-compat flat fields `orphan_node_count` / `total_node_count` / `total_edge_count` are retained as top-level mirrors of `graph_check.*` (Crux #3). The `wire_doctor` MCP tool now returns structured JSON (graph_check + workflow_check sub-objects) instead of plain Markdown. The `report_markdown` field in the response still contains a human-readable 2-axis summary.

- **`docs/onboarding.md` §2 + §5 補強** (issue `15a46ce6` follow-up doc) — §2 末尾に「wiring entries that carry `metadata.source_uri` or `metadata.maintenance_exempt: true` are recognised as self-attached and are excluded from the `wire_doctor` / `wire_close` orphan count」 1 文追加、 §5 smoke 節に healthy graph の report literal (`orphan nodes (no edges, not self-attached): 0`) と non-zero count 時の typical cause 説明追加。 dogfood 使用者 (mia 自走 smoke 等) が diagnostic シグナルを誤読する 2 次事故源 (= 「全件 orphan flag = misconfigured」 と判定する drift) を doc 側で予防。 MCP resource `wire-guide://onboarding` は次回 `cargo install` 経由 binary embed 反映。
- **`graph_scan_summary` orphan 判定 refine** — `metadata.source_uri` を持つ wiring entry (= Layer 6 Adapter 経由で外部 SoT を fetch する node) と `metadata.maintenance_exempt: true` を持つ node を orphan カウントから除外。 onboarding §2 「Add an edge ... optional but recommended」 規約と整合 (edges は traceability 目的の optional な装飾、 wiring entry は単体で `source_uri` 経由 fetch 動作する)。 report literal も「`orphan nodes (no edges, not self-attached): N`」 に refine、 意味を明示。 影響範囲: `wire_doctor` + `wire_close` 両方の `orphan_node_count` 数値が refine (= edges optional 規約下で全件 orphan 報告される false-positive 除去)。

### Deprecated

### Removed

- **`wire_workflow_check` MCP tool 撤去** (issue `19d888ee`) — `wire_doctor` 1 本に diagnose 入口を統一する design.md §1 思想に整合。 core 側関数は internal sensor として保持 (commit `b3d3536`)。
- **`wire_graph_check` MCP tool 撤去** — `wire_workflow_check` と同型 pattern で MCP 表面から削除、 sensor 化 (Step 1、 commit `59363cb`)。
- **`wire_graph_check` / `wire_workflow_check` 関数 + 関連 struct を core から削除** (issue `7069dede` + `16962ec8` + `291b219d` 統合 close、 Step 2)。 同等の 2 軸 coverage audit は Probe registry (`graph.*` / `workflow.*` Finding) 経由で `wire_doctor` から行う。 削除した型: `WireGraphCheckInput` / `WireGraphCheckOutput` / `WireWorkflowCheckInput` / `WireWorkflowCheckOutput` / `CoveredNode` / `UncoveredNode` / `UndeclaredNode` / `ExemptNode`。
- **`WireDoctorOutput` を `{ report_markdown: String }` のみに縮約**。 backward-compat flat mirror fields (`orphan_node_count` / `total_node_count` / `total_edge_count`) + nested sub-objects (`graph_check` / `workflow_check`) を物理削除 (Crux #3 carry 消化)。 数値カウントが必要な consumer は `graph_scan_summary(&storage)` を直接呼ぶ (shared primitive、 `wire_close` も同じ経路)。 MCP `wire_doctor` tool は Markdown 文字列を直接返す形に simplification。 e2e tests も `graph_scan_summary` 経由に migrate 済。

### Fixed

- **`wire_node_delete` docstring + MCP description を実装 (storage cascade) に整合** (issue `bdb786f4`、 commit `a2d2b6d`) — storage 側は edges table FK NOT-NULL + 同 Tx cascade-delete で dangling 状態を作らない実装が正、 docstring / MCP description の「edges are NOT cascade-deleted」 宣言が嘘だったのを doc 側で寄せて訂正。 `graph.dangling_edge` Probe は external DB drift / migration corruption / 直 SQL writes 検知用の defensive sensor として保持、 module docstring + test NOTE も同 framing に書き換え。

- **node `metadata` stringified-JSON drift at storage boundary** (issue `22dcf208`) — `SqliteStorage::insert_node` / `update_node_metadata` now route every metadata payload through a `normalize_metadata_storage` helper before writing the row. `Value::Object` passes through unchanged; `Value::String(s)` is re-parsed and the result must itself be a JSON object (otherwise `WireError::InvalidMetadata`); other shapes (`Null` / `Bool` / `Number` / `Array`) are rejected outright. Closes the silent path that let one historical persona node end up stored as a string-encoded JSON literal while the other personas were stored as objects (shape drift discovered while triaging the `ceee21d9` follow-up). The batch entry point `wire_nodes_create_batch` shares the same guard because it iterates `insert_node` row-by-row. New `WireError::InvalidMetadata(String)` variant is added to `domain/error.rs`. Read path (`row_to_node`) remains best-effort to preserve legacy compatibility for rows written before this guard; surviving stringified rows are healed by case-by-case data fixes (e.g. the shi persona node).
- **`wire_doctor` false-positive orphan flag** (issue `15a46ce6`) — wiring entry (= `metadata.source_uri` を持つ outline_node) が edge 不在で orphan 判定されていた drift を fix。 2026-06-19 shi dogfood session で 41/41 全件 orphan flag が再現、 実体は wire_query / wire_prompt_context で正常 fetch 動作確認済の構造だった。 上記 `graph_scan_summary` refine で構造除去、 dogfood 使用者が diagnostic シグナル誤読する 2 次事故源を解消。 regression test: `graph_scan_excludes_self_attached_wiring_from_orphans`。

### Security

## [0.3.0] - 2026-06-19

### Added

- **P3b — `persona-wire-adapter-mini-app` external crate**: `MiniAppAdapter` + `mini-app://` URI parse + 関連 tests を core (`crates/persona-wire-core`) から外部 crate (`crates/persona-wire-adapter-mini-app`) へ物理 move。 core が `mini-app-core` dep に依存しない状態を達成 = single-binary OSS distribution の前提条件成立。 詳細は `docs/plugin-trait.md` §2.1 / §3 参照。
- **P3b — `persona-wire-adapter-sqlite-x` external crate**: 任意 SQLite file に対する generic SoT adapter (scheme `sqlite://`)。 URI form `sqlite://<path>?query=<SQL>` (primary) / `?table=<name>&limit=<n>` (sugar) で SELECT 結果を JSON rows として返す。 mini-app schema convention に縛られず、 Fly.io self-hosting (P4) や volume mount 経由の single-binary 配布で「mini-app 入れなくていい」 道を確保する鉄板 adapter (issue `2b734072` P3 Plugin 候補)。
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

[Unreleased]: https://github.com/ynishi/persona-wire/compare/v0.8.1...HEAD
[0.8.1]: https://github.com/ynishi/persona-wire/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/ynishi/persona-wire/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/ynishi/persona-wire/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/ynishi/persona-wire/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/ynishi/persona-wire/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/ynishi/persona-wire/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/ynishi/persona-wire/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/ynishi/persona-wire/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ynishi/persona-wire/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/ynishi/persona-wire/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/ynishi/persona-wire/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ynishi/persona-wire/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ynishi/persona-wire/compare/441a727...v0.1.0
