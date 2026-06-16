//! Template Merger Strategy。
//!
//! persona-pack の Overlay (= `[extra.persona_wire.projections.<axis>]`) を base
//! template (wire DB の動的 register or `BUILTIN_PROJECTIONS`) と merge する戦略を
//! 明示的に持つ。 完全 replace だけでなく append / prepend / partial section
//! 上書きを persona-pack 側で指定可能にする = engineering
//! 規律で組む base infra。
//!
//! ```toml
//! [extra.persona_wire.projections.active]
//! strategy = "append"   # default = "replace"
//! template = "...emote / register 上乗せ..."
//! target   = "markdown"
//! ```

/// Overlay と base template を merge するときの戦略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeStrategy {
    /// overlay で base を完全に置き換える (= default、 toml `strategy = "replace"`)。
    Replace,
    /// `<base>\n<overlay>` で末尾に追加する (= `strategy = "append"`)。
    Append,
    /// `<overlay>\n<base>` で先頭に追加する (= `strategy = "prepend"`)。
    Prepend,
    /// base 内の `{{!-- <section_name> --}}` marker を overlay で置換する
    /// (= partial section merge、 `strategy = "section:<name>"`)。 marker 不在時は
    /// Append fallback。
    Section(String),
}

impl MergeStrategy {
    /// toml literal から strategy を parse。 不明 / 空文字は Replace に倒す。
    pub fn parse(s: &str) -> Self {
        let trimmed = s.trim();
        if let Some(rest) = trimmed.strip_prefix("section:") {
            return Self::Section(rest.to_string());
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "append" => Self::Append,
            "prepend" => Self::Prepend,
            "replace" | "" => Self::Replace,
            _ => Self::Replace,
        }
    }

    /// base に overlay を merge する。 戦略別に分岐。
    pub fn merge(&self, base: &str, overlay: &str) -> String {
        match self {
            Self::Replace => overlay.to_string(),
            Self::Append => {
                if base.is_empty() {
                    overlay.to_string()
                } else if base.ends_with('\n') {
                    format!("{base}{overlay}")
                } else {
                    format!("{base}\n{overlay}")
                }
            }
            Self::Prepend => {
                if base.is_empty() {
                    overlay.to_string()
                } else if overlay.ends_with('\n') {
                    format!("{overlay}{base}")
                } else {
                    format!("{overlay}\n{base}")
                }
            }
            Self::Section(name) => {
                let marker = format!("{{{{!-- {name} --}}}}");
                if base.contains(&marker) {
                    base.replace(&marker, overlay)
                } else {
                    // marker 不在は Append fallback (= overlay を捨てるよりは載せる、
                    // best-effort)。 warnings は caller layer で push する想定。
                    Self::Append.merge(base, overlay)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_is_replace() {
        assert_eq!(MergeStrategy::parse(""), MergeStrategy::Replace);
        assert_eq!(MergeStrategy::parse("replace"), MergeStrategy::Replace);
        assert_eq!(MergeStrategy::parse("REPLACE"), MergeStrategy::Replace);
        assert_eq!(MergeStrategy::parse("nonsense"), MergeStrategy::Replace);
    }

    #[test]
    fn parse_append_prepend() {
        assert_eq!(MergeStrategy::parse("append"), MergeStrategy::Append);
        assert_eq!(MergeStrategy::parse(" Append "), MergeStrategy::Append);
        assert_eq!(MergeStrategy::parse("prepend"), MergeStrategy::Prepend);
    }

    #[test]
    fn parse_section_with_name() {
        match MergeStrategy::parse("section:emote") {
            MergeStrategy::Section(name) => assert_eq!(name, "emote"),
            _ => panic!("expected Section"),
        }
    }

    #[test]
    fn replace_overrides_base() {
        let out = MergeStrategy::Replace.merge("base body", "overlay body");
        assert_eq!(out, "overlay body");
    }

    #[test]
    fn append_concatenates_with_newline() {
        let out = MergeStrategy::Append.merge("## Active set\n- foo", "(^_^)");
        assert_eq!(out, "## Active set\n- foo\n(^_^)");
    }

    #[test]
    fn append_preserves_trailing_newline() {
        let out = MergeStrategy::Append.merge("base\n", "tail");
        assert_eq!(out, "base\ntail");
    }

    #[test]
    fn prepend_puts_overlay_first() {
        let out = MergeStrategy::Prepend.merge("base body", "## emote-header");
        assert_eq!(out, "## emote-header\nbase body");
    }

    #[test]
    fn section_replaces_marker() {
        let base = "## Active set\n{{!-- emote --}}\n- list";
        let out = MergeStrategy::Section("emote".to_string()).merge(base, "(^_^)");
        assert_eq!(out, "## Active set\n(^_^)\n- list");
    }

    #[test]
    fn section_falls_back_to_append_when_marker_missing() {
        let base = "## Active set\n- list";
        let out = MergeStrategy::Section("nonexistent".to_string()).merge(base, "(^_^)");
        assert_eq!(out, "## Active set\n- list\n(^_^)");
    }

    #[test]
    fn empty_base_returns_overlay_for_append_prepend() {
        assert_eq!(MergeStrategy::Append.merge("", "ov"), "ov");
        assert_eq!(MergeStrategy::Prepend.merge("", "ov"), "ov");
    }
}
