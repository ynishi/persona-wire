# persona-wire-core::application::projection_mapper

Mapper boundary: [`Projection`] Domain Entity ↔ `projections` table row.

Fowler PoEAA Data Mapper (Ch.10) — `NamedProjection` is the
persistence-shape DTO (anemic row mirror), [`Projection`] is the Domain
Entity carrying VO and cross-field invariants. This module is the
**single SoT** for translating between the two shapes;
[`ProjectionRegistry`](super::projection_registry::ProjectionRegistry)
and any future projection consumer route through here instead of
touching the DTO struct directly.

# Pattern selection (SoT)

Persona-wire takes the **narrow** reading of Data Mapper: the Registry
(PoEAA Ch.18) acts as the Mapper bridge through this module, instead of
introducing an independent `Mapper<Dto, Entity>` trait. See
[`projection_registry`](super::projection_registry) module docs for the
PoEAA Registry vs DDD Repository decision recorded in code.

Promoting this to a literal Fowler Mapper trait is a carry that fires
only when a second parallel Mapper (Spec Mapper / overlay Mapper)
arrives and the inherent helpers start duplicating shape — until then,
the free functions below are intentionally not behind a trait.

Sibling of [`wiring_mapper`](super::wiring_mapper) and
[`workflow_mapper`](super::workflow_mapper). The three together complete
the Data Mapper land for the entity layer.

Storage form (cf. `domain/entity/projection.rs` module docs):

```text
Row {
  name:               String,
  spec_ref:           String,
  template:           String,
  target_form:        "prompt" | "markdown" | "ascii" | ...,
  template_engine:    Option<String>,
  projection_kind:    Option<String>,
  projection_config:  Option<Value>,   // JSON
}
```

`PluginDispatch` is flattened to the three optional columns at the DTO
boundary; the Entity carries the discriminated `Default | Custom { .. }`
shape so application code never sees the loose `Option` triple.

Round-trip property: `dto_to_projection(projection_to_dto(p))? == p` for
any [`Projection`] constructed through its `from_parts` constructor.

## Functions

- `dto_to_projection` — DTO → Domain Entity. Runs all VO validations and rejects illegal
- `projection_to_dto` — Domain Entity → DTO. Total (no failure path) — Entity invariants are

## Types

- `NamedProjection` — Persistence DTO. Anemic by design — invariants live in [`Projection`].

