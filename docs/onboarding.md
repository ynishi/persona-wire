# Onboarding — Wiring a new persona end-to-end

This guide walks through the full workflow for adding a new persona to
persona-wire, from a fresh install to a working `wire_prompt_context` call
and a sample Skill / Prompt that consumes it.

It is also exposed through the MCP server as a resource at
`wire-guide://onboarding`, so a client (Claude Code Skill, CLI agent, etc.)
can fetch it with `read_resource` without leaving the session.

## 0. Mental model in one paragraph

The graph holds **wiring entries** — one `Node` per axis you want to expose
for the persona. Each wiring entry carries a `metadata.source_uri` that
points at the real Source-of-Truth (mini-app table, file, …). A
**Specification** picks the wiring entries; a **NamedProjection** binds the
Specification to a handlebars template and a target form. `wire_render`
fetches the source fresh through the Layer 6 Adapter and renders the
template. `wire_prompt_context` walks every axis for the persona, optionally
filtered by `projection_names`, and concatenates the rendered blocks into a
single PromptContext string. Optional per-persona overlays live in
persona-pack under `[extra.persona_wire.projections.<axis>]` and are folded
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

## 2. Set up a persona's wiring entries (one Node per axis)

Pick the axes you want (`active` / `ng` / `trigger` / `handoff` / `toolmap`
is one common shape, but anything works). For each axis create one node
with a `source_uri`. Add an edge from the persona node for traceability
(optional but recommended).

```jsonc
// MCP: wire_node_create
{ "id": "alpha",                "type": "persona",
  "metadata": { "display": "alpha" } }

{ "id": "alpha.active",         "type": "outline_node",
  "metadata": { "persona": "alpha", "axis": "active",
                "source_uri": "mini-app://alpha_active_context" } }

{ "id": "alpha.handoff",        "type": "outline_node",
  "metadata": { "persona": "alpha", "axis": "handoff",
                "source_uri": "file:~/path/to/alpha/handoff/" } }

// MCP: wire_edge_create
{ "id": "e.alpha.active",  "src": "alpha", "tgt": "alpha.active",  "kind": "routes_to" }
{ "id": "e.alpha.handoff", "src": "alpha", "tgt": "alpha.handoff", "kind": "routes_to" }
```

`source_uri` schemes supported today:

- `mini-app://<table_name>` — opens `~/.mini-app/<table>/<table>.db` via the
  `mini-app-core` SDK and lists all rows. `MINI_APP_USER_DIR` overrides the
  base directory.
- `file://<path>` or `file:<path>` — `std::fs::read`. `~/` is expanded.
  Directory paths return the newest mtime child (handy for
  `handoff/YYYY-MM-DD.md` patterns).

Bulk-insert through `wire_nodes_create_batch` / `wire_edges_create_batch`
when you have many axes at once.

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

The render-time context has:

- `count`, `persona_id`, `axis`
- `entries`: `[ { wiring_entry: { axis, source_uri }, fetched_data: <Adapter return value> } ]`
- `nodes`: legacy projection of the matched nodes (kept for ad-hoc use cases)

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

```jsonc
// MCP: wire_prompt_context — renders every registered axis for the persona
{ "persona_id": "alpha" }

// MCP: wire_prompt_context — explicit subset
{ "persona_id": "alpha", "projection_names": ["active"] }
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

## 7. Add another persona

The whole flow above is the same per persona — register the Node(s),
register the Specification(s), register the NamedProjection(s), optionally
add a persona-pack overlay. Nothing in `persona-wire-core` needs to be
recompiled to support a new persona; it is entirely data.

## 8. Reference

- Crate-level Rustdoc (architecture, layer split, persistence schema):
  `cargo doc --workspace --open -p persona-wire-core`
- Specification AST: [`persona_wire_core::domain::specification`]
- Render flow + prompt-context flow: top of
  `crates/persona-wire-core/src/lib.rs`
- MCP server instructions: `get_info().instructions`
- This guide as MCP resource: `wire-guide://onboarding`
