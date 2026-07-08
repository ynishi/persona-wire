//! Layer 6 Adapter (SoT) — reflects concept-doc §3 Layer 6 + §5 #3 / §P3b.
//!
//! The core keeps only the Adapter trait and the bundled `FileAdapter`. Each
//! wiring entry node's `metadata.source_uri` is dispatched by scheme — plugin
//! axis 1 of 3.
//!
//! Bundled scheme:
//! - `file://<absolute-or-tilde-path>` — reads the raw contents via
//!   std::fs::read (json/toml parsing is a future extension; currently the
//!   contents are returned as a string).
//!
//!   Query param extensions (R5):
//!   - `?tail=last_section` — the trailing section (split at markdown `## `
//!     h2 boundaries; returns everything from the last h2 onward)
//!   - `?tail_n=<N>` — the last N lines (line-based; capped at
//!     [`TAIL_N_MAX`] = 1000 lines as a context size guard)
//!   - no query param → fetch the whole file (backward-compat)
//!   - unknown / unparsable values → graceful fail = whole-file fetch
//!
//!   Metadata (R4):
//!   - `size_bytes` — size of the whole file in bytes (the original file
//!     size, not the post-tail body size)
//!   - `modified_at` — last modified time (Unix epoch seconds, u64)
//!   - `metadata` — nested metadata object (`filename` / `full_path` /
//!     `last_modified` / `size_bytes` / `age_days`)
//!
//! Provided by external crates (split out in P3b):
//! - `mini-app://<table>...` → `persona-wire-adapter-mini-app` crate (`MiniAppAdapter`)
//!
//! The outline / persona-pack / journal schemes are carried by external
//! adapter crates.
//!
//! ## Adapter authoring guide (conventions for adding a new scheme)
//!
//! Add one crate per scheme, named `persona-wire-adapter-<scheme>`, to the
//! workspace. The canonical reference is `persona-wire-adapter-rss` (minimal,
//! stateless, direct SDK integration).
//!
//! - **Three-function split**: `parse_<scheme>_uri` ([`WireUri`] → Spec struct),
//!   transport fetch (no domain knowledge), and `normalize_<scheme>`
//!   (raw response → Wire JSON shape). HTTP transport is provided by the
//!   shared `persona-wire-transport-http` crate (promoted 2026-07-07);
//!   HTTP-backed adapters use its `HttpClient` instead of hand-rolling
//!   `reqwest` calls.
//! - **Guard constants**: declare item caps / timeouts / text truncation as
//!   `pub const` (rss example: `DEFAULT_LIMIT=20` / `FETCH_TIMEOUT=30s` /
//!   `SUMMARY_MAX_CHARS=500`). Align timeouts with existing adapters
//!   (`DEFAULT_RPC_TIMEOUT` in the mcp adapter).
//! - **Error / query conventions**: missing or invalid required components
//!   (empty host, `limit=0`, ...) fail loud with [`WireError::Storage`].
//!   Unknown query keys are silently ignored (forward-compat convention).
//!   Missing output fields are `null`; timestamps are RFC3339. A missing
//!   source is graceful (`FileAdapter` in this file: non-existent path →
//!   `body: null` with `Ok`).
//! - **Docs**: `#![warn(missing_docs)]` plus a crate-root `//!` header with
//!   three sections: Architecture / URI grammar / Output shape.
//! - **Tests**: parse / normalize are offline unit tests over inline
//!   fixtures. Never add tests that depend on live network access.
//! - **Registration**: add one `.with_adapter(XxxAdapter)` line to the
//!   `PluginRegistry::default_builder_for_wire()` chain on the boot side
//!   (`persona-wire-mcp/src/lib.rs`). Scheme collisions fail fast at
//!   registry build time.
//!
//! ## External service integration policy (decided 2026-07-07)
//!
//! - When a service exposes a public SDK / API, call it **directly** from the
//!   adapter. Do not relay through an MCP integration (this is a core benefit
//!   of the Rust + Adapter pattern).
//! - UX first: never make the user repeat authentication. Receive credentials
//!   via environment variables and never embed secrets in `source_uri`.
//!   Choose the auth mechanism per SDK on a case-by-case basis.
//! - Adapter expansion targets coverage (including minor services), not
//!   demand-ranked prioritization. The only exclusion criterion is that the
//!   service has been discontinued.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::wire_uri::WireUri;
use async_trait::async_trait;
use serde_json::Value;

/// Upper bound for `N` in `?tail_n=<N>` (context size guard).
/// Values above this are clamped before taking the tail lines.
pub const TAIL_N_MAX: usize = 1000;

/// Adapter trait — plugin axis 1 of 3 (SoT Adapter).
///
/// Uses `#[async_trait]` (`Pin<Box<Future>>`) to stay dyn-compatible; the
/// PluginRegistry holds multiple impls uniformly as `Arc<dyn Adapter>`.
///
/// Responsibilities as an ACL Facade:
/// - URI grammar parsing is centralized on the registry side
///   (`WireUri::parse`). An Adapter receives the parsed `WireUri` and handles
///   scheme-specific semantic interpretation, external SDK calls, and
///   translation into Wire definition JSON.
/// - Existing adapters with internal parsers may keep extracting the full URI
///   string via `uri.as_raw()` for now (carry); new adapters should use typed
///   access (`host()` / `query()` etc.).
#[async_trait]
pub trait Adapter: Send + Sync {
    /// URI scheme identifier handled by this adapter (e.g. `"mini-app"` /
    /// `"file"` / `"pg"`).
    ///
    /// The `PluginRegistry` (application layer) matches this against the
    /// `source_uri` prefix for dispatch. One scheme = one impl as a rule
    /// (collisions fail fast at registry build time).
    fn scheme(&self) -> &'static str;

    /// Interprets the parsed `WireUri` per scheme and returns fresh data as a
    /// `serde_json::Value`.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value>;

    /// Capability accessor for the wire-layer pagination driver (Layer 2 of
    /// GH #1). Override to opt into wire-layer pagination by returning
    /// `Some(self)` once the adapter also implements [`Pageable`].
    ///
    /// Default returns `None` — existing adapters keep working with zero
    /// changes. Rust trait objects don't support cross-trait downcasting
    /// without `Any + 'static` bounds, so this companion accessor is the
    /// idiomatic pattern (mirrors `std::error::Error::source`). The wire
    /// layer (`application::use_cases::fetch_with_pagination_awareness`)
    /// calls this to decide whether to drive a [`Pageable::fetch_page`] loop
    /// or fall back to a plain capped `fetch` with a WARN.
    fn as_pageable(&self) -> Option<&dyn Pageable> {
        None
    }
}

/// Opaque pagination cursor. Each adapter picks the variant matching its
/// upstream API. The wire layer treats this as an opaque token to thread
/// through subsequent `fetch_page` calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    /// GitHub-style page number: `?page=N`.
    PageNumber(u32),
    /// GitHub-style `Link` header `rel="next"` URL.
    LinkHeader(String),
    /// Notion / Slack-style opaque continuation token.
    NextToken(String),
    /// Todoist-style numeric offset.
    Offset(u64),
}

/// Wire-layer heuristic threshold used when a caller requests `limit` items
/// from an adapter that does **not** implement [`Pageable`] (Layer 2 of GH
/// #1). This is not a per-adapter contract — adapters that want proper
/// pagination should implement [`Pageable`] and override
/// [`Adapter::as_pageable`]; this constant only decides when the wire layer
/// should emit a WARN before falling back to a plain capped `fetch`.
///
/// Value: `100`, matching GitHub's REST list API `per_page` cap — a
/// de-facto industry norm for this class of API.
pub const NON_PAGEABLE_MAX_HINT: usize = 100;

/// Capability trait: an adapter that can paginate its upstream API.
///
/// The wire-layer fetch driver checks whether an adapter implements
/// `Pageable`. If it does, and the caller requests more items than
/// `max_page_size` returns, the driver threads the returned `Cursor`
/// through subsequent `fetch_page` calls until the requested count is
/// satisfied or the upstream signals end-of-data (`Ok((_, None))`).
///
/// Adapters that do not implement `Pageable` are called through the plain
/// `Adapter::fetch` path; the wire layer emits a WARN log when a caller
/// requests `limit > NON_PAGEABLE_MAX_HINT` from such an adapter (see Layer
/// 2 follow-up subtask).
///
/// Once the loop finishes (or the caller's `limit` is satisfied), the driver
/// hands the collected items to [`Pageable::wrap_items`] to assemble the
/// final response (Layer 3a of GH #1).
#[async_trait]
pub trait Pageable: Send + Sync {
    /// Maximum items returned in a single upstream request. When the caller
    /// requests more than this value, the wire layer drives the pagination
    /// loop through `fetch_page`.
    fn max_page_size(&self) -> usize;

    /// Fetch one page. `cursor = None` requests the first page. Returns
    /// `(items, next_cursor)`; a `None` next_cursor signals end-of-data.
    ///
    /// The exact `Item` shape is per-adapter but each item is a
    /// `serde_json::Value` matching the adapter's normalized single-item
    /// shape (same shape the adapter's `fetch` would put in the top-level
    /// `items` array).
    async fn fetch_page(
        &self,
        uri: &WireUri,
        cursor: Option<Cursor>,
    ) -> WireResult<(Vec<Value>, Option<Cursor>)>;

    /// Wraps the items collected across a completed pagination loop into the
    /// final wire-compatible `serde_json::Value` returned to the caller.
    ///
    /// Without this hook, the wire-layer driver would have to guess a
    /// generic response shape, which would silently diverge from the shape
    /// `Adapter::fetch` produces on the non-paginated fast path (e.g.
    /// GitHub's `{repo, kind, items}`). Overriding `wrap_items` lets each
    /// `Pageable` adapter preserve its canonical shape across both paths.
    ///
    /// Sync (not `async`): building the wrapper JSON is pure, no I/O.
    ///
    /// Default produces the generic `{"count": items.len(), "items": items}`
    /// shape (no `scheme` — assembling that is `Adapter`'s business, and this
    /// generic default has no scheme knowledge).
    fn wrap_items(&self, items: Vec<Value>, _uri: &WireUri) -> WireResult<Value> {
        Ok(serde_json::json!({
            "count": items.len(),
            "items": items,
        }))
    }
}

// ---- file adapter (std::fs) ----

pub struct FileAdapter;

/// Tail-fetch mode internal to `FileAdapter`.
///
/// - [`TailMode::Full`]        — no query param / invalid value (graceful fail)
/// - [`TailMode::LastSection`] — `?tail=last_section`
/// - [`TailMode::LastN`]       — `?tail_n=N` (N already clamped to [`TAIL_N_MAX`])
#[derive(Debug, Clone, PartialEq, Eq)]
enum TailMode {
    Full,
    LastSection,
    LastN(usize),
}

impl FileAdapter {
    /// Takes the path part of `file://<path>` or `file:<path>` and reads the
    /// raw contents via std::fs::read. Paths starting with `~/` are
    /// HOME-expanded. When a directory is given, the single child file with
    /// the newest mtime is read. A non-existent path returns `Ok` with
    /// `body: null, metadata: null` (graceful).
    ///
    /// No query param = backward-compat (whole-file fetch).
    /// The result includes the R4 metadata (`size_bytes` / `modified_at` /
    /// nested `metadata` field).
    pub async fn fetch_file(&self, raw_path: &str) -> WireResult<serde_json::Value> {
        self.fetch_file_impl(raw_path, TailMode::Full).await
    }

    async fn fetch_file_impl(
        &self,
        raw_path: &str,
        mode: TailMode,
    ) -> WireResult<serde_json::Value> {
        let resolved = resolve_file_path(raw_path)?;

        // Graceful: non-existent path → body: null, metadata: null (not a WireError)
        if !resolved.exists() {
            return Ok(serde_json::json!({
                "scheme": "file",
                "kind": "file",
                "path": resolved.display().to_string(),
                "body": serde_json::Value::Null,
                "metadata": serde_json::Value::Null,
            }));
        }

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
            let meta_json = build_file_metadata(&newest);
            let body = apply_tail(&body_full, &mode);
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "newest_in_dir",
                "dir": resolved.display().to_string(),
                "path": newest.display().to_string(),
                "body": body,
                "size_bytes": size_bytes,
                "modified_at": modified_at,
                "metadata": meta_json,
            }))
        } else {
            let body_full = std::fs::read_to_string(&resolved)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            let size_bytes = meta.len();
            let modified_at = mtime_unix(&meta);
            let meta_json = build_file_metadata(&resolved);
            let body = apply_tail(&body_full, &mode);
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "file",
                "path": resolved.display().to_string(),
                "body": body,
                "size_bytes": size_bytes,
                "modified_at": modified_at,
                "metadata": meta_json,
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
        // The file URI accepts lenient, non-RFC forms like `file://~/foo`, so
        // extract the path part from the raw string via strip_prefix (typed
        // host/path would treat `~` as a host and change the behavior).
        let source_uri = uri.as_raw();
        let rest = source_uri
            .strip_prefix("file://")
            .or_else(|| source_uri.strip_prefix("file:"))
            .ok_or_else(|| WireError::Storage(format!("file adapter: bad uri: {source_uri}")))?;
        let mode = parse_tail_mode(uri);
        self.fetch_file_impl(rest, mode).await
    }
}

/// Determines the [`TailMode`] from the `WireUri` query params.
///
/// - `?tail=last_section` → [`TailMode::LastSection`]
/// - `?tail_n=N` (integer N > 0) → [`TailMode::LastN`] (N clamped to [`TAIL_N_MAX`])
/// - unknown value / unparsable / N=0 → [`TailMode::Full`] (graceful fail)
fn parse_tail_mode(uri: &WireUri) -> TailMode {
    if let Some(tail) = uri.query_get("tail") {
        if tail == "last_section" {
            return TailMode::LastSection;
        }
        // Unknown value → graceful fail = Full
        return TailMode::Full;
    }
    if let Some(n_str) = uri.query_get("tail_n") {
        if let Ok(n) = n_str.parse::<usize>() {
            if n > 0 {
                return TailMode::LastN(n.min(TAIL_N_MAX));
            }
        }
        // Unparsable / n=0 → graceful fail = Full
        return TailMode::Full;
    }
    TailMode::Full
}

/// Applies `mode` to `body` and returns the resulting substring.
///
/// - [`TailMode::Full`]        — returns `body` unchanged
/// - [`TailMode::LastSection`] — returns from the last `## ` h2 heading line to the end
/// - [`TailMode::LastN`]       — returns the last N lines joined with `"\n"`
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

/// Returns the byte position of the last markdown h2 heading (a line starting
/// with `## `) in `body`. Returns `0` when none is found (= return the whole body).
fn last_h2_pos(body: &str) -> usize {
    let needle = "\n## ";
    if let Some(pos) = body.rfind(needle) {
        // Return from the byte after `\n` (= the leading `#`)
        return pos + 1;
    }
    if body.starts_with("## ") {
        return 0;
    }
    0
}

/// Extracts Unix epoch seconds from `std::fs::Metadata`. Returns `0` when unavailable.
fn mtime_unix(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Builds the R4 metadata JSON from stat(path). Returns Null on stat failure
/// (no panic). `last_modified` is UNIX epoch seconds (u64); `age_days` is the
/// difference from now in days (u64). No chrono dependency — uses
/// std::time::SystemTime only.
fn build_file_metadata(path: &std::path::Path) -> serde_json::Value {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let size_bytes = meta.len();
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let full_path = path.display().to_string();
            let last_modified: Option<u64> = meta.modified().ok().and_then(|mtime| {
                mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            });
            let age_days: Option<u64> = meta.modified().ok().and_then(|mtime| {
                std::time::SystemTime::now()
                    .duration_since(mtime)
                    .ok()
                    .map(|d| d.as_secs() / 86400)
            });
            serde_json::json!({
                "filename": filename,
                "full_path": full_path,
                "last_modified": last_modified,
                "size_bytes": size_bytes,
                "age_days": age_days,
            })
        }
        Err(_) => serde_json::Value::Null,
    }
}

fn resolve_file_path(raw: &str) -> WireResult<PathBuf> {
    // `~/...` -> $HOME expansion
    // Strip `#fragment` and `?query` from the path (both are invalid for
    // filesystem lookup)
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

    // ---- helpers ----

    fn write_test_file(name: &str, content: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("pw_adapter_test_{name}"));
        std::fs::write(&path, content).expect("write temp file");
        path
    }

    // ---- existing tests (backward-compat) ----

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

    // ---- R4: metadata (topic style — flat top-level fields) ----

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
        // A 5-line file with tail_n=2000 (> TAIL_N_MAX=1000) → clamp → all 5 lines returned
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
        // R5: body is the last section only
        let body = v["body"].as_str().unwrap();
        assert!(
            body.starts_with("## Section 2"),
            "R5: last section; got: {body}"
        );
        assert!(!body.contains("Section 1"), "R5: no earlier section");
        // R4: size_bytes is the original whole-file size (not the post-tail body)
        assert_eq!(
            v["size_bytes"].as_u64().unwrap(),
            full_size,
            "R4: size_bytes = full file size"
        );
        // R4: modified_at is present as u64
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

    // ---- R4 tests (metadata expose — main style: nested metadata object) ----

    #[tokio::test]
    async fn r4_metadata_present_for_existing_file() {
        // Use this source file itself — guaranteed to exist at test time.
        let me = file!();
        let abs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(me);
        let a = FileAdapter;
        let v = a.fetch_file(&abs.display().to_string()).await.unwrap();
        let meta = &v["metadata"];
        assert!(
            !meta.is_null(),
            "metadata should be present for an existing file"
        );
        assert!(meta["filename"].is_string(), "filename should be a string");
        assert!(
            meta["full_path"].is_string(),
            "full_path should be a string"
        );
        assert!(
            meta["size_bytes"].is_number(),
            "size_bytes should be a number"
        );
        // age_days may be null on platforms without mtime, but should be present as a key
        assert!(meta.get("age_days").is_some(), "age_days key should exist");
    }

    #[tokio::test]
    async fn r4_metadata_null_for_nonexistent_file() {
        let a = FileAdapter;
        let v = a
            .fetch_file("/tmp/__persona_wire_nonexistent_r4_test_file__")
            .await
            .unwrap();
        assert!(
            v["body"].is_null(),
            "body should be null for a non-existent file"
        );
        assert!(
            v["metadata"].is_null(),
            "metadata should be null for a non-existent file"
        );
    }

    #[tokio::test]
    async fn r4_body_backward_compat() {
        // body field should still be a string for an existing file (backward-compat).
        let me = file!();
        let abs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(me);
        let a = FileAdapter;
        let v = a.fetch_file(&abs.display().to_string()).await.unwrap();
        assert!(
            v["body"].is_string(),
            "body should remain a string for an existing file"
        );
        assert!(
            v["body"].as_str().unwrap().contains("Layer 6 Adapter"),
            "body should contain expected file content"
        );
    }

    #[tokio::test]
    async fn r4_metadata_field_types() {
        // filename must match the basename; full_path must be absolute.
        let me = file!();
        let abs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(me);
        let a = FileAdapter;
        let v = a.fetch_file(&abs.display().to_string()).await.unwrap();
        let meta = &v["metadata"];
        let filename = meta["filename"].as_str().unwrap();
        assert_eq!(filename, "adapter.rs", "filename should be the basename");
        let full_path = meta["full_path"].as_str().unwrap();
        assert!(
            full_path.ends_with("adapter.rs"),
            "full_path should end with adapter.rs"
        );
        assert!(
            full_path.starts_with('/'),
            "full_path should be an absolute path"
        );
    }

    // ---- Pageable / Cursor (Layer 1) ----

    #[test]
    fn cursor_variants_construct_and_compare() {
        let page_number = Cursor::PageNumber(2);
        let link_header = Cursor::LinkHeader("https://api.example.com?page=3".to_string());
        let next_token = Cursor::NextToken("abc123".to_string());
        let offset = Cursor::Offset(50);

        // equality within the same variant
        assert_eq!(page_number, Cursor::PageNumber(2));
        assert_eq!(
            link_header.clone(),
            Cursor::LinkHeader("https://api.example.com?page=3".to_string())
        );
        assert_eq!(next_token.clone(), Cursor::NextToken("abc123".to_string()));
        assert_eq!(offset, Cursor::Offset(50));

        // inequality within the same variant (different payload)
        assert_ne!(page_number, Cursor::PageNumber(3));
        assert_ne!(next_token, Cursor::NextToken("xyz789".to_string()));

        // inequality across variants
        assert_ne!(page_number, link_header);
        assert_ne!(link_header, offset);
        assert_ne!(Cursor::Offset(2), Cursor::PageNumber(2));
    }

    #[test]
    fn pageable_is_object_safe() {
        // Compile-time-only assertion: `dyn Pageable` must be constructible
        // as a trait object (Send + Sync + no non-dispatchable generics).
        fn _assert_object_safe(_p: &dyn Pageable) {}
    }

    struct MockPageableAdapter;

    #[async_trait]
    impl Pageable for MockPageableAdapter {
        fn max_page_size(&self) -> usize {
            2
        }

        async fn fetch_page(
            &self,
            _uri: &WireUri,
            cursor: Option<Cursor>,
        ) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
            match cursor {
                None => Ok((
                    vec![serde_json::json!({"id": 1}), serde_json::json!({"id": 2})],
                    Some(Cursor::NextToken("page-2".to_string())),
                )),
                Some(Cursor::NextToken(ref token)) if token == "page-2" => {
                    Ok((vec![serde_json::json!({"id": 3})], None))
                }
                Some(other) => Err(WireError::Storage(format!(
                    "mock adapter: unexpected cursor: {other:?}"
                ))),
            }
        }
    }

    #[tokio::test]
    async fn mock_pageable_adapter_fetch_page_threads_cursor() {
        let adapter = MockPageableAdapter;
        let uri = WireUri::parse("mock://items").unwrap();

        assert_eq!(adapter.max_page_size(), 2);

        let (first_items, next) = adapter.fetch_page(&uri, None).await.unwrap();
        assert_eq!(first_items.len(), 2);
        assert_eq!(next, Some(Cursor::NextToken("page-2".to_string())));

        let (second_items, next2) = adapter.fetch_page(&uri, next).await.unwrap();
        assert_eq!(second_items.len(), 1);
        assert_eq!(next2, None);
    }

    #[test]
    fn mock_pageable_adapter_wrap_items_default_shape() {
        // MockPageableAdapter does not override `wrap_items` — it must fall
        // back to the trait's default `{count, items}` shape (no `scheme`).
        let adapter = MockPageableAdapter;
        let uri = WireUri::parse("mock://items").unwrap();
        let items = vec![serde_json::json!({"id": 1}), serde_json::json!({"id": 2})];

        let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();

        assert_eq!(wrapped["count"], 2);
        assert_eq!(wrapped["items"], serde_json::Value::Array(items));
        assert!(
            wrapped.get("scheme").is_none(),
            "default wrap_items must not add a `scheme` field: {wrapped}"
        );
    }

    // ---- Adapter::as_pageable (Layer 2) ----

    #[async_trait]
    impl Adapter for MockPageableAdapter {
        fn scheme(&self) -> &'static str {
            "mock"
        }

        async fn fetch(&self, _uri: &WireUri) -> WireResult<serde_json::Value> {
            Ok(serde_json::json!({"scheme": "mock", "count": 0, "items": []}))
        }

        fn as_pageable(&self) -> Option<&dyn Pageable> {
            Some(self)
        }
    }

    #[test]
    fn as_pageable_default_returns_none() {
        // FileAdapter implements Adapter without overriding as_pageable —
        // the default impl must return None (backward-compat).
        let a = FileAdapter;
        assert!(a.as_pageable().is_none());
    }

    #[test]
    fn as_pageable_override_returns_some() {
        let a = MockPageableAdapter;
        let pageable = a.as_pageable();
        assert!(pageable.is_some(), "override should return Some(self)");
        assert_eq!(pageable.unwrap().max_page_size(), 2);
    }
}
