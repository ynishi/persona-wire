# persona-wire-core::application::projection_overlay

Projection Overlay — Wire の domain 型 (上流 SoT 非依存)。

`wire_prompt_context` Phase 0 で各 slot の base template に被せる Overlay 情報。
取得経路は `PluginRegistry::route("<scheme>://<persona_id>/projections")` 経由の
Adapter dispatch (= ACL Facade、 上流 SDK 固有の TOML field / Type を Wire に漏らさない)。

Wire 定義の return JSON shape (Adapter が返す側の契約):

```json
{
  "scheme": "<adapter-scheme>",
  "persona_id": "<id>",
  "projections": {
    "<slot>": {
      "template": "<string>",
      "target_form": "markdown" | "json" | "text" | "ascii",
      "merge_strategy": "replace" | "append" | "prepend" | "section"
    }
  }
}
```

`target_form` / `merge_strategy` の値は文字列で受け取り、 Wire の typed enum
(`TargetForm` / `MergeStrategy`) に parse する。 未知値は default (markdown / replace)。

## Functions

- `parse_overlay_response` — Adapter return JSON を `BTreeMap<slot, ProjectionOverlay>` に翻訳する。

## Types

- `ProjectionOverlay` — 1 slot 分の Overlay 解決結果 (Wire domain 型)。

