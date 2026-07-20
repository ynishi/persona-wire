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
//!   Query param extensions (R5, now routed through the unified
//!   [`crate::infrastructure::filter`] vocabulary via
//!   [`Adapter::filter_caps`] / [`crate::infrastructure::filter::WireFilters::parse`]):
//!   - `?tail=last_section` — the trailing section (split at markdown `## `
//!     h2 boundaries; returns everything from the last h2 onward)
//!   - `?tail_n=<N>` — the last N lines (line-based; capped at
//!     [`TAIL_N_MAX`] = 1000 lines as a context size guard)
//!   - `?lines=<FROM>-<TO>` — a 1-origin inclusive line range; `TO` beyond
//!     the total line count clamps gracefully, `FROM` beyond the total
//!     returns an empty body. Mutually exclusive with `?tail` / `?tail_n`
//!     (fails loud if both are present).
//!   - no query param → fetch the whole file (backward-compat)
//!   - unknown / unparsable values → **fail loud** (`Err`), per the unified
//!     filter error policy (behavior changed from the earlier graceful
//!     whole-file fallback; see `filter` module docs)
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
//! - **Pagination**: adapters that support `?limit=N` where `N` can exceed a
//!   single upstream page MUST drive the pagination loop internally inside
//!   `Adapter::fetch` and emit a truthful `has_more` field distinguishing
//!   truncated-at-limit from upstream-exhausted. Cursor form (Link header,
//!   NextToken, offset, ...) is a private implementation detail — the wire
//!   layer never sees it. Adapters that ignore `?limit` (single-shot fetches
//!   such as `FileAdapter`) simply return their canonical shape.
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
//!
//! ### `?auth=<service_key>` query param convention (Phase 1, decided
//! together with `application::auth`)
//!
//! Every HTTP-authenticated adapter honors an optional `?auth=<service_key>`
//! query param on its `source_uri`:
//!
//! - `<service_key>` is a **credential reference key only** — never a
//!   secret. It is looked up via
//!   `persona_wire_credentials::Credentials::get(service_key)` (env var →
//!   OS keyring), exactly like the adapter's own literal default service
//!   name (e.g. `"github"`).
//! - When present, `<service_key>` **overrides** the adapter's literal
//!   default service name for that one fetch (e.g. `?auth=github-alt` looks
//!   up the `github-alt` credential instead of `github`) — lets one wiring
//!   entry authenticate as a different identity than another entry using
//!   the same adapter/scheme.
//! - When absent, behavior is unchanged: the adapter's literal default
//!   service name is used (full backward compatibility).
//! - This is an ordinary query key from every adapter's own URI-grammar
//!   perspective — it follows the same "unknown query keys are silently
//!   ignored" convention documented above, so adding `?auth=` never
//!   conflicts with an adapter's own `?kind=` / `?limit=` / etc. params.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::filter::{FilterCap, WireFilters};
use crate::infrastructure::wire_uri::WireUri;
use async_trait::async_trait;

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

    /// Declares the cross-cutting [`FilterCap`]s this adapter interprets via
    /// [`WireFilters::parse`]. Default is empty (no cross-cutting filter
    /// support) so every pre-existing adapter compiles unchanged; opting in
    /// is additive.
    fn filter_caps(&self) -> &'static [FilterCap] {
        &[]
    }

    /// Whether the wire layer may claim filter-vocabulary keys this adapter
    /// did not declare and apply them itself, post-fetch (GH #10 — see
    /// [`WireFilters::split_post`]). Default `true`: the vocabulary keys
    /// (`limit` / `lines` / `tail` / `tail_n` / `since` / `until` / `query`)
    /// are reserved cross-cutting names, so a normal adapter never interprets
    /// an undeclared one as addressing.
    ///
    /// Passthrough adapters whose URI grammar forwards arbitrary query keys
    /// to an upstream (e.g. `mcp://`, where `?query=` becomes a tool
    /// argument) MUST override this to `false` — stripping a vocabulary key
    /// from their URI would change the upstream call.
    fn post_filterable(&self) -> bool {
        true
    }

    /// Interprets the parsed `WireUri` per scheme and returns fresh data as a
    /// `serde_json::Value`.
    ///
    /// Adapters honoring `?limit=N` MUST return `min(N, upstream)` items and
    /// emit a truthful `has_more` field in their canonical response shape,
    /// distinguishing "truncated at limit" from "upstream exhausted".
    /// Pagination against the upstream API is an implementation detail of the
    /// adapter — the wire layer never sees cursor state.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value>;
}

// ---- file adapter (std::fs) ----

/// Bundled `file://` / `file:` scheme [`Adapter`] backed by `std::fs`. See
/// the module-level docs for the URI grammar (query param extensions) and
/// output shape.
pub struct FileAdapter;

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
        self.fetch_file_impl(raw_path, &WireFilters::default())
            .await
    }

    async fn fetch_file_impl(
        &self,
        raw_path: &str,
        filters: &WireFilters,
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
            let body = apply_filters(&body_full, filters);
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
            let body = apply_filters(&body_full, filters);
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

    fn filter_caps(&self) -> &'static [FilterCap] {
        &[FilterCap::LineRange, FilterCap::Tail { n_max: TAIL_N_MAX }]
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
        let filters = WireFilters::parse(uri, self.filter_caps())?;
        if filters.line_range.is_some() && filters.tail.is_some() {
            return Err(WireError::Storage(
                "lines and tail are mutually exclusive".to_string(),
            ));
        }
        self.fetch_file_impl(rest, &filters).await
    }
}

/// Applies the parsed [`WireFilters`] to `body` and returns the resulting
/// substring. Thin delegate to [`WireFilters::apply_to_text`] — the
/// text-slicing engine moved to the shared filter module so document
/// adapters outside this crate (obsidian / future ones) reuse the same
/// semantics (GH #6 Phase 3).
fn apply_filters(body: &str, filters: &WireFilters) -> String {
    filters.apply_to_text(body)
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
    use crate::infrastructure::filter::TailSpec;

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

    // ---- R5 → unified filter policy: fail loud (was graceful) — ?tail=invalid ----
    //
    // Behavior change (adapter-filter-if Phase 1, decided): `?tail=unknown`
    // used to silently fall back to whole-file fetch. Under the unified
    // filter error policy (see `infrastructure::filter` module docs), a
    // type-invalid filter value is now `Err` (fail loud).

    #[tokio::test]
    async fn file_adapter_r5_tail_invalid_fails_loud() {
        let content = "some content\n";
        let path = write_test_file("r5_tail_inv.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail=invalid", path.display())).unwrap();
        let a = FileAdapter;
        let r = a.fetch(&uri).await;
        assert!(r.is_err(), "unknown ?tail= value should fail loud");
    }

    // ---- R5 → unified filter policy: fail loud (was graceful) — ?tail_n=abc ----

    #[tokio::test]
    async fn file_adapter_r5_tail_n_invalid_fails_loud() {
        let content = "some content\n";
        let path = write_test_file("r5_tail_n_inv.txt", content);
        let uri = WireUri::parse(&format!("file://{}?tail_n=abc", path.display())).unwrap();
        let a = FileAdapter;
        let r = a.fetch(&uri).await;
        assert!(r.is_err(), "non-numeric ?tail_n= value should fail loud");
    }

    // ---- ?lines=FROM-TO ----

    #[tokio::test]
    async fn file_adapter_lines_normal_range() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let path = write_test_file("lines_normal.txt", content);
        let uri = WireUri::parse(&format!("file://{}?lines=2-4", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["body"].as_str().unwrap(), "line2\nline3\nline4");
    }

    #[tokio::test]
    async fn file_adapter_lines_to_beyond_total_clamps_gracefully() {
        let content = "line1\nline2\nline3\n";
        let path = write_test_file("lines_over.txt", content);
        let uri = WireUri::parse(&format!("file://{}?lines=2-100", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["body"].as_str().unwrap(), "line2\nline3");
    }

    #[tokio::test]
    async fn file_adapter_lines_from_beyond_total_returns_empty() {
        let content = "line1\nline2\n";
        let path = write_test_file("lines_from_over.txt", content);
        let uri = WireUri::parse(&format!("file://{}?lines=10-20", path.display())).unwrap();
        let a = FileAdapter;
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["body"].as_str().unwrap(), "");
    }

    // ---- lines + tail mutual exclusivity ----

    #[tokio::test]
    async fn file_adapter_lines_and_tail_n_mutually_exclusive() {
        let content = "line1\nline2\nline3\n";
        let path = write_test_file("lines_and_tail.txt", content);
        let uri = WireUri::parse(&format!("file://{}?lines=1-2&tail_n=1", path.display())).unwrap();
        let a = FileAdapter;
        let r = a.fetch(&uri).await;
        assert!(
            r.is_err(),
            "lines and tail_n together should fail loud (mutually exclusive)"
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

    // ---- unit tests: apply_filters delegate ----
    // (text-slicing engine unit tests live in `infrastructure::filter`
    // next to `WireFilters::apply_to_text` — GH #6 Phase 3 promotion)

    #[test]
    fn apply_filters_delegates_to_shared_engine() {
        let filters = WireFilters {
            line_range: Some((2, 4)),
            ..Default::default()
        };
        assert_eq!(apply_filters("a\nb\nc\nd\ne\n", &filters), "b\nc\nd");
        let tail = WireFilters {
            tail: Some(TailSpec::LastN(2)),
            ..Default::default()
        };
        assert_eq!(apply_filters("a\nb\nc\n", &tail), "b\nc");
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
}
