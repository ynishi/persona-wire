# persona-wire-core::application::bundle_install

Bundle install use case — TOML parse → name resolution → registry
dispatch → install report → install log append.

Consumed by `wire_bundle_install` (MCP / CLI surface). The Bundle
[`BundleRegistry`](crate::application::bundle_registry::BundleRegistry)
owns CRUD on the `bundles` table; this module owns the parse +
dispatch flow that turns one Bundle row into many registry writes.

# Sections handled (v1)

- `[[specs]]` → [`SpecRegistry`]
- `[[projections]]` → [`ProjectionRegistry`]
- `[[nodes]]` → `SqliteStorage::insert_node`
- `[[edges]]` → `SqliteStorage::insert_edge`

`[[wirings]]` / `[[workflows]]` dispatch is the same shape — handled
through the existing `wire_workflow_register` flow when wired up at
the MCP surface. The install report carries per-entity rows so each
section can be extended without changing the public report shape.

# Conflict resolution

Name conflict policy is selected per-install via
[`ConflictMode`](crate::domain::entity::bundle::ConflictMode):

- `Increment` (default) — entity name auto-increments (`-1` / `-2` ...)
  until a free slot is found. Internal references inside the same
  bundle (e.g. `projections.spec_ref` pointing at `specs.name`) are
  rewritten to the final name.
- `Skip` — leave the existing entity, record the collision in the
  install report's `skipped[]`.
- `Error` — abort on first collision. Nothing is written.

# Atomicity

v1 is **non-transactional** — dispatch iterates section-by-section
against the registries. Failures partway through leave previously
installed entities in place; the install report's `errors[]` lists
the boundary. SQLite transaction wrapping is a follow-up carry.

## Functions

- `install_bundle` — Install the entities declared in `bundle.body` into the storage's

## Types

- `BundleManifest` — Top-level TOML manifest deserialized from `Bundle.body`. All section
- `EdgeEntry` — (no documentation)
- `NodeEntry` — (no documentation)
- `ProjectionEntry` — (no documentation)
- `SpecEntry` — (no documentation)
- `WiringEntry` — Wiring section entry — one slot binding for one persona.
- `WorkflowEntry` — Workflow section entry — mirrors the shape consumed by

