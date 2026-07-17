# persona-wire-adapter-mastodon 0.14.0

persona-wire Adapter for Mastodon (scheme `mastodon://`).

## Architecture

`MastodonAdapter` is a stateless [`Adapter`] impl split into three
independent functions, mirroring `persona-wire-adapter-github`:

- [`parse_mastodon_uri`] — `WireUri` → [`MastodonUriSpec`] (instance +
  timeline kind + `limit` / `local` / `only_media`).
- [`resolve_service_key`] — `WireUri` → credentials service key (the
  `?auth=` override, or the crate-wide default).
- [`mastodon_http_client`] — service key + timeline kind → a
  Bearer-authenticated (or, for `timelines/public` only,
  unauthenticated-fallback) [`HttpClient`].

`Adapter::fetch` wires the three together, then fetches Mastodon's REST
API v1 response as-is and wraps it in the Wire JSON shape below — Phase 1
does not re-shape individual status objects (see "Output shape").

## URI grammar

```text
mastodon://<instance>/timelines/home?limit=N&auth=<key>
mastodon://<instance>/timelines/public?local=true&only_media=false&limit=N&auth=<key>
```

- `<instance>` is the URI host (e.g. `mastodon.social` / `fosstodon.org`);
  an empty host fails loud. The upstream base URL is `https://<instance>`.
- The path must be exactly `timelines/home` or `timelines/public`; any
  other path fails loud.
- `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
  A non-numeric or zero value fails loud. A value above
  [`MASTODON_LIMIT_MAX`] (Mastodon's own per-request ceiling) is clamped
  down to it, with a `tracing::warn!` (not a hard failure — unlike
  `?limit=0`, an oversized limit is a harmless, silently-correctable
  input).
- `local` / `only_media` apply to `timelines/public` only (default
  `false` for both); `true` / `false` are accepted, any other value fails
  loud. For `timelines/home`, both are silently ignored (not read, not
  validated) — Mastodon's home-timeline endpoint has no such filter, same
  convention as `persona-wire-adapter-github`'s `state` being ignored for
  `kind=releases`.
- `?auth=<service_key>` selects which credentials-provider service key to
  resolve the Bearer token from, letting one caller manage tokens for
  multiple instances (e.g. `?auth=work` resolves the `work` service key
  instead of the crate-wide default, [`DEFAULT_SERVICE_KEY`]). Absent or
  empty `?auth=` uses the default. This key never reaches the upstream
  request — it is consumed by [`resolve_service_key`] and does not appear
  in [`MastodonUriSpec::endpoint_url`]'s query string.
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-github` / `-rss`).

## Auth

Resolved per-fetch (not at boot) via
`persona_wire_credentials::Credentials::default_chain().get(service_key)`,
so a token change takes effect without restarting the process. Set a
token via the `PERSONA_WIRE_TOKEN_<SERVICE_KEY>` environment variable (or,
for the default service key, the `MASTODON_ACCESS_TOKEN` alias), or store
one in the OS keychain via `persona-wire token set mastodon`.

Unlike `persona-wire-adapter-github` (unauthenticated fallback for every
request — GitHub's public-repo read endpoints tolerate it), auth
resolution here is **asymmetric per timeline**:

- `timelines/home` always requires a resolved token (Mastodon has no
  unauthenticated "home" concept — it is inherently the caller's own
  feed) — a missing token **fails loud**.
- `timelines/public` (instance-local or federated public feed) works
  unauthenticated — a missing token falls back gracefully (logged via
  `tracing::info!`, not an error).

A backend error while resolving the token (e.g. keychain access denied)
always fails loud and propagates, for both timelines — only "no token
configured" is treated as `None`.

## Output shape

```json
{
  "instance": "mastodon.social",
  "kind": "timelines/home",
  "items": [ /* raw Mastodon Status objects, unmodified */ ]
}
```

`items` is the upstream Mastodon REST API v1 response array passed
through unmodified (Phase 1 does not extract or rename individual
`Status` fields, unlike `persona-wire-adapter-github`'s per-item
normalization) — see the module-level "Phase 1 scope" note below. A
non-array upstream response fails loud, naming the instance and kind.

## Phase 1 scope

Only the two read-only timeline endpoints above are implemented:
`GET /api/v1/timelines/home` and `GET /api/v1/timelines/public`. No
posting, no notifications, no search, no account lookup, no pagination
beyond a single `?limit=N` page (Mastodon's `Link`-header pagination,
mirroring `persona-wire-adapter-github`'s multi-page loop, is a future
extension once a caller needs more than one page's worth of items).

## vs. `activitypub://`

`persona-wire-adapter-activitypub` reads the **public, unauthenticated**
surface of the ActivityPub protocol (an actor's outbox / profile
document) and works against any compliant Fediverse server (Mastodon,
Misskey, Pleroma, ...) via the generic ActivityStreams shape. This crate
is **Mastodon-native**: it calls Mastodon's own REST API v1
(`/api/v1/timelines/...`), which is Mastodon-specific (not a generic
ActivityPub concept) and, for `timelines/home`, requires the caller's own
Bearer token — a capability the generic ActivityPub outbox model has no
equivalent for. Use `activitypub://` for cross-instance public reads of a
specific account; use `mastodon://` for a caller's own home feed or an
instance's local/public timeline.

