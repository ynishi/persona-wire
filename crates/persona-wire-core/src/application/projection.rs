//! ProjectionRenderer Plugin trait — Plugin 軸 3 / 3。
//!
//! NamedProjection の「種別」を Plugin 化する。 default = `static` (template engine
//! で render するだけ、 現状 path)。 拡張 = `llm` (render 後 LLM で summarize)
//! / `code` (impl 関数で組み立て) / `cache` (memoize) 等。
//!
//! Renamed from `Projection` (P3a 当時) to `ProjectionRenderer` so the Domain
//! Entity [`crate::domain::entity::projection::Projection`] (Data Mapper land)
//! can own the unqualified name without an import collision.
//!
//! ## Plugin author 視点
//!
//! ```ignore
//! use persona_wire_core::application::projection::{ProjectionRenderer, ProjectionInput};
//! use persona_wire_core::domain::error::WireResult;
//!
//! pub struct LlmProjection { /* anthropic client 等 */ }
//!
//! impl ProjectionRenderer for LlmProjection {
//!     fn kind(&self) -> &'static str { "llm" }
//!     async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String> {
//!         let base = input.template_engine.render(input.template, input.spec_result)?;
//!         // base prompt を LLM に投げて summarize
//!         todo!()
//!     }
//! }
//! ```
//!
//! P3a 段階では trait + `StaticProjection` default impl のみ。 use_cases.rs の
//! 既存 dispatch (render free fn 直呼び) は維持、 PluginRegistry 経由 dispatch
//! 配線は P3a 後段 (NamedProjection schema 拡張と並走) で実施。

use crate::domain::entity::TargetForm;
use crate::domain::error::WireResult;
use crate::infrastructure::template::TemplateEngine;
use async_trait::async_trait;

/// ProjectionRenderer Plugin。 render 動作の種別 (`static` / `llm` / `code` / …) を Plugin 化。
///
/// dyn-compatible にするため `#[async_trait]` で `Pin<Box<Future>>` 化。
#[async_trait]
pub trait ProjectionRenderer: Send + Sync {
    /// projection 種別 id (`"static"` / `"llm"` / `"code"` / …)。
    /// NamedProjection 側の `projection_kind` field と一致するものに dispatch。
    fn kind(&self) -> &'static str;

    /// 入力 (spec 結果 + template + persona overlay + target_form) を受けて
    /// 最終 string を返す。 `StaticProjection` はそのまま `TemplateEngine` に流すだけ。
    async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String>;
}

/// Projection への入力束。 borrowed view で渡し、 lifetime 1 つで圧縮。
pub struct ProjectionInput<'a> {
    /// `wire_query` 相当の評価結果 (Adapter fetch 後 or graph 内部走査後の JSON)。
    pub spec_result: &'a serde_json::Value,
    /// overlay merge 済 template literal。
    pub template: &'a str,
    /// dispatch 済 template engine (`HandlebarsEngine` 等)。
    pub template_engine: &'a dyn TemplateEngine,
    /// 出力形式 (Prompt / Markdown / Json / Ascii)。
    pub target_form: TargetForm,
    /// 対象 persona id (overlay / config 切替に使う任意 hint)。
    pub persona_id: Option<&'a str>,
    /// projection 固有 config (LLM endpoint / cache TTL 等)。 schema は impl 側責務。
    pub config: &'a serde_json::Value,
}

/// Core 同梱 default projection。 `TemplateEngine` で render するだけの「静的」 form。
///
/// `wire_init` / `wire_render` / `wire_prompt_context` の既存 path と挙動 1:1 等価。
#[derive(Default)]
pub struct StaticProjection;

impl StaticProjection {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProjectionRenderer for StaticProjection {
    fn kind(&self) -> &'static str {
        "static"
    }

    async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String> {
        input
            .template_engine
            .render(input.template, input.spec_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::template::HandlebarsEngine;
    use serde_json::json;

    #[tokio::test]
    async fn static_projection_kind_is_static() {
        assert_eq!(StaticProjection::new().kind(), "static");
    }

    #[tokio::test]
    async fn static_projection_delegates_to_template_engine() {
        let eng = HandlebarsEngine::new();
        let data = json!({"name": "alpha"});
        let cfg = json!({});
        let input = ProjectionInput {
            spec_result: &data,
            template: "Hi, {{name}}!",
            template_engine: &eng,
            target_form: TargetForm::Prompt,
            persona_id: None,
            config: &cfg,
        };
        let out = StaticProjection::new().render(input).await.unwrap();
        assert_eq!(out, "Hi, alpha!");
    }
}
