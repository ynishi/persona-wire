# persona-wire-core::domain::entity::persona_id

`PersonaId` — owner identity Value Object.

Wraps a non-empty `String` that names a persona registered in the external
`persona-pack` SoT. The persona's full identity (name / role / overlays)
lives in `persona-pack`; `ContextWiring` only carries the id by reference.

## Invariants

- **non-empty** — `PersonaId::new("")` returns `DomainError::InvalidPersonaId`.

Character set / length bounds are persona-pack's responsibility (external
SoT). Domain Entity layer keeps the contract minimal so id values coming
from any persona-pack revision remain compatible.

## Types

- `PersonaId` — Persona identifier Value Object.

