# persona-wire-core::application::doctor::probes::graph_edges_zero

# graph.edges_zero

Detects the "fresh database" anomaly: a graph with zero edges has not
had its wire layer provisioned yet. Emitted as [`Severity::Error`] in
Full mode only.

## Persona-scoped mode skip (Phase A)

When [`ProbeCtx::persona_filter`] is `Some`, [`GraphEdgesZero::scan`]
returns immediately without emitting any finding. In Phase A a single
persona is **not** the edge-based target of the graph axis — its
operation is closed over the persona-pack overlay, [Wiring entries],
and [Workflow triggers] — so reporting `edges=0` as a hard failure
for one persona is a design mismatch (cf. issue `9f70b493`).

## Lifting the Phase A skip (Phase β path)

When persona-to-persona graph wiring is formalised, the early return
in [`GraphEdgesZero::scan`] should be removed. Phase β prerequisites:

1. A persona-to-persona edge type (e.g. `routes_to_persona`) is
   registered in `type_registry`.
2. `persona-pack` declares an opt-in flag (under `[extra.persona_wire]`
   or its successor) for "subject to graph-axis health checks".
3. A migration issue lands that removes the
   `ctx.persona_filter.is_some()` early-return.

## `workflow_def` exclusion (phase-invariant)

[`workflow_def`] Nodes are excluded from the edge tally below. A
Workflow Entity completes its lifecycle via trigger/action and never
participates in edge-based wiring; counting its (always-zero) edges
would mask real "wire not provisioned" signals once a single workflow
is registered. This exclusion is invariant across Phase A/β.

[`workflow_def`]: crate::application::workflow_mapper::WORKFLOW_TYPE
[Wiring entries]: crate::application::wiring_mapper
[Workflow triggers]: crate::domain::entity::workflow

## Types

- `GraphEdgesZero` — (no documentation)

