# persona-wire-core::application::doctor::render

Finding → Markdown render + verdict 集約。

design.md §5 (verdict) / §8 (output 形式) に対応。

## Functions

- `aggregate_verdict` — design §5: error >= 1 → BROKEN / warn >= 1 → DEGRADED / 全 PASS → HEALTHY。
- `render_adapters` — Renders the `## Adapters` section (adapter-filter-if Phase 1): one line
- `to_markdown` — (no documentation)

## Types

- `Verdict` — (no documentation)

