# persona-wire-adapter-rss 0.14.0

persona-wire Adapter for RSS/Atom/JSON feeds (scheme `rss://`).

## Architecture

`RssAdapter` is a stateless [`Adapter`] impl split into three independent
functions:

- [`parse_rss_uri`] — `WireUri` → `RssUriSpec` (target URL + item limit).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient`
  (promoted to a shared crate 2026-07-07; no RSS-specific knowledge in the
  transport layer).
- [`normalize_feed`] — feed bytes → the Wire JSON shape below, via
  `feed_rs::parser::parse` (auto-detects RSS 2.0 / RSS 1.0 / Atom / JSON
  Feed; no manual format branching).

## URI grammar

```text
rss://<host>/<path>[?scheme=http][?limit=N]
```

- Default target is `https://<host><path>`; `?scheme=http` downgrades to
  plain HTTP (any other `scheme` value is ignored and falls back to
  `https`, matching the forward-compatible convention below).
- `?limit=N` caps the number of items returned (default
  [`DEFAULT_LIMIT`]). Parsed via the shared
  [`WireFilters::parse`]; unbounded declaration
  ([`FilterCap::Limit { max: None }`]). Non-numeric or zero fails loud;
  unknown filter-vocabulary keys (`?query=` etc.) fail loud too.
- Adapter-specific addressing keys (currently only `?scheme=`) are
  silently ignored when unknown (same forward-compatible convention as
  `persona-wire-adapter-obsidian`).
- An empty host is an error.

## Output shape

```json
{
  "feed":  { "title": "...|null", "url": "<fetched url>" },
  "items": [
    { "title": "...|null", "link": "...|null",
      "published": "<RFC3339>|null", "summary": "...|null" }
  ]
}
```

