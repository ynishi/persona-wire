# persona-wire-core::infrastructure::rendering

Rendering adapter — render extracted graph subsets into output forms.

Engine: `handlebars` (Mustache superset) — supports section iteration
(`{{#each list}}…{{/each}}`), conditionals (`{{#if cond}}…{{/if}}`),
dotted path lookup (`{{a.b.c}}`), and the existing scalar substitution
syntax (backward-compatible with the P1 minimal engine).

HTML-escape is **disabled** globally: wire emits markdown / prompt /
json / ascii, none of which want HTML entity encoding (`<` → `&lt;`).

## Functions

- `render` — Render `template` against `data` using a handlebars engine.

