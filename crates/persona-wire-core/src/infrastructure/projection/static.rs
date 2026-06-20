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
}
