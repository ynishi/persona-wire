//! Layer 6 Adapter (SoT) — concept-doc §3 Layer 6 + §5 #3 / §P3b 反映済。
//!
//! Adapter trait + 同梱 `FileAdapter` のみを core で保持。 各 wiring entry node の
//! `metadata.source_uri` を scheme 別に dispatch する Plugin 軸 1 / 3。
//!
//! 同梱 scheme:
//! - `file://<absolute-or-tilde-path>` — std::fs::read で raw 字面を取得 (json/toml は将来
//!   parse 拡張、 現状は string として返す)。
//!
//!   query param 拡張 (R5):
//!   - `?tail=last_section` — 末尾章 (markdown `## ` h2 boundary で切り、最後の h2 以降を返す)
//!   - `?tail_n=<N>` — 末尾 N 行 (line-based; 上限 [`TAIL_N_MAX`] = 1000 行 = context size guard)
//!   - query param なし → file 全体 fetch (backward-compat)
//!   - 不明値 / parse 不能 → graceful fail = 全体 fetch
//!
//!   metadata (R4):
//!   - `size_bytes` — ファイル全体のバイト数 (tail 適用後 body size ではなく元 file 全体の size)
//!   - `modified_at` — 最終更新時刻 (Unix epoch 秒, u64)
//!
//! 外部 crate で提供 (P3b で分離済):
//! - `mini-app://<table>...` → `persona-wire-adapter-mini-app` crate (`MiniAppAdapter`)
//!
//! outline / persona-pack / journal scheme は外部 adapter crate carry。

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::wire_uri::WireUri;
use async_trait::async_trait;

/// `?tail_n=<N>` の N の上限 (context size guard)。
/// 超過時はこの値に clamp して末尾行を取得する。
pub const TAIL_N_MAX: usize = 1000;

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

/// `FileAdapter` 内部の tail 取得モード。
///
/// - [`TailMode::Full`]        — query param なし / 不正値 (graceful fail)
/// - [`TailMode::LastSection`] — `?tail=last_section`
/// - [`TailMode::LastN`]       — `?tail_n=N` (N は [`TAIL_N_MAX`] に clamp 済)
#[derive(Debug, Clone, PartialEq, Eq)]
enum TailMode {
    Full,
    LastSection,
    LastN(usize),
}

impl FileAdapter {
    /// `file://<path>` or `file:<path>` の path 部分を受けて、 std::fs::read で raw 字面を取得。
    /// `~/` で始まる場合は HOME 展開。 directory が渡された場合は最新 mtime の child file 1 件を読む。
    ///
    /// query param なし = backward-compat (全体 fetch)。
    /// R4 metadata (`size_bytes` / `modified_at`) を結果に含む。
    pub async fn fetch_file(&self, raw_path: &str) -> WireResult<serde_json::Value> {
        self.fetch_file_impl(raw_path, TailMode::Full).await
    }

    async fn fetch_file_impl(
        &self,
        raw_path: &str,
        mode: TailMode,
    ) -> WireResult<serde_json::Value> {
        let resolved = resolve_file_path(raw_path)?;
        let meta = std::fs::metadata(&resolved)
            .map_err(|e| WireError::Storage(format!("file adapter: stat: {e}")))?;
        if meta.is_dir() {
            let newest = newest_child(&resolved)?;
            let body_full = std::fs::read_to_string(&newest)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            let child_meta = std::fs::metadata(&newest)
                .map_err(|e| WireError::Storage(format!("file adapter: stat child: {e}")))?;
            let size_bytes = child_meta.len();
            let modified_at = mtime_unix(&child_meta);
            let body = apply_tail(&body_full, &mode);
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "newest_in_dir",
                "dir": resolved.display().to_string(),
                "path": newest.display().to_string(),
                "body": body,
                "size_bytes": size_bytes,
                "modified_at": modified_at,
            }))
        } else {
            let body_full = std::fs::read_to_string(&resolved)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            let size_bytes = meta.len();
            let modified_at = mtime_unix(&meta);
            let body = apply_tail(&body_full, &mode);
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "file",
                "path": resolved.display().to_string(),
                "body": body,
                "size_bytes": size_bytes,
                "modified_at": modified_at,
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
        let mode = parse_tail_mode(uri);
        self.fetch_file_impl(rest, mode).await
    }
}

/// `WireUri` の query params から [`TailMode`] を決定する。
///
/// - `?tail=last_section` → [`TailMode::LastSection`]
/// - `?tail_n=N` (N > 0 の整数) → [`TailMode::LastN`] (N は [`TAIL_N_MAX`] に clamp)
/// - 不明値 / parse 不能 / N=0 → [`TailMode::Full`] (graceful fail)
fn parse_tail_mode(uri: &WireUri) -> TailMode {
    if let Some(tail) = uri.query_get("tail") {
        if tail == "last_section" {
            return TailMode::LastSection;
        }
        // 不明値 → graceful fail = Full
        return TailMode::Full;
    }
    if let Some(n_str) = uri.query_get("tail_n") {
        if let Ok(n) = n_str.parse::<usize>() {
            if n > 0 {
                return TailMode::LastN(n.min(TAIL_N_MAX));
            }
        }
        // parse 不能 / n=0 → graceful fail = Full
        return TailMode::Full;
    }
    TailMode::Full
}

/// `body` に `mode` を適用し部分文字列を返す。
///
/// - [`TailMode::Full`]        — `body` をそのまま返す
/// - [`TailMode::LastSection`] — 最後の `## ` h2 見出し行から末尾まで返す
/// - [`TailMode::LastN`]       — 末尾 N 行を `"\n"` で join して返す
fn apply_tail(body: &str, mode: &TailMode) -> String {
    match mode {
        TailMode::Full => body.to_string(),
        TailMode::LastSection => {
            let pos = last_h2_pos(body);
            body[pos..].to_string()
        }
        TailMode::LastN(n) => {
            let lines: Vec<&str> = body.lines().collect();
            let skip = lines.len().saturating_sub(*n);
            lines[skip..].join("\n")
        }
    }
}

/// `body` 内の最後の markdown h2 見出し (`## ` で始まる行) の byte 位置を返す。
/// 見つからない場合は `0` (= body 全体を返す)。
fn last_h2_pos(body: &str) -> usize {
    let needle = "\n## ";
    if let Some(pos) = body.rfind(needle) {
        // `\n` の次の byte (= `#` の先頭) から返す
        return pos + 1;
    }
    if body.starts_with("## ") {
        return 0;
    }
    0
}

/// `std::fs::Metadata` から Unix epoch 秒を取り出す。取得不能なら `0`。
fn mtime_unix(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn resolve_file_path(raw: &str) -> WireResult<PathBuf> {
    // `~/...` -> $HOME 展開
    // `#fragment` と `?query` を path から剥がす (どちらも filesystem lookup では無効)
    let stripped = raw.split('#').next().unwrap_or(raw);
    let stripped = stripped.split('?').next().unwrap_or(stripped);
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

    // ---- ヘルパー ----

    fn write_test_file(name: &str, content: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("pw_adapter_test_{name}"));
        std::fs::write(&path, content).expect("write temp file");
        path
    }

    // ---- 既存テスト (backward-compat) ----

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

    // ---- R4: metadata ----

    #[tokio::test]
    async fn file_adapter_r4_metadata_size_and_mtime() {
        let content = "hello r4 metadata\n";
        let path = write_test_file("r4_meta.txt", content);
        let uri = WireUri::parse(&format!("file://{}", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["body"].as_str().unwrap(), content, "body unchanged");
        assert!(
            v["size_bytes"].as_u64().unwrap() > 0,
            "size_bytes present and > 0"
        );
        assert!(
            v["modified_at"].as_u64().is_some(),
            "modified_at present as u64"
        );
    }

    // ---- R5: tail=last_section ----

    #[tokio::test]
    async fn file_adapter_r5_tail_last_section() {
        let content =
            "# Title\n\nIntro text.\n\n## Section 1\n\nContent 1.\n\n## Section 2\n\nContent 2.\n";
        let path = write_test_file("r5_last_section.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail=last_section", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        let body = v["body"].as_str().unwrap();
        assert!(
            body.starts_with("## Section 2"),
            "should start with last h2; got: {body}"
        );
        assert!(
            !body.contains("Section 1"),
            "should not contain earlier section; got: {body}"
        );
    }

    // ---- R5: tail_n ----

    #[tokio::test]
    async fn file_adapter_r5_tail_n() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let path = write_test_file("r5_tail_n.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail_n=3", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        let body = v["body"].as_str().unwrap();
        assert_eq!(body, "line3\nline4\nline5", "last 3 lines");
    }

    // ---- R5: tail_n clamp (N > TAIL_N_MAX) ----

    #[tokio::test]
    async fn file_adapter_r5_tail_n_clamp() {
        // 5 行のファイルで tail_n=2000 (> TAIL_N_MAX=1000) → clamp → 全 5 行が返る
        let content = "a1\na2\na3\na4\na5\n";
        let path = write_test_file("r5_clamp.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail_n=2000", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        let body = v["body"].as_str().unwrap();
        assert!(
            body.contains("a1"),
            "clamp: all lines returned; got: {body}"
        );
        assert!(
            body.contains("a5"),
            "clamp: all lines returned; got: {body}"
        );
    }

    // ---- R5: no params — backward-compat ----

    #[tokio::test]
    async fn file_adapter_r5_no_params_backward_compat() {
        let content = "full content here\n";
        let path = write_test_file("r5_no_params.txt", content);
        let uri = WireUri::parse(&format!("file://{}", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["body"].as_str().unwrap(), content, "full body returned");
        assert_eq!(v["scheme"].as_str().unwrap(), "file");
        assert_eq!(v["kind"].as_str().unwrap(), "file");
    }

    // ---- R5: graceful fail — ?tail=invalid ----

    #[tokio::test]
    async fn file_adapter_r5_tail_invalid_graceful_fail() {
        let content = "some content\n";
        let path = write_test_file("r5_tail_inv.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail=invalid", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(
            v["body"].as_str().unwrap(),
            content,
            "graceful fail: full body returned"
        );
    }

    // ---- R5: graceful fail — ?tail_n=abc ----

    #[tokio::test]
    async fn file_adapter_r5_tail_n_invalid_graceful_fail() {
        let content = "some content\n";
        let path = write_test_file("r5_tail_n_inv.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail_n=abc", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(
            v["body"].as_str().unwrap(),
            content,
            "graceful fail: full body returned"
        );
    }

    // ---- R4 + R5 combined ----

    #[tokio::test]
    async fn file_adapter_r5_r4_combined() {
        let content = "# Header\n\n## Section 1\n\nContent 1.\n\n## Section 2\n\nContent 2.\n";
        let full_size = content.len() as u64;
        let path = write_test_file("r5_r4_combo.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail=last_section", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        // R5: body は末尾セクションのみ
        let body = v["body"].as_str().unwrap();
        assert!(
            body.starts_with("## Section 2"),
            "R5: last section; got: {body}"
        );
        assert!(!body.contains("Section 1"), "R5: no earlier section");
        // R4: size_bytes は元 file 全体の size (tail 後 body ではない)
        assert_eq!(
            v["size_bytes"].as_u64().unwrap(),
            full_size,
            "R4: size_bytes = full file size"
        );
        // R4: modified_at は u64 として存在
        assert!(
            v["modified_at"].as_u64().is_some(),
            "R4: modified_at present"
        );
    }

    // ---- unit tests: pure functions ----

    #[test]
    fn last_h2_pos_finds_last_section() {
        let body = "# Title\n\n## S1\n\nContent\n\n## S2\n\nEnd\n";
        let pos = last_h2_pos(body);
        assert!(body[pos..].starts_with("## S2"), "pos={pos}");
    }

    #[test]
    fn last_h2_pos_no_h2_returns_zero() {
        let body = "No heading here\n";
        assert_eq!(last_h2_pos(body), 0);
    }

    #[test]
    fn last_h2_pos_h2_at_start() {
        let body = "## Only\n\nContent\n";
        assert_eq!(last_h2_pos(body), 0);
    }

    #[test]
    fn apply_tail_full_returns_body() {
        let body = "a\nb\nc\n";
        assert_eq!(apply_tail(body, &TailMode::Full), body);
    }

    #[test]
    fn apply_tail_last_n_returns_last_lines() {
        let body = "a\nb\nc\nd\ne\n";
        let result = apply_tail(body, &TailMode::LastN(3));
        assert_eq!(result, "c\nd\ne");
    }

    #[test]
    fn apply_tail_last_n_returns_all_when_n_exceeds_line_count() {
        let body = "x\ny\n";
        let result = apply_tail(body, &TailMode::LastN(1000));
        assert_eq!(result, "x\ny");
    }

    #[test]
    fn parse_tail_mode_unknown_tail_returns_full() {
        let uri = WireUri::parse("file:///tmp/x?tail=unknown").unwrap();
        assert_eq!(parse_tail_mode(&uri), TailMode::Full);
    }

    #[test]
    fn parse_tail_mode_last_section() {
        let uri = WireUri::parse("file:///tmp/x?tail=last_section").unwrap();
        assert_eq!(parse_tail_mode(&uri), TailMode::LastSection);
    }

    #[test]
    fn parse_tail_mode_tail_n_clamped() {
        let uri = WireUri::parse("file:///tmp/x?tail_n=5000").unwrap();
        assert_eq!(parse_tail_mode(&uri), TailMode::LastN(TAIL_N_MAX));
    }

    #[test]
    fn parse_tail_mode_tail_n_abc_returns_full() {
        let uri = WireUri::parse("file:///tmp/x?tail_n=abc").unwrap();
        assert_eq!(parse_tail_mode(&uri), TailMode::Full);
    }

    #[test]
    fn parse_tail_mode_no_params_returns_full() {
        let uri = WireUri::parse("file:///tmp/x").unwrap();
        assert_eq!(parse_tail_mode(&uri), TailMode::Full);
    }
}
