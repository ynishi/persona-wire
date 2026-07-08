# persona-wire-core::domain::entity::context_wiring

`ContextWiring` — per-persona Composition Root (Aggregate Root identity).

Marks the consistency boundary for one persona's context (its
[`crate::domain::entity::wiring::Wiring`] set and
[`crate::domain::entity::workflow::Workflow`] set). The boundary is
represented by [`PersonaId`] alone — there is exactly one
`ContextWiring` per persona, no surrogate id.

# Skinny by design

`ContextWiring` does **not** hold `Vec<Wiring>` / `Vec<Workflow>` in
memory. The wirings and workflows that belong to a persona live in the
Math backend graph (`Node` rows of type `outline_node` / `workflow_def`)
and are reached through the Repository (`crate::domain::graph`) when a
caller actually needs them. The Aggregate Root only carries the identity
that says "these are the rows that move together as one consistency
unit".

The persona-scoped **read view** lives in
[`wire_context_get`](crate::application::use_cases::wire_context_get):
the application use case takes a `ContextWiring` (or its persona id),
walks the consistency boundary via the Math backend repository, and
returns a summary snapshot. Multi-`Wiring` atomic write boundaries
(reset / replication / migration) are deliberately not modelled yet;
until a real use case appears, batch writes stay non-atomic at the
application surface and the Aggregate Root keeps no in-memory collection
state.

Keeping the traversal in the application layer preserves the standard
DDD layering (`domain → ⊥`, no inbound dependency on application or
infrastructure) — the same discipline the rest of `domain::entity`
follows.

# Surface

Not re-exported at the entity module root. The Wire's external surface
is [`Projection`]; the Aggregate Root is internal vocabulary, used by
entity-layer composition (today) and by future application code that
needs an explicit consistency-boundary handle.

[`PersonaId`]: crate::domain::entity::persona_id::PersonaId
[`Projection`]: crate::domain::entity::projection::Projection

## Types

- `ContextWiring` — Per-persona Composition Root.

