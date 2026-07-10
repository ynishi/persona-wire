# persona-wire-core::application

Application layer — use cases and registries.

Holds:
- [`spec_registry`]       — Specification registry (dynamic / composable selector)
- [`projection_registry`] — NamedProjection registry (fixed / named view)
- [`use_cases`]           — wire_init / wire_close / wire_doctor / etc. flows
- [`auth`]                — indirect authentication reference layer
  (`AuthSpec` / `AuthResolver`) consumed by adapter fetches via the
  `?auth=<service_key>` URI query param convention

