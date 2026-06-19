//! Layer 6 Adapter (SoT) — concept-doc §3 Layer 6 + §5 #3 / §P3b 反映済。
//!
//! Adapter trait + 同梱 `FileAdapter` のみを core で保持。 各 wiring entry node の
//! `metadata.source_uri` を scheme 別に dispatch する Plugin 軸 1 / 3。
//!
//! 同梱 scheme:
//! - `file://<absolute-or-tilde-path>` — std::fs::read で raw 字面を取得 (json/toml は将来
//!   parse 拡張、 現状は string として返す)。
//!
//! 外部 crate で提供 (P3b で分離済):
//! - `mini-app://<table>...` → `persona-wire-adapter-mini-app` crate (`MiniAppAdapter`)
//!
//! outline / persona-pack / journal scheme は外部 adapter crate carry。

use std::path::PathBuf;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::wire_uri::WireUri;
use async_trait::async_trait;

/// Adapter trait — Plugin 軸 1 / 3 (SoT Adapter)。
///
/// dyn-compatible にするため `#[async_trait]` で `Pin<Box<Future>>` 化。 PluginRegistry
/// が `Arc<dyn Adapter>` で複数 impl を一様に保持する前提。
///
/// ACL Facade として機能する責務:
/// - URI grammar parse は registry 側 (`WireUri::parse`) が一手に集約済。 Adapter は
///   parsed `WireUri` を受けて、 scheme 固有の semantic 解釈 + 外部 SDK 呼出し +
///   Wire 定義 JSON への翻訳 を担う。
/// - 既存 adapter で internal parser を持つものは `uri.as_raw()` で full URI 文字列を
///   取り出して当面互換 (carry)、 新規 adapter は typed access (`host()` / `query()` 等)
///   推奨。
#[async_trait]
pub trait Adapter: Send + Sync {
    /// このアダプタが扱う URI scheme 識別子 (例: `"mini-app"` / `"file"` / `"pg"`).
    ///
    /// `PluginRegistry` (application 層) が `source_uri` の prefix と突き合わせて
    /// dispatch 判定に使う。 1 scheme = 1 impl が原則 (collision は registry build 時に
    /// fail-fast)。
    fn scheme(&self) -> &'static str;

    /// parsed `WireUri` を scheme 別に解釈し、 fresh data を `serde_json::Value` で返す。
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value>;
}

// ---- file adapter (std::fs) ----

pub struct FileAdapter;

impl FileAdapter {
    /// `file://<path>` or `file:<path>` の path 部分を受けて、 std::fs::read で raw 字面を取得。
    /// `~/` で始まる場合は HOME 展開。 directory が渡された場合は最新 mtime の child file 1 件を読む。
    pub async fn fetch_file(&self, raw_path: &str) -> WireResult<serde_json::Value> {
        let resolved = resolve_file_path(raw_path)?;
        let meta = std::fs::metadata(&resolved)
            .map_err(|e| WireError::Storage(format!("file adapter: stat: {e}")))?;
        if meta.is_dir() {
            let newest = newest_child(&resolved)?;
            let body = std::fs::read_to_string(&newest)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "newest_in_dir",
                "dir": resolved.display().to_string(),
                "path": newest.display().to_string(),
                "body": body,
            }))
        } else {
            let body = std::fs::read_to_string(&resolved)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "file",
                "path": resolved.display().to_string(),
                "body": body,
            }))
        }
    }
}

#[async_trait]
impl Adapter for FileAdapter {
    fn scheme(&self) -> &'static str {
        "file"
    }

    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        // file URI は `file://~/foo` 等の非 RFC な lenient form を受け入れたいため、
        // raw 文字列から strip_prefix で path 部を取り出す (typed host/path だと
        // `~` が host 扱いになり挙動が変わる)。
        let source_uri = uri.as_raw();
        let rest = source_uri
            .strip_prefix("file://")
            .or_else(|| source_uri.strip_prefix("file:"))
            .ok_or_else(|| WireError::Storage(format!("file adapter: bad uri: {source_uri}")))?;
        self.fetch_file(rest).await
    }
}

fn resolve_file_path(raw: &str) -> WireResult<PathBuf> {
    // `~/...` -> $HOME 展開、 `#fragment` を path から剥がす (anchor は wire 内で無視)
    let stripped = raw.split('#').next().unwrap_or(raw);
    let expanded = if let Some(rest) = stripped.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("file adapter: HOME unset".to_string()))?;
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(stripped)
    };
    Ok(expanded)
}

fn newest_child(dir: &std::path::Path) -> WireResult<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| WireError::Storage(format!("file adapter: read_dir: {e}")))?
        .filter_map(|r| r.ok())
        .filter(|e| e.path().is_file())
        .collect();
    if entries.is_empty() {
        return Err(WireError::Storage(format!(
            "file adapter: empty dir: {}",
            dir.display()
        )));
    }
    entries.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .ok()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    Ok(entries
        .last()
        .map(|e| e.path())
        .expect("non-empty sorted entries"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_adapter_reads_existing_file() {
        let me = file!();
        let abs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(me);
        let uri = WireUri::parse(&format!("file://{}", abs.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        let body = v["body"].as_str().unwrap();
        assert!(body.contains("Layer 6 Adapter"));
    }

    #[tokio::test]
    async fn file_adapter_rejects_non_file_uri() {
        let a = FileAdapter;
        let uri = WireUri::parse("ssh://nope/x").unwrap();
        let r = a.fetch(&uri).await;
        assert!(r.is_err());
    }
}
