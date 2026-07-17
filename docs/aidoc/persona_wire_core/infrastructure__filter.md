# persona-wire-core::infrastructure::filter

Unified adapter filter vocabulary + shared parser.

## Architecture

Before this module, cross-cutting query params (`?limit=`, `?tail=`,
`?since=`) were parsed independently by each adapter (`FileAdapter`'s
`parse_tail_mode`, the todoist/notion/slack adapters' own ad-hoc
`limit` parsing, ...), so the same semantic filter grew 3 slightly
different error dialects across the workspace: some silently ignored
bad input, some clamped, some fail-loud'd with adapter-specific message
shapes. [`FilterCap`] / [`WireFilters`] / [`WireFilters::parse`]
consolidate that into **one closed vocabulary** (7 known keys) and
**one error policy** (see the table below), so every adapter that opts
in via [`crate::infrastructure::adapter::Adapter::filter_caps`] gets
identical parsing behavior for free.

## Filter vs Addressing

A `source_uri` query string carries two orthogonal kinds of keys:
- **Filter keys** — cross-cutting, adapter-agnostic slicing/selection
  (`limit` / `lines` / `tail` / `tail_n` / `since` / `until` / `query`).
  This module owns their grammar and error policy.
- **Addressing keys** — adapter-specific identity/routing params
  (`kind`, `state`, `auth`, `project_id`, ...). These stay entirely
  inside each adapter's own parser; [`WireFilters::parse`] never
  inspects or rejects them (see the "unknown query keys are silently
  ignored" convention documented in
  [`crate::infrastructure::adapter`]).

## Unified error policy

| condition | behavior |
|---|---|
| filter key absent | field stays `None` (default) |
| value present but wrong type (`limit=abc`, `lines=x-y`, `tail_n=abc`, `tail=unknown`) | `Err` (fail loud) |
| value well-typed but exceeds a declared cap (`limit=5000` w/ `max=Some(40)`, `tail_n=5000` w/ `n_max=1000`) | clamp to the cap + `tracing::warn!` |
| `limit=0` (list-shaped filter with no useful semantics for zero) | `Err` |
| `lines=FROM-TO` with `FROM > TO`, or `FROM == 0` (1-origin violation) | `Err` |
| filter key present in the URI but not declared in `caps` | `Err` ("not supported by this adapter") |

## Types

- `FilterCap` — A cross-cutting filter capability an [`Adapter`](crate::infrastructure::adapter::Adapter)
- `TailSpec` — Tail-slicing variant for [`WireFilters::tail`] (promoted from the
- `WireFilters` — Parsed cross-cutting filters for one `source_uri` fetch, produced by

