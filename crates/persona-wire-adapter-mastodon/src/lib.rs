//! persona-wire Adapter for Mastodon (scheme `mastodon://`).
//!
//! ## Architecture
//!
//! `MastodonAdapter` is a stateless [`Adapter`] impl split into three
//! independent functions, mirroring `persona-wire-adapter-github`:
//!
//! - [`parse_mastodon_uri`] — `WireUri` → [`MastodonUriSpec`] (instance +
//!   timeline kind + `limit` / `local` / `only_media`).
//! - [`resolve_service_key`] — `WireUri` → credentials service key (the
//!   `?auth=` override, or the crate-wide default).
//! - [`mastodon_http_client`] — service key + timeline kind → a
//!   Bearer-authenticated (or, for `timelines/public` only,
//!   unauthenticated-fallback) [`HttpClient`].
//!
//! `Adapter::fetch` wires the three together, then fetches Mastodon's REST
//! API v1 response as-is and wraps it in the Wire JSON shape below — Phase 1
//! does not re-shape individual status objects (see "Output shape").
//!
//! ## URI grammar
//!
//! ```text
//! mastodon://<instance>/timelines/home?limit=N&auth=<key>
//! mastodon://<instance>/timelines/public?local=true&only_media=false&limit=N&auth=<key>
//! ```
//!
//! - `<instance>` is the URI host (e.g. `mastodon.social` / `fosstodon.org`);
//!   an empty host fails loud. The upstream base URL is `https://<instance>`.
//! - The path must be exactly `timelines/home` or `timelines/public`; any
//!   other path fails loud.
//! - `limit` caps the number of items returned (default [`DEFAULT_LIMIT`]).
//!   A non-numeric or zero value fails loud. A value above
//!   [`MASTODON_LIMIT_MAX`] (Mastodon's own per-request ceiling) is clamped
//!   down to it, with a `tracing::warn!` (not a hard failure — unlike
//!   `?limit=0`, an oversized limit is a harmless, silently-correctable
//!   input).
//! - `local` / `only_media` apply to `timelines/public` only (default
//!   `false` for both); `true` / `false` are accepted, any other value fails
//!   loud. For `timelines/home`, both are silently ignored (not read, not
//!   validated) — Mastodon's home-timeline endpoint has no such filter, same
//!   convention as `persona-wire-adapter-github`'s `state` being ignored for
//!   `kind=releases`.
//! - `?auth=<service_key>` selects which credentials-provider service key to
//!   resolve the Bearer token from, letting one caller manage tokens for
//!   multiple instances (e.g. `?auth=work` resolves the `work` service key
//!   instead of the crate-wide default, [`DEFAULT_SERVICE_KEY`]). Absent or
//!   empty `?auth=` uses the default. This key never reaches the upstream
//!   request — it is consumed by [`resolve_service_key`] and does not appear
//!   in [`MastodonUriSpec::endpoint_url`]'s query string.
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-github` / `-rss`).
//!
//! ## Auth
//!
//! Resolved per-fetch (not at boot) via
//! `persona_wire_credentials::Credentials::default_chain().get(service_key)`,
//! so a token change takes effect without restarting the process. Set a
//! token via the `PERSONA_WIRE_TOKEN_<SERVICE_KEY>` environment variable (or,
//! for the default service key, the `MASTODON_ACCESS_TOKEN` alias), or store
//! one in the OS keychain via `persona-wire token set mastodon`.
//!
//! Unlike `persona-wire-adapter-github` (unauthenticated fallback for every
//! request — GitHub's public-repo read endpoints tolerate it), auth
//! resolution here is **asymmetric per timeline**:
//!
//! - `timelines/home` always requires a resolved token (Mastodon has no
//!   unauthenticated "home" concept — it is inherently the caller's own
//!   feed) — a missing token **fails loud**.
//! - `timelines/public` (instance-local or federated public feed) works
//!   unauthenticated — a missing token falls back gracefully (logged via
//!   `tracing::info!`, not an error).
//!
//! A backend error while resolving the token (e.g. keychain access denied)
//! always fails loud and propagates, for both timelines — only "no token
//! configured" is treated as `None`.
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "instance": "mastodon.social",
//!   "kind": "timelines/home",
//!   "items": [ /* raw Mastodon Status objects, unmodified */ ]
//! }
//! ```
//!
//! `items` is the upstream Mastodon REST API v1 response array passed
//! through unmodified (Phase 1 does not extract or rename individual
//! `Status` fields, unlike `persona-wire-adapter-github`'s per-item
//! normalization) — see the module-level "Phase 1 scope" note below. A
//! non-array upstream response fails loud, naming the instance and kind.
//!
//! ## Phase 1 scope
//!
//! Only the two read-only timeline endpoints above are implemented:
//! `GET /api/v1/timelines/home` and `GET /api/v1/timelines/public`. No
//! posting, no notifications, no search, no account lookup, no pagination
//! beyond a single `?limit=N` page (Mastodon's `Link`-header pagination,
//! mirroring `persona-wire-adapter-github`'s multi-page loop, is a future
//! extension once a caller needs more than one page's worth of items).
//!
//! ## vs. `activitypub://`
//!
//! `persona-wire-adapter-activitypub` reads the **public, unauthenticated**
//! surface of the ActivityPub protocol (an actor's outbox / profile
//! document) and works against any compliant Fediverse server (Mastodon,
//! Misskey, Pleroma, ...) via the generic ActivityStreams shape. This crate
//! is **Mastodon-native**: it calls Mastodon's own REST API v1
//! (`/api/v1/timelines/...`), which is Mastodon-specific (not a generic
//! ActivityPub concept) and, for `timelines/home`, requires the caller's own
//! Bearer token — a capability the generic ActivityPub outbox model has no
//! equivalent for. Use `activitypub://` for cross-instance public reads of a
//! specific account; use `mastodon://` for a caller's own home feed or an
//! instance's local/public timeline.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use persona_wire_credentials::Credentials;
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `items` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Mastodon's own upper bound for `?limit=` on timeline endpoints. A
/// requested `limit` above this is clamped down (with a `tracing::warn!`),
/// not rejected — see the module docs "URI grammar".
pub const MASTODON_LIMIT_MAX: usize = 40;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-transport-http::DEFAULT_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Default credentials service key when `?auth=` is absent from the URI.
pub const DEFAULT_SERVICE_KEY: &str = "mastodon";

/// persona-wire Adapter for Mastodon (`mastodon://` scheme).
pub struct MastodonAdapter;

#[async_trait]
impl Adapter for MastodonAdapter {
    fn scheme(&self) -> &'static str {
        "mastodon"
    }

    /// Fetch `spec.kind`'s timeline for the instance derived from `uri`. See
    /// the module docs for URI grammar, auth resolution (asymmetric per
    /// timeline), and output shape.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_mastodon_uri(uri)?;
        let service_key = resolve_service_key(uri, DEFAULT_SERVICE_KEY);
        let client = mastodon_http_client(service_key, spec.kind)?;
        let raw = client.get_json(&spec.endpoint_url()).await?;
        shape_response(&spec, raw)
    }
}

/// The two Mastodon REST API v1 timeline endpoints this adapter can target,
/// selected via the URI path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MastodonKind {
    Home,
    Public,
}

impl MastodonKind {
    fn as_str(self) -> &'static str {
        match self {
            MastodonKind::Home => "timelines/home",
            MastodonKind::Public => "timelines/public",
        }
    }
}

/// Parsed `mastodon://` URI: instance + timeline kind + `limit` / `local` /
/// `only_media`. `local` / `only_media` are always populated (default
/// `false`) but only meaningful — and only rendered into the request URL —
/// when `kind == Public` (see [`MastodonUriSpec::endpoint_url`]).
#[derive(Debug)]
struct MastodonUriSpec {
    instance: String,
    kind: MastodonKind,
    limit: usize,
    local: bool,
    only_media: bool,
}

impl MastodonUriSpec {
    /// Builds the full Mastodon REST API v1 request URL for this spec. The
    /// `?auth=` credentials-selector query key is never forwarded here — it
    /// is consumed by [`resolve_service_key`] before this is called.
    fn endpoint_url(&self) -> String {
        match self.kind {
            MastodonKind::Home => format!(
                "https://{}/api/v1/timelines/home?limit={}",
                self.instance, self.limit
            ),
            MastodonKind::Public => format!(
                "https://{}/api/v1/timelines/public?local={}&only_media={}&limit={}",
                self.instance, self.local, self.only_media, self.limit
            ),
        }
    }
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`MastodonUriSpec`]. See the module-level "URI grammar" section
/// for the exact rules and failure conditions.
fn parse_mastodon_uri(uri: &WireUri) -> WireResult<MastodonUriSpec> {
    let instance = uri
        .host()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            WireError::Storage(format!(
                "mastodon adapter: missing instance (host) in '{}'",
                uri.as_raw()
            ))
        })?
        .to_string();

    let segments: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    let kind = match segments.as_slice() {
        ["timelines", "home"] => MastodonKind::Home,
        ["timelines", "public"] => MastodonKind::Public,
        _ => {
            return Err(WireError::Storage(format!(
                "mastodon adapter: invalid path '{}' in '{}' (expected /timelines/home or /timelines/public)",
                uri.path(),
                uri.as_raw()
            )));
        }
    };

    let limit = parse_limit(uri.query_get("limit"))?;

    // `local` / `only_media` only apply to `timelines/public`; for
    // `timelines/home` they are silently ignored (not even validated), same
    // convention as `state` for `kind=releases` in the github adapter.
    let (local, only_media) = if kind == MastodonKind::Public {
        (
            parse_bool_query(uri.query_get("local"), false, "local")?,
            parse_bool_query(uri.query_get("only_media"), false, "only_media")?,
        )
    } else {
        (false, false)
    };

    Ok(MastodonUriSpec {
        instance,
        kind,
        limit,
        local,
        only_media,
    })
}

/// Parse and validate the `?limit=` query value. An oversized value is
/// clamped to [`MASTODON_LIMIT_MAX`] (with a `tracing::warn!`), not
/// rejected; see the module docs "URI grammar".
fn parse_limit(raw: Option<&str>) -> WireResult<usize> {
    match raw {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "mastodon adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "mastodon adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            if n > MASTODON_LIMIT_MAX {
                tracing::warn!(
                    requested = n,
                    clamped_to = MASTODON_LIMIT_MAX,
                    "mastodon adapter: requested limit exceeds Mastodon's per-request max; clamping"
                );
                return Ok(MASTODON_LIMIT_MAX);
            }
            Ok(n)
        }
        None => Ok(DEFAULT_LIMIT),
    }
}

/// Parse and validate a `"true"` / `"false"` query value (used for `local`
/// and `only_media`). Absent defaults to `default`; any other value fails
/// loud, naming the offending query key.
fn parse_bool_query(raw: Option<&str>, default: bool, name: &str) -> WireResult<bool> {
    match raw {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(bad) => Err(WireError::Storage(format!(
            "mastodon adapter: invalid {name} '{bad}' (must be 'true' or 'false')"
        ))),
    }
}

/// Resolves the credentials service key for a `mastodon://` fetch: the
/// `?auth=<key>` query param overrides `default_key` for callers managing
/// tokens for multiple instances (e.g. `?auth=work` resolves the `work`
/// service key's token instead of `default_key`'s). Absent or empty
/// `?auth=` uses `default_key` unchanged. This key is consumed here — it
/// never reaches the upstream request URL (see
/// [`MastodonUriSpec::endpoint_url`]).
fn resolve_service_key<'a>(uri: &'a WireUri, default_key: &'a str) -> &'a str {
    uri.query_get("auth")
        .filter(|k| !k.is_empty())
        .unwrap_or(default_key)
}

/// Builds a `service_key`-scoped [`HttpClient`], auth-gated per `kind` (see
/// the module docs "Auth"): `Home` requires a resolved token and fails loud
/// otherwise; `Public` falls back to an unauthenticated client when no token
/// resolves. A backend error while resolving the token (not merely "no
/// token configured") always propagates, for both kinds.
fn mastodon_http_client(service_key: &str, kind: MastodonKind) -> WireResult<HttpClient> {
    let token = Credentials::default_chain().get(service_key)?;
    let client = HttpClient::new("mastodon adapter").with_timeout(FETCH_TIMEOUT);
    match (token, kind) {
        (Some(token), _) => Ok(client.with_bearer(token)),
        (None, MastodonKind::Home) => Err(WireError::Storage(missing_token_msg(
            service_key,
            MastodonKind::Home,
        ))),
        (None, MastodonKind::Public) => {
            tracing::info!(
                service_key = %service_key,
                "mastodon adapter: no token resolved for '{}'; proceeding unauthenticated (public timeline)",
                service_key
            );
            Ok(client)
        }
    }
}

/// Builds the fail-loud "no token" error message for `service_key`,
/// matching the wording convention of the other auth-required adapters
/// (`-notion` / `-slack` / `-todoist`): names the primary
/// `PERSONA_WIRE_TOKEN_<SERVICE_KEY>` env var, the `MASTODON_ACCESS_TOKEN`
/// alias when `service_key` is the crate-wide default, and the
/// `persona-wire token set <service_key>` keychain-storage command.
fn missing_token_msg(service_key: &str, kind: MastodonKind) -> String {
    let primary_var = format!(
        "PERSONA_WIRE_TOKEN_{}",
        service_key.to_uppercase().replace('-', "_")
    );
    if service_key == DEFAULT_SERVICE_KEY {
        format!(
            "mastodon adapter: no token found for '{service_key}' ({} requires auth; set {primary_var} / MASTODON_ACCESS_TOKEN, or run 'persona-wire token set {service_key}')",
            kind.as_str()
        )
    } else {
        format!(
            "mastodon adapter: no token found for '{service_key}' ({} requires auth; set {primary_var}, or run 'persona-wire token set {service_key}')",
            kind.as_str()
        )
    }
}

/// Wraps the raw Mastodon REST API v1 response (`raw`, expected to be a JSON
/// array) into the Wire JSON shape (see module docs "Output shape") without
/// re-shaping individual items. Fails loud, naming the instance and kind,
/// when `raw` isn't a JSON array.
fn shape_response(spec: &MastodonUriSpec, raw: serde_json::Value) -> WireResult<serde_json::Value> {
    let items = raw.as_array().cloned().ok_or_else(|| {
        WireError::Storage(format!(
            "mastodon adapter: unexpected response shape for {} ({}): expected a JSON array",
            spec.instance,
            spec.kind.as_str()
        ))
    })?;
    Ok(serde_json::json!({
        "instance": spec.instance,
        "kind": spec.kind.as_str(),
        "items": items,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_mastodon_uri ----

    fn parse(uri: &str) -> WireResult<MastodonUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_mastodon_uri(&wire)
    }

    #[test]
    fn parse_mastodon_uri_home_default() {
        let spec = parse("mastodon://mastodon.social/timelines/home").unwrap();
        assert_eq!(spec.instance, "mastodon.social");
        assert_eq!(spec.kind, MastodonKind::Home);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_mastodon_uri_public_default() {
        let spec = parse("mastodon://fosstodon.org/timelines/public").unwrap();
        assert_eq!(spec.kind, MastodonKind::Public);
        assert!(!spec.local, "local defaults to false");
        assert!(!spec.only_media, "only_media defaults to false");
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_mastodon_uri_public_local_true() {
        let spec = parse("mastodon://fosstodon.org/timelines/public?local=true").unwrap();
        assert!(spec.local);
    }

    #[test]
    fn parse_mastodon_uri_public_only_media_true() {
        let spec = parse("mastodon://fosstodon.org/timelines/public?only_media=true").unwrap();
        assert!(spec.only_media);
    }

    #[test]
    fn parse_mastodon_uri_limit_clamped_above_max() {
        let spec = parse("mastodon://mastodon.social/timelines/home?limit=41").unwrap();
        assert_eq!(spec.limit, MASTODON_LIMIT_MAX, "41 clamps down to 40");
    }

    #[test]
    fn parse_mastodon_uri_limit_at_max_not_clamped() {
        let spec = parse("mastodon://mastodon.social/timelines/home?limit=40").unwrap();
        assert_eq!(spec.limit, 40);
    }

    #[test]
    fn parse_mastodon_uri_invalid_path_fails_loud() {
        let err = parse("mastodon://mastodon.social/statuses").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid path"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_mastodon_uri_missing_instance_fails_loud() {
        let err = parse("mastodon:///timelines/home").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing instance"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_mastodon_uri_limit_non_numeric_fails_loud() {
        let err = parse("mastodon://mastodon.social/timelines/home?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_mastodon_uri_limit_zero_fails_loud() {
        let err = parse("mastodon://mastodon.social/timelines/home?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_mastodon_uri_public_invalid_local_fails_loud() {
        let err = parse("mastodon://fosstodon.org/timelines/public?local=yes").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid local"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_mastodon_uri_home_ignores_local_and_only_media() {
        // `local` / `only_media` are silently ignored (not even validated)
        // for `timelines/home`, per module docs.
        let spec = parse("mastodon://mastodon.social/timelines/home?local=bogus&only_media=bogus")
            .unwrap();
        assert!(!spec.local);
        assert!(!spec.only_media);
    }

    #[test]
    fn parse_mastodon_uri_unknown_query_key_ignored() {
        let spec = parse("mastodon://mastodon.social/timelines/home?utm_source=foo").unwrap();
        assert_eq!(spec.instance, "mastodon.social");
        assert_eq!(spec.kind, MastodonKind::Home);
    }

    #[test]
    fn parse_mastodon_uri_auth_param_passthrough() {
        // `?auth=` is a recognized key but is not validated by
        // `parse_mastodon_uri` itself (consumed separately by
        // `resolve_service_key`) — it must not cause a parse failure.
        let spec = parse("mastodon://mastodon.social/timelines/home?auth=work&limit=5").unwrap();
        assert_eq!(spec.limit, 5);
    }

    // ---- endpoint_url ----

    #[test]
    fn endpoint_url_home_shape() {
        let spec = parse("mastodon://mastodon.social/timelines/home?limit=5").unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://mastodon.social/api/v1/timelines/home?limit=5"
        );
    }

    #[test]
    fn endpoint_url_public_shape() {
        let spec =
            parse("mastodon://fosstodon.org/timelines/public?local=true&only_media=false&limit=10")
                .unwrap();
        assert_eq!(
            spec.endpoint_url(),
            "https://fosstodon.org/api/v1/timelines/public?local=true&only_media=false&limit=10"
        );
    }

    #[test]
    fn endpoint_url_never_includes_auth_param() {
        let spec = parse("mastodon://mastodon.social/timelines/home?auth=work&limit=5").unwrap();
        assert!(
            !spec.endpoint_url().contains("auth"),
            "auth is consumed by resolve_service_key, never forwarded upstream"
        );
    }

    // ---- resolve_service_key ----

    #[test]
    fn resolve_service_key_default_when_absent() {
        let wire = WireUri::parse("mastodon://mastodon.social/timelines/home").unwrap();
        assert_eq!(resolve_service_key(&wire, "mastodon"), "mastodon");
    }

    #[test]
    fn resolve_service_key_override_from_auth_param() {
        let wire = WireUri::parse("mastodon://mastodon.social/timelines/home?auth=work").unwrap();
        assert_eq!(resolve_service_key(&wire, "mastodon"), "work");
    }

    #[test]
    fn resolve_service_key_empty_auth_falls_back_to_default() {
        let wire = WireUri::parse("mastodon://mastodon.social/timelines/home?auth=").unwrap();
        assert_eq!(resolve_service_key(&wire, "mastodon"), "mastodon");
    }

    // ---- shape_response ----

    #[test]
    fn shape_response_wraps_raw_items_unmodified() {
        let spec = parse("mastodon://mastodon.social/timelines/home").unwrap();
        let raw = serde_json::json!([{ "id": "1", "content": "hello" }]);
        let v = shape_response(&spec, raw.clone()).unwrap();
        assert_eq!(v["instance"].as_str().unwrap(), "mastodon.social");
        assert_eq!(v["kind"].as_str().unwrap(), "timelines/home");
        assert_eq!(v["items"], raw, "items passed through unmodified");
    }

    #[test]
    fn shape_response_non_array_fails_loud() {
        let spec = parse("mastodon://mastodon.social/timelines/home").unwrap();
        let raw = serde_json::json!({ "error": "not found" });
        let err = shape_response(&spec, raw).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("expected a JSON array"),
            "unexpected error: {msg}"
        );
    }

    // ---- missing_token_msg ----

    #[test]
    fn missing_token_msg_default_key_includes_alias() {
        let msg = missing_token_msg("mastodon", MastodonKind::Home);
        assert!(msg.contains("PERSONA_WIRE_TOKEN_MASTODON"));
        assert!(msg.contains("MASTODON_ACCESS_TOKEN"));
        assert!(msg.contains("persona-wire token set mastodon"));
    }

    #[test]
    fn missing_token_msg_override_key_no_alias() {
        let msg = missing_token_msg("work", MastodonKind::Home);
        assert!(msg.contains("PERSONA_WIRE_TOKEN_WORK"));
        assert!(
            !msg.contains("MASTODON_ACCESS_TOKEN"),
            "override key has no conventional alias"
        );
        assert!(msg.contains("persona-wire token set work"));
    }
}
