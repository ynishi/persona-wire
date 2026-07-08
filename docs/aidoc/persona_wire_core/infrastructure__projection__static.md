# persona-wire-core::infrastructure::projection::static

`StaticProjection` — core 同梱 default Adapter for [`ProjectionRenderer`].

`TemplateEngine` で render するだけの「静的」 form。 `wire_init` / `wire_render` /
`wire_prompt_context` の既存 path と挙動 1:1 等価。

## Hole-1 解消後の構造

以前は呼び出し側が [`ProjectionInput::template_engine`] で engine 参照を毎回
渡していたが、 Port 化に伴い `ProjectionInput` から engine field を除去した。
Adapter は constructor で `Arc<dyn TemplateEngine>` を hold し、 `render` 内で
自前の engine を使う。

[`ProjectionInput::template_engine`]: crate::domain::port::ProjectionInput

## Types

- `StaticProjection` — Core 同梱 default projection。

