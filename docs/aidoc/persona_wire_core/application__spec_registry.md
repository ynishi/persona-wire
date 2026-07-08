# persona-wire-core::application::spec_registry

Specification registry — store dynamic / composable Specifications by name.

Backed by the storage layer (`specifications` table). Each entry is the
JSON-serialised form of a `Specification`. Domain-neutral: callers register
arbitrary Specifications (BP: Specification pattern).

## Types

- `SpecRegistry` — (no documentation)
- `SpecRow` — Full registry row read surface for `wire_spec_get` / `wire_spec_list` —

