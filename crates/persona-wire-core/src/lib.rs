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
//! ## Design rationale
//!
//! The sections below collect the architecture-level decisions that were
//! previously drafted in `docs/design/*.md` while the layout was being
//! shaped. The design docs are now retired — this is the SoT.
//!
//! ### Three-layer split (Math backend / Domain Entity / thin Application)
//!
//! 1. **Math backend Graph** ([`domain::graph`]) — open-vocabulary primitives
//!    (Node / Edge / Severity / Specification / CRUD / Compute / Constraint /
//!    AutoVersion / Repository). Tenant-agnostic and persona-agnostic; used
//!    as a backend SDK. It does not know about personas, slots, sources, or
//!    projections.
//! 2. **Domain Entity Layer** ([`domain::entity`]) — persona-wire's
//!    first-class vocabulary (`PersonaId` / `Slot` / `Source` / `Wiring` /
//!    `Workflow` / `Projection`). Owns invariants and behavior; uses the
//!    Math backend as a persistence SDK. Aggregate composition is documented
//!    in [`domain::entity`] module docs.
//! 3. **Application Layer** ([`application`]) — thin orchestrator that lifts
//!    Domain Entity operations onto the use-case surface that transport
//!    adapters (MCP / CLI / future RPC) target. Knowledge gravitates to
//!    layer 2; this layer stays slim.
//!
//! ### Slot vocabulary (was `axis`)
//!
//! The persona-context binding identifier is now named `Slot`. Earlier code
//! used the AI-jargon word `axis`, which collided with three legitimately
//! orthogonal uses of "axis" elsewhere in the crate:
//!
//! - Graph compute primitive — Traversal / Execution / Constraint axes
//!   (3 orthogonal operation kinds).
//! - Doctor diagnostic surface — `Axis::{Graph, Workflow}` (2 health axes).
//! - Plugin registry — Adapter / Engine / Projection plugin axes
//!   (3 orthogonal slots).
//!
//! In contrast, persona context values like `mailbox` / `mail` / `news` are
//! not orthogonal axes — they are slot names. Decisive evidence (recorded
//! when the rename was decided) included the storage projection name
//! template `"{persona_id}.section.{axis}"` ("section の中の axis 値"
//! structure = `axis` is a name, not a classifier) and the error message
//! `"must contain at least one axis name"` (the word "name" gives the
//! game away).
//!
//! The rename landed in two cuts:
//!
//! 1. Domain Entity layer carries [`domain::entity::Slot`] as a first-class
//!    Value Object on [`domain::entity::wiring::Wiring`].
//! 2. The storage metadata key is still literally `"axis"` (legacy SQLite
//!    rows). [`application::wiring_mapper`] is the single translation
//!    boundary: `Slot ↔ Node.metadata["axis"]`. New code routes through the
//!    mapper; reading `metadata["axis"]` directly is disallowed.
//!
//! ### PoEAA Registry vs DDD Repository (Projection)
//!
//! Projection persistence goes through
//! [`application::projection_registry::ProjectionRegistry`], a PoEAA
//! Registry (Fowler PoEAA Ch.18) — an application-layer service that
//! provides named access to well-known objects as a structured alternative
//! to global access. A DDD Repository (Evans DDD Ch.6 / Vernon IDDD Ch.12)
//! was considered and **not adopted**: it would move persistence vocabulary
//! into a domain port and collapse the application service into a pass-through.
//!
//! [`application::wiring_mapper`] and [`application::workflow_mapper`] are
//! sibling Data Mappers invoked directly from use cases against the Math
//! backend Node repository — Wiring and Workflow do not get their own
//! Registry because they are persisted as graph nodes, not as a separately
//! named table.
//!
//! ### Data Mapper — narrow reading
//!
//! Fowler PoEAA's Data Mapper (Ch.10) requires *some* mapper that translates
//! between persistence shape and Domain shape. The literal pattern uses an
//! independent Mapper class; persona-wire takes the **narrow** reading and
//! lets the Registry (Projection case) or the use case (Wiring / Workflow
//! cases) own the mapper bridge through the
//! [`application::projection_mapper`] / [`application::wiring_mapper`] /
//! [`application::workflow_mapper`] modules. Promoting these to a literal
//! Fowler Mapper trait is a carry that fires only when a second parallel
//! mapper with the same shape arrives. Until then, the free functions
//! intentionally do not sit behind a trait.
//!
//! ### DDD / BP citation table
//!
//! Best-practice citations carry an honest **literal / narrow / 独自整理**
//! tag so future readers can tell which parts of the BP are quoted verbatim
//! and which are persona-wire's own extension.
//!
//! | BP | Where applied | Tag |
//! |---|---|---|
//! | Fowler PoEAA Ch.10 — Data Mapper | Projection / Wiring / Workflow mappers | **narrow** — persistence-vs-domain separation literal; the independent Mapper class is replaced by Registry / use-case owners |
//! | Fowler PoEAA Ch.18 — Registry | `ProjectionRegistry` named lookup | **literal** |
//! | Evans DDD Ch.9 — Specification | [`domain::graph::specification`] — VO + `is_satisfied_by` + and/or/not algebra | **literal** |
//! | Vernon IDDD Ch.10 Rule 2 — Small Aggregates | Single-transaction consistency boundary, not field count | **narrow** — boundary axis only; field-count framing was explicitly excluded |
//! | Vernon IDDD Ch.10 Rule 3 — Reference by Identity | `Projection` references `Specification` via `SpecName` instead of embedding | **literal** |
//! | Fowler bliki 2003 — Anemic Domain Model anti-pattern | Domain Entity owns invariants; DTOs (`NamedProjection`) are intentionally anemic at the persistence boundary | **narrow** — anti-pattern scope limited to the domain layer; the DTO exclusion is an interpretive extension |
//! | CQRS Read Model | Projection's `(name, source-query, transform, output)` 4-concern decomposition | **独自整理** — the 4-tuple shape itself is persona-wire's framing, not literal in Greg Young's original |
//! | Yaron Minsky — Make Illegal States Unrepresentable (Effective ML Revisited) | `PluginDispatch` enum collapses `(engine, kind, config)` Option triple to a `Default` / `Custom { .. }` discriminated union | **narrow** — the slogan and discriminated-union pattern are literal; the 2^N arithmetic phrasing is persona-wire's gloss |
//!
//! [spec-bp]: https://en.wikipedia.org/wiki/Specification_pattern

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use domain::error::WireError;
pub use domain::error::WireResult;
