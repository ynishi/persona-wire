# persona-wire

A SingleMCPApp graph engine for persona × SoT × workflow context routing.

Concept seed: Edge/Node Graph + Lifecycle + Knowledge SoT navigator (see `docs/concept-2026-06-14.md`).

## Workspace layout

```
persona-wire/
├── Cargo.toml                  # workspace root
└── crates/
    ├── persona-wire-core/      # Domain + Application + Infrastructure (transport-agnostic)
    ├── persona-wire-mcp/       # rmcp server library (exposes serve_stdio())
    └── persona-wire/           # unified bin (clap CLI + `mcp` subcommand dispatch)
```

## Architecture (v4 / BP-aligned)

DDD + Hexagonal layer split (see `docs/concept-2026-06-14.md` once P0 lands):

| Layer | Where | Contents |
|---|---|---|
| Surface | `mcp` / `cli` | MCP Tool surface + clap subcommand surface |
| Application | `core::application` | Use cases, `SpecRegistry` (dynamic), `ProjectionRegistry` (named) |
| Domain Core | `core::domain` | 6 primitive: Graph / CRUD / Compute / Constraint / AutoVersion / **Specification** |
| Infrastructure | `core::infrastructure` | SQLite storage adapter, Rendering adapter |

### Why two-axis query

- **Dynamic axis** — `Specification` (Evans / Fowler BP): composable first-class predicate object (`and` / `or` / `not`) for arbitrary runtime queries.
- **Fixed axis** — `NamedProjection` (CQRS Read Model): registered named query + template + target form (Prompt / Markdown / JSON / ASCII).

The **ProjectionAsPrompt** core (= the heart of wire's value prop) emerges from chaining: `Specification` → traversal → template binding → `Rendering Adapter` → consumable form.

## Build

```sh
cargo check --workspace
cargo build --workspace
cargo test --workspace
```

## Run

```sh
# CLI
cargo run -p persona-wire -- init --db /tmp/wire.db

# MCP server (stdio transport) — `mcp` subcommand dispatches into persona-wire-mcp::serve_stdio
cargo run -p persona-wire -- mcp
```

## Status

Skeleton land. P0 design doc + P1 storage/use-case wiring pending.

See `docs/ROADMAP.md` once it lands for phase plan (P0 doc → P1 storage+core → P2 adapters → P3 daemon → P4 RelationMap → P5 WorkflowEngine).

## License

Dual: MIT OR Apache-2.0.
