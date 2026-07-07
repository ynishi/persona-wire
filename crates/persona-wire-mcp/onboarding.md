# Onboarding — Wiring a new persona end-to-end

This guide walks through the full workflow for adding a new persona to
persona-wire, from a fresh install to a working `wire_prompt_context` call
and a sample Skill / Prompt that consumes it.

It is also exposed through the MCP server as a resource at
`wire-guide://onboarding`, so a client (Claude Code Skill, CLI agent, etc.)
can fetch it with `read_resource` without leaving the session.

## 0. Mental model in one paragraph

### Where persona-wire sits

persona-wire is a **routing layer** between two neighbours:

- **persona-pack** — SoT for persona identity / prompt body / per-persona
  overlay (`[extra.persona_wire.projections.<axis>]`). persona-wire reads
  it; it does not write back.
- **per-axis SoT** — mini-app tables (`mailbox`, `brief`, …) or files
  under `~/my-personas/<persona>/…`. persona-wire never owns these rows;
  it points at them via `metadata.source_uri` and fetches fresh through
  Layer 6 Adapters at render time.

The graph itself is a small SQLite store of **wiring entries + Specs +
NamedProjections** — no persona content lives inside.

### Concept

The graph holds **wiring entries** — one `Node` per **Slot** you want to
expose for the persona. A Slot is the persona-context binding identifier
(e.g. `active` / `handoff` / `mailbox`); it is a first-class Value Object
on `domain::entity::wiring::Wiring`. The narrative below uses the word
*Slot* for the concept, but the storage metadata key and the persona-pack
overlay key are still literally `axis` (legacy SQLite rows + persona-pack
TOML keys); `application::wiring_mapper` is the single translation
boundary (`Slot ↔ Node.metadata["axis"]`). New code should route through
the mapper rather than reading `metadata["axis"]` directly.

Each wiring entry carries a `metadata.source_uri` that points at the real
Source-of-Truth (mini-app table, file, …). A **Specification** picks the
wiring entries; a **NamedProjection** binds the Specification to a
handlebars template and a target form. `wire_prompt_context` fresh-fetches
each wiring entry's `source_uri` through the Layer 6 Adapter and renders
the template; `wire_render` / `wire_init` render the same template against
graph state only (no adapter fetch — see §3 for the two render-context
shapes).
`wire_prompt_context` walks every Slot for the persona, optionally
filtered by `projection_names` (include subset) and / or
`projection_exclude_names` (exclude subset), and concatenates the
rendered blocks into a single PromptContext string. The two filter
arguments compose as AND NOT (`include \ exclude`): exclude wins on
intersection, unknown names are silently ignored. Optional per-persona
overlays live in persona-pack under
`[extra.persona_wire.projections.<axis>]` (literal key) and are folded
into the base template through a `MergeStrategy`.

## 1. Install + run

```sh
# Build / install the unified binary (CLI + `mcp` subcommand)
cargo install --path crates/persona-wire

# Inspect / mutate the graph from the shell
persona-wire init
persona-wire wire-doctor

# Start the MCP server (stdio transport)
persona-wire mcp
```

The default SQLite database lives at `~/.persona-wire/store.db` (override
with `--db <path>` or the `PERSONA_WIRE_DB` env var). The MCP server
exposes the `wire_*` tools listed in the server `instructions` and the
guide resource at `wire-guide://onboarding`.

## 2. Set up a persona's wiring entries (one Node per Slot)

Pick the Slots you want (`active` / `ng` / `trigger` / `handoff` / `toolmap`
is one common shape, but anything works). The Slot name lives on the
node's `metadata.axis` (legacy literal key). For each Slot create one node
with a `source_uri`. Add an edge from the persona node for traceability
(optional but recommended — wiring entries that carry `metadata.source_uri`
or `metadata.maintenance_exempt: true` are recognised as **self-attached**
and are excluded from the `wire_doctor` / `wire_close` orphan count, so an
edge-less wiring entry will not be flagged as a graph-health issue).

**Identity model (v0.7+)**: `wire_node_create` / `wire_edge_create` /
`wire_spec_register` / `wire_projection_register` accept a human-readable
`name` only; the server mints the opaque ULID `id` and returns it in
the response (`{"id": "<26-char ULID>", "name": "..."}`). All subsequent
operations (`wire_node_update` / `wire_node_delete` / `wire_edge_delete`
/ `wire_edge_create.src` / `.tgt` / `wire_spec_delete` /
`wire_projection_delete` / `wire_render.projection_ref` /
`wire_query.spec_ref`) accept either the ULID **or** the `name` and
resolve internally. Nodes and edges allow duplicate `name`s — the
resolver returns `AmbiguousName` and forces ULID disambiguation when
that happens. Specifications and projections enforce `UNIQUE(name)` at
the storage layer (registry semantics), so name lookups there are
always single-row.

```jsonc
// MCP: wire_node_create  (server mints id, returns {id, name})
{ "name": "alpha",                "type": "persona",
  "metadata": { "display": "alpha" } }

{ "name": "alpha.active",         "type": "outline_node",
  "metadata": { "persona": "alpha", "axis": "active",
                "source_uri": "mini-app://alpha_active_context" } }

{ "name": "alpha.handoff",        "type": "outline_node",
  "metadata": { "persona": "alpha", "axis": "handoff",
                "source_uri": "file:~/path/to/alpha/handoff/" } }

// MCP: wire_edge_create  (src/tgt accept ULID or name)
{ "name": "e.alpha.active",  "src": "alpha", "tgt": "alpha.active",  "kind": "routes_to" }
{ "name": "e.alpha.handoff", "src": "alpha", "tgt": "alpha.handoff", "kind": "routes_to" }
```

`source_uri` schemes supported today:

- `mini-app://<table_name>[?scope=<scope>&root=<dir>&limit=<n>]` — opens
  the table via the `mini-app-core` SDK and lists all rows. Default
  (no `?scope=`) reads `~/.mini-app/<table>/<table>.db`
  (`MINI_APP_USER_DIR` overrides the base directory). The optional
  reserved keys redirect the list-all path to a hard target:
  - `?scope=user` — User-scope `<base>/<table>/<table>.db` (no fallback;
    `<base>` = `$MINI_APP_USER_DIR` or `~/.mini-app/`, or `?root=<dir>`).
  - `?scope=<project-name>&root=<dir>` — Project-scope
    `<dir>/<table>/<table>.db`. `root` is **required** when
    `scope=<project-name>` is set (parse fails fast otherwise). Use this
    to wire a project-scoped mini-app table that lives outside
    `~/.mini-app/` — e.g. `mini-app://<table>?scope=<project>&root=<path-to-mini-app>`.
  - `?limit=<n>` — caps the row count (default `1000`). See §2c for
    tuning rules of thumb (mailbox-style axes ≈ `10`, latest-snapshot
    axes ≈ `1`).
- `mini-app://<table>?alias=<name>[&<k>=<v>]*[&limit=<n>][&scope=<scope>][&root=<dir>]`
  — alias path (see §2b). Resolves through a pre-registered mini-app
  `_aliases` entry so filter / sort / limit live on the mini-app side.
- `file://<path>` or `file:<path>` — `std::fs::read`. `~/` is expanded.
  Directory paths return the newest mtime child (handy for
  `handoff/YYYY-MM-DD.md` patterns).
- `obsidian:///<vault-root>/<note>[?frontmatter={on|off}&links={off|edge}]` —
  reads a Markdown note from a local Obsidian vault directory via `tokio::fs`,
  parses YAML / TOML frontmatter via `gray_matter`, and optionally extracts
  `[[wiki-link]]` references when `?links=edge` is set (default `off`).
  Returns `{ vault_path, note_path, frontmatter, body, wiki_links? }`.
  Example: `obsidian:////Users/me/vault/daily.md?frontmatter=on&links=edge`.
- `rss://<host>/<path>[?scheme=http][?limit=N]` — fetches an RSS 2.0 /
  RSS 1.0 / Atom / JSON Feed document (format auto-detected by
  `feed_rs`) and returns
  `{ feed: { title, url }, items: [{ title, link, published, summary }] }`.
  `?scheme=http` downgrades to plain HTTP; default `https`. `?limit=N`
  caps the item count (default 20). No credentials required.
- `github://<owner>/<repo>[?kind=issues|pulls|releases][&state=open|closed|all][&limit=N]` —
  fetches issues / pull requests / releases via the GitHub REST API.
  Default `kind=issues`; the issues endpoint returns PRs too, so the
  adapter filters out entries with a `pull_request` key. Optional
  bearer auth via §2d — public repos work unauthenticated (60 req/h
  rate limit), authenticated requests get 5000 req/h.
- `todoist://tasks[?project_id=..][?filter=..][&limit=N]` /
  `todoist://projects[?limit=N]` — fetches active tasks or projects
  via the Todoist unified API v1 (the retired REST v2 endpoints are
  not used). `filter` accepts a natural-language query
  (e.g. `today | overdue`) and is exclusive with `project_id`. Bearer
  auth is **mandatory**; missing tokens fail loud with the setup
  instructions from §2d. `limit` is capped at the API maximum of 200.
- `notion://search[?query=..][&object=page|data_source][&limit=N]` /
  `notion://database/<database_id>[?limit=N]` /
  `notion://data-source/<data_source_id>[?limit=N]` /
  `notion://page/<page_id>[?limit=N]` — fetches search results, data
  source query results, or the top-level children of a page via the
  Notion API v1 pinned at `Notion-Version: 2026-03-11`. The
  `database` kind resolves through `GET /databases/{id}` and requires
  exactly one data source (zero or multiple fail loud with the
  available data-source IDs listed). Page titles are extracted by
  scanning `properties` for the entry whose type is `"title"` (the
  property name is user-defined and never assumed). Bearer auth is
  mandatory and the integration must be shared with the target
  page / database via *Add connections* in Notion. `limit` is capped
  at 100.
- `slack://channels[?types=..][&limit=N][&exclude_archived=true|false]` /
  `slack://history/<channel_id>[?limit=N][&oldest=<ts>][&latest=<ts>]` /
  `slack://user/<user_id>` — fetches conversation lists, channel
  history, or user info via the Slack Web API. Bearer auth (a bot
  token, `xoxb-...`) is mandatory and sent via the Authorization
  header only (per Slack's no-tokens-in-querystrings rule for apps
  created 2020-11 or later). API errors (`ok: false` with a 200
  status) are lifted into fail loud `WireError::Storage` with the
  error code embedded (and `needed` / `provided` for `missing_scope`).
  Message `ts` is kept as a string to avoid float precision loss;
  thread parents are detected via `thread_ts == ts`. Self-hosted
  internal apps stay on the pre-2025 Tier 3 for `conversations.history`
  (50+ req/min) per Slack's 2025-06 clarification. `limit` is capped
  at 999.

Bulk-insert through `wire_nodes_create_batch` / `wire_edges_create_batch`
when you have many axes at once.

## 2d. Adapter credentials — token setup

Adapters that hit remote APIs (`github://` / `todoist://` / `notion://` /
`slack://`) resolve their bearer token through `persona-wire-credentials`
on **every fetch** — no daemon restart is required after a token
change, and there is no boot-time keychain prompt.

### Resolution order (per fetch)

For each `<service>` (`github` / `todoist` / `notion` / `slack`) the
provider chain is checked in this order and the first `Some(token)` wins:

1. Environment variable `PERSONA_WIRE_TOKEN_<SERVICE>` (uppercase; e.g.
   `PERSONA_WIRE_TOKEN_GITHUB`)
2. Conventional environment variable alias:
   - `github` → `GITHUB_TOKEN`
   - `todoist` → `TODOIST_API_TOKEN`
   - `notion` → `NOTION_TOKEN`
   - `slack` → `SLACK_BOT_TOKEN`
3. OS keyring (`macOS Keychain` / `Windows Credential Manager` /
   Linux Secret Service), service = `persona-wire`, account = the
   service name

Empty env vars are treated as absent (they fall through to the next
provider). Backend errors from the OS keyring **fail loud** — the
adapter does not silently fall through to the next provider on failure
(the exception is `NoEntry`, which is a normal miss).

### macOS Keychain access prompts

On macOS, `token set` and `token get`-through-adapter still trigger a
Keychain access dialog (the secret value is actually read). `token
status` was updated to check for entry existence without extracting
the secret, so it now surfaces at most one dialog ("キーへのアクセス"),
not two. Clicking "常に許可" on the first prompt for a given binary
grants the ACL for subsequent runs — until `cargo install` replaces
the binary and its codesign hash changes, at which point the prompt
returns.

`github://` falls back to unauthenticated requests when no token is
found (public repos work). The other three fail loud with the setup
instructions if no token is available.

### CLI — `persona-wire token`

```sh
# Register a token.
# TTY: paste at the prompt — input is masked (no-echo) like `passwd`.
# Token values are never printed back.
persona-wire token set github
#   Token for github: ****...   (input hidden)

# Pipe: stdin one-liner (shell history hazard — prefer the TTY path for
# interactive use).
echo "<paste-token-here>" | persona-wire token set github

# Show which provider supplies each service (env / keyring / none).
# Never prints token values.
persona-wire token status
#   github:   env         (PERSONA_WIRE_TOKEN_GITHUB or GITHUB_TOKEN is set)
#   todoist:  keyring
#   notion:   none
#   slack:    none

# Remove a token from the keyring (idempotent — a missing entry is a no-op).
persona-wire token rm slack
```

`token set` reads from a masked TTY prompt or piped stdin; the token is
never taken as a command-line argument, so it does not appear in shell
history or `ps`. `token` is a CLI-only surface — it is intentionally not
exposed as an MCP tool, so a tool-call argument cannot leak a token into a
session log.

### Where to obtain a token

| service | token type | source |
|---------|------------|--------|
| GitHub  | Fine-grained PAT | Settings → Developer settings → Personal access tokens → Fine-grained tokens. `Contents: read` + `Issues: read` + `Pull requests: read` covers `github://`. |
| Todoist | Personal API token | Settings → Integrations → Developer (top of the page). |
| Notion  | Internal integration token | https://notion.so/profile/integrations → New integration (`ntn_...`). Then **Add connections** on each page / database the integration needs to read. |
| Slack   | Bot token (`xoxb-...`) | Create an app at https://api.slack.com/apps → OAuth & Permissions → install to your workspace. Minimum scopes for read-only use: `channels:read` + `channels:history` + `users:read`. |

### Wiring a credentialed source into a Node

Once the token is registered, adding the wiring entry is identical to
the local schemes above — just pass a scheme URI in `source_uri`:

```jsonc
// MCP: wire_node_create
{ "name": "alpha.gh_issues",
  "type": "outline_node",
  "metadata": {
    "persona":    "alpha",
    "axis":       "gh_issues",
    "source_uri": "github://ynishi/persona-wire?kind=issues&state=open&limit=10"
  } }
```

Do **not** embed the token in the URI. Credentials are resolved at fetch
time through the provider chain above; `source_uri` values are stored in
plaintext in the SQLite graph and would leak a token literal.

## 2b. Alias path — filtered / paginated fetches

> **Status**: Both global alias storage and per-table `_aliases` are
> resolved. As of mini-app v0.12.1+ the default destination of
> `alias_create` is **global storage** (`<base>/_global.db` →
> `_global_aliases` table), and `wire` resolves it first; legacy
> per-table `_aliases` rows still resolve via fallback (see §URI form
> below). The `scope=user` / `scope=<project>` reserved keys redirect
> the alias lookup to the corresponding `_global.db`. The remaining
> wire-side scope-outs (aggregator / multi-source / pattern source)
> are listed in §Known limitations below.

### Step 0 — Pick a storage and register the alias

The mini-app installation owns two alias storages:

- **global** (`<base>/_global.db` → `_global_aliases`): default for
  new aliases since mini-app v0.12.1. Alias name is **unique within
  a scope across all tables**, so multiple tables sharing the same
  alias name in the same scope is rejected with `ALIAS_ALREADY_EXISTS`.
  Use a `<table>_for_<persona>` / `<persona>_<purpose>` convention
  to avoid collisions (the mailbox / friend_map smoke uses
  `for_shi` / `friend_for_shi`).
- **per-table** (`<base>/<table>/<table>.db` → `_aliases`): legacy
  layout. Still resolved via fallback when the legacy URI form
  (`?scope=` omitted) does not find the alias in the User-scope
  `_global.db`.

Standard registration via the mini-app MCP server:

```
mcp__mini-app__info({ table: "<table>" })          # field definitions
mcp__mini-app__alias_list({ table: "<table>" })    # existing aliases (both storages)
mcp__mini-app__alias_create({                       # writes to global by default
  table: "<table>",
  name:  "<alias_name>",
  filter: { "type": "eq", "field": "to", "value": "<persona>" },
  scope: "user",                                    # "project" | "user"
  limit: 30
})
```

If your callers already have aliases in the per-table `_aliases`
layout, they keep working through the legacy fallback path — no
migration is required to start using `wire`.

### URI form

```
mini-app://<table>?alias=<name>[&<k>=<v>]*[&limit=<n>][&scope=<scope>][&root=<dir>]
```

- `alias=<name>` — required for this path. Resolution order depends
  on `scope`:

  | URI form                                | lookup target                                                                            |
  |-----------------------------------------|------------------------------------------------------------------------------------------|
  | `?alias=N` (no `scope`)                 | User-scope `_global.db` first; on miss, per-table `<table>.db._aliases` (legacy fallback) |
  | `?scope=user&alias=N`                   | User-scope `_global.db` only (hard target, no fallback)                                  |
  | `?scope=<project-name>&root=<dir>&alias=N` | Project-scope `<dir>/_global.db` only (hard target, no fallback)                      |

- `<k>=<v>` — bind variables consumed by the alias body
  (e.g. `?alias=unread_for&persona=alpha`).
- `limit=<n>` — caps the row count returned by the alias body.
- `scope=user|<project-name>` (reserved key) — selects a hard target
  in `_global.db`. Per-table `_aliases` fallback is **not** consulted
  when `scope` is set.
- `root=<dir>` (reserved key) — explicit base directory (`~/` is
  expanded against `$HOME`). Required when `scope=<project-name>` is
  set (parse fails fast otherwise). For `scope=user` / `scope=` omitted
  it overrides `$MINI_APP_USER_DIR` / `~/.mini-app/` and resolves to
  `<root>/<table>/<table>.db` for per-table fetches plus
  `<root>/_global.db` for global alias resolution.

### Example — global alias (mini-app v0.12.1+ default)

```jsonc
// 1. Register the alias in global storage.
// mcp__mini-app__alias_create({
//   table: "mailbox", name: "for_shi", scope: "user",
//   filter: { "type": "eq", "field": "to", "value": "shi" }, limit: 30
// })

// 2. Reference it from a wiring entry. `?scope=user` is optional —
//    the legacy URI form (no `?scope=`) falls back to global first.
{ "name": "shi.mailbox",
  "type": "outline_node",
  "metadata": {
    "persona":    "shi",
    "axis":       "mailbox",
    "source_uri": "mini-app://mailbox?alias=for_shi"
  } }
```

If `wire_prompt_context` returns
`alias '<name>' not found in _global.db (User scope) nor per-table
<table>._aliases fallback`, neither storage has the alias — re-check
the registration or the alias name (global names are scope-unique
across tables, so a collision on a different table surfaces as a
different error: `execute_alias_run failed: table not found: <other>`).

### Known limitations — aggregator / multi-source / pattern source

`wire`'s `Layer 6 Adapter::fetch_via_alias` is scoped to
**Single-source, non-aggregator** aliases. The following alias shapes
exist in mini-app but are intentionally rejected with a clear error on
the wire side, tracked as P3b carry:

- aliases with an `aggregator` (`Count` / `Sum` / `Avg` / `Min` /
  `Max` / `GroupBy`) — wire returns
  `alias '<name>' has aggregator — wire scope 外 (P3b carry)`.
- aliases with `SourceSpec::Multi(...)` (multiple source tables) —
  wire returns
  `alias '<name>' has Multi / Pattern source — wire scope 外 (P3b carry)`.
- aliases with `SourceSpec::Pattern(...)` (glob over multiple tables) —
  same error as Multi.

For these shapes, either keep the alias for direct mini-app callers
and add a Single-source / `Rows` mirror alias for `wire`, or use the
bare `mini-app://<table>` form and push the aggregation / cross-table
join to the NamedProjection template (handlebars `{{#each}}` +
`{{#if}}`) or a sibling consumer skill.

## 2c. Tuning an existing wiring entry — `wire_node_update`

Wiring entries are tuned in place via `wire_node_update`. The opaque
ULID `id` is preserved, so Specifications and edges referencing the
entry stay intact — no delete + recreate dance is needed. The `id`
field accepts either the ULID or the human-readable `name`.

```jsonc
// Default: metadata_patch mode (RFC 7396 shallow merge).
// Only the listed keys are touched; the rest of `metadata` is kept.
// `id` here is the `id_or_name` resolver input — pass the ULID or
// the original `name` you used in wire_node_create.
{
  "id": "alpha.mailbox",
  "metadata_patch": {
    "source_uri": "mini-app://mailbox?alias=for_alpha&limit=10"
  }
}

// Optional: full replace mode for a wholesale rewrite.
{
  "id": "alpha.mailbox",
  "metadata": { "persona": "alpha", "axis": "mailbox",
                "source_uri": "mini-app://mailbox?alias=for_alpha&limit=5" },
  "mode": "replace"
}
```

### `&limit=<n>` tuning pattern

The most common reason to call `wire_node_update` is right-sizing the
fetch budget on an existing `source_uri`. Rules of thumb:

- **Append the right separator**: if the URI already contains `?`,
  append `&limit=N`; otherwise start the query string with `?limit=N`.
  ```
  mini-app://mailbox?alias=for_alpha       →  + &limit=10
  mini-app://alpha_brief                   →  + ?limit=1
  ```
- **Typical values**:
  - mailbox-style streams (newest-first inbox) → `&limit=10`
  - latest-snapshot tables (`*_brief`, `*_state`) → `?limit=1`
  - small tables (≲ 5 rows total) — leave the default, no `limit` needed
  - `file://` sources — out of scope (limit applies to mini-app rows only)
- **No alias rewiring required**: `limit` is honoured both on the bare
  list path (`mini-app://<table>?limit=N`) and on the alias path
  (`?alias=<name>&limit=N`). Existing aliases keep working.

After the patch, run `wire_prompt_context` end-to-end to confirm the row
count and template output reflect the new cap. `wire_render` reflects
the metadata / node changes but does not exercise the adapter, so it
will not show a different row count on its own.

## 3. Register the Specification and NamedProjection (template = data)

There is no hard-coded projection list inside the crate. Every projection
is data, registered through the same tool surface.

```jsonc
// MCP: wire_spec_register — picks one wiring entry per axis
{
  "name": "alpha.spec.active",
  "json": "{\"And\":[{\"TypeIs\":\"outline_node\"},{\"MetadataEq\":{\"path\":\"persona\",\"value\":\"alpha\"}},{\"MetadataEq\":{\"path\":\"axis\",\"value\":\"active\"}}]}"
}

// MCP: wire_projection_register
{
  "name": "alpha.section.active",
  "spec_ref": "alpha.spec.active",
  "target_form": "markdown",
  "template": "## Active set\n{{#each entries}}{{#each this.fetched_data.rows}}- [{{#if this.data.pin}}pin{{else}}-{{/if}}] {{this.id}} — {{this.data.label}}\n{{/each}}{{/each}}"
}
```

The render-time context depends on which use case renders the projection.
There are two shapes, and the difference is not cosmetic — only one of
them fetches the `source_uri` through a Layer 6 Adapter.

**`wire_prompt_context` (async, Adapter-fetched)** — every wiring entry's
`source_uri` is fresh-fetched through the Layer 6 Adapter before the
template runs. Context shape:

- `count` — always `1` for the wire_prompt_context per-slot path (one
  entry per rendered projection)
- `persona_id` — the persona passed to `wire_prompt_context`
- `slot` — the slot name (legacy metadata key `axis` on the wiring
  node; the render context surfaces it as `slot`)
- `entries`: `[ { wiring_entry: { slot, source_uri }, fetched_data: <Adapter return value> } ]`
  — this is where fetched adapter output lives; iterate with
  `{{#each entries}}{{this.fetched_data...}}{{/each}}`. The
  `wiring_entry` view is deliberately minimal (only `slot` and
  `source_uri`); if you need node metadata inside the template, route
  it through the adapter output or add a `nodes[]` axis via
  `wire_render`

**`wire_render` / `wire_init` (sync, no Adapter fetch)** — the spec is
evaluated against the graph and the matched nodes are handed to the
template verbatim. **No `source_uri` fetch happens**, so
`this.fetched_data` is not available. Context shape:

- `count` — number of matched nodes
- `names` — comma-separated node names (`"n1, n2, …"`)
- `nodes`: `[ { id, type, metadata } ]` — the matched nodes as-is; use
  this axis when you only need graph state, not adapter output
- `persona_id` — only present on `wire_init` (it is name-addressed on
  `wire_render` and omitted)

Rule of thumb: iterate `entries[].fetched_data` if the template needs
adapter output (mailbox rows, feed items, page bodies, …) — call it via
`wire_prompt_context`. Iterate `nodes[]` if the template only needs
graph state (id / type / metadata) — either path renders it.

Handlebars features available: `{{var}}`, `{{a.b.c}}`, `{{#each list}}…{{/each}}`,
`{{#if cond}}…{{else}}…{{/if}}`. HTML escaping is disabled. A parse failure
returns `{{render-error: <message>}} <raw template>` so the failure is visible
in the rendered output instead of disappearing into a panic.

## 4. Optional — persona-pack overlay

When a persona needs to deviate from the registered base template (a
register-specific emote, a header line, a different target form), drop an
overlay into `~/persona-pack/<persona_id>/prompt.toml`:

```toml
[extra.persona_wire.projections.active]
template = "(^_^) Active set sweep\n{{> base }}"   # `base` placeholder is illustrative
target   = "markdown"        # optional, default = "markdown"
strategy = "append"          # replace | append | prepend | section:<name>
```

The resolver walks `[extra.persona_wire.projections.*]` once per call,
turns each entry into a `ProjectionOverlay { template, target_form, strategy }`,
and the use case folds the overlay into the base template through
`MergeStrategy::merge(base, overlay)` before rendering. `Section(name)`
replaces a `{{!-- <name> --}}` marker inside the base template; it falls
back to `Append` when the marker is absent.

## 5. Smoke-test the persona

```sh
# CLI smoke
persona-wire wire-doctor
persona-wire query --spec '{"And":[{"TypeIs":"outline_node"},{"MetadataEq":{"path":"persona","value":"alpha"}}]}'
```

A healthy graph reports `orphan nodes (no edges, not self-attached): 0`.
Wiring entries that hold `metadata.source_uri` or `metadata.maintenance_exempt:
true` are treated as self-attached and are not counted as orphans even when
they carry no edges (see §2). A non-zero count typically points at a bare
persona node with no edges and no `source_uri` — add an edge or a metadata
field to silence it.

### wire_doctor — 2-axis integrated health report

`wire_doctor` is a 2-axis integrated health check: axis 1 (graph
connectivity) and axis 2 (workflow coverage) are evaluated through the
internal `application::doctor::probes` registry (`graph_*` / `workflow_*`
Probes) and emitted into a structured JSON response:

```jsonc
// MCP: wire_doctor — 2-axis integrated health check
{}
```

Response shape:

```json
{
  "orphan_node_count": 0,
  "total_node_count": 3,
  "total_edge_count": 2,
  "report_markdown": "# wire_doctor report\n\n## graph_check …\n\n## workflow_check …",
  "graph_check": {
    "orphan_count": 0,
    "total_nodes": 3,
    "total_edges": 2,
    "report_markdown": "## graph_check (axis 1: graph connectivity)\n…"
  },
  "workflow_check": {
    "total_nodes": 3,
    "declared_covered_count": 1,
    "declared_covered": [],
    "declared_uncovered": [],
    "undeclared": [],
    "exempt": [],
    "workflows_observed": 1
  }
}
```

The top-level `orphan_node_count` / `total_node_count` / `total_edge_count`
fields are backward-compat mirrors of `graph_check.orphan_count` /
`graph_check.total_nodes` / `graph_check.total_edges` respectively. Existing
callers that read only the flat fields continue to work unchanged.

> Axis-1-only inspection used to ship as a standalone `wire_graph_check`
> MCP tool. From 0.4.0 the tool was retired and graph connectivity is
> reported through the `wire_doctor` 2-axis report (the `graph_check`
> sub-object). For implementation detail see the
> `application::doctor::probes::graph_*` Probe registry in the Rustdoc.

```jsonc
// MCP: wire_prompt_context — renders every registered axis for the persona
{ "persona_id": "alpha" }

// MCP: wire_prompt_context — explicit subset (include)
{ "persona_id": "alpha", "projection_names": ["active"] }

// MCP: wire_prompt_context — exclude a few noisy slots from the full set
// (v0.9.0+) Useful when you want "everything except mail / tick_log / etc."
// without enumerating the full remainder on the include side.
{ "persona_id": "alpha", "projection_exclude_names": ["tick_log"] }

// MCP: wire_prompt_context — include ∧ ¬exclude (AND NOT, v0.9.0+)
// Both arguments compose: exclude wins on intersection, unknown names
// are silently ignored.
{ "persona_id": "alpha",
  "projection_names":         ["active", "handoff", "mailbox"],
  "projection_exclude_names": ["mailbox"] }
```

Successful output looks like:

```json
{
  "persona_id": "alpha",
  "projections": [
    { "name": "alpha.section.active", "target_form": "Markdown",
      "rendered": "## Active set\n- [pin] item-1 — label-1\n…" }
  ],
  "prompt_context": "## Active set\n- [pin] item-1 — label-1\n…",
  "warnings": []
}
```

If a wiring entry has no matching projection registered (or the optional
overlay refers to an unknown axis), it surfaces in the `warnings` array
and the rest of the call still succeeds.

## 6. Wire it into a Skill / Prompt

The wake-time pattern is to call `wire_prompt_context` once and inline the
resulting `prompt_context` block into the session. A minimal Skill body:

```
1. Call `mcp__persona-wire__wire_prompt_context({ persona_id: "<id>" })`.
2. Emit the returned `prompt_context` verbatim as a single block in the
   output, before any summary / checklist line.
3. If `warnings[]` is non-empty, surface them as `⚠ persona_wire: <warning>`.
```

A handy subset call for narrower steps:

```
mcp__persona-wire__wire_prompt_context({
  persona_id: "<id>",
  projection_names: ["handoff"]
})
```

Or, when you want everything except a few noisy slots (v0.9.0+):

```
mcp__persona-wire__wire_prompt_context({
  persona_id:               "<id>",
  projection_exclude_names: ["tick_log", "mailbox"]
})
```

## 6b. Loop / review / update-check trigger pattern

A common ask is "for this Node, surface an update-check instruction the
next time the persona wakes / closes a session / runs a periodic tick".
The dedicated `wire_workflow_*` Tools cover the discrete-event trigger
(see *Implemented* below); the same intent also composes cleanly from
the existing Query / Projection / Adapter primitives when a custom
cadence is needed.

### Use cases

- UC1 — *session-close review*: at session end, list Nodes whose
  `review_due` is past or whose `last_verified_at` is older than a
  cadence threshold.
- UC2 — *wake-time pending list*: at session start, inject a short
  "check these before you start" block.
- UC3 — *stale node surfacing*: periodically (cron / launchd / Hook) call
  the same projection and route its output to a notification channel.

All three are the same shape: a Projection over Nodes whose metadata says
"I need to be revisited". Only the trigger (who calls it) differs.

### Recipe — metadata + Specification + Projection

1. Tag Nodes at creation time. The current Specification AST is leaf =
   `TypeIs` / `MetadataEq` and composite = `And` / `Or` / `Not` (see
   `docs/wire-query-spec.md`) — there is no time-aware `MetadataLte` yet,
   so model "needs review" as a plain boolean / enum flag in metadata:

   ```jsonc
   // MCP: wire_node_create
   {
     "name": "alpha.handoff",
     "type": "outline_node",
     "metadata": {
       "persona":         "alpha",
       "axis":            "handoff",
       "source_uri":      "file:~/persona/alpha/handoff/",
       "review_on_close": true,
       "review_note":     "drop the next handoff file before /work-close"
     }
   }
   ```

   (Cadence-driven freshness — "older than 7d" — is best done one layer
   out: keep `last_verified_at` in metadata, run the date comparison in
   the consuming Skill or in a `mini-app://` Adapter that filters
   server-side, and let wire_query stay shape-pure with `MetadataEq`.)

2. Register a "review_pending" Specification and Projection:

   ```jsonc
   // MCP: wire_spec_register
   {
     "name": "alpha.spec.review_pending",
     "json": "{\"And\":[{\"MetadataEq\":{\"path\":\"persona\",\"value\":\"alpha\"}},{\"MetadataEq\":{\"path\":\"review_on_close\",\"value\":true}}]}"
   }

   // MCP: wire_projection_register
   {
     "name":        "alpha.section.review_pending",
     "spec_ref":    "alpha.spec.review_pending",
     "target_form": "markdown",
     "template":    "## Review pending\n{{#each entries}}- **{{this.wiring_entry.slot}}** — {{this.wiring_entry.source_uri}}\n{{/each}}"
   }
   ```

3. Call the projection from whichever Trigger fits — the Tool surface is
   identical regardless of who pulls the trigger:

   ```jsonc
   mcp__persona-wire__wire_prompt_context({
     persona_id:       "alpha",
     projection_names: ["review_pending"]   // ← axis name, NOT full projection name
   })
   ```

   Two conventions to keep aligned (else the projection is silently
   skipped with a warning):

   - The Node's `metadata.axis` value must match the suffix used in the
     projection name. `wire_prompt_context` iterates the persona's Nodes
     and for each one looks up a projection literally named
     `<persona>.section.<axis>` — the example above needs Node axis
     `review_pending` + projection name `alpha.section.review_pending`.
   - `projection_names[]` takes **axis names**, not the full projection
     name (so `"review_pending"`, not `"alpha.section.review_pending"`).

### Trigger layer (generic — Skill / Command / Hook / cron)

The wire side only emits the rendered block; *what fires the call* is a
caller-side concern and intentionally not modeled inside wire. Common
trigger surfaces:

- **session-close Skill / Command** — a project's own close-session
  routine calls `wire_prompt_context({ projection_names: ["...review_pending"] })`
  and inlines the result before writing the handoff.
- **wake Skill** — the same call at session start, before the persona
  takes its first action.
- **Hook (e.g. UserPromptSubmit, SessionStart)** — wire the call into a
  harness Hook so the prompt is injected without an explicit user step.
- **cron / launchd / external scheduler** — call the MCP tool from a
  scheduled job and route the rendered Markdown to a notification sink
  (mail, mini-app row, log file).

All four share one wire call; the loop / cadence lives in the Trigger.

### Implemented — `wire_workflow_*` + `wire_doctor` coverage audit

The trigger / action portion (`wire_workflow_fire` /
`wire_workflow_register` / `wire_workflow_list` / `wire_workflow_delete`)
is implemented. A workflow is a Node carrying
`metadata.maintained_by.event = "<event>"`; firing the event runs the
declared action.

The coverage audit (declared maintenance plan ↔ actually-wired workflow
Nodes) used to ship as a standalone `wire_workflow_check` MCP tool.
From 0.4.0 the tool was retired and the audit is now reported through
the `wire_doctor` 2-axis report (the `workflow_check` sub-object plus
`findings[]` with Severity). See `application::doctor::probes::workflow_*`
in the Rustdoc for the Probe registry shape.

```jsonc
// MCP: wire_workflow_fire — invoke every workflow whose
// metadata.maintained_by.event matches the given event for this persona.
{ "event": "session_close", "persona_id": "alpha" }
//   → { "fired": [ { "node_id": "...", "result": { "prompt_context": "..." } } ],
//       "skipped": [ ... ] }

// MCP: wire_doctor — graph (axis 1) + workflow coverage (axis 2) audit.
{ "persona_id": "alpha" }
//   → { "graph_check": { ... }, "workflow_check": { ... }, "findings": [ ... ] }
```

Declarative cadence (e.g. `every 7d`) and write-side helpers are still
in the carry roadmap; until those land, model time-aware cadence one
layer out as described in the recipe above and use `wire_workflow_*` +
`wire_doctor` for the discrete-event trigger / coverage audit.

## 6c. Migrating from a per-persona config layer

If a project's wake / session-close skill previously read per-persona
Management Scope from a custom config layer (e.g., a project-specific
`[extra.persona_work]` block on persona-pack, or a side-file under
`~/my-personas/<id>/work-config.toml`), the canonical wire-side path
collapses to three steps:

1. Register one wire Node per axis (active / handoff / toolmap /
   priorities / tick_log / mailbox / …) — see §2. The Node holds only
   the `source_uri`; the data still lives in its SoT (mini-app row /
   file / outline node).
2. Call `wire_prompt_context(persona_id="<id>")` from the wake skill —
   it iterates the persona's registered axes, fresh-fetches each
   `source_uri` through the Layer 6 Adapter, and returns one
   concatenated PromptContext literal. The wake skill no longer needs a
   per-persona Management Scope read.
3. For session-close maintenance (handoff emit, tick log append, brief
   refresh), register a workflow Node with
   `metadata.maintained_by.event = "session_close"` and call
   `wire_workflow_fire({ event: "session_close", persona_id: "<id>" })`
   from the close skill — see §6b for the workflow tool shape.

`projection_names: ["axis"]` lets the caller subset the inject (handoff
only at close, full set at wake), so the same wiring serves both ends
of the session without skill-side branching. `projection_exclude_names`
(v0.9.0+) covers the opposite case — render every wired axis except
the noisy ones (e.g. `["tick_log", "friend_map"]` at work-mode wake,
without enumerating the full remainder on the include side).

## 7. Add another persona

The whole flow above is the same per persona — register the Node(s),
register the Specification(s), register the NamedProjection(s), optionally
add a persona-pack overlay. Nothing in `persona-wire-core` needs to be
recompiled to support a new persona; it is entirely data.

## 8. Bundle — scaffolding installer

Once one persona has gone through §2–§5 by hand, the next persona is
mostly the same shape: a Node, a Specification, a NamedProjection, an
optional Wiring + Workflow. The Bundle layer packages that shape as a
TOML manifest registered once and installed any number of times.

### 8.1 Manifest shape

```toml
[bundle]
name = "quickstart"
version = "0.1.0"
description = "Minimal persona + spec + projection scaffold."

# All section arrays are optional. Empty bundle (= header only)
# parses successfully and dispatches as a no-op.

[[nodes]]
name = "shi"
node_type = "persona"
metadata = { owner = "ytk", role = "companion" }

[[edges]]
from_name = "shi"     # resolves against same-bundle nodes first, then graph
to_name   = "dolly"
edge_type = "routes_to"

[[specs]]
name = "active_personas"
spec = { TypeIs = "persona" }                       # externally-tagged enum
# spec = { MetadataEq = { path = "owner", value = "ytk" } }
# spec = { And = [ { TypeIs = "persona" }, { Not = { TypeIs = "channel" } } ] }

[[projections]]
name = "personas_overview"
spec_ref = "active_personas"                        # resolves bundle-first, then registry
template = "## Personas\n{{#each nodes}}- {{name}}\n{{/each}}"
target_form = "prompt"                              # prompt | markdown | json | ascii

[[wirings]]
persona_id = "shi"
slot = "mailbox"
source_uri = "mini-app://mailbox?alias=for_shi"
projection_ref = "personas_overview"                # optional

[[workflows]]
id = "shi-wake"
persona_id = "shi"
trigger = { kind = "on_demand" }                    # or { kind = "on_event", event = "..." }
action = { kind = "no_op" }                         # or { kind = "emit_projection", projection_names = ["..."] }
```

The `spec` body uses the Specification serde shape verbatim
(externally-tagged enum — first key is the variant name). The `trigger`
and `action` bodies pass through to `wire_workflow_register`, so every
Workflow invariant the entity layer already enforces still applies.

### 8.2 Register, install, inspect

The same five operations are available through the CLI and the MCP tool
surface (`wire_bundle_register` / `wire_bundle_list` /
`wire_bundle_get` / `wire_bundle_install` / `wire_bundle_delete`).

```sh
# CLI flow
persona-wire bundle register --file bundles/quickstart.toml
persona-wire bundle list
persona-wire bundle install --ref quickstart            # mode=increment (default)
persona-wire bundle install --ref quickstart --mode skip
persona-wire bundle get --ref quickstart                # returns full TOML body
persona-wire bundle delete --ref quickstart             # install history retained
```

### 8.3 Conflict resolution

Name collisions are resolved per `--mode`:

- `increment` (default) — non-destructive auto-suffix. An entity named
  `shi` collides with the existing `shi` row → installs as `shi-1`. A
  second re-install becomes `shi-2`. Internal references inside the
  same manifest (`projections.spec_ref`, `edges.from_name` /
  `to_name`) are rewritten to the post-rename names so re-installing
  never produces a half-broken graph.
- `skip` — leave the existing row alone, record the collision in the
  install report's `skipped[]`. Idempotent for fixed-name bundles.
- `error` — abort the whole install on the first collision. Nothing is
  written. Strict mode for scaffold-into-empty environments.

Force / overwrite is intentionally **not** in v1 — install history
(`bundle_installs` table) is already populated each install so a future
History UI can carry it.

### 8.4 Install report

`wire_bundle_install` returns a structured report (also pretty-printed
by the CLI):

```json
{
  "install_id": "01JC...",
  "bundle_id": "01J9...",
  "mode": "increment",
  "installed": [
    { "kind": "spec",       "original_name": "active_personas",   "final_name": "active_personas",   "id": "..." },
    { "kind": "projection", "original_name": "personas_overview", "final_name": "personas_overview", "id": "..." },
    { "kind": "node",       "original_name": "shi",               "final_name": "shi",               "id": "..." }
  ],
  "skipped": [],
  "errors":  []
}
```

Per-entity failures (bad spec body, unknown `target_form`, missing
referenced node, etc.) land in `errors[]` without aborting the
remaining sections — except in `error` mode, where the first failure
short-circuits.

A sample bundle is bundled at `bundles/quickstart.toml`.

## 9. Reference

- Crate-level Rustdoc (architecture, layer split, persistence schema):
  `cargo doc --workspace --open -p persona-wire-core`
- Specification AST: [`persona_wire_core::domain::specification`]
- Render flow + prompt-context flow: top of
  `crates/persona-wire-core/src/lib.rs`
- MCP server instructions: `get_info().instructions`
- This guide as MCP resource: `wire-guide://onboarding`
