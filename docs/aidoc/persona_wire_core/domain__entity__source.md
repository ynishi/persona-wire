# persona-wire-core::domain::entity::source

`Source` Entity — SoT location (URI) that a `Wiring` points at.

Wraps a URI string with minimal Domain-side invariants. The detailed parse
(typed scheme / host / path / query view) lives in the infrastructure layer
([`crate::infrastructure::wire_uri::WireUri`]) and is used by the adapter
dispatcher (`PluginRegistry::route`). Domain Entity layer intentionally
avoids depending on infrastructure types.

## Invariants

- **non-empty**
- **scheme prefix present** — value matches `<scheme>:<rest>` where
  `<scheme>` is non-empty.

Strict scheme grammar validation (ALPHA-first, ALPHA/DIGIT/`+-.`) is the
infrastructure layer's concern; surfacing it on construction belongs to
the adapter route step, not to the Domain Entity. This keeps Source
cheap to construct and free from infra coupling.

Owned by [`crate::domain::entity::wiring::Wiring`] (Step C land carry).

## Types

- `Source` — Source URI Value Object.

