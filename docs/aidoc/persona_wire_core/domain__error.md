# persona-wire-core::domain::error

Error types layered by responsibility.

- [`DomainError`] — pure domain failures (invalid entity construction,
  constraint violations, unresolved references). Step C carry: every
  `domain::entity::*` constructor returns `Result<_, DomainError>`.
- [`WireError`] — top-level facade that wraps `DomainError` via `From`
  plus residual infrastructure / catch-all variants (`Storage` / `Other`).
  Application + Infrastructure layers may surface either layer's error.

Future split (Application / Infrastructure dedicated enums) is carry —
current scope keeps `Storage` / `Other` flat under `WireError`.

## Types

- `DomainError` — (no documentation)
- `WireError` — (no documentation)
- `WireResult` — (no documentation)

