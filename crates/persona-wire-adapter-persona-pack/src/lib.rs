//! `persona-wire-adapter-persona-pack` — persona-pack SDK access の ACL Facade。
//!
//! ## 役割
//!
//! persona-pack は persona の Pack TOML を扱う外部 crate。 そこに
//! `[extra.persona_wire.projections.<axis>]` という Wire 用 Overlay を書く運用がある
//! が、 上流 crate の API / TOML field を **persona-wire の core layer に漏らさない**
//! ように本 crate が境界として機能する (= ACL Facade)。
//!
//! ## Wire 側が定義する URI 形式
//!
//! - scheme = `persona-pack`
//! - host = `<persona_id>` (必須)
//! - path = `/projections` (現状唯一の resource、 将来分岐余地)
//! - query = (なし、 将来 `?axis=<name>` 等の subset selection を carry)
//!
//! 例: `persona-pack://dolly/projections`
//!
//! ## Wire 側が定義する return JSON shape (ACL boundary)
//!
//! ```json
//! {
//!   "scheme": "persona-pack",
//!   "persona_id": "<id>",
//!   "projections": {
//!     "<axis>": {
//!       "template": "<string>",
//!       "target_form": "markdown" | "json" | "text" | "ascii",
//!       "merge_strategy": "replace" | "append" | "prepend" | "section"
//!     },
//!     ...
//!   }
//! }
//! ```
//!
//! persona-pack 上流の TOML field 名 (`target` / `strategy` 等) や `extra` table の階層は
//! 本 adapter 内で吸収し、 上記 Wire 定義 shape に翻訳する (上流 drift 遮断)。
//!
//! ## Best-effort semantics
//!
//! - persona 不在 / `[extra.persona_wire]` 不在 = empty `projections: {}` を返す
//! - read error も silent (= 上流 SDK の error type を core に漏らさず空 overlay 扱い)
//! - 「不存在 = 空」 path で wire_prompt_context Phase 0 の overlay fallback 仕様と整合

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use persona_pack::PackRoot;
use persona_wire_core::domain::error::{WireError, WireResult};
use persona_wire_core::infrastructure::adapter::Adapter;
use persona_wire_core::infrastructure::wire_uri::WireUri;

/// persona-pack SDK 経由で persona Pack の `[extra.persona_wire.projections.<axis>]`
/// overlay table を取得し、 Wire 定義 JSON shape で返す Adapter。
///
/// インスタンス化は `PersonaPackAdapter::new(root)` または env-resolved
/// `PersonaPackAdapter::from_env()`。 root は persona-pack の root dir
/// (e.g. `~/persona-pack`)。
#[derive(Debug, Clone)]
pub struct PersonaPackAdapter {
    root: PathBuf,
}

impl PersonaPackAdapter {
    /// 明示 root を受けて adapter を構築。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// `PERSONA_PACK_ROOT` env、 未設定なら `~/persona-pack` を root として構築。
    /// `HOME` も解決不能なら `WireError::Storage`。
    pub fn from_env() -> WireResult<Self> {
        if let Ok(p) = std::env::var("PERSONA_PACK_ROOT") {
            if !p.is_empty() {
                return Ok(Self::new(PathBuf::from(p)));
            }
        }
        let home = std::env::var("HOME").map_err(|_| {
            WireError::Storage("persona-pack adapter: HOME unset".to_string())
        })?;
        Ok(Self::new(PathBuf::from(home).join("persona-pack")))
    }

    /// root への参照を返す (test / debug 用)。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl Adapter for PersonaPackAdapter {
    fn scheme(&self) -> &'static str {
        "persona-pack"
    }

    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        // host = persona_id (必須)。 path は `/projections` 想定だが、 現状は 1 resource
        // しかないので path mismatch は warning 出さず素通り (将来 `/extra-foo` 等を
        // 増やす時に path-based dispatch を追加する carry)。
        let persona_id = uri.host().ok_or_else(|| {
            WireError::Storage(format!(
                "persona-pack adapter: missing persona_id in uri host: {}",
                uri.as_raw()
            ))
        })?;
        if persona_id.is_empty() {
            return Err(WireError::Storage(format!(
                "persona-pack adapter: empty persona_id in uri: {}",
                uri.as_raw()
            )));
        }

        let projections = read_persona_projections(&self.root, persona_id);
        Ok(serde_json::json!({
            "scheme": "persona-pack",
            "persona_id": persona_id,
            "projections": projections,
        }))
    }
}

/// persona-pack `[extra.persona_wire.projections.<axis>]` table を読んで、
/// **Wire 定義 JSON shape の `projections` object** に翻訳して返す。
///
/// best-effort: persona 不在 / 階層 missing / parse fail はすべて空 object に倒す
/// (= ACL boundary で上流 error を遮断、 silent fallback)。
fn read_persona_projections(root: &Path, persona_id: &str) -> serde_json::Value {
    let pack = PackRoot::new(root.to_path_buf());
    let persona = match pack.read(persona_id) {
        Ok(p) => p,
        Err(_) => return serde_json::json!({}),
    };

    let Some(persona_wire) = persona.extra.get("persona_wire") else {
        return serde_json::json!({});
    };
    let Some(table) = persona_wire.as_table() else {
        return serde_json::json!({});
    };
    let Some(projections) = table.get("projections").and_then(|v| v.as_table()) else {
        return serde_json::json!({});
    };

    let mut out = serde_json::Map::new();
    for (axis, entry) in projections.iter() {
        let Some(e) = entry.as_table() else { continue };
        let Some(template) = e.get("template").and_then(|v| v.as_str()) else {
            continue;
        };
        // 上流 field 名 (`target` / `strategy`) を Wire 定義 (`target_form` / `merge_strategy`)
        // に rename する = ACL boundary での field 翻訳。
        let target_form = e
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown")
            .to_string();
        let merge_strategy = e
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("replace")
            .to_string();
        out.insert(
            axis.clone(),
            serde_json::json!({
                "template": template,
                "target_form": target_form,
                "merge_strategy": merge_strategy,
            }),
        );
    }
    serde_json::Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_is_persona_pack() {
        let a = PersonaPackAdapter::new("/nonexistent");
        assert_eq!(a.scheme(), "persona-pack");
    }

    #[tokio::test]
    async fn fetch_missing_persona_returns_empty_projections() {
        let a = PersonaPackAdapter::new("/nonexistent/persona-pack-root");
        let uri = WireUri::parse("persona-pack://__definitely_not_a_persona__/projections")
            .unwrap();
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["scheme"], "persona-pack");
        assert_eq!(v["persona_id"], "__definitely_not_a_persona__");
        assert!(v["projections"].is_object());
        assert_eq!(v["projections"].as_object().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn fetch_rejects_missing_persona_id_host() {
        let a = PersonaPackAdapter::new("/nonexistent");
        // scheme だけで host 不在 = WireUri 上は path-only URI として parse される。
        // host=None なので adapter 側で reject される。
        let uri = WireUri::parse("persona-pack:/projections").unwrap();
        let r = a.fetch(&uri).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("missing persona_id"));
    }

    #[tokio::test]
    async fn fetch_with_real_pack_extracts_overlay_table() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let persona_id = "shi";
        let persona_dir = tmp.path().join(persona_id);
        fs::create_dir_all(&persona_dir).unwrap();
        let toml = r#"
[meta]
id = "shi"
name = "shi"
origin = "hand"

[prompt]
body = "test"

[extra.persona_wire.projections.active]
template = "ACTIVE OVERLAY"
target = "markdown"
strategy = "append"

[extra.persona_wire.projections.bio]
template = "BIO OVERLAY"
"#;
        fs::write(persona_dir.join("prompt.toml"), toml).unwrap();

        let a = PersonaPackAdapter::new(tmp.path());
        let uri = WireUri::parse(&format!("persona-pack://{persona_id}/projections"))
            .unwrap();
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["scheme"], "persona-pack");
        assert_eq!(v["persona_id"], persona_id);

        let projections = v["projections"].as_object().unwrap();
        assert_eq!(projections.len(), 2);

        // active: 全 field 指定済
        assert_eq!(projections["active"]["template"], "ACTIVE OVERLAY");
        assert_eq!(projections["active"]["target_form"], "markdown");
        assert_eq!(projections["active"]["merge_strategy"], "append");

        // bio: target / strategy 不在 → default 値 (markdown / replace) に倒れる
        assert_eq!(projections["bio"]["template"], "BIO OVERLAY");
        assert_eq!(projections["bio"]["target_form"], "markdown");
        assert_eq!(projections["bio"]["merge_strategy"], "replace");
    }
}
