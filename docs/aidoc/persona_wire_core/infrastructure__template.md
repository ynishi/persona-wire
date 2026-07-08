# persona-wire-core::infrastructure::template

Template Engine Plugin trait — Plugin 軸 2 / 3。

handlebars 既存実装を `HandlebarsEngine` として trait 実装に再パッケージし、
外部 crate (例: `wire-template-jinja` / `wire-template-tera`) が同じ surface で
差し込めるようにする。

既存 `rendering::render` free fn は本 trait の薄い `HandlebarsEngine` 委譲に
移行済 (call site 維持、 後方互換)。

## Plugin author 視点

```ignore
use persona_wire_core::infrastructure::template::TemplateEngine;
use persona_wire_core::domain::error::WireResult;

pub struct JinjaEngine { /* ... */ }

impl TemplateEngine for JinjaEngine {
    fn id(&self) -> &'static str { "jinja" }
    fn render(&self, template: &str, context: &serde_json::Value) -> WireResult<String> {
        // minijinja::Environment で render
        todo!()
    }
}
```

## Types

- `HandlebarsEngine` — Core 同梱 default engine。 handlebars (Mustache superset) を薄くラップ。

## Traits

- `TemplateEngine` — Template Engine Plugin。 1 engine = 1 impl (`handlebars` / `jinja` / `tera` 等)。

