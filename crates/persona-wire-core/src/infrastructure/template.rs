//! Template Engine Plugin trait — Plugin 軸 2 / 3。
//!
//! handlebars 既存実装を `HandlebarsEngine` として trait 実装に再パッケージし、
//! 外部 crate (例: `wire-template-jinja` / `wire-template-tera`) が同じ surface で
//! 差し込めるようにする。
//!
//! 既存 `rendering::render` free fn は本 trait の薄い `HandlebarsEngine` 委譲に
//! 移行済 (call site 維持、 後方互換)。
//!
//! ## Plugin author 視点
//!
//! ```ignore
//! use persona_wire_core::infrastructure::template::TemplateEngine;
//! use persona_wire_core::domain::error::WireResult;
//!
//! pub struct JinjaEngine { /* ... */ }
//!
//! impl TemplateEngine for JinjaEngine {
//!     fn id(&self) -> &'static str { "jinja" }
//!     fn render(&self, template: &str, context: &serde_json::Value) -> WireResult<String> {
//!         // minijinja::Environment で render
//!         todo!()
//!     }
//! }
//! ```

use crate::domain::error::WireResult;
use handlebars::{no_escape, Handlebars};

/// Template Engine Plugin。 1 engine = 1 impl (`handlebars` / `jinja` / `tera` 等)。
///
/// NamedProjection 側に登録される `template_engine` field と `id()` を突き合わせて
/// `PluginRegistry` が dispatch する (P3a では `id()` 登録のみ、 dispatch 配線は
/// use_cases 移行と並走で P3a 後段で実施)。
pub trait TemplateEngine: Send + Sync {
    /// engine 識別子 (`"handlebars"` / `"jinja"` / `"tera"` …)。
    fn id(&self) -> &'static str;

    /// `template` 文字列 + `context` (JSON) → rendered string。
    /// engine 固有の helper / partial 拡張は impl 内に閉じる。
    fn render(&self, template: &str, context: &serde_json::Value) -> WireResult<String>;
}

/// Core 同梱 default engine。 handlebars (Mustache superset) を薄くラップ。
///
/// behaviour:
/// - Scalar substitution: `{{key.path}}`
/// - Section iteration: `{{#each list}}…{{/each}}`
/// - Conditionals: `{{#if cond}}…{{/if}}`
/// - Missing paths: empty string (strict_mode=false)
/// - HTML escape: OFF (markdown/prompt/json/ascii 出力で `<` → `&lt;` 化を避ける)
/// - 解析/render error は `{{render-error: <msg>}} <template>` 形式で返す (panic しない)
#[derive(Default)]
pub struct HandlebarsEngine;

impl HandlebarsEngine {
    pub fn new() -> Self {
        Self
    }
}

impl TemplateEngine for HandlebarsEngine {
    fn id(&self) -> &'static str {
        "handlebars"
    }

    fn render(&self, template: &str, context: &serde_json::Value) -> WireResult<String> {
        let mut hb = Handlebars::new();
        hb.register_escape_fn(no_escape);
        hb.set_strict_mode(false);
        match hb.render_template(template, context) {
            Ok(s) => Ok(s),
            Err(e) => Ok(format!("{{{{render-error: {}}}}} {}", e, template)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handlebars_engine_id_is_handlebars() {
        assert_eq!(HandlebarsEngine::new().id(), "handlebars");
    }

    #[test]
    fn handlebars_engine_renders_scalar() {
        let eng = HandlebarsEngine::new();
        let out = eng
            .render("Hi, {{name}}!", &json!({"name": "alpha"}))
            .unwrap();
        assert_eq!(out, "Hi, alpha!");
    }

    #[test]
    fn handlebars_engine_renders_nested_path() {
        let eng = HandlebarsEngine::new();
        let out = eng
            .render(
                "Owner is {{owner.name}}.",
                &json!({"owner": {"name": "user_a"}}),
            )
            .unwrap();
        assert_eq!(out, "Owner is user_a.");
    }

    #[test]
    fn handlebars_engine_missing_path_renders_empty() {
        let eng = HandlebarsEngine::new();
        let out = eng
            .render("[{{absent}}]", &json!({"name": "alpha"}))
            .unwrap();
        assert_eq!(out, "[]");
    }
}
