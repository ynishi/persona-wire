# persona-wire-adapter-todoist 0.12.0

persona-wire Adapter for Todoist (scheme `todoist://`).

## Architecture

`TodoistAdapter` is a stateless [`Adapter`] impl split into three
independent functions:

- [`parse_todoist_uri`] — `WireUri` → `TodoistUriSpec` (kind + optional
  project filter / natural-language filter + item limit).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
  Todoist-specific knowledge in the transport layer).
- Internal `next_cursor` loop in [`Adapter::fetch`] — accumulates
  Todoist API v1 response pages (`{"results": [...], "next_cursor":
  ...}`) into the Wire JSON shape below, branching only on `kind` for
  the per-item normalization.

## URI grammar

```text
todoist://tasks[?project_id=<id>][?filter=<query>][&limit=N]
todoist://projects[?limit=N]
```

- `host` selects the endpoint kind (`tasks` / `projects`); an empty or
  invalid value **fails loud** — a typo here would otherwise silently
  return a different class of data (matching
  `persona-wire-adapter-github`'s `?kind=` convention).
- The path must be empty (or `/`); any additional path segment fails
  loud.
- For `kind=tasks`: `?filter=<query>` (a Todoist natural-language filter,
  e.g. `today | overdue`) selects the `/tasks/filter` endpoint;
  otherwise `/tasks` is used, optionally scoped by `?project_id=<id>`.
  **`filter` and `project_id` are mutually exclusive** — the filter
  endpoint does not accept a `project_id` parameter, so specifying both
  fails loud rather than silently dropping one.
- For `kind=projects`, `project_id` and `filter` are unknown query keys
  (silently ignored, not even read).
- `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
  A non-numeric or zero value fails loud; there is no upper bound at
  parse time. [`MAX_LIMIT`] (the Todoist API's own per-request cap of
  200) is enforced only when the adapter builds the upstream request URL;
  `?limit=N` with `N > MAX_LIMIT` triggers the internal pagination loop
  (see "Pagination" below).
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss` / `-github`).
- The `filter` value is percent-decoded once at parse time (it commonly
  contains spaces and `|`, e.g. `today | overdue`, and callers may supply
  it either raw or percent-encoded), then percent/form-encoded exactly
  once when building the request URL (via `url::Url::query_pairs_mut`).

## Auth

Resolved per-fetch (not at boot) via
`persona_wire_credentials::Credentials::default_chain().get("todoist")`.
Unlike `persona-wire-adapter-github`, Todoist has no unauthenticated
access mode — a missing token **fails loud**. Set a token via the
`PERSONA_WIRE_TOKEN_TODOIST` or `TODOIST_API_TOKEN` environment variable,
or store one in the OS keychain via `persona-wire token set todoist`. The
token is found under Todoist Settings → Integrations → Developer.

Todoist enforces a rate limit of roughly 1,000 requests per 15 minutes
per user; exceeding it returns HTTP 429, which surfaces as a normal fetch
failure via `persona_wire_transport_http::HttpClient`.

## Output shape

```json
{ "kind": "tasks", "items": [ ... ], "has_more": false }
```

`has_more` is `true` when the adapter truncated the result at `?limit=N`
and the upstream still had more items available. It is `false` when the
loop terminated because Todoist's `next_cursor` was `null`.

`items` entries for `kind=tasks`:

```json
{
  "id": "...|null", "content": "...|null",
  "description_excerpt": "...|null", "project_id": "...|null",
  "priority": 1, "labels": ["..."],
  "due": { "date": "...|null", "string": "...|null", "is_recurring": false } | null,
  "deadline_date": "...|null", "completed_at": "...|null",
  "added_at": "...|null", "updated_at": "...|null"
}
```

`items` entries for `kind=projects`:

```json
{
  "id": "...|null", "name": "...|null", "color": "...|null",
  "is_favorite": false, "is_archived": false, "is_inbox": false,
  "view_style": "...|null"
}
```

## Pagination

`Adapter::fetch` drives the pagination loop internally: it follows the
response body's `next_cursor` field (an opaque token; `null` signals
end-of-data) across repeated requests until it has accumulated `?limit=N`
items or the upstream signals end-of-data. The cursor form is a private
implementation detail — the wire layer only sees the final assembled
`{kind, items, has_more}` shape. Every upstream request is sent with
`limit = MAX_LIMIT` (Todoist's own per-request ceiling of 200), so the
loop runs once for `?limit <= MAX_LIMIT` and continues page-by-page for
larger requests.

