//! persona-wire Adapter for Slack (scheme `slack://`).
//!
//! ## Architecture
//!
//! `SlackAdapter` is a stateless [`Adapter`] impl split into three
//! independent functions:
//!
//! - [`parse_slack_uri`] — `WireUri` → `SlackUriSpec` (endpoint kind +
//!   kind-specific filters + item limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
//!   Slack-specific knowledge in the transport layer).
//! - Per-kind loop drivers (`drive_channels_loop` / `drive_history_loop`)
//!   plus the single-shot [`normalize_user`] — accumulate results across
//!   `response_metadata.next_cursor` pages for paginatable kinds, and
//!   assemble the Wire JSON shape below, one per endpoint kind.
//!
//! ## URI grammar
//!
//! ```text
//! slack://channels[?types=public_channel|private_channel|mpim|im][&limit=N][&exclude_archived=true|false]
//! slack://history/<channel_id>[?limit=N][&oldest=<ts>][&latest=<ts>]
//! slack://user/<user_id>
//! ```
//!
//! - `host` selects the endpoint kind (`channels` / `history` / `user`); an
//!   empty or invalid value **fails loud** — a typo here would otherwise
//!   silently return a different class of data (matching
//!   `persona-wire-adapter-todoist`'s host-selects-kind convention).
//! - For `kind=channels`, the path must be empty (or `/`); any additional
//!   path segment fails loud. `?types=` accepts a comma-separated subset of
//!   `public_channel` / `private_channel` / `mpim` / `im` (Slack's
//!   `conversations.list` `types` enum) — an unrecognized entry fails loud;
//!   omitted defaults to `public_channel`. `?exclude_archived=true|false`
//!   defaults to `true`; any other value fails loud.
//! - For `kind=history`, the path must be exactly one segment (the
//!   `channel_id`); a missing id, or any additional path segment, fails
//!   loud (the id's own format is not further validated — a malformed id
//!   surfaces as a normal Slack API error, e.g. `channel_not_found`).
//!   `?oldest=<ts>` / `?latest=<ts>` (Slack Unix-seconds-with-microseconds
//!   timestamps, e.g. `1512085950.000216`) are percent-decoded once at parse
//!   time and passed through to the Slack API verbatim — this adapter does
//!   not validate their format.
//! - For `kind=user`, the path must be exactly one segment (the `user_id`);
//!   same missing/extra-segment rule as `kind=history`.
//! - `types` / `exclude_archived` are unknown query keys for `kind=history`
//!   / `kind=user` (not even read); `oldest` / `latest` are unknown query
//!   keys for `kind=channels` / `kind=user` (not even read) (module docs
//!   "URI grammar").
//! - `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
//!   A non-numeric or zero value fails loud; there is no upper bound at
//!   parse time. [`MAX_LIMIT`] (Slack's own `conversations.list` /
//!   `conversations.history` per-request ceiling of 999) is enforced only
//!   when the adapter builds each upstream request; `?limit=N` with
//!   `N > MAX_LIMIT` triggers the internal pagination loop (see
//!   "Pagination" below).
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss` / `-github` / `-todoist` /
//!   `-notion`).
//!
//! ## Auth
//!
//! Resolved per-fetch (not at boot) via
//! `persona_wire_credentials::Credentials::default_chain().get("slack")`.
//! Slack has no unauthenticated access mode — a missing token **fails
//! loud**. Set a token via the `PERSONA_WIRE_TOKEN_SLACK` or
//! `SLACK_BOT_TOKEN` environment variable, or store one in the OS keychain
//! via `persona-wire token set slack`. The token is a Slack bot token
//! (`xoxb-...` prefix), minted under a Slack app's OAuth & Permissions page.
//! It is sent as an `Authorization: Bearer` header (per `HttpClient`) —
//! **never** as a query-string parameter, per Slack's own guidance for apps
//! created since November 2020.
//!
//! The minimal OAuth scopes this adapter needs are `channels:read`,
//! `channels:history`, and `users:read`. **A private channel must have the
//! bot explicitly invited** (`/invite @<bot>` in that channel) — a bot token
//! without that invite gets a `not_in_channel` API error on
//! `conversations.history`, which surfaces as a normal fetch failure (see
//! "Error handling" below).
//!
//! The literal `"slack"` service key is overridable per-fetch via the
//! URI's `?auth=<service_key>` query param (see `persona_wire_core::
//! infrastructure::adapter`'s "External service integration policy" for the
//! convention); absent, behavior is unchanged.
//!
//! Slack's HTTP response is always `200 OK`; success/failure is signalled by
//! the response body's `{"ok": true|false}` field (see "Error handling"
//! below) — a `429` status is the sole HTTP-level exception, carrying a
//! `Retry-After` header, which this adapter does not implement
//! client-side throttling for (a `429` surfaces as a normal fetch failure
//! via `persona_wire_transport_http::HttpClient`). Slack's tiered rate
//! limits (Tier 2, `conversations.list` / `users.info`; Tier 3,
//! `conversations.history`) apply to public Slack Marketplace apps as of the
//! 2025-05 update; an **internal, customer-built app** (a bot token used
//! only within its own workspace, never distributed) is explicitly exempted
//! from that update per Slack's own clarification
//! (<https://docs.slack.dev/ja-jp/2025-05-terms-rate-limit-update-and-faq/>),
//! so `conversations.history`'s Tier 3 ceiling (50+ requests/minute) remains
//! in effect for the internal-app usage this adapter targets.
//!
//! ### Error handling
//!
//! Every Slack Web API response is HTTP `200 OK` with a JSON body carrying
//! `{"ok": bool, ...}`. This adapter inspects `ok` after every fetch: `ok:
//! true` proceeds to normalization; `ok: false` **fails loud** with the
//! response's `error` code (and, when present, its `needed` /  `provided`
//! scope hint) folded into the error message — e.g. `not_in_channel` (bot
//! not invited to a private channel) or `missing_scope` (token lacks a
//! required OAuth scope).
//!
//! ## Output shape
//!
//! For `kind=channels`:
//!
//! ```json
//! { "kind": "channels", "items": [ ... ], "has_more": false }
//! ```
//!
//! `items` entries:
//!
//! ```json
//! {
//!   "id": "...|null", "name": "...|null",
//!   "is_private": false, "is_archived": false, "is_member": true,
//!   "num_members": 4, "topic": "...|null", "purpose": "...|null"
//! }
//! ```
//!
//! `topic` / `purpose` are each the corresponding object's `value` field
//! (Slack's own `topic` / `purpose` are `{value, creator, last_set}`
//! objects; only `value` is surfaced here). `has_more` is `true` when
//! `response_metadata.next_cursor` is present and non-empty.
//!
//! For `kind=history`:
//!
//! ```json
//! { "kind": "history", "channel_id": "...", "items": [ ... ], "has_more": false }
//! ```
//!
//! `items` entries:
//!
//! ```json
//! {
//!   "type": "...|null", "user": "...|null", "text": "...|null",
//!   "ts": "...|null", "thread_ts": "...|null",
//!   "is_thread_parent": false, "reply_count": null, "subtype": "...|null"
//! }
//! ```
//!
//! `user` is the Slack user *id* (not a resolved display name — resolving it
//! is the caller's responsibility, e.g. via `slack://user/<id>`). `ts` /
//! `thread_ts` are passed through verbatim as strings (Slack's own
//! Unix-seconds-with-microseconds form, e.g. `1512085950.000216`; parsing as
//! a float would lose the sub-second precision that makes each `ts` unique
//! within a channel). `is_thread_parent` is `true` when `thread_ts == ts`
//! (a message starting its own thread); `false` for a thread reply
//! (`thread_ts` present and `!= ts`) or a non-threaded message (`thread_ts`
//! absent). `text` is truncated to [`TEXT_MAX_CHARS`] `char`s (context size
//! guard). `has_more` is Slack's own top-level `has_more` field.
//!
//! For `kind=user`:
//!
//! ```json
//! {
//!   "kind": "user", "id": "...|null", "name": "...|null",
//!   "real_name": "...|null", "display_name": "...|null",
//!   "email": "...|null", "is_bot": false, "deleted": false
//! }
//! ```
//!
//! `display_name` is `profile.display_name`; `email` is `profile.email`
//! (`null` when the token's scopes do not include the email-visibility
//! scope, or the user has none set — Slack omits the field rather than
//! sending an explicit `null` in that case, and the missing-field path
//! yields the same `null` here).
//!
//! ## Pagination
//!
//! `Adapter::fetch` drives the pagination loop internally for
//! `kind=channels` and `kind=history`: it follows the response body's
//! `response_metadata.next_cursor` field (an opaque token; an empty string,
//! `null`, or absent field all signal end-of-data) across repeated requests
//! until it has accumulated `?limit=N` items or the upstream signals
//! end-of-data. The cursor form is a private implementation detail — the
//! wire layer only sees the final assembled per-kind shape with a truthful
//! `has_more` field.
//!
//! `kind=user` is a single-object fetch (`users.info`), not paginated;
//! `?limit=N` is silently ignored for that kind.
//!
//! Every upstream request is sent with `limit = min(spec.limit, MAX_LIMIT)`
//! (Slack's own per-request ceiling of 999), so the loop runs once for
//! `?limit <= MAX_LIMIT` and continues page-by-page for larger requests.

#![warn(missing_docs)]

use async_trait::async_trait;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use persona_wire_credentials::Credentials;
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `items` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Maximum allowed `?limit=` value (Slack's own `conversations.list` /
/// `conversations.history` page-size ceiling).
pub const MAX_LIMIT: usize = 999;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-adapter-notion::FETCH_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Max message `text` length in `char`s before truncation (context size
/// guard).
pub const TEXT_MAX_CHARS: usize = 500;

/// Slack Web API base URL.
pub const API_BASE: &str = "https://slack.com/api";

/// Whitelist for `?types=` entries on `kind=channels` (Slack's
/// `conversations.list` `types` enum).
const CHANNEL_TYPES_WHITELIST: &[&str] = &["public_channel", "private_channel", "mpim", "im"];

/// persona-wire Adapter for Slack (`slack://` scheme).
pub struct SlackAdapter;

#[async_trait]
impl Adapter for SlackAdapter {
    fn scheme(&self) -> &'static str {
        "slack"
    }

    /// Fetch `spec.kind` items, driving the `next_cursor` pagination loop
    /// internally for paginatable kinds (`channels` / `history`). See the
    /// module docs for URI grammar, auth resolution, error handling, and
    /// output shape (including `has_more` semantics).
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_slack_uri(uri)?;
        let client = slack_http_client(uri)?;
        match &spec.kind {
            SlackKind::Channels => drive_channels_loop(&client, &spec).await,
            SlackKind::History(channel_id) => drive_history_loop(&client, channel_id, &spec).await,
            SlackKind::User(user_id) => {
                let raw = client.get_json(&build_user_url(user_id)).await?;
                check_ok(&raw)?;
                normalize_user(&raw)
            }
        }
    }
}

/// Builds a fresh, Slack-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth").
fn slack_http_client(uri: &WireUri) -> WireResult<HttpClient> {
    // Auth is resolved per-fetch (not at boot); see module docs "Auth".
    // Slack has no unauthenticated access mode, so a missing token fails
    // loud here.
    let service_key = resolve_service_key(uri, "slack");
    let token = Credentials::default_chain().get(service_key)?.ok_or_else(|| {
        if service_key == "slack" {
            WireError::Storage(
                "slack adapter: no token found for 'slack' (set PERSONA_WIRE_TOKEN_SLACK / SLACK_BOT_TOKEN, or run 'persona-wire token set slack')"
                    .to_string(),
            )
        } else {
            WireError::Storage(format!(
                "slack adapter: no token found for '{service_key}' (set PERSONA_WIRE_TOKEN_<KEY> uppercased, or run 'persona-wire token set {service_key}')"
            ))
        }
    })?;
    Ok(HttpClient::new("slack adapter")
        .with_timeout(FETCH_TIMEOUT)
        .with_bearer(token))
}

/// Resolves the credential service key for this fetch: the URI's
/// `?auth=<service_key>` query param when present (reference key only,
/// never a secret — see `persona_wire_core::infrastructure::adapter`'s
/// "External service integration policy"), otherwise `default_key` (this
/// adapter's literal `"slack"` service name, preserving pre-existing
/// behavior when the param is absent).
fn resolve_service_key<'a>(uri: &'a WireUri, default_key: &'static str) -> &'a str {
    uri.query_get("auth").unwrap_or(default_key)
}

/// The three Slack endpoint kinds this adapter can target, selected via the
/// URI host. `History` / `User` carry the parsed id segment (so no
/// `Option`/invariant juggling is needed downstream — the id is only
/// reachable through the variant that requires it).
#[derive(Debug, Clone, PartialEq, Eq)]
enum SlackKind {
    Channels,
    History(String),
    User(String),
}

/// Parsed `slack://` URI: endpoint kind (+ id, when applicable) + kind-scoped
/// filters + item limit.
#[derive(Debug)]
struct SlackUriSpec {
    kind: SlackKind,
    /// Comma-separated `conversations.list` `types` filter. `Some` only for
    /// `kind == Channels`.
    types: Option<String>,
    /// `Some` only for `kind == Channels`.
    exclude_archived: Option<bool>,
    /// `Some` only for `kind == History` when `?oldest=` is present.
    oldest: Option<String>,
    /// `Some` only for `kind == History` when `?latest=` is present.
    latest: Option<String>,
    limit: usize,
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`SlackUriSpec`]. See the module-level "URI grammar" section for
/// the exact rules and failure conditions.
fn parse_slack_uri(uri: &WireUri) -> WireResult<SlackUriSpec> {
    let limit = parse_limit(uri.query_get("limit"))?;

    let kind = match uri.host() {
        Some("channels") => {
            let path = uri.path();
            if !path.is_empty() && path != "/" {
                return Err(WireError::Storage(format!(
                    "slack adapter: unexpected path segment in '{}' (expected slack://channels)",
                    uri.as_raw()
                )));
            }
            SlackKind::Channels
        }
        Some("history") => SlackKind::History(parse_single_id_segment(uri, "history")?),
        Some("user") => SlackKind::User(parse_single_id_segment(uri, "user")?),
        Some(bad) if !bad.is_empty() => {
            return Err(WireError::Storage(format!(
                "slack adapter: invalid kind '{bad}' (must be one of: channels, history, user)"
            )));
        }
        _ => {
            return Err(WireError::Storage(format!(
                "slack adapter: missing kind (host) in '{}' (expected slack://channels, slack://history/<channel_id>, or slack://user/<user_id>)",
                uri.as_raw()
            )));
        }
    };

    // `types` / `exclude_archived` are unknown query keys for kinds other
    // than Channels; `oldest` / `latest` are unknown query keys for kinds
    // other than History. Neither is even read outside its owning kind
    // (module docs "URI grammar").
    let (types, exclude_archived, oldest, latest) = match &kind {
        SlackKind::Channels => {
            let types = parse_types(uri.query_get("types"))?;
            let exclude_archived = parse_exclude_archived(uri.query_get("exclude_archived"))?;
            (Some(types), Some(exclude_archived), None, None)
        }
        SlackKind::History(_) => {
            let oldest = uri.query_get("oldest").map(|s| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8_lossy()
                    .into_owned()
            });
            let latest = uri.query_get("latest").map(|s| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8_lossy()
                    .into_owned()
            });
            (None, None, oldest, latest)
        }
        SlackKind::User(_) => (None, None, None, None),
    };

    Ok(SlackUriSpec {
        kind,
        types,
        exclude_archived,
        oldest,
        latest,
        limit,
    })
}

/// Parses the single required path segment (the id) for `kind_label`
/// (`"history"` / `"user"`). A missing or extra segment fails loud. The id's
/// own format is not further validated (module docs "URI grammar").
fn parse_single_id_segment(uri: &WireUri, kind_label: &str) -> WireResult<String> {
    let segments: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [id] => Ok(id.to_string()),
        [] => Err(WireError::Storage(format!(
            "slack adapter: missing id in '{}' (expected slack://{kind_label}/<id>)",
            uri.as_raw()
        ))),
        _ => Err(WireError::Storage(format!(
            "slack adapter: unexpected extra path segment(s) in '{}' (expected slack://{kind_label}/<id>)",
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
                    "slack adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "slack adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            Ok(n)
        }
        None => Ok(DEFAULT_LIMIT),
    }
}

/// Parse and validate the `?types=` query value for `kind=channels` (see
/// module docs "URI grammar"). Absent defaults to `"public_channel"`; any
/// comma-separated entry outside [`CHANNEL_TYPES_WHITELIST`] fails loud.
fn parse_types(raw: Option<&str>) -> WireResult<String> {
    match raw {
        None => Ok("public_channel".to_string()),
        Some(raw) => {
            for entry in raw.split(',') {
                if !CHANNEL_TYPES_WHITELIST.contains(&entry) {
                    return Err(WireError::Storage(format!(
                        "slack adapter: invalid types entry '{entry}' in '{raw}' (must be one of: public_channel, private_channel, mpim, im)"
                    )));
                }
            }
            Ok(raw.to_string())
        }
    }
}

/// Parse and validate the `?exclude_archived=` query value for
/// `kind=channels` (see module docs "URI grammar"). Absent defaults to
/// `true`; any value other than `"true"` / `"false"` fails loud.
fn parse_exclude_archived(raw: Option<&str>) -> WireResult<bool> {
    match raw {
        None => Ok(true),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(bad) => Err(WireError::Storage(format!(
            "slack adapter: invalid exclude_archived '{bad}' (must be 'true' or 'false')"
        ))),
    }
}

/// Percent-encodes `s` for safe embedding as a single query-string value.
/// Over-encoding (e.g. `.` → `%2E`) is harmless — Slack decodes it back —
/// and this crate has no `url` dependency to encode more precisely.
fn encode_query(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

/// Builds the `GET /conversations.list` request URL for `spec`
/// (`spec.kind == Channels`). `types` is not percent-encoded — it is
/// restricted to [`CHANNEL_TYPES_WHITELIST`] entries (comma-separated),
/// which are all query-safe as-is. The per-request `limit` is
/// `min(spec.limit, MAX_LIMIT)`; `spec.limit > MAX_LIMIT` triggers the
/// internal pagination loop.
fn build_channels_url(spec: &SlackUriSpec) -> String {
    let types = spec.types.as_deref().unwrap_or("public_channel");
    let exclude_archived = spec.exclude_archived.unwrap_or(true);
    let per_request_limit = spec.limit.min(MAX_LIMIT);
    format!(
        "{API_BASE}/conversations.list?types={types}&limit={per_request_limit}&exclude_archived={exclude_archived}"
    )
}

/// Builds the `GET /conversations.history` request URL for `channel_id` +
/// `spec` (`spec.kind == History(channel_id)`). The per-request `limit` is
/// `min(spec.limit, MAX_LIMIT)`; `spec.limit > MAX_LIMIT` triggers the
/// internal pagination loop.
fn build_history_url(channel_id: &str, spec: &SlackUriSpec) -> String {
    let per_request_limit = spec.limit.min(MAX_LIMIT);
    let mut url = format!(
        "{API_BASE}/conversations.history?channel={}&limit={per_request_limit}",
        encode_query(channel_id),
    );
    if let Some(oldest) = &spec.oldest {
        url.push_str(&format!("&oldest={}", encode_query(oldest)));
    }
    if let Some(latest) = &spec.latest {
        url.push_str(&format!("&latest={}", encode_query(latest)));
    }
    url
}

/// Builds the `GET /conversations.list` request URL for the wire-layer
/// pagination path (internal loop in [`Adapter::fetch`]), with an optional `?cursor=`
/// query param appended. `cursor = None` is byte-identical to
/// [`build_channels_url`] (kept as a separate function rather than adding a
/// `cursor` parameter to `build_channels_url` itself, so its existing call
/// site in `Adapter::fetch` and its tests stay untouched). Slack's cursor is
/// documented as already URL-safe, but it is percent-encoded here anyway
/// (via the existing [`encode_query`]) to stay defensive — over-encoding is
/// harmless (module docs on `encode_query`).
fn build_channels_url_with_cursor(spec: &SlackUriSpec, cursor: Option<&str>) -> String {
    let mut url = build_channels_url(spec);
    if let Some(token) = cursor {
        url.push_str(&format!("&cursor={}", encode_query(token)));
    }
    url
}

/// Builds the `GET /conversations.history` request URL for the wire-layer
/// pagination path (internal loop in [`Adapter::fetch`]), with an optional `?cursor=`
/// query param appended. `cursor = None` is byte-identical to
/// [`build_history_url`] (same rationale as
/// [`build_channels_url_with_cursor`]).
fn build_history_url_with_cursor(
    channel_id: &str,
    spec: &SlackUriSpec,
    cursor: Option<&str>,
) -> String {
    let mut url = build_history_url(channel_id, spec);
    if let Some(token) = cursor {
        url.push_str(&format!("&cursor={}", encode_query(token)));
    }
    url
}

/// Builds the `GET /users.info` request URL for `user_id`
/// (`spec.kind == User(user_id)`).
fn build_user_url(user_id: &str) -> String {
    format!("{API_BASE}/users.info?user={}", encode_query(user_id))
}

/// Extracts the pagination cursor token from a Slack API response body's
/// `response_metadata.next_cursor` field. Slack often sends an empty string
/// (rather than omitting the field or sending `null`) when there are no
/// more pages — treated as `None` here (module docs "Empty-string
/// next_cursor").
fn slack_next_cursor_token(raw: &serde_json::Value) -> Option<String> {
    raw.get("response_metadata")
        .and_then(|m| m.get("next_cursor"))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Drives the `next_cursor` loop for `GET /conversations.list`.
async fn drive_channels_loop(
    client: &HttpClient,
    spec: &SlackUriSpec,
) -> WireResult<serde_json::Value> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let has_more = loop {
        let raw = client
            .get_json(&build_channels_url_with_cursor(spec, cursor.as_deref()))
            .await?;
        check_ok(&raw)?;
        let channels = raw.get("channels").and_then(|v| v.as_array()).ok_or_else(|| {
            WireError::Storage(
                "slack adapter: unexpected response shape for channels: expected an object with a 'channels' array"
                    .to_string(),
            )
        })?;
        items.extend(channels.iter().map(normalize_channel_item));
        let next = slack_next_cursor_token(&raw);
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
        "kind": "channels",
        "items": items,
        "has_more": has_more,
    }))
}

/// Drives the `next_cursor` loop for `GET /conversations.history`.
async fn drive_history_loop(
    client: &HttpClient,
    channel_id: &str,
    spec: &SlackUriSpec,
) -> WireResult<serde_json::Value> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut cursor: Option<String> = None;
    let has_more = loop {
        let raw = client
            .get_json(&build_history_url_with_cursor(
                channel_id,
                spec,
                cursor.as_deref(),
            ))
            .await?;
        check_ok(&raw)?;
        let messages = raw.get("messages").and_then(|v| v.as_array()).ok_or_else(|| {
            WireError::Storage(format!(
                "slack adapter: unexpected response shape for channel '{channel_id}' history: expected an object with a 'messages' array"
            ))
        })?;
        items.extend(messages.iter().map(normalize_message));
        let next = slack_next_cursor_token(&raw);
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
        "kind": "history",
        "channel_id": channel_id,
        "items": items,
        "has_more": has_more,
    }))
}

/// Inspects a Slack Web API response's `{"ok": bool, ...}` envelope (see
/// module docs "Error handling"). `ok: true` is `Ok(())`; `ok: false` fails
/// loud with the response's `error` code folded into the message, plus the
/// `needed` / `provided` scope hint when both are present (Slack's
/// `missing_scope` shape).
fn check_ok(raw: &serde_json::Value) -> WireResult<()> {
    let ok = raw.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        return Ok(());
    }
    let error = raw
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown_error");
    let needed = raw.get("needed").and_then(|v| v.as_str());
    let provided = raw.get("provided").and_then(|v| v.as_str());
    let msg = match (needed, provided) {
        (Some(needed), Some(provided)) => {
            format!("slack adapter: api error '{error}' (needed={needed}, provided={provided})")
        }
        _ => format!("slack adapter: api error '{error}'"),
    };
    Err(WireError::Storage(msg))
}

/// Normalizes a single-page `GET /conversations.list` response into the
/// Wire JSON shape. Used by unit tests only; [`Adapter::fetch`] drives the
/// multi-page loop and assembles the shape inline.
#[cfg(test)]
fn normalize_channels(raw: &serde_json::Value) -> WireResult<serde_json::Value> {
    let channels = raw.get("channels").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(
            "slack adapter: unexpected response shape for channels: expected an object with a 'channels' array"
                .to_string(),
        )
    })?;
    let items: Vec<serde_json::Value> = channels.iter().map(normalize_channel_item).collect();
    let has_more = raw
        .get("response_metadata")
        .and_then(|m| m.get("next_cursor"))
        .and_then(|c| c.as_str())
        .map(|c| !c.is_empty())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "kind": "channels",
        "items": items,
        "has_more": has_more,
    }))
}

/// Normalizes a single Slack channel JSON object.
fn normalize_channel_item(v: &serde_json::Value) -> serde_json::Value {
    let id = v.get("id").and_then(|x| x.as_str());
    let name = v.get("name").and_then(|x| x.as_str());
    let is_private = v.get("is_private").and_then(|x| x.as_bool());
    let is_archived = v.get("is_archived").and_then(|x| x.as_bool());
    let is_member = v.get("is_member").and_then(|x| x.as_bool());
    let num_members = v.get("num_members").and_then(|x| x.as_u64());
    let topic = v
        .get("topic")
        .and_then(|t| t.get("value"))
        .and_then(|x| x.as_str());
    let purpose = v
        .get("purpose")
        .and_then(|t| t.get("value"))
        .and_then(|x| x.as_str());

    serde_json::json!({
        "id": id,
        "name": name,
        "is_private": is_private,
        "is_archived": is_archived,
        "is_member": is_member,
        "num_members": num_members,
        "topic": topic,
        "purpose": purpose,
    })
}

/// Normalizes a single-page `GET /conversations.history` response into the
/// Wire JSON shape. Used by unit tests only; [`Adapter::fetch`] drives the
/// multi-page loop and assembles the shape inline.
#[cfg(test)]
fn normalize_history(channel_id: &str, raw: &serde_json::Value) -> WireResult<serde_json::Value> {
    let messages = raw.get("messages").and_then(|v| v.as_array()).ok_or_else(|| {
        WireError::Storage(format!(
            "slack adapter: unexpected response shape for channel '{channel_id}' history: expected an object with a 'messages' array"
        ))
    })?;
    let items: Vec<serde_json::Value> = messages.iter().map(normalize_message).collect();
    let has_more = raw
        .get("has_more")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    Ok(serde_json::json!({
        "kind": "history",
        "channel_id": channel_id,
        "items": items,
        "has_more": has_more,
    }))
}

/// Normalizes a single Slack message JSON object.
fn normalize_message(v: &serde_json::Value) -> serde_json::Value {
    let msg_type = v.get("type").and_then(|x| x.as_str());
    let user = v.get("user").and_then(|x| x.as_str());
    let text = v.get("text").and_then(|x| x.as_str()).map(truncate_text);
    let ts = v.get("ts").and_then(|x| x.as_str());
    let thread_ts = v.get("thread_ts").and_then(|x| x.as_str());
    let is_thread_parent = matches!((ts, thread_ts), (Some(t), Some(tt)) if t == tt);
    let reply_count = v.get("reply_count").and_then(|x| x.as_u64());
    let subtype = v.get("subtype").and_then(|x| x.as_str());

    serde_json::json!({
        "type": msg_type,
        "user": user,
        "text": text,
        "ts": ts,
        "thread_ts": thread_ts,
        "is_thread_parent": is_thread_parent,
        "reply_count": reply_count,
        "subtype": subtype,
    })
}

/// Normalizes a `GET /users.info` response (`raw`, expected to be an object
/// with a `user` field) into the Wire JSON shape. See module docs "Output
/// shape".
fn normalize_user(raw: &serde_json::Value) -> WireResult<serde_json::Value> {
    let user = raw.get("user").and_then(|v| v.as_object()).ok_or_else(|| {
        WireError::Storage(
            "slack adapter: unexpected response shape for user: expected an object with a 'user' field"
                .to_string(),
        )
    })?;
    let id = user.get("id").and_then(|x| x.as_str());
    let name = user.get("name").and_then(|x| x.as_str());
    let real_name = user.get("real_name").and_then(|x| x.as_str());
    let display_name = user
        .get("profile")
        .and_then(|p| p.get("display_name"))
        .and_then(|x| x.as_str());
    let email = user
        .get("profile")
        .and_then(|p| p.get("email"))
        .and_then(|x| x.as_str());
    let is_bot = user.get("is_bot").and_then(|x| x.as_bool());
    let deleted = user.get("deleted").and_then(|x| x.as_bool());

    Ok(serde_json::json!({
        "kind": "user",
        "id": id,
        "name": name,
        "real_name": real_name,
        "display_name": display_name,
        "email": email,
        "is_bot": is_bot,
        "deleted": deleted,
    }))
}

/// Truncate `s` to at most [`TEXT_MAX_CHARS`] `char`s (boundary-safe —
/// counts `char`s, not bytes), appending `…` when truncation occurred.
/// Mirrors `persona-wire-adapter-notion::truncate_text`.
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
        let uri = WireUri::parse("slack://channels").unwrap();
        assert_eq!(resolve_service_key(&uri, "slack"), "slack");
    }

    #[test]
    fn resolve_service_key_overrides_when_auth_param_present() {
        let uri = WireUri::parse("slack://channels?auth=slack-alt").unwrap();
        assert_eq!(resolve_service_key(&uri, "slack"), "slack-alt");
    }

    // ---- parse_slack_uri ----

    fn parse(uri: &str) -> WireResult<SlackUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_slack_uri(&wire)
    }

    #[test]
    fn parse_slack_uri_kind_channels_default() {
        let spec = parse("slack://channels").unwrap();
        assert_eq!(spec.kind, SlackKind::Channels);
        assert_eq!(spec.types.as_deref(), Some("public_channel"));
        assert_eq!(spec.exclude_archived, Some(true));
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_slack_uri_kind_history() {
        let spec = parse("slack://history/C012AB3CD").unwrap();
        assert_eq!(spec.kind, SlackKind::History("C012AB3CD".to_string()));
    }

    #[test]
    fn parse_slack_uri_kind_user() {
        let spec = parse("slack://user/U061F7AUR").unwrap();
        assert_eq!(spec.kind, SlackKind::User("U061F7AUR".to_string()));
    }

    #[test]
    fn parse_slack_uri_invalid_kind_fails_loud() {
        let err = parse("slack://reactions").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_slack_uri_empty_host_fails_loud() {
        let err = parse("slack:///").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_slack_uri_history_missing_id_fails_loud() {
        let err = parse("slack://history").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing id"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_slack_uri_user_missing_id_fails_loud() {
        let err = parse("slack://user").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing id"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_slack_uri_channels_extra_path_fails_loud() {
        let err = parse("slack://channels/extra").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unexpected path segment"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_slack_uri_history_extra_segment_fails_loud() {
        let err = parse("slack://history/C012AB3CD/extra").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unexpected extra path segment"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_slack_uri_types_single_valid() {
        let spec = parse("slack://channels?types=private_channel").unwrap();
        assert_eq!(spec.types.as_deref(), Some("private_channel"));
    }

    #[test]
    fn parse_slack_uri_types_comma_valid() {
        let spec = parse("slack://channels?types=public_channel,mpim").unwrap();
        assert_eq!(spec.types.as_deref(), Some("public_channel,mpim"));
    }

    #[test]
    fn parse_slack_uri_types_invalid_fails_loud() {
        let err = parse("slack://channels?types=bogus").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid types entry"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_slack_uri_types_partial_invalid_fails_loud() {
        let err = parse("slack://channels?types=public_channel,bogus").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid types entry"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_slack_uri_exclude_archived_explicit_false() {
        let spec = parse("slack://channels?exclude_archived=false").unwrap();
        assert_eq!(spec.exclude_archived, Some(false));
    }

    #[test]
    fn parse_slack_uri_exclude_archived_invalid_fails_loud() {
        let err = parse("slack://channels?exclude_archived=nope").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("invalid exclude_archived"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_slack_uri_history_and_user_ignore_channels_only_keys() {
        let spec = parse("slack://history/C1?types=mpim&exclude_archived=false").unwrap();
        assert_eq!(spec.types, None);
        assert_eq!(spec.exclude_archived, None);
    }

    #[test]
    fn parse_slack_uri_channels_and_user_ignore_history_only_keys() {
        let spec = parse("slack://channels?oldest=1&latest=2").unwrap();
        assert_eq!(spec.oldest, None);
        assert_eq!(spec.latest, None);
    }

    #[test]
    fn parse_slack_uri_oldest_and_latest_pass_through_decoded() {
        let spec =
            parse("slack://history/C1?oldest=1512085950.000216&latest=1512085960.5").unwrap();
        assert_eq!(spec.oldest.as_deref(), Some("1512085950.000216"));
        assert_eq!(spec.latest.as_deref(), Some("1512085960.5"));
    }

    #[test]
    fn parse_slack_uri_limit_zero_fails_loud() {
        let err = parse("slack://channels?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_slack_uri_limit_non_numeric_fails_loud() {
        let err = parse("slack://channels?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_slack_uri_limit_above_max_ok() {
        let spec = parse("slack://channels?limit=1500").unwrap();
        assert_eq!(spec.limit, 1500);
    }

    #[test]
    fn parse_slack_uri_limit_999_ok() {
        let spec = parse("slack://channels?limit=999").unwrap();
        assert_eq!(spec.limit, 999);
    }

    #[test]
    fn parse_slack_uri_unknown_query_key_ignored() {
        let spec = parse("slack://channels?utm_source=foo").unwrap();
        assert_eq!(spec.kind, SlackKind::Channels);
    }

    // ---- build_*_url ----

    #[test]
    fn build_channels_url_defaults() {
        let spec = parse("slack://channels").unwrap();
        let url = build_channels_url(&spec);
        assert_eq!(
            url,
            "https://slack.com/api/conversations.list?types=public_channel&limit=20&exclude_archived=true"
        );
    }

    #[test]
    fn build_history_url_with_oldest_and_latest() {
        let spec = parse("slack://history/C1?oldest=1.5&latest=2.5&limit=5").unwrap();
        let url = build_history_url("C1", &spec);
        assert!(url.starts_with("https://slack.com/api/conversations.history?channel=C1&limit=5"));
        assert!(url.contains("oldest=1%2E5"));
        assert!(url.contains("latest=2%2E5"));
    }

    #[test]
    fn build_user_url_shape() {
        let url = build_user_url("U061F7AUR");
        assert_eq!(url, "https://slack.com/api/users.info?user=U061F7AUR");
    }

    // ---- check_ok ----

    #[test]
    fn check_ok_true_is_ok() {
        let raw = serde_json::json!({ "ok": true, "channels": [] });
        assert!(check_ok(&raw).is_ok());
    }

    #[test]
    fn check_ok_false_error_only() {
        let raw = serde_json::json!({ "ok": false, "error": "not_in_channel" });
        let err = check_ok(&raw).unwrap_err();
        let msg = format!("{err}");
        assert_eq!(
            msg,
            "storage error: slack adapter: api error 'not_in_channel'"
        );
    }

    #[test]
    fn check_ok_false_with_needed_and_provided() {
        let raw = serde_json::json!({
            "ok": false,
            "error": "missing_scope",
            "needed": "channels:history",
            "provided": "channels:read"
        });
        let err = check_ok(&raw).unwrap_err();
        let msg = format!("{err}");
        assert_eq!(
            msg,
            "storage error: slack adapter: api error 'missing_scope' (needed=channels:history, provided=channels:read)"
        );
    }

    // ---- normalize_channels ----

    fn channels_fixture() -> serde_json::Value {
        // Verbatim shape from the official Slack `conversations.list`
        // response (module docs "URI grammar").
        serde_json::json!({
            "ok": true,
            "channels": [{
                "id": "C012AB3CD",
                "name": "general",
                "is_channel": true,
                "is_private": false,
                "is_archived": false,
                "is_general": true,
                "is_member": true,
                "created": 1449252889,
                "creator": "U012A3CDE",
                "name_normalized": "general",
                "topic": { "value": "Company-wide", "creator": "U012A3CDE", "last_set": 1449252889 },
                "purpose": { "value": "team-wide", "creator": "U012A3CDE", "last_set": 1449252889 },
                "num_members": 4
            }],
            "response_metadata": { "next_cursor": "" }
        })
    }

    #[test]
    fn normalize_channels_field_mapping_and_topic_purpose() {
        let v = normalize_channels(&channels_fixture()).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "channels");
        assert!(!v["has_more"].as_bool().unwrap());
        let item = &v["items"][0];
        assert_eq!(item["id"].as_str().unwrap(), "C012AB3CD");
        assert_eq!(item["name"].as_str().unwrap(), "general");
        assert!(!item["is_private"].as_bool().unwrap());
        assert!(!item["is_archived"].as_bool().unwrap());
        assert!(item["is_member"].as_bool().unwrap());
        assert_eq!(item["num_members"].as_u64().unwrap(), 4);
        assert_eq!(item["topic"].as_str().unwrap(), "Company-wide");
        assert_eq!(item["purpose"].as_str().unwrap(), "team-wide");
    }

    #[test]
    fn normalize_channels_has_more_true_when_next_cursor_non_empty() {
        let mut fixture = channels_fixture();
        fixture["response_metadata"]["next_cursor"] =
            serde_json::Value::String("bmV4dF9jdXJzb3I".to_string());
        let v = normalize_channels(&fixture).unwrap();
        assert!(v["has_more"].as_bool().unwrap());
    }

    #[test]
    fn normalize_channels_missing_topic_purpose_is_null() {
        let raw = serde_json::json!({
            "ok": true,
            "channels": [{ "id": "C1", "name": "x" }],
            "response_metadata": { "next_cursor": "" }
        });
        let v = normalize_channels(&raw).unwrap();
        assert!(v["items"][0]["topic"].is_null());
        assert!(v["items"][0]["purpose"].is_null());
    }

    #[test]
    fn normalize_channels_non_object_response_fails_loud() {
        let raw = serde_json::json!([1, 2, 3]);
        let err = normalize_channels(&raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected an object with a 'channels' array"),
            "unexpected error: {msg}"
        );
    }

    // ---- normalize_history ----

    fn history_fixture() -> serde_json::Value {
        // Verbatim shape from the official Slack `conversations.history`
        // response (module docs "URI grammar").
        serde_json::json!({
            "ok": true,
            "messages": [
                { "type": "message", "user": "U061F7AUR", "text": "hello", "ts": "1512085950.000216" },
                {
                    "type": "message", "user": "U061F7AUR", "text": "thread parent",
                    "ts": "1482960137.003543", "thread_ts": "1482960137.003543", "reply_count": 3
                },
                {
                    "type": "message", "user": "U061F7AUR", "text": "a reply",
                    "ts": "1482960200.000100", "thread_ts": "1482960137.003543"
                }
            ],
            "has_more": true,
            "pin_count": 0,
            "response_metadata": { "next_cursor": "bmV4dF90czoxNTEyMDg1ODYx" }
        })
    }

    #[test]
    fn normalize_history_field_mapping() {
        let v = normalize_history("C012AB3CD", &history_fixture()).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "history");
        assert_eq!(v["channel_id"].as_str().unwrap(), "C012AB3CD");
        assert!(v["has_more"].as_bool().unwrap());
        let plain = &v["items"][0];
        assert_eq!(plain["type"].as_str().unwrap(), "message");
        assert_eq!(plain["user"].as_str().unwrap(), "U061F7AUR");
        assert_eq!(plain["text"].as_str().unwrap(), "hello");
        assert_eq!(plain["ts"].as_str().unwrap(), "1512085950.000216");
        assert!(plain["thread_ts"].is_null());
        assert!(!plain["is_thread_parent"].as_bool().unwrap());
        assert!(plain["reply_count"].is_null());
    }

    #[test]
    fn normalize_history_thread_parent_detected() {
        let v = normalize_history("C1", &history_fixture()).unwrap();
        let parent = &v["items"][1];
        assert_eq!(
            parent["ts"].as_str().unwrap(),
            parent["thread_ts"].as_str().unwrap()
        );
        assert!(parent["is_thread_parent"].as_bool().unwrap());
        assert_eq!(parent["reply_count"].as_u64().unwrap(), 3);
    }

    #[test]
    fn normalize_history_thread_reply_not_parent() {
        let v = normalize_history("C1", &history_fixture()).unwrap();
        let reply = &v["items"][2];
        assert_ne!(
            reply["ts"].as_str().unwrap(),
            reply["thread_ts"].as_str().unwrap()
        );
        assert!(!reply["is_thread_parent"].as_bool().unwrap());
    }

    #[test]
    fn normalize_history_text_truncated() {
        let long_text = "a".repeat(600);
        let raw = serde_json::json!({
            "ok": true,
            "messages": [{ "type": "message", "text": long_text, "ts": "1.0" }],
            "has_more": false
        });
        let v = normalize_history("C1", &raw).unwrap();
        let text = v["items"][0]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), TEXT_MAX_CHARS + 1, "500 + ellipsis");
        assert!(text.ends_with('…'));
    }

    #[test]
    fn normalize_history_non_object_response_fails_loud() {
        let raw = serde_json::json!({ "ok": true });
        let err = normalize_history("C1", &raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected an object with a 'messages' array"),
            "unexpected error: {msg}"
        );
    }

    // ---- normalize_user ----

    fn user_fixture() -> serde_json::Value {
        // Verbatim shape from the official Slack `users.info` response
        // (module docs "URI grammar").
        serde_json::json!({
            "ok": true,
            "user": {
                "id": "U061F7AUR",
                "name": "egonspengler",
                "real_name": "Egon Spengler",
                "is_bot": false,
                "deleted": false,
                "profile": {
                    "display_name": "spengler",
                    "real_name": "Egon Spengler",
                    "email": "spengler@example.com"
                }
            }
        })
    }

    #[test]
    fn normalize_user_field_mapping() {
        let v = normalize_user(&user_fixture()).unwrap();
        assert_eq!(v["kind"].as_str().unwrap(), "user");
        assert_eq!(v["id"].as_str().unwrap(), "U061F7AUR");
        assert_eq!(v["name"].as_str().unwrap(), "egonspengler");
        assert_eq!(v["real_name"].as_str().unwrap(), "Egon Spengler");
        assert_eq!(v["display_name"].as_str().unwrap(), "spengler");
        assert_eq!(v["email"].as_str().unwrap(), "spengler@example.com");
        assert!(!v["is_bot"].as_bool().unwrap());
        assert!(!v["deleted"].as_bool().unwrap());
    }

    #[test]
    fn normalize_user_email_absent_is_null() {
        let mut fixture = user_fixture();
        fixture["user"]["profile"]
            .as_object_mut()
            .unwrap()
            .remove("email");
        let v = normalize_user(&fixture).unwrap();
        assert!(v["email"].is_null());
    }

    #[test]
    fn normalize_user_missing_fields_are_null() {
        let raw = serde_json::json!({ "ok": true, "user": { "id": "U1" } });
        let v = normalize_user(&raw).unwrap();
        assert_eq!(v["id"].as_str().unwrap(), "U1");
        assert!(v["name"].is_null());
        assert!(v["real_name"].is_null());
        assert!(v["display_name"].is_null());
        assert!(v["email"].is_null());
        assert!(v["is_bot"].is_null());
        assert!(v["deleted"].is_null());
    }

    #[test]
    fn normalize_user_non_object_response_fails_loud() {
        let raw = serde_json::json!({ "ok": true });
        let err = normalize_user(&raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected an object with a 'user' field"),
            "unexpected error: {msg}"
        );
    }

    // ---- internal pagination helpers ----
    //
    // The `next_cursor` loop is driven internally by `Adapter::fetch` over
    // `HttpClient` (a concrete struct not behind a mockable trait), and this
    // workspace's convention (established in `adapter.rs` crate docs) is
    // that Adapter tests are offline unit tests over inline fixtures. The
    // URL builders and the cursor-token extractor are exercised as pure
    // functions below.

    #[test]
    fn slack_channels_url_with_cursor() {
        let spec = parse("slack://channels").unwrap();
        let url = build_channels_url_with_cursor(&spec, Some("bmV4dF9jdXJzb3I"));
        assert!(
            url.contains("&cursor=bmV4dF9jdXJzb3I"),
            "unexpected url: {url}"
        );
    }

    #[test]
    fn slack_channels_url_without_cursor_matches_fast_path() {
        let spec = parse("slack://channels").unwrap();
        let with_none = build_channels_url_with_cursor(&spec, None);
        let fast_path = build_channels_url(&spec);
        assert_eq!(with_none, fast_path);
        assert!(!with_none.contains("cursor="));
    }

    #[test]
    fn slack_history_url_with_cursor() {
        let spec = parse("slack://history/C1").unwrap();
        let url = build_history_url_with_cursor("C1", &spec, Some("bmV4dF90czoxNTEy"));
        assert!(
            url.contains("&cursor=bmV4dF90czoxNTEy"),
            "unexpected url: {url}"
        );
    }

    #[test]
    fn slack_history_url_without_cursor_matches_fast_path() {
        let spec = parse("slack://history/C1").unwrap();
        let with_none = build_history_url_with_cursor("C1", &spec, None);
        let fast_path = build_history_url("C1", &spec);
        assert_eq!(with_none, fast_path);
        assert!(!with_none.contains("cursor="));
    }

    #[test]
    fn slack_next_cursor_empty_string_is_none() {
        let raw = serde_json::json!({
            "ok": true,
            "channels": [],
            "response_metadata": { "next_cursor": "" }
        });
        assert_eq!(slack_next_cursor_token(&raw), None);
    }

    #[test]
    fn slack_next_cursor_non_empty_is_some() {
        let raw = serde_json::json!({
            "ok": true,
            "channels": [],
            "response_metadata": { "next_cursor": "bmV4dF9jdXJzb3I" }
        });
        assert_eq!(
            slack_next_cursor_token(&raw).as_deref(),
            Some("bmV4dF9jdXJzb3I")
        );
    }

    #[test]
    fn slack_next_cursor_missing_is_none() {
        let raw = serde_json::json!({ "ok": true, "channels": [] });
        assert_eq!(slack_next_cursor_token(&raw), None);
    }

    #[test]
    fn slack_channels_url_clamps_limit_over_max() {
        let spec = parse("slack://channels?limit=5000").unwrap();
        let url = build_channels_url(&spec);
        assert!(
            url.contains(&format!("limit={MAX_LIMIT}")),
            "over-max limit must clamp at MAX_LIMIT: {url}"
        );
        assert!(
            !url.contains("limit=5000"),
            "raw limit leaked into URL: {url}"
        );
    }
}
