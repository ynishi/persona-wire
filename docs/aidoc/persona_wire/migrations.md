# persona-wire::migrations

Schema migration framework — Diesel / sqlx style, scoped to persona-wire's
SQLite store. Each numbered migration declares an idempotent `up` step
that the [`Runner`] tracks in a `schema_migrations` table so re-running
is a no-op once applied.

Migrations are listed in [`ALL`] in **execution order**. A new schema
change adds one module under this directory plus one entry at the end of
`ALL`. Down migrations are intentionally not modelled in v1 — the
framework leaves room for a future `Migration::down` extension once a
real rollback need shows up.

## Types

- `AppliedNow` — (no documentation)
- `AppliedRow` — (no documentation)
- `Runner` — Driver around a `Connection` that knows how to read / write the
- `Status` — (no documentation)

## Traits

- `Migration` — One immutable, monotonic schema change. `id` must be unique across the

## Constants

- `ALL` — Registry of all known migrations, in **execution order**. Append-only.

