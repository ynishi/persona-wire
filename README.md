# persona-wire

A small graph engine for persona × SoT × workflow context routing. Wire
turns a runtime [`Specification`][spec] over a property graph into a rendered
string (Prompt / Markdown / JSON / ASCII) by binding it to a registered
template — the **ProjectionAsPrompt** pattern — and concatenates one or more
such renderings into a wake-time prompt context.

## Documentation

**The crate's Rustdoc is the source of truth for design and API.**

- **Onboarding (end-to-end how-to)** — [`docs/onboarding.md`][onboarding].
  Walks through install → wiring entries → Specification / NamedProjection
  registration → optional persona-pack overlay → `wire_prompt_context`
  smoke-test → Skill / Prompt wiring. The same document is also bundled
  into the MCP server as the resource `wire-guide://onboarding`, so a
  client can `read_resource` it without leaving the session.
- Crate docs (architecture, layer split, render / prompt-context flow,
  persistence schema): the `//!` block at the top of
  [`persona-wire-core/src/lib.rs`][lib-rs]. Run locally with
  `cargo doc --workspace --open -p persona-wire-core`, or read the
  published docs at <https://docs.rs/persona-wire-core/>.
- Concept seed (archived, early narrative): [`docs/_archive/concept-2026-06-14.md`][concept].
  The current design SoT is the crate Rustdoc — the early concept doc is
  kept as a historical reference for the P0 framing.
- MCP tool surface: published docs for
  [`persona-wire-mcp`](https://docs.rs/persona-wire-mcp/).
- CLI subcommands: `persona-wire --help` (or
  [`persona-wire`](https://docs.rs/persona-wire/) on docs.rs).

## Workspace layout

```
persona-wire/
├── Cargo.toml                          # workspace root
└── crates/
    ├── persona-wire-core/              # Domain + Application + Infrastructure (transport-agnostic)
    ├── persona-wire-adapter-mini-app/  # external Adapter for `mini-app://` SoT scheme
    ├── persona-wire-adapter-sqlite-x/  # external Adapter for raw `sqlite://` SoT (Fly.io / single-binary)
    ├── persona-wire-mcp/               # rmcp server library (exposes serve_stdio())
    └── persona-wire/                   # unified bin (clap CLI + `mcp` subcommand dispatch)
```

## Architecture at a glance (DDD + Hexagonal)

| Layer | Where | Contents |
|---|---|---|
| Surface | `mcp` / `cli` | MCP Tool surface, clap subcommands |
| Application | `core::application` | Use cases; `SpecRegistry` (dynamic) and `ProjectionRegistry` (named) read model; `MergeStrategy`; persona-pack overlay resolver |
| Domain Core | `core::domain` | `Node` / `Edge` / composable [`Specification`][spec] / autoversion / repository trait (in `domain::graph`); first-class persona vocabulary `Wiring` / `Workflow` / `Projection` / `ContextWiring` + Value Objects `PersonaId` / `Slot` / `Source` / `SpecRef` / `TargetForm` (in `domain::entity`); Hexagonal Driven Port `ProjectionRenderer` (in `domain::port`) |
| Infrastructure | `core::infrastructure` | SQLite storage, handlebars rendering, Layer 6 SoT Adapter (`file:` via `std::fs`; `mini-app://` via the external `persona-wire-adapter-mini-app` crate; `sqlite://` via the external `persona-wire-adapter-sqlite-x` crate) |

Two complementary query axes, both first-class:

- **Dynamic axis** — inline `Specification` evaluated on demand
  (`wire_query`). Good for ad-hoc filters.
- **Fixed axis** — `NamedProjection = (spec_ref, template, target_form)`
  registered by name (`wire_render`, `wire_prompt_context`). Good for
  stable surfaces like wake-time injection.

Diagnostic surface:

- **`wire_doctor`** — 2-axis integrated health report: axis 1 graph
  connectivity + axis 2 workflow coverage. Both axes are evaluated
  through the internal `application::doctor::probes` registry
  (`graph_*` / `workflow_*` Probes) and emitted as a structured JSON
  response (`graph_check` / `workflow_check` sub-objects + `findings[]`
  with Severity). Top-level backward-compatible flat fields
  (`orphan_node_count` / `total_node_count` / `total_edge_count`) are
  retained as mirrors of `graph_check.*`. Earlier 0.3.x exposed
  `wire_graph_check` / `wire_workflow_check` as standalone MCP tools;
  both were retired in 0.4.0 and folded into the `wire_doctor` Probe
  registry.

There is **no hard-coded projection list in the crate** — every projection
is data, registered through `ProjectionRegistry`. Optional template
overlays per persona live in persona-pack
(`[extra.persona_wire.projections.<axis>]`) and are folded in via
`MergeStrategy` (`Replace` / `Append` / `Prepend` / `Section(name)`).

For the full layer-by-layer description, the persistence schema, and the
render / prompt-context flow diagrams, see the crate Rustdoc above.

## Build

```sh
cargo check --workspace
cargo build --workspace
cargo test --workspace
cargo doc --workspace --open -p persona-wire-core   # browse the design docs
```

## Run

```sh
# CLI
cargo run -p persona-wire -- init --db /tmp/wire.db

# MCP server (stdio transport) — `mcp` subcommand dispatches into persona-wire-mcp::serve_stdio
cargo run -p persona-wire -- mcp
```

## Migrating v0.6.x → v0.7.0 (ULID identity model)

`v0.7.0` swaps the stringly `id` columns on `nodes` / `edges` for opaque
ULIDs, and moves the human-readable label to the new `name` column.
A dedicated binary handles the chained schema + data rewrite for
existing stores:

```sh
# 1. Dry-run first (default). Inspects the schema, builds the id mapping
#    in memory, and reports what would happen — NO mutation.
cargo run -p persona-wire --bin migrate_id_to_ulid -- --db <path>

# 2. Apply for real. Backup is mandatory; default destination is
#    `<db>.pre-ulid.bak` (sibling of the source). Pass `--backup <path>`
#    to override, `--mapping-out <json>` to persist the old→new id map.
cargo run -p persona-wire --bin migrate_id_to_ulid -- --db <path> --apply \
    --mapping-out /tmp/persona-wire-id-mapping.json
```

The binary opens a single `BEGIN IMMEDIATE` transaction with foreign keys
temporarily disabled, rewrites every FK + version pointer, flips the
PKs, validates via `PRAGMA foreign_key_check`, and only commits on a
clean post-migration sanity pass. Re-running on an already-migrated DB
is a no-op (skipped at schema detection).

## License

Dual: MIT OR Apache-2.0.

[spec]: https://en.wikipedia.org/wiki/Specification_pattern
[lib-rs]: https://github.com/ynishi/persona-wire/blob/main/crates/persona-wire-core/src/lib.rs
[concept]: docs/_archive/concept-2026-06-14.md
