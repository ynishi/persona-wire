//! persona-wire Adapter for Todoist (scheme `todoist://`).
//!
//! ## Architecture
//!
//! `TodoistAdapter` is a stateless [`Adapter`] impl split into three
//! independent functions:
//!
//! - [`parse_todoist_uri`] — `WireUri` → `TodoistUriSpec` (kind + optional
//!   project filter / natural-language filter + item limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
//!   Todoist-specific knowledge in the transport layer).
//! - Internal `next_cursor` loop in [`Adapter::fetch`] — accumulates
//!   Todoist API v1 response pages (`{"results": [...], "next_cursor":
//!   ...}`) into the Wire JSON shape below, branching only on `kind` for
//!   the per-item normalization.
//!
//! ## URI grammar
//!
//! ```text
//! todoist://tasks[?project_id=<id>][?filter=<query>][&limit=N]
//! todoist://projects[?limit=N]
//! ```
//!
//! - `host` selects the endpoint kind (`tasks` / `projects`); an empty or
//!   invalid value **fails loud** — a typo here would otherwise silently
//!   return a different class of data (matching
//!   `persona-wire-adapter-github`'s `?kind=` convention).
//! - The path must be empty (or `/`); any additional path segment fails
//!   loud.
//! - For `kind=tasks`: `?filter=<query>` (a Todoist natural-language filter,
//!   e.g. `today | overdue`) selects the `/tasks/filter` endpoint;
//!   otherwise `/tasks` is used, optionally scoped by `?project_id=<id>`.
//!   **`filter` and `project_id` are mutually exclusive** — the filter
//!   endpoint does not accept a `project_id` parameter, so specifying both
//!   fails loud rather than silently dropping one.
//! - For `kind=projects`, `project_id` and `filter` are unknown query keys
//!   (silently ignored, not even read).
//! - `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
//!   A non-numeric or zero value fails loud; there is no upper bound at
//!   parse time. [`MAX_LIMIT`] (the Todoist API's own per-request cap of
//!   200) is enforced only when the adapter builds the upstream request URL;
//!   `?limit=N` with `N > MAX_LIMIT` triggers the internal pagination loop
//!   (see "Pagination" below).
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss` / `-github`).
//! - The `filter` value is percent-decoded once at parse time (it commonly
//!   contains spaces and `|`, e.g. `today | overdue`, and callers may supply
//!   it either raw or percent-encoded), then percent/form-encoded exactly
//!   once when building the request URL (via `url::Url::query_pairs_mut`).
//!
//! ## Auth
//!
//! Resolved per-fetch (not at boot) via
//! `persona_wire_credentials::Credentials::default_chain().get("todoist")`.
//! Unlike `persona-wire-adapter-github`, Todoist has no unauthenticated
//! access mode — a missing token **fails loud**. Set a token via the
//! `PERSONA_WIRE_TOKEN_TODOIST` or `TODOIST_API_TOKEN` environment variable,
//! or store one in the OS keychain via `persona-wire token set todoist`. The
//! token is found under Todoist Settings → Integrations → Developer.
//!
//! The literal `"todoist"` service key is overridable per-fetch via the
//! URI's `?auth=<service_key>` query param (see `persona_wire_core::
//! infrastructure::adapter`'s "External service integration policy" for the
//! convention); absent, behavior is unchanged.
//!
//! Todoist enforces a rate limit of roughly 1,000 requests per 15 minutes
//! per user; exceeding it returns HTTP 429, which surfaces as a normal fetch
//! failure via `persona_wire_transport_http::HttpClient`.
//!
//! ## Output shape
//!
//! ```json
//! { "kind": "tasks", "items": [ ... ], "has_more": false }
//! ```
//!
//! `has_more` is `true` when the adapter truncated the result at `?limit=N`
//! and the upstream still had more items available. It is `false` when the
//! loop terminated because Todoist's `next_cursor` was `null`.
//!
//! `items` entries for `kind=tasks`:
//!
//! ```json
//! {
//!   "id": "...|null", "content": "...|null",
//!   "description_excerpt": "...|null", "project_id": "...|null",
//!   "priority": 1, "labels": ["..."],
//!   "due": { "date": "...|null", "string": "...|null", "is_recurring": false } | null,
//!   "deadline_date": "...|null", "completed_at": "...|null",
//!   "added_at": "...|null", "updated_at": "...|null"
//! }
//! ```
//!
//! `items` entries for `kind=projects`:
//!
//! ```json
//! {
//!   "id": "...|null", "name": "...|null", "color": "...|null",
//!   "is_favorite": false, "is_archived": false, "is_inbox": false,
//!   "view_style": "...|null"
//! }
//! ```
//!
//! ## Pagination
//!
//! `Adapter::fetch` drives the pagination loop internally: it follows the
//! response body's `next_cursor` field (an opaque token; `null` signals
//! end-of-data) across repeated requests until it has accumulated `?limit=N`
//! items or the upstream signals end-of-data. The cursor form is a private
//! implementation detail — the wire layer only sees the final assembled
//! `{kind, items, has_more}` shape. Every upstream request is sent with
//! `limit = MAX_LIMIT` (Todoist's own per-request ceiling of 200), so the
//! loop runs once for `?limit <= MAX_LIMIT` and continues page-by-page for
//! larger requests.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{FilterCap, WireError, WireFilters, WireResult};
use persona_wire_credentials::Credentials;
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `items` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Maximum allowed `?limit=` value (Todoist API's own `1..=200` constraint).
pub const MAX_LIMIT: usize = 200;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-adapter-github::FETCH_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Max `description_excerpt` length in `char`s before truncation (context
/// size guard).
pub const DESCRIPTION_MAX_CHARS: usize = 500;

/// Todoist unified API v1 base URL (the REST v2 `rest/v2` API was retired
/// 2026-02-10; this adapter targets the current API only).
pub const API_BASE: &str = "https://api.todoist.com/api/v1";

/// persona-wire Adapter for Todoist (`todoist://` scheme).
pub struct TodoistAdapter;

#[async_trait]
impl Adapter for TodoistAdapter {
    fn scheme(&self) -> &'static str {
        "todoist"
    }

    fn filter_caps(&self) -> &'static [FilterCap] {
        &[FilterCap::Limit { max: None }]
    }

    /// Fetch `spec.kind` items, driving the `next_cursor` pagination loop
    /// internally until `?limit=N` items are accumulated or the upstream
    /// signals end-of-data. See the module docs for URI grammar, auth
    /// resolution, and output shape (including `has_more` semantics).
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let filters = WireFilters::parse(uri, self.filter_caps())?;
        let spec = parse_todoist_uri(uri, filters.limit.unwrap_or(DEFAULT_LIMIT))?;
        let client = todoist_http_client(uri)?;
        let mut items: Vec<serde_json::Value> = Vec::new();
        let mut cursor: Option<String> = None;
        let has_more = loop {
            let url = build_request_url(&spec, cursor.as_deref());
            let raw = client.get_json(&url).await?;
            let arr = response_array(spec.kind, &raw)?;
            items.extend(normalize_items(spec.kind, arr));
            let next = raw
                .get("next_cursor")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if items.len() >= spec.limit {
                break items.len() > spec.limit || next.is_some();
            }
            match next {
                Some(token) => cursor = Some(token),
                None => break false,
            }
        };
        items.truncate(spec.limit);
        Ok(serde_json::json!({
            "kind": spec.kind.as_str(),
            "items": items,
            "has_more": has_more,
        }))
    }
}

/// Builds a fresh, Todoist-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth").
fn todoist_http_client(uri: &WireUri) -> WireResult<HttpClient> {
    // Auth is resolved per-fetch (not at boot); see module docs "Auth".
    // Todoist has no unauthenticated access mode, unlike the github
    // adapter, so a missing token fails loud here.
    let service_key = resolve_service_key(uri, "todoist");
    let token = Credentials::default_chain().get(service_key)?.ok_or_else(|| {
        if service_key == "todoist" {
            WireError::Storage(
                "todoist adapter: no token found for 'todoist' (set PERSONA_WIRE_TOKEN_TODOIST / TODOIST_API_TOKEN, or run 'persona-wire token set todoist')"
                    .to_string(),
            )
        } else {
            WireError::Storage(format!(
                "todoist adapter: no token found for '{service_key}' (set PERSONA_WIRE_TOKEN_<KEY> uppercased, or run 'persona-wire token set {service_key}')"
            ))
        }
    })?;
    Ok(HttpClient::new("todoist adapter")
        .with_timeout(FETCH_TIMEOUT)
        .with_bearer(token))
}

/// Resolves the credential service key for this fetch: the URI's
/// `?auth=<service_key>` query param when present (reference key only,
/// never a secret — see `persona_wire_core::infrastructure::adapter`'s
/// "External service integration policy"), otherwise `default_key` (this
/// adapter's literal `"todoist"` service name, preserving pre-existing
/// behavior when the param is absent).
fn resolve_service_key<'a>(uri: &'a WireUri, default_key: &'static str) -> &'a str {
    uri.query_get("auth").unwrap_or(default_key)
}

/// The two Todoist endpoint kinds this adapter can target, selected via the
/// URI host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoistKind {
    Tasks,
    Projects,
}

impl TodoistKind {
    fn as_str(self) -> &'static str {
        match self {
            TodoistKind::Tasks => "tasks",
            TodoistKind::Projects => "projects",
        }
    }
}

/// Parsed `todoist://` URI: endpoint kind + optional project scope /
/// natural-language filter + item limit.
#[derive(Debug)]
struct TodoistUriSpec {
    kind: TodoistKind,
    /// `Some` only for `kind == Tasks` when `?project_id=` is present.
    project_id: Option<String>,
    /// `Some` only for `kind == Tasks` when `?filter=` is present.
    filter: Option<String>,
    limit: usize,
}

/// Builds one upstream request URL for `Adapter::fetch`'s internal loop.
///
/// The per-request `limit` is `min(spec.limit, MAX_LIMIT)` — capped at
/// Todoist's per-request ceiling of 200 so that `?limit > MAX_LIMIT`
/// triggers the loop and picks up the remainder via `next_cursor`. When
/// `cursor` is `Some`, the continuation token from the previous page's
/// `next_cursor` field is appended as a `cursor=<token>` query param; other
/// query params (`limit`, for tasks `project_id` / `filter`'s `query`) are
/// resent unchanged, since Todoist's cursor does not itself carry the
/// filter/scope state.
fn build_request_url(spec: &TodoistUriSpec, cursor: Option<&str>) -> String {
    let per_request_limit = spec.limit.min(MAX_LIMIT).to_string();
    let base = match spec.kind {
        TodoistKind::Tasks => {
            if let Some(filter) = &spec.filter {
                let mut url = url::Url::parse(&format!("{API_BASE}/tasks/filter"))
                    .expect("API_BASE + /tasks/filter is a valid URL");
                url.query_pairs_mut()
                    .append_pair("query", filter)
                    .append_pair("limit", &per_request_limit);
                url
            } else {
                let mut url = url::Url::parse(&format!("{API_BASE}/tasks"))
                    .expect("API_BASE + /tasks is a valid URL");
                {
                    let mut qp = url.query_pairs_mut();
                    qp.append_pair("limit", &per_request_limit);
                    if let Some(project_id) = &spec.project_id {
                        qp.append_pair("project_id", project_id);
                    }
                }
                url
            }
        }
        TodoistKind::Projects => {
            let mut url = url::Url::parse(&format!("{API_BASE}/projects"))
                .expect("API_BASE + /projects is a valid URL");
            url.query_pairs_mut()
                .append_pair("limit", &per_request_limit);
            url
        }
    };
    match cursor {
        Some(token) => {
            let mut url = base;
            url.query_pairs_mut().append_pair("cursor", token);
            url.to_string()
        }
        None => base.to_string(),
    }
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`TodoistUriSpec`]. See the module-level "URI grammar" section for
/// the exact rules and failure conditions. Cross-cutting filters
/// (`?limit=`) are parsed separately via [`WireFilters::parse`] and passed
/// in as `limit`.
fn parse_todoist_uri(uri: &WireUri, limit: usize) -> WireResult<TodoistUriSpec> {
    let kind = match uri.host() {
        Some("tasks") => TodoistKind::Tasks,
        Some("projects") => TodoistKind::Projects,
        Some(bad) if !bad.is_empty() => {
            return Err(WireError::Storage(format!(
                "todoist adapter: invalid kind '{bad}' (must be one of: tasks, projects)"
            )));
        }
        _ => {
            return Err(WireError::Storage(format!(
                "todoist adapter: missing kind (host) in '{}' (expected todoist://tasks or todoist://projects)",
                uri.as_raw()
            )));
        }
    };

    let path = uri.path();
    if !path.is_empty() && path != "/" {
        return Err(WireError::Storage(format!(
            "todoist adapter: unexpected path segment in '{}' (expected todoist://tasks or todoist://projects)",
            uri.as_raw()
        )));
    }

    match kind {
        TodoistKind::Tasks => {
            let project_id = uri.query_get("project_id").map(|s| s.to_string());
            // `WireUri::query_get` returns the raw (undecoded) query value;
            // decode once here so callers may pass `filter` either raw or
            // percent-encoded (module docs "URI grammar"). It is re-encoded
            // exactly once in `endpoint_url` via `url::Url::query_pairs_mut`.
            let filter = uri.query_get("filter").map(|s| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8_lossy()
                    .into_owned()
            });
            if project_id.is_some() && filter.is_some() {
                return Err(WireError::Storage(format!(
                    "todoist adapter: 'filter' and 'project_id' are mutually exclusive in '{}' (the filter endpoint does not accept project_id)",
                    uri.as_raw()
                )));
            }
            Ok(TodoistUriSpec {
                kind,
                project_id,
                filter,
                limit,
            })
        }
        // `project_id` / `filter` are unknown query keys for `projects` and
        // are not even read (module docs "URI grammar").
        TodoistKind::Projects => Ok(TodoistUriSpec {
            kind,
            project_id: None,
            filter: None,
            limit,
        }),
    }
}

// (`parse_limit` was removed — cross-cutting `?limit=` parsing is now done
// by `WireFilters::parse` on the adapter side.)

/// Normalize a single-page Todoist API v1 response (`raw`, expected to be
/// an object with a `results` array) per `spec.kind` into the Wire JSON
/// shape (see module docs "Output shape"). `has_more` is `true` when the
/// single-page results (after truncation to `spec.limit`) leave more upstream
/// data (either the raw array exceeded `limit`, or `next_cursor` is set).
///
/// This helper is used by unit tests to exercise the parse-and-shape path
/// offline; [`Adapter::fetch`] drives the multi-page `next_cursor` loop and
/// assembles the shape inline, so it does not call this function.
#[cfg(test)]
fn normalize_todoist(
    spec: &TodoistUriSpec,
    raw: &serde_json::Value,
) -> WireResult<serde_json::Value> {
    let results = response_array(spec.kind, raw)?;
    let all = normalize_items(spec.kind, results);
    let next_cursor = raw.get("next_cursor").and_then(|v| v.as_str());
    let has_more = all.len() > spec.limit || next_cursor.is_some();
    let items: Vec<serde_json::Value> = all.into_iter().take(spec.limit).collect();

    Ok(serde_json::json!({
        "kind": spec.kind.as_str(),
        "items": items,
        "has_more": has_more,
    }))
}

/// Extracts the raw `results` JSON array from a Todoist API v1 response,
/// failing loud (naming the kind) when the response isn't shaped as
/// expected. Shared by [`normalize_todoist`] and the internal Link-header
/// loop in [`Adapter::fetch`].
fn response_array(
    kind: TodoistKind,
    raw: &serde_json::Value,
) -> WireResult<&Vec<serde_json::Value>> {
    raw.get("results").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(format!(
            "todoist adapter: unexpected response shape for kind '{}': expected an object with a 'results' array",
            kind.as_str()
        ))
    })
}

/// Normalizes every entry in `arr` per `kind`. No `limit` truncation is
/// applied here — [`Adapter::fetch`]'s internal loop truncates across
/// accumulated pages instead.
fn normalize_items(kind: TodoistKind, arr: &[serde_json::Value]) -> Vec<serde_json::Value> {
    match kind {
        TodoistKind::Tasks => arr.iter().map(normalize_task).collect(),
        TodoistKind::Projects => arr.iter().map(normalize_project).collect(),
    }
}

/// Normalize a single Todoist task JSON object.
fn normalize_task(v: &serde_json::Value) -> serde_json::Value {
    let id = v.get("id").and_then(|x| x.as_str());
    let content = v.get("content").and_then(|x| x.as_str());
    let description_excerpt = v
        .get("description")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(truncate_description);
    let project_id = v.get("project_id").and_then(|x| x.as_str());
    let priority = v.get("priority").and_then(|x| x.as_u64());
    let labels: Vec<serde_json::Value> = v
        .get("labels")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let due = v.get("due").filter(|d| !d.is_null()).map(|d| {
        serde_json::json!({
            "date": d.get("date").and_then(|x| x.as_str()),
            "string": d.get("string").and_then(|x| x.as_str()),
            "is_recurring": d.get("is_recurring").and_then(|x| x.as_bool()),
        })
    });
    let deadline_date = v
        .get("deadline")
        .filter(|d| !d.is_null())
        .and_then(|d| d.get("date"))
        .and_then(|x| x.as_str());
    let completed_at = v.get("completed_at").and_then(|x| x.as_str());
    let added_at = v.get("added_at").and_then(|x| x.as_str());
    let updated_at = v.get("updated_at").and_then(|x| x.as_str());

    serde_json::json!({
        "id": id,
        "content": content,
        "description_excerpt": description_excerpt,
        "project_id": project_id,
        "priority": priority,
        "labels": labels,
        "due": due,
        "deadline_date": deadline_date,
        "completed_at": completed_at,
        "added_at": added_at,
        "updated_at": updated_at,
    })
}

/// Normalize a single Todoist project JSON object.
fn normalize_project(v: &serde_json::Value) -> serde_json::Value {
    let id = v.get("id").and_then(|x| x.as_str());
    let name = v.get("name").and_then(|x| x.as_str());
    let color = v.get("color").and_then(|x| x.as_str());
    let is_favorite = v.get("is_favorite").and_then(|x| x.as_bool());
    let is_archived = v.get("is_archived").and_then(|x| x.as_bool());
    let is_inbox = v.get("inbox_project").and_then(|x| x.as_bool());
    let view_style = v.get("view_style").and_then(|x| x.as_str());

    serde_json::json!({
        "id": id,
        "name": name,
        "color": color,
        "is_favorite": is_favorite,
        "is_archived": is_archived,
        "is_inbox": is_inbox,
        "view_style": view_style,
    })
}

/// Truncate `s` to at most [`DESCRIPTION_MAX_CHARS`] `char`s (boundary-safe
/// — counts `char`s, not bytes), appending `…` when truncation occurred.
/// Mirrors `persona-wire-adapter-github::truncate_body`.
fn truncate_description(s: &str) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(DESCRIPTION_MAX_CHARS).collect();
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
        let uri = WireUri::parse("todoist://tasks").unwrap();
        assert_eq!(resolve_service_key(&uri, "todoist"), "todoist");
    }

    #[test]
    fn resolve_service_key_overrides_when_auth_param_present() {
        let uri = WireUri::parse("todoist://tasks?auth=todoist-alt").unwrap();
        assert_eq!(resolve_service_key(&uri, "todoist"), "todoist-alt");
    }

    // ---- parse_todoist_uri ----

    /// Helper: parse with the adapter's default limit (matches the pre-Phase-2
    /// default when no `?limit=` was supplied).
    fn parse(uri: &str) -> WireResult<TodoistUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_todoist_uri(&wire, DEFAULT_LIMIT)
    }

    /// Helper: parse with an explicit limit (simulates
    /// `filters.limit = Some(n)` after `WireFilters::parse`).
    fn parse_with_limit(uri: &str, limit: usize) -> WireResult<TodoistUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_todoist_uri(&wire, limit)
    }

    #[test]
    fn parse_todoist_uri_kind_tasks() {
        let spec = parse("todoist://tasks").unwrap();
        assert_eq!(spec.kind, TodoistKind::Tasks);
        assert_eq!(spec.project_id, None);
        assert_eq!(spec.filter, None);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_todoist_uri_kind_projects() {
        let spec = parse("todoist://projects").unwrap();
        assert_eq!(spec.kind, TodoistKind::Projects);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_todoist_uri_invalid_kind_fails_loud() {
        let err = parse("todoist://commits").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_todoist_uri_empty_host_fails_loud() {
        let err = parse("todoist:///").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_todoist_uri_extra_path_segment_fails_loud() {
        let err = parse("todoist://tasks/extra").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unexpected path segment"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_todoist_uri_filter_and_project_id_conflict_fails_loud() {
        let err = parse("todoist://tasks?filter=today&project_id=123").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("mutually exclusive"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_todoist_uri_project_id_only() {
        let spec = parse("todoist://tasks?project_id=123").unwrap();
        assert_eq!(spec.project_id.as_deref(), Some("123"));
        assert_eq!(spec.filter, None);
    }

    #[test]
    fn parse_todoist_uri_filter_only() {
        let spec = parse("todoist://tasks?filter=today").unwrap();
        assert_eq!(spec.filter.as_deref(), Some("today"));
        assert_eq!(spec.project_id, None);
    }

    #[test]
    fn parse_todoist_uri_filter_raw_value_accepted() {
        let spec = parse_with_limit("todoist://tasks?filter=today | overdue&limit=5", 5).unwrap();
        assert_eq!(spec.filter.as_deref(), Some("today | overdue"));
        let url = build_request_url(&spec, None);
        assert!(!url.contains(' '), "space must be encoded: {url}");
        assert!(!url.contains('|'), "pipe must be encoded: {url}");
    }

    #[test]
    fn parse_todoist_uri_projects_ignores_project_id_and_filter() {
        let spec = parse("todoist://projects?project_id=123&filter=today").unwrap();
        assert_eq!(spec.kind, TodoistKind::Projects);
        assert_eq!(spec.project_id, None);
        assert_eq!(spec.filter, None);
    }

    #[test]
    fn parse_todoist_uri_limit_forwarded() {
        // parse_todoist_uri now receives limit as a parameter; it just
        // stores it. Validation (non-numeric / zero) lives in WireFilters.
        let spec = parse_with_limit("todoist://tasks", 5).unwrap();
        assert_eq!(spec.limit, 5);
    }

    #[test]
    fn parse_todoist_uri_unknown_addressing_key_ignored() {
        let spec = parse("todoist://tasks?utm_source=foo").unwrap();
        assert_eq!(spec.kind, TodoistKind::Tasks);
        assert_eq!(spec.project_id, None);
        assert_eq!(spec.filter, None);
    }

    // ---- filter_caps + WireFilters integration (Phase 2 unified filter IF) ----

    fn parse_filters(uri: &str) -> WireResult<WireFilters> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        WireFilters::parse(&wire, TodoistAdapter.filter_caps())
    }

    #[test]
    fn filter_caps_declares_limit_unbounded() {
        assert_eq!(
            TodoistAdapter.filter_caps(),
            &[FilterCap::Limit { max: None }]
        );
    }

    #[test]
    fn wire_filters_limit_override() {
        let f = parse_filters("todoist://tasks?limit=5").unwrap();
        assert_eq!(f.limit, Some(5));
    }

    #[test]
    fn wire_filters_limit_above_adapter_max_ok() {
        // MAX_LIMIT is enforced per-request during pagination; the WireFilters
        // cap is unbounded, so oversized values pass parse.
        let f = parse_filters("todoist://tasks?limit=500").unwrap();
        assert_eq!(f.limit, Some(500));
    }

    #[test]
    fn wire_filters_limit_zero_fails_loud() {
        let err = parse_filters("todoist://tasks?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("positive"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_filters_limit_non_numeric_fails_loud() {
        let err = parse_filters("todoist://tasks?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("invalid"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_filters_undeclared_filter_key_errors() {
        // ?query= is not declared; ?filter= is an adapter-specific addressing
        // key and stays inline (Todoist DSL, out of the WireFilters vocabulary).
        let err = parse_filters("todoist://tasks?query=x").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("query") && msg.contains("not supported"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn endpoint_url_tasks_default_shape() {
        let spec = parse_with_limit("todoist://tasks?limit=5", 5).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/tasks?limit=5"
        );
    }

    #[test]
    fn endpoint_url_tasks_with_project_id() {
        let spec = parse_with_limit("todoist://tasks?project_id=123&limit=5", 5).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/tasks?limit=5&project_id=123"
        );
    }

    #[test]
    fn endpoint_url_tasks_filter_encodes_query() {
        let spec =
            parse_with_limit("todoist://tasks?filter=today%20%7C%20overdue&limit=5", 5).unwrap();
        assert_eq!(spec.filter.as_deref(), Some("today | overdue"));
        let url = build_request_url(&spec, None);
        assert!(
            url.starts_with("https://api.todoist.com/api/v1/tasks/filter?"),
            "unexpected endpoint: {url}"
        );
        // The raw space and `|` must not appear literally in the URL.
        assert!(!url.contains(' '), "space must be encoded: {url}");
        assert!(!url.contains('|'), "pipe must be encoded: {url}");
    }

    #[test]
    fn endpoint_url_projects_shape() {
        let spec = parse_with_limit("todoist://projects?limit=5", 5).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/projects?limit=5"
        );
    }

    // ---- normalize_todoist ----

    fn task_fixture() -> serde_json::Value {
        // Field names/shapes verbatim from the official Todoist API v1
        // response (module docs "URI grammar" / task spec).
        serde_json::json!({
            "id": "6X7rM8997g3RQmvh",
            "content": "Buy Milk",
            "description": "Some description",
            "project_id": "6Cq6MFQP7wCXjcgP",
            "section_id": serde_json::Value::Null,
            "parent_id": serde_json::Value::Null,
            "labels": ["Food", "Shopping"],
            "priority": 1,
            "due": {
                "date": "2026-07-08",
                "timezone": serde_json::Value::Null,
                "string": "tomorrow",
                "lang": "en",
                "is_recurring": false
            },
            "deadline": {
                "date": "2026-07-10",
                "lang": "en"
            },
            "duration": serde_json::Value::Null,
            "is_collapsed": false,
            "child_order": 1,
            "day_order": -1,
            "responsible_uid": serde_json::Value::Null,
            "assigned_by_uid": serde_json::Value::Null,
            "completed_at": serde_json::Value::Null,
            "added_by_uid": "2671355",
            "added_at": "2026-07-01T08:25:05.000000Z",
            "updated_at": "2026-07-02T08:25:05.000000Z",
        })
    }

    #[test]
    fn normalize_todoist_tasks_field_mapping() {
        let raw = serde_json::json!({ "results": [task_fixture()], "next_cursor": serde_json::Value::Null });
        let spec = parse("todoist://tasks").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "tasks");
        let item = &v["items"][0];
        assert_eq!(item["id"].as_str().unwrap(), "6X7rM8997g3RQmvh");
        assert_eq!(item["content"].as_str().unwrap(), "Buy Milk");
        assert_eq!(
            item["description_excerpt"].as_str().unwrap(),
            "Some description"
        );
        assert_eq!(item["project_id"].as_str().unwrap(), "6Cq6MFQP7wCXjcgP");
        assert_eq!(item["priority"].as_u64().unwrap(), 1);
        let labels: Vec<&str> = item["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["Food", "Shopping"]);
        assert_eq!(item["due"]["date"].as_str().unwrap(), "2026-07-08");
        assert_eq!(item["due"]["string"].as_str().unwrap(), "tomorrow");
        assert!(!item["due"]["is_recurring"].as_bool().unwrap());
        assert_eq!(item["deadline_date"].as_str().unwrap(), "2026-07-10");
        assert!(item["completed_at"].is_null());
        assert_eq!(
            item["added_at"].as_str().unwrap(),
            "2026-07-01T08:25:05.000000Z"
        );
        assert_eq!(
            item["updated_at"].as_str().unwrap(),
            "2026-07-02T08:25:05.000000Z"
        );
    }

    #[test]
    fn normalize_todoist_tasks_no_due_is_null() {
        let mut fixture = task_fixture();
        fixture["due"] = serde_json::Value::Null;
        fixture["deadline"] = serde_json::Value::Null;
        let raw = serde_json::json!({ "results": [fixture] });
        let spec = parse("todoist://tasks").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        let item = &v["items"][0];
        assert!(item["due"].is_null());
        assert!(item["deadline_date"].is_null());
    }

    #[test]
    fn normalize_todoist_tasks_empty_description_is_null() {
        let mut fixture = task_fixture();
        fixture["description"] = serde_json::Value::String(String::new());
        let raw = serde_json::json!({ "results": [fixture] });
        let spec = parse("todoist://tasks").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        assert!(v["items"][0]["description_excerpt"].is_null());
    }

    #[test]
    fn normalize_todoist_tasks_missing_fields_are_null() {
        let raw = serde_json::json!({ "results": [{ "id": "1" }] });
        let spec = parse("todoist://tasks").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        let item = &v["items"][0];
        assert!(item["content"].is_null());
        assert!(item["description_excerpt"].is_null());
        assert!(item["due"].is_null());
        assert!(item["deadline_date"].is_null());
        assert_eq!(
            item["labels"].as_array().unwrap().len(),
            0,
            "no `labels` key -> empty array"
        );
    }

    #[test]
    fn normalize_todoist_tasks_description_truncated() {
        let long_description = "a".repeat(600);
        let mut fixture = task_fixture();
        fixture["description"] = serde_json::Value::String(long_description);
        let raw = serde_json::json!({ "results": [fixture] });
        let spec = parse("todoist://tasks").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        let excerpt = v["items"][0]["description_excerpt"].as_str().unwrap();
        assert_eq!(
            excerpt.chars().count(),
            DESCRIPTION_MAX_CHARS + 1,
            "500 + ellipsis"
        );
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn normalize_todoist_projects_field_mapping() {
        let raw = serde_json::json!({
            "results": [{
                "id": "6Cq6MFQP7wCXjcgP",
                "name": "Groceries",
                "description": "",
                "parent_id": serde_json::Value::Null,
                "folder_id": serde_json::Value::Null,
                "workspace_id": serde_json::Value::Null,
                "child_order": 1,
                "color": "charcoal",
                "is_shared": false,
                "is_collapsed": false,
                "is_favorite": true,
                "inbox_project": false,
                "can_assign_tasks": false,
                "is_archived": false,
                "view_style": "list",
                "created_at": "2026-01-01T00:00:00.000000Z",
                "updated_at": "2026-01-02T00:00:00.000000Z",
            }]
        });
        let spec = parse("todoist://projects").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "projects");
        let item = &v["items"][0];
        assert_eq!(item["id"].as_str().unwrap(), "6Cq6MFQP7wCXjcgP");
        assert_eq!(item["name"].as_str().unwrap(), "Groceries");
        assert_eq!(item["color"].as_str().unwrap(), "charcoal");
        assert!(item["is_favorite"].as_bool().unwrap());
        assert!(!item["is_archived"].as_bool().unwrap());
        assert!(!item["is_inbox"].as_bool().unwrap());
        assert_eq!(item["view_style"].as_str().unwrap(), "list");
    }

    #[test]
    fn normalize_todoist_empty_results() {
        let raw = serde_json::json!({ "results": [], "next_cursor": serde_json::Value::Null });
        let spec = parse("todoist://tasks").unwrap();
        let v = normalize_todoist(&spec, &raw).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn normalize_todoist_non_object_response_fails_loud() {
        let raw = serde_json::json!([1, 2, 3]);
        let spec = parse("todoist://tasks").unwrap();
        let err = normalize_todoist(&spec, &raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected an object with a 'results' array"),
            "unexpected error: {msg}"
        );
    }

    // ---- internal pagination (build_request_url) ----
    //
    // The Link-header / cursor loop is driven internally by `Adapter::fetch`
    // over `HttpClient` (a concrete struct not behind a mockable trait), and
    // this workspace's convention (established in `adapter.rs` crate docs) is
    // that Adapter tests are offline unit tests over inline fixtures. The
    // request-URL construction is exercised as a pure function below.

    #[test]
    fn build_request_url_tasks_default_caps_at_max_limit() {
        // ?limit=5: per-request limit = min(5, 200) = 5.
        let spec = parse_with_limit("todoist://tasks?limit=5", 5).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/tasks?limit=5"
        );
    }

    #[test]
    fn build_request_url_tasks_over_max_caps_at_max() {
        // ?limit=500: per-request limit = min(500, MAX_LIMIT=200) = 200.
        let spec = parse_with_limit("todoist://tasks?limit=500", 500).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/tasks?limit=200"
        );
    }

    #[test]
    fn build_request_url_tasks_over_max_with_project_scope() {
        let spec = parse_with_limit("todoist://tasks?project_id=123&limit=500", 500).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/tasks?limit=200&project_id=123"
        );
    }

    #[test]
    fn build_request_url_projects_over_max_caps_at_max() {
        let spec = parse_with_limit("todoist://projects?limit=500", 500).unwrap();
        assert_eq!(
            build_request_url(&spec, None),
            "https://api.todoist.com/api/v1/projects?limit=200"
        );
    }

    #[test]
    fn build_request_url_with_cursor_appends_token() {
        let spec = parse_with_limit("todoist://tasks?limit=500", 500).unwrap();
        let url = build_request_url(&spec, Some("abc"));
        assert!(url.contains("cursor=abc"), "unexpected url: {url}");
        assert!(url.contains("limit=200"), "unexpected url: {url}");
    }
}
