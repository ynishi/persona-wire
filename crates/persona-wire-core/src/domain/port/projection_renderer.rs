//! `ProjectionRenderer` — Driven Port for projection rendering.
//!
//! Domain が宣言する「Projection (= Wire の OUT 表面) をどう render するか」 の
//! 契約 IF。 default static path は [`crate::infrastructure::projection::StaticProjection`]
//! が実装し、 MCP / その他の OUT 経路は同 trait を impl する別 Adapter が担う。
//!
//! ## なぜ Domain Port なのか
//!
//! Projection 自体が Domain Entity ([`crate::domain::entity::projection::Projection`])
//! であり、 「どの Source から取った data を どう render するか」 の方向性は
//! Domain Logic に属する。 したがって render 契約も Domain が握り、 具象実装は
//! Adapter (Infrastructure) が hexagonal の OUT 側で差し込む。
//!
//! ## Hole-1 解消: `TemplateEngine` 依存の排除
//!
//! 以前は [`ProjectionInput`] が `&dyn TemplateEngine` を持ち、 Port が Infrastructure
//! 型へ依存していた (Domain → Infrastructure 違反)。 Adapter 側で `TemplateEngine` を
//! hold する形に signature を改修し、 Port 入力からは engine を排除した。

use crate::domain::entity::TargetForm;
use crate::domain::error::WireResult;
use async_trait::async_trait;

/// `ProjectionRenderer` Plugin。 render 動作の種別 (`static` / `mcp_tool` 等) を Plugin 化。
///
/// dyn-compatible にするため `#[async_trait]` で `Pin<Box<Future>>` 化。
#[async_trait]
pub trait ProjectionRenderer: Send + Sync {
    /// projection 種別 id (`"static"` / `"mcp_tool"` / …)。
    /// NamedProjection 側の `projection_kind` field と一致するものに dispatch。
    fn kind(&self) -> &'static str;

    /// 入力 (spec 結果 + template + overlay) を受けて最終 string を返す。
    /// `TemplateEngine` 等の技術依存資源は impl 側が hold する。
    async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String>;
}

/// Projection への入力束。 borrowed view で渡し、 lifetime 1 つで圧縮。
///
/// Infrastructure 型 (`TemplateEngine` 等) への参照は持たない — Domain Port の
/// 入力束として技術依存ゼロを保つ (Hole-1 解消)。
pub struct ProjectionInput<'a> {
    /// `wire_query` 相当の評価結果 (Adapter fetch 後 or graph 内部走査後の JSON)。
    pub spec_result: &'a serde_json::Value,
    /// overlay merge 済 template literal。
    pub template: &'a str,
    /// 出力形式 (Prompt / Markdown / Json / Ascii)。
    pub target_form: TargetForm,
    /// 対象 persona id (overlay / config 切替に使う任意 hint)。
    pub persona_id: Option<&'a str>,
    /// projection 固有 config (LLM endpoint / cache TTL 等)。 schema は impl 側責務。
    pub config: &'a serde_json::Value,
}
