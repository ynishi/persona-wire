//! persona-pack `[extra.persona_wire.projections.<axis>]` Overlay resolver。
//!
//! 配線 (source_uri) の SoT は **wire DB の wiring entry** (= Node `<persona>.<axis>`
//! の `metadata.source_uri`)、 persona-pack には書かない (= 二重管理 drift 防止)。
//! 本 resolver は **Projection template の Overlay only** を扱う = persona 固有
//! emote / register が要る axis のみ persona-pack で override する path。
//!
//! 構造:
//! ```toml
//! [extra.persona_wire.projections.active]
//! strategy = "append"          # optional, default = "replace"。 merger::MergeStrategy 参照
//! template = "...emote..."
//! target   = "markdown"        # optional, default = "markdown"
//! ```
//!
//! Override 無い axis は wire-core の `BUILTIN_PROJECTIONS` (5 軸共通 form) に
//! fallback する (wire_prompt_context 側の 3 段優先順 = overlay (merger 経由) >
//! wire DB projection register > builtin の 1 段目)。

use crate::application::merger::MergeStrategy;
use crate::application::projection_registry::TargetForm;
use crate::domain::error::{WireError, WireResult};
use persona_pack::PackRoot;
use std::path::PathBuf;

/// persona-pack root を解決する (env `PERSONA_PACK_ROOT` → `~/persona-pack/`)。
fn resolve_pack_root() -> WireResult<PathBuf> {
    if let Ok(p) = std::env::var("PERSONA_PACK_ROOT") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME")
        .map_err(|_| WireError::Storage("persona-pack resolver: HOME unset".to_string()))?;
    Ok(PathBuf::from(home).join("persona-pack"))
}

/// 1 axis 分の Overlay 解決結果。
#[derive(Debug, Clone)]
pub struct ProjectionOverlay {
    pub template: String,
    pub target_form: TargetForm,
    pub strategy: MergeStrategy,
}

/// `[extra.persona_wire.projections.<axis>]` Overlay table を返す。
/// 未配置 / persona-pack read fail / `[extra.persona_wire]` 不在は `Ok(None)` で
/// silent fallback (best-effort)。 strategy 不在 = `MergeStrategy::Replace` (default)。
pub fn read_projection_overlays(
    persona_id: &str,
) -> WireResult<Option<std::collections::BTreeMap<String, ProjectionOverlay>>> {
    let root = resolve_pack_root()?;
    let pack = PackRoot::new(root);
    let persona = match pack.read(persona_id) {
        Ok(p) => p,
        Err(_) => return Ok(None), // persona-pack 不在は silent skip
    };

    let Some(persona_wire) = persona.extra.get("persona_wire") else {
        return Ok(None);
    };
    let Some(table) = persona_wire.as_table() else {
        return Ok(None);
    };
    let Some(projections) = table.get("projections").and_then(|v| v.as_table()) else {
        return Ok(None);
    };

    let mut out = std::collections::BTreeMap::new();
    for (axis, entry) in projections.iter() {
        let Some(e) = entry.as_table() else { continue };
        let Some(template) = e.get("template").and_then(|v| v.as_str()) else {
            continue;
        };
        let target = e
            .get("target")
            .and_then(|v| v.as_str())
            .and_then(|s| TargetForm::parse(s).ok())
            .unwrap_or(TargetForm::Markdown);
        let strategy = e
            .get("strategy")
            .and_then(|v| v.as_str())
            .map(MergeStrategy::parse)
            .unwrap_or(MergeStrategy::Replace);
        out.insert(
            axis.clone(),
            ProjectionOverlay {
                template: template.to_string(),
                target_form: target,
                strategy,
            },
        );
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_persona_returns_none_silently() {
        // 存在しない persona id → Ok(None) で silent fallback。
        let r = read_projection_overlays("__definitely_not_a_persona__");
        assert!(matches!(r, Ok(None)));
    }
}
