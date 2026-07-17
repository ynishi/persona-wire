# persona-wire-adapter-obsidian 0.14.0

persona-wire Adapter for Obsidian vault (scheme `obsidian://`).

Reads Markdown notes from a local Obsidian vault directory, extracts
YAML / TOML frontmatter via [`gray_matter`], and returns structured JSON.

## URI form

```text
obsidian:///<vault-root>/<note>[?frontmatter={on|off}&links={off|edge}]
```

- `<vault-root>` — path to the Obsidian vault directory (absolute or `~/`-prefixed)
- `<note>` — note file name (relative to vault root)
- `?frontmatter` = `on` (default) | `off`
- `?links` = `off` (default) | `edge` (returns `wiki_links` array)

## Return shape

```json
{
  "vault_path": "<absolute vault root>",
  "note_path": "<note filename relative to vault>",
  "frontmatter": { ... } | null,
  "body": "<markdown body without frontmatter fence>",
  "wiki_links": [{"target": "Note A", "raw": "[[Note A]]"}]
}
```

`wiki_links` is only present when `?links=edge` is specified.

