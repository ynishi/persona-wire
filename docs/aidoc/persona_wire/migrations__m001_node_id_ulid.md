# persona-wire::migrations::m001_node_id_ulid

001 — `nodes` / `edges` stringly `id` → opaque ULID + `name` extraction.

Idempotent at the column level: if both `nodes.name` and `edges.name`
already exist, `up()` is a no-op (the framework still records the
migration as applied so [`pw-migrate status`] reflects the current
shape correctly even on a DB that was migrated before the framework
existed).

## Types

- `Mig` — Zero-sized [`Migration`] impl for the `001_node_id_ulid` schema

## Constants

- `MIGRATION` — Registry entry for migration 001 — referenced from

