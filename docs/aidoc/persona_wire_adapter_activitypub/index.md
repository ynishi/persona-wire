# persona-wire-adapter-activitypub 0.14.0

persona-wire Adapter for the Fediverse (scheme `activitypub://`).

## Architecture

`ActivityPubAdapter` is a stateless [`Adapter`] impl split into
independent functions, mirroring `persona-wire-adapter-rss`:

- [`parse_activitypub_uri`] — `WireUri` → `ActivityPubUriSpec` (instance +
  user + output kind + item limit).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient`
  ([`fetch_actor`] for the actor document, [`fetch_outbox`] for the
  2-page outbox fetch), with `Accept: application/activity+json` on
  every request.
- [`normalize_actor`] / [`normalize_posts`] — raw ActivityStreams JSON →
  the Wire JSON shapes below.

Only the **public**, unauthenticated surface of the ActivityPub protocol
is exercised: an actor's outbox and profile document. `follow` / write /
DM / private post / any authenticated action are out of MVP scope.

Fediverse compat: any instance implementing the ActivityPub actor +
`OrderedCollection` outbox conventions (Mastodon, Misskey, Pleroma,
Firefish, and others) — no server-specific branching, only defensive
`serde_json::Value` field access since not every implementation matches
the spec byte-for-byte.

## URI grammar

```text
activitypub://<instance>/@<user>[?kind=profile|outbox][?limit=N]
```

- `instance` (the URI host) is the Fediverse instance hostname (e.g.
  `mastodon.social`); an empty host is an error.
- The path must be `/@<user>` (Mastodon handle convention); a missing
  `@` prefix is an error. Internally this resolves to the canonical
  ActivityPub actor URL `https://<instance>/users/<user>`.
- `?kind=` selects the output shape (default `outbox`): `outbox` (the
  actor's most recent public posts) or `profile` (actor metadata).
- `?limit=N` caps the number of posts returned (default
  [`DEFAULT_LIMIT`], `outbox` only). A non-numeric or zero value fails
  loud.
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss`).

## Output shape

`?kind=outbox` (default):
```json
{
  "kind": "outbox",
  "actor": { "url": "...", "handle": "@user@instance" },
  "posts": [
    { "id": "...", "content": "...|null", "published": "<RFC3339>|null",
      "url": "...|null", "attachments": [{ "type": "...", "url": "..." }] }
  ]
}
```

`?kind=profile`:
```json
{
  "kind": "profile",
  "actor": { "url": "...", "handle": "...", "name": "...|null",
    "summary": "...|null", "followers_url": "...|null",
    "following_url": "...|null" }
}
```

HTML `content` / `summary` are passed through undecoded and unsanitized
(the caller's responsibility); no length truncation is applied (Mastodon
caps posts at 500 chars, Misskey at 3000 — truncating here would
silently disagree with the source instance).

