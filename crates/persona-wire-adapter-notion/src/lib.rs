//! persona-wire Adapter for Notion (scheme `notion://`).
//!
//! ## Architecture
//!
//! `NotionAdapter` is a stateless [`Adapter`] impl split into three
//! independent responsibilities:
//!
//! - [`parse_notion_uri`] — `WireUri` → `NotionUriSpec` (endpoint kind +
//!   optional search query/object filter + item limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
//!   Notion-specific knowledge in the transport layer).
//! - Per-kind loop drivers (`fetch_search` / `drive_data_source_loop` /
//!   `fetch_page_kind`) — accumulate results across `next_cursor` pages and
//!   assemble the Wire JSON shape (see "Output shape" below), one per
//!   endpoint kind.
//!
//! ## URI grammar
//!
//! ```text
//! notion://search[?query=<text>][&object=page|data_source][&limit=N]
//! notion://database/<database_id>[?limit=N]
//! notion://data-source/<data_source_id>[?limit=N]
//! notion://page/<page_id>[?limit=N]
//! ```
//!
//! - `host` selects the endpoint kind (`search` / `database` / `data-source`
//!   / `page`); an empty or invalid value **fails loud** — a typo here would
//!   otherwise silently return a different class of data (matching
//!   `persona-wire-adapter-todoist`'s host-selects-kind convention).
//! - For `kind=search`, the path must be empty (or `/`); `?query=<text>` is
//!   optional (omitted = search everything, title match only per the Notion
//!   API) and is percent-decoded once at parse time, the same convention as
//!   `persona-wire-adapter-todoist`'s `filter`. `?object=page|data_source`
//!   restricts the search to one object type (Notion's `filter.value` enum
//!   as of API version 2025-09-03, which renamed the legacy `database` value
//!   to `data_source`); an invalid value fails loud.
//! - For `kind=database` / `kind=data-source` / `kind=page`, the path must
//!   be exactly one segment (the id); a missing id, or any additional path
//!   segment, fails loud.
//! - `kind=database` first resolves the database's data sources
//!   (`GET /databases/{id}`) before querying: exactly one data source
//!   continues transparently; zero **fails loud** ("no data sources"); two
//!   or more **fails loud** and lists the `notion://data-source/<id>` URIs
//!   to pick from explicitly (a database can have multiple typed data
//!   sources since the 2025-09-03 multi-source-database API change).
//! - `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
//!   A non-numeric or zero value fails loud; there is no upper bound at
//!   parse time. [`MAX_LIMIT`] (Notion's own `page_size` ceiling of 100)
//!   is enforced only when the adapter builds each upstream request;
//!   `?limit=N` with `N > MAX_LIMIT` triggers the internal pagination
//!   loop (see "Pagination" below). `page_size` is always sent explicitly
//!   to the Notion API (the default behavior for an absent `page_size` is
//!   undocumented).
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss` / `-github` / `-todoist`); for
//!   `kind=database` / `-data-source` / `-page`, `query` / `object` are
//!   themselves unknown query keys (not even read).
//! - `kind=page` fetches only the page's direct child blocks
//!   (`GET /blocks/{id}/children`) — nested children (`has_children=true`)
//!   are **not** recursively fetched, as a context size guard.
//!
//! ## Auth
//!
//! Resolved per-fetch (not at boot) via
//! `persona_wire_credentials::Credentials::default_chain().get("notion")`.
//! Like `persona-wire-adapter-todoist`, Notion has no unauthenticated access
//! mode — a missing token **fails loud**. Set a token via the
//! `PERSONA_WIRE_TOKEN_NOTION` or `NOTION_TOKEN` environment variable, or
//! store one in the OS keychain via `persona-wire token set notion`. The
//! token is a Notion internal integration secret (`ntn_...` / legacy
//! `secret_...` prefix, minted on a workspace's Settings → Connections →
//! Develop or manage integrations page).
//!
//! **The integration must also be explicitly shared with each page or
//! database** via that page/database's "•••" menu → "Add connections" —
//! Notion returns HTTP 404 for otherwise-valid ids the integration has not
//! been granted access to, which surfaces as a normal fetch failure via
//! `persona_wire_transport_http::HttpClient`.
//!
//! The literal `"notion"` service key is overridable per-fetch via the
//! URI's `?auth=<service_key>` query param (see `persona_wire_core::
//! infrastructure::adapter`'s "External service integration policy" for the
//! convention); absent, behavior is unchanged.
//!
//! Notion enforces an average rate limit of roughly 3 requests per second
//! per integration; exceeding it returns HTTP 429 with a `Retry-After`
//! header. This adapter does not implement client-side throttling — a 429
//! surfaces as a normal fetch failure.
//!
//! ## Output shape
//!
//! For `kind=search`:
//!
//! ```json
//! { "kind": "search", "query": "...|null", "items": [ ... ], "has_more": false }
//! ```
//!
//! For `kind=database` / `kind=data-source` (both resolve to a data source
//! query):
//!
//! ```json
//! { "kind": "data_source_query", "data_source_id": "...", "items": [ ... ], "has_more": false }
//! ```
//!
//! `items` entries for both of the above (Notion page objects):
//!
//! ```json
//! {
//!   "id": "...|null", "object": "...|null",
//!   "title": "...|null", "url": "...|null",
//!   "last_edited_time": "...|null", "in_trash": false
//! }
//! ```
//!
//! `title` is extracted by scanning `properties` for the entry whose
//! `type == "title"` (the property's own name is user-defined and not
//! fixed, e.g. "Name" / "Title" / anything) and concatenating its rich-text
//! runs' `plain_text`; `null` when no such property exists or it yields no
//! text.
//!
//! For `kind=page`:
//!
//! ```json
//! { "kind": "page", "page_id": "...", "blocks": [ ... ], "has_more": false }
//! ```
//!
//! `blocks` entries:
//!
//! ```json
//! { "type": "...|null", "text": "...|null" }
//! ```
//!
//! `text` is the block type's own rich-text array's `plain_text` runs
//! concatenated and truncated to [`TEXT_MAX_CHARS`] `char`s; block types
//! without a rich-text array (e.g. `divider` / `child_page` / `image`) carry
//! `text: null` alongside their `type`.
//!
//! ## Pagination
//!
//! `Adapter::fetch` drives the pagination loop internally: it follows the
//! response body's `next_cursor` field (an opaque token; `has_more: false`
//! or a `null`/absent `next_cursor` signals end-of-data) across repeated
//! requests until it has accumulated `?limit=N` items or the upstream
//! signals end-of-data. The cursor form is a private implementation
//! detail — the wire layer only sees the final assembled per-kind shape
//! with a truthful `has_more` field.
//!
//! Every upstream request is sent with `page_size = min(spec.limit,
//! MAX_LIMIT)` (Notion's own per-request ceiling of 100), so the loop runs
//! once for `?limit <= MAX_LIMIT` and continues page-by-page for larger
//! requests. All four kinds (`search` / `database` / `data-source` /
//! `page`) paginate the same way; `kind=database` resolves the single data
//! source id once up front (before the loop starts), then re-uses it for
//! every page.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{FilterCap, WireError, WireFilters, WireResult};
use persona_wire_credentials::Credentials;
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `items` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Maximum allowed `?limit=` value (Notion API's own `page_size` ceiling).
pub const MAX_LIMIT: usize = 100;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-adapter-todoist::FETCH_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Max block/title text length in `char`s before truncation (context size
/// guard).
pub const TEXT_MAX_CHARS: usize = 500;

/// Notion API base URL.
pub const API_BASE: &str = "https://api.notion.com/v1";

/// Notion API version pinned by this adapter (sent via the `Notion-Version`
/// header on every request). As of this version, `archived` was renamed to
/// `in_trash` on page/block objects, and search's `filter.value` enum uses
/// `data_source` (not the legacy `database`).
pub const NOTION_VERSION: &str = "2026-03-11";

/// persona-wire Adapter for Notion (`notion://` scheme).
pub struct NotionAdapter;

#[async_trait]
impl Adapter for NotionAdapter {
    fn scheme(&self) -> &'static str {
        "notion"
    }

    fn filter_caps(&self) -> &'static [FilterCap] {
        &[FilterCap::Limit { max: None }, FilterCap::TextQuery]
    }

    /// Fetch `spec.kind` items, driving the `next_cursor` pagination loop
    /// internally until `?limit=N` items are accumulated or the upstream
    /// signals end-of-data. See the module docs for URI grammar, auth
    /// resolution, and output shape (including `has_more` semantics).
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let filters = WireFilters::parse(uri, self.filter_caps())?;
        let spec = parse_notion_uri(uri, &filters)?;
        let client = notion_http_client(uri)?;
        match &spec.kind {
            NotionKind::Search => fetch_search(&client, &spec).await,
            NotionKind::Database(database_id) => {
                let raw_db = client
                    .get_json(&format!("{API_BASE}/databases/{database_id}"))
                    .await?;
                let data_source_id = resolve_single_data_source(&raw_db)?;
                let (items, has_more) =
                    drive_data_source_loop(&client, &data_source_id, spec.limit).await?;
                Ok(serde_json::json!({
                    "kind": "data_source_query",
                    // For `kind=database`, the URI-supplied identifier is a
                    // database id, but the actual query targets a resolved
                    // single data source. The wrapped shape carries the
                    // database id (matches the module docs "Pagination"
                    // note): callers referring to their original URI find
                    // the same id back on the response.
                    "data_source_id": database_id,
                    "items": items,
                    "has_more": has_more,
                }))
            }
            NotionKind::DataSource(data_source_id) => {
                let (items, has_more) =
                    drive_data_source_loop(&client, data_source_id, spec.limit).await?;
                Ok(serde_json::json!({
                    "kind": "data_source_query",
                    "data_source_id": data_source_id,
                    "items": items,
                    "has_more": has_more,
                }))
            }
            NotionKind::Page(page_id) => fetch_page_kind(&client, page_id, spec.limit).await,
        }
    }
}

/// Builds a fresh, Notion-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth"). Shared by every fetch path so all
/// stay in sync on headers/timeout/version.
fn notion_http_client(uri: &WireUri) -> WireResult<HttpClient> {
    // Auth is resolved per-fetch (not at boot); see module docs "Auth".
    // Notion has no unauthenticated access mode, unlike the github
    // adapter, so a missing token fails loud here.
    let service_key = resolve_service_key(uri, "notion");
    let token = Credentials::default_chain().get(service_key)?.ok_or_else(|| {
        if service_key == "notion" {
            WireError::Storage(
                "notion adapter: no token found for 'notion' (set PERSONA_WIRE_TOKEN_NOTION / NOTION_TOKEN, or run 'persona-wire token set notion')"
                    .to_string(),
            )
        } else {
            WireError::Storage(format!(
                "notion adapter: no token found for '{service_key}' (set PERSONA_WIRE_TOKEN_<KEY> uppercased, or run 'persona-wire token set {service_key}')"
            ))
        }
    })?;
    Ok(HttpClient::new("notion adapter")
        .with_timeout(FETCH_TIMEOUT)
        .with_header("Notion-Version", NOTION_VERSION)
        .with_bearer(token))
}

/// Resolves the credential service key for this fetch: the URI's
/// `?auth=<service_key>` query param when present (reference key only,
/// never a secret — see `persona_wire_core::infrastructure::adapter`'s
/// "External service integration policy"), otherwise `default_key` (this
/// adapter's literal `"notion"` service name, preserving pre-existing
/// behavior when the param is absent).
fn resolve_service_key<'a>(uri: &'a WireUri, default_key: &'static str) -> &'a str {
    uri.query_get("auth").unwrap_or(default_key)
}

/// The four Notion endpoint kinds this adapter can target, selected via the
/// URI host. `Database` / `DataSource` / `Page` carry the parsed id
/// segment (so no `Option`/invariant juggling is needed downstream — the
/// id is only reachable through the variant that requires it).
#[derive(Debug, Clone, PartialEq, Eq)]
enum NotionKind {
    Search,
    Database(String),
    DataSource(String),
    Page(String),
}

/// Parsed `notion://` URI: endpoint kind (+ id, when applicable) + optional
/// search query/object filter + item limit.
#[derive(Debug)]
struct NotionUriSpec {
    kind: NotionKind,
    /// `Some` only for `kind == Search` when `?query=` is present.
    query: Option<String>,
    /// `Some` only for `kind == Search` when `?object=` is present.
    object: Option<String>,
    limit: usize,
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`NotionUriSpec`], using `filters` for cross-cutting `?limit=` /
/// `?query=` values (already parsed and validated by
/// [`WireFilters::parse`]). See the module-level "URI grammar" section for
/// the exact rules and failure conditions.
fn parse_notion_uri(uri: &WireUri, filters: &WireFilters) -> WireResult<NotionUriSpec> {
    let limit = filters.limit.unwrap_or(DEFAULT_LIMIT);

    let kind = match uri.host() {
        Some("search") => {
            let path = uri.path();
            if !path.is_empty() && path != "/" {
                return Err(WireError::Storage(format!(
                    "notion adapter: unexpected path segment in '{}' (expected notion://search)",
                    uri.as_raw()
                )));
            }
            NotionKind::Search
        }
        Some("database") => NotionKind::Database(parse_single_id_segment(uri, "database")?),
        Some("data-source") => NotionKind::DataSource(parse_single_id_segment(uri, "data-source")?),
        Some("page") => NotionKind::Page(parse_single_id_segment(uri, "page")?),
        Some(bad) if !bad.is_empty() => {
            return Err(WireError::Storage(format!(
                "notion adapter: invalid kind '{bad}' (must be one of: search, database, data-source, page)"
            )));
        }
        _ => {
            return Err(WireError::Storage(format!(
                "notion adapter: missing kind (host) in '{}' (expected notion://search, notion://database/<id>, notion://data-source/<id>, or notion://page/<id>)",
                uri.as_raw()
            )));
        }
    };

    // `query` is meaningful only for Search; other kinds silently ignore a
    // supplied `filters.query` (preserving pre-Phase-2 behavior). `object`
    // is an adapter-specific addressing key (not part of the WireFilters
    // vocabulary) and stays inline here.
    let (query, object) = if kind == NotionKind::Search {
        let query = filters.query.clone();
        let object = match uri.query_get("object") {
            None => None,
            Some(o @ ("page" | "data_source")) => Some(o.to_string()),
            Some(bad) => {
                return Err(WireError::Storage(format!(
                    "notion adapter: invalid object '{bad}' (must be one of: page, data_source)"
                )));
            }
        };
        (query, object)
    } else {
        (None, None)
    };

    Ok(NotionUriSpec {
        kind,
        query,
        object,
        limit,
    })
}

/// Parses the single required path segment (the id) for `kind_label`
/// (`"database"` / `"data-source"` / `"page"`). A missing or extra segment
/// fails loud.
fn parse_single_id_segment(uri: &WireUri, kind_label: &str) -> WireResult<String> {
    let segments: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [id] => Ok(id.to_string()),
        [] => Err(WireError::Storage(format!(
            "notion adapter: missing id in '{}' (expected notion://{kind_label}/<id>)",
            uri.as_raw()
        ))),
        _ => Err(WireError::Storage(format!(
            "notion adapter: unexpected extra path segment(s) in '{}' (expected notion://{kind_label}/<id>)",
            uri.as_raw()
        ))),
    }
}

// (`parse_limit` was removed — cross-cutting `?limit=` parsing is now done
// by `WireFilters::parse` on the adapter side.)

/// Builds the `POST /search` request body for `spec` — only the fields
/// present in the URI are included (module docs "URI grammar").
fn search_request_body(spec: &NotionUriSpec) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    if let Some(q) = &spec.query {
        body.insert("query".to_string(), serde_json::Value::String(q.clone()));
    }
    if let Some(obj) = &spec.object {
        body.insert(
            "filter".to_string(),
            serde_json::json!({ "property": "object", "value": obj }),
        );
    }
    body.insert(
        "page_size".to_string(),
        serde_json::json!(spec.limit.min(MAX_LIMIT)),
    );
    serde_json::Value::Object(body)
}

/// Builds the `POST /search` request body for `spec` with an optional
/// `start_cursor` inserted (the internal pagination loop in
/// [`Adapter::fetch`] threads the token returned in the previous page's
/// `next_cursor`). `cursor = None` is byte-identical to
/// [`search_request_body`] — kept as a separate function so
/// [`search_request_body`]'s existing tests stay untouched.
fn search_request_body_with_cursor(
    spec: &NotionUriSpec,
    cursor: Option<&str>,
) -> serde_json::Value {
    let mut body = search_request_body(spec);
    if let Some(token) = cursor {
        body.as_object_mut()
            .expect("search_request_body always returns a JSON object")
            .insert(
                "start_cursor".to_string(),
                serde_json::Value::String(token.to_string()),
            );
    }
    body
}

/// Builds the `POST /data_sources/{id}/query` request body for the
/// wire-layer pagination path, with an optional `start_cursor`. Shared by
/// `kind=database` (after resolving the single data source) and
/// `kind=data-source`. Always requests `page_size = `[`MAX_LIMIT`] — the
/// wire-layer driver caps the total item count across accumulated pages,
/// not per-request (mirrors
/// `TodoistUriSpec::endpoint_url_for_first_page`'s "always request the
/// adapter's max page size" convention).
fn data_source_query_body(cursor: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert("page_size".to_string(), serde_json::json!(MAX_LIMIT));
    if let Some(token) = cursor {
        body.insert(
            "start_cursor".to_string(),
            serde_json::Value::String(token.to_string()),
        );
    }
    serde_json::Value::Object(body)
}

/// Builds the `GET /blocks/{page_id}/children` request URL for the
/// wire-layer pagination path, with an optional `start_cursor` query
/// param (percent-encoded — Notion's cursor is opaque and may contain
/// characters requiring encoding). Always requests `page_size = `
/// [`MAX_LIMIT`], for the same reason as [`data_source_query_body`].
fn page_children_url(page_id: &str, cursor: Option<&str>) -> String {
    match cursor {
        Some(token) => {
            let encoded =
                percent_encoding::utf8_percent_encode(token, percent_encoding::NON_ALPHANUMERIC);
            format!(
                "{API_BASE}/blocks/{page_id}/children?page_size={MAX_LIMIT}&start_cursor={encoded}"
            )
        }
        None => format!("{API_BASE}/blocks/{page_id}/children?page_size={MAX_LIMIT}"),
    }
}

/// Extracts the raw `results` JSON array from a Notion API response, failing
/// loud (naming `context`) when the response isn't shaped as expected.
/// Shared by the internal loop drivers in [`Adapter::fetch`] and the
/// `normalize_*` test helpers.
fn extract_results<'a>(
    raw: &'a serde_json::Value,
    context: &str,
) -> WireResult<&'a Vec<serde_json::Value>> {
    raw.get("results").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(format!(
            "notion adapter: unexpected response shape for {context}: expected an object with a 'results' array"
        ))
    })
}

/// Extracts the pagination cursor token from a Notion API response body:
/// `has_more: false`, or a `null`/absent `next_cursor`, both signal
/// end-of-data (`None`).
fn next_cursor_token(raw: &serde_json::Value) -> Option<String> {
    let has_more = raw
        .get("has_more")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !has_more {
        return None;
    }
    raw.get("next_cursor")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Drives the `next_cursor` loop for a `POST /data_sources/{id}/query`
/// endpoint. Shared by `NotionKind::Database` (after the up-front
/// resolve call) and `NotionKind::DataSource`.
async fn drive_data_source_loop(
    client: &HttpClient,
    data_source_id: &str,
    limit: usize,
) -> WireResult<(Vec<serde_json::Value>, bool)> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let has_more = loop {
        let body = data_source_query_body(cursor.as_deref());
        let raw = client
            .post_json(
                &format!("{API_BASE}/data_sources/{data_source_id}/query"),
                &body,
            )
            .await?;
        let results = extract_results(&raw, &format!("data source '{data_source_id}' query"))?;
        items.extend(results.iter().map(normalize_page_item));
        let next = next_cursor_token(&raw);
        if items.len() >= limit {
            break items.len() > limit || next.is_some();
        }
        match next {
            Some(t) => cursor = Some(t),
            None => break false,
        }
    };
    items.truncate(limit);
    Ok((items, has_more))
}

/// Drives the `next_cursor` loop for `POST /search`.
async fn fetch_search(client: &HttpClient, spec: &NotionUriSpec) -> WireResult<serde_json::Value> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let has_more = loop {
        let body = search_request_body_with_cursor(spec, cursor.as_deref());
        let raw = client
            .post_json(&format!("{API_BASE}/search"), &body)
            .await?;
        let results = extract_results(&raw, "search")?;
        items.extend(results.iter().map(normalize_page_item));
        let next = next_cursor_token(&raw);
        if items.len() >= spec.limit {
            break items.len() > spec.limit || next.is_some();
        }
        match next {
            Some(t) => cursor = Some(t),
            None => break false,
        }
    };
    items.truncate(spec.limit);
    Ok(serde_json::json!({
        "kind": "search",
        "query": spec.query,
        "items": items,
        "has_more": has_more,
    }))
}

/// Drives the `next_cursor` loop for `GET /blocks/{page_id}/children`.
async fn fetch_page_kind(
    client: &HttpClient,
    page_id: &str,
    limit: usize,
) -> WireResult<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let has_more = loop {
        let url = page_children_url(page_id, cursor.as_deref());
        let raw = client.get_json(&url).await?;
        let results = extract_results(&raw, &format!("page '{page_id}' blocks"))?;
        blocks.extend(results.iter().map(normalize_block));
        let next = next_cursor_token(&raw);
        if blocks.len() >= limit {
            break blocks.len() > limit || next.is_some();
        }
        match next {
            Some(t) => cursor = Some(t),
            None => break false,
        }
    };
    blocks.truncate(limit);
    Ok(serde_json::json!({
        "kind": "page",
        "page_id": page_id,
        "blocks": blocks,
        "has_more": has_more,
    }))
}

/// Resolves the single data source id for a `GET /databases/{id}`
/// response. A database can have multiple typed data sources (since the
/// 2025-09-03 multi-source-database API change) — zero or 2+ sources fail
/// loud rather than guessing (module docs "URI grammar").
fn resolve_single_data_source(raw_db: &serde_json::Value) -> WireResult<String> {
    let sources = raw_db
        .get("data_sources")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            WireError::Storage(
                "notion adapter: unexpected response shape for database: expected a 'data_sources' array"
                    .to_string(),
            )
        })?;
    match sources.len() {
        0 => Err(WireError::Storage(
            "notion adapter: database has no data sources".to_string(),
        )),
        1 => sources[0]
            .get("id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                WireError::Storage(
                    "notion adapter: database's data source entry is missing 'id'".to_string(),
                )
            }),
        n => {
            let choices: Vec<String> = sources
                .iter()
                .filter_map(|s| s.get("id").and_then(|x| x.as_str()))
                .map(|id| format!("notion://data-source/{id}"))
                .collect();
            Err(WireError::Storage(format!(
                "notion adapter: database has {n} data sources; use one of: {}",
                choices.join(", ")
            )))
        }
    }
}

/// Extracts the plain-text title from a Notion page object's `properties`
/// map by scanning for the entry whose `type == "title"` (the property's
/// own name is user-defined, e.g. "Name" / "Title" / anything). Returns
/// `None` when no such property exists, or it has no rich-text runs.
fn extract_title(page: &serde_json::Value) -> Option<String> {
    let properties = page.get("properties")?.as_object()?;
    for prop in properties.values() {
        if prop.get("type").and_then(|t| t.as_str()) != Some("title") {
            continue;
        }
        let title_arr = prop.get("title")?.as_array()?;
        let text: String = title_arr
            .iter()
            .filter_map(|rt| rt.get("plain_text").and_then(|x| x.as_str()))
            .collect();
        return if text.is_empty() { None } else { Some(text) };
    }
    None
}

/// Normalizes a single Notion page JSON object (shared by `kind=search` and
/// `kind=data_source_query` results — both return the same page object
/// shape). See module docs "Output shape".
fn normalize_page_item(v: &serde_json::Value) -> serde_json::Value {
    let id = v.get("id").and_then(|x| x.as_str());
    let object = v.get("object").and_then(|x| x.as_str());
    let title = extract_title(v);
    let url = v.get("url").and_then(|x| x.as_str());
    let last_edited_time = v.get("last_edited_time").and_then(|x| x.as_str());
    let in_trash = v.get("in_trash").and_then(|x| x.as_bool());

    serde_json::json!({
        "id": id,
        "object": object,
        "title": title,
        "url": url,
        "last_edited_time": last_edited_time,
        "in_trash": in_trash,
    })
}

/// Normalizes a single-page `POST /search` response into the Wire JSON
/// shape. Used by unit tests only; [`Adapter::fetch`] drives the multi-page
/// loop and assembles the shape inline.
#[cfg(test)]
fn normalize_search(
    spec: &NotionUriSpec,
    raw: &serde_json::Value,
) -> WireResult<serde_json::Value> {
    let results = raw.get("results").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(
            "notion adapter: unexpected response shape for search: expected an object with a 'results' array"
                .to_string(),
        )
    })?;
    let items: Vec<serde_json::Value> = results.iter().map(normalize_page_item).collect();
    let has_more = raw
        .get("has_more")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "kind": "search",
        "query": spec.query,
        "items": items,
        "has_more": has_more,
    }))
}

/// Normalizes a single-page `POST /data_sources/{id}/query` response into
/// the Wire JSON shape. Used by unit tests only; [`Adapter::fetch`] drives
/// the multi-page loop and assembles the shape inline.
#[cfg(test)]
fn normalize_data_source_query(
    data_source_id: &str,
    raw: &serde_json::Value,
) -> WireResult<serde_json::Value> {
    let results = raw.get("results").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(format!(
            "notion adapter: unexpected response shape for data source '{data_source_id}' query: expected an object with a 'results' array"
        ))
    })?;
    let items: Vec<serde_json::Value> = results.iter().map(normalize_page_item).collect();
    let has_more = raw
        .get("has_more")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "kind": "data_source_query",
        "data_source_id": data_source_id,
        "items": items,
        "has_more": has_more,
    }))
}

/// Extracts and truncates a single block's rich-text `plain_text` runs.
/// Block types without a rich-text array (e.g. `divider` / `child_page` /
/// `image`) yield `None`.
fn extract_block_text(block: &serde_json::Value) -> Option<String> {
    let block_type = block.get("type").and_then(|x| x.as_str())?;
    let rich_text = block.get(block_type)?.get("rich_text")?.as_array()?;
    let text: String = rich_text
        .iter()
        .filter_map(|rt| rt.get("plain_text").and_then(|x| x.as_str()))
        .collect();
    Some(truncate_text(&text))
}

/// Normalizes a single Notion block JSON object.
fn normalize_block(block: &serde_json::Value) -> serde_json::Value {
    let block_type = block.get("type").and_then(|x| x.as_str());
    let text = extract_block_text(block);
    serde_json::json!({
        "type": block_type,
        "text": text,
    })
}

/// Normalizes a single-page `GET /blocks/{id}/children` response into the
/// Wire JSON shape. Used by unit tests only; [`Adapter::fetch`] drives the
/// multi-page loop and assembles the shape inline.
#[cfg(test)]
fn normalize_page(page_id: &str, raw: &serde_json::Value) -> WireResult<serde_json::Value> {
    let results = raw.get("results").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(format!(
            "notion adapter: unexpected response shape for page '{page_id}' blocks: expected an object with a 'results' array"
        ))
    })?;
    let blocks: Vec<serde_json::Value> = results.iter().map(normalize_block).collect();
    let has_more = raw
        .get("has_more")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "kind": "page",
        "page_id": page_id,
        "blocks": blocks,
        "has_more": has_more,
    }))
}

/// Truncate `s` to at most [`TEXT_MAX_CHARS`] `char`s (boundary-safe —
/// counts `char`s, not bytes), appending `…` when truncation occurred.
/// Mirrors `persona-wire-adapter-github::truncate_body`.
fn truncate_text(s: &str) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(TEXT_MAX_CHARS).collect();
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
        let uri = WireUri::parse("notion://search").unwrap();
        assert_eq!(resolve_service_key(&uri, "notion"), "notion");
    }

    #[test]
    fn resolve_service_key_overrides_when_auth_param_present() {
        let uri = WireUri::parse("notion://search?auth=notion-alt").unwrap();
        assert_eq!(resolve_service_key(&uri, "notion"), "notion-alt");
    }

    // ---- parse_notion_uri ----

    /// Helper: run WireFilters::parse against the adapter's declared caps
    /// and thread the resulting snapshot into `parse_notion_uri`, matching
    /// the production `NotionAdapter::fetch` code path.
    fn parse(uri: &str) -> WireResult<NotionUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        let filters = WireFilters::parse(&wire, NotionAdapter.filter_caps())?;
        parse_notion_uri(&wire, &filters)
    }

    #[test]
    fn parse_notion_uri_kind_search_default() {
        let spec = parse("notion://search").unwrap();
        assert_eq!(spec.kind, NotionKind::Search);
        assert_eq!(spec.query, None);
        assert_eq!(spec.object, None);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_notion_uri_kind_search_query_decoded() {
        let spec = parse("notion://search?query=Bug%20bash").unwrap();
        assert_eq!(spec.query.as_deref(), Some("Bug bash"));
    }

    #[test]
    fn parse_notion_uri_kind_search_object_page() {
        let spec = parse("notion://search?object=page").unwrap();
        assert_eq!(spec.object.as_deref(), Some("page"));
    }

    #[test]
    fn parse_notion_uri_kind_search_object_data_source() {
        let spec = parse("notion://search?object=data_source").unwrap();
        assert_eq!(spec.object.as_deref(), Some("data_source"));
    }

    #[test]
    fn parse_notion_uri_kind_search_invalid_object_fails_loud() {
        let err = parse("notion://search?object=database").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid object"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_search_extra_path_fails_loud() {
        let err = parse("notion://search/extra").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unexpected path segment"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_notion_uri_kind_database() {
        let spec = parse("notion://database/d9824bdc-8445-4327-be8b-5b47500af6ce").unwrap();
        assert_eq!(
            spec.kind,
            NotionKind::Database("d9824bdc-8445-4327-be8b-5b47500af6ce".to_string())
        );
    }

    #[test]
    fn parse_notion_uri_kind_data_source() {
        let spec = parse("notion://data-source/1a44be12-0953-4631-b498-9e5817518db8").unwrap();
        assert_eq!(
            spec.kind,
            NotionKind::DataSource("1a44be12-0953-4631-b498-9e5817518db8".to_string())
        );
    }

    #[test]
    fn parse_notion_uri_kind_page() {
        let spec = parse("notion://page/be633bf1-dfa0-436d-b259-571129a590e5").unwrap();
        assert_eq!(
            spec.kind,
            NotionKind::Page("be633bf1-dfa0-436d-b259-571129a590e5".to_string())
        );
    }

    #[test]
    fn parse_notion_uri_invalid_kind_fails_loud() {
        let err = parse("notion://commits").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_empty_host_fails_loud() {
        let err = parse("notion:///").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_database_missing_id_fails_loud() {
        let err = parse("notion://database").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing id"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_database_extra_segment_fails_loud() {
        let err = parse("notion://database/abc/extra").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unexpected extra path segment"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_notion_uri_non_search_ignores_query_and_object() {
        let spec = parse("notion://database/abc?query=foo&object=page").unwrap();
        assert_eq!(spec.kind, NotionKind::Database("abc".to_string()));
        assert_eq!(spec.query, None);
        assert_eq!(spec.object, None);
    }

    #[test]
    fn parse_notion_uri_limit_zero_fails_loud() {
        let err = parse("notion://search?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("positive"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_notion_uri_limit_non_numeric_fails_loud() {
        let err = parse("notion://search?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("invalid"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_notion_uri_limit_above_max_ok() {
        // The Limit cap is unbounded at the WireFilters level; the adapter
        // clamps per-request page_size to MAX_LIMIT during pagination.
        let spec = parse("notion://search?limit=500").unwrap();
        assert_eq!(spec.limit, 500);
    }

    #[test]
    fn parse_notion_uri_limit_100_ok() {
        let spec = parse("notion://search?limit=100").unwrap();
        assert_eq!(spec.limit, 100);
    }

    #[test]
    fn parse_notion_uri_unknown_addressing_key_ignored() {
        let spec = parse("notion://search?utm_source=foo").unwrap();
        assert_eq!(spec.kind, NotionKind::Search);
        assert_eq!(spec.query, None);
    }

    // ---- filter_caps + WireFilters integration (Phase 2 unified filter IF) ----

    #[test]
    fn filter_caps_declares_limit_and_text_query() {
        assert_eq!(
            NotionAdapter.filter_caps(),
            &[FilterCap::Limit { max: None }, FilterCap::TextQuery]
        );
    }

    #[test]
    fn wire_filters_undeclared_filter_key_errors() {
        let wire = WireUri::parse("notion://search?since=2026-01-01").expect("valid WireUri");
        let err = WireFilters::parse(&wire, NotionAdapter.filter_caps()).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("since") && msg.contains("not supported"),
            "unexpected error: {msg}"
        );
    }

    // ---- search_request_body ----

    #[test]
    fn search_request_body_minimal() {
        let spec = parse("notion://search").unwrap();
        let body = search_request_body(&spec);
        assert!(body.get("query").is_none());
        assert!(body.get("filter").is_none());
        assert_eq!(body["page_size"].as_u64().unwrap(), DEFAULT_LIMIT as u64);
    }

    #[test]
    fn search_request_body_with_query_and_object() {
        let spec = parse("notion://search?query=Bug%20bash&object=page&limit=5").unwrap();
        let body = search_request_body(&spec);
        assert_eq!(body["query"].as_str().unwrap(), "Bug bash");
        assert_eq!(body["filter"]["property"].as_str().unwrap(), "object");
        assert_eq!(body["filter"]["value"].as_str().unwrap(), "page");
        assert_eq!(body["page_size"].as_u64().unwrap(), 5);
    }

    // ---- resolve_single_data_source ----

    fn database_fixture_one_source() -> serde_json::Value {
        serde_json::json!({
            "object": "database",
            "id": "d9824bdc-8445-4327-be8b-5b47500af6ce",
            "data_sources": [
                { "id": "1a44be12-0953-4631-b498-9e5817518db8", "name": "My Task Tracker" }
            ]
        })
    }

    #[test]
    fn resolve_single_data_source_exact_one_ok() {
        let id = resolve_single_data_source(&database_fixture_one_source()).unwrap();
        assert_eq!(id, "1a44be12-0953-4631-b498-9e5817518db8");
    }

    #[test]
    fn resolve_single_data_source_zero_fails_loud() {
        let raw = serde_json::json!({ "object": "database", "data_sources": [] });
        let err = resolve_single_data_source(&raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no data sources"), "unexpected error: {msg}");
    }

    #[test]
    fn resolve_single_data_source_multiple_fails_loud_and_lists_choices() {
        let raw = serde_json::json!({
            "object": "database",
            "data_sources": [
                { "id": "src-a", "name": "A" },
                { "id": "src-b", "name": "B" }
            ]
        });
        let err = resolve_single_data_source(&raw).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("2 data sources"), "unexpected error: {msg}");
        assert!(
            msg.contains("notion://data-source/src-a"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("notion://data-source/src-b"),
            "unexpected error: {msg}"
        );
    }

    // ---- normalize_search / normalize_data_source_query (shared page item shape) ----

    fn search_page_fixture() -> serde_json::Value {
        // Verbatim shape from the official Notion API `/search` and
        // `/data_sources/{id}/query` response (module docs "URI grammar").
        serde_json::json!({
            "object": "list",
            "results": [{
                "object": "page",
                "id": "be633bf1-dfa0-436d-b259-571129a590e5",
                "created_time": "2024-01-01T00:00:00.000Z",
                "last_edited_time": "2024-01-05T00:00:00.000Z",
                "cover": serde_json::Value::Null,
                "icon": serde_json::Value::Null,
                "parent": {
                    "type": "data_source_id",
                    "data_source_id": "1a44be12-0953-4631-b498-9e5817518db8",
                    "database_id": "d9824bdc-8445-4327-be8b-5b47500af6ce"
                },
                "in_trash": false,
                "properties": {
                    "Name": {
                        "id": "title",
                        "type": "title",
                        "title": [{
                            "type": "text",
                            "text": { "content": "Bug bash", "link": serde_json::Value::Null },
                            "plain_text": "Bug bash",
                            "href": serde_json::Value::Null
                        }]
                    }
                },
                "url": "https://www.notion.so/Bug-bash-be633bf1dfa0436db259571129a590e5",
                "public_url": serde_json::Value::Null
            }],
            "next_cursor": serde_json::Value::Null,
            "has_more": false
        })
    }

    #[test]
    fn normalize_search_field_mapping_and_title_extraction() {
        let spec = parse("notion://search?query=Bug%20bash").unwrap();
        let v = normalize_search(&spec, &search_page_fixture()).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "search");
        assert_eq!(v["query"].as_str().unwrap(), "Bug bash");
        assert!(!v["has_more"].as_bool().unwrap());
        let item = &v["items"][0];
        assert_eq!(
            item["id"].as_str().unwrap(),
            "be633bf1-dfa0-436d-b259-571129a590e5"
        );
        assert_eq!(item["object"].as_str().unwrap(), "page");
        assert_eq!(item["title"].as_str().unwrap(), "Bug bash");
        assert_eq!(
            item["url"].as_str().unwrap(),
            "https://www.notion.so/Bug-bash-be633bf1dfa0436db259571129a590e5"
        );
        assert_eq!(
            item["last_edited_time"].as_str().unwrap(),
            "2024-01-05T00:00:00.000Z"
        );
        assert!(!item["in_trash"].as_bool().unwrap());
    }

    #[test]
    fn normalize_search_query_null_when_absent() {
        let spec = parse("notion://search").unwrap();
        let v = normalize_search(&spec, &search_page_fixture()).unwrap();
        assert!(v["query"].is_null());
    }

    #[test]
    fn normalize_title_extraction_property_name_agnostic() {
        // Property key is "Task Title" (not "Name") — extraction scans by
        // `type == "title"`, not by property name.
        let mut fixture = search_page_fixture();
        let props = fixture["results"][0]["properties"].take();
        let title_prop = props["Name"].clone();
        fixture["results"][0]["properties"] = serde_json::json!({ "Task Title": title_prop });
        let spec = parse("notion://search").unwrap();
        let v = normalize_search(&spec, &fixture).unwrap();
        assert_eq!(v["items"][0]["title"].as_str().unwrap(), "Bug bash");
    }

    #[test]
    fn normalize_title_missing_property_is_null() {
        let mut fixture = search_page_fixture();
        fixture["results"][0]["properties"] = serde_json::json!({
            "Status": { "id": "abc", "type": "select", "select": serde_json::Value::Null }
        });
        let spec = parse("notion://search").unwrap();
        let v = normalize_search(&spec, &fixture).unwrap();
        assert!(v["items"][0]["title"].is_null());
    }

    #[test]
    fn normalize_search_non_object_response_fails_loud() {
        let raw = serde_json::json!([1, 2, 3]);
        let spec = parse("notion://search").unwrap();
        let err = normalize_search(&spec, &raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected an object with a 'results' array"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn normalize_data_source_query_field_mapping() {
        let v = normalize_data_source_query(
            "1a44be12-0953-4631-b498-9e5817518db8",
            &search_page_fixture(),
        )
        .unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "data_source_query");
        assert_eq!(
            v["data_source_id"].as_str().unwrap(),
            "1a44be12-0953-4631-b498-9e5817518db8"
        );
        assert_eq!(v["items"][0]["title"].as_str().unwrap(), "Bug bash");
    }

    #[test]
    fn normalize_empty_results() {
        let raw = serde_json::json!({ "results": [], "has_more": false });
        let spec = parse("notion://search").unwrap();
        let v = normalize_search(&spec, &raw).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
    }

    // ---- normalize_page (blocks) ----

    fn blocks_fixture() -> serde_json::Value {
        // Verbatim shape from the official Notion API
        // `/blocks/{id}/children` response (module docs "URI grammar").
        serde_json::json!({
            "object": "list",
            "type": "block",
            "results": [{
                "object": "block",
                "id": "c02fc1d3-db8b-45c5-a222-27595b15aea7",
                "has_children": false,
                "in_trash": false,
                "type": "paragraph",
                "paragraph": {
                    "rich_text": [{
                        "type": "text",
                        "text": { "content": "Sample paragraph content", "link": serde_json::Value::Null },
                        "plain_text": "Sample paragraph content",
                        "href": serde_json::Value::Null
                    }],
                    "color": "default"
                }
            }],
            "next_cursor": serde_json::Value::Null,
            "has_more": false
        })
    }

    #[test]
    fn normalize_page_paragraph_text() {
        let v = normalize_page("be633bf1-dfa0-436d-b259-571129a590e5", &blocks_fixture()).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "page");
        assert_eq!(
            v["page_id"].as_str().unwrap(),
            "be633bf1-dfa0-436d-b259-571129a590e5"
        );
        assert!(!v["has_more"].as_bool().unwrap());
        let block = &v["blocks"][0];
        assert_eq!(block["type"].as_str().unwrap(), "paragraph");
        assert_eq!(block["text"].as_str().unwrap(), "Sample paragraph content");
    }

    #[test]
    fn normalize_page_block_without_rich_text_is_null() {
        let raw = serde_json::json!({
            "results": [{ "object": "block", "id": "x", "type": "divider", "divider": {} }],
            "has_more": false
        });
        let v = normalize_page("page-1", &raw).unwrap();
        let block = &v["blocks"][0];
        assert_eq!(block["type"].as_str().unwrap(), "divider");
        assert!(block["text"].is_null());
    }

    #[test]
    fn normalize_page_text_truncated() {
        let long_text = "a".repeat(600);
        let raw = serde_json::json!({
            "results": [{
                "object": "block",
                "type": "paragraph",
                "paragraph": {
                    "rich_text": [{ "plain_text": long_text }]
                }
            }],
            "has_more": false
        });
        let v = normalize_page("page-1", &raw).unwrap();
        let text = v["blocks"][0]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), TEXT_MAX_CHARS + 1, "500 + ellipsis");
        assert!(text.ends_with('…'));
    }

    #[test]
    fn normalize_page_empty_results() {
        let raw = serde_json::json!({ "results": [], "has_more": false });
        let v = normalize_page("page-1", &raw).unwrap();
        assert_eq!(v["blocks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn normalize_page_non_object_response_fails_loud() {
        let raw = serde_json::json!({ "message": "Not Found" });
        let err = normalize_page("page-1", &raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected an object with a 'results' array"),
            "unexpected error: {msg}"
        );
    }

    // ---- internal pagination helpers ----
    //
    // The `next_cursor` loop is driven internally by `Adapter::fetch` over
    // `HttpClient` (a concrete struct not behind a mockable trait), and
    // this workspace's convention (established in `adapter.rs` crate docs)
    // is that Adapter tests are offline unit tests over inline fixtures.
    // The request body / URL builders and the cursor-token extractor are
    // exercised as pure functions below.

    #[test]
    fn notion_next_cursor_token_signals_end_on_has_more_false() {
        let raw = serde_json::json!({ "has_more": false, "next_cursor": "abc" });
        assert_eq!(next_cursor_token(&raw), None);
    }

    #[test]
    fn notion_next_cursor_token_signals_end_on_null_next_cursor() {
        let raw = serde_json::json!({ "has_more": true, "next_cursor": null });
        assert_eq!(next_cursor_token(&raw), None);
    }

    #[test]
    fn notion_next_cursor_token_extracts_when_has_more_and_token_set() {
        let raw = serde_json::json!({ "has_more": true, "next_cursor": "abc123" });
        assert_eq!(next_cursor_token(&raw).as_deref(), Some("abc123"));
    }

    #[test]
    fn notion_search_request_body_with_start_cursor() {
        let spec = parse("notion://search?query=Bug%20bash").unwrap();
        let body = search_request_body_with_cursor(&spec, Some("abc123"));
        assert_eq!(body["query"].as_str().unwrap(), "Bug bash");
        assert_eq!(body["start_cursor"].as_str().unwrap(), "abc123");
    }

    #[test]
    fn notion_search_request_body_without_cursor_matches_fast_path() {
        let spec = parse("notion://search?query=Bug%20bash").unwrap();
        let with_none = search_request_body_with_cursor(&spec, None);
        let fast_path = search_request_body(&spec);
        assert_eq!(with_none, fast_path);
        assert!(with_none.get("start_cursor").is_none());
    }

    #[test]
    fn notion_data_source_body_with_start_cursor() {
        let body = data_source_query_body(Some("abc123"));
        assert_eq!(body["page_size"].as_u64().unwrap(), MAX_LIMIT as u64);
        assert_eq!(body["start_cursor"].as_str().unwrap(), "abc123");
    }

    #[test]
    fn notion_data_source_body_without_cursor_omits_start_cursor() {
        let body = data_source_query_body(None);
        assert_eq!(body["page_size"].as_u64().unwrap(), MAX_LIMIT as u64);
        assert!(body.get("start_cursor").is_none());
    }

    #[test]
    fn notion_page_url_with_start_cursor() {
        let url = page_children_url("page-1", Some("abc123"));
        assert_eq!(
            url,
            format!("{API_BASE}/blocks/page-1/children?page_size={MAX_LIMIT}&start_cursor=abc123")
        );
    }

    #[test]
    fn notion_page_url_without_cursor_omits_start_cursor() {
        let url = page_children_url("page-1", None);
        assert_eq!(
            url,
            format!("{API_BASE}/blocks/page-1/children?page_size={MAX_LIMIT}")
        );
        assert!(!url.contains("start_cursor"));
    }
}
