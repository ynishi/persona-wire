# persona-wire-core::infrastructure::storage

SQLite storage adapter.

P1 scope: type_registry / nodes / edges / versions schema + CRUD primitive.
specifications / projections / workflow_runs tables are P2+ carry.

## Functions

- `default_db_path` — Resolve the default DB path for persona-wire. Follows the persona-x family

## Types

- `ProjectionFullRow` — Row tuple returned by `get_projection_full_*` / `list_projections_full`:
- `SpecificationFullRow` — Row tuple returned by `get_specification_full_*` / `list_specifications_full`:
- `SqliteStorage` — (no documentation)

