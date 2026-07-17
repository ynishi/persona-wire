# persona-wire-core::infrastructure::adapter

Layer 6 Adapter (SoT) — reflects concept-doc §3 Layer 6 + §5 #3 / §P3b.

The core keeps only the Adapter trait and the bundled `FileAdapter`. Each
wiring entry node's `metadata.source_uri` is dispatched by scheme — plugin
axis 1 of 3.

Bundled scheme:
- `file://<absolute-or-tilde-path>` — reads the raw contents via
  std::fs::read (json/toml parsing is a future extension; currently the
  contents are returned as a string).

  Query param extensions (R5, now routed through the unified
  [`crate::infrastructure::filter`] vocabulary via
  [`Adapter::filter_caps`] / [`crate::infrastructure::filter::WireFilters::parse`]):
  - `?tail=last_section` — the trailing section (split at markdown `## `
    h2 boundaries; returns everything from the last h2 onward)
  - `?tail_n=<N>` — the last N lines (line-based; capped at
    [`TAIL_N_MAX`] = 1000 lines as a context size guard)
  - `?lines=<FROM>-<TO>` — a 1-origin inclusive line range; `TO` beyond
    the total line count clamps gracefully, `FROM` beyond the total
    returns an empty body. Mutually exclusive with `?tail` / `?tail_n`
    (fails loud if both are present).
  - no query param → fetch the whole file (backward-compat)
  - unknown / unparsable values → **fail loud** (`Err`), per the unified
    filter error policy (behavior changed from the earlier graceful
    whole-file fallback; see `filter` module docs)

  Metadata (R4):
  - `size_bytes` — size of the whole file in bytes (the original file
    size, not the post-tail body size)
  - `modified_at` — last modified time (Unix epoch seconds, u64)
  - `metadata` — nested metadata object (`filename` / `full_path` /
    `last_modified` / `size_bytes` / `age_days`)

Provided by external crates (split out in P3b):
- `mini-app://<table>...` → `persona-wire-adapter-mini-app` crate (`MiniAppAdapter`)

The outline / persona-pack / journal schemes are carried by external
adapter crates.

## Adapter authoring guide (conventions for adding a new scheme)

Add one crate per scheme, named `persona-wire-adapter-<scheme>`, to the
workspace. The canonical reference is `persona-wire-adapter-rss` (minimal,
stateless, direct SDK integration).

- **Three-function split**: `parse_<scheme>_uri` ([`WireUri`] → Spec struct),
  transport fetch (no domain knowledge), and `normalize_<scheme>`
  (raw response → Wire JSON shape). HTTP transport is provided by the
  shared `persona-wire-transport-http` crate (promoted 2026-07-07);
  HTTP-backed adapters use its `HttpClient` instead of hand-rolling
  `reqwest` calls.
- **Guard constants**: declare item caps / timeouts / text truncation as
  `pub const` (rss example: `DEFAULT_LIMIT=20` / `FETCH_TIMEOUT=30s` /
  `SUMMARY_MAX_CHARS=500`). Align timeouts with existing adapters
  (`DEFAULT_RPC_TIMEOUT` in the mcp adapter).
- **Error / query conventions**: missing or invalid required components
  (empty host, `limit=0`, ...) fail loud with [`WireError::Storage`].
  Unknown query keys are silently ignored (forward-compat convention).
  Missing output fields are `null`; timestamps are RFC3339. A missing
  source is graceful (`FileAdapter` in this file: non-existent path →
  `body: null` with `Ok`).
- **Docs**: `#![warn(missing_docs)]` plus a crate-root `//!` header with
  three sections: Architecture / URI grammar / Output shape.
- **Tests**: parse / normalize are offline unit tests over inline
  fixtures. Never add tests that depend on live network access.
- **Registration**: add one `.with_adapter(XxxAdapter)` line to the
  `PluginRegistry::default_builder_for_wire()` chain on the boot side
  (`persona-wire-mcp/src/lib.rs`). Scheme collisions fail fast at
  registry build time.
- **Pagination**: adapters that support `?limit=N` where `N` can exceed a
  single upstream page MUST drive the pagination loop internally inside
  `Adapter::fetch` and emit a truthful `has_more` field distinguishing
  truncated-at-limit from upstream-exhausted. Cursor form (Link header,
  NextToken, offset, ...) is a private implementation detail — the wire
  layer never sees it. Adapters that ignore `?limit` (single-shot fetches
  such as `FileAdapter`) simply return their canonical shape.

## External service integration policy (decided 2026-07-07)

- When a service exposes a public SDK / API, call it **directly** from the
  adapter. Do not relay through an MCP integration (this is a core benefit
  of the Rust + Adapter pattern).
- UX first: never make the user repeat authentication. Receive credentials
  via environment variables and never embed secrets in `source_uri`.
  Choose the auth mechanism per SDK on a case-by-case basis.
- Adapter expansion targets coverage (including minor services), not
  demand-ranked prioritization. The only exclusion criterion is that the
  service has been discontinued.

### `?auth=<service_key>` query param convention (Phase 1, decided
together with `application::auth`)

Every HTTP-authenticated adapter honors an optional `?auth=<service_key>`
query param on its `source_uri`:

- `<service_key>` is a **credential reference key only** — never a
  secret. It is looked up via
  `persona_wire_credentials::Credentials::get(service_key)` (env var →
  OS keyring), exactly like the adapter's own literal default service
  name (e.g. `"github"`).
- When present, `<service_key>` **overrides** the adapter's literal
  default service name for that one fetch (e.g. `?auth=github-alt` looks
  up the `github-alt` credential instead of `github`) — lets one wiring
  entry authenticate as a different identity than another entry using
  the same adapter/scheme.
- When absent, behavior is unchanged: the adapter's literal default
  service name is used (full backward compatibility).
- This is an ordinary query key from every adapter's own URI-grammar
  perspective — it follows the same "unknown query keys are silently
  ignored" convention documented above, so adding `?auth=` never
  conflicts with an adapter's own `?kind=` / `?limit=` / etc. params.

## Types

- `FileAdapter` — Bundled `file://` / `file:` scheme [`Adapter`] backed by `std::fs`. See

## Traits

- `Adapter` — Adapter trait — plugin axis 1 of 3 (SoT Adapter).

## Constants

- `TAIL_N_MAX` — Upper bound for `N` in `?tail_n=<N>` (context size guard).

