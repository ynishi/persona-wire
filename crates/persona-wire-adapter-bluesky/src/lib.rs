//! persona-wire Adapter for Bluesky (scheme `bluesky://`).
//!
//! ## Architecture
//!
//! `BlueskyAdapter` is a stateless [`Adapter`] impl split into independent
//! functions, mirroring `persona-wire-adapter-activitypub` /
//! `persona-wire-adapter-rss`:
//!
//! - [`parse_bluesky_uri`] — `WireUri` → `BlueskyUriSpec` (actor + output
//!   kind + item limit + optional post rkey).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient`
//!   ([`fetch_author_feed`] / [`fetch_profile`] / [`fetch_post_thread`]),
//!   each hitting a single unauthenticated AT Protocol XRPC endpoint on the
//!   public AppView.
//! - [`normalize_feed`] / [`normalize_profile`] / [`normalize_thread`] — raw
//!   `app.bsky.*` lexicon JSON → the Wire JSON shapes below.
//!
//! Only the **public**, unauthenticated AppView surface is exercised: an
//! actor's author feed, profile, and a single post's thread. Authenticated
//! endpoints (home timeline / DMs / follow / write / OAuth) are out of MVP
//! scope.
//!
//! ## AT Protocol glossary
//!
//! - **DID** (`did:plc:...`) — an actor's permanent, non-human-readable
//!   identifier; never changes even if the handle does.
//! - **Handle** (`alice.bsky.social`) — an actor's human-readable,
//!   DNS-backed name; resolves to a DID.
//! - **rkey** — the trailing path segment of a record's URI (e.g. the
//!   `3jzfc...` in `at://did:plc:xxx/app.bsky.feed.post/3jzfc...`), a
//!   per-collection-per-repo unique key.
//! - **at URI** — `at://<did-or-handle>/<collection>/<rkey>`, the
//!   canonical address of a single record inside a repo.
//! - **XRPC** — AT Protocol's HTTP RPC convention: every endpoint is
//!   `GET/POST /xrpc/<lexicon-id>` (e.g. `app.bsky.feed.getAuthorFeed`).
//! - **AppView** — a service (here, the public `public.api.bsky.app`
//!   instance) that aggregates raw repo records into query-friendly views
//!   (feeds, profiles, threads) without requiring auth.
//!
//! ## URI grammar
//!
//! ```text
//! bluesky://<actor>[?kind=feed|profile|thread][?limit=N][?post=<rkey>]
//! ```
//!
//! - `actor` (the URI host) is a Bluesky handle (e.g. `bsky.app`) or DID
//!   (`did:plc:xxx`); an empty host is an error.
//! - `?kind=` selects the output shape (default `feed`):
//!   - `feed` = `getAuthorFeed` — the actor's most recent posts (including
//!     reposts and replies).
//!   - `profile` = `getProfile` — actor metadata (display name / bio /
//!     follower counts).
//!   - `thread` = `getPostThread` — a single post plus its direct replies;
//!     requires `?post=<rkey>`.
//! - `?limit=N` caps the number of feed entries returned (default
//!   [`DEFAULT_LIMIT`], `feed` only; parsed and range-checked for every
//!   `kind`, matching `persona-wire-adapter-activitypub`'s convention). A
//!   non-numeric, zero, or over-[`MAX_LIMIT`] value fails loud.
//! - `?post=<rkey>` supplies the target post's rkey for `?kind=thread`
//!   (required — missing it is an error); silently ignored for any other
//!   `kind`.
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss` /
//!   `persona-wire-adapter-activitypub`).
//!
//! ## Output shape
//!
//! `?kind=feed` (default):
//! ```json
//! {
//!   "kind": "feed",
//!   "actor": "<handle or did>",
//!   "posts": [
//!     { "uri": "...", "cid": "...",
//!       "author": { "handle": "...", "did": "...", "displayName": "...|null" },
//!       "text": "...", "created_at": "<RFC3339>|null",
//!       "reply_count": 0, "repost_count": 0, "like_count": 0,
//!       "is_repost": false, "is_reply": false }
//!   ]
//! }
//! ```
//!
//! `?kind=profile`:
//! ```json
//! {
//!   "kind": "profile",
//!   "actor": { "handle": "...", "did": "...", "displayName": "...|null",
//!     "description": "...|null", "followers_count": 0, "follows_count": 0,
//!     "posts_count": 0 }
//! }
//! ```
//!
//! `?kind=thread`:
//! ```json
//! {
//!   "kind": "thread",
//!   "post": { "<same shape as a feed entry>" },
//!   "replies": [ { "<same shape as a feed entry>" } ]
//! }
//! ```
//!
//! `replies` is one level deep only — nested replies-of-replies are dropped,
//! not flattened. Missing numeric fields default to `0` (matching the
//! Bluesky API's own behavior); missing string fields are `null`.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{FilterCap, WireError, WireFilters, WireResult};
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `posts` cap when `?limit=` is absent from the URI (`feed` only).
pub const DEFAULT_LIMIT: usize = 30;

/// Maximum accepted `?limit=` value — matches `getAuthorFeed`'s real-world
/// API-side cap.
pub const MAX_LIMIT: usize = 100;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-transport-http::DEFAULT_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Base URL of the public, unauthenticated AT Protocol AppView.
pub const APPVIEW_BASE: &str = "https://public.api.bsky.app";

/// persona-wire Adapter for Bluesky (`bluesky://` scheme).
pub struct BlueskyAdapter;

#[async_trait]
impl Adapter for BlueskyAdapter {
    fn scheme(&self) -> &'static str {
        "bluesky"
    }

    fn filter_caps(&self) -> &'static [FilterCap] {
        &[FilterCap::Limit {
            max: Some(MAX_LIMIT),
        }]
    }

    /// Dispatch on `?kind=` to the matching XRPC fetch + normalize pair.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let filters = WireFilters::parse(uri, self.filter_caps())?;
        let spec = parse_bluesky_uri(uri, filters.limit.unwrap_or(DEFAULT_LIMIT))?;
        let client = HttpClient::new("bluesky adapter").with_timeout(FETCH_TIMEOUT);

        match spec.kind {
            Kind::Feed => {
                let feed_json = fetch_author_feed(&client, &spec.actor, spec.limit).await?;
                Ok(normalize_feed(&feed_json, &spec.actor, spec.limit))
            }
            Kind::Profile => {
                let profile_json = fetch_profile(&client, &spec.actor).await?;
                Ok(normalize_profile(&profile_json))
            }
            Kind::Thread => {
                // parse_bluesky_uri already rejects Kind::Thread without a
                // post_rkey, so this is an internal invariant, not a
                // caller-facing error path.
                let rkey = spec.post_rkey.as_deref().ok_or_else(|| {
                    WireError::Storage(
                        "bluesky adapter: internal error: thread kind without post_rkey"
                            .to_string(),
                    )
                })?;
                let at_uri = format!("at://{}/app.bsky.feed.post/{}", spec.actor, rkey);
                let thread_json = fetch_post_thread(&client, &at_uri).await?;
                Ok(normalize_thread(&thread_json))
            }
        }
    }
}

/// `?kind=` selector — which XRPC endpoint to fetch and how to normalize it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `getAuthorFeed` — the actor's recent posts. Default.
    Feed,
    /// `getProfile` — actor metadata.
    Profile,
    /// `getPostThread` — a single post plus its direct replies.
    Thread,
}

/// Parsed `bluesky://` URI: actor (handle or DID), output kind, feed item
/// limit, and optional thread post rkey (see module docs for the grammar).
#[derive(Debug, Clone)]
struct BlueskyUriSpec {
    actor: String,
    kind: Kind,
    limit: usize,
    post_rkey: Option<String>,
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into a [`BlueskyUriSpec`].
///
/// - `host` (actor) is required and must be non-empty.
/// - `?kind=` must be `feed` (default), `profile`, or `thread`; any other
///   value fails loud.
/// - Cross-cutting filters (`?limit=N`) are parsed separately via
///   [`WireFilters::parse`] (declared cap `Limit { max: Some(MAX_LIMIT) }`)
///   and passed in as `limit`. `limit > MAX_LIMIT` is clamped to
///   [`MAX_LIMIT`] with a `tracing::warn!` (behavior change from earlier
///   versions where it hard-errored).
/// - `?post=<rkey>` is required when `kind=thread` (error if absent);
///   silently accepted-but-unused for any other `kind`.
fn parse_bluesky_uri(uri: &WireUri, limit: usize) -> WireResult<BlueskyUriSpec> {
    let actor = uri.host().filter(|h| !h.is_empty()).ok_or_else(|| {
        WireError::Storage(format!(
            "bluesky adapter: missing host in '{}'",
            uri.as_raw()
        ))
    })?;

    let kind = match uri.query_get("kind") {
        None | Some("feed") => Kind::Feed,
        Some("profile") => Kind::Profile,
        Some("thread") => Kind::Thread,
        Some(other) => {
            return Err(WireError::Storage(format!(
                "bluesky adapter: unknown kind '{other}' (must be 'feed', 'profile', or 'thread')"
            )));
        }
    };

    let post_rkey = uri.query_get("post").map(|s| s.to_string());
    if kind == Kind::Thread && post_rkey.is_none() {
        return Err(WireError::Storage(format!(
            "bluesky adapter: 'kind=thread' requires '?post=<rkey>' in '{}'",
            uri.as_raw()
        )));
    }

    Ok(BlueskyUriSpec {
        actor: actor.to_string(),
        kind,
        limit,
        post_rkey,
    })
}

/// `GET /xrpc/app.bsky.feed.getAuthorFeed?actor=<actor>&limit=<limit>` and
/// return the parsed response JSON (`{"feed": [...], "cursor": "..."?}`).
async fn fetch_author_feed(
    client: &HttpClient,
    actor: &str,
    limit: usize,
) -> WireResult<serde_json::Value> {
    let mut url = url::Url::parse(&format!("{APPVIEW_BASE}/xrpc/app.bsky.feed.getAuthorFeed"))
        .expect("APPVIEW_BASE + endpoint is a valid URL");
    url.query_pairs_mut()
        .append_pair("actor", actor)
        .append_pair("limit", &limit.to_string());
    client.get_json(url.as_str()).await
}

/// `GET /xrpc/app.bsky.actor.getProfile?actor=<actor>` and return the parsed
/// response JSON (`ProfileViewDetailed`).
async fn fetch_profile(client: &HttpClient, actor: &str) -> WireResult<serde_json::Value> {
    let mut url = url::Url::parse(&format!("{APPVIEW_BASE}/xrpc/app.bsky.actor.getProfile"))
        .expect("APPVIEW_BASE + endpoint is a valid URL");
    url.query_pairs_mut().append_pair("actor", actor);
    client.get_json(url.as_str()).await
}

/// `GET /xrpc/app.bsky.feed.getPostThread?uri=<at_uri>` and return the
/// parsed response JSON (`{"thread": {...}}`).
async fn fetch_post_thread(client: &HttpClient, at_uri: &str) -> WireResult<serde_json::Value> {
    let mut url = url::Url::parse(&format!("{APPVIEW_BASE}/xrpc/app.bsky.feed.getPostThread"))
        .expect("APPVIEW_BASE + endpoint is a valid URL");
    url.query_pairs_mut().append_pair("uri", at_uri);
    client.get_json(url.as_str()).await
}

/// Normalize a `getAuthorFeed` response (`{"feed": [{"post": ..., "reason":
/// ...?, "reply": ...?}]}`) into the Wire `feed` shape. `limit` re-truncates
/// the already-API-limited `feed` array defensively (mirrors
/// `persona-wire-adapter-activitypub::normalize_posts`).
fn normalize_feed(feed_json: &serde_json::Value, actor: &str, limit: usize) -> serde_json::Value {
    let posts: Vec<serde_json::Value> = feed_json
        .get("feed")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().take(limit).map(normalize_feed_entry).collect())
        .unwrap_or_default();

    serde_json::json!({
        "kind": "feed",
        "actor": actor,
        "posts": posts,
    })
}

/// Normalize one `feed[]` entry (the `{"post": ..., "reason": ...?, "reply":
/// ...?}` wrapper) into a Wire `post` shape, deriving `is_repost` from a
/// `reason.$type == "app.bsky.feed.defs#reasonRepost"` sibling and
/// `is_reply` from the mere presence of a `reply` sibling.
fn normalize_feed_entry(entry: &serde_json::Value) -> serde_json::Value {
    let empty = serde_json::Value::Null;
    let post = entry.get("post").unwrap_or(&empty);
    let is_repost = entry
        .get("reason")
        .and_then(|r| r.get("$type"))
        .and_then(|v| v.as_str())
        == Some("app.bsky.feed.defs#reasonRepost");
    let is_reply = entry.get("reply").is_some();
    with_repost_reply_flags(normalize_post(post), is_repost, is_reply)
}

/// Normalize a raw `app.bsky.feed.defs#postView`-shaped JSON object into the
/// Wire `post` shape, minus `is_repost` / `is_reply` (those are only
/// derivable from the entry wrapper, not the post itself — see
/// [`normalize_feed_entry`] / [`with_repost_reply_flags`]).
fn normalize_post(post: &serde_json::Value) -> serde_json::Value {
    let author = post.get("author");
    let record = post.get("record");
    serde_json::json!({
        "uri": post.get("uri").and_then(|v| v.as_str()),
        "cid": post.get("cid").and_then(|v| v.as_str()),
        "author": {
            "handle": author.and_then(|a| a.get("handle")).and_then(|v| v.as_str()),
            "did": author.and_then(|a| a.get("did")).and_then(|v| v.as_str()),
            "displayName": author.and_then(|a| a.get("displayName")).and_then(|v| v.as_str()),
        },
        "text": record.and_then(|r| r.get("text")).and_then(|v| v.as_str()),
        "created_at": record.and_then(|r| r.get("createdAt")).and_then(|v| v.as_str()),
        "reply_count": post.get("replyCount").and_then(|v| v.as_i64()).unwrap_or(0),
        "repost_count": post.get("repostCount").and_then(|v| v.as_i64()).unwrap_or(0),
        "like_count": post.get("likeCount").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

/// Insert `is_repost` / `is_reply` into an already-built Wire `post` object
/// (as produced by [`normalize_post`]). `base` is always an `Object` here —
/// [`normalize_post`] only ever returns `serde_json::json!({ ... })`.
fn with_repost_reply_flags(
    base: serde_json::Value,
    is_repost: bool,
    is_reply: bool,
) -> serde_json::Value {
    let mut base = base;
    if let serde_json::Value::Object(map) = &mut base {
        map.insert("is_repost".to_string(), serde_json::json!(is_repost));
        map.insert("is_reply".to_string(), serde_json::json!(is_reply));
    }
    base
}

/// Normalize a `getProfile` response (`ProfileViewDetailed`) into the Wire
/// `profile` shape.
fn normalize_profile(profile_json: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "kind": "profile",
        "actor": {
            "handle": profile_json.get("handle").and_then(|v| v.as_str()),
            "did": profile_json.get("did").and_then(|v| v.as_str()),
            "displayName": profile_json.get("displayName").and_then(|v| v.as_str()),
            "description": profile_json.get("description").and_then(|v| v.as_str()),
            "followers_count": profile_json.get("followersCount").and_then(|v| v.as_i64()).unwrap_or(0),
            "follows_count": profile_json.get("followsCount").and_then(|v| v.as_i64()).unwrap_or(0),
            "posts_count": profile_json.get("postsCount").and_then(|v| v.as_i64()).unwrap_or(0),
        },
    })
}

/// Normalize a `getPostThread` response (`{"thread": {"post": ...,
/// "replies": [{"post": ...}, ...]}}`) into the Wire `thread` shape.
///
/// Only the root `post` and the *direct* (first-level) `replies[].post`
/// entries are extracted; each `replies[]` entry's own nested `replies`
/// array (a reply-of-a-reply) is ignored outright — not flattened into the
/// result (depth-1 truncation is intentional to bound response size).
/// Thread-view posts carry no `reason` / `reply` sibling (unlike feed
/// entries), so `is_repost` / `is_reply` are always `false` here.
fn normalize_thread(thread_json: &serde_json::Value) -> serde_json::Value {
    let thread = thread_json.get("thread");

    let post = thread
        .and_then(|t| t.get("post"))
        .map(|p| with_repost_reply_flags(normalize_post(p), false, false))
        .unwrap_or(serde_json::Value::Null);

    let replies: Vec<serde_json::Value> = thread
        .and_then(|t| t.get("replies"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|reply_node| reply_node.get("post"))
                .map(|p| with_repost_reply_flags(normalize_post(p), false, false))
                .collect()
        })
        .unwrap_or_default();

    serde_json::json!({
        "kind": "thread",
        "post": post,
        "replies": replies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_bluesky_uri ----

    /// Helper: parse with the adapter's default limit (backwards-compatible
    /// with the pre-Phase-2 default when no `?limit=` was supplied).
    fn parse(uri: &str) -> WireResult<BlueskyUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_bluesky_uri(&wire, DEFAULT_LIMIT)
    }

    /// Helper: parse with an explicit limit (simulates
    /// `filters.limit = Some(n)` after `WireFilters::parse`).
    fn parse_with_limit(uri: &str, limit: usize) -> WireResult<BlueskyUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_bluesky_uri(&wire, limit)
    }

    #[test]
    fn parse_bluesky_uri_handle_host_ok() {
        let spec = parse("bluesky://bsky.app").unwrap();
        assert_eq!(spec.actor, "bsky.app");
        assert_eq!(spec.kind, Kind::Feed);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
        assert!(spec.post_rkey.is_none());
    }

    #[test]
    fn parse_bluesky_uri_did_host_ok() {
        let spec = parse("bluesky://did:plc:z72i7hdynmk6r22z27h6tvur").unwrap();
        assert_eq!(spec.actor, "did:plc:z72i7hdynmk6r22z27h6tvur");
    }

    #[test]
    fn parse_bluesky_uri_empty_host_errors() {
        let err = parse("bluesky://").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing host"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_bluesky_uri_kind_feed_default() {
        let spec = parse("bluesky://bsky.app?kind=feed").unwrap();
        assert_eq!(spec.kind, Kind::Feed);
    }

    #[test]
    fn parse_bluesky_uri_kind_profile() {
        let spec = parse("bluesky://bsky.app?kind=profile").unwrap();
        assert_eq!(spec.kind, Kind::Profile);
    }

    #[test]
    fn parse_bluesky_uri_kind_thread_with_post() {
        let spec = parse("bluesky://bsky.app?kind=thread&post=3jzfcijpj2z2a").unwrap();
        assert_eq!(spec.kind, Kind::Thread);
        assert_eq!(spec.post_rkey, Some("3jzfcijpj2z2a".to_string()));
    }

    #[test]
    fn parse_bluesky_uri_unknown_kind_errors() {
        let err = parse("bluesky://bsky.app?kind=xxx").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unknown kind"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_bluesky_uri_limit_forwarded() {
        // parse_bluesky_uri now receives limit as a parameter; it just
        // forwards it (validation lives in WireFilters::parse).
        let spec = parse_with_limit("bluesky://bsky.app", 5).unwrap();
        assert_eq!(spec.limit, 5);
    }

    #[test]
    fn parse_bluesky_uri_thread_without_post_errors() {
        let err = parse("bluesky://bsky.app?kind=thread").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("requires '?post=<rkey>'"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_bluesky_uri_post_ignored_for_non_thread_kind() {
        let spec = parse("bluesky://bsky.app?kind=feed&post=abc").unwrap();
        assert_eq!(spec.kind, Kind::Feed);
        assert_eq!(spec.post_rkey, Some("abc".to_string()));
    }

    #[test]
    fn parse_bluesky_uri_unknown_query_key_ignored() {
        let spec = parse("bluesky://bsky.app?utm_source=foo").unwrap();
        assert_eq!(spec.kind, Kind::Feed);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    // ---- filter_caps + WireFilters integration (Phase 2 unified filter IF) ----

    fn parse_filters(uri: &str) -> WireResult<WireFilters> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        WireFilters::parse(&wire, BlueskyAdapter.filter_caps())
    }

    #[test]
    fn filter_caps_declares_limit_capped_at_max() {
        assert_eq!(
            BlueskyAdapter.filter_caps(),
            &[FilterCap::Limit {
                max: Some(MAX_LIMIT)
            }]
        );
    }

    #[test]
    fn wire_filters_limit_within_max() {
        let f = parse_filters("bluesky://bsky.app?limit=30").unwrap();
        assert_eq!(f.limit, Some(30));
    }

    #[test]
    fn wire_filters_limit_at_max_not_clamped() {
        let f = parse_filters("bluesky://bsky.app?limit=100").unwrap();
        assert_eq!(f.limit, Some(MAX_LIMIT));
    }

    #[test]
    fn wire_filters_limit_clamped_above_max() {
        // Behavior change (Phase 2): limit>100 previously errored;
        // now clamps to MAX_LIMIT with a `tracing::warn!`.
        let f = parse_filters("bluesky://bsky.app?limit=101").unwrap();
        assert_eq!(f.limit, Some(MAX_LIMIT), "101 clamps down to 100");
    }

    #[test]
    fn wire_filters_limit_non_numeric_errors() {
        let err = parse_filters("bluesky://bsky.app?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("invalid"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_filters_limit_zero_errors() {
        let err = parse_filters("bluesky://bsky.app?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("limit") && msg.contains("positive"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn wire_filters_undeclared_filter_key_errors() {
        let err = parse_filters("bluesky://bsky.app?query=x").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("query") && msg.contains("not supported"),
            "unexpected error: {msg}"
        );
    }

    // ---- normalize_feed ----

    const FEED_FIXTURE: &str = r#"{
        "feed": [
            {
                "post": {
                    "uri": "at://did:plc:alice/app.bsky.feed.post/1",
                    "cid": "cid-1",
                    "author": { "did": "did:plc:alice", "handle": "alice.bsky.social", "displayName": "Alice" },
                    "record": { "text": "hello world", "createdAt": "2024-01-01T00:00:00.000Z" },
                    "replyCount": 1,
                    "repostCount": 2,
                    "likeCount": 3
                }
            },
            {
                "post": {
                    "uri": "at://did:plc:bob/app.bsky.feed.post/2",
                    "cid": "cid-2",
                    "author": { "did": "did:plc:bob", "handle": "bob.bsky.social", "displayName": "Bob" },
                    "record": { "text": "reposted note", "createdAt": "2024-01-02T00:00:00.000Z" }
                },
                "reason": { "$type": "app.bsky.feed.defs#reasonRepost", "by": { "did": "did:plc:alice" } }
            },
            {
                "post": {
                    "uri": "at://did:plc:carol/app.bsky.feed.post/3",
                    "cid": "cid-3",
                    "author": { "did": "did:plc:carol", "handle": "carol.bsky.social", "displayName": "Carol" },
                    "record": { "text": "a reply", "createdAt": "2024-01-03T00:00:00.000Z" }
                },
                "reply": {
                    "root": { "uri": "at://did:plc:alice/app.bsky.feed.post/1", "cid": "cid-1" },
                    "parent": { "uri": "at://did:plc:alice/app.bsky.feed.post/1", "cid": "cid-1" }
                }
            }
        ]
    }"#;

    #[test]
    fn normalize_feed_extracts_actor_and_shape() {
        let feed_json: serde_json::Value = serde_json::from_str(FEED_FIXTURE).unwrap();
        let out = normalize_feed(&feed_json, "alice.bsky.social", DEFAULT_LIMIT);
        assert_eq!(out["kind"], "feed");
        assert_eq!(out["actor"], "alice.bsky.social");
        let posts = out["posts"].as_array().unwrap();
        assert_eq!(posts.len(), 3);
    }

    #[test]
    fn normalize_feed_ordinary_post_fields() {
        let feed_json: serde_json::Value = serde_json::from_str(FEED_FIXTURE).unwrap();
        let out = normalize_feed(&feed_json, "alice.bsky.social", DEFAULT_LIMIT);
        let p = &out["posts"][0];
        assert_eq!(p["uri"], "at://did:plc:alice/app.bsky.feed.post/1");
        assert_eq!(p["cid"], "cid-1");
        assert_eq!(p["author"]["handle"], "alice.bsky.social");
        assert_eq!(p["author"]["did"], "did:plc:alice");
        assert_eq!(p["author"]["displayName"], "Alice");
        assert_eq!(p["text"], "hello world");
        assert_eq!(p["created_at"], "2024-01-01T00:00:00.000Z");
        assert_eq!(p["reply_count"], 1);
        assert_eq!(p["repost_count"], 2);
        assert_eq!(p["like_count"], 3);
        assert_eq!(p["is_repost"], false);
        assert_eq!(p["is_reply"], false);
    }

    #[test]
    fn normalize_feed_identifies_repost() {
        let feed_json: serde_json::Value = serde_json::from_str(FEED_FIXTURE).unwrap();
        let out = normalize_feed(&feed_json, "alice.bsky.social", DEFAULT_LIMIT);
        let p = &out["posts"][1];
        assert_eq!(p["is_repost"], true);
        assert_eq!(p["is_reply"], false);
    }

    #[test]
    fn normalize_feed_identifies_reply() {
        let feed_json: serde_json::Value = serde_json::from_str(FEED_FIXTURE).unwrap();
        let out = normalize_feed(&feed_json, "alice.bsky.social", DEFAULT_LIMIT);
        let p = &out["posts"][2];
        assert_eq!(p["is_repost"], false);
        assert_eq!(p["is_reply"], true);
    }

    #[test]
    fn normalize_feed_limit_truncates() {
        let feed_json: serde_json::Value = serde_json::from_str(FEED_FIXTURE).unwrap();
        let out = normalize_feed(&feed_json, "alice.bsky.social", 1);
        assert_eq!(out["posts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn normalize_feed_missing_fields_default_null_or_zero() {
        let feed_json: serde_json::Value = serde_json::from_str(
            r#"{ "feed": [ { "post": { "uri": "at://x/app.bsky.feed.post/1" } } ] }"#,
        )
        .unwrap();
        let out = normalize_feed(&feed_json, "x", DEFAULT_LIMIT);
        let p = &out["posts"][0];
        assert!(p["cid"].is_null());
        assert!(p["author"]["handle"].is_null());
        assert!(p["text"].is_null());
        assert!(p["created_at"].is_null());
        assert_eq!(p["reply_count"], 0);
        assert_eq!(p["repost_count"], 0);
        assert_eq!(p["like_count"], 0);
    }

    #[test]
    fn normalize_feed_empty_array_when_feed_missing() {
        let feed_json: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
        let out = normalize_feed(&feed_json, "x", DEFAULT_LIMIT);
        assert_eq!(out["posts"], serde_json::json!([]));
    }

    // ---- normalize_profile ----

    const PROFILE_FIXTURE: &str = r#"{
        "did": "did:plc:alice",
        "handle": "alice.bsky.social",
        "displayName": "Alice",
        "description": "hello, I post things",
        "followersCount": 100,
        "followsCount": 50,
        "postsCount": 200
    }"#;

    #[test]
    fn normalize_profile_full_shape() {
        let profile_json: serde_json::Value = serde_json::from_str(PROFILE_FIXTURE).unwrap();
        let out = normalize_profile(&profile_json);
        assert_eq!(out["kind"], "profile");
        assert_eq!(out["actor"]["handle"], "alice.bsky.social");
        assert_eq!(out["actor"]["did"], "did:plc:alice");
        assert_eq!(out["actor"]["displayName"], "Alice");
        assert_eq!(out["actor"]["description"], "hello, I post things");
        assert_eq!(out["actor"]["followers_count"], 100);
        assert_eq!(out["actor"]["follows_count"], 50);
        assert_eq!(out["actor"]["posts_count"], 200);
    }

    #[test]
    fn normalize_profile_missing_fields_default_null_or_zero() {
        let profile_json: serde_json::Value =
            serde_json::from_str(r#"{ "did": "did:plc:bob", "handle": "bob.bsky.social" }"#)
                .unwrap();
        let out = normalize_profile(&profile_json);
        assert!(out["actor"]["displayName"].is_null());
        assert!(out["actor"]["description"].is_null());
        assert_eq!(out["actor"]["followers_count"], 0);
        assert_eq!(out["actor"]["follows_count"], 0);
        assert_eq!(out["actor"]["posts_count"], 0);
    }

    // ---- normalize_thread ----

    const THREAD_FIXTURE: &str = r#"{
        "thread": {
            "post": {
                "uri": "at://did:plc:alice/app.bsky.feed.post/1",
                "cid": "cid-1",
                "author": { "did": "did:plc:alice", "handle": "alice.bsky.social", "displayName": "Alice" },
                "record": { "text": "root post", "createdAt": "2024-01-01T00:00:00.000Z" },
                "replyCount": 2,
                "repostCount": 0,
                "likeCount": 5
            },
            "replies": [
                {
                    "post": {
                        "uri": "at://did:plc:bob/app.bsky.feed.post/2",
                        "cid": "cid-2",
                        "author": { "did": "did:plc:bob", "handle": "bob.bsky.social", "displayName": "Bob" },
                        "record": { "text": "first reply", "createdAt": "2024-01-01T01:00:00.000Z" }
                    }
                },
                {
                    "post": {
                        "uri": "at://did:plc:carol/app.bsky.feed.post/3",
                        "cid": "cid-3",
                        "author": { "did": "did:plc:carol", "handle": "carol.bsky.social", "displayName": "Carol" },
                        "record": { "text": "second reply, with its own nested reply", "createdAt": "2024-01-01T02:00:00.000Z" }
                    },
                    "replies": [
                        {
                            "post": {
                                "uri": "at://did:plc:dave/app.bsky.feed.post/4",
                                "cid": "cid-4",
                                "author": { "did": "did:plc:dave", "handle": "dave.bsky.social" },
                                "record": { "text": "nested reply, must be dropped" }
                            }
                        }
                    ]
                }
            ]
        }
    }"#;

    #[test]
    fn normalize_thread_extracts_root_post() {
        let thread_json: serde_json::Value = serde_json::from_str(THREAD_FIXTURE).unwrap();
        let out = normalize_thread(&thread_json);
        assert_eq!(out["kind"], "thread");
        assert_eq!(
            out["post"]["uri"],
            "at://did:plc:alice/app.bsky.feed.post/1"
        );
        assert_eq!(out["post"]["text"], "root post");
        assert_eq!(out["post"]["reply_count"], 2);
        assert_eq!(out["post"]["is_repost"], false);
        assert_eq!(out["post"]["is_reply"], false);
    }

    #[test]
    fn normalize_thread_extracts_first_level_replies_only() {
        let thread_json: serde_json::Value = serde_json::from_str(THREAD_FIXTURE).unwrap();
        let out = normalize_thread(&thread_json);
        let replies = out["replies"].as_array().unwrap();
        assert_eq!(
            replies.len(),
            2,
            "only the 2 direct replies, nesting dropped"
        );
        assert_eq!(replies[0]["text"], "first reply");
        assert_eq!(
            replies[1]["text"],
            "second reply, with its own nested reply"
        );
    }

    #[test]
    fn normalize_thread_does_not_flatten_nested_replies() {
        let thread_json: serde_json::Value = serde_json::from_str(THREAD_FIXTURE).unwrap();
        let out = normalize_thread(&thread_json);
        let replies = out["replies"].as_array().unwrap();
        let found_nested = replies
            .iter()
            .any(|r| r["text"] == "nested reply, must be dropped");
        assert!(!found_nested, "nested reply-of-reply must not appear");
    }

    #[test]
    fn normalize_thread_missing_thread_key_yields_null_post_and_empty_replies() {
        let thread_json: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
        let out = normalize_thread(&thread_json);
        assert!(out["post"].is_null());
        assert_eq!(out["replies"], serde_json::json!([]));
    }

    // ---- Adapter::fetch (URI validation reached before any network call) ----

    #[tokio::test]
    async fn fetch_rejects_thread_without_post_before_any_network_call() {
        let uri = WireUri::parse("bluesky://bsky.app?kind=thread").unwrap();
        let err = BlueskyAdapter.fetch(&uri).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("requires '?post=<rkey>'"),
            "unexpected error: {msg}"
        );
    }
}
