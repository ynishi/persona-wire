//! Unified adapter filter vocabulary + shared parser.
//!
//! ## Architecture
//!
//! Before this module, cross-cutting query params (`?limit=`, `?tail=`,
//! `?since=`) were parsed independently by each adapter (`FileAdapter`'s
//! `parse_tail_mode`, the todoist/notion/slack adapters' own ad-hoc
//! `limit` parsing, ...), so the same semantic filter grew 3 slightly
//! different error dialects across the workspace: some silently ignored
//! bad input, some clamped, some fail-loud'd with adapter-specific message
//! shapes. [`FilterCap`] / [`WireFilters`] / [`WireFilters::parse`]
//! consolidate that into **one closed vocabulary** (7 known keys) and
//! **one error policy** (see the table below), so every adapter that opts
//! in via [`crate::infrastructure::adapter::Adapter::filter_caps`] gets
//! identical parsing behavior for free.
//!
//! ## Filter vs Addressing
//!
//! A `source_uri` query string carries two orthogonal kinds of keys:
//! - **Filter keys** — cross-cutting, adapter-agnostic slicing/selection
//!   (`limit` / `lines` / `tail` / `tail_n` / `since` / `until` / `query`).
//!   This module owns their grammar and error policy.
//! - **Addressing keys** — adapter-specific identity/routing params
//!   (`kind`, `state`, `auth`, `project_id`, ...). These stay entirely
//!   inside each adapter's own parser; [`WireFilters::parse`] never
//!   inspects or rejects them (see the "unknown query keys are silently
//!   ignored" convention documented in
//!   [`crate::infrastructure::adapter`]).
//!
//! ## Unified error policy
//!
//! | condition | behavior |
//! |---|---|
//! | filter key absent | field stays `None` (default) |
//! | value present but wrong type (`limit=abc`, `lines=x-y`, `tail_n=abc`, `tail=unknown`) | `Err` (fail loud) |
//! | value well-typed but exceeds a declared cap (`limit=5000` w/ `max=Some(40)`, `tail_n=5000` w/ `n_max=1000`) | clamp to the cap + `tracing::warn!` |
//! | `lines=FROM-TO` with `FROM > TO`, or `FROM == 0` (1-origin violation) | `Err` |
//! | filter key present in the URI but not declared in `caps` | `Err` ("not supported by this adapter") |

use std::fmt;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::wire_uri::WireUri;

/// A cross-cutting filter capability an [`Adapter`](crate::infrastructure::adapter::Adapter)
/// declares support for via `filter_caps()`. [`WireFilters::parse`] only
/// honors query keys whose capability is present in the `caps` slice passed
/// to it; everything else in this closed vocabulary is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterCap {
    /// `?limit=N` — upper bound on the number of items a list-shaped
    /// response returns. `max` is the clamp ceiling (`None` = unbounded).
    Limit {
        /// Clamp ceiling for `N`. `None` = no upper bound declared.
        max: Option<usize>,
    },
    /// `?lines=FROM-TO` — 1-origin inclusive line-range slicing of a
    /// document-shaped response body.
    LineRange,
    /// `?tail=last_section` / `?tail_n=N` — trailing-slice selection of a
    /// document-shaped response body. `n_max` is the clamp ceiling for `N`.
    Tail {
        /// Clamp ceiling for `?tail_n=N`.
        n_max: usize,
    },
    /// `?since=TS` / `?until=TS` — time-range selection. `TS` is an opaque
    /// string; the adapter interprets its own timestamp format.
    SinceUntil,
    /// `?query=TEXT` — free-text search. `TEXT` is percent-decoded before
    /// being handed to the adapter.
    TextQuery,
}

impl fmt::Display for FilterCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterCap::Limit { max: Some(m) } => write!(f, "limit(max={m})"),
            FilterCap::Limit { max: None } => write!(f, "limit"),
            FilterCap::LineRange => write!(f, "lines"),
            FilterCap::Tail { n_max } => write!(f, "tail(n_max={n_max})"),
            FilterCap::SinceUntil => write!(f, "since-until"),
            FilterCap::TextQuery => write!(f, "query"),
        }
    }
}

/// Parsed cross-cutting filters for one `source_uri` fetch, produced by
/// [`WireFilters::parse`]. Fields left `None` mean "the caller did not
/// request that filter" — adapters treat that as their existing
/// backward-compatible default (e.g. whole-body fetch).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WireFilters {
    /// `?limit=N`, already clamped against the adapter's declared `max`.
    pub limit: Option<usize>,
    /// `?lines=FROM-TO`, 1-origin inclusive.
    pub line_range: Option<(usize, usize)>,
    /// `?tail=last_section` / `?tail_n=N`.
    pub tail: Option<TailSpec>,
    /// `?since=TS`, opaque string (adapter interprets).
    pub since: Option<String>,
    /// `?until=TS`, opaque string (adapter interprets).
    pub until: Option<String>,
    /// `?query=TEXT`, percent-decoded.
    pub query: Option<String>,
}

/// Tail-slicing variant for [`WireFilters::tail`] (promoted from the
/// former `FileAdapter`-private `TailMode`, now a shared public vocabulary
/// item so every adapter declaring [`FilterCap::Tail`] gets the same
/// semantics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailSpec {
    /// `?tail=last_section` — everything from the last markdown `## `
    /// h2 heading onward.
    LastSection,
    /// `?tail_n=N` — the last `N` lines (already clamped to the
    /// adapter's declared `n_max`).
    LastN(usize),
}

impl WireFilters {
    /// Parses the closed filter-key vocabulary out of `uri`'s query
    /// params, honoring only the capabilities declared in `caps`. See the
    /// module-level error policy table for the exact per-key behavior.
    /// Query keys outside the vocabulary (`limit` / `lines` / `tail` /
    /// `tail_n` / `since` / `until` / `query`) are addressing keys and are
    /// always ignored here regardless of `caps`.
    pub fn parse(uri: &WireUri, caps: &[FilterCap]) -> WireResult<Self> {
        let mut filters = WireFilters::default();

        let limit_cap = caps.iter().find_map(|c| match c {
            FilterCap::Limit { max } => Some(*max),
            _ => None,
        });
        let line_range_cap = caps.iter().any(|c| matches!(c, FilterCap::LineRange));
        let tail_cap = caps.iter().find_map(|c| match c {
            FilterCap::Tail { n_max } => Some(*n_max),
            _ => None,
        });
        let since_until_cap = caps.iter().any(|c| matches!(c, FilterCap::SinceUntil));
        let text_query_cap = caps.iter().any(|c| matches!(c, FilterCap::TextQuery));

        if let Some(raw) = uri.query_get("limit") {
            let max = limit_cap.ok_or_else(|| unsupported_filter("limit", caps))?;
            let n: usize = raw
                .parse()
                .map_err(|_| invalid_value("limit", raw, "a non-negative integer"))?;
            filters.limit = Some(match max {
                Some(m) => clamp_with_warn("limit", n, m),
                None => n,
            });
        }

        if let Some(raw) = uri.query_get("lines") {
            if !line_range_cap {
                return Err(unsupported_filter("lines", caps));
            }
            filters.line_range = Some(parse_line_range(raw)?);
        }

        if let Some(raw) = uri.query_get("tail") {
            let n_max = tail_cap.ok_or_else(|| unsupported_filter("tail", caps))?;
            if raw != "last_section" {
                return Err(invalid_value("tail", raw, "'last_section'"));
            }
            let _ = n_max; // last_section is not clamped; n_max only applies to tail_n
            filters.tail = Some(TailSpec::LastSection);
        } else if let Some(raw) = uri.query_get("tail_n") {
            let n_max = tail_cap.ok_or_else(|| unsupported_filter("tail_n", caps))?;
            let n: usize = raw
                .parse()
                .map_err(|_| invalid_value("tail_n", raw, "a positive integer"))?;
            filters.tail = Some(TailSpec::LastN(clamp_with_warn("tail_n", n, n_max)));
        }

        if let Some(raw) = uri.query_get("since") {
            if !since_until_cap {
                return Err(unsupported_filter("since", caps));
            }
            filters.since = Some(raw.to_string());
        }
        if let Some(raw) = uri.query_get("until") {
            if !since_until_cap {
                return Err(unsupported_filter("until", caps));
            }
            filters.until = Some(raw.to_string());
        }

        if let Some(raw) = uri.query_get("query") {
            if !text_query_cap {
                return Err(unsupported_filter("query", caps));
            }
            filters.query = Some(percent_decode(raw));
        }

        Ok(filters)
    }
}

/// Short vocabulary name for a capability, used in the "supported: ..."
/// hint of [`unsupported_filter`]. Matches the query-key spelling for
/// single-key caps; `Tail` (which backs 2 keys, `tail` / `tail_n`) reports
/// its cap name once.
fn filter_key_name(cap: &FilterCap) -> &'static str {
    match cap {
        FilterCap::Limit { .. } => "limit",
        FilterCap::LineRange => "lines",
        FilterCap::Tail { .. } => "tail",
        FilterCap::SinceUntil => "since-until",
        FilterCap::TextQuery => "query",
    }
}

fn unsupported_filter(key: &str, caps: &[FilterCap]) -> WireError {
    let supported: Vec<&str> = caps.iter().map(filter_key_name).collect();
    WireError::Storage(format!(
        "filter '{key}' not supported by this adapter (supported: {})",
        supported.join(", ")
    ))
}

fn invalid_value(key: &str, raw: &str, expected: &str) -> WireError {
    WireError::Storage(format!(
        "{key}: invalid value '{raw}' (expected {expected})"
    ))
}

/// Clamps `n` to `max`, emitting a `tracing::warn!` when clamping occurs
/// (unified error-policy "上限超過 → clamp + warn" behavior).
fn clamp_with_warn(key: &str, n: usize, max: usize) -> usize {
    if n > max {
        tracing::warn!(filter = key, requested = n, max, "wire filter clamped");
        max
    } else {
        n
    }
}

/// Parses `FROM-TO` (1-origin inclusive) for `?lines=`. `FROM` must be
/// `>= 1` and `<= TO`; both violations fail loud per the unified policy.
fn parse_line_range(raw: &str) -> WireResult<(usize, usize)> {
    let expected = "FROM-TO (1-origin, e.g. '10-40')";
    let (from_s, to_s) = raw
        .split_once('-')
        .ok_or_else(|| invalid_value("lines", raw, expected))?;
    let from: usize = from_s
        .parse()
        .map_err(|_| invalid_value("lines", raw, expected))?;
    let to: usize = to_s
        .parse()
        .map_err(|_| invalid_value("lines", raw, expected))?;
    if from == 0 {
        return Err(invalid_value(
            "lines",
            raw,
            "1-origin line numbers (FROM must be >= 1)",
        ));
    }
    if from > to {
        return Err(invalid_value("lines", raw, "FROM <= TO"));
    }
    Ok((from, to))
}

/// Minimal RFC 3986 §2.1 percent-decoding for `?query=` values.
/// [`WireUri::query_get`] hands query values back raw (undecoded); this
/// decodes `%XX` escapes and falls back to lossy UTF-8 reassembly,
/// mirroring the `percent_encoding` crate behavior already used by the
/// external adapter crates (todoist / notion / slack) without adding that
/// dependency to this crate for one call site.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> WireUri {
        WireUri::parse(s).unwrap()
    }

    // ---- FilterCap Display ----

    #[test]
    fn filter_cap_display_all_variants() {
        assert_eq!(
            FilterCap::Limit { max: Some(40) }.to_string(),
            "limit(max=40)"
        );
        assert_eq!(FilterCap::Limit { max: None }.to_string(), "limit");
        assert_eq!(FilterCap::LineRange.to_string(), "lines");
        assert_eq!(
            FilterCap::Tail { n_max: 1000 }.to_string(),
            "tail(n_max=1000)"
        );
        assert_eq!(FilterCap::SinceUntil.to_string(), "since-until");
        assert_eq!(FilterCap::TextQuery.to_string(), "query");
    }

    // ---- parse: per-filter normal path ----

    #[test]
    fn parse_limit_within_max() {
        let caps = [FilterCap::Limit { max: Some(40) }];
        let f = WireFilters::parse(&uri("mini-app://t?limit=10"), &caps).unwrap();
        assert_eq!(f.limit, Some(10));
    }

    #[test]
    fn parse_limit_unbounded_cap() {
        let caps = [FilterCap::Limit { max: None }];
        let f = WireFilters::parse(&uri("mini-app://t?limit=999999"), &caps).unwrap();
        assert_eq!(f.limit, Some(999_999));
    }

    #[test]
    fn parse_line_range_normal() {
        let caps = [FilterCap::LineRange];
        let f = WireFilters::parse(&uri("file:///x?lines=10-40"), &caps).unwrap();
        assert_eq!(f.line_range, Some((10, 40)));
    }

    #[test]
    fn parse_tail_last_section() {
        let caps = [FilterCap::Tail { n_max: 1000 }];
        let f = WireFilters::parse(&uri("file:///x?tail=last_section"), &caps).unwrap();
        assert_eq!(f.tail, Some(TailSpec::LastSection));
    }

    #[test]
    fn parse_tail_n_normal() {
        let caps = [FilterCap::Tail { n_max: 1000 }];
        let f = WireFilters::parse(&uri("file:///x?tail_n=5"), &caps).unwrap();
        assert_eq!(f.tail, Some(TailSpec::LastN(5)));
    }

    #[test]
    fn parse_since_until_normal() {
        let caps = [FilterCap::SinceUntil];
        let f = WireFilters::parse(
            &uri("mini-app://t?since=2026-01-01&until=2026-02-01"),
            &caps,
        )
        .unwrap();
        assert_eq!(f.since.as_deref(), Some("2026-01-01"));
        assert_eq!(f.until.as_deref(), Some("2026-02-01"));
    }

    #[test]
    fn parse_query_normal_and_percent_decoded() {
        let caps = [FilterCap::TextQuery];
        let f = WireFilters::parse(&uri("mini-app://t?query=hello%20world"), &caps).unwrap();
        assert_eq!(f.query.as_deref(), Some("hello world"));
    }

    // ---- parse: type-invalid → Err ----

    #[test]
    fn parse_limit_invalid_type_errs() {
        let caps = [FilterCap::Limit { max: Some(40) }];
        let r = WireFilters::parse(&uri("mini-app://t?limit=abc"), &caps);
        assert!(r.is_err());
    }

    #[test]
    fn parse_lines_invalid_type_errs() {
        let caps = [FilterCap::LineRange];
        let r = WireFilters::parse(&uri("file:///x?lines=x-y"), &caps);
        assert!(r.is_err());
    }

    #[test]
    fn parse_tail_n_invalid_type_errs() {
        let caps = [FilterCap::Tail { n_max: 1000 }];
        let r = WireFilters::parse(&uri("file:///x?tail_n=abc"), &caps);
        assert!(r.is_err());
    }

    #[test]
    fn parse_tail_unknown_value_errs() {
        let caps = [FilterCap::Tail { n_max: 1000 }];
        let r = WireFilters::parse(&uri("file:///x?tail=unknown_value"), &caps);
        assert!(r.is_err());
    }

    // ---- parse: over-limit → clamp + no error ----

    #[test]
    fn parse_limit_over_max_clamps() {
        let caps = [FilterCap::Limit { max: Some(40) }];
        let f = WireFilters::parse(&uri("mini-app://t?limit=5000"), &caps).unwrap();
        assert_eq!(f.limit, Some(40));
    }

    #[test]
    fn parse_tail_n_over_max_clamps() {
        let caps = [FilterCap::Tail { n_max: 1000 }];
        let f = WireFilters::parse(&uri("file:///x?tail_n=5000"), &caps).unwrap();
        assert_eq!(f.tail, Some(TailSpec::LastN(1000)));
    }

    // ---- parse: undeclared cap key → Err ----

    #[test]
    fn parse_undeclared_limit_errs() {
        let caps = [FilterCap::LineRange];
        let r = WireFilters::parse(&uri("mini-app://t?limit=5"), &caps);
        assert!(r.is_err());
        let msg = format!("{}", r.unwrap_err());
        assert!(msg.contains("limit"), "message should name the key: {msg}");
        assert!(
            msg.contains("lines"),
            "message should list supported keys: {msg}"
        );
    }

    // ---- parse: addressing keys are ignored regardless of caps ----

    #[test]
    fn parse_ignores_addressing_keys() {
        let caps: [FilterCap; 0] = [];
        let f = WireFilters::parse(&uri("mini-app://t?kind=issue&state=open"), &caps).unwrap();
        assert_eq!(f, WireFilters::default());
    }

    // ---- parse: lines boundary violations ----

    #[test]
    fn parse_lines_from_greater_than_to_errs() {
        let caps = [FilterCap::LineRange];
        let r = WireFilters::parse(&uri("file:///x?lines=40-10"), &caps);
        assert!(r.is_err());
    }

    #[test]
    fn parse_lines_zero_origin_errs() {
        let caps = [FilterCap::LineRange];
        let r = WireFilters::parse(&uri("file:///x?lines=0-5"), &caps);
        assert!(r.is_err());
    }

    // ---- parse: composite (limit + query together) ----

    #[test]
    fn parse_limit_and_query_composite() {
        let caps = [FilterCap::Limit { max: Some(40) }, FilterCap::TextQuery];
        let f = WireFilters::parse(&uri("mini-app://t?limit=5&query=hello"), &caps).unwrap();
        assert_eq!(f.limit, Some(5));
        assert_eq!(f.query.as_deref(), Some("hello"));
    }

    // ---- parse: no filter keys present → all None ----

    #[test]
    fn parse_no_params_returns_default() {
        let caps = [FilterCap::LineRange, FilterCap::Tail { n_max: 1000 }];
        let f = WireFilters::parse(&uri("file:///x"), &caps).unwrap();
        assert_eq!(f, WireFilters::default());
    }

    #[test]
    fn percent_decode_handles_plain_ascii() {
        assert_eq!(percent_decode("hello"), "hello");
    }
}
