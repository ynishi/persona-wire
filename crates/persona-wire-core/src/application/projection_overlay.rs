//! Projection Overlay — Wire の domain 型 (上流 SoT 非依存)。
//!
//! `wire_prompt_context` Phase 0 で各 axis の base template に被せる Overlay 情報。
//! 取得経路は `PluginRegistry::route("<scheme>://<persona_id>/projections")` 経由の
//! Adapter dispatch (= ACL Facade、 上流 SDK 固有の TOML field / Type を Wire に漏らさない)。
//!
//! Wire 定義の return JSON shape (Adapter が返す側の契約):
//!
//! ```json
//! {
//!   "scheme": "<adapter-scheme>",
//!   "persona_id": "<id>",
//!   "projections": {
//!     "<axis>": {
//!       "template": "<string>",
//!       "target_form": "markdown" | "json" | "text" | "ascii",
//!       "merge_strategy": "replace" | "append" | "prepend" | "section"
//!     }
//!   }
//! }
//! ```
//!
//! `target_form` / `merge_strategy` の値は文字列で受け取り、 Wire の typed enum
//! (`TargetForm` / `MergeStrategy`) に parse する。 未知値は default (markdown / replace)。

use std::collections::BTreeMap;

use crate::application::merger::MergeStrategy;
use crate::application::projection_registry::TargetForm;
use crate::domain::error::WireResult;

/// 1 axis 分の Overlay 解決結果 (Wire domain 型)。
#[derive(Debug, Clone)]
pub struct ProjectionOverlay {
    pub template: String,
    pub target_form: TargetForm,
    pub strategy: MergeStrategy,
}

/// Adapter return JSON を `BTreeMap<axis, ProjectionOverlay>` に翻訳する。
///
/// best-effort: `projections` 不在 / 形不正は空 map (= 「overlay 無し」 として扱う)。
/// Wire domain enum の parse fail は default 値に倒す (本 module 内で挙動を吸収)。
pub fn parse_overlay_response(
    value: &serde_json::Value,
) -> WireResult<BTreeMap<String, ProjectionOverlay>> {
    let Some(projections) = value.get("projections").and_then(|v| v.as_object()) else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (axis, entry) in projections.iter() {
        let Some(obj) = entry.as_object() else { continue };
        let Some(template) = obj.get("template").and_then(|v| v.as_str()) else {
            continue;
        };
        let target_form = obj
            .get("target_form")
            .and_then(|v| v.as_str())
            .and_then(|s| TargetForm::parse(s).ok())
            .unwrap_or(TargetForm::Markdown);
        let strategy = obj
            .get("merge_strategy")
            .and_then(|v| v.as_str())
            .map(MergeStrategy::parse)
            .unwrap_or(MergeStrategy::Replace);
        out.insert(
            axis.clone(),
            ProjectionOverlay {
                template: template.to_string(),
                target_form,
                strategy,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_projections_returns_empty_map() {
        let v = serde_json::json!({"projections": {}});
        let m = parse_overlay_response(&v).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn missing_projections_key_returns_empty_map() {
        let v = serde_json::json!({"scheme": "persona-pack"});
        let m = parse_overlay_response(&v).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn parses_full_entry() {
        let v = serde_json::json!({
            "projections": {
                "active": {
                    "template": "T",
                    "target_form": "markdown",
                    "merge_strategy": "append"
                }
            }
        });
        let m = parse_overlay_response(&v).unwrap();
        assert_eq!(m.len(), 1);
        let o = m.get("active").unwrap();
        assert_eq!(o.template, "T");
        assert!(matches!(o.target_form, TargetForm::Markdown));
        assert!(matches!(o.strategy, MergeStrategy::Append));
    }

    #[test]
    fn missing_optional_fields_default() {
        let v = serde_json::json!({
            "projections": {
                "bio": { "template": "B" }
            }
        });
        let m = parse_overlay_response(&v).unwrap();
        let o = m.get("bio").unwrap();
        assert!(matches!(o.target_form, TargetForm::Markdown));
        assert!(matches!(o.strategy, MergeStrategy::Replace));
    }

    #[test]
    fn entry_without_template_skipped() {
        let v = serde_json::json!({
            "projections": {
                "x": { "target_form": "markdown" }
            }
        });
        let m = parse_overlay_response(&v).unwrap();
        assert!(m.is_empty());
    }
}
