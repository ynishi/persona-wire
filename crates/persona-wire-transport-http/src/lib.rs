//! Shared HTTP transport for persona-wire Adapters (`persona-wire-transport-http`).
//!
//! ## Architecture
//!
//! [`HttpClient`] is a builder-style, stateless-per-call HTTP client shared by
//! every HTTP-backed Adapter crate. It carries no scheme-specific knowledge
//! (no feed parsing, no API-specific response shaping) — that stays in the
//! calling Adapter. `ctx` is a short, human-readable prefix (e.g.
//! `"rss adapter"`) baked into every error message so failures are traceable
//! back to the Adapter that produced them, matching the message form each
//! Adapter used before this crate existed.
//!
//! ```text
//! RssAdapter, NotionAdapter, ... (scheme-specific parse / normalize)
//!        │
//!        ▼
//! HttpClient::new(ctx).with_timeout(..).with_bearer(..).with_header(..)
//!        │
//!        ▼
//! reqwest::Client (rustls-tls, per-call)
//! ```
//!
//! ## API
//!
//! - [`HttpClient::new`] takes only `ctx`; [`DEFAULT_TIMEOUT`] applies unless
//!   overridden via [`HttpClient::with_timeout`].
//! - [`HttpClient::with_bearer`] sets an `Authorization: Bearer <token>`
//!   header from a [`secrecy::SecretString`].
//! - [`HttpClient::with_header`] appends an arbitrary fixed header (e.g.
//!   `Notion-Version`).
//! - [`HttpClient::get_bytes`] / [`HttpClient::get_json`] /
//!   [`HttpClient::post_json`] perform the request; JSON variants parse the
//!   response body as [`serde_json::Value`].
//!
//! ## Error conventions
//!
//! Every failure is [`persona_wire_core::WireError::Storage`] with a
//! `"{ctx}: <what> fetching '{url}': {cause}"` shaped message (see the
//! `*_err` helpers in this module for the exact wording per failure kind).
//! Non-2xx HTTP status is treated as a fetch failure, not a partial success.

#![warn(missing_docs)]

use std::time::Duration;

use persona_wire_core::{WireError, WireResult};
use secrecy::{ExposeSecret, SecretString};

/// Default per-request timeout (connect + body) when
/// [`HttpClient::with_timeout`] is not called.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Builder-style shared HTTP client for persona-wire Adapters.
///
/// Every setter consumes `self` and returns `Self` (chainable). A fresh
/// `reqwest::Client` is built per call (stateless; Adapters call this at most
/// a handful of times per `fetch`, so connection pooling across calls is not
/// worth the added lifetime complexity here).
pub struct HttpClient {
    ctx: &'static str,
    timeout: Duration,
    bearer: Option<SecretString>,
    extra_headers: Vec<(String, String)>,
}

impl HttpClient {
    /// New client. `ctx` is a short prefix identifying the calling Adapter
    /// (e.g. `"rss adapter"`), embedded verbatim in every error message.
    pub fn new(ctx: &'static str) -> Self {
        Self {
            ctx,
            timeout: DEFAULT_TIMEOUT,
            bearer: None,
            extra_headers: Vec::new(),
        }
    }

    /// Override the per-request timeout (default [`DEFAULT_TIMEOUT`]).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attach an `Authorization: Bearer <token>` header to every request.
    ///
    /// The token is held as a [`secrecy::SecretString`] and only exposed at
    /// request-build time; it is never logged or printed via `Debug`
    /// (`HttpClient` intentionally does not derive `Debug`).
    pub fn with_bearer(mut self, token: SecretString) -> Self {
        self.bearer = Some(token);
        self
    }

    /// Attach an arbitrary fixed header (e.g. `Notion-Version: 2022-06-28`)
    /// to every request. Call multiple times to add multiple headers.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.extra_headers
            .push((name.to_string(), value.to_string()));
        self
    }

    fn build_client(&self) -> WireResult<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| client_build_err(self.ctx, &e))
    }

    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.bearer {
            req = req.bearer_auth(token.expose_secret());
        }
        for (name, value) in &self.extra_headers {
            req = req.header(name.as_str(), value.as_str());
        }
        req
    }

    async fn send(&self, req: reqwest::RequestBuilder, url: &str) -> WireResult<reqwest::Response> {
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                timeout_err(self.ctx, self.timeout, url, &e)
            } else {
                network_err(self.ctx, url, &e)
            }
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(status_err(self.ctx, url, status));
        }
        Ok(resp)
    }

    /// Plain HTTP GET, returning the raw response body.
    pub async fn get_bytes(&self, url: &str) -> WireResult<Vec<u8>> {
        let client = self.build_client()?;
        let req = self.apply_headers(client.get(url));
        let resp = self.send(req, url).await?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| body_err(self.ctx, url, &e))?;
        Ok(bytes.to_vec())
    }

    /// HTTP GET, parsing the response body as JSON.
    pub async fn get_json(&self, url: &str) -> WireResult<serde_json::Value> {
        let bytes = self.get_bytes(url).await?;
        serde_json::from_slice(&bytes).map_err(|e| json_parse_err(self.ctx, url, &e))
    }

    /// HTTP GET, parsing the response body as JSON and also extracting the
    /// `rel="next"` URL from an RFC 5988 `Link` response header (the
    /// pagination convention used by GitHub's REST API and others).
    ///
    /// Returns `(body, None)` when the response has no `Link` header, or the
    /// header has no `rel="next"` entry — this is the common case (most API
    /// responses aren't the last page of *and* don't paginate at all), so
    /// it is not treated as an error.
    pub async fn get_json_with_next_link(
        &self,
        url: &str,
    ) -> WireResult<(serde_json::Value, Option<String>)> {
        let client = self.build_client()?;
        let req = self.apply_headers(client.get(url));
        let resp = self.send(req, url).await?;
        // Headers borrow from `resp`, so extract the (owned) next-link value
        // before `resp.bytes()` consumes it.
        let next = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_link_next);
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| body_err(self.ctx, url, &e))?;
        let value =
            serde_json::from_slice(&bytes).map_err(|e| json_parse_err(self.ctx, url, &e))?;
        Ok((value, next))
    }

    /// HTTP POST with a JSON body, parsing the response body as JSON.
    pub async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> WireResult<serde_json::Value> {
        let client = self.build_client()?;
        let req = self.apply_headers(client.post(url).json(body));
        let resp = self.send(req, url).await?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| body_err(self.ctx, url, &e))?;
        serde_json::from_slice(&bytes).map_err(|e| json_parse_err(self.ctx, url, &e))
    }
}

// ---- error message helpers (free functions so the exact wording is
// unit-testable offline without needing a live reqwest::Error instance) ----

fn timeout_err(ctx: &str, timeout: Duration, url: &str, err: impl std::fmt::Display) -> WireError {
    WireError::Storage(format!(
        "{ctx}: http timeout ({timeout:?}) fetching '{url}': {err}"
    ))
}

fn network_err(ctx: &str, url: &str, err: impl std::fmt::Display) -> WireError {
    WireError::Storage(format!("{ctx}: network error fetching '{url}': {err}"))
}

fn status_err(ctx: &str, url: &str, status: reqwest::StatusCode) -> WireError {
    WireError::Storage(format!("{ctx}: http status {status} fetching '{url}'"))
}

fn body_err(ctx: &str, url: &str, err: impl std::fmt::Display) -> WireError {
    WireError::Storage(format!("{ctx}: reading response body from '{url}': {err}"))
}

fn client_build_err(ctx: &str, err: impl std::fmt::Display) -> WireError {
    WireError::Storage(format!("{ctx}: http client build: {err}"))
}

fn json_parse_err(ctx: &str, url: &str, err: impl std::fmt::Display) -> WireError {
    WireError::Storage(format!("{ctx}: response json parse from '{url}': {err}"))
}

/// Extracts the `rel="next"` URL from an RFC 5988 `Link` header value, e.g.:
///
/// ```text
/// <https://api.github.com/repos/x/y/issues?page=2>; rel="next", <...>; rel="last"
/// ```
///
/// A free function (not a method) so it is unit-testable offline over
/// literal header-value fixtures, matching this module's convention for the
/// `*_err` helpers above — no live HTTP round-trip needed to exercise the
/// parsing logic.
///
/// - Splits on `,` for multiple link entries, then on `;` within an entry.
/// - Matches both quoted (`rel="next"`) and unquoted (`rel=next`) forms.
/// - Returns `None` when no entry has `rel="next"` (including malformed
///   entries — this never panics; a broken `Link` header just degrades to
///   "no next page" rather than an error).
fn parse_link_next(header_value: &str) -> Option<String> {
    for entry in header_value.split(',') {
        let mut url: Option<String> = None;
        let mut is_next = false;
        for part in entry.split(';') {
            let part = part.trim();
            if let Some(inner) = part.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
                url = Some(inner.to_string());
            } else if let Some(rel) = part.strip_prefix("rel=") {
                if rel.trim_matches('"') == "next" {
                    is_next = true;
                }
            }
        }
        if is_next {
            return url;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- error message wording (offline, no live reqwest::Error needed) ----

    #[test]
    fn timeout_err_matches_expected_form() {
        let e = timeout_err(
            "rss adapter",
            Duration::from_secs(30),
            "https://x/feed.xml",
            "boom",
        );
        let msg = format!("{e}");
        assert!(msg.contains("rss adapter: http timeout"));
        assert!(msg.contains("30s"));
        assert!(msg.contains("fetching 'https://x/feed.xml'"));
        assert!(msg.contains("boom"));
    }

    #[test]
    fn network_err_matches_expected_form() {
        let e = network_err("rss adapter", "https://x/feed.xml", "conn refused");
        let msg = format!("{e}");
        assert_eq!(
            msg,
            "storage error: rss adapter: network error fetching 'https://x/feed.xml': conn refused"
        );
    }

    #[test]
    fn status_err_matches_expected_form() {
        let status = reqwest::StatusCode::from_u16(500).unwrap();
        let e = status_err("rss adapter", "https://x/feed.xml", status);
        let msg = format!("{e}");
        assert_eq!(
            msg,
            "storage error: rss adapter: http status 500 Internal Server Error fetching 'https://x/feed.xml'"
        );
    }

    #[test]
    fn body_err_matches_expected_form() {
        let e = body_err("rss adapter", "https://x/feed.xml", "truncated");
        let msg = format!("{e}");
        assert_eq!(
            msg,
            "storage error: rss adapter: reading response body from 'https://x/feed.xml': truncated"
        );
    }

    #[test]
    fn client_build_err_matches_expected_form() {
        let e = client_build_err("rss adapter", "bad tls config");
        let msg = format!("{e}");
        assert_eq!(
            msg,
            "storage error: rss adapter: http client build: bad tls config"
        );
    }

    #[test]
    fn json_parse_err_matches_expected_form() {
        let raw_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let e = json_parse_err("notion adapter", "https://x/y", &raw_err);
        let msg = format!("{e}");
        assert!(msg.starts_with(
            "storage error: notion adapter: response json parse from 'https://x/y': "
        ));
    }

    // ---- builder state (private-field access from the same crate) ----

    #[test]
    fn new_uses_default_timeout_and_no_auth() {
        let c = HttpClient::new("test ctx");
        assert_eq!(c.ctx, "test ctx");
        assert_eq!(c.timeout, DEFAULT_TIMEOUT);
        assert!(c.bearer.is_none());
        assert!(c.extra_headers.is_empty());
    }

    #[test]
    fn with_timeout_overrides_default() {
        let c = HttpClient::new("test ctx").with_timeout(Duration::from_secs(5));
        assert_eq!(c.timeout, Duration::from_secs(5));
    }

    #[test]
    fn with_bearer_sets_token() {
        let c = HttpClient::new("test ctx").with_bearer(SecretString::from("tok-123".to_string()));
        assert_eq!(c.bearer.as_ref().unwrap().expose_secret(), "tok-123");
    }

    #[test]
    fn with_header_appends_multiple() {
        let c = HttpClient::new("test ctx")
            .with_header("Notion-Version", "2022-06-28")
            .with_header("X-Custom", "1");
        assert_eq!(
            c.extra_headers,
            vec![
                ("Notion-Version".to_string(), "2022-06-28".to_string()),
                ("X-Custom".to_string(), "1".to_string()),
            ]
        );
    }

    // ---- parse_link_next (RFC 5988 Link header, Layer 3a of GH #1) ----
    //
    // These test the extraction logic directly over literal header-value
    // fixtures rather than through a live (or wiremock'd) HTTP round-trip:
    // neither `wiremock` nor a hand-rolled mock server is used anywhere in
    // this workspace yet (checked at implementation time — no existing
    // pattern to extend), and `adapter.rs`'s crate-root docs establish the
    // repo-wide convention that Adapter-adjacent tests are offline unit
    // tests over inline fixtures, never live network access. Factoring the
    // parsing out to a free function (mirroring the `*_err` helpers above)
    // gets full coverage of the new logic without adding a new dependency
    // or a live listener for a single header-parsing feature.

    #[test]
    fn parse_link_next_extracts_rel_next_url() {
        let header = r#"<https://api.github.com/repos/x/y/issues?page=2>; rel="next""#;
        assert_eq!(
            parse_link_next(header),
            Some("https://api.github.com/repos/x/y/issues?page=2".to_string())
        );
    }

    #[test]
    fn parse_link_next_multiple_entries_returns_next_only() {
        let header = concat!(
            r#"<https://api.github.com/repos/x/y/issues?page=2>; rel="next", "#,
            r#"<https://api.github.com/repos/x/y/issues?page=1>; rel="prev", "#,
            r#"<https://api.github.com/repos/x/y/issues?page=5>; rel="last""#,
        );
        assert_eq!(
            parse_link_next(header),
            Some("https://api.github.com/repos/x/y/issues?page=2".to_string())
        );
    }

    #[test]
    fn parse_link_next_rel_last_only_returns_none() {
        let header = r#"<https://api.github.com/repos/x/y/issues?page=5>; rel="last""#;
        assert_eq!(parse_link_next(header), None);
    }

    #[test]
    fn parse_link_next_unquoted_rel_matches() {
        let header = "<https://api.example.com/next>; rel=next";
        assert_eq!(
            parse_link_next(header),
            Some("https://api.example.com/next".to_string())
        );
    }

    #[test]
    fn parse_link_next_empty_string_returns_none() {
        assert_eq!(parse_link_next(""), None);
    }

    #[test]
    fn parse_link_next_malformed_entry_does_not_panic() {
        // No `<...>` URL, no `rel=` param — degrades to "no next page".
        let header = "garbage, also garbage";
        assert_eq!(parse_link_next(header), None);
    }
}
