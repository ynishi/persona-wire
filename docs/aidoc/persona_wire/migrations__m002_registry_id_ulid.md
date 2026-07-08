# persona-wire::migrations::m002_registry_id_ulid

002 — `specifications` / `projections` rebuild: `name TEXT PK` →
`id TEXT PK + name TEXT NOT NULL UNIQUE`.

SQLite cannot rename a PK in place, so this follows the canonical
12-step ALTER recipe (CREATE new + INSERT SELECT + DROP old + RENAME).
Idempotent: if both tables already have an `id` column, body is a no-op.

## Types

- `Mig` — Zero-sized [`Migration`] impl for the `002_registry_id_ulid`

## Constants

- `MIGRATION` — Registry entry for migration 002 — referenced from

