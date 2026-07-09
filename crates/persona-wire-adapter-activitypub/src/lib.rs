//! persona-wire Adapter for the Fediverse (scheme `activitypub://`).
//!
//! ## Architecture
//!
//! `ActivityPubAdapter` is a stateless [`Adapter`] impl split into
//! independent functions, mirroring `persona-wire-adapter-rss`:
//!
//! - [`parse_activitypub_uri`] — `WireUri` → `ActivityPubUriSpec` (instance +
//!   user + output kind + item limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient`
//!   ([`fetch_actor`] for the actor document, [`fetch_outbox`] for the
//!   2-page outbox fetch), with `Accept: application/activity+json` on
//!   every request.
//! - [`normalize_actor`] / [`normalize_posts`] — raw ActivityStreams JSON →
//!   the Wire JSON shapes below.
//!
//! Only the **public**, unauthenticated surface of the ActivityPub protocol
//! is exercised: an actor's outbox and profile document. `follow` / write /
//! DM / private post / any authenticated action are out of MVP scope.
//!
//! Fediverse compat: any instance implementing the ActivityPub actor +
//! `OrderedCollection` outbox conventions (Mastodon, Misskey, Pleroma,
//! Firefish, and others) — no server-specific branching, only defensive
//! `serde_json::Value` field access since not every implementation matches
//! the spec byte-for-byte.
//!
//! ## URI grammar
//!
//! ```text
//! activitypub://<instance>/@<user>[?kind=profile|outbox][?limit=N]
//! ```
//!
//! - `instance` (the URI host) is the Fediverse instance hostname (e.g.
//!   `mastodon.social`); an empty host is an error.
//! - The path must be `/@<user>` (Mastodon handle convention); a missing
//!   `@` prefix is an error. Internally this resolves to the canonical
//!   ActivityPub actor URL `https://<instance>/users/<user>`.
//! - `?kind=` selects the output shape (default `outbox`): `outbox` (the
//!   actor's most recent public posts) or `profile` (actor metadata).
//! - `?limit=N` caps the number of posts returned (default
//!   [`DEFAULT_LIMIT`], `outbox` only). A non-numeric or zero value fails
//!   loud.
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss`).
//!
//! ## Output shape
//!
//! `?kind=outbox` (default):
//! ```json
//! {
//!   "kind": "outbox",
//!   "actor": { "url": "...", "handle": "@user@instance" },
//!   "posts": [
//!     { "id": "...", "content": "...|null", "published": "<RFC3339>|null",
//!       "url": "...|null", "attachments": [{ "type": "...", "url": "..." }] }
//!   ]
//! }
//! ```
//!
//! `?kind=profile`:
//! ```json
//! {
//!   "kind": "profile",
//!   "actor": { "url": "...", "handle": "...", "name": "...|null",
//!     "summary": "...|null", "followers_url": "...|null",
//!     "following_url": "...|null" }
//! }
//! ```
//!
//! HTML `content` / `summary` are passed through undecoded and unsanitized
//! (the caller's responsibility); no length truncation is applied (Mastodon
//! caps posts at 500 chars, Misskey at 3000 — truncating here would
//! silently disagree with the source instance).

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `posts` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-transport-http::DEFAULT_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// persona-wire Adapter for the Fediverse (`activitypub://` scheme).
pub struct ActivityPubAdapter;

#[async_trait]
impl Adapter for ActivityPubAdapter {
    fn scheme(&self) -> &'static str {
        "activitypub"
    }

    /// Fetch the actor at the URL derived from `uri`, then either normalize
    /// the actor profile (`?kind=profile`) or fetch + normalize its outbox
    /// (`?kind=outbox`, default).
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_activitypub_uri(uri)?;
        let client = HttpClient::new("activitypub adapter")
            .with_timeout(FETCH_TIMEOUT)
            .with_header("Accept", "application/activity+json");
        let actor_url = format!("https://{}/users/{}", spec.instance, spec.user);
        let actor_json = fetch_actor(&client, &actor_url).await?;
        let actor = normalize_actor(&actor_json);

        match spec.kind {
            Kind::Profile => Ok(serde_json::json!({
                "kind": "profile",
                "actor": actor,
            })),
            Kind::Outbox => {
                let outbox_url = actor_json
                    .get("outbox")
                    .and_then(extract_url_field)
                    .ok_or_else(|| {
                        WireError::Storage(format!(
                            "activitypub adapter: actor json missing 'outbox' field: {actor_url}"
                        ))
                    })?;
                let items = fetch_outbox(&client, &outbox_url, spec.limit).await?;
                let posts = normalize_posts(&items, spec.limit);
                Ok(serde_json::json!({
                    "kind": "outbox",
                    "actor": {
                        "url": actor.get("url").cloned().unwrap_or(serde_json::Value::Null),
                        "handle": actor.get("handle").cloned().unwrap_or(serde_json::Value::Null),
                    },
                    "posts": posts,
                }))
            }
        }
    }
}

/// `?kind=` selector — which document to normalize and return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Actor's `OrderedCollection` outbox (recent public posts). Default.
    Outbox,
    /// Actor profile document (name / summary / follower counts).
    Profile,
}

/// Parsed `activitypub://` URI: instance host, user, output kind, and item
/// limit (see module docs for the grammar).
#[derive(Debug, Clone)]
struct ActivityPubUriSpec {
    instance: String,
    user: String,
    kind: Kind,
    limit: usize,
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into an [`ActivityPubUriSpec`].
///
/// - `host` (instance hostname) is required and must be non-empty.
/// - `path` must be `/@<user>`; a missing `@` prefix is an error (Mastodon
///   handle convention — see module docs).
/// - `?kind=` must be `outbox` (default) or `profile`; any other value
///   fails loud.
/// - `?limit=N` must parse as a positive integer; non-numeric or `0` fails
///   loud with [`WireError::Storage`].
fn parse_activitypub_uri(uri: &WireUri) -> WireResult<ActivityPubUriSpec> {
    let instance = uri.host().filter(|h| !h.is_empty()).ok_or_else(|| {
        WireError::Storage(format!(
            "activitypub adapter: missing host in '{}'",
            uri.as_raw()
        ))
    })?;

    let user = uri
        .path()
        .strip_prefix("/@")
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            WireError::Storage(format!(
                "activitypub adapter: path must be '/@<user>' in '{}'",
                uri.as_raw()
            ))
        })?;

    let kind = match uri.query_get("kind") {
        None | Some("outbox") => Kind::Outbox,
        Some("profile") => Kind::Profile,
        Some(other) => {
            return Err(WireError::Storage(format!(
                "activitypub adapter: unknown kind '{other}' (must be 'outbox' or 'profile')"
            )));
        }
    };

    let limit = match uri.query_get("limit") {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "activitypub adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "activitypub adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            n
        }
        None => DEFAULT_LIMIT,
    };

    Ok(ActivityPubUriSpec {
        instance: instance.to_string(),
        user: user.to_string(),
        kind,
        limit,
    })
}

/// `GET <actor_url>` and return the parsed actor JSON. Thin wrapper around
/// [`HttpClient::get_json`] kept as its own function so the actor fetch is
/// a distinct, independently-named step in the [`Adapter::fetch`] pipeline.
async fn fetch_actor(client: &HttpClient, actor_url: &str) -> WireResult<serde_json::Value> {
    client.get_json(actor_url).await
}

/// Fetch an actor's outbox: `GET <outbox_url>` (an ActivityStreams
/// `OrderedCollection`) to find its `first` page URL, then `GET` that page
/// (an `OrderedCollectionPage`) and return its `orderedItems` array, capped
/// at `limit` raw entries. No further pagination is followed (MVP —
/// `first` page only).
async fn fetch_outbox(
    client: &HttpClient,
    outbox_url: &str,
    limit: usize,
) -> WireResult<Vec<serde_json::Value>> {
    let outbox = client.get_json(outbox_url).await?;
    let first_url = outbox
        .get("first")
        .and_then(extract_url_field)
        .ok_or_else(|| {
            WireError::Storage(format!(
                "activitypub adapter: outbox missing 'first' page url: {outbox_url}"
            ))
        })?;
    let page = client.get_json(&first_url).await?;
    let items = page
        .get("orderedItems")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(items.into_iter().take(limit).collect())
}

/// Normalize an ActivityPub actor JSON document into the Wire `actor` shape
/// (`url` / `handle` / `name` / `summary` / `followers_url` /
/// `following_url`).
///
/// `url` and `handle` are derived from the actor's own `id` +
/// `preferredUsername` fields (not re-derived from the request URI), so the
/// canonical actor IRI an instance reports is what callers see even when it
/// differs from the `https://<instance>/users/<user>` convention this
/// adapter fetches. Missing fields are `null`.
fn normalize_actor(actor_json: &serde_json::Value) -> serde_json::Value {
    let url = actor_json.get("id").and_then(|v| v.as_str());
    let username = actor_json.get("preferredUsername").and_then(|v| v.as_str());
    let host = url.and_then(extract_host);
    let handle = match (username, host) {
        (Some(u), Some(h)) => Some(format!("@{u}@{h}")),
        _ => None,
    };

    serde_json::json!({
        "url": url,
        "handle": handle,
        "name": actor_json.get("name").and_then(|v| v.as_str()),
        "summary": actor_json.get("summary").and_then(|v| v.as_str()),
        "followers_url": actor_json.get("followers").and_then(extract_url_field),
        "following_url": actor_json.get("following").and_then(extract_url_field),
    })
}

/// Normalize outbox `orderedItems` into the Wire `posts` shape.
///
/// Only `Create` activities are kept — `Announce` (boost) and any other
/// activity type are skipped (MVP scope is the actor's own posts only, per
/// module docs). Within a kept `Create`, `object` must be an embedded JSON
/// object (not a bare IRI string); activities whose object was not
/// inlined by the source instance are skipped rather than erroring, since
/// resolving them would require an additional fetch out of MVP scope.
///
/// `id` is preferred from the wrapping activity (`activity.id`, matching
/// "activity id URL" in the output shape), falling back to `object.id`.
/// `attachments` defensively accepts either a bare URL string or a `Link`
/// object (`{"href": "..."}"`) per attachment entry, since not every
/// instance's JSON matches the spec byte-for-byte.
///
/// Results are capped at `limit` (applied after the `Create` filter, so the
/// count reflects actual returned posts, not raw outbox entries).
fn normalize_posts(items: &[serde_json::Value], limit: usize) -> Vec<serde_json::Value> {
    items
        .iter()
        .filter(|activity| activity.get("type").and_then(|v| v.as_str()) == Some("Create"))
        .filter_map(|activity| {
            let object = activity.get("object")?;
            if !object.is_object() {
                return None;
            }
            Some((activity, object))
        })
        .take(limit)
        .map(|(activity, object)| {
            let id = activity
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| object.get("id").and_then(|v| v.as_str()))
                .map(|s| s.to_string());
            let content = object
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let published = object
                .get("published")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let url = object.get("url").and_then(extract_url_field);
            let attachments = normalize_attachments(object);
            serde_json::json!({
                "id": id,
                "content": content,
                "published": published,
                "url": url,
                "attachments": attachments,
            })
        })
        .collect()
}

/// Normalize an object's `attachment` array into `[{ "type", "url" }]`.
/// Entries without a resolvable `url` are skipped; `type` defaults to
/// `"Document"` when absent (matches the ActivityStreams base type).
fn normalize_attachments(object: &serde_json::Value) -> Vec<serde_json::Value> {
    object
        .get("attachment")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|att| {
                    let url = att.get("url").and_then(extract_url_field)?;
                    let ty = att
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Document")
                        .to_string();
                    Some(serde_json::json!({ "type": ty, "url": url }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Defensively extract a URL string out of a `serde_json::Value` that may be
/// a bare string, an array (first resolvable entry wins — e.g. multiple
/// `Link` objects), or a `Link`-shaped object (`{"href": "..."}"`). Returns
/// `None` when none of these shapes match, rather than erroring — not every
/// instance's JSON matches the spec byte-for-byte.
fn extract_url_field(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => arr.iter().find_map(extract_url_field),
        serde_json::Value::Object(map) => map
            .get("href")
            .and_then(|h| h.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Extract the host component out of an absolute URL string (e.g.
/// `https://mastodon.social/users/alice` → `Some("mastodon.social")`), via
/// plain string splitting (no `url` crate dependency — this adapter has no
/// other need for full URL parsing).
fn extract_host(url_str: &str) -> Option<&str> {
    let after_scheme = url_str.split_once("://")?.1;
    Some(match after_scheme.find('/') {
        Some(idx) => &after_scheme[..idx],
        None => after_scheme,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_activitypub_uri ----

    fn parse(uri: &str) -> WireResult<ActivityPubUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_activitypub_uri(&wire)
    }

    #[test]
    fn parse_activitypub_uri_at_user_path_ok() {
        let spec = parse("activitypub://mastodon.social/@alice").unwrap();
        assert_eq!(spec.instance, "mastodon.social");
        assert_eq!(spec.user, "alice");
        assert_eq!(spec.kind, Kind::Outbox);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_activitypub_uri_empty_host_errors() {
        let err = parse("activitypub:///@alice").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing host"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_activitypub_uri_missing_at_prefix_errors() {
        let err = parse("activitypub://mastodon.social/alice").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("must be '/@<user>'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_activitypub_uri_kind_profile() {
        let spec = parse("activitypub://mastodon.social/@alice?kind=profile").unwrap();
        assert_eq!(spec.kind, Kind::Profile);
    }

    #[test]
    fn parse_activitypub_uri_kind_outbox_default() {
        let spec = parse("activitypub://mastodon.social/@alice?kind=outbox").unwrap();
        assert_eq!(spec.kind, Kind::Outbox);
    }

    #[test]
    fn parse_activitypub_uri_unknown_kind_errors() {
        let err = parse("activitypub://mastodon.social/@alice?kind=xxx").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_activitypub_uri_limit_override() {
        let spec = parse("activitypub://mastodon.social/@alice?limit=5").unwrap();
        assert_eq!(spec.limit, 5);
    }

    #[test]
    fn parse_activitypub_uri_limit_non_numeric_errors() {
        let err = parse("activitypub://mastodon.social/@alice?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_activitypub_uri_limit_zero_errors() {
        let err = parse("activitypub://mastodon.social/@alice?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_activitypub_uri_unknown_query_key_ignored() {
        let spec = parse("activitypub://mastodon.social/@alice?utm_source=foo").unwrap();
        assert_eq!(spec.kind, Kind::Outbox);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_activitypub_uri_combined_kind_and_limit() {
        let spec = parse("activitypub://mastodon.social/@alice?kind=profile&limit=3").unwrap();
        assert_eq!(spec.kind, Kind::Profile);
        assert_eq!(spec.limit, 3);
    }

    // ---- normalize_actor ----

    const ACTOR_FIXTURE: &str = r#"{
        "id": "https://mastodon.social/users/alice",
        "preferredUsername": "alice",
        "name": "Alice",
        "summary": "<p>bio</p>",
        "followers": "https://mastodon.social/users/alice/followers",
        "following": "https://mastodon.social/users/alice/following"
    }"#;

    #[test]
    fn normalize_actor_full_shape() {
        let actor_json: serde_json::Value = serde_json::from_str(ACTOR_FIXTURE).unwrap();
        let v = normalize_actor(&actor_json);
        assert_eq!(v["url"], "https://mastodon.social/users/alice");
        assert_eq!(v["handle"], "@alice@mastodon.social");
        assert_eq!(v["name"], "Alice");
        assert_eq!(v["summary"], "<p>bio</p>");
        assert_eq!(
            v["followers_url"],
            "https://mastodon.social/users/alice/followers"
        );
        assert_eq!(
            v["following_url"],
            "https://mastodon.social/users/alice/following"
        );
    }

    #[test]
    fn normalize_actor_missing_fields_are_null() {
        let actor_json: serde_json::Value = serde_json::from_str(
            r#"{ "id": "https://mastodon.social/users/bob", "preferredUsername": "bob" }"#,
        )
        .unwrap();
        let v = normalize_actor(&actor_json);
        assert_eq!(v["handle"], "@bob@mastodon.social");
        assert!(v["name"].is_null());
        assert!(v["summary"].is_null());
        assert!(v["followers_url"].is_null());
        assert!(v["following_url"].is_null());
    }

    #[test]
    fn normalize_actor_missing_id_or_username_yields_null_handle() {
        let actor_json: serde_json::Value = serde_json::from_str(r#"{ "name": "No Id" }"#).unwrap();
        let v = normalize_actor(&actor_json);
        assert!(v["url"].is_null());
        assert!(v["handle"].is_null());
    }

    // ---- normalize_posts ----

    fn create_note(id: &str, content: &str, url: &str, attachments: &str) -> String {
        format!(
            r#"{{
                "type": "Create",
                "id": "{id}",
                "object": {{
                    "type": "Note",
                    "content": "{content}",
                    "published": "2024-01-01T00:00:00Z",
                    "url": "{url}",
                    "attachment": {attachments}
                }}
            }}"#
        )
    }

    #[test]
    fn normalize_posts_extracts_create_note_fields() {
        let raw = create_note(
            "https://mastodon.social/users/alice/statuses/1/activity",
            "hello world",
            "https://mastodon.social/@alice/1",
            "[]",
        );
        let items: Vec<serde_json::Value> = vec![serde_json::from_str(&raw).unwrap()];
        let posts = normalize_posts(&items, DEFAULT_LIMIT);
        assert_eq!(posts.len(), 1);
        assert_eq!(
            posts[0]["id"],
            "https://mastodon.social/users/alice/statuses/1/activity"
        );
        assert_eq!(posts[0]["content"], "hello world");
        assert_eq!(posts[0]["published"], "2024-01-01T00:00:00Z");
        assert_eq!(posts[0]["url"], "https://mastodon.social/@alice/1");
        assert_eq!(posts[0]["attachments"], serde_json::json!([]));
    }

    #[test]
    fn normalize_posts_extracts_attachments() {
        let raw = create_note(
            "id-1",
            "with image",
            "https://x/1",
            r#"[{"type":"Image","url":"https://x/img.png"}]"#,
        );
        let items: Vec<serde_json::Value> = vec![serde_json::from_str(&raw).unwrap()];
        let posts = normalize_posts(&items, DEFAULT_LIMIT);
        let attachments = posts[0]["attachments"].as_array().unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0]["type"], "Image");
        assert_eq!(attachments[0]["url"], "https://x/img.png");
    }

    #[test]
    fn normalize_posts_skips_announce_activities() {
        let create = create_note("id-create", "own post", "https://x/1", "[]");
        let announce = r#"{
            "type": "Announce",
            "id": "id-announce",
            "object": "https://other.instance/users/bob/statuses/2"
        }"#;
        let items: Vec<serde_json::Value> = vec![
            serde_json::from_str(&create).unwrap(),
            serde_json::from_str(announce).unwrap(),
        ];
        let posts = normalize_posts(&items, DEFAULT_LIMIT);
        assert_eq!(
            posts.len(),
            1,
            "Announce is skipped, only the Create survives"
        );
        assert_eq!(posts[0]["id"], "id-create");
    }

    #[test]
    fn normalize_posts_limit_truncates() {
        let items: Vec<serde_json::Value> = (0..5)
            .map(|i| {
                serde_json::from_str(&create_note(
                    &format!("id-{i}"),
                    "post",
                    "https://x/p",
                    "[]",
                ))
                .unwrap()
            })
            .collect();
        let posts = normalize_posts(&items, 2);
        assert_eq!(posts.len(), 2, "limit=2 truncates to two posts");
        assert_eq!(posts[0]["id"], "id-0");
        assert_eq!(posts[1]["id"], "id-1");
    }

    #[test]
    fn normalize_posts_non_embedded_object_is_skipped() {
        let raw = r#"{
            "type": "Create",
            "id": "id-bare",
            "object": "https://x/note-not-embedded"
        }"#;
        let items: Vec<serde_json::Value> = vec![serde_json::from_str(raw).unwrap()];
        let posts = normalize_posts(&items, DEFAULT_LIMIT);
        assert!(
            posts.is_empty(),
            "non-embedded (bare IRI) object is skipped, not fetched"
        );
    }

    #[test]
    fn normalize_posts_missing_optional_fields_are_null() {
        let raw = r#"{
            "type": "Create",
            "id": "id-minimal",
            "object": { "type": "Note" }
        }"#;
        let items: Vec<serde_json::Value> = vec![serde_json::from_str(raw).unwrap()];
        let posts = normalize_posts(&items, DEFAULT_LIMIT);
        assert_eq!(posts.len(), 1);
        assert!(posts[0]["content"].is_null());
        assert!(posts[0]["published"].is_null());
        assert!(posts[0]["url"].is_null());
        assert_eq!(posts[0]["attachments"], serde_json::json!([]));
    }

    // ---- extract_url_field / extract_host ----

    #[test]
    fn extract_url_field_string() {
        assert_eq!(
            extract_url_field(&serde_json::json!("https://x/1")),
            Some("https://x/1".to_string())
        );
    }

    #[test]
    fn extract_url_field_link_object() {
        assert_eq!(
            extract_url_field(&serde_json::json!({ "type": "Link", "href": "https://x/1" })),
            Some("https://x/1".to_string())
        );
    }

    #[test]
    fn extract_url_field_array_of_link_objects() {
        assert_eq!(
            extract_url_field(&serde_json::json!([
                { "type": "Link", "href": "https://x/1", "mediaType": "text/html" }
            ])),
            Some("https://x/1".to_string())
        );
    }

    #[test]
    fn extract_url_field_unrecognized_shape_returns_none() {
        assert_eq!(extract_url_field(&serde_json::json!(42)), None);
    }

    #[test]
    fn extract_host_from_absolute_url() {
        assert_eq!(
            extract_host("https://mastodon.social/users/alice"),
            Some("mastodon.social")
        );
    }

    #[test]
    fn extract_host_no_path() {
        assert_eq!(
            extract_host("https://mastodon.social"),
            Some("mastodon.social")
        );
    }

    #[test]
    fn extract_host_no_scheme_returns_none() {
        assert_eq!(extract_host("mastodon.social/users/alice"), None);
    }
}
