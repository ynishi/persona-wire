# persona-wire-adapter-persona-pack 0.14.0

`persona-wire-adapter-persona-pack` — persona-pack SDK access の ACL Facade。

## 役割

persona-pack は persona の Pack TOML を扱う外部 crate。 そこに
`[extra.persona_wire.projections.<axis>]` という Wire 用 Overlay を書く運用がある
が、 上流 crate の API / TOML field を **persona-wire の core layer に漏らさない**
ように本 crate が境界として機能する (= ACL Facade)。

## Wire 側が定義する URI 形式

- scheme = `persona-pack`
- host = `<persona_id>` (必須)
- path = `/projections` (現状唯一の resource、 将来分岐余地)
- query = (なし、 将来 `?axis=<name>` 等の subset selection を carry)

例: `persona-pack://bob/projections`

## Wire 側が定義する return JSON shape (ACL boundary)

```json
{
  "scheme": "persona-pack",
  "persona_id": "<id>",
  "projections": {
    "<axis>": {
      "template": "<string>",
      "target_form": "markdown" | "json" | "text" | "ascii",
      "merge_strategy": "replace" | "append" | "prepend" | "section"
    },
    ...
  }
}
```

persona-pack 上流の TOML field 名 (`target` / `strategy` 等) や `extra` table の階層は
本 adapter 内で吸収し、 上記 Wire 定義 shape に翻訳する (上流 drift 遮断)。

## Best-effort semantics

- persona 不在 / `[extra.persona_wire]` 不在 = empty `projections: {}` を返す
- read error も silent (= 上流 SDK の error type を core に漏らさず空 overlay 扱い)
- 「不存在 = 空」 path で wire_prompt_context Phase 0 の overlay fallback 仕様と整合

