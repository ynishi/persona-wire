//! WireUri — Wire 内で扱う URI の typed view (Layer 6 Adapter dispatch の共通入力)。
//!
//! 目的:
//! - URI grammar parse を **registry 一手に集約** (各 Adapter が strip_prefix を重複実装する
//!   drift を構造除去)。
//! - Adapter は `scheme()` / `host()` / `path()` / `query()` の typed access か、 互換のための
//!   `as_raw()` (full URI 文字列) のどちらかを選んで使う。
//!
//! 適用範囲 (RFC 3986 minimal subset):
//! - `scheme:[//authority]path[?query][#fragment]`
//! - authority は host 1 要素のみ (userinfo / port は parse しない、 必要になったら拡張)
//! - query は `key=value&key=value` flat form のみ (multi-value は最初の 1 個を採用)
//!
//! ACL Facade 観点:
//! - URI 形式の定義責任は **Wire 側** (本 module + `PluginRegistry::route`)。
//! - 外部 SDK (persona-pack / mini-app / sqlite / file 等) の固有 grammar は Adapter 内に閉じる。

use std::collections::BTreeMap;

use crate::domain::error::{WireError, WireResult};

/// Parsed view of a `<scheme>://<host>/<path>?<query>#<fragment>` style URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireUri {
    scheme: String,
    host: Option<String>,
    path: String,
    query: BTreeMap<String, String>,
    fragment: Option<String>,
    raw: String,
}

impl WireUri {
    /// Parse a URI string into typed components.
    ///
    /// Accepted forms:
    /// - `scheme:rest` (no authority)
    /// - `scheme://host/path?query#fragment`
    /// - `scheme:///path` (empty authority)
    pub fn parse(uri: &str) -> WireResult<Self> {
        let raw = uri.to_string();
        let (scheme, remainder) = uri
            .split_once(':')
            .ok_or_else(|| WireError::Storage(format!("wire uri: missing scheme: {uri}")))?;
        if scheme.is_empty() {
            return Err(WireError::Storage(format!("wire uri: empty scheme: {uri}")));
        }
        if !is_valid_scheme(scheme) {
            return Err(WireError::Storage(format!(
                "wire uri: invalid scheme `{scheme}`: must start with ALPHA and contain only ALPHA/DIGIT/+-.",
            )));
        }

        // Strip fragment first (last `#`)
        let (rest, fragment) = match remainder.split_once('#') {
            Some((before, after)) => (before, Some(after.to_string())),
            None => (remainder, None),
        };

        // Strip query
        let (path_with_authority, query_str) = match rest.split_once('?') {
            Some((before, after)) => (before, Some(after)),
            None => (rest, None),
        };

        // Split authority (// prefix) from path
        let (host, path) = if let Some(after_slashes) = path_with_authority.strip_prefix("//") {
            // host = up to next `/` (or end)
            match after_slashes.find('/') {
                Some(idx) => (
                    Some(after_slashes[..idx].to_string()),
                    after_slashes[idx..].to_string(),
                ),
                None => (Some(after_slashes.to_string()), String::new()),
            }
        } else {
            (None, path_with_authority.to_string())
        };

        let query = match query_str {
            Some(q) => parse_query(q),
            None => BTreeMap::new(),
        };

        Ok(Self {
            scheme: scheme.to_string(),
            host,
            path,
            query,
            fragment,
            raw,
        })
    }

    /// URI scheme (例: `"file"` / `"mini-app"` / `"persona-pack"`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Host (authority) component, `None` if URI lacks `//` prefix.
    /// Empty string is treated as `Some("")` (e.g. `file:///path` has host=`""`).
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// Path component (may be empty string).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Query parameters (flat `key=value` map).
    pub fn query(&self) -> &BTreeMap<String, String> {
        &self.query
    }

    /// Single query parameter lookup helper.
    pub fn query_get(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(|s| s.as_str())
    }

    /// Fragment (`#` 以降), `None` if absent.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// 原 URI 文字列。 既存 adapter の internal parser に渡す互換 path。
    pub fn as_raw(&self) -> &str {
        &self.raw
    }
}

impl std::fmt::Display for WireUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

fn is_valid_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (pair.to_string(), String::new()),
        };
        // first-wins: 重複 key は無視
        out.entry(k).or_insert(v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_uri() {
        let u = WireUri::parse("persona-pack://bob/projections?axis=active#frag").unwrap();
        assert_eq!(u.scheme(), "persona-pack");
        assert_eq!(u.host(), Some("bob"));
        assert_eq!(u.path(), "/projections");
        assert_eq!(u.query_get("axis"), Some("active"));
        assert_eq!(u.fragment(), Some("frag"));
        assert_eq!(
            u.as_raw(),
            "persona-pack://bob/projections?axis=active#frag"
        );
    }

    #[test]
    fn parses_file_scheme_with_triple_slash() {
        let u = WireUri::parse("file:///tmp/x.json").unwrap();
        assert_eq!(u.scheme(), "file");
        assert_eq!(u.host(), Some("")); // empty authority
        assert_eq!(u.path(), "/tmp/x.json");
    }

    #[test]
    fn parses_file_scheme_without_authority() {
        let u = WireUri::parse("file:/tmp/x.json").unwrap();
        assert_eq!(u.scheme(), "file");
        assert_eq!(u.host(), None);
        assert_eq!(u.path(), "/tmp/x.json");
    }

    #[test]
    fn parses_mini_app_with_query() {
        let u = WireUri::parse("mini-app://issue?scope=user&limit=10").unwrap();
        assert_eq!(u.scheme(), "mini-app");
        assert_eq!(u.host(), Some("issue"));
        assert_eq!(u.path(), "");
        assert_eq!(u.query_get("scope"), Some("user"));
        assert_eq!(u.query_get("limit"), Some("10"));
    }

    #[test]
    fn parses_host_only() {
        let u = WireUri::parse("persona-pack://bob").unwrap();
        assert_eq!(u.host(), Some("bob"));
        assert_eq!(u.path(), "");
    }

    #[test]
    fn rejects_missing_scheme() {
        let r = WireUri::parse("no-colon-here");
        assert!(r.is_err());
    }

    #[test]
    fn rejects_empty_scheme() {
        let r = WireUri::parse(":nope");
        assert!(r.is_err());
    }

    #[test]
    fn rejects_invalid_scheme_chars() {
        let r = WireUri::parse("1bad://x");
        assert!(r.is_err());
    }

    #[test]
    fn raw_preserves_input() {
        let raw = "file://~/foo/bar";
        let u = WireUri::parse(raw).unwrap();
        // file://~/foo/bar の host="~" は file adapter の semantic 上は raw 経由で扱う想定。
        // raw を素のまま保持できることを保証。
        assert_eq!(u.as_raw(), raw);
    }

    #[test]
    fn display_returns_raw() {
        let u = WireUri::parse("file:///tmp/x").unwrap();
        assert_eq!(format!("{u}"), "file:///tmp/x");
    }

    #[test]
    fn empty_query_value_ok() {
        let u = WireUri::parse("mini-app://t?flag&k=v").unwrap();
        assert_eq!(u.query_get("flag"), Some(""));
        assert_eq!(u.query_get("k"), Some("v"));
    }

    #[test]
    fn fragment_only() {
        let u = WireUri::parse("file:/p#anchor").unwrap();
        assert_eq!(u.path(), "/p");
        assert_eq!(u.fragment(), Some("anchor"));
    }
}
