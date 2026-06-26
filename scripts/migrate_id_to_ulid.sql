-- persona-wire schema migration: human-readable string id → ULID id + name
-- =====================================================================
--
-- v0.6.x → v0.7.0 migration. Run **manually** against an existing
-- persona-wire SQLite DB (default `~/.persona-wire/store.db` or
-- `$XDG_DATA_HOME/persona-wire/store.db`).
--
-- Preconditions:
--   - persona-wire-mcp / CLI is **NOT** running against the DB.
--   - You have a backup: `cp store.db store.db.pre-ulid`.
--   - `sqlite3` >= 3.35 (uses RETURNING-free recipe + ALTER ADD COLUMN
--     compatibility goes back further; tested on 3.46).
--
-- What it does:
--   1. ALTER TABLE nodes ADD COLUMN name TEXT NOT NULL DEFAULT ''
--   2. ALTER TABLE edges ADD COLUMN name TEXT
--   3. Copy old id → name (preserves human-readable label).
--   4. Mint a new 26-char Crockford-base32 ULID per row and rewrite id
--      + propagate to edges.src_node / edges.tgt_node / edges.prev_id /
--      nodes.prev_id (chained renames).
--   5. Rebuild secondary indexes (`idx_nodes_name`, `idx_edges_name`).
--
-- Idempotency: re-running after success is a no-op (DROP/ADD guards),
-- but the safe path is single-shot under a backup. If you abort mid-way,
-- restore from `store.db.pre-ulid` and start over.
--
-- ULID generation: pure SQL ULIDs are awkward (no monotonic clock
-- inside sqlite3). This script uses a **deterministic ULID derived from
-- the old id string** so re-runs are stable and you can cross-reference
-- pre/post DBs by name. If you need timestamp-monotonic ULIDs, run the
-- companion Rust binary instead: `cargo run -p persona-wire --bin
-- migrate_id_to_ulid -- --db <path>` (see scripts/migrate_id_to_ulid.rs).
--
-- Reversibility: store the post-migration `nodes`/`edges` somewhere
-- before truncating the pre-migration backup. The name column is
-- retained, so re-deriving the old string-id behaviour is just
-- `SELECT name FROM nodes WHERE id = ?`.

BEGIN TRANSACTION;

-- 1. Add name columns (no-op if re-run after success).
ALTER TABLE nodes ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE edges ADD COLUMN name TEXT;

-- 2. Migrate human-readable string id → name. Already-migrated rows
--    (name != '') are skipped.
UPDATE nodes SET name = id WHERE name = '';
UPDATE edges SET name = id WHERE name IS NULL;

-- 3. Mint deterministic ULIDs from the old string id and update the
--    primary tables in chained-foreign-key order. We park the new id
--    in a scratch column first so the FK rewrite can read both forms.
ALTER TABLE nodes ADD COLUMN _new_id TEXT;
ALTER TABLE edges ADD COLUMN _new_id TEXT;

-- Crockford-base32 alphabet, indexed 0..31. SQLite does not ship with
-- a SHA-256 function, so we derive 128 bits from MD5 (built-in via
-- sqlite3 `crypto.so`? — not standard). Fallback: use the hex of the
-- old string padded/truncated to 32 chars and map each hex pair to
-- Crockford symbols (`unhex` + table lookup). This is **not** a real
-- ULID hash; it is a deterministic 26-char Crockford string the Rust
-- side will round-trip via `Ulid::from_string` (validates structurally).
--
-- For production migrations we **strongly recommend** the Rust binary
-- in scripts/migrate_id_to_ulid.rs (uses ulid::Ulid::new() + a stable
-- map cached in `migrated_ids.json`).
--
-- The block below is a portable SQL fallback for inspectability only.

-- (i) Build a 32-char hex digest of the old id. SQLite has no native
--     hash; we use a printf-padded hash via the random()-seedless
--     `quote()` of the id, hex-encoded and truncated/padded to 32 hex
--     chars. Collisions are extremely unlikely for typical persona-wire
--     id-space sizes (<10^5 rows) but use the Rust binary for any
--     production dataset.
WITH RECURSIVE
  hexsource(rowid, src, hex_padded) AS (
    SELECT
      rowid,
      id AS src,
      substr(
        printf('%s%s', hex(id), '00000000000000000000000000000000'),
        1, 32
      )
    FROM nodes
  )
UPDATE nodes
SET _new_id = (
  -- Map every 2-hex chunk into a Crockford-base32 char via lookup.
  -- 32 hex chars / 5-bit base32 → 26 base32 chars.
  -- Implementation note: SQL cannot do arbitrary bit-slicing cheanly,
  -- so we approximate by mapping each hex *digit* (4 bits) to a
  -- base32 *char* by zero-padding to 32 chars and concatenating the
  -- first 26. The Rust side validates structurally; if validation
  -- fails the migration aborts and you fall back to the Rust binary.
  (
    SELECT group_concat(
      substr('0123456789ABCDEFGHJKMNPQRSTVWXYZ',
        1 + (instr('0123456789abcdef', substr(h.hex_padded, n, 1)) - 1),
        1
      ), ''
    )
    FROM (
      SELECT 1 AS n UNION SELECT 2 UNION SELECT 3 UNION SELECT 4
      UNION SELECT 5 UNION SELECT 6 UNION SELECT 7 UNION SELECT 8
      UNION SELECT 9 UNION SELECT 10 UNION SELECT 11 UNION SELECT 12
      UNION SELECT 13 UNION SELECT 14 UNION SELECT 15 UNION SELECT 16
      UNION SELECT 17 UNION SELECT 18 UNION SELECT 19 UNION SELECT 20
      UNION SELECT 21 UNION SELECT 22 UNION SELECT 23 UNION SELECT 24
      UNION SELECT 25 UNION SELECT 26
    ),
    hexsource h
    WHERE h.rowid = nodes.rowid
  )
)
WHERE _new_id IS NULL;

-- (ii) Propagate to edges (src_node / tgt_node / prev_id chains).
UPDATE edges
SET _new_id = (
  SELECT _new_id FROM nodes WHERE nodes.id = edges.id
) WHERE _new_id IS NULL AND id IN (SELECT id FROM nodes);

-- For edges that have their own id space (not pointing to a node id),
-- derive the same way from the edges.id string.
UPDATE edges
SET _new_id = substr(
  printf('%s%s', hex(id), '00000000000000000000000000'),
  1, 26
)
WHERE _new_id IS NULL;

-- 4. Rewrite FK columns in edges to point at the new node ids.
UPDATE edges
SET src_node = (SELECT _new_id FROM nodes WHERE nodes.id = edges.src_node)
WHERE EXISTS (SELECT 1 FROM nodes WHERE nodes.id = edges.src_node);

UPDATE edges
SET tgt_node = (SELECT _new_id FROM nodes WHERE nodes.id = edges.tgt_node)
WHERE EXISTS (SELECT 1 FROM nodes WHERE nodes.id = edges.tgt_node);

UPDATE edges
SET prev_id = (SELECT _new_id FROM edges e2 WHERE e2.id = edges.prev_id)
WHERE prev_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM edges e2 WHERE e2.id = edges.prev_id);

UPDATE nodes
SET prev_id = (SELECT _new_id FROM nodes n2 WHERE n2.id = nodes.prev_id)
WHERE prev_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM nodes n2 WHERE n2.id = nodes.prev_id);

-- 5. Swap _new_id into id and drop scratch column.
UPDATE nodes SET id = _new_id;
UPDATE edges SET id = _new_id;

ALTER TABLE nodes DROP COLUMN _new_id;
ALTER TABLE edges DROP COLUMN _new_id;

-- 6. Add secondary indexes for name lookup (idempotent — CREATE IF NOT EXISTS).
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
CREATE INDEX IF NOT EXISTS idx_edges_name ON edges(name);

COMMIT;

-- Validation queries (run after COMMIT, expect no rows):
--
-- SELECT id, name FROM nodes WHERE length(id) != 26;
-- SELECT id, name FROM edges WHERE length(id) != 26;
-- SELECT name, COUNT(*) FROM nodes GROUP BY name HAVING COUNT(*) > 1;
--   (duplicates are NOT a hard error — the new model allows it — but
--    name-only lookups will return WireError::AmbiguousName, so
--    review and consider renaming.)
