# persona-wire-adapter-matrix 0.14.0

persona-wire Adapter for Matrix (scheme `matrix://`).

## Architecture

`MatrixAdapter` is a stateless [`Adapter`] impl split into three
independent pieces, matching the repo-wide adapter convention (see
`persona_wire_core::infrastructure::adapter` crate docs):

- [`parse_matrix_uri`] — `WireUri` → [`MatrixRequest`] (homeserver + kind
  dispatch: `sync` or `rooms/<room_id>/messages`).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
  Matrix-specific knowledge in the transport layer).
- [`Adapter::fetch`] — builds the upstream request URL per kind, sends it,
  and wraps the raw Matrix Client-Server API v3 JSON response in a small
  `{scheme, kind, homeserver, ..., body}` envelope (see "Output shape").

Phase 1 covers exactly two Matrix Client-Server API v3 endpoints:
`GET /_matrix/client/v3/sync` and `GET /_matrix/client/v3/rooms/{room_id}/messages`.
Sending messages, room membership management, and end-to-end encryption
are out of scope for this Phase.

## URI grammar

```text
matrix://<homeserver>/sync[?limit=N][&auth=<key>]
matrix://<homeserver>/rooms/<room_id>/messages[?limit=N][&dir=b|f][&auth=<key>]
```

- `host` is the Matrix homeserver (e.g. `matrix.org`); an empty host
  fails loud. The upstream request URL is always `https://<homeserver>`.
- The first path segment selects the endpoint: `sync` (no further
  segments) or `rooms/<room_id>/messages` (exactly three segments). Any
  other path **fails loud** — matching `persona-wire-adapter-github`'s
  `?kind=` convention, an unrecognized shape here would otherwise mean a
  silently wrong request rather than a clear rejection.
- `<room_id>` is a Matrix room ID (`!abc:matrix.org`) or room alias
  (`#room:matrix.org`), percent-encoded in the URI path segment; the
  adapter percent-decodes it before embedding it in the upstream request
  URL (re-encoded exactly once there via `url::Url::path_segments_mut`,
  the same "decode once, encode once" convention as
  `persona-wire-adapter-todoist`'s `filter` query value).
- `limit` caps the number of events returned by either endpoint (default
  [`DEFAULT_LIMIT`]). A non-numeric or zero value fails loud.
- `dir` only applies to `rooms/<room_id>/messages` (pagination direction);
  default `"b"` (backwards). `"f"` is also accepted; any other value
  fails loud. It is not read at all for `sync` (unapplicable — same
  "silently ignored for the kind it doesn't apply to" convention as
  `persona-wire-adapter-github`'s `state` for `kind=releases`).
- `auth` selects the credential `service_key` (see "Auth"); it is
  captured by [`resolve_service_key`] before endpoint dispatch and never
  forwarded to the upstream Matrix request URL.
- All other query keys are silently ignored (same forward-compatible
  convention as every other adapter in this workspace).

## Auth

Every Matrix Client-Server API v3 endpoint covered by Phase 1 is called
with `Authorization: Bearer <access_token>` — unlike
`persona-wire-adapter-github`, this adapter has **no unauthenticated
fallback**; a missing token fails loud (matching
`persona-wire-adapter-todoist`'s auth-required convention).

The credential `service_key` defaults to [`DEFAULT_SERVICE_KEY`]
(`"matrix"`), resolved per-fetch (not at boot) via
`persona_wire_credentials::Credentials::default_chain().get(service_key)`.
Store an access token via `persona-wire token set matrix`, or set the
`PERSONA_WIRE_TOKEN_MATRIX` environment variable.

Multi-homeserver setups (distinct accounts per homeserver) override the
service key per-URI via `?auth=<service_key>`, e.g.
`matrix://work.example.org/sync?auth=matrix-work` resolves the token
under `PERSONA_WIRE_TOKEN_MATRIX_WORK` / `persona-wire token set
matrix-work` instead of the default `matrix` key.

## Output shape

```json
{
  "scheme": "matrix",
  "kind": "sync",
  "homeserver": "matrix.org",
  "body": { /* raw /_matrix/client/v3/sync response */ }
}
```

```json
{
  "scheme": "matrix",
  "kind": "rooms_messages",
  "homeserver": "matrix.org",
  "room_id": "!abc:matrix.org",
  "body": { /* raw /_matrix/client/v3/rooms/{room_id}/messages response */ }
}
```

`body` is the upstream Matrix JSON response verbatim (no per-field
normalization in Phase 1 — the raw `sync` / `messages` response shapes
are Matrix spec, not this crate's concern to re-flatten).

