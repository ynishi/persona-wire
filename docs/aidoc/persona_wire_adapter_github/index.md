# persona-wire-adapter-github 0.12.1

persona-wire Adapter for GitHub (scheme `github://`).

## Architecture

`GithubAdapter` is a stateless [`Adapter`] impl split into three
independent functions:

- [`parse_github_uri`] — `WireUri` → `GithubUriSpec` (owner + repo + kind +
  state + limit).
- HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
  GitHub-specific knowledge in the transport layer).
- Internal Link-header loop in [`Adapter::fetch`] — accumulates a raw
  GitHub REST API response (JSON array per page) into the Wire JSON
  shape below, branching only on `kind` for the per-item normalization.

## URI grammar

```text
github://<owner>/<repo>[?kind=issues|pulls|releases][&state=open|closed|all][&limit=N]
```

- `host` is the repo owner (org or user); an empty host fails loud.
- The first path segment is the repo name; a missing repo, or any
  additional path segment beyond it, fails loud.
- `kind` defaults to `issues`. Unlike `?scheme=` in
  `persona-wire-adapter-rss` (which falls back gracefully on an unknown
  value), an invalid `kind` **fails loud**: a typo here silently returns a
  different class of data (pulls instead of issues, say) rather than the
  graceful-fallback shape mismatch RSS's `scheme=` guards against, so the
  safer default is to reject it outright.
- `state` defaults to `open` and only applies to `issues` / `pulls`; for
  `kind=releases` it is silently ignored (not read, not validated) since
  GitHub releases have no `state` field. For `issues` / `pulls`, an
  invalid value (anything other than `open` / `closed` / `all`) fails
  loud.
- `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
  A non-numeric or zero value fails loud. For `kind=issues`, the GitHub
  `per_page` sent upstream is over-fetched (4× `limit`, capped at
  [`GITHUB_PER_PAGE_MAX`]) so that up to `limit` real issues can still be
  returned even when the repo mixes many pull requests into the
  `/issues` endpoint (see "GitHub's `/issues` endpoint mixes..." note
  below). `pulls` and `releases` fetch `per_page = limit` directly, since
  there is no post-fetch filtering for those kinds. This over-fetch has
  no effect when `limit >= 25`, since `4 * 25 = 100` already saturates
  the GitHub API's `per_page` ceiling.
- Unknown query keys are silently ignored (same forward-compatible
  convention as `persona-wire-adapter-rss`).

## Auth

Resolved per-fetch (not at boot) via
`persona_wire_credentials::Credentials::default_chain().get("github")`, so
a token change takes effect without restarting the process and avoids a
keychain prompt on every boot when no token is configured. Set a token via
the `PERSONA_WIRE_TOKEN_GITHUB` or `GITHUB_TOKEN` environment variable, or
store one in the OS keychain via `persona-wire token set github`.

The literal `"github"` service key is overridable per-fetch via the URI's
`?auth=<service_key>` query param (see `persona_wire_core::infrastructure
::adapter`'s "External service integration policy" for the convention);
absent, behavior is unchanged.

When no token resolves, the adapter proceeds unauthenticated — this works
for public repos but is subject to GitHub's unauthenticated rate limit
(60 requests/hour per IP). A backend error while resolving the token
(e.g. keychain access denied) fails loud and propagates; only "no token
configured" is treated as `None`.

## Output shape

```json
{
  "repo": { "owner": "...", "name": "..." },
  "kind": "issues",
  "items": [ ... ],
  "has_more": false
}
```

`has_more` is `true` when the adapter truncated the result at `?limit=N`
and the upstream still had more items available (either the current page
overshot `limit`, or a next-page Link header was returned). It is
`false` when the loop terminated because the upstream ran out of data.

`items` entries for `kind=issues` / `kind=pulls`:

```json
{
  "number": 1, "title": "...|null", "state": "...|null",
  "author": "...|null", "created_at": "...|null", "updated_at": "...|null",
  "url": "...|null", "labels": ["..."], "body_excerpt": "...|null"
}
```

`items` entries for `kind=releases`:

```json
{
  "tag": "...|null", "name": "...|null", "published_at": "...|null",
  "url": "...|null", "prerelease": true, "body_excerpt": "...|null"
}
```

GitHub's `/issues` endpoint mixes pull requests into the response (any
entry that carries a `pull_request` key); `kind=issues` filters those out
before normalizing, so the returned `items` count can be lower than the
requested `limit`.

## Pagination

`Adapter::fetch` drives the pagination loop internally: it follows GitHub's
RFC 5988 `Link` response header (`rel="next"`) across repeated requests
until it has accumulated `?limit=N` items or the upstream signals
end-of-data. The cursor form is a private implementation detail — the wire
layer only sees the final assembled `{repo, kind, items, has_more}` shape.

For `?limit <= GITHUB_PER_PAGE_MAX` (100), a single upstream request is
sufficient in the common case (the loop terminates after one iteration).
For `kind=issues`, the per-page over-fetch heuristic (`per_page = min(4 *
limit, 100)`) still applies to reduce the chance of running the loop when
GitHub's `/issues` endpoint mixes in pull requests that get filtered
post-fetch. If the filtered count still falls short, the loop follows the
`Link` header to the next page.

