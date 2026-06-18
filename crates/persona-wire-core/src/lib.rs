//! # persona-wire-core
//!
//! Transport-agnostic core for the `persona-wire` graph engine. The crate's
//! value proposition is **ProjectionAsPrompt**: turn an arbitrary
//! [`Specification`](domain::specification::Specification) over a small
//! property graph into a rendered string (Prompt / Markdown / JSON / ASCII)
//! by binding it to a registered template, then concatenate one or more such
//! renderings into a wake-time prompt context.
//!
//! No MCP or CLI dependencies — `persona-wire-mcp` and the unified
//! `persona-wire` binary both depend on this crate and adapt their own
//! transport surfaces on top of the use cases exported here.
//!
//! ## Layer split (DDD + Hexagonal)
//!
//! - [`domain`] — Entities, Value Objects, and business rules. Pure code with
//!   no I/O.
//!   - [`domain::graph`] — `Node` / `Edge` / `Severity`. The persistent
//!     graph entities. `Node.metadata` is a free-form `serde_json::Value`,
//!     which is what every higher layer queries against.
//!   - [`domain::specification`] — composable predicate
//!     (`TypeIs` / `MetadataEq` / `Reachable` / `And` / `Or` / `Not`). The
//!     canonical [Specification pattern][spec-bp] applied to the graph: each
//!     variant is a tiny domain object; combinators (`and` / `or` / `not`,
//!     plus `std::ops::Not`) build composite predicates at runtime.
//!     [`Specification::is_satisfied_by`](domain::specification::Specification::is_satisfied_by)
//!     evaluates the predicate against one [`Node`](domain::graph::Node).
//!   - [`domain::error`] — `WireError` / `WireResult` shared across the crate.
//!   - [`domain::autoversion`] — versioning of registered entities.
//!   - [`domain::repository`] — the repository trait surface that the
//!     infrastructure layer implements.
//!
//! - [`application`] — Use cases and registries. Coordinates the domain and
//!   infrastructure layers; this is the API surface that transport adapters
//!   target.
//!   - [`application::spec_registry::SpecRegistry`] — persistent **registry**
//!     of named [`Specification`](domain::specification::Specification) values.
//!     `register` / `get` / `list`, JSON-serialised in the `specifications`
//!     table.
//!   - [`application::projection_registry::NamedProjection`] /
//!     [`application::projection_registry::ProjectionRegistry`] — **CQRS Read
//!     Model**: a `NamedProjection` is a `(name, spec_ref, template,
//!     target_form)` tuple. `spec_ref` points at an entry in `SpecRegistry`;
//!     `template` is a handlebars body; `target_form` is one of
//!     [`Prompt` / `Markdown` / `Json` / `Ascii`](application::projection_registry::TargetForm).
//!     The registry persists projections in the `projections` table — there
//!     is **no hard-coded projection list anywhere in the crate**, every
//!     projection is data.
//!   - [`application::merger::MergeStrategy`] — combine an overlay template
//!     into a base template (`Replace` / `Append` / `Prepend` /
//!     `Section(name)`). `Section` substitutes `{{!-- <name> --}}` markers
//!     and falls back to `Append` when the marker is absent.
//!   - [`application::persona_pack_resolver`] — read template overlays from
//!     `~/persona-pack/<id>/prompt.toml` (or `$PERSONA_PACK_ROOT`) under
//!     `[extra.persona_wire.projections.<axis>]`. The resolver returns only
//!     overlays (template / target_form / strategy); the source-of-truth for
//!     wiring entries stays in the graph.
//!   - [`application::use_cases`] — the high-level operations
//!     (`wire_init` / `wire_close` / `wire_doctor` / `wire_query` /
//!     `wire_render` / `wire_prompt_context` / batch creators / deleters).
//!
//! - [`infrastructure`] — Adapters bound to a concrete backend.
//!   - [`infrastructure::storage::SqliteStorage`] — SQLite implementation of
//!     the repository surface (`nodes` / `edges` / `specifications` /
//!     `projections` tables and a `type_registry` for the open vocabulary).
//!   - [`infrastructure::rendering`] — handlebars template engine over the
//!     query-result context. Behaves like a Mustache superset
//!     (`{{var}}`, `{{#each list}}…{{/each}}`, `{{#if cond}}…{{/if}}`,
//!     dotted paths) and emits a visible `{{render-error: …}}` prefix on
//!     parse failure instead of panicking or silently fallback-ing.
//!   - [`infrastructure::adapter`] — Layer 6 **SoT Adapter**. Each axis
//!     wiring entry carries a `metadata.source_uri`; the
//!     [`PluginRegistry`](application::plugin_registry::PluginRegistry)
//!     dispatches by scheme prefix to an `Arc<dyn Adapter>`:
//!     - `file://<path>` / `file:<path>` → `FileAdapter` (`std::fs` with
//!       `~/` expansion; for a directory it picks the newest mtime child).
//!     - `mini-app://<table>` → `MiniAppAdapter` (external crate
//!       `persona-wire-adapter-mini-app`; consumer wires it on top of
//!       [`PluginRegistry::default_builder_for_wire`](application::plugin_registry::PluginRegistry::default_builder_for_wire)).
//!
//! ## Two query axes
//!
//! Wire exposes two complementary axes; both are first-class:
//!
//! - **Dynamic axis** — caller supplies an inline
//!   [`Specification`](domain::specification::Specification) and gets the
//!   matching nodes back via
//!   [`wire_query`](application::use_cases::wire_query). Good for ad-hoc
//!   inspection, scripts, and one-off filters.
//! - **Fixed axis** — caller registers a `(spec, template, target_form)` as a
//!   [`NamedProjection`](application::projection_registry::NamedProjection)
//!   and refers to it by `spec_ref` / `projection_ref`. Good for stable
//!   surfaces such as wake-time injection.
//!
//! ## Render flow (`wire_render`)
//!
//! ```text
//!  ProjectionRegistry.get(name)
//!    → NamedProjection { spec_ref, template, target_form }
//!         │
//!         │ spec_ref
//!         ▼
//!  SpecRegistry.get(spec_ref)
//!    → Specification (TypeIs / MetadataEq / And / Or / Not / Reachable)
//!         │
//!         │ Specification::is_satisfied_by
//!         ▼
//!  collect_matching_nodes(storage, spec) → Vec<Node>
//!         │
//!         │ context build: { count, nodes, entries, … }
//!         ▼
//!  rendering::render(target_form, template, context)
//!    → String (Prompt / Markdown / JSON / ASCII)
//! ```
//!
//! ## PromptContext flow (`wire_prompt_context`)
//!
//! Persona-scoped one-shot entry intended for wake-time auto-load:
//!
//! 1. Read the optional `[extra.persona_wire.projections.<axis>]` overlays
//!    for the persona (best-effort; missing persona-pack is silently
//!    tolerated).
//! 2. Discover the persona's axes by querying the graph with a
//!    `Specification` (`TypeIs("outline_node")` AND
//!    `MetadataEq("persona", <persona_id>)`). The axis list is therefore
//!    **data, not code** — adding an axis is a graph insert.
//! 3. For each axis, look up the base
//!    [`NamedProjection`](application::projection_registry::NamedProjection)
//!    by the conventional name `<persona_id>.section.<axis>`. If an overlay
//!    is present, run `MergeStrategy::merge(base, overlay)`. Fetch the axis
//!    payload through the Layer 6 Adapter via the wiring entry's
//!    `source_uri`, then render the block.
//! 4. Concatenate the rendered blocks into a single `prompt_context` string.
//!    `projection_names: Some([...])` restricts the walk to an explicit
//!    subset; `None` walks every registered axis for the persona.
//!
//! No template content is hard-coded inside this crate. The set of axes, the
//! base templates, and the optional overlays are all data managed through
//! the regular registry / persona-pack surfaces.
//!
//! ## Persistence schema (SQLite, set up by
//! [`SqliteStorage::migrate`](infrastructure::storage::SqliteStorage::migrate))
//!
//! - `type_registry(name TEXT PK, kind TEXT, schema_json TEXT, severity_allowed TEXT)`
//! - `nodes(id TEXT PK, type TEXT FK→type_registry.name, sot_ref TEXT?, confidence REAL?, …, metadata TEXT)`
//! - `edges(id TEXT PK, src_node TEXT FK→nodes.id, tgt_node TEXT FK→nodes.id, kind TEXT FK→type_registry.name, severity TEXT?, metadata TEXT, …)`
//! - `specifications(name TEXT PK, expr_json TEXT, created_at INTEGER)`
//! - `projections(name TEXT PK, spec_ref TEXT, template TEXT, target_form TEXT, created_at INTEGER)`
//! - `versions(…)` — autoversion ledger.
//! - `workflow_runs(…)` — reserved for the workflow engine layer.
//!
//! The graph vocabulary is **open** but type-checked: any `Node` or `Edge`
//! must reference a row in `type_registry`. The default seed is loaded by
//! [`SqliteStorage::seed_default_types`](infrastructure::storage::SqliteStorage::seed_default_types).
//!
//! [spec-bp]: https://en.wikipedia.org/wiki/Specification_pattern

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use domain::error::WireError;
pub use domain::error::WireResult;
