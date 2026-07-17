//! `StaticProjection` — core 同梱 default Adapter for [`ProjectionRenderer`].
//!
//! `TemplateEngine` で render するだけの「静的」 form。 `wire_init` / `wire_render` /
//! `wire_prompt_context` の既存 path と挙動 1:1 等価。
//!
//! ## Hole-1 解消後の構造
//!
//! 以前は呼び出し側が [`ProjectionInput::template_engine`] で engine 参照を毎回
//! 渡していたが、 Port 化に伴い `ProjectionInput` から engine field を除去した。
//! Adapter は constructor で `Arc<dyn TemplateEngine>` を hold し、 `render` 内で
//! 自前の engine を使う。
//!
//! [`ProjectionInput::template_engine`]: crate::domain::port::ProjectionInput

use std::sync::Arc;

use crate::domain::error::WireResult;
use crate::domain::port::{ProjectionInput, ProjectionRenderer};
use crate::infrastructure::template::{HandlebarsEngine, TemplateEngine};
use async_trait::async_trait;

/// Core 同梱 default projection。
///
/// 構築時に `Arc<dyn TemplateEngine>` を hold し、 `render` でその engine に
/// `template` + `spec_result` を流すだけ。
pub struct StaticProjection {
    engine: Arc<dyn TemplateEngine>,
}

impl StaticProjection {
    /// 任意の `TemplateEngine` を hold する constructor。
    pub fn with_engine(engine: Arc<dyn TemplateEngine>) -> Self {
        Self { engine }
    }

    /// `HandlebarsEngine` (Core default) を hold する shortcut。
    pub fn new() -> Self {
        Self {
            engine: Arc::new(HandlebarsEngine::new()),
        }
    }
}

impl Default for StaticProjection {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProjectionRenderer for StaticProjection {
    fn kind(&self) -> &'static str {
        "static"
    }

    async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String> {
        self.engine.render(input.template, input.spec_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::entity::TargetForm;
    use serde_json::json;

    #[tokio::test]
    async fn static_projection_kind_is_static() {
        assert_eq!(StaticProjection::new().kind(), "static");
    }

    #[tokio::test]
    async fn static_projection_delegates_to_template_engine() {
        let proj = StaticProjection::new();
        let data = json!({"name": "alpha"});
        let cfg = json!({});
        let input = ProjectionInput {
            spec_result: &data,
            template: "Hi, {{name}}!",
            target_form: TargetForm::Prompt,
            persona_id: None,
            config: &cfg,
        };
        let out = proj.render(input).await.unwrap();
        assert_eq!(out, "Hi, alpha!");
    }

    #[tokio::test]
    async fn static_projection_engine_error_surfaces_as_render_error_marker() {
        // 現状仕様: HandlebarsEngine は template syntax error を WireResult::Err で
        // 返さず、 Ok 内に `{{render-error: ...}}` marker を埋めて返す。 これを
        // pin する spec test (今後 Err 経路化する設計判断が出るまでの behavior 固定)。
        let proj = StaticProjection::new();
        let data = json!({});
        let cfg = json!({});
        let input = ProjectionInput {
            spec_result: &data,
            template: "{{#if unterminated",
            target_form: TargetForm::Prompt,
            persona_id: None,
            config: &cfg,
        };
        let out = proj
            .render(input)
            .await
            .expect("engine returns Ok marker, not Err");
        assert!(
            out.contains("{{render-error:"),
            "expected render-error marker, got: {out}"
        );
    }

    #[tokio::test]
    async fn static_projection_accepts_optional_persona_id_through_input() {
        // persona_id は static path では使われないが Port IF 上は受け取れる必要がある。
        // Some/None 両 path で構築 + render 成功することを観測。
        let proj = StaticProjection::new();
        let data = json!({"k": 1});
        let cfg = json!({});
        let with_persona = ProjectionInput {
            spec_result: &data,
            template: "v={{k}}",
            target_form: TargetForm::Markdown,
            persona_id: Some("alice"),
            config: &cfg,
        };
        assert_eq!(proj.render(with_persona).await.unwrap(), "v=1");
        let without = ProjectionInput {
            spec_result: &data,
            template: "v={{k}}",
            target_form: TargetForm::Markdown,
            persona_id: None,
            config: &cfg,
        };
        assert_eq!(proj.render(without).await.unwrap(), "v=1");
    }

    // -- Port 契約 verification: 2nd impl ----------------------------------
    //
    // `ProjectionRenderer` は Domain が宣言する Driven Port。 Hexagonal の正味は
    // 「同 trait を満たす別 Adapter が差し込める」 ことなので、 test 専用 mock を
    // もう 1 つ実装して dyn-compatibility と kind() dispatch 識別子の独立性を確認する。

    /// Test-only mock: input.template を逆順返却 + kind="mock" を返す。
    /// Adapter として trait を満たすだけの最小実装。
    struct ReverseMockProjection;

    #[async_trait]
    impl ProjectionRenderer for ReverseMockProjection {
        fn kind(&self) -> &'static str {
            "mock"
        }
        async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String> {
            Ok(input.template.chars().rev().collect())
        }
    }

    #[tokio::test]
    async fn second_renderer_impl_satisfies_port_contract() {
        let mock = ReverseMockProjection;
        assert_eq!(mock.kind(), "mock");
        let data = json!({});
        let cfg = json!({});
        let input = ProjectionInput {
            spec_result: &data,
            template: "abc",
            target_form: TargetForm::Json,
            persona_id: None,
            config: &cfg,
        };
        assert_eq!(mock.render(input).await.unwrap(), "cba");
    }

    #[tokio::test]
    async fn port_is_dyn_compatible_across_implementations() {
        // dyn ProjectionRenderer を 2 impl で hold できる = Port が hexagonal の
        // 差し込み点として機能している (object safety + Send + Sync 維持)。
        let renderers: Vec<Arc<dyn ProjectionRenderer>> = vec![
            Arc::new(StaticProjection::new()),
            Arc::new(ReverseMockProjection),
        ];
        let kinds: Vec<&'static str> = renderers.iter().map(|r| r.kind()).collect();
        assert_eq!(kinds, vec!["static", "mock"]);

        // kind() で dispatch 先を識別できる (NamedProjection.projection_kind との対応軸)。
        let data = json!({"x": 7});
        let cfg = json!({});
        for r in &renderers {
            let input = ProjectionInput {
                spec_result: &data,
                template: "tmpl",
                target_form: TargetForm::Prompt,
                persona_id: None,
                config: &cfg,
            };
            // 両 impl の戻り値型 + WireResult contract が trait 経由で揃う。
            let _ = r.render(input).await.unwrap();
        }
    }
}
