//! PluginRegistry — 3 軸 Plugin (Adapter / TemplateEngine / Projection) を統合管理。
//!
//! server boot 時に register、 runtime mutation なし (= immutable after `build()`)。
//! Plugin の物理境界は外部 crate (例: `wire-adapter-pg` / `wire-template-jinja` /
//! `wire-projection-llm`)、 boot 側 (`persona-wire-mcp` / `persona-wire` bin) で
//! `PluginRegistry::builder()` に流し込んで構築する。
//!
//! ## boot 例
//!
//! ```ignore
//! use persona_wire_core::application::plugin_registry::PluginRegistry;
//! use persona_wire_core::infrastructure::adapter::FileAdapter;
//! use persona_wire_core::infrastructure::template::HandlebarsEngine;
//! use persona_wire_core::application::projection::StaticProjection;
//! use persona_wire_adapter_mini_app::MiniAppAdapter;
//!
//! let registry = PluginRegistry::default_builder_for_wire()
//!     .with_adapter(MiniAppAdapter)
//!     .build()
//!     .expect("plugin registry build");
//! ```
//!
//! P3a stage: registry skeleton + builder + lookup surface のみ。 use_cases.rs
//! 側の dispatch 配線 (registry を引数で受け取って fetch / render を引く form)
//! は P3a 後段で順次差し替え (現状は free fn `fetch_via_adapter` + `rendering::render`
//! 直呼びを維持、 後方互換)。

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::application::projection::ProjectionRenderer;
use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::adapter::Adapter;
use crate::infrastructure::template::TemplateEngine;
use crate::infrastructure::wire_uri::WireUri;

/// 3 軸 Plugin を統合管理する immutable registry。
///
/// build 後の mutation 不可。 dispatch は scheme / id / kind 文字列引きで O(1)。
#[derive(Clone, Default)]
pub struct PluginRegistry {
    adapters: HashMap<&'static str, Arc<dyn Adapter>>,
    engines: HashMap<&'static str, Arc<dyn TemplateEngine>>,
    projections: HashMap<&'static str, Arc<dyn ProjectionRenderer>>,
}

impl fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("schemes", &self.schemes())
            .field("engine_ids", &self.engine_ids())
            .field("projection_kinds", &self.projection_kinds())
            .finish()
    }
}

impl PluginRegistry {
    /// builder pattern 入口。
    pub fn builder() -> PluginRegistryBuilder {
        PluginRegistryBuilder::default()
    }

    /// Core 同梱 plugin の builder を返す convenience 関数。
    ///
    /// P3b で mini-app adapter が外部 crate (`persona-wire-adapter-mini-app`) に分離
    /// されたため、 consumer (`persona-wire-mcp` / `persona-wire` bin) は本 builder に
    /// `.with_adapter(MiniAppAdapter)` を chain して使う。
    ///
    /// 同梱内訳 (core):
    /// - `FileAdapter` (scheme `"file"`)
    /// - `HandlebarsEngine` (id `"handlebars"`)
    /// - `StaticProjection` (kind `"static"`)
    pub fn default_builder_for_wire() -> PluginRegistryBuilder {
        use crate::application::projection::StaticProjection;
        use crate::infrastructure::adapter::FileAdapter;
        use crate::infrastructure::template::HandlebarsEngine;
        Self::builder()
            .with_adapter(FileAdapter)
            .with_engine(HandlebarsEngine::new())
            .with_projection(StaticProjection::new())
    }

    /// Core 同梱 plugin のみで registry を build する shortcut。
    /// mini-app scheme を含めたい場合は [`default_builder_for_wire`] を使い、
    /// caller 側で `MiniAppAdapter` を chain すること。
    pub fn default_for_wire() -> WireResult<Self> {
        Self::default_builder_for_wire().build()
    }

    /// `source_uri` の scheme prefix に該当する adapter を引く (parse なし、 lookup のみ)。
    /// 未登録 scheme は `None`。
    ///
    /// Adapter の `fetch` を呼ぶ場合は [`route`](Self::route) を使うこと
    /// (parse + lookup を 1 箇所に集約する canonical 経路)。
    pub fn adapter_for_uri(&self, source_uri: &str) -> Option<&Arc<dyn Adapter>> {
        let scheme = source_uri.split_once(':').map(|(s, _)| s)?;
        self.adapters.get(scheme)
    }

    /// URI grammar parse + scheme dispatch を 1 step で行う canonical entry。
    ///
    /// 返り値の `(adapter, WireUri)` をそのまま `adapter.fetch(&uri).await` に流せる。
    /// scheme 未登録は `WireError::Storage` (Adapter trait の `fetch` 失敗と同 error 軸)。
    pub fn route(&self, source_uri: &str) -> WireResult<(Arc<dyn Adapter>, WireUri)> {
        let uri = WireUri::parse(source_uri)?;
        let adapter = self.adapters.get(uri.scheme()).cloned().ok_or_else(|| {
            WireError::Storage(format!(
                "plugin registry: no adapter registered for scheme `{}` (uri: {})",
                uri.scheme(),
                source_uri,
            ))
        })?;
        Ok((adapter, uri))
    }

    /// scheme literal から adapter を引く。
    pub fn adapter(&self, scheme: &str) -> Option<&Arc<dyn Adapter>> {
        self.adapters.get(scheme)
    }

    /// engine id から template engine を引く。
    pub fn engine(&self, id: &str) -> Option<&Arc<dyn TemplateEngine>> {
        self.engines.get(id)
    }

    /// kind id から projection を引く。
    pub fn projection(&self, kind: &str) -> Option<&Arc<dyn ProjectionRenderer>> {
        self.projections.get(kind)
    }

    /// 登録済 scheme 一覧 (`wire_doctor` 表示用)。
    pub fn schemes(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.adapters.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// 登録済 engine id 一覧。
    pub fn engine_ids(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.engines.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// 登録済 projection kind 一覧。
    pub fn projection_kinds(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.projections.keys().copied().collect();
        v.sort_unstable();
        v
    }
}

/// builder。 同一 scheme / id / kind の重複登録は `build()` 時に fail-fast。
#[derive(Default)]
pub struct PluginRegistryBuilder {
    adapters: Vec<Arc<dyn Adapter>>,
    engines: Vec<Arc<dyn TemplateEngine>>,
    projections: Vec<Arc<dyn ProjectionRenderer>>,
}

impl PluginRegistryBuilder {
    pub fn with_adapter<A: Adapter + 'static>(mut self, adapter: A) -> Self {
        self.adapters.push(Arc::new(adapter));
        self
    }

    pub fn with_engine<E: TemplateEngine + 'static>(mut self, engine: E) -> Self {
        self.engines.push(Arc::new(engine));
        self
    }

    pub fn with_projection<P: ProjectionRenderer + 'static>(mut self, projection: P) -> Self {
        self.projections.push(Arc::new(projection));
        self
    }

    /// fail-fast: 同一 scheme / id / kind が複数あれば error。
    pub fn build(self) -> WireResult<PluginRegistry> {
        let mut adapters = HashMap::new();
        for a in self.adapters {
            let scheme = a.scheme();
            if adapters.insert(scheme, a).is_some() {
                return Err(WireError::Storage(format!(
                    "plugin registry: duplicate adapter scheme `{scheme}`"
                )));
            }
        }
        let mut engines = HashMap::new();
        for e in self.engines {
            let id = e.id();
            if engines.insert(id, e).is_some() {
                return Err(WireError::Storage(format!(
                    "plugin registry: duplicate template engine id `{id}`"
                )));
            }
        }
        let mut projections = HashMap::new();
        for p in self.projections {
            let kind = p.kind();
            if projections.insert(kind, p).is_some() {
                return Err(WireError::Storage(format!(
                    "plugin registry: duplicate projection kind `{kind}`"
                )));
            }
        }
        Ok(PluginRegistry {
            adapters,
            engines,
            projections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::projection::StaticProjection;
    use crate::infrastructure::adapter::FileAdapter;
    use crate::infrastructure::template::HandlebarsEngine;

    #[test]
    fn empty_registry_has_no_plugins() {
        let reg = PluginRegistry::builder().build().unwrap();
        assert!(reg.schemes().is_empty());
        assert!(reg.engine_ids().is_empty());
        assert!(reg.projection_kinds().is_empty());
    }

    #[test]
    fn registers_all_three_axes() {
        let reg = PluginRegistry::builder()
            .with_adapter(FileAdapter)
            .with_engine(HandlebarsEngine::new())
            .with_projection(StaticProjection::new())
            .build()
            .unwrap();
        assert_eq!(reg.schemes(), vec!["file"]);
        assert_eq!(reg.engine_ids(), vec!["handlebars"]);
        assert_eq!(reg.projection_kinds(), vec!["static"]);
    }

    #[test]
    fn default_builder_for_wire_has_core_plugins_only() {
        let reg = PluginRegistry::default_builder_for_wire().build().unwrap();
        assert_eq!(reg.schemes(), vec!["file"]);
        assert_eq!(reg.engine_ids(), vec!["handlebars"]);
        assert_eq!(reg.projection_kinds(), vec!["static"]);
    }

    #[test]
    fn adapter_for_uri_dispatches_by_scheme() {
        let reg = PluginRegistry::builder()
            .with_adapter(FileAdapter)
            .build()
            .unwrap();
        assert!(reg.adapter_for_uri("file:///tmp/x").is_some());
        assert!(reg.adapter_for_uri("mini-app://x").is_none());
        assert!(reg.adapter_for_uri("no-scheme").is_none());
    }

    #[test]
    fn duplicate_scheme_fails_build() {
        let err = PluginRegistry::builder()
            .with_adapter(FileAdapter)
            .with_adapter(FileAdapter)
            .build()
            .unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("duplicate adapter scheme"));
        assert!(msg.contains("file"));
    }

    #[test]
    fn duplicate_engine_fails_build() {
        let err = PluginRegistry::builder()
            .with_engine(HandlebarsEngine::new())
            .with_engine(HandlebarsEngine::new())
            .build()
            .unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("duplicate template engine id"));
    }

    #[test]
    fn duplicate_projection_fails_build() {
        let err = PluginRegistry::builder()
            .with_projection(StaticProjection::new())
            .with_projection(StaticProjection::new())
            .build()
            .unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("duplicate projection kind"));
    }
}
