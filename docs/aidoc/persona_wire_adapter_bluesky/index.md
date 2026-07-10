# persona-wire-adapter-bluesky 0.12.1

persona-wire Adapter for Bluesky (scheme `bluesky://`).

## Architecture

`BlueskyAdapter` is a stateless [`Adapter`] impl split into independent
functions, mirroring `persona-wire-adapter-activitypub` /
`persona-wire-adapter-rss`:

- [`parse_bluesky_uri`] — `WireUri` → `BlueskyUriSpec` (actor + output
  kind + item limit + optional post rkey).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient`
  ([`fetch_author_feed`] / [`fetch_profile`] / [`fetch_post_thread`]),
  each hitting a single unauthenticated AT Protocol XRPC endpoint on the
  public AppView.
- [`normalize_feed`] / [`normalize_profile`] / [`normalize_thread`] — raw
  `app.bsky.*` lexicon JSON → the Wire JSON shapes below.

Only the **public**, unauthenticated AppView surface is exercised: an
actor's author feed, profile, and a single post's thread. Authenticated
endpoints (home timeline / DMs / follow / write / OAuth) are out of MVP
scope.

## AT Protocol glossary

- **DID** (`did:plc:...`) — an actor's permanent, non-human-readable
  identifier; never changes even if the handle does.
- **Handle** (`alice.bsky.social`) — an actor's human-readable,
  DNS-backed name; resolves to a DID.
- **rkey** — the trailing path segment of a record's URI (e.g. the
  `3jzfc...` in `at://did:plc:xxx/app.bsky.feed.post/3jzfc...`), a
  per-collection-per-repo unique key.
- **at URI** — `at://<did-or-handle>/<collection>/<rkey>`, the
  canonical address of a single record inside a repo.
- **XRPC** — AT Protocol's HTTP RPC convention: every endpoint is
  `GET/POST /xrpc/<lexicon-id>` (e.g. `app.bsky.feed.getAuthorFeed`).
- **AppView** — a service (here, the public `public.api.bsky.app`
  instance) that aggregates raw repo records into query-friendly views
  (feeds, profiles, threads) without requiring auth.

## URI grammar

```text
bluesky://<actor>[?kind=feed|profile|thread][?limit=N][?post=<rkey>]
```

- `actor` (the URI host) is a Bluesky handle (e.g. `bsky.app`) or DID
  (`did:plc:xxx`); an empty host is an error.
- `?kind=` selects the output shape (default `feed`):
  - `feed` = `getAuthorFeed` — the actor's most recent posts (including
    reposts and replies).
  - `profile` = `getProfile` — actor metadata (display name / bio /
    follower counts).
  - `thread` = `getPostThread` — a single post plus its direct replies;
    requires `?post=<rkey>`.
- `?limit=N` caps the number of feed entries returned (default
  [`DEFAULT_LIMIT`], `feed` only; parsed and range-checked for every
  `kind`, matching `persona-wire-adapter-activitypub`'s convention). A
  non-numeric, zero, or over-[`MAX_LIMIT`] value fails loud.
- `?post=<rkey>` supplies the target post's rkey for `?kind=thread`
  (required — missing it is an error); silently ignored for any other
  `kind`.
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss` /
  `persona-wire-adapter-activitypub`).

## Output shape

`?kind=feed` (default):
```json
{
  "kind": "feed",
  "actor": "<handle or did>",
  "posts": [
    { "uri": "...", "cid": "...",
      "author": { "handle": "...", "did": "...", "displayName": "...|null" },
      "text": "...", "created_at": "<RFC3339>|null",
      "reply_count": 0, "repost_count": 0, "like_count": 0,
      "is_repost": false, "is_reply": false }
  ]
}
```

`?kind=profile`:
```json
{
  "kind": "profile",
  "actor": { "handle": "...", "did": "...", "displayName": "...|null",
    "description": "...|null", "followers_count": 0, "follows_count": 0,
    "posts_count": 0 }
}
```

`?kind=thread`:
```json
{
  "kind": "thread",
  "post": { "<same shape as a feed entry>" },
  "replies": [ { "<same shape as a feed entry>" } ]
}
```

`replies` is one level deep only — nested replies-of-replies are dropped,
not flattened. Missing numeric fields default to `0` (matching the
Bluesky API's own behavior); missing string fields are `null`.

