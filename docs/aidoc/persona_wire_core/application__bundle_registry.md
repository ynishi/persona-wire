# persona-wire-core::application::bundle_registry

Bundle registry — PoEAA Registry pattern (Fowler PoEAA Ch.18) for
named [`Bundle`] lookup, mirroring the
[`crate::application::spec_registry::SpecRegistry`] /
[`crate::application::projection_registry::ProjectionRegistry`] stance.

# Pattern selection (SoT)

- **PoEAA Registry** — application-layer service that provides named
  access to well-known objects. `BundleRegistry::register / get / list
  / delete` is the typed lookup surface; CLI / MCP / use cases reach a
  `Bundle` through this Registry, never by reaching into
  `SqliteStorage` directly. **This is the chosen pattern.**
- **DDD Repository** — not adopted, same rationale as Projection
  (collapses the application service into a pass-through).

# Scope (v1)

`BundleRegistry` owns **CRUD** only:
- `register`  — upsert by name (`-1` / `-2` ... auto-increment lives in
  the install use case, not here; `register` itself overwrites on
  same-name to match `SpecRegistry::register` semantics).
- `get` / `get_by_id` — name- or id-based lookup.
- `list` — name-ascending summary.
- `delete` — by name or id; install history (`bundle_installs`) is
  intentionally preserved across bundle deletion for the future
  History UI.

Bundle **install** (TOML parse → name resolution → dispatch to
Spec/Projection/Wiring/Workflow registries) is a separate use case
that consumes `BundleRegistry::get`. It is intentionally not a
`register` post-hook because (a) a bundle may be registered, then
installed multiple times under different `ConflictMode`s, and (b)
parse-time errors should not block bundle registration.

## Types

- `BundleRegistry` — (no documentation)

