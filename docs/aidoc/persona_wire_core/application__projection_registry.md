# persona-wire-core::application::projection_registry

Projection registry — PoEAA Registry pattern (Fowler PoEAA Ch.18) for
named [`Projection`] lookup, routing through the Data Mapper boundary
at [`projection_mapper`](super::projection_mapper).

# Pattern selection (SoT)

- **PoEAA Registry** (Fowler PoEAA Ch.18) — application-layer service
  that provides named access to well-known objects, a structured
  alternative to global access. `ProjectionRegistry::register / get /
  list` is the typed lookup surface; the persona-wire CLI / MCP / use
  cases consume `Projection` through this Registry, never by reaching
  into `SqliteStorage` directly. **This is the chosen pattern.**
- **DDD Repository** (Evans DDD Ch.6 / Vernon IDDD Ch.12) — a Domain
  Port (trait) that abstracts Aggregate persistence so the Domain
  depends only on the abstraction. **Not adopted.** Replacing the
  Registry with a Repository trait would move persistence vocabulary
  into `domain/port/` and collapse the application service into a
  pass-through, breaking the PoEAA-narrow stance recorded below.

Fowler PoEAA's Data Mapper (Ch.10) requires *some* mapper to translate
between persistence shape and Domain shape. The literal pattern has an
independent Mapper class; persona-wire takes the **narrow** reading and
lets the Registry own that bridge through the
[`projection_mapper`](super::projection_mapper) module. That keeps one application-layer entry point for everything
`name`-addressable about a Projection: lookup, persistence, and DTO
translation are co-located rather than spread across an artificial
Repository / Mapper / Registry trio.

# Layering

```text
CLI / MCP / use_cases.rs
        │
        ▼
ProjectionRegistry          ← PoEAA Registry (this module)
        │
        ▼
projection_mapper           ← Data Mapper boundary (DTO ↔ Entity)
        │
        ▼
SqliteStorage               ← Infrastructure (column tuple primitives)
```

The DTO (`NamedProjection`) + Entity round-trip lives in
[`projection_mapper`](super::projection_mapper). This module owns only
the SQLite column tuple ↔ DTO translation (`upsert_dto` / `get_dto`)
and the `register / get / list` flow surface. A follow-up carry pushes
the column-tuple half down to `projection_mapper` as well, leaving the
Registry as a pure named-lookup facade.

# Sibling consumers

[`wiring_mapper`](super::wiring_mapper) /
[`workflow_mapper`](super::workflow_mapper) are sibling Data Mappers
invoked directly from `use_cases.rs` against the Math backend Node
Repository — Wiring / Workflow do **not** have a Registry counterpart
because they are persisted as graph nodes, not as a separately-named
table. The Registry layer is Projection-specific by design.

## Types

- `ProjectionRegistry` — (no documentation)
- `ProjectionRow` — Full registry row read surface for `wire_projection_get` /

