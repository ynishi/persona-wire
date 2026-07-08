# persona-wire-core::domain::graph::autoversion

AutoVersion primitive — append-only version chain with diff/rollback.

Every update to a node/edge inserts a new row with incremented `version`
and `prev_id` pointing to the previous version. Latest row wins for default
reads; historical rows kept for diff/rollback.

## Types

- `VersionRecord` — (no documentation)
- `VersionTargetKind` — (no documentation)

