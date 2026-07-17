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
//! - Internal Link-header loop in [`Adapter::fetch`] — accumulates a raw
//!   GitHub REST API response (JSON array per page) into the Wire JSON
//!   shape below, branching only on `kind` for the per-item normalization.
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
//! The literal `"github"` service key is overridable per-fetch via the URI's
//! `?auth=<service_key>` query param (see `persona_wire_core::infrastructure
//! ::adapter`'s "External service integration policy" for the convention);
//! absent, behavior is unchanged.
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
//!   "items": [ ... ],
//!   "has_more": false
//! }
//! ```
//!
//! `has_more` is `true` when the adapter truncated the result at `?limit=N`
//! and the upstream still had more items available (either the current page
//! overshot `limit`, or a next-page Link header was returned). It is
//! `false` when the loop terminated because the upstream ran out of data.
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
//! ## Pagination
//!
//! `Adapter::fetch` drives the pagination loop internally: it follows GitHub's
//! RFC 5988 `Link` response header (`rel="next"`) across repeated requests
//! until it has accumulated `?limit=N` items or the upstream signals
//! end-of-data. The cursor form is a private implementation detail — the wire
//! layer only sees the final assembled `{repo, kind, items, has_more}` shape.
//!
//! For `?limit <= GITHUB_PER_PAGE_MAX` (100), a single upstream request is
//! sufficient in the common case (the loop terminates after one iteration).
//! For `kind=issues`, the per-page over-fetch heuristic (`per_page = min(4 *
//! limit, 100)`) still applies to reduce the chance of running the loop when
//! GitHub's `/issues` endpoint mixes in pull requests that get filtered
//! post-fetch. If the filtered count still falls short, the loop follows the
//! `Link` header to the next page.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{FilterCap, WireError, WireFilters, WireResult};
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

    fn filter_caps(&self) -> &'static [FilterCap] {
        &[FilterCap::Limit { max: None }]
    }

    /// Fetch `spec.kind` items for the repo derived from `uri`, driving the
    /// Link-header pagination loop internally until `?limit=N` items are
    /// accumulated or the upstream signals end-of-data. See the module docs
    /// for URI grammar, auth resolution, and output shape (including
    /// `has_more` semantics).
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let filters = WireFilters::parse(uri, self.filter_caps())?;
        let spec = parse_github_uri(uri, filters.limit.unwrap_or(DEFAULT_LIMIT))?;
        let client = github_http_client(uri)?;
        let mut items: Vec<serde_json::Value> = Vec::new();
        let mut url = spec.endpoint_url();
        let has_more = loop {
            let (raw, next_link) = client.get_json_with_next_link(&url).await?;
            let arr = response_array(&spec.owner, &spec.repo, spec.kind, &raw)?;
            items.extend(normalize_items(spec.kind, arr));
            if items.len() >= spec.limit {
                // Truthful `has_more`: the current page overshot `limit`, or
                // the upstream has more pages available.
                break items.len() > spec.limit || next_link.is_some();
            }
            match next_link {
                Some(next) => url = next,
                None => break false, // upstream exhausted before hitting limit
            }
        };
        items.truncate(spec.limit);
        Ok(serde_json::json!({
            "repo": { "owner": spec.owner, "name": spec.repo },
            "kind": spec.kind.as_str(),
            "items": items,
            "has_more": has_more,
        }))
    }
}

/// Builds a fresh, GitHub-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth").
fn github_http_client(uri: &WireUri) -> WireResult<HttpClient> {
    let service_key = resolve_service_key(uri, "github");
    let token = Credentials::default_chain().get(service_key)?;
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

/// Resolves the credential service key for this fetch: the URI's
/// `?auth=<service_key>` query param when present (reference key only,
/// never a secret — see `persona_wire_core::infrastructure::adapter`'s
/// "External service integration policy"), otherwise `default_key` (this
/// adapter's literal `"github"` service name, preserving pre-existing
/// behavior when the param is absent).
fn resolve_service_key<'a>(uri: &'a WireUri, default_key: &'static str) -> &'a str {
    uri.query_get("auth").unwrap_or(default_key)
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
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`GithubUriSpec`]. See the module-level "URI grammar" section for
/// the exact rules and failure conditions. Cross-cutting filters
/// (`?limit=`) are parsed separately via [`WireFilters::parse`] and passed
/// in as `limit`.
fn parse_github_uri(uri: &WireUri, limit: usize) -> WireResult<GithubUriSpec> {
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

    Ok(GithubUriSpec {
        owner,
        repo,
        kind,
        state,
        limit,
    })
}

/// Normalize a single-page GitHub REST API response (`raw`, expected to be a
/// JSON array) per `spec.kind` into the Wire JSON shape (see module docs
/// "Output shape"). `has_more` is `true` when the raw array (post-filter for
/// `kind=issues`) contained more entries than `spec.limit`.
///
/// This helper is used by unit tests to exercise the parse-and-shape path
/// offline; [`Adapter::fetch`] drives the multi-page Link-header loop and
/// assembles the shape inline, so it does not call this function.
#[cfg(test)]
fn normalize_github(
    spec: &GithubUriSpec,
    raw: &serde_json::Value,
) -> WireResult<serde_json::Value> {
    let arr = response_array(&spec.owner, &spec.repo, spec.kind, raw)?;
    let all: Vec<serde_json::Value> = normalize_items(spec.kind, arr);
    let has_more = all.len() > spec.limit;
    let items: Vec<serde_json::Value> = all.into_iter().take(spec.limit).collect();

    Ok(serde_json::json!({
        "repo": { "owner": spec.owner, "name": spec.repo },
        "kind": spec.kind.as_str(),
        "items": items,
        "has_more": has_more,
    }))
}

/// Extracts the raw JSON array from a GitHub REST API response, failing
/// loud (naming the repo + kind) when the response isn't a JSON array.
/// Shared by [`normalize_github`] and the internal Link-header loop in
/// [`Adapter::fetch`].
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
/// (callers apply `.take(limit)` themselves — the [`Adapter::fetch`]
/// internal loop normalizes a full page and truncates across accumulated
/// pages instead).
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

    // ---- resolve_service_key (?auth= override, network-free) ----

    #[test]
    fn resolve_service_key_defaults_when_auth_param_absent() {
        let uri = WireUri::parse("github://octocat/hello-world").unwrap();
        assert_eq!(resolve_service_key(&uri, "github"), "github");
    }

    #[test]
    fn resolve_service_key_overrides_when_auth_param_present() {
        let uri = WireUri::parse("github://octocat/hello-world?auth=github-alt").unwrap();
        assert_eq!(resolve_service_key(&uri, "github"), "github-alt");
    }

    // ---- parse_github_uri ----

    /// Helper: parse with the adapter's default limit (backwards-compatible
    /// with the pre-Phase-2 default when no `?limit=` was supplied).
    fn parse(uri: &str) -> WireResult<GithubUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_github_uri(&wire, DEFAULT_LIMIT)
    }

    /// Helper: parse with an explicit limit (simulates
    /// `filters.limit = Some(n)` after `WireFilters::parse`).
    fn parse_with_limit(uri: &str, limit: usize) -> WireResult<GithubUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_github_uri(&wire, limit)
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
    fn parse_github_uri_limit_forwarded() {
        let spec = parse_with_limit("github://octocat/hello-world", 5).unwrap();
        assert_eq!(spec.limit, 5);
    }

    // ---- filter_caps + WireFilters integration (Phase 2 unified filter IF) ----

    fn parse_filters(uri: &str) -> WireResult<WireFilters> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        WireFilters::parse(&wire, GithubAdapter.filter_caps())
    }

    #[test]
    fn filter_caps_declares_limit_unbounded() {
        assert_eq!(
            GithubAdapter.filter_caps(),
            &[FilterCap::Limit { max: None }]
        );
    }

    #[test]
    fn wire_filters_limit_override() {
        let f = parse_filters("github://octocat/hello-world?limit=5").unwrap();
        assert_eq!(f.limit, Some(5));
    }

    #[test]
    fn wire_filters_limit_non_numeric_fails_loud() {
        let err = parse_filters("github://octocat/hello-world?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("invalid"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_filters_limit_zero_fails_loud() {
        let err = parse_filters("github://octocat/hello-world?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("positive"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_filters_undeclared_filter_key_errors() {
        let err = parse_filters("github://octocat/hello-world?query=x").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("query") && msg.contains("not supported"),
            "unexpected error: {msg}"
        );
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
        let spec = parse_with_limit("github://octocat/hello-world?limit=5", 5).unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=20"
        );
    }

    #[test]
    fn endpoint_url_issues_over_fetches_per_page() {
        let spec = parse_with_limit("github://octocat/hello-world?limit=3", 3).unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=12"
        );
    }

    #[test]
    fn endpoint_url_issues_per_page_capped_at_100() {
        // limit=50 * 4 = 200, but GitHub's per_page ceiling caps it at 100.
        let spec = parse_with_limit("github://octocat/hello-world?limit=50", 50).unwrap();
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
        let spec = parse_with_limit("github://octocat/hello-world?limit=250", 250).unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=100"
        );
    }

    #[test]
    fn endpoint_url_pulls_shape_uses_limit_directly() {
        // `pulls` has no post-fetch filtering, so per_page = limit (no over-fetch).
        let spec = parse_with_limit("github://octocat/hello-world?kind=pulls&limit=5", 5).unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/pulls?state=open&per_page=5"
        );
    }

    #[test]
    fn endpoint_url_releases_shape_has_no_state() {
        let spec =
            parse_with_limit("github://octocat/hello-world?kind=releases&limit=5", 5).unwrap();
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
        let spec = parse_with_limit("github://octocat/hello-world?limit=1", 1).unwrap();
        let v = normalize_github(&spec, &raw).unwrap();
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["number"].as_u64().unwrap(), 1);
    }

    // ---- normalize_github shape carries has_more? no: only Adapter::fetch
    // sets `has_more`, and Adapter::fetch drives live HTTP via HttpClient
    // (a concrete struct, not behind a mockable trait). This workspace's
    // convention (established in `adapter.rs` crate docs) is that Adapter
    // tests are offline unit tests over inline fixtures, never live network
    // access. `normalize_github` and `normalize_items` stay pure and are
    // exercised through the fixtures above; the internal loop's `has_more`
    // semantics are documented in the module docs "Output shape" and covered
    // by the standing convention that adapter integration is verified
    // against live upstreams outside the unit-test path.
}
