# persona-wire-core::infrastructure::projection

Projection Adapters — concrete [`ProjectionRenderer`] implementations.

Default 同梱 = [`StaticProjection`] (`TemplateEngine` 委譲のみ)。 LLM /
MCP tool 等の OUT は別 crate の Adapter で同 trait を impl して差し込む。

[`ProjectionRenderer`]: crate::domain::port::ProjectionRenderer

