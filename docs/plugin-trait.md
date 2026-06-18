# Plugin Trait — canonical SoT

> Status: **P3a (trait + registry skeleton landed)**.
> Trait surface はこの 3 つで固定。method 追加は minor bump、 break は major bump。
> use_cases.rs 側の dispatch 配線 (PluginRegistry 経由) は P3a 後段で順次差し替え。

## 1. Why — Platform 中立 / Infra 路線

persona-wire は persona × SoT × workflow context routing の **配線層 (Wire)** に
徹する Infra crate。 Platform (Claude Desktop / Cursor / Open WebUI / 自前 host …)
は persona Runtime + adapter を**同梱して囲い込みたい**動機を持つが、 ユーザー
視点では:

- persona は user の所有物 (persona-pack で portable に記述済)
- SoT は user が選ぶ (mini-app / pg / vector / file / HTTP REST / …)
- Wire 自体はどの Platform でも動く Infra であるべき

= **Plugin 拡張面を最初から開いておく**ことが Wire の design rationale。 3 軸
(SoT Adapter / Template Engine / Projection) は DDD + Hexagonal の境界 4 層
(Domain / Application / Infrastructure / Surface) の Infrastructure / Application
境界に乗せた。

GenAI 黎明期の framework は結局 Plugin / Integration 拡張面でしか戦えなかった
経験則 (LangChain / LlamaIndex 系) を踏襲。 Wire は最初から Plugin 軸で書いて
いる。

## 2. 3 軸 Plugin Surface

### 2.1 SoT Adapter — `infrastructure::adapter::Adapter`

URI scheme 1 つにつき 1 impl。 `<scheme>://...` を JSON に変換するだけの最小
責務。 Node 化 (row → Node mapping) は caller (use case 層) が行う。

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    /// このアダプタが扱う URI scheme 識別子 (例: "mini-app" / "file" / "pg")
    fn scheme(&self) -> &'static str;

    /// `<scheme>://...` 全体を受け取り JSON を返す
    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value>;
}
```

**Core 同梱**:

- `FileAdapter` (scheme `"file"`) — `file://...` or `file:...`
- `MiniAppAdapter` (scheme `"mini-app"`) — `mini-app://<table>?scope=&root=&alias=&limit=&...`

**外部 Plugin 例**:

```rust
pub struct PgAdapter { pool: PgPool }

#[async_trait]
impl Adapter for PgAdapter {
    fn scheme(&self) -> &'static str { "pg" }
    async fn fetch(&self, uri: &str) -> WireResult<Value> {
        // pg://<dsn>?query=<sql>&params=...
        todo!()
    }
}
```

### 2.2 Template Engine — `infrastructure::template::TemplateEngine`

1 engine = 1 impl。 NamedProjection 側の `template_engine` field と `id()` を
突き合わせて dispatch。

```rust
pub trait TemplateEngine: Send + Sync {
    /// engine 識別子 ("handlebars" / "jinja" / "tera" …)
    fn id(&self) -> &'static str;

    /// template + context (JSON) → rendered string
    fn render(&self, template: &str, context: &serde_json::Value) -> WireResult<String>;
}
```

**Core 同梱**: `HandlebarsEngine` (id `"handlebars"`) — Mustache superset + section
iteration + conditionals + dotted path + missing path = "" + HTML escape OFF。

**外部 Plugin 例**:

```rust
pub struct JinjaEngine { env: minijinja::Environment<'static> }

impl TemplateEngine for JinjaEngine {
    fn id(&self) -> &'static str { "jinja" }
    fn render(&self, t: &str, c: &Value) -> WireResult<String> { todo!() }
}
```

### 2.3 Projection — `application::projection::Projection`

NamedProjection の「種別」 を Plugin 化。 default = `static` (TemplateEngine
委譲、 現状 path)。 拡張 = `llm` / `code` / `cache` 等。

```rust
#[async_trait]
pub trait Projection: Send + Sync {
    /// 種別 id ("static" / "llm" / "code" / …)
    fn kind(&self) -> &'static str;

    /// spec_result + template + persona overlay + target_form → 最終 string
    async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String>;
}

pub struct ProjectionInput<'a> {
    pub spec_result: &'a serde_json::Value,
    pub template: &'a str,
    pub template_engine: &'a dyn TemplateEngine,
    pub target_form: TargetForm,            // Prompt / Markdown / Json / Ascii
    pub persona_id: Option<&'a str>,
    pub config: &'a serde_json::Value,      // projection 固有 (LLM endpoint 等)
}
```

**Core 同梱**: `StaticProjection` (kind `"static"`) — `TemplateEngine` に委譲
するだけ。 既存 `wire_init` / `wire_render` / `wire_prompt_context` と挙動 1:1
等価。

**外部 Plugin 例**:

```rust
pub struct LlmProjection { client: anthropic::Client }

#[async_trait]
impl Projection for LlmProjection {
    fn kind(&self) -> &'static str { "llm" }
    async fn render(&self, input: ProjectionInput<'_>) -> WireResult<String> {
        let base = input.template_engine.render(input.template, input.spec_result)?;
        self.client.summarize(&base, input.config).await
    }
}
```

## 3. PluginRegistry — `application::plugin_registry::PluginRegistry`

3 軸を統合管理する **immutable registry**。 server boot 時に register、 runtime
mutation なし (= `build()` 後は不変)。

```rust
let registry = PluginRegistry::builder()
    .with_adapter(FileAdapter)                  // core builtin
    .with_adapter(MiniAppAdapter)               // core builtin
    .with_adapter(PgAdapter::new(pool))         // 外部 crate
    .with_engine(HandlebarsEngine::new())       // core builtin
    .with_engine(JinjaEngine::new())            // 外部 crate
    .with_projection(StaticProjection::new())   // core builtin (default)
    .with_projection(LlmProjection::new(...))   // 外部 crate
    .build()?;                                  // 重複 scheme/id/kind は fail-fast
```

**Lookup surface** (immutable):

- `registry.adapter_for_uri("file:///tmp/x")` → `Option<&Arc<dyn Adapter>>`
- `registry.adapter("file")` → 同上 (scheme literal 指定)
- `registry.engine("handlebars")` → `Option<&Arc<dyn TemplateEngine>>`
- `registry.projection("static")` → `Option<&Arc<dyn Projection>>`
- `registry.schemes()` / `engine_ids()` / `projection_kinds()` → `wire_doctor` 表示用

**Build-time validation**: 同一 `scheme()` / `id()` / `kind()` が複数登録された場合
`build()` が `WireError::Storage("plugin registry: duplicate ...")` を返す
(fail-fast)。

## 4. NamedProjection Schema 拡張 (P3a 後段)

P3a Phase 1 (本 land) は trait + registry skeleton のみ。 NamedProjection schema
への field 追加と use_cases 配線は Phase 2 で実施 (後方互換維持):

```jsonc
{
  "id": "shi_workflow_context",
  "spec_ref": "...",
  "template": "Hello {{persona.name}}, today: {{trigger.name}}",
  "template_engine": "handlebars",   // ← Phase 2 新規 (省略時 "handlebars")
  "projection_kind": "static",        // ← Phase 2 新規 (省略時 "static")
  "projection_config": {},            // ← Phase 2 新規 (projection 固有 config)
  "target_form": "Prompt"
}
```

既存 NamedProjection (3 field 不在) は省略時 default で動く = migration 不要。

## 5. 外部 crate 作り方 (walkthrough: `wire-adapter-pg`)

```toml
# Cargo.toml
[package]
name = "wire-adapter-pg"
version = "0.1.0"
edition = "2021"

[dependencies]
persona-wire-core = "0.3"   # P3a land 後の version
async-trait = "0.1"
serde_json = "1"
sqlx = { version = "0.8", features = ["postgres", "runtime-tokio"] }
url = "2"
```

```rust
// src/lib.rs
use async_trait::async_trait;
use persona_wire_core::domain::error::{WireError, WireResult};
use persona_wire_core::infrastructure::adapter::Adapter;
use sqlx::PgPool;

pub struct PgAdapter { pool: PgPool }

impl PgAdapter {
    pub fn new(pool: PgPool) -> Self { Self { pool } }
}

#[async_trait]
impl Adapter for PgAdapter {
    fn scheme(&self) -> &'static str { "pg" }

    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value> {
        let url = url::Url::parse(source_uri)
            .map_err(|e| WireError::Storage(format!("pg adapter: {e}")))?;
        // url.query() から SQL + params を取り出して self.pool で実行
        // …
        todo!()
    }
}
```

```rust
// boot 側 (persona-wire bin の main.rs 等)
let registry = PluginRegistry::builder()
    .with_adapter(FileAdapter)
    .with_adapter(MiniAppAdapter)
    .with_adapter(PgAdapter::new(pool))         // ← 外部 crate 投入
    .with_engine(HandlebarsEngine::new())
    .with_projection(StaticProjection::new())
    .build()?;
```

## 6. Stability Policy

| 変更 | semver |
|---|---|
| trait に新 method (default impl 付き) を追加 | minor bump |
| trait の既存 method 形を変更 / 削除 | **major bump** |
| `ProjectionInput<'a>` に新 field 追加 (pub) | minor bump (struct 直接構築する caller は影響受けるため future-proof 化検討) |
| 新 Plugin 軸を追加 | minor bump |
| Core 同梱 impl (`FileAdapter` / `MiniAppAdapter` / `HandlebarsEngine` / `StaticProjection`) を crate 外に切り出し | minor bump (feature gate で後方互換) |

P3b で `MiniAppAdapter` を `wire-adapter-mini-app` 別 crate に切り出す予定
(default features から外す)。 同じ semver policy で minor bump。

## 7. Done Criteria

- **P3a Phase 1 (本 land)**:
  - [x] `Adapter::scheme()` 追加 + 既存 2 impl 対応
  - [x] `TemplateEngine` trait + `HandlebarsEngine` default impl
  - [x] `Projection` trait + `StaticProjection` default impl + `ProjectionInput<'a>`
  - [x] `PluginRegistry` + builder + fail-fast 重複検査
  - [x] cargo check / test (新規 6 件含む 165 件) / clippy / fmt 全 PASS
  - [x] 本 doc (canonical SoT) 起稿

- **P3a Phase 2 (carry)**:
  - [ ] NamedProjection schema に `template_engine` / `projection_kind` / `projection_config` field 追加
  - [ ] use_cases.rs の dispatch を PluginRegistry 経由に書き換え (`fetch_via_adapter` 直呼び廃止)
  - [ ] `wire_doctor` 出力に `schemes` / `engine_ids` / `projection_kinds` 追加

- **P3a Phase 3 (外部 crate 動作証明)**:
  - [ ] `wire-adapter-pg` 1 件試作 → smoke 通過 = P3a 完了

## 関連

- `crates/persona-wire-core/src/infrastructure/adapter.rs` (Adapter trait + 2 builtin impl)
- `crates/persona-wire-core/src/infrastructure/template.rs` (TemplateEngine trait + HandlebarsEngine)
- `crates/persona-wire-core/src/application/projection.rs` (Projection trait + StaticProjection)
- `crates/persona-wire-core/src/application/plugin_registry.rs` (PluginRegistry + builder)
