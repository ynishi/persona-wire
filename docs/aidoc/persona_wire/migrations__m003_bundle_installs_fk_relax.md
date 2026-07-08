# persona-wire::migrations::m003_bundle_installs_fk_relax

003 — relax `bundle_installs.bundle_id` to nullable + ON DELETE SET NULL.

Bundle v1's `bundle_registry` documentation contract states "install
history is intentionally preserved across bundle deletion", but the
initial 0.7.0 schema declared
`bundle_id TEXT NOT NULL REFERENCES bundles(id)` (= default RESTRICT),
which the SQLite engine enforces as a foreign_key_check on DELETE.
Result: `wire_bundle_delete` failed with `FOREIGN KEY constraint
failed` whenever an install log entry referenced the bundle (= every
installed bundle).

This migration applies the classic SQLite ALTER recipe to rebuild the
table with the corrected FK shape while preserving existing install
rows verbatim. Idempotent — re-running once `bundle_id` already
accepts NULL leaves the schema untouched.

## Types

- `Mig` — Zero-sized [`Migration`] impl for the

## Constants

- `MIGRATION` — Registry entry for migration 003 — referenced from

