//! persona-wire Adapter for GitHub (scheme `github://`).
//!
//! ## Architecture
//!
//! `GithubAdapter` is a stateless [`Adapter`] impl split into three
//! independent functions:
//!
//! - [`parse_github_uri`] — `WireUri` → `GithubUriSpec` (owner + repo + kind +
//!   state + limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
//!   GitHub-specific knowledge in the transport layer).
//! - [`normalize_github`] — raw GitHub REST API response (JSON array) → the
//!   Wire JSON shape below, branching only on `kind`.
//!
//! ## URI grammar
//!
//! ```text
//! github://<owner>/<repo>[?kind=issues|pulls|releases][&state=open|closed|all][&limit=N]
//! ```
//!
//! - `host` is the repo owner (org or user); an empty host fails loud.
//! - The first path segment is the repo name; a missing repo, or any
//!   additional path segment beyond it, fails loud.
//! - `kind` defaults to `issues`. Unlike `?scheme=` in
//!   `persona-wire-adapter-rss` (which falls back gracefully on an unknown
//!   value), an invalid `kind` **fails loud**: a typo here silently returns a
//!   different class of data (pulls instead of issues, say) rather than the
//!   graceful-fallback shape mismatch RSS's `scheme=` guards against, so the
//!   safer default is to reject it outright.
//! - `state` defaults to `open` and only applies to `issues` / `pulls`; for
//!   `kind=releases` it is silently ignored (not read, not validated) since
//!   GitHub releases have no `state` field. For `issues` / `pulls`, an
//!   invalid value (anything other than `open` / `closed` / `all`) fails
//!   loud.
//! - `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
//!   A non-numeric or zero value fails loud. For `kind=issues`, the GitHub
//!   `per_page` sent upstream is over-fetched (4× `limit`, capped at
//!   [`GITHUB_PER_PAGE_MAX`]) so that up to `limit` real issues can still be
//!   returned even when the repo mixes many pull requests into the
//!   `/issues` endpoint (see "GitHub's `/issues` endpoint mixes..." note
//!   below). `pulls` and `releases` fetch `per_page = limit` directly, since
//!   there is no post-fetch filtering for those kinds. This over-fetch has
//!   no effect when `limit >= 25`, since `4 * 25 = 100` already saturates
//!   the GitHub API's `per_page` ceiling.
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss`).
//!
//! ## Auth
//!
//! Resolved per-fetch (not at boot) via
//! `persona_wire_credentials::Credentials::default_chain().get("github")`, so
//! a token change takes effect without restarting the process and avoids a
//! keychain prompt on every boot when no token is configured. Set a token via
//! the `PERSONA_WIRE_TOKEN_GITHUB` or `GITHUB_TOKEN` environment variable, or
//! store one in the OS keychain via `persona-wire token set github`.
//!
//! When no token resolves, the adapter proceeds unauthenticated — this works
//! for public repos but is subject to GitHub's unauthenticated rate limit
//! (60 requests/hour per IP). A backend error while resolving the token
//! (e.g. keychain access denied) fails loud and propagates; only "no token
//! configured" is treated as `None`.
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "repo": { "owner": "...", "name": "..." },
//!   "kind": "issues",
//!   "items": [ ... ]
//! }
//! ```
//!
//! `items` entries for `kind=issues` / `kind=pulls`:
//!
//! ```json
//! {
//!   "number": 1, "title": "...|null", "state": "...|null",
//!   "author": "...|null", "created_at": "...|null", "updated_at": "...|null",
//!   "url": "...|null", "labels": ["..."], "body_excerpt": "...|null"
//! }
//! ```
//!
//! `items` entries for `kind=releases`:
//!
//! ```json
//! {
//!   "tag": "...|null", "name": "...|null", "published_at": "...|null",
//!   "url": "...|null", "prerelease": true, "body_excerpt": "...|null"
//! }
//! ```
//!
//! GitHub's `/issues` endpoint mixes pull requests into the response (any
//! entry that carries a `pull_request` key); `kind=issues` filters those out
//! before normalizing, so the returned `items` count can be lower than the
//! requested `limit`.
//!
//! ## Pagination (Layer 3a of GH #1)
//!
//! `GithubAdapter` implements [`Pageable`]: when a caller requests
//! `?limit=N` with `N` greater than [`GITHUB_PER_PAGE_MAX`] (100), the
//! wire-layer driver (`persona_wire_core::application::use_cases`) threads a
//! [`Cursor::LinkHeader`] extracted from GitHub's RFC 5988 `Link` response
//! header across repeated requests instead of the single capped fetch
//! `Adapter::fetch` performs. `parse_github_uri`'s `limit` parsing already
//! accepted values above 100 before this adapter had any pagination
//! support (the parser never validated an upper bound); those requests now
//! actually retrieve more than one page instead of being silently capped
//! by the first `per_page` request. `limit <= 100` is unaffected — it stays
//! on the existing single-request fast path.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{
    adapter::{Adapter, Cursor, Pageable},
    wire_uri::WireUri,
};
use persona_wire_core::{WireError, WireResult};
use persona_wire_credentials::Credentials;
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `items` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-adapter-rss::FETCH_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Max `body_excerpt` length in `char`s before truncation (context size guard).
pub const BODY_MAX_CHARS: usize = 500;

/// GitHub REST API base URL.
pub const API_BASE: &str = "https://api.github.com";

/// GitHub REST API's maximum allowed `per_page` value.
pub const GITHUB_PER_PAGE_MAX: usize = 100;

/// persona-wire Adapter for GitHub (`github://` scheme).
pub struct GithubAdapter;

#[async_trait]
impl Adapter for GithubAdapter {
    fn scheme(&self) -> &'static str {
        "github"
    }

    /// Fetch `spec.kind` items for the repo derived from `uri` and normalize
    /// them. See the module docs for URI grammar, auth resolution, and
    /// output shape.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_github_uri(uri)?;
        let client = github_http_client()?;
        let raw = client.get_json(&spec.endpoint_url()).await?;
        normalize_github(&spec, &raw)
    }

    /// Opts into the wire-layer pagination driver (Layer 3a of GH #1). See
    /// the module docs "Pagination" section.
    fn as_pageable(&self) -> Option<&dyn Pageable> {
        Some(self)
    }
}

/// Builds a fresh, GitHub-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth"). Shared by `Adapter::fetch` and
/// `Pageable::fetch_page` so both paths stay in sync on headers/timeout.
fn github_http_client() -> WireResult<HttpClient> {
    let token = Credentials::default_chain().get("github")?;
    let mut client = HttpClient::new("github adapter")
        .with_timeout(FETCH_TIMEOUT)
        .with_header("Accept", "application/vnd.github+json")
        .with_header("X-GitHub-Api-Version", "2022-11-28")
        // GitHub's REST API rejects requests without a User-Agent (403).
        .with_header("User-Agent", "persona-wire");
    if let Some(token) = token {
        client = client.with_bearer(token);
    }
    Ok(client)
}

/// The three GitHub REST endpoints this adapter can target, selected via the
/// `?kind=` query param.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GithubKind {
    Issues,
    Pulls,
    Releases,
}

impl GithubKind {
    fn as_str(self) -> &'static str {
        match self {
            GithubKind::Issues => "issues",
            GithubKind::Pulls => "pulls",
            GithubKind::Releases => "releases",
        }
    }
}

/// Parsed `github://` URI: owner + repo + endpoint kind + issue/pull state
/// filter + item limit.
#[derive(Debug)]
struct GithubUriSpec {
    owner: String,
    repo: String,
    kind: GithubKind,
    /// `None` when `kind == Releases` (state does not apply; see module docs).
    state: Option<String>,
    limit: usize,
}

impl GithubUriSpec {
    /// Builds the full GitHub REST API request URL for this spec.
    ///
    /// `kind=issues` over-fetches `per_page` (4× `limit`, capped at
    /// [`GITHUB_PER_PAGE_MAX`]) since GitHub's `/issues` endpoint mixes in
    /// pull requests that [`normalize_github`] filters out post-fetch; see
    /// the module docs "URI grammar" section. `pulls` and `releases` have no
    /// post-fetch filtering, so `per_page = limit` directly.
    fn endpoint_url(&self) -> String {
        match self.kind {
            GithubKind::Issues => {
                let state = self.state.as_deref().unwrap_or("open");
                // `saturating_mul(4)` is always >= `self.limit` for `limit >= 1`
                // (guaranteed by `parse_github_uri`'s `limit == 0` rejection), so
                // `min` alone suffices here; `clamp(self.limit, MAX)` would panic
                // when `self.limit > GITHUB_PER_PAGE_MAX` (unbounded URI input).
                let per_page = self.limit.saturating_mul(4).min(GITHUB_PER_PAGE_MAX);
                format!(
                    "{API_BASE}/repos/{}/{}/issues?state={state}&per_page={per_page}",
                    self.owner, self.repo,
                )
            }
            GithubKind::Pulls => {
                let state = self.state.as_deref().unwrap_or("open");
                format!(
                    "{API_BASE}/repos/{}/{}/pulls?state={state}&per_page={}",
                    self.owner, self.repo, self.limit
                )
            }
            GithubKind::Releases => {
                format!(
                    "{API_BASE}/repos/{}/{}/releases?per_page={}",
                    self.owner, self.repo, self.limit
                )
            }
        }
    }

    /// Builds the request URL for the *first page* of the wire-layer
    /// pagination loop (Layer 3a of GH #1, `Pageable::fetch_page` with
    /// `cursor = None`).
    ///
    /// Unlike [`Self::endpoint_url`] (which scales `per_page` to the
    /// caller's requested `limit`, including the 4× over-fetch heuristic for
    /// `issues`), this always requests `per_page = `[`GITHUB_PER_PAGE_MAX`]
    /// — the wire-layer driver caps the total item count by breaking the
    /// loop early once enough items accumulate across pages, so there is no
    /// `limit`-based over-fetch heuristic to apply here.
    fn endpoint_url_for_first_page(&self) -> String {
        match self.kind {
            GithubKind::Issues => {
                let state = self.state.as_deref().unwrap_or("open");
                format!(
                    "{API_BASE}/repos/{}/{}/issues?state={state}&per_page={GITHUB_PER_PAGE_MAX}",
                    self.owner, self.repo,
                )
            }
            GithubKind::Pulls => {
                let state = self.state.as_deref().unwrap_or("open");
                format!(
                    "{API_BASE}/repos/{}/{}/pulls?state={state}&per_page={GITHUB_PER_PAGE_MAX}",
                    self.owner, self.repo,
                )
            }
            GithubKind::Releases => {
                format!(
                    "{API_BASE}/repos/{}/{}/releases?per_page={GITHUB_PER_PAGE_MAX}",
                    self.owner, self.repo,
                )
            }
        }
    }
}

/// Resolves the request URL for one `Pageable::fetch_page` call from the
/// pagination `Cursor`.
///
/// - `cursor = None` → the first-page endpoint ([`GithubUriSpec::endpoint_url_for_first_page`]).
/// - `Some(Cursor::LinkHeader(url))` → that URL, used literally (GitHub's
///   `Link` header already carries the full next-page URL including query
///   params — no rebuild needed).
/// - Any other `Cursor` variant → fails loud with [`WireError::Storage`].
///   GitHub only ever produces `LinkHeader` cursors, so seeing another
///   variant here means caller confusion (e.g. threading a cursor from a
///   different adapter), not a legitimate pagination state.
///
/// A free function (not inlined into `fetch_page`) so the cursor→URL
/// decision is unit-testable offline without a live HTTP round-trip.
fn resolve_fetch_page_url(spec: &GithubUriSpec, cursor: &Option<Cursor>) -> WireResult<String> {
    match cursor {
        None => Ok(spec.endpoint_url_for_first_page()),
        Some(Cursor::LinkHeader(url)) => Ok(url.clone()),
        Some(other) => Err(WireError::Storage(format!(
            "github adapter: unsupported cursor variant for pagination: {other:?}"
        ))),
    }
}

#[async_trait]
impl Pageable for GithubAdapter {
    /// GitHub's REST list API `per_page` ceiling.
    fn max_page_size(&self) -> usize {
        GITHUB_PER_PAGE_MAX
    }

    /// Fetches one page (see [`resolve_fetch_page_url`] for the cursor→URL
    /// decision) and normalizes it the same way [`normalize_github`] does
    /// (same PR-filter for `kind=issues`), without truncating to `limit` —
    /// the wire-layer driver truncates across accumulated pages.
    async fn fetch_page(
        &self,
        uri: &WireUri,
        cursor: Option<Cursor>,
    ) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
        let spec = parse_github_uri(uri)?;
        let url = resolve_fetch_page_url(&spec, &cursor)?;

        let client = github_http_client()?;
        let (raw, next_link) = client.get_json_with_next_link(&url).await?;

        let arr = response_array(&spec.owner, &spec.repo, spec.kind, &raw)?;
        let items = normalize_items(spec.kind, arr);
        let next_cursor = next_link.map(Cursor::LinkHeader);
        Ok((items, next_cursor))
    }

    /// Preserves `Adapter::fetch`'s `{repo, kind, items}` output shape across
    /// the pagination path (see module docs "Pagination").
    fn wrap_items(
        &self,
        items: Vec<serde_json::Value>,
        uri: &WireUri,
    ) -> WireResult<serde_json::Value> {
        let spec = parse_github_uri(uri)?;
        Ok(serde_json::json!({
            "repo": { "owner": spec.owner, "name": spec.repo },
            "kind": spec.kind.as_str(),
            "items": items,
        }))
    }
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`GithubUriSpec`]. See the module-level "URI grammar" section for
/// the exact rules and failure conditions.
fn parse_github_uri(uri: &WireUri) -> WireResult<GithubUriSpec> {
    let owner = uri
        .host()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            WireError::Storage(format!(
                "github adapter: missing owner (host) in '{}'",
                uri.as_raw()
            ))
        })?
        .to_string();

    let segments: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    let repo = match segments.as_slice() {
        [] => {
            return Err(WireError::Storage(format!(
                "github adapter: missing repo in '{}' (expected github://<owner>/<repo>)",
                uri.as_raw()
            )));
        }
        [repo] => repo.to_string(),
        _ => {
            return Err(WireError::Storage(format!(
                "github adapter: unexpected extra path segment(s) in '{}' (expected github://<owner>/<repo>)",
                uri.as_raw()
            )));
        }
    };

    let kind = match uri.query_get("kind") {
        None | Some("issues") => GithubKind::Issues,
        Some("pulls") => GithubKind::Pulls,
        Some("releases") => GithubKind::Releases,
        Some(bad) => {
            return Err(WireError::Storage(format!(
                "github adapter: invalid kind '{bad}' (must be one of: issues, pulls, releases)"
            )));
        }
    };

    // `state` only applies to issues/pulls; for releases it is silently
    // ignored (not even validated) since GitHub releases have no `state`
    // field (see module docs "URI grammar").
    let state = if kind == GithubKind::Releases {
        None
    } else {
        match uri.query_get("state") {
            None => Some("open".to_string()),
            Some(s @ ("open" | "closed" | "all")) => Some(s.to_string()),
            Some(bad) => {
                return Err(WireError::Storage(format!(
                    "github adapter: invalid state '{bad}' (must be one of: open, closed, all)"
                )));
            }
        }
    };

    let limit = match uri.query_get("limit") {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "github adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "github adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            n
        }
        None => DEFAULT_LIMIT,
    };

    Ok(GithubUriSpec {
        owner,
        repo,
        kind,
        state,
        limit,
    })
}

/// Normalize a GitHub REST API response (`raw`, expected to be a JSON array)
/// per `spec.kind` into the Wire JSON shape (see module docs "Output shape").
fn normalize_github(
    spec: &GithubUriSpec,
    raw: &serde_json::Value,
) -> WireResult<serde_json::Value> {
    let arr = response_array(&spec.owner, &spec.repo, spec.kind, raw)?;
    let items: Vec<serde_json::Value> = normalize_items(spec.kind, arr)
        .into_iter()
        .take(spec.limit)
        .collect();

    Ok(serde_json::json!({
        "repo": { "owner": spec.owner, "name": spec.repo },
        "kind": spec.kind.as_str(),
        "items": items,
    }))
}

/// Extracts the raw JSON array from a GitHub REST API response, failing
/// loud (naming the repo + kind) when the response isn't a JSON array.
/// Shared by [`normalize_github`] and `Pageable::fetch_page`.
fn response_array<'a>(
    owner: &str,
    repo: &str,
    kind: GithubKind,
    raw: &'a serde_json::Value,
) -> WireResult<&'a Vec<serde_json::Value>> {
    raw.as_array().ok_or_else(|| {
        WireError::Storage(format!(
            "github adapter: unexpected response shape for {owner}/{repo} ({}): expected a JSON array",
            kind.as_str()
        ))
    })
}

/// Normalizes every entry in `arr` per `kind`, with no `limit` truncation
/// (callers apply `.take(limit)` themselves — the pagination path
/// (`Pageable::fetch_page`) normalizes a full page and lets the wire-layer
/// driver truncate across accumulated pages instead).
///
/// GitHub's `/issues` endpoint mixes in pull requests (entries carrying a
/// `pull_request` key); `kind=issues` filters those out first, per module
/// docs "URI grammar" / "Pagination".
fn normalize_items(kind: GithubKind, arr: &[serde_json::Value]) -> Vec<serde_json::Value> {
    match kind {
        GithubKind::Issues => arr
            .iter()
            .filter(|v| v.get("pull_request").is_none())
            .map(normalize_issue_or_pull)
            .collect(),
        GithubKind::Pulls => arr.iter().map(normalize_issue_or_pull).collect(),
        GithubKind::Releases => arr.iter().map(normalize_release).collect(),
    }
}

/// Normalize a single GitHub issue or pull-request JSON object (same shape
/// for both, since GitHub's issues endpoint returns pulls with the same
/// fields).
fn normalize_issue_or_pull(v: &serde_json::Value) -> serde_json::Value {
    let number = v.get("number").and_then(|x| x.as_u64());
    let title = v.get("title").and_then(|x| x.as_str());
    let state = v.get("state").and_then(|x| x.as_str());
    let author = v
        .get("user")
        .and_then(|u| u.get("login"))
        .and_then(|x| x.as_str());
    let created_at = v.get("created_at").and_then(|x| x.as_str());
    let updated_at = v.get("updated_at").and_then(|x| x.as_str());
    let url = v.get("html_url").and_then(|x| x.as_str());
    let labels: Vec<serde_json::Value> = v
        .get("labels")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                .map(|s| serde_json::Value::String(s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let body_excerpt = v.get("body").and_then(|x| x.as_str()).map(truncate_body);

    serde_json::json!({
        "number": number,
        "title": title,
        "state": state,
        "author": author,
        "created_at": created_at,
        "updated_at": updated_at,
        "url": url,
        "labels": labels,
        "body_excerpt": body_excerpt,
    })
}

/// Normalize a single GitHub release JSON object.
fn normalize_release(v: &serde_json::Value) -> serde_json::Value {
    let tag = v.get("tag_name").and_then(|x| x.as_str());
    let name = v.get("name").and_then(|x| x.as_str());
    let published_at = v.get("published_at").and_then(|x| x.as_str());
    let url = v.get("html_url").and_then(|x| x.as_str());
    let prerelease = v.get("prerelease").and_then(|x| x.as_bool());
    let body_excerpt = v.get("body").and_then(|x| x.as_str()).map(truncate_body);

    serde_json::json!({
        "tag": tag,
        "name": name,
        "published_at": published_at,
        "url": url,
        "prerelease": prerelease,
        "body_excerpt": body_excerpt,
    })
}

/// Truncate `s` to at most [`BODY_MAX_CHARS`] `char`s (boundary-safe — counts
/// `char`s, not bytes), appending `…` when truncation occurred. Mirrors
/// `persona-wire-adapter-rss::truncate_summary`.
fn truncate_body(s: &str) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(BODY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_github_uri ----

    fn parse(uri: &str) -> WireResult<GithubUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_github_uri(&wire)
    }

    #[test]
    fn parse_github_uri_default_kind_issues() {
        let spec = parse("github://octocat/hello-world").unwrap();
        assert_eq!(spec.owner, "octocat");
        assert_eq!(spec.repo, "hello-world");
        assert_eq!(spec.kind, GithubKind::Issues);
        assert_eq!(spec.state.as_deref(), Some("open"));
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_github_uri_kind_issues_explicit() {
        let spec = parse("github://octocat/hello-world?kind=issues").unwrap();
        assert_eq!(spec.kind, GithubKind::Issues);
    }

    #[test]
    fn parse_github_uri_kind_pulls() {
        let spec = parse("github://octocat/hello-world?kind=pulls").unwrap();
        assert_eq!(spec.kind, GithubKind::Pulls);
        assert_eq!(spec.state.as_deref(), Some("open"));
    }

    #[test]
    fn parse_github_uri_kind_releases() {
        let spec = parse("github://octocat/hello-world?kind=releases").unwrap();
        assert_eq!(spec.kind, GithubKind::Releases);
        assert_eq!(spec.state, None, "state does not apply to releases");
    }

    #[test]
    fn parse_github_uri_invalid_kind_fails_loud() {
        let err = parse("github://octocat/hello-world?kind=commits").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_github_uri_state_closed() {
        let spec = parse("github://octocat/hello-world?state=closed").unwrap();
        assert_eq!(spec.state.as_deref(), Some("closed"));
    }

    #[test]
    fn parse_github_uri_state_all() {
        let spec = parse("github://octocat/hello-world?state=all").unwrap();
        assert_eq!(spec.state.as_deref(), Some("all"));
    }

    #[test]
    fn parse_github_uri_invalid_state_fails_loud() {
        let err = parse("github://octocat/hello-world?state=merged").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid state"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_github_uri_releases_invalid_state_silently_ignored() {
        // kind=releases: state is not even validated, per module docs.
        let spec = parse("github://octocat/hello-world?kind=releases&state=bogus").unwrap();
        assert_eq!(spec.kind, GithubKind::Releases);
        assert_eq!(spec.state, None);
    }

    #[test]
    fn parse_github_uri_limit_override() {
        let spec = parse("github://octocat/hello-world?limit=5").unwrap();
        assert_eq!(spec.limit, 5);
    }

    #[test]
    fn parse_github_uri_limit_non_numeric_fails_loud() {
        let err = parse("github://octocat/hello-world?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_github_uri_limit_zero_fails_loud() {
        let err = parse("github://octocat/hello-world?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_github_uri_empty_owner_fails_loud() {
        let err = parse("github:///hello-world").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing owner"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_github_uri_missing_repo_fails_loud() {
        let err = parse("github://octocat").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing repo"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_github_uri_extra_path_segment_fails_loud() {
        let err = parse("github://octocat/hello-world/extra").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("extra path segment"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_github_uri_unknown_query_key_ignored() {
        let spec = parse("github://octocat/hello-world?utm_source=foo").unwrap();
        assert_eq!(spec.owner, "octocat");
        assert_eq!(spec.repo, "hello-world");
        assert_eq!(spec.kind, GithubKind::Issues);
    }

    #[test]
    fn endpoint_url_issues_shape() {
        // limit=5 over-fetches to per_page=20 (4x) so post-filter PR removal
        // still leaves enough real issues to satisfy the requested limit.
        let spec = parse("github://octocat/hello-world?limit=5").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=20"
        );
    }

    #[test]
    fn endpoint_url_issues_over_fetches_per_page() {
        let spec = parse("github://octocat/hello-world?limit=3").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=12"
        );
    }

    #[test]
    fn endpoint_url_issues_per_page_capped_at_100() {
        // limit=50 * 4 = 200, but GitHub's per_page ceiling caps it at 100.
        let spec = parse("github://octocat/hello-world?limit=50").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=100"
        );
    }

    #[test]
    fn endpoint_url_issues_large_limit_does_not_panic() {
        // limit > GITHUB_PER_PAGE_MAX must not panic (regression guard for
        // the clamp-vs-min choice in `endpoint_url`); per_page still caps at
        // GITHUB_PER_PAGE_MAX.
        let spec = parse("github://octocat/hello-world?limit=250").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=100"
        );
    }

    #[test]
    fn endpoint_url_pulls_shape_uses_limit_directly() {
        // `pulls` has no post-fetch filtering, so per_page = limit (no over-fetch).
        let spec = parse("github://octocat/hello-world?kind=pulls&limit=5").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/pulls?state=open&per_page=5"
        );
    }

    #[test]
    fn endpoint_url_releases_shape_has_no_state() {
        let spec = parse("github://octocat/hello-world?kind=releases&limit=5").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/releases?per_page=5"
        );
    }

    // ---- normalize_github ----

    fn issue_fixture(number: u64, has_pull_request: bool) -> serde_json::Value {
        let mut v = serde_json::json!({
            "number": number,
            "title": format!("Issue {number}"),
            "state": "open",
            "user": { "login": "octocat" },
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-02T00:00:00Z",
            "html_url": format!("https://github.com/octocat/hello-world/issues/{number}"),
            "labels": [ { "name": "bug" }, { "name": "help wanted" } ],
            "body": "Some body text",
        });
        if has_pull_request {
            v["pull_request"] = serde_json::json!({ "url": "https://api.github.com/pulls/1" });
        }
        v
    }

    #[test]
    fn normalize_github_issues_excludes_pull_requests() {
        let raw = serde_json::json!([issue_fixture(1, false), issue_fixture(2, true)]);
        let spec = parse("github://octocat/hello-world").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "PR-tagged entry excluded from issues");
        assert_eq!(items[0]["number"].as_u64().unwrap(), 1);
    }

    #[test]
    fn normalize_github_issues_field_mapping() {
        let raw = serde_json::json!([issue_fixture(42, false)]);
        let spec = parse("github://octocat/hello-world").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        assert_eq!(v["repo"]["owner"].as_str().unwrap(), "octocat");
        assert_eq!(v["repo"]["name"].as_str().unwrap(), "hello-world");
        assert_eq!(v["kind"].as_str().unwrap(), "issues");
        let item = &v["items"][0];
        assert_eq!(item["number"].as_u64().unwrap(), 42);
        assert_eq!(item["title"].as_str().unwrap(), "Issue 42");
        assert_eq!(item["state"].as_str().unwrap(), "open");
        assert_eq!(item["author"].as_str().unwrap(), "octocat");
        assert_eq!(item["created_at"].as_str().unwrap(), "2024-01-01T00:00:00Z");
        assert_eq!(item["updated_at"].as_str().unwrap(), "2024-01-02T00:00:00Z");
        assert_eq!(
            item["url"].as_str().unwrap(),
            "https://github.com/octocat/hello-world/issues/42"
        );
        let labels: Vec<&str> = item["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["bug", "help wanted"]);
        assert_eq!(item["body_excerpt"].as_str().unwrap(), "Some body text");
    }

    #[test]
    fn normalize_github_pulls_basic_shape() {
        let raw = serde_json::json!([issue_fixture(7, true)]);
        let spec = parse("github://octocat/hello-world?kind=pulls").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "pulls");
        let items = v["items"].as_array().unwrap();
        assert_eq!(
            items.len(),
            1,
            "pulls endpoint keeps pull_request-tagged entries"
        );
        assert_eq!(items[0]["number"].as_u64().unwrap(), 7);
    }

    #[test]
    fn normalize_github_releases_basic_shape() {
        let raw = serde_json::json!([{
            "tag_name": "v1.0.0",
            "name": "First release",
            "published_at": "2024-01-01T00:00:00Z",
            "html_url": "https://github.com/octocat/hello-world/releases/tag/v1.0.0",
            "prerelease": false,
            "body": "Release notes",
        }]);
        let spec = parse("github://octocat/hello-world?kind=releases").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "releases");
        let item = &v["items"][0];
        assert_eq!(item["tag"].as_str().unwrap(), "v1.0.0");
        assert_eq!(item["name"].as_str().unwrap(), "First release");
        assert_eq!(
            item["published_at"].as_str().unwrap(),
            "2024-01-01T00:00:00Z"
        );
        assert_eq!(
            item["url"].as_str().unwrap(),
            "https://github.com/octocat/hello-world/releases/tag/v1.0.0"
        );
        assert!(!item["prerelease"].as_bool().unwrap());
        assert_eq!(item["body_excerpt"].as_str().unwrap(), "Release notes");
    }

    #[test]
    fn normalize_github_missing_fields_are_null() {
        let raw = serde_json::json!([{ "number": 1 }]);
        let spec = parse("github://octocat/hello-world").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        let item = &v["items"][0];
        assert!(item["title"].is_null());
        assert!(item["author"].is_null(), "no `user` key -> null author");
        assert!(item["body_excerpt"].is_null(), "no `body` key -> null");
        assert_eq!(
            item["labels"].as_array().unwrap().len(),
            0,
            "no `labels` key -> empty array"
        );
    }

    #[test]
    fn normalize_github_body_excerpt_truncated() {
        let long_body = "a".repeat(600);
        let raw = serde_json::json!([{ "number": 1, "body": long_body }]);
        let spec = parse("github://octocat/hello-world").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        let excerpt = v["items"][0]["body_excerpt"].as_str().unwrap();
        assert_eq!(
            excerpt.chars().count(),
            BODY_MAX_CHARS + 1,
            "500 + ellipsis"
        );
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn normalize_github_non_array_response_fails_loud() {
        let raw = serde_json::json!({ "message": "Not Found" });
        let spec = parse("github://octocat/hello-world").unwrap();
        let err = normalize_github(&spec, &raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected a JSON array"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn normalize_github_limit_truncates_after_filter() {
        // 3 raw entries, one PR-tagged; limit=1 should cap the final (filtered) list.
        let raw = serde_json::json!([
            issue_fixture(1, false),
            issue_fixture(2, false),
            issue_fixture(3, true),
        ]);
        let spec = parse("github://octocat/hello-world?limit=1").unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["number"].as_u64().unwrap(), 1);
    }

    // ---- Pageable (Layer 3a of GH #1) ----
    //
    // `fetch_page` / `Adapter::fetch` both perform live HTTP via
    // `HttpClient` (a concrete struct, not behind a mockable trait), and
    // this workspace has no wiremock / hand-rolled mock-server pattern
    // (checked `persona-wire-adapter-github`'s Cargo.toml and every other
    // adapter crate at implementation time — neither is used anywhere).
    // `adapter.rs`'s crate-root docs also establish the repo-wide
    // convention that Adapter tests are offline unit tests over inline
    // fixtures, never live network access. So the cursor→URL decision
    // (`resolve_fetch_page_url`) and the shape-building (`wrap_items`) are
    // tested directly as the pure functions they are, per the Done
    // Criteria's "or verify via URL construction helper" allowance —
    // mirroring the existing `endpoint_url_*` test pattern below.

    #[test]
    fn github_pageable_max_page_size_is_100() {
        let adapter = GithubAdapter;
        assert_eq!(adapter.max_page_size(), GITHUB_PER_PAGE_MAX);
    }

    #[test]
    fn github_as_pageable_returns_some() {
        let adapter = GithubAdapter;
        let pageable = adapter.as_pageable();
        assert!(pageable.is_some(), "override should return Some(self)");
        assert_eq!(pageable.unwrap().max_page_size(), GITHUB_PER_PAGE_MAX);
    }

    #[test]
    fn github_fetch_page_first_call_uses_kind_endpoint() {
        // cursor = None routes through `endpoint_url_for_first_page`
        // (per_page = 100 unconditionally), not the limit-scaled
        // `endpoint_url` the non-paginated fast path uses.
        let issues = parse("github://octocat/hello-world").unwrap();
        assert_eq!(
            resolve_fetch_page_url(&issues, &None).unwrap(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=100"
        );

        let pulls = parse("github://octocat/hello-world?kind=pulls").unwrap();
        assert_eq!(
            resolve_fetch_page_url(&pulls, &None).unwrap(),
            "https://api.github.com/repos/octocat/hello-world/pulls?state=open&per_page=100"
        );

        let releases = parse("github://octocat/hello-world?kind=releases").unwrap();
        assert_eq!(
            resolve_fetch_page_url(&releases, &None).unwrap(),
            "https://api.github.com/repos/octocat/hello-world/releases?per_page=100"
        );
    }

    #[test]
    fn github_fetch_page_with_link_cursor_uses_url_directly() {
        let spec = parse("github://octocat/hello-world").unwrap();
        let cursor = Some(Cursor::LinkHeader(
            "https://api.github.com/repositories/1/issues?page=2&state=open".to_string(),
        ));
        assert_eq!(
            resolve_fetch_page_url(&spec, &cursor).unwrap(),
            "https://api.github.com/repositories/1/issues?page=2&state=open",
            "LinkHeader url used literally, no rebuild"
        );
    }

    #[test]
    fn github_fetch_page_rejects_other_cursor_variants() {
        let spec = parse("github://octocat/hello-world").unwrap();
        for cursor in [
            Cursor::PageNumber(2),
            Cursor::NextToken("abc123".to_string()),
            Cursor::Offset(10),
        ] {
            let err = resolve_fetch_page_url(&spec, &Some(cursor)).unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains("unsupported cursor variant"),
                "unexpected error: {msg}"
            );
        }
    }

    #[test]
    fn github_wrap_items_produces_repo_kind_items_shape() {
        let adapter = GithubAdapter;
        let uri = WireUri::parse("github://octocat/hello-world?kind=pulls").unwrap();
        let items = vec![serde_json::json!({"number": 1})];

        let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();

        assert_eq!(wrapped["repo"]["owner"].as_str().unwrap(), "octocat");
        assert_eq!(wrapped["repo"]["name"].as_str().unwrap(), "hello-world");
        assert_eq!(wrapped["kind"].as_str().unwrap(), "pulls");
        assert_eq!(wrapped["items"], serde_json::Value::Array(items));
    }
}
