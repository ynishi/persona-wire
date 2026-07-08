# persona-wire-core::application::doctor::probe

Probe trait — wire_doctor の検査素子。

1 Probe = 1 kind が default、 1 Probe で複数 kind emit も許容。
全 Probe は default で registry に埋め込まれる (registry::default)。

## Types

- `FindingSink` — Finding の蓄積先。
- `ProbeCtx` — Probe 走査時の context。 `persona_filter` が Some なら persona-scoped mode。

## Traits

- `Probe` — 検査素子。 doctor は registry から Vec<Box<dyn Probe>> を取り順次 scan を呼ぶ。

