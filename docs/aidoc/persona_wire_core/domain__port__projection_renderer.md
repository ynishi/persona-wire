# persona-wire-core::domain::port::projection_renderer

`ProjectionRenderer` — Driven Port for projection rendering.

Domain が宣言する「Projection (= Wire の OUT 表面) をどう render するか」 の
契約 IF。 default static path は [`crate::infrastructure::projection::StaticProjection`]
が実装し、 MCP / その他の OUT 経路は同 trait を impl する別 Adapter が担う。

## なぜ Domain Port なのか

Projection 自体が Domain Entity ([`crate::domain::entity::projection::Projection`])
であり、 「どの Source から取った data を どう render するか」 の方向性は
Domain Logic に属する。 したがって render 契約も Domain が握り、 具象実装は
Adapter (Infrastructure) が hexagonal の OUT 側で差し込む。

## Hole-1 解消: `TemplateEngine` 依存の排除

以前は [`ProjectionInput`] が `&dyn TemplateEngine` を持ち、 Port が Infrastructure
型へ依存していた (Domain → Infrastructure 違反)。 Adapter 側で `TemplateEngine` を
hold する形に signature を改修し、 Port 入力からは engine を排除した。

## Types

- `ProjectionInput` — Projection への入力束。 borrowed view で渡し、 lifetime 1 つで圧縮。

## Traits

- `ProjectionRenderer` — `ProjectionRenderer` Plugin。 render 動作の種別 (`static` / `mcp_tool` 等) を Plugin 化。

