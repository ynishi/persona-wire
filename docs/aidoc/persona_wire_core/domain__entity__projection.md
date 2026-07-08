# persona-wire-core::domain::entity::projection

`Projection` Domain Entity — rendering intent for a `Wiring`.

Aggregate Root identified by [`ProjectionName`]. Carries the rendering
intent (which Specification to evaluate + which template / form / plugin
to render with). Persisted via the application-layer
[`crate::application::projection_registry::ProjectionRegistry`] using the
Data Mapper pattern (Fowler PoEAA Ch.10).

## Persistence pattern (SoT)

- **PoEAA Registry** ([`crate::application::projection_registry`]) —
  named application-layer lookup surface (`register / get / list`).
  This is the only entry point CLI / MCP / use cases use to reach a
  `Projection`.
- **PoEAA Data Mapper** ([`crate::application::projection_mapper`]) —
  shape translation between the [`Projection`] Entity (this module)
  and `NamedProjection` (the SQLite row mirror DTO). The Registry
  owns the Mapper bridge — persona-wire takes the **narrow** reading
  of Fowler's Mapper class and does not split out a separate
  `Mapper<Dto, Entity>` trait until a second parallel mapper exists.
- **DDD Repository** — **not adopted.** A Domain Port trait would
  collapse the application-layer Registry into a pass-through; the
  PoEAA Registry stance is intentional. See
  [`crate::application::projection_registry`] module docs for the
  recorded decision.

## Invariants

- [`ProjectionName`] / [`SpecName`] / [`ProjectionTemplate`] — non-empty.
- [`TargetForm`] — value domain enforced by the enum itself.
- [`PluginDispatch`] — `Default` (= framework defaults) or `Custom { engine,
  kind, config }` with non-empty `engine` / `kind`. The 3 Optional-field
  shape used at the persistence boundary collapses to these two states;
  illegal combinations (engine only / kind only) are rejected at the
  mapper boundary.

Cross-aggregate referential integrity (the [`SpecName`] actually resolving
against a `SpecRegistry` row) is **not** enforced here — that is a
soft reference handled by `wire_render` / `wire_doctor` at use-case time.

## Vernon IDDD Rule 3 (Identity-by-Name)

[`SpecName`] is the Identity Value Object that lets `Projection` reference
the `Specification` aggregate by name only (no aggregate-to-aggregate
pointer). The legacy type alias [`SpecRef`] is preserved as a re-export
for back-compat — new code should prefer `SpecName`.

## Types

- `PluginDispatch` — Plugin dispatch hints, modelled to eliminate illegal states at the type
- `Projection` — Domain Entity for a registered persona-wire projection.
- `ProjectionId` — (no documentation)
- `ProjectionName` — Projection identifier Value Object. Non-empty.
- `ProjectionTemplate` — Render template body Value Object. Non-empty.
- `SpecName` — Identity Value Object for a registered `Specification`. Non-empty.
- `SpecRef` — Back-compat alias for [`SpecName`]. Kept so external crates (and the
- `SpecificationId` — (no documentation)
- `TargetForm` — Render output form. Domain vocabulary — moved from `application` so

