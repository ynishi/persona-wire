# persona-wire-core::domain::entity::wiring

`Wiring` Entity — 1 slot binding within one persona's context.

A `Wiring` carries the natural composite key `(PersonaId, Slot)`, owns a
[`Source`] directly, and refers to a registered
[`crate::domain::entity::projection::Projection`] by identity
([`ProjectionName`]) — Vernon IDDD Rule 3 (cross-aggregate reference by
identity).

`Wiring` is **internal** to the entity layer: the Wire's external surface
is `Projection`, and reading raw `Wiring` data from the application would
bypass the rendering boundary. See the module-level "Surface policy" in
[`crate::domain::entity`] for details.

# Vocabulary (Slot vs. legacy `axis`)

Several legacy callsites and the storage shape carry the slot concept
under the field name `axis` (`Node.metadata["axis"]`,
`application::projection_naming::workflow_emit_projection_name`,
`<persona>.section.<axis>` derive). That name is a jargon placeholder:
`mailbox` / `mail` / `news` are not orthogonal axes, they are sibling
slot names inside one persona's context (see
[`crate::domain::entity::slot`] module docs). The entity carries
[`Slot`]; the mapper boundary translates `Slot ↔ metadata["axis"]` until
the storage rename is performed.

# Identity

Identity is the **natural composite key** `(PersonaId, Slot)`. The
existing graph storage keys wiring nodes by `format!("{persona}.{slot}")`,
and [`Wiring::storage_node_id`] exposes that legacy node-id form as the
bridge. A surrogate key (UUID) shape is plausible long-term but is a
separate persistence migration; the entity layer itself does not commit
to surrogate keys.

# Invariants

- `persona_id` + `slot` are validated through their VO constructors.
- `source` carries the SoT URI ([`Source`] enforces non-empty + scheme).
- `projection_ref` is optional — a wiring may exist before its renderer
  projection is registered. The runtime treats a missing projection as a
  skip + warning rather than a hard error.

# Persistence

Persisted through the existing Math backend Repository (`Node` CRUD via
[`crate::domain::graph`]). No dedicated Registry / DTO / table — see the
"Persistence" section in [`crate::domain::entity`] for the rationale.

## Types

- `Wiring` — Wiring Domain Entity.

