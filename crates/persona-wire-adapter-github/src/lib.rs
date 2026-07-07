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
//!   A non-numeric or zero value fails loud.
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

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
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

        // Auth is resolved per-fetch (not at boot); see module docs "Auth".
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

        let raw = client.get_json(&spec.endpoint_url()).await?;
        normalize_github(&spec, &raw)
    }
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
    fn endpoint_url(&self) -> String {
        match self.kind {
            GithubKind::Issues | GithubKind::Pulls => {
                let state = self.state.as_deref().unwrap_or("open");
                format!(
                    "{API_BASE}/repos/{}/{}/{}?state={state}&per_page={}",
                    self.owner,
                    self.repo,
                    self.kind.as_str(),
                    self.limit
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
    let arr = raw.as_array().ok_or_else(|| {
        WireError::Storage(format!(
            "github adapter: unexpected response shape for {}/{} ({}): expected a JSON array",
            spec.owner,
            spec.repo,
            spec.kind.as_str()
        ))
    })?;

    let items: Vec<serde_json::Value> = match spec.kind {
        // GitHub's /issues endpoint mixes in pull requests (entries carrying
        // a `pull_request` key) — filter those out, per module docs.
        GithubKind::Issues => arr
            .iter()
            .filter(|v| v.get("pull_request").is_none())
            .take(spec.limit)
            .map(normalize_issue_or_pull)
            .collect(),
        GithubKind::Pulls => arr
            .iter()
            .take(spec.limit)
            .map(normalize_issue_or_pull)
            .collect(),
        GithubKind::Releases => arr.iter().take(spec.limit).map(normalize_release).collect(),
    };

    Ok(serde_json::json!({
        "repo": { "owner": spec.owner, "name": spec.repo },
        "kind": spec.kind.as_str(),
        "items": items,
    }))
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
        let spec = parse("github://octocat/hello-world?limit=5").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://api.github.com/repos/octocat/hello-world/issues?state=open&per_page=5"
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
}
