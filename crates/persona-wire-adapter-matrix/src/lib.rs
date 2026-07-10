//! persona-wire Adapter for Matrix (scheme `matrix://`).
//!
//! ## Architecture
//!
//! `MatrixAdapter` is a stateless [`Adapter`] impl split into three
//! independent pieces, matching the repo-wide adapter convention (see
//! `persona_wire_core::infrastructure::adapter` crate docs):
//!
//! - [`parse_matrix_uri`] — `WireUri` → [`MatrixRequest`] (homeserver + kind
//!   dispatch: `sync` or `rooms/<room_id>/messages`).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient` (no
//!   Matrix-specific knowledge in the transport layer).
//! - [`Adapter::fetch`] — builds the upstream request URL per kind, sends it,
//!   and wraps the raw Matrix Client-Server API v3 JSON response in a small
//!   `{scheme, kind, homeserver, ..., body}` envelope (see "Output shape").
//!
//! Phase 1 covers exactly two Matrix Client-Server API v3 endpoints:
//! `GET /_matrix/client/v3/sync` and `GET /_matrix/client/v3/rooms/{room_id}/messages`.
//! Sending messages, room membership management, and end-to-end encryption
//! are out of scope for this Phase.
//!
//! ## URI grammar
//!
//! ```text
//! matrix://<homeserver>/sync[?limit=N][&auth=<key>]
//! matrix://<homeserver>/rooms/<room_id>/messages[?limit=N][&dir=b|f][&auth=<key>]
//! ```
//!
//! - `host` is the Matrix homeserver (e.g. `matrix.org`); an empty host
//!   fails loud. The upstream request URL is always `https://<homeserver>`.
//! - The first path segment selects the endpoint: `sync` (no further
//!   segments) or `rooms/<room_id>/messages` (exactly three segments). Any
//!   other path **fails loud** — matching `persona-wire-adapter-github`'s
//!   `?kind=` convention, an unrecognized shape here would otherwise mean a
//!   silently wrong request rather than a clear rejection.
//! - `<room_id>` is a Matrix room ID (`!abc:matrix.org`) or room alias
//!   (`#room:matrix.org`), percent-encoded in the URI path segment; the
//!   adapter percent-decodes it before embedding it in the upstream request
//!   URL (re-encoded exactly once there via `url::Url::path_segments_mut`,
//!   the same "decode once, encode once" convention as
//!   `persona-wire-adapter-todoist`'s `filter` query value).
//! - `limit` caps the number of events returned by either endpoint (default
//!   [`DEFAULT_LIMIT`]). A non-numeric or zero value fails loud.
//! - `dir` only applies to `rooms/<room_id>/messages` (pagination direction);
//!   default `"b"` (backwards). `"f"` is also accepted; any other value
//!   fails loud. It is not read at all for `sync` (unapplicable — same
//!   "silently ignored for the kind it doesn't apply to" convention as
//!   `persona-wire-adapter-github`'s `state` for `kind=releases`).
//! - `auth` selects the credential `service_key` (see "Auth"); it is
//!   captured by [`resolve_service_key`] before endpoint dispatch and never
//!   forwarded to the upstream Matrix request URL.
//! - All other query keys are silently ignored (same forward-compatible
//!   convention as every other adapter in this workspace).
//!
//! ## Auth
//!
//! Every Matrix Client-Server API v3 endpoint covered by Phase 1 is called
//! with `Authorization: Bearer <access_token>` — unlike
//! `persona-wire-adapter-github`, this adapter has **no unauthenticated
//! fallback**; a missing token fails loud (matching
//! `persona-wire-adapter-todoist`'s auth-required convention).
//!
//! The credential `service_key` defaults to [`DEFAULT_SERVICE_KEY`]
//! (`"matrix"`), resolved per-fetch (not at boot) via
//! `persona_wire_credentials::Credentials::default_chain().get(service_key)`.
//! Store an access token via `persona-wire token set matrix`, or set the
//! `PERSONA_WIRE_TOKEN_MATRIX` environment variable.
//!
//! Multi-homeserver setups (distinct accounts per homeserver) override the
//! service key per-URI via `?auth=<service_key>`, e.g.
//! `matrix://work.example.org/sync?auth=matrix-work` resolves the token
//! under `PERSONA_WIRE_TOKEN_MATRIX_WORK` / `persona-wire token set
//! matrix-work` instead of the default `matrix` key.
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "scheme": "matrix",
//!   "kind": "sync",
//!   "homeserver": "matrix.org",
//!   "body": { /* raw /_matrix/client/v3/sync response */ }
//! }
//! ```
//!
//! ```json
//! {
//!   "scheme": "matrix",
//!   "kind": "rooms_messages",
//!   "homeserver": "matrix.org",
//!   "room_id": "!abc:matrix.org",
//!   "body": { /* raw /_matrix/client/v3/rooms/{room_id}/messages response */ }
//! }
//! ```
//!
//! `body` is the upstream Matrix JSON response verbatim (no per-field
//! normalization in Phase 1 — the raw `sync` / `messages` response shapes
//! are Matrix spec, not this crate's concern to re-flatten).

#![warn(missing_docs)]

use async_trait::async_trait;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use persona_wire_credentials::Credentials;
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `limit` cap when `?limit=` is absent from the URI (applies to
/// both `sync` and `rooms/<room_id>/messages`).
pub const DEFAULT_LIMIT: usize = 50;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-adapter-github::FETCH_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Default credential `service_key` when `?auth=` is absent from the URI.
pub const DEFAULT_SERVICE_KEY: &str = "matrix";

/// persona-wire Adapter for Matrix (`matrix://` scheme).
pub struct MatrixAdapter;

#[async_trait]
impl Adapter for MatrixAdapter {
    fn scheme(&self) -> &'static str {
        "matrix"
    }

    /// Fetch the `sync` or `rooms/<room_id>/messages` payload identified by
    /// `uri`, and wrap the raw upstream JSON in the envelope documented in
    /// the module docs "Output shape". See the module docs for URI grammar
    /// and auth resolution.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let req = parse_matrix_uri(uri)?;
        let client = matrix_http_client(uri)?;
        match req {
            MatrixRequest::Sync { homeserver, limit } => {
                let url = sync_url(&homeserver, limit)?;
                let body = client.get_json(&url).await?;
                Ok(serde_json::json!({
                    "scheme": "matrix",
                    "kind": "sync",
                    "homeserver": homeserver,
                    "body": body,
                }))
            }
            MatrixRequest::RoomMessages {
                homeserver,
                room_id,
                limit,
                dir,
            } => {
                let url = messages_url(&homeserver, &room_id, &dir, limit)?;
                let body = client.get_json(&url).await?;
                Ok(serde_json::json!({
                    "scheme": "matrix",
                    "kind": "rooms_messages",
                    "homeserver": homeserver,
                    "room_id": room_id,
                    "body": body,
                }))
            }
        }
    }
}

/// Determines the credential `service_key` for `uri`: `?auth=<key>`
/// overrides `default`, enabling multi-homeserver credential resolution
/// without changing the URI's host. Mirrors
/// `persona_wire_adapter_github`'s `resolve_service_key` helper.
fn resolve_service_key<'a>(uri: &'a WireUri, default: &'a str) -> &'a str {
    uri.query_get("auth").unwrap_or(default)
}

/// Builds a fresh, Matrix-configured `HttpClient` (auth resolved per-call,
/// not at boot; see module docs "Auth"). Fails loud when no token resolves
/// for the URI's `service_key` — unlike
/// `persona_wire_adapter_github::github_http_client`, Matrix has no
/// unauthenticated fallback (module docs "Auth").
fn matrix_http_client(uri: &WireUri) -> WireResult<HttpClient> {
    matrix_http_client_from_creds(uri, &Credentials::default_chain())
}

/// Inner helper that takes an explicit [`Credentials`] chain. Extracted so
/// unit tests can inject an env-only chain (via
/// [`Credentials::with_providers`]) and avoid touching the OS keyring —
/// Linux CI runners have no `secret-service` DBus daemon, so the default
/// chain hard-errors before the fail-loud guidance message can be
/// produced.
fn matrix_http_client_from_creds(
    uri: &WireUri,
    credentials: &Credentials,
) -> WireResult<HttpClient> {
    let service_key = resolve_service_key(uri, DEFAULT_SERVICE_KEY);
    let token = credentials.get(service_key)?.ok_or_else(|| {
        let env_var = service_key.to_uppercase().replace('-', "_");
        WireError::Storage(format!(
            "matrix adapter: no token configured for service '{service_key}' (set PERSONA_WIRE_TOKEN_{env_var} or run `persona-wire token set {service_key}`)"
        ))
    })?;
    Ok(HttpClient::new("matrix adapter")
        .with_timeout(FETCH_TIMEOUT)
        .with_bearer(token))
}

/// The two Matrix Client-Server API v3 endpoints this adapter can target,
/// selected via the URI path (module docs "URI grammar").
#[derive(Debug, Clone, PartialEq, Eq)]
enum MatrixRequest {
    /// `GET /_matrix/client/v3/sync`.
    Sync { homeserver: String, limit: usize },
    /// `GET /_matrix/client/v3/rooms/{room_id}/messages`.
    RoomMessages {
        homeserver: String,
        room_id: String,
        limit: usize,
        /// Pagination direction: `"b"` (backwards, default) or `"f"` (forwards).
        dir: String,
    },
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`MatrixRequest`]. See the module-level "URI grammar" section for
/// the exact rules and failure conditions.
fn parse_matrix_uri(uri: &WireUri) -> WireResult<MatrixRequest> {
    let homeserver = uri
        .host()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            WireError::Storage(format!(
                "matrix adapter: missing homeserver (host) in '{}'",
                uri.as_raw()
            ))
        })?
        .to_string();

    let segments: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();

    match segments.as_slice() {
        ["sync"] => {
            let limit = parse_limit(uri)?;
            Ok(MatrixRequest::Sync { homeserver, limit })
        }
        ["rooms", room_id, "messages"] => {
            let room_id = percent_decode_str(room_id)
                .decode_utf8_lossy()
                .into_owned();
            let limit = parse_limit(uri)?;
            let dir = parse_dir(uri)?;
            Ok(MatrixRequest::RoomMessages {
                homeserver,
                room_id,
                limit,
                dir,
            })
        }
        _ => Err(WireError::Storage(format!(
            "matrix adapter: unsupported path '{}' (expected matrix://<homeserver>/sync or matrix://<homeserver>/rooms/<room_id>/messages)",
            uri.as_raw()
        ))),
    }
}

/// Parses `?limit=` (module docs "URI grammar"): defaults to
/// [`DEFAULT_LIMIT`]; a non-numeric or zero value fails loud.
fn parse_limit(uri: &WireUri) -> WireResult<usize> {
    match uri.query_get("limit") {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "matrix adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "matrix adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            Ok(n)
        }
        None => Ok(DEFAULT_LIMIT),
    }
}

/// Parses `?dir=` for `rooms/<room_id>/messages` (module docs "URI
/// grammar"): defaults to `"b"`; `"f"` is also accepted; any other value
/// fails loud. Callers only invoke this for the `RoomMessages` kind — `dir`
/// is not read at all for `sync`.
fn parse_dir(uri: &WireUri) -> WireResult<String> {
    match uri.query_get("dir") {
        None => Ok("b".to_string()),
        Some(d @ ("b" | "f")) => Ok(d.to_string()),
        Some(bad) => Err(WireError::Storage(format!(
            "matrix adapter: invalid dir '{bad}' (must be one of: b, f)"
        ))),
    }
}

/// Builds the `GET /_matrix/client/v3/sync` request URL. `limit` is
/// forwarded as an inline JSON `filter` (`{"room":{"timeline":{"limit":N}}}`,
/// the Matrix Client-Server API's mechanism for bounding the number of
/// timeline events returned) plus `timeout=0` (Phase 1 never long-polls; see
/// module docs "Architecture").
fn sync_url(homeserver: &str, limit: usize) -> WireResult<String> {
    let mut url = url::Url::parse(&format!("https://{homeserver}/_matrix/client/v3/sync"))
        .map_err(|e| {
            WireError::Storage(format!(
                "matrix adapter: invalid homeserver '{homeserver}': {e}"
            ))
        })?;
    let filter = serde_json::json!({"room": {"timeline": {"limit": limit}}}).to_string();
    url.query_pairs_mut()
        .append_pair("filter", &filter)
        .append_pair("timeout", "0");
    Ok(url.to_string())
}

/// Builds the `GET /_matrix/client/v3/rooms/{room_id}/messages` request URL.
/// `room_id` is percent-encoded into the path segment by
/// `url::Url::path_segments_mut` (the "encode once" half of the "decode
/// once, encode once" convention documented in module docs "URI grammar").
fn messages_url(homeserver: &str, room_id: &str, dir: &str, limit: usize) -> WireResult<String> {
    // room_id (`!abc:matrix.org` / `#alias:matrix.org`) contains RFC 3986
    // sub-delims (`!`) and reserved (`:`) that `url::Url::path_segments_mut`'s
    // default encoder leaves untouched. Encode explicitly (matching the
    // Matrix Client-Server API v3 convention of URL-encoding room IDs).
    let encoded_room = utf8_percent_encode(room_id, NON_ALPHANUMERIC).to_string();
    let mut url = url::Url::parse(&format!(
        "https://{homeserver}/_matrix/client/v3/rooms/{encoded_room}/messages"
    ))
    .map_err(|e| {
        WireError::Storage(format!(
            "matrix adapter: invalid homeserver '{homeserver}' or room_id '{room_id}': {e}"
        ))
    })?;
    url.query_pairs_mut()
        .append_pair("dir", dir)
        .append_pair("limit", &limit.to_string());
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_matrix_uri: sync ----

    fn parse(uri: &str) -> WireResult<MatrixRequest> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_matrix_uri(&wire)
    }

    #[test]
    fn parse_sync_default_limit() {
        let req = parse("matrix://matrix.org/sync").unwrap();
        assert_eq!(
            req,
            MatrixRequest::Sync {
                homeserver: "matrix.org".to_string(),
                limit: DEFAULT_LIMIT,
            }
        );
    }

    #[test]
    fn parse_sync_explicit_limit() {
        let req = parse("matrix://matrix.org/sync?limit=10").unwrap();
        assert_eq!(
            req,
            MatrixRequest::Sync {
                homeserver: "matrix.org".to_string(),
                limit: 10,
            }
        );
    }

    // ---- parse_matrix_uri: rooms/<room_id>/messages ----

    #[test]
    fn parse_rooms_messages_default_dir_and_limit() {
        let req = parse("matrix://matrix.org/rooms/!abc:matrix.org/messages").unwrap();
        assert_eq!(
            req,
            MatrixRequest::RoomMessages {
                homeserver: "matrix.org".to_string(),
                room_id: "!abc:matrix.org".to_string(),
                limit: DEFAULT_LIMIT,
                dir: "b".to_string(),
            }
        );
    }

    #[test]
    fn parse_rooms_messages_dir_f_explicit() {
        let req =
            parse("matrix://matrix.org/rooms/!abc:matrix.org/messages?dir=f&limit=5").unwrap();
        assert_eq!(
            req,
            MatrixRequest::RoomMessages {
                homeserver: "matrix.org".to_string(),
                room_id: "!abc:matrix.org".to_string(),
                limit: 5,
                dir: "f".to_string(),
            }
        );
    }

    #[test]
    fn parse_rooms_messages_room_id_percent_decoded() {
        // `!` -> %21, `:` -> %3A
        let req = parse("matrix://matrix.org/rooms/%21abc%3Amatrix.org/messages").unwrap();
        assert_eq!(
            req,
            MatrixRequest::RoomMessages {
                homeserver: "matrix.org".to_string(),
                room_id: "!abc:matrix.org".to_string(),
                limit: DEFAULT_LIMIT,
                dir: "b".to_string(),
            }
        );
    }

    // ---- parse_matrix_uri: failures ----

    #[test]
    fn parse_invalid_path_fails_loud() {
        let err = parse("matrix://matrix.org/unknown").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unsupported path"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_missing_homeserver_fails_loud() {
        let err = parse("matrix:///sync").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("missing homeserver"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_invalid_limit_fails_loud() {
        let err = parse("matrix://matrix.org/sync?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_invalid_dir_fails_loud() {
        let err =
            parse("matrix://matrix.org/rooms/!abc:matrix.org/messages?dir=sideways").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid dir"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_unknown_query_key_ignored() {
        let req = parse("matrix://matrix.org/sync?utm_source=foo").unwrap();
        assert_eq!(
            req,
            MatrixRequest::Sync {
                homeserver: "matrix.org".to_string(),
                limit: DEFAULT_LIMIT,
            }
        );
    }

    // ---- resolve_service_key ----

    #[test]
    fn resolve_service_key_defaults_when_auth_absent() {
        let wire = WireUri::parse("matrix://matrix.org/sync").unwrap();
        assert_eq!(resolve_service_key(&wire, DEFAULT_SERVICE_KEY), "matrix");
    }

    #[test]
    fn resolve_service_key_overridden_by_auth_query() {
        let wire = WireUri::parse("matrix://work.example.org/sync?auth=matrix-work").unwrap();
        assert_eq!(
            resolve_service_key(&wire, DEFAULT_SERVICE_KEY),
            "matrix-work"
        );
    }

    // ---- matrix_http_client ----
    //
    // Sequential within one #[test] fn (not two separate fns) because both
    // branches share the process-global `PERSONA_WIRE_TOKEN_MATRIX` env var
    // and `cargo test` runs tests in parallel threads — the same convention
    // `persona-wire-credentials::tests::env_provider_alias_fallback_and_primary_precedence`
    // uses to avoid flaking on shared env state.
    /// Env-only [`Credentials`] chain — bypasses the OS keyring so tests
    /// stay hermetic (Linux CI has no `secret-service` DBus daemon). See
    /// [`matrix_http_client_from_creds`] doc for rationale.
    fn env_only_credentials() -> Credentials {
        use persona_wire_credentials::EnvTokenProvider;
        Credentials::with_providers(vec![Box::new(EnvTokenProvider)])
    }

    #[test]
    fn matrix_http_client_fails_loud_without_token_then_builds_with_env_token() {
        std::env::remove_var("PERSONA_WIRE_TOKEN_MATRIX");
        let uri = WireUri::parse("matrix://matrix.org/sync").unwrap();
        let creds = env_only_credentials();

        // `HttpClient` intentionally does not derive `Debug` (module docs of
        // `persona_wire_transport_http`), so `Result::unwrap_err` (which
        // bounds `T: Debug`) does not apply here — match instead.
        let err = match matrix_http_client_from_creds(&uri, &creds) {
            Err(e) => e,
            Ok(_) => panic!("expected Err without PERSONA_WIRE_TOKEN_MATRIX set"),
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("PERSONA_WIRE_TOKEN_MATRIX"),
            "unexpected error: {msg}"
        );

        std::env::set_var("PERSONA_WIRE_TOKEN_MATRIX", "dummy-token");
        let result = matrix_http_client_from_creds(&uri, &creds);
        std::env::remove_var("PERSONA_WIRE_TOKEN_MATRIX");
        assert!(
            result.is_ok(),
            "expected Ok once PERSONA_WIRE_TOKEN_MATRIX is set"
        );
    }

    #[test]
    fn matrix_http_client_honors_auth_override_env_var() {
        std::env::remove_var("PERSONA_WIRE_TOKEN_MATRIX_WORK");
        let uri = WireUri::parse("matrix://work.example.org/sync?auth=matrix-work").unwrap();
        let creds = env_only_credentials();

        let err = match matrix_http_client_from_creds(&uri, &creds) {
            Err(e) => e,
            Ok(_) => panic!("expected Err without PERSONA_WIRE_TOKEN_MATRIX_WORK set"),
        };
        assert!(format!("{err}").contains("PERSONA_WIRE_TOKEN_MATRIX_WORK"));

        std::env::set_var("PERSONA_WIRE_TOKEN_MATRIX_WORK", "dummy-token");
        let result = matrix_http_client_from_creds(&uri, &creds);
        std::env::remove_var("PERSONA_WIRE_TOKEN_MATRIX_WORK");
        assert!(result.is_ok());
    }

    // ---- sync_url / messages_url ----

    #[test]
    fn sync_url_shape() {
        let url = sync_url("matrix.org", 20).unwrap();
        assert!(url.starts_with("https://matrix.org/_matrix/client/v3/sync?"));
        assert!(url.contains("timeout=0"));
        assert!(url.contains("filter="));
        // The raw JSON braces must not appear literally (query-encoded).
        assert!(!url.contains('{'), "filter JSON must be encoded: {url}");
    }

    #[test]
    fn messages_url_shape_encodes_room_id() {
        let url = messages_url("matrix.org", "!abc:matrix.org", "b", 30).unwrap();
        assert!(url.starts_with("https://matrix.org/_matrix/client/v3/rooms/"));
        assert!(url.contains("/messages?"));
        assert!(url.contains("dir=b"));
        assert!(url.contains("limit=30"));
        // `!` must be percent-encoded in the path segment.
        assert!(
            !url[..url.find('?').unwrap()].contains('!'),
            "room_id must be percent-encoded in the path: {url}"
        );
    }
}
