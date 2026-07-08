# persona-wire-core::application::plugin_registry

PluginRegistry — 3 軸 Plugin (Adapter / TemplateEngine / Projection) を統合管理。

server boot 時に register、 runtime mutation なし (= immutable after `build()`)。
Plugin の物理境界は外部 crate (例: `wire-adapter-pg` / `wire-template-jinja` /
`wire-projection-llm`)、 boot 側 (`persona-wire-mcp` / `persona-wire` bin) で
`PluginRegistry::builder()` に流し込んで構築する。

## boot 例

```ignore
use persona_wire_core::application::plugin_registry::PluginRegistry;
use persona_wire_core::infrastructure::adapter::FileAdapter;
use persona_wire_core::infrastructure::template::HandlebarsEngine;
use persona_wire_core::infrastructure::projection::StaticProjection;
use persona_wire_adapter_mini_app::MiniAppAdapter;

let registry = PluginRegistry::default_builder_for_wire()
    .with_adapter(MiniAppAdapter)
    .build()
    .expect("plugin registry build");
```

P3a stage: registry skeleton + builder + lookup surface のみ。 use_cases.rs
側の dispatch 配線 (registry を引数で受け取って fetch / render を引く form)
は P3a 後段で順次差し替え (現状は free fn `fetch_via_adapter` + `rendering::render`
直呼びを維持、 後方互換)。

## Types

- `PluginRegistry` — 3 軸 Plugin を統合管理する immutable registry。
- `PluginRegistryBuilder` — builder。 同一 scheme / id / kind の重複登録は `build()` 時に fail-fast。

