# persona-wire-core::domain::entity::bundle

`Bundle` Domain Entity — scaffolding template that bundles
Spec / Projection / Wiring / Workflow (+ optional Node / Edge) into a
single TOML document for one-shot install.

Aggregate Root identified by [`BundleName`]. The body is the literal TOML
payload — parsing happens at install time, not at register time, so a
malformed bundle can be registered, inspected, then discarded without
corrupting the registry.

## Persistence pattern (SoT)

Same PoEAA Registry stance as `Projection`: the application-layer
[`crate::application::bundle_registry::BundleRegistry`] is the single
lookup surface. Persistence lives in the `bundles` SQLite table (one row
per bundle, `name` unique). Install history lives in `bundle_installs`
(one row per `install` use-case invocation) and feeds the future
History / Force / Undo carry.

## Invariants

- [`BundleName`] / [`BundleVersion`] — non-empty.
- `body` — non-empty TOML literal. Schema validity is **not** enforced
  at construction; the install use case parses and reports per-entity
  errors at dispatch time.

## Types

- `Bundle` — Registry-shaped Bundle row. `body` carries the literal TOML payload.
- `BundleId` — (no documentation)
- `BundleInstallReport` — Result of one bundle install. Always written to the `bundle_installs`
- `BundleName` — Bundle identifier Value Object. Non-empty.
- `BundleRef` — Lookup key for bundle CRUD use cases.
- `BundleVersion` — Bundle version Value Object. Non-empty. SemVer comparison logic is v2
- `ConflictMode` — Conflict resolution mode for `wire_bundle_install`.
- `ErrorItem` — (no documentation)
- `InstalledItem` — Per-entity outcome of one bundle install.
- `SkippedItem` — (no documentation)

