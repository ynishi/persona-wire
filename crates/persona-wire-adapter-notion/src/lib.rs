//! persona-wire Adapter for Notion (scheme `notion://`).
//!
//! ## Architecture
//!
//! `NotionAdapter` is a stateless [`Adapter`] impl split into three
//! independent functions:
//!
//! - [`parse_notion_uri`] — `WireUri` → `NotionUriSpec` (endpoint kind +
//!   optional search query/object filter + item limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
//!   Notion-specific knowledge in the transport layer).
//! - [`normalize_search`] / [`normalize_data_source_query`] /
//!   [`normalize_page`] — raw Notion API response → the Wire JSON shape
//!   below, one per endpoint kind.
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
//!   A non-numeric, zero, or out-of-range (> [`MAX_LIMIT`], Notion's own
//!   `page_size` ceiling) value fails loud. It is always sent explicitly to
//!   the Notion API (the default behavior for an absent `page_size` is
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
//! ## Pagination (Layer 3b-notion of GH #1)
//!
//! `NotionAdapter` implements [`Pageable`]: when a caller requests
//! `?limit=N` with `N` greater than [`MAX_LIMIT`] (100), the wire-layer
//! driver (`persona_wire_core::application::use_cases`) threads a
//! [`Cursor::NextToken`] extracted from the response body's `next_cursor`
//! field (a nullable string, alongside a `has_more` boolean; either `false`
//! `has_more` or a `null`/absent `next_cursor` signals end-of-data) across
//! repeated requests instead of the single capped fetch `Adapter::fetch`
//! performs. All four kinds (`search` / `database` / `data-source` /
//! `page`) support pagination — `kind=database` re-resolves its data
//! source on every page (an extra `GET /databases/{id}` round trip per
//! page loop iteration), since that resolution is a lookup, not a
//! paginated operation, and this keeps the adapter stateless.
//! `parse_notion_uri`'s `limit` parsing still rejects `limit > MAX_LIMIT`
//! at parse time (module docs "URI grammar" above) — relaxing that guard
//! is a follow-up decision for after all three Layer 3b adapters
//! (todoist / notion / slack) have `Pageable` impls, not part of this
//! layer. `limit <= MAX_LIMIT` is unaffected — it stays on the existing
//! single-request fast path.
//!
//! `Pageable::wrap_items` preserves each kind's canonical output shape
//! (see "Output shape" above) with `has_more: false` on the pagination
//! path — a caller who requested `limit > MAX_LIMIT` via the Pageable
//! path got exactly what they asked for, and the fast-path `has_more`
//! semantic ("upstream still has more") does not naturally extend to a
//! completed pagination loop. For `kind=database`, the wrapped shape's
//! `data_source_id` field carries the URI's `database_id` (not the
//! resolved data source id): `wrap_items` is sync (no I/O, per the
//! `Pageable` trait contract) and only has the parsed URI to work with,
//! so it cannot repeat the per-page resolution `fetch_page` performs.

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

    /// Fetch `spec.kind` items and normalize them. See the module docs for
    /// URI grammar, auth resolution, and output shape.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_notion_uri(uri)?;
        let client = notion_http_client()?;

        match &spec.kind {
            NotionKind::Search => {
                let body = search_request_body(&spec);
                let raw = client
                    .post_json(&format!("{API_BASE}/search"), &body)
                    .await?;
                normalize_search(&spec, &raw)
            }
            NotionKind::Database(database_id) => {
                let raw_db = client
                    .get_json(&format!("{API_BASE}/databases/{database_id}"))
                    .await?;
                let data_source_id = resolve_single_data_source(&raw_db)?;
                let raw = client
                    .post_json(
                        &format!("{API_BASE}/data_sources/{data_source_id}/query"),
                        &serde_json::json!({ "page_size": spec.limit }),
                    )
                    .await?;
                normalize_data_source_query(&data_source_id, &raw)
            }
            NotionKind::DataSource(data_source_id) => {
                let raw = client
                    .post_json(
                        &format!("{API_BASE}/data_sources/{data_source_id}/query"),
                        &serde_json::json!({ "page_size": spec.limit }),
                    )
                    .await?;
                normalize_data_source_query(data_source_id, &raw)
            }
            NotionKind::Page(page_id) => {
                let url = format!(
                    "{API_BASE}/blocks/{page_id}/children?page_size={}",
                    spec.limit
                );
                let raw = client.get_json(&url).await?;
                normalize_page(page_id, &raw)
            }
        }
    }

    /// Opts into the wire-layer pagination driver (Layer 3b-notion of GH
    /// #1). See the module docs "Pagination" section.
    fn as_pageable(&self) -> Option<&dyn Pageable> {
        Some(self)
    }
}

/// Builds a fresh, Notion-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth"). Shared by `Adapter::fetch` and
/// `Pageable::fetch_page` (all four kinds) so every path stays in sync on
/// headers/timeout/version.
fn notion_http_client() -> WireResult<HttpClient> {
    // Auth is resolved per-fetch (not at boot); see module docs "Auth".
    // Notion has no unauthenticated access mode, unlike the github
    // adapter, so a missing token fails loud here.
    let token = Credentials::default_chain().get("notion")?.ok_or_else(|| {
        WireError::Storage(
            "notion adapter: no token found for 'notion' (set PERSONA_WIRE_TOKEN_NOTION / NOTION_TOKEN, or run 'persona-wire token set notion')"
                .to_string(),
        )
    })?;
    Ok(HttpClient::new("notion adapter")
        .with_timeout(FETCH_TIMEOUT)
        .with_header("Notion-Version", NOTION_VERSION)
        .with_bearer(token))
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
/// into a [`NotionUriSpec`]. See the module-level "URI grammar" section for
/// the exact rules and failure conditions.
fn parse_notion_uri(uri: &WireUri) -> WireResult<NotionUriSpec> {
    let limit = parse_limit(uri.query_get("limit"))?;

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

    // `query` / `object` are unknown query keys for every kind other than
    // Search and are not even read (module docs "URI grammar").
    let (query, object) = if kind == NotionKind::Search {
        let query = uri.query_get("query").map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        });
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

/// Parse and validate the `?limit=` query value (see module docs "URI
/// grammar" for the exact rules).
fn parse_limit(raw: Option<&str>) -> WireResult<usize> {
    match raw {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "notion adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "notion adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            if n > MAX_LIMIT {
                return Err(WireError::Storage(format!(
                    "notion adapter: invalid limit '{raw}' (must be <= {MAX_LIMIT})"
                )));
            }
            Ok(n)
        }
        None => Ok(DEFAULT_LIMIT),
    }
}

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
    body.insert("page_size".to_string(), serde_json::json!(spec.limit));
    serde_json::Value::Object(body)
}

/// Builds the `POST /search` request body for `spec` with an optional
/// `start_cursor` inserted (the wire-layer pagination path,
/// `Pageable::fetch_page`). `cursor = None` is byte-identical to
/// [`search_request_body`] (the non-paginated fast path never has a cursor
/// to thread) — kept as a separate function rather than adding a `cursor`
/// parameter to `search_request_body` itself, so its existing call site in
/// `Adapter::fetch` and its tests stay untouched.
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

/// Extracts the opaque cursor token from a `Pageable::fetch_page` `cursor`
/// argument. `None` (first page) yields `Ok(None)`; only
/// `Cursor::NextToken` is a valid non-`None` variant — Notion's cursor is
/// always an opaque string (matches the `NextToken` variant's canonical
/// semantics from the `Cursor` enum docs). Any other variant fails loud
/// (module docs "Cursor variant discipline").
fn notion_cursor_token(cursor: &Option<Cursor>) -> WireResult<Option<&str>> {
    match cursor {
        None => Ok(None),
        Some(Cursor::NextToken(token)) => Ok(Some(token.as_str())),
        Some(other) => Err(WireError::Storage(format!(
            "notion adapter: unsupported cursor variant for pagination: {other:?}"
        ))),
    }
}

/// Extracts the raw `results` JSON array from a Notion API response for
/// the wire-layer pagination path, failing loud (naming `context`) when
/// the response isn't shaped as expected. A pagination-path-only sibling
/// of the inline extraction each `normalize_*` function performs (kept
/// separate rather than factored into a single shared helper, so the
/// existing `normalize_*` functions and their tests stay untouched).
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

/// Extracts the pagination cursor from a Notion API response body per
/// module docs "Response `next_cursor` extraction": `has_more: false`, or
/// a `null`/absent `next_cursor`, both signal end-of-data (`None`).
fn next_cursor_from_response(raw: &serde_json::Value) -> Option<Cursor> {
    let has_more = raw
        .get("has_more")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !has_more {
        return None;
    }
    raw.get("next_cursor")
        .and_then(|v| v.as_str())
        .map(|s| Cursor::NextToken(s.to_string()))
}

/// Fetches one page of a `/data_sources/{id}/query` response using an
/// already-built `client` (shared by [`fetch_page_for_data_source`] and
/// [`fetch_page_for_database`], the latter needing the same client for
/// its preceding `GET /databases/{id}` resolve call).
async fn fetch_data_source_page(
    client: &HttpClient,
    data_source_id: &str,
    cursor: Option<&str>,
) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
    let body = data_source_query_body(cursor);
    let raw = client
        .post_json(
            &format!("{API_BASE}/data_sources/{data_source_id}/query"),
            &body,
        )
        .await?;
    let results = extract_results(&raw, &format!("data source '{data_source_id}' query"))?;
    let items = results.iter().map(normalize_page_item).collect();
    Ok((items, next_cursor_from_response(&raw)))
}

/// `Pageable::fetch_page` for `NotionKind::Search`.
async fn fetch_page_for_search(
    spec: &NotionUriSpec,
    cursor: Option<&str>,
) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
    let client = notion_http_client()?;
    let body = search_request_body_with_cursor(spec, cursor);
    let raw = client
        .post_json(&format!("{API_BASE}/search"), &body)
        .await?;
    let results = extract_results(&raw, "search")?;
    let items = results.iter().map(normalize_page_item).collect();
    Ok((items, next_cursor_from_response(&raw)))
}

/// `Pageable::fetch_page` for `NotionKind::DataSource`.
async fn fetch_page_for_data_source(
    data_source_id: &str,
    cursor: Option<&str>,
) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
    let client = notion_http_client()?;
    fetch_data_source_page(&client, data_source_id, cursor).await
}

/// `Pageable::fetch_page` for `NotionKind::Database`. Re-resolves the
/// database's single data source on every page (module docs
/// "Pagination"), reusing the same `client` for both the resolve call and
/// the subsequent data-source-query call.
async fn fetch_page_for_database(
    database_id: &str,
    cursor: Option<&str>,
) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
    let client = notion_http_client()?;
    let raw_db = client
        .get_json(&format!("{API_BASE}/databases/{database_id}"))
        .await?;
    let data_source_id = resolve_single_data_source(&raw_db)?;
    fetch_data_source_page(&client, &data_source_id, cursor).await
}

/// `Pageable::fetch_page` for `NotionKind::Page`.
async fn fetch_page_for_page(
    page_id: &str,
    cursor: Option<&str>,
) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
    let client = notion_http_client()?;
    let url = page_children_url(page_id, cursor);
    let raw = client.get_json(&url).await?;
    let results = extract_results(&raw, &format!("page '{page_id}' blocks"))?;
    let items = results.iter().map(normalize_block).collect();
    Ok((items, next_cursor_from_response(&raw)))
}

#[async_trait]
impl Pageable for NotionAdapter {
    /// Notion API's own `page_size` ceiling (matches [`MAX_LIMIT`]; see
    /// module docs "URI grammar").
    fn max_page_size(&self) -> usize {
        MAX_LIMIT
    }

    /// Fetches one page (dispatching by kind; see the per-function docs
    /// above) and normalizes it the same way `Adapter::fetch`'s per-kind
    /// branch does, without truncating to `limit` — the wire-layer driver
    /// truncates across accumulated pages. Rejects any cursor variant
    /// other than `None` / `Some(Cursor::NextToken(_))` (module docs
    /// "Cursor variant discipline").
    async fn fetch_page(
        &self,
        uri: &WireUri,
        cursor: Option<Cursor>,
    ) -> WireResult<(Vec<serde_json::Value>, Option<Cursor>)> {
        let spec = parse_notion_uri(uri)?;
        let token = notion_cursor_token(&cursor)?;

        match &spec.kind {
            NotionKind::Search => fetch_page_for_search(&spec, token).await,
            NotionKind::Database(database_id) => fetch_page_for_database(database_id, token).await,
            NotionKind::DataSource(data_source_id) => {
                fetch_page_for_data_source(data_source_id, token).await
            }
            NotionKind::Page(page_id) => fetch_page_for_page(page_id, token).await,
        }
    }

    /// Preserves each kind's canonical output shape (module docs "Output
    /// shape") across the pagination path, with `has_more: false` (module
    /// docs "Pagination" — see that section for the `kind=database`
    /// `data_source_id` caveat).
    fn wrap_items(
        &self,
        items: Vec<serde_json::Value>,
        uri: &WireUri,
    ) -> WireResult<serde_json::Value> {
        let spec = parse_notion_uri(uri)?;
        match &spec.kind {
            NotionKind::Search => Ok(serde_json::json!({
                "kind": "search",
                "query": spec.query,
                "items": items,
                "has_more": false,
            })),
            NotionKind::DataSource(data_source_id) => Ok(serde_json::json!({
                "kind": "data_source_query",
                "data_source_id": data_source_id,
                "items": items,
                "has_more": false,
            })),
            NotionKind::Database(database_id) => Ok(serde_json::json!({
                "kind": "data_source_query",
                // `wrap_items` is sync (no I/O, per the `Pageable` trait
                // contract) and only has the parsed URI to work with, so
                // it cannot repeat the per-page `GET /databases/{id}`
                // resolution `fetch_page` performs. `database_id` (the
                // caller-supplied identifier that transparently resolved
                // to the same data source for every page of this loop) is
                // used here instead of the actual data source id.
                "data_source_id": database_id,
                "items": items,
                "has_more": false,
            })),
            NotionKind::Page(page_id) => Ok(serde_json::json!({
                "kind": "page",
                "page_id": page_id,
                // The fast-path shape (`normalize_page`) names this field
                // `blocks`, not `items` — matched here verbatim rather
                // than following the generic `items` name, to honestly
                // preserve the per-kind shape (module docs "Output
                // shape").
                "blocks": items,
                "has_more": false,
            })),
        }
    }
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

/// Normalizes a `POST /search` response (`raw`, expected to be an object
/// with a `results` array) into the Wire JSON shape. See module docs
/// "Output shape".
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

/// Normalizes a `POST /data_sources/{id}/query` response (`raw`, expected
/// to be an object with a `results` array) into the Wire JSON shape. Shared
/// by `kind=database` (after resolving the single data source) and
/// `kind=data-source`. See module docs "Output shape".
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

/// Normalizes a `GET /blocks/{id}/children` response (`raw`, expected to be
/// an object with a `results` array) into the Wire JSON shape. See module
/// docs "Output shape".
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

    // ---- parse_notion_uri ----

    fn parse(uri: &str) -> WireResult<NotionUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_notion_uri(&wire)
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
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_limit_non_numeric_fails_loud() {
        let err = parse("notion://search?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_limit_101_fails_loud() {
        let err = parse("notion://search?limit=101").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_notion_uri_limit_100_ok() {
        let spec = parse("notion://search?limit=100").unwrap();
        assert_eq!(spec.limit, 100);
    }

    #[test]
    fn parse_notion_uri_unknown_query_key_ignored() {
        let spec = parse("notion://search?utm_source=foo").unwrap();
        assert_eq!(spec.kind, NotionKind::Search);
        assert_eq!(spec.query, None);
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

    // ---- Pageable (Layer 3b-notion of GH #1) ----
    //
    // Same no-live-HTTP rationale as `persona-wire-adapter-github` /
    // `persona-wire-adapter-todoist` (see those crates' test module docs):
    // `fetch_page` / `Adapter::fetch` both perform live HTTP via
    // `HttpClient` (a concrete struct, not behind a mockable trait), and
    // this workspace has no wiremock / hand-rolled mock-server pattern.
    // The cursor→token decision (`notion_cursor_token`), the request
    // body/URL builders (`search_request_body_with_cursor` /
    // `data_source_query_body` / `page_children_url`), and the
    // shape-building (`wrap_items`) are tested directly as the pure
    // functions they are.

    #[test]
    fn notion_pageable_max_page_size_is_100() {
        let adapter = NotionAdapter;
        assert_eq!(adapter.max_page_size(), MAX_LIMIT);
    }

    #[test]
    fn notion_as_pageable_returns_some() {
        let adapter = NotionAdapter;
        let pageable = adapter.as_pageable();
        assert!(pageable.is_some(), "override should return Some(self)");
        assert_eq!(pageable.unwrap().max_page_size(), MAX_LIMIT);
    }

    #[test]
    fn notion_fetch_page_rejects_other_cursor_variants() {
        for cursor in [
            Cursor::PageNumber(2),
            Cursor::LinkHeader("https://example.com".to_string()),
            Cursor::Offset(10),
        ] {
            let err = notion_cursor_token(&Some(cursor)).unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains("unsupported cursor variant"),
                "unexpected error: {msg}"
            );
        }
    }

    #[test]
    fn notion_cursor_token_none_and_next_token() {
        assert_eq!(notion_cursor_token(&None).unwrap(), None);
        assert_eq!(
            notion_cursor_token(&Some(Cursor::NextToken("abc123".to_string()))).unwrap(),
            Some("abc123")
        );
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

    #[test]
    fn notion_wrap_items_search_preserves_query_field() {
        let adapter = NotionAdapter;
        let uri = WireUri::parse("notion://search?query=Bug%20bash").unwrap();
        let items = vec![serde_json::json!({"id": "1"})];

        let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();

        assert_eq!(wrapped["kind"].as_str().unwrap(), "search");
        assert_eq!(wrapped["query"].as_str().unwrap(), "Bug bash");
        assert_eq!(wrapped["items"], serde_json::Value::Array(items));
    }

    #[test]
    fn notion_wrap_items_data_source_preserves_data_source_id() {
        let adapter = NotionAdapter;
        let uri =
            WireUri::parse("notion://data-source/1a44be12-0953-4631-b498-9e5817518db8").unwrap();
        let items = vec![serde_json::json!({"id": "1"})];

        let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();

        assert_eq!(wrapped["kind"].as_str().unwrap(), "data_source_query");
        assert_eq!(
            wrapped["data_source_id"].as_str().unwrap(),
            "1a44be12-0953-4631-b498-9e5817518db8"
        );
        assert_eq!(wrapped["items"], serde_json::Value::Array(items));
    }

    #[test]
    fn notion_wrap_items_database_uses_database_id_as_data_source_id() {
        // Documented gray-area decision (module docs "Pagination"):
        // `wrap_items` cannot repeat the per-page data-source resolution
        // `fetch_page` performs (sync, no I/O), so the URI's database_id
        // stands in for the resolved data_source_id.
        let adapter = NotionAdapter;
        let uri = WireUri::parse("notion://database/d9824bdc-8445-4327-be8b-5b47500af6ce").unwrap();
        let items = vec![serde_json::json!({"id": "1"})];

        let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();

        assert_eq!(wrapped["kind"].as_str().unwrap(), "data_source_query");
        assert_eq!(
            wrapped["data_source_id"].as_str().unwrap(),
            "d9824bdc-8445-4327-be8b-5b47500af6ce"
        );
        assert_eq!(wrapped["items"], serde_json::Value::Array(items));
    }

    #[test]
    fn notion_wrap_items_page_preserves_page_id() {
        let adapter = NotionAdapter;
        let uri = WireUri::parse("notion://page/be633bf1-dfa0-436d-b259-571129a590e5").unwrap();
        let items = vec![serde_json::json!({"type": "paragraph"})];

        let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();

        assert_eq!(wrapped["kind"].as_str().unwrap(), "page");
        assert_eq!(
            wrapped["page_id"].as_str().unwrap(),
            "be633bf1-dfa0-436d-b259-571129a590e5"
        );
        // The fast-path shape (`normalize_page`) names this field
        // `blocks`, not `items` (module docs "Output shape").
        assert_eq!(wrapped["blocks"], serde_json::Value::Array(items));
    }

    #[test]
    fn notion_wrap_items_sets_has_more_false_on_pagination_path() {
        let adapter = NotionAdapter;
        let items = vec![serde_json::json!({"id": "1"})];

        for uri_str in [
            "notion://search",
            "notion://data-source/1a44be12-0953-4631-b498-9e5817518db8",
            "notion://database/d9824bdc-8445-4327-be8b-5b47500af6ce",
            "notion://page/be633bf1-dfa0-436d-b259-571129a590e5",
        ] {
            let uri = WireUri::parse(uri_str).unwrap();
            let wrapped = adapter.wrap_items(items.clone(), &uri).unwrap();
            assert!(
                !wrapped["has_more"].as_bool().unwrap(),
                "has_more should be false for {uri_str}"
            );
        }
    }
}
