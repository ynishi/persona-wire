# persona-wire-core::application::merger

Template Merger Strategy。

persona-pack の Overlay (= `[extra.persona_wire.projections.<slot>]`) を base
template (wire DB の動的 register or `BUILTIN_PROJECTIONS`) と merge する戦略を
明示的に持つ。 完全 replace だけでなく append / prepend / partial section
上書きを persona-pack 側で指定可能にする = engineering
規律で組む base infra。

```toml
[extra.persona_wire.projections.active]
strategy = "append"   # default = "replace"
template = "...emote / register 上乗せ..."
target   = "markdown"
```

## Types

- `MergeStrategy` — Overlay と base template を merge するときの戦略。

