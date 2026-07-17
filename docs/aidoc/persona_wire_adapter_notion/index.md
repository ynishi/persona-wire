# persona-wire-adapter-notion 0.14.0

persona-wire Adapter for Notion (scheme `notion://`).

## Architecture

`NotionAdapter` is a stateless [`Adapter`] impl split into three
independent responsibilities:

- [`parse_notion_uri`] — `WireUri` → `NotionUriSpec` (endpoint kind +
  optional search query/object filter + item limit).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
  Notion-specific knowledge in the transport layer).
- Per-kind loop drivers (`fetch_search` / `drive_data_source_loop` /
  `fetch_page_kind`) — accumulate results across `next_cursor` pages and
  assemble the Wire JSON shape (see "Output shape" below), one per
  endpoint kind.

## URI grammar

```text
notion://search[?query=<text>][&object=page|data_source][&limit=N]
notion://database/<database_id>[?limit=N]
notion://data-source/<data_source_id>[?limit=N]
notion://page/<page_id>[?limit=N]
```

- `host` selects the endpoint kind (`search` / `database` / `data-source`
  / `page`); an empty or invalid value **fails loud** — a typo here would
  otherwise silently return a different class of data (matching
  `persona-wire-adapter-todoist`'s host-selects-kind convention).
- For `kind=search`, the path must be empty (or `/`); `?query=<text>` is
  optional (omitted = search everything, title match only per the Notion
  API) and is percent-decoded once at parse time, the same convention as
  `persona-wire-adapter-todoist`'s `filter`. `?object=page|data_source`
  restricts the search to one object type (Notion's `filter.value` enum
  as of API version 2025-09-03, which renamed the legacy `database` value
  to `data_source`); an invalid value fails loud.
- For `kind=database` / `kind=data-source` / `kind=page`, the path must
  be exactly one segment (the id); a missing id, or any additional path
  segment, fails loud.
- `kind=database` first resolves the database's data sources
  (`GET /databases/{id}`) before querying: exactly one data source
  continues transparently; zero **fails loud** ("no data sources"); two
  or more **fails loud** and lists the `notion://data-source/<id>` URIs
  to pick from explicitly (a database can have multiple typed data
  sources since the 2025-09-03 multi-source-database API change).
- `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
  A non-numeric or zero value fails loud; there is no upper bound at
  parse time. [`MAX_LIMIT`] (Notion's own `page_size` ceiling of 100)
  is enforced only when the adapter builds each upstream request;
  `?limit=N` with `N > MAX_LIMIT` triggers the internal pagination
  loop (see "Pagination" below). `page_size` is always sent explicitly
  to the Notion API (the default behavior for an absent `page_size` is
  undocumented).
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss` / `-github` / `-todoist`); for
  `kind=database` / `-data-source` / `-page`, `query` / `object` are
  themselves unknown query keys (not even read).
- `kind=page` fetches only the page's direct child blocks
  (`GET /blocks/{id}/children`) — nested children (`has_children=true`)
  are **not** recursively fetched, as a context size guard.

## Auth

Resolved per-fetch (not at boot) via
`persona_wire_credentials::Credentials::default_chain().get("notion")`.
Like `persona-wire-adapter-todoist`, Notion has no unauthenticated access
mode — a missing token **fails loud**. Set a token via the
`PERSONA_WIRE_TOKEN_NOTION` or `NOTION_TOKEN` environment variable, or
store one in the OS keychain via `persona-wire token set notion`. The
token is a Notion internal integration secret (`ntn_...` / legacy
`secret_...` prefix, minted on a workspace's Settings → Connections →
Develop or manage integrations page).

**The integration must also be explicitly shared with each page or
database** via that page/database's "•••" menu → "Add connections" —
Notion returns HTTP 404 for otherwise-valid ids the integration has not
been granted access to, which surfaces as a normal fetch failure via
`persona_wire_transport_http::HttpClient`.

The literal `"notion"` service key is overridable per-fetch via the
URI's `?auth=<service_key>` query param (see `persona_wire_core::
infrastructure::adapter`'s "External service integration policy" for the
convention); absent, behavior is unchanged.

Notion enforces an average rate limit of roughly 3 requests per second
per integration; exceeding it returns HTTP 429 with a `Retry-After`
header. This adapter does not implement client-side throttling — a 429
surfaces as a normal fetch failure.

## Output shape

For `kind=search`:

```json
{ "kind": "search", "query": "...|null", "items": [ ... ], "has_more": false }
```

For `kind=database` / `kind=data-source` (both resolve to a data source
query):

```json
{ "kind": "data_source_query", "data_source_id": "...", "items": [ ... ], "has_more": false }
```

`items` entries for both of the above (Notion page objects):

```json
{
  "id": "...|null", "object": "...|null",
  "title": "...|null", "url": "...|null",
  "last_edited_time": "...|null", "in_trash": false
}
```

`title` is extracted by scanning `properties` for the entry whose
`type == "title"` (the property's own name is user-defined and not
fixed, e.g. "Name" / "Title" / anything) and concatenating its rich-text
runs' `plain_text`; `null` when no such property exists or it yields no
text.

For `kind=page`:

```json
{ "kind": "page", "page_id": "...", "blocks": [ ... ], "has_more": false }
```

`blocks` entries:

```json
{ "type": "...|null", "text": "...|null" }
```

`text` is the block type's own rich-text array's `plain_text` runs
concatenated and truncated to [`TEXT_MAX_CHARS`] `char`s; block types
without a rich-text array (e.g. `divider` / `child_page` / `image`) carry
`text: null` alongside their `type`.

## Pagination

`Adapter::fetch` drives the pagination loop internally: it follows the
response body's `next_cursor` field (an opaque token; `has_more: false`
or a `null`/absent `next_cursor` signals end-of-data) across repeated
requests until it has accumulated `?limit=N` items or the upstream
signals end-of-data. The cursor form is a private implementation
detail — the wire layer only sees the final assembled per-kind shape
with a truthful `has_more` field.

Every upstream request is sent with `page_size = min(spec.limit,
MAX_LIMIT)` (Notion's own per-request ceiling of 100), so the loop runs
once for `?limit <= MAX_LIMIT` and continues page-by-page for larger
requests. All four kinds (`search` / `database` / `data-source` /
`page`) paginate the same way; `kind=database` resolves the single data
source id once up front (before the loop starts), then re-uses it for
every page.

