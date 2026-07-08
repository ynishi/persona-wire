# persona-wire-adapter-slack 0.12.0

persona-wire Adapter for Slack (scheme `slack://`).

## Architecture

`SlackAdapter` is a stateless [`Adapter`] impl split into three
independent functions:

- [`parse_slack_uri`] — `WireUri` → `SlackUriSpec` (endpoint kind +
  kind-specific filters + item limit).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
  Slack-specific knowledge in the transport layer).
- Per-kind loop drivers (`drive_channels_loop` / `drive_history_loop`)
  plus the single-shot [`normalize_user`] — accumulate results across
  `response_metadata.next_cursor` pages for paginatable kinds, and
  assemble the Wire JSON shape below, one per endpoint kind.

## URI grammar

```text
slack://channels[?types=public_channel|private_channel|mpim|im][&limit=N][&exclude_archived=true|false]
slack://history/<channel_id>[?limit=N][&oldest=<ts>][&latest=<ts>]
slack://user/<user_id>
```

- `host` selects the endpoint kind (`channels` / `history` / `user`); an
  empty or invalid value **fails loud** — a typo here would otherwise
  silently return a different class of data (matching
  `persona-wire-adapter-todoist`'s host-selects-kind convention).
- For `kind=channels`, the path must be empty (or `/`); any additional
  path segment fails loud. `?types=` accepts a comma-separated subset of
  `public_channel` / `private_channel` / `mpim` / `im` (Slack's
  `conversations.list` `types` enum) — an unrecognized entry fails loud;
  omitted defaults to `public_channel`. `?exclude_archived=true|false`
  defaults to `true`; any other value fails loud.
- For `kind=history`, the path must be exactly one segment (the
  `channel_id`); a missing id, or any additional path segment, fails
  loud (the id's own format is not further validated — a malformed id
  surfaces as a normal Slack API error, e.g. `channel_not_found`).
  `?oldest=<ts>` / `?latest=<ts>` (Slack Unix-seconds-with-microseconds
  timestamps, e.g. `1512085950.000216`) are percent-decoded once at parse
  time and passed through to the Slack API verbatim — this adapter does
  not validate their format.
- For `kind=user`, the path must be exactly one segment (the `user_id`);
  same missing/extra-segment rule as `kind=history`.
- `types` / `exclude_archived` are unknown query keys for `kind=history`
  / `kind=user` (not even read); `oldest` / `latest` are unknown query
  keys for `kind=channels` / `kind=user` (not even read) (module docs
  "URI grammar").
- `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
  A non-numeric or zero value fails loud; there is no upper bound at
  parse time. [`MAX_LIMIT`] (Slack's own `conversations.list` /
  `conversations.history` per-request ceiling of 999) is enforced only
  when the adapter builds each upstream request; `?limit=N` with
  `N > MAX_LIMIT` triggers the internal pagination loop (see
  "Pagination" below).
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss` / `-github` / `-todoist` /
  `-notion`).

## Auth

Resolved per-fetch (not at boot) via
`persona_wire_credentials::Credentials::default_chain().get("slack")`.
Slack has no unauthenticated access mode — a missing token **fails
loud**. Set a token via the `PERSONA_WIRE_TOKEN_SLACK` or
`SLACK_BOT_TOKEN` environment variable, or store one in the OS keychain
via `persona-wire token set slack`. The token is a Slack bot token
(`xoxb-...` prefix), minted under a Slack app's OAuth & Permissions page.
It is sent as an `Authorization: Bearer` header (per `HttpClient`) —
**never** as a query-string parameter, per Slack's own guidance for apps
created since November 2020.

The minimal OAuth scopes this adapter needs are `channels:read`,
`channels:history`, and `users:read`. **A private channel must have the
bot explicitly invited** (`/invite @<bot>` in that channel) — a bot token
without that invite gets a `not_in_channel` API error on
`conversations.history`, which surfaces as a normal fetch failure (see
"Error handling" below).

Slack's HTTP response is always `200 OK`; success/failure is signalled by
the response body's `{"ok": true|false}` field (see "Error handling"
below) — a `429` status is the sole HTTP-level exception, carrying a
`Retry-After` header, which this adapter does not implement
client-side throttling for (a `429` surfaces as a normal fetch failure
via `persona_wire_transport_http::HttpClient`). Slack's tiered rate
limits (Tier 2, `conversations.list` / `users.info`; Tier 3,
`conversations.history`) apply to public Slack Marketplace apps as of the
2025-05 update; an **internal, customer-built app** (a bot token used
only within its own workspace, never distributed) is explicitly exempted
from that update per Slack's own clarification
(<https://docs.slack.dev/ja-jp/2025-05-terms-rate-limit-update-and-faq/>),
so `conversations.history`'s Tier 3 ceiling (50+ requests/minute) remains
in effect for the internal-app usage this adapter targets.

### Error handling

Every Slack Web API response is HTTP `200 OK` with a JSON body carrying
`{"ok": bool, ...}`. This adapter inspects `ok` after every fetch: `ok:
true` proceeds to normalization; `ok: false` **fails loud** with the
response's `error` code (and, when present, its `needed` /  `provided`
scope hint) folded into the error message — e.g. `not_in_channel` (bot
not invited to a private channel) or `missing_scope` (token lacks a
required OAuth scope).

## Output shape

For `kind=channels`:

```json
{ "kind": "channels", "items": [ ... ], "has_more": false }
```

`items` entries:

```json
{
  "id": "...|null", "name": "...|null",
  "is_private": false, "is_archived": false, "is_member": true,
  "num_members": 4, "topic": "...|null", "purpose": "...|null"
}
```

`topic` / `purpose` are each the corresponding object's `value` field
(Slack's own `topic` / `purpose` are `{value, creator, last_set}`
objects; only `value` is surfaced here). `has_more` is `true` when
`response_metadata.next_cursor` is present and non-empty.

For `kind=history`:

```json
{ "kind": "history", "channel_id": "...", "items": [ ... ], "has_more": false }
```

`items` entries:

```json
{
  "type": "...|null", "user": "...|null", "text": "...|null",
  "ts": "...|null", "thread_ts": "...|null",
  "is_thread_parent": false, "reply_count": null, "subtype": "...|null"
}
```

`user` is the Slack user *id* (not a resolved display name — resolving it
is the caller's responsibility, e.g. via `slack://user/<id>`). `ts` /
`thread_ts` are passed through verbatim as strings (Slack's own
Unix-seconds-with-microseconds form, e.g. `1512085950.000216`; parsing as
a float would lose the sub-second precision that makes each `ts` unique
within a channel). `is_thread_parent` is `true` when `thread_ts == ts`
(a message starting its own thread); `false` for a thread reply
(`thread_ts` present and `!= ts`) or a non-threaded message (`thread_ts`
absent). `text` is truncated to [`TEXT_MAX_CHARS`] `char`s (context size
guard). `has_more` is Slack's own top-level `has_more` field.

For `kind=user`:

```json
{
  "kind": "user", "id": "...|null", "name": "...|null",
  "real_name": "...|null", "display_name": "...|null",
  "email": "...|null", "is_bot": false, "deleted": false
}
```

`display_name` is `profile.display_name`; `email` is `profile.email`
(`null` when the token's scopes do not include the email-visibility
scope, or the user has none set — Slack omits the field rather than
sending an explicit `null` in that case, and the missing-field path
yields the same `null` here).

## Pagination

`Adapter::fetch` drives the pagination loop internally for
`kind=channels` and `kind=history`: it follows the response body's
`response_metadata.next_cursor` field (an opaque token; an empty string,
`null`, or absent field all signal end-of-data) across repeated requests
until it has accumulated `?limit=N` items or the upstream signals
end-of-data. The cursor form is a private implementation detail — the
wire layer only sees the final assembled per-kind shape with a truthful
`has_more` field.

`kind=user` is a single-object fetch (`users.info`), not paginated;
`?limit=N` is silently ignored for that kind.

Every upstream request is sent with `limit = min(spec.limit, MAX_LIMIT)`
(Slack's own per-request ceiling of 999), so the loop runs once for
`?limit <= MAX_LIMIT` and continues page-by-page for larger requests.

