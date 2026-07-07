//! persona-wire Adapter for RSS/Atom/JSON feeds (scheme `rss://`).
//!
//! ## Architecture
//!
//! `RssAdapter` is a stateless [`Adapter`] impl split into three independent
//! functions:
//!
//! - [`parse_rss_uri`] — `WireUri` → `RssUriSpec` (target URL + item limit).
//! - HTTP fetch — delegated to `persona_wire_transport_http::HttpClient`
//!   (promoted to a shared crate 2026-07-07; no RSS-specific knowledge in the
//!   transport layer).
//! - [`normalize_feed`] — feed bytes → the Wire JSON shape below, via
//!   `feed_rs::parser::parse` (auto-detects RSS 2.0 / RSS 1.0 / Atom / JSON
//!   Feed; no manual format branching).
//!
//! ## URI grammar
//!
//! ```text
//! rss://<host>/<path>[?scheme=http][?limit=N]
//! ```
//!
//! - Default target is `https://<host><path>`; `?scheme=http` downgrades to
//!   plain HTTP (any other `scheme` value is ignored and falls back to
//!   `https`, matching the forward-compatible convention below).
//! - `?limit=N` caps the number of items returned (default
//!   [`DEFAULT_LIMIT`]). A non-numeric or zero value fails loud.
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-obsidian`).
//! - An empty host is an error.
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "feed":  { "title": "...|null", "url": "<fetched url>" },
//!   "items": [
//!     { "title": "...|null", "link": "...|null",
//!       "published": "<RFC3339>|null", "summary": "...|null" }
//!   ]
//! }
//! ```

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};
use persona_wire_transport_http::HttpClient;
use std::time::Duration;

/// Default `items` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 20;

/// Per-request HTTP timeout (connect + body), matching
/// `persona-wire-adapter-mcp::DEFAULT_RPC_TIMEOUT`.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Max `summary` length in `char`s before truncation (context size guard).
pub const SUMMARY_MAX_CHARS: usize = 500;

/// persona-wire Adapter for RSS/Atom/JSON feeds (`rss://` scheme).
pub struct RssAdapter;

#[async_trait]
impl Adapter for RssAdapter {
    fn scheme(&self) -> &'static str {
        "rss"
    }

    /// Fetch the feed at the URL derived from `uri` and normalize it.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_rss_uri(uri)?;
        let client = HttpClient::new("rss adapter").with_timeout(FETCH_TIMEOUT);
        let bytes = client.get_bytes(&spec.url).await?;
        normalize_feed(&bytes, &spec.url, spec.limit)
    }
}

/// Parsed `rss://` URI: target URL + item limit.
#[derive(Debug)]
struct RssUriSpec {
    url: String,
    limit: usize,
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into an [`RssUriSpec`].
///
/// - `host` is required and must be non-empty.
/// - `?scheme=http` downgrades the target scheme; any other value (or
///   absence) defaults to `https`.
/// - `?limit=N` must parse as a positive integer; non-numeric or `0` fails
///   loud with [`WireError::Storage`].
fn parse_rss_uri(uri: &WireUri) -> WireResult<RssUriSpec> {
    let host = uri.host().filter(|h| !h.is_empty()).ok_or_else(|| {
        WireError::Storage(format!("rss adapter: missing host in '{}'", uri.as_raw()))
    })?;

    let scheme = match uri.query_get("scheme") {
        Some("http") => "http",
        _ => "https",
    };

    let limit = match uri.query_get("limit") {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "rss adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "rss adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            n
        }
        None => DEFAULT_LIMIT,
    };

    let url = format!("{scheme}://{host}{}", uri.path());

    Ok(RssUriSpec { url, limit })
}

/// Parse `bytes` as an RSS 2.0 / RSS 1.0 / Atom / JSON Feed document (format
/// auto-detected by `feed_rs::parser`) and normalize it to the Wire JSON
/// shape (see module docs).
///
/// `src_url` is echoed back into `feed.url` (the URL the bytes were fetched
/// from, not a value read out of the feed document itself).
fn normalize_feed(bytes: &[u8], src_url: &str, limit: usize) -> WireResult<serde_json::Value> {
    let feed = feed_rs::parser::parse(bytes)
        .map_err(|e| WireError::Storage(format!("rss adapter: feed parse: {e}")))?;

    let feed_title = feed.title.map(|t| t.content);

    let items: Vec<serde_json::Value> = feed
        .entries
        .into_iter()
        .take(limit)
        .map(|entry| {
            let title = entry.title.map(|t| t.content);
            let link = entry.links.first().map(|l| l.href.clone());
            let published = entry.published.map(|dt| dt.to_rfc3339());
            let summary = entry.summary.map(|t| truncate_summary(&t.content));
            serde_json::json!({
                "title": title,
                "link": link,
                "published": published,
                "summary": summary,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "feed": {
            "title": feed_title,
            "url": src_url,
        },
        "items": items,
    }))
}

/// Truncate `s` to at most [`SUMMARY_MAX_CHARS`] `char`s (boundary-safe —
/// counts `char`s, not bytes), appending `…` when truncation occurred.
fn truncate_summary(s: &str) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(SUMMARY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_rss_uri ----

    fn parse(uri: &str) -> WireResult<RssUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_rss_uri(&wire)
    }

    #[test]
    fn parse_rss_uri_https_default() {
        let spec = parse("rss://example.com/feed.xml").unwrap();
        assert_eq!(spec.url, "https://example.com/feed.xml");
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_rss_uri_scheme_http_downgrade() {
        let spec = parse("rss://example.com/feed.xml?scheme=http").unwrap();
        assert_eq!(spec.url, "http://example.com/feed.xml");
    }

    #[test]
    fn parse_rss_uri_unknown_scheme_value_defaults_https() {
        let spec = parse("rss://example.com/feed.xml?scheme=ftp").unwrap();
        assert_eq!(spec.url, "https://example.com/feed.xml");
    }

    #[test]
    fn parse_rss_uri_limit_override() {
        let spec = parse("rss://example.com/feed.xml?limit=5").unwrap();
        assert_eq!(spec.limit, 5);
    }

    #[test]
    fn parse_rss_uri_limit_non_numeric_errors() {
        let err = parse("rss://example.com/feed.xml?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_rss_uri_limit_zero_errors() {
        let err = parse("rss://example.com/feed.xml?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_rss_uri_empty_host_errors() {
        let err = parse("rss:///feed.xml").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing host"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_rss_uri_unknown_query_key_ignored() {
        let spec = parse("rss://example.com/feed.xml?utm_source=foo").unwrap();
        assert_eq!(spec.url, "https://example.com/feed.xml");
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_rss_uri_combined_scheme_and_limit() {
        let spec = parse("rss://example.com/feed.xml?scheme=http&limit=3").unwrap();
        assert_eq!(spec.url, "http://example.com/feed.xml");
        assert_eq!(spec.limit, 3);
    }

    #[test]
    fn parse_rss_uri_host_with_port_preserved() {
        let spec = parse("rss://example.com:8080/feed.xml").unwrap();
        assert_eq!(spec.url, "https://example.com:8080/feed.xml");
    }

    // ---- normalize_feed ----

    const RSS2_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
<channel>
<title>Example Feed</title>
<link>https://example.com/</link>
<description>Example feed for tests</description>
<item>
<title>Item One</title>
<link>https://example.com/1</link>
<pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
<description>Summary one</description>
</item>
<item>
<title>Item Two</title>
<link>https://example.com/2</link>
<pubDate>Tue, 02 Jan 2024 00:00:00 GMT</pubDate>
</item>
</channel>
</rss>"#;

    const ATOM_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<title>Atom Feed</title>
<link href="https://example.com/"/>
<updated>2024-01-01T00:00:00Z</updated>
<id>urn:uuid:feed-1</id>
<entry>
<title>Atom Entry</title>
<link href="https://example.com/atom1"/>
<id>urn:uuid:entry-1</id>
<updated>2024-01-01T00:00:00Z</updated>
<published>2024-01-01T00:00:00Z</published>
<summary>Atom summary</summary>
</entry>
</feed>"#;

    #[test]
    fn normalize_feed_rss2_basic_shape() {
        let v = normalize_feed(RSS2_FIXTURE.as_bytes(), "https://example.com/feed.xml", 20)
            .expect("rss2 parse ok");
        assert_eq!(v["feed"]["title"].as_str().unwrap(), "Example Feed");
        assert_eq!(
            v["feed"]["url"].as_str().unwrap(),
            "https://example.com/feed.xml"
        );
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "both items present");
        assert_eq!(items[0]["title"].as_str().unwrap(), "Item One");
        assert_eq!(items[0]["link"].as_str().unwrap(), "https://example.com/1");
        assert_eq!(items[0]["summary"].as_str().unwrap(), "Summary one");
        assert!(items[0]["published"].is_string(), "published present");
    }

    #[test]
    fn normalize_feed_rss2_missing_summary_is_null() {
        let v = normalize_feed(RSS2_FIXTURE.as_bytes(), "https://example.com/feed.xml", 20)
            .expect("rss2 parse ok");
        let items = v["items"].as_array().unwrap();
        assert!(
            items[1]["summary"].is_null(),
            "item without <description> has null summary"
        );
    }

    #[test]
    fn normalize_feed_atom_basic_shape() {
        let v = normalize_feed(ATOM_FIXTURE.as_bytes(), "https://example.com/atom.xml", 20)
            .expect("atom parse ok");
        assert_eq!(v["feed"]["title"].as_str().unwrap(), "Atom Feed");
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["title"].as_str().unwrap(), "Atom Entry");
        assert_eq!(
            items[0]["link"].as_str().unwrap(),
            "https://example.com/atom1"
        );
        assert_eq!(items[0]["summary"].as_str().unwrap(), "Atom summary");
        assert_eq!(
            items[0]["published"].as_str().unwrap(),
            "2024-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn normalize_feed_limit_truncates() {
        let v = normalize_feed(RSS2_FIXTURE.as_bytes(), "https://example.com/feed.xml", 1)
            .expect("rss2 parse ok");
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "limit=1 truncates to a single item");
        assert_eq!(items[0]["title"].as_str().unwrap(), "Item One");
    }

    #[test]
    fn normalize_feed_invalid_bytes_errors() {
        let err =
            normalize_feed(b"not a feed at all", "https://example.com/feed.xml", 20).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("feed parse"), "unexpected error: {msg}");
    }

    #[test]
    fn truncate_summary_no_truncation_under_limit() {
        let s = "short summary";
        assert_eq!(truncate_summary(s), s);
    }

    #[test]
    fn truncate_summary_truncates_over_500_chars_with_ellipsis() {
        let s = "a".repeat(600);
        let out = truncate_summary(&s);
        assert_eq!(out.chars().count(), SUMMARY_MAX_CHARS + 1, "500 + ellipsis");
        assert!(out.ends_with('…'), "ends with ellipsis marker");
        assert!(
            out.starts_with(&"a".repeat(SUMMARY_MAX_CHARS)),
            "first 500 chars preserved"
        );
    }

    #[test]
    fn truncate_summary_exact_boundary_no_ellipsis() {
        let s = "a".repeat(SUMMARY_MAX_CHARS);
        let out = truncate_summary(&s);
        assert_eq!(out, s, "exactly at the limit — no truncation marker");
    }

    #[test]
    fn normalize_feed_summary_truncated_in_context() {
        let long_summary = "b".repeat(600);
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
<channel>
<title>Long Feed</title>
<item>
<title>Long Item</title>
<description>{long_summary}</description>
</item>
</channel>
</rss>"#
        );
        let v = normalize_feed(xml.as_bytes(), "https://example.com/feed.xml", 20)
            .expect("rss2 parse ok");
        let items = v["items"].as_array().unwrap();
        let summary = items[0]["summary"].as_str().unwrap();
        assert_eq!(summary.chars().count(), SUMMARY_MAX_CHARS + 1);
        assert!(summary.ends_with('…'));
    }
}
