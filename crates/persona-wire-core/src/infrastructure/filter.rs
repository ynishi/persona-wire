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
//! | `limit=0` (list-shaped filter with no useful semantics for zero) | `Err` |
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
    /// Applies the document-shaped text filters (`line_range` / `tail`) to
    /// `body` and returns the resulting substring. `line_range` takes
    /// precedence over `tail` (parse validates the two are mutually
    /// exclusive); neither present returns `body` unchanged
    /// (backward-compat whole-body fetch).
    ///
    /// Shared text-slicing engine for every adapter declaring
    /// [`FilterCap::LineRange`] / [`FilterCap::Tail`] — promoted from the
    /// former `FileAdapter`-private helpers so document adapters (file /
    /// obsidian / future ones) share one semantics (GH #6 Phase 3).
    pub fn apply_to_text(&self, body: &str) -> String {
        if let Some((from, to)) = self.line_range {
            apply_lines(body, from, to)
        } else {
            apply_tail(body, self.tail.as_ref())
        }
    }

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
                .map_err(|_| invalid_value("limit", raw, "a positive integer"))?;
            if n == 0 {
                return Err(invalid_value("limit", raw, "a positive integer (> 0)"));
            }
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

    /// Splits the vocabulary keys requested in `uri` into "native" (declared
    /// in `caps`, left in the URI for the adapter's own
    /// [`parse`](WireFilters::parse)) and "post" (requested but undeclared,
    /// AND wire-applicable) sets. Returns `None` when no post-set key is
    /// requested — the fetch proceeds exactly as before GH #10.
    ///
    /// Wire-applicable capabilities are [`FilterCap::TextQuery`] (applied to
    /// list-shaped `items[]` responses), [`FilterCap::LineRange`] and
    /// [`FilterCap::Tail`] (applied to document-shaped `body` strings via
    /// [`apply_to_text`](WireFilters::apply_to_text)). Undeclared
    /// [`FilterCap::Limit`] / [`FilterCap::SinceUntil`] requests are NOT
    /// split — they stay in the URI and keep failing loud in the adapter's
    /// own parse (limit interacts with upstream pagination and since/until
    /// needs a per-shape timestamp path; see GH #10 "Out" scope).
    ///
    /// Post-set values are validated with the same grammar / error policy as
    /// native parsing (wrong types and `FROM > TO` fail loud here, before
    /// any fetch happens).
    pub fn split_post(uri: &WireUri, caps: &[FilterCap]) -> WireResult<Option<PostFilterPlan>> {
        let declared_lines = caps.iter().any(|c| matches!(c, FilterCap::LineRange));
        let declared_tail = caps.iter().any(|c| matches!(c, FilterCap::Tail { .. }));
        let declared_query = caps.iter().any(|c| matches!(c, FilterCap::TextQuery));

        let post_lines = uri.query_get("lines").is_some() && !declared_lines;
        let requested_tail_key = if uri.query_get("tail").is_some() {
            Some("tail")
        } else if uri.query_get("tail_n").is_some() {
            Some("tail_n")
        } else {
            None
        };
        let post_tail = requested_tail_key.is_some() && !declared_tail;
        let post_query = uri.query_get("query").is_some() && !declared_query;

        if !(post_lines || post_tail || post_query) {
            return Ok(None);
        }

        // Validate post values through the shared parser by augmenting the
        // declared caps with the wire-applicable set. Undeclared limit /
        // since / until requests still fail loud inside this parse — the
        // augmentation never adds those caps.
        let mut augmented: Vec<FilterCap> = caps.to_vec();
        if !declared_lines {
            augmented.push(FilterCap::LineRange);
        }
        if !declared_tail {
            augmented.push(FilterCap::Tail {
                n_max: crate::infrastructure::adapter::TAIL_N_MAX,
            });
        }
        if !declared_query {
            augmented.push(FilterCap::TextQuery);
        }
        let full = WireFilters::parse(uri, &augmented)?;

        let mut filters = WireFilters::default();
        let mut strip_keys: Vec<&'static str> = Vec::new();
        let mut applied_keys: Vec<&'static str> = Vec::new();
        if post_lines {
            filters.line_range = full.line_range;
            strip_keys.push("lines");
            applied_keys.push("lines");
        }
        if post_tail {
            filters.tail = full.tail.clone();
            strip_keys.push("tail");
            strip_keys.push("tail_n");
            applied_keys.push(requested_tail_key.expect("post_tail implies a requested key"));
        }
        if post_query {
            filters.query = full.query.clone();
            strip_keys.push("query");
            applied_keys.push("query");
        }
        // Same mutual-exclusion rule the document adapters enforce natively,
        // applied to the post set (native/post cross-pairs are allowed: the
        // native slice happens in the adapter, the post slice on its result).
        if filters.line_range.is_some() && filters.tail.is_some() {
            return Err(WireError::Storage(
                "lines and tail are mutually exclusive".to_string(),
            ));
        }
        Ok(Some(PostFilterPlan {
            filters,
            strip_keys,
            applied_keys,
        }))
    }
}

/// Wire-layer post-filter plan produced by [`WireFilters::split_post`] (GH
/// #10): the subset of requested filters the adapter did not declare but the
/// wire layer applies in memory after the fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostFilterPlan {
    /// The post-set filters. Only wire-applicable fields are ever populated
    /// (`line_range` / `tail` / `query`) — never `limit` / `since` / `until`.
    pub filters: WireFilters,
    /// Vocabulary query keys to strip from the raw URI before adapter
    /// dispatch (so the adapter's own parse never sees the post-set keys).
    pub strip_keys: Vec<&'static str>,
    /// The requested key spellings, recorded verbatim into the
    /// `post_filtered` response marker after application.
    pub applied_keys: Vec<&'static str>,
}

impl PostFilterPlan {
    /// Applies the post-set filters to the adapter's response `value` in
    /// place and inserts the `post_filtered: [<keys>]` marker.
    ///
    /// Shape contract (fail loud on mismatch — a requested filter is never
    /// silently ignored):
    /// - `query` requires a list-shaped response: a top-level `items` array.
    ///   Items are retained when any string leaf (case-insensitive) contains
    ///   the query text.
    /// - `lines` / `tail` require a document-shaped response: a top-level
    ///   `body` string, sliced via [`WireFilters::apply_to_text`].
    /// - The response must be a JSON object (marker insertion target).
    ///
    /// Under-fill semantics: post-filters apply AFTER the adapter's native
    /// `limit` / pagination, so a limited fetch may return fewer than
    /// `limit` items once narrowed — by design, documented, not compensated.
    pub fn apply(&self, value: &mut serde_json::Value) -> WireResult<()> {
        if value.as_object().is_none() {
            return Err(WireError::Storage(format!(
                "wire post-filter ({}) requires a JSON-object response, got {}",
                self.applied_keys.join(", "),
                json_type_name(value)
            )));
        }

        if let Some(q) = &self.filters.query {
            let items = value
                .get_mut("items")
                .and_then(|v| v.as_array_mut())
                .ok_or_else(|| {
                    WireError::Storage(
                        "wire post-filter 'query' requires a list-shaped response \
                         (top-level `items` array) — this adapter's response has none"
                            .to_string(),
                    )
                })?;
            let needle = q.to_lowercase();
            items.retain(|item| json_contains_text(item, &needle));
        }

        if self.filters.line_range.is_some() || self.filters.tail.is_some() {
            let body = value
                .get("body")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    WireError::Storage(
                        "wire post-filter 'lines'/'tail' requires a document-shaped \
                         response (top-level `body` string) — this adapter's response \
                         has none"
                            .to_string(),
                    )
                })?;
            let sliced = self.filters.apply_to_text(&body);
            value["body"] = serde_json::Value::String(sliced);
        }

        let obj = value
            .as_object_mut()
            .expect("checked to be an object above");
        obj.insert(
            "post_filtered".to_string(),
            serde_json::Value::Array(
                self.applied_keys
                    .iter()
                    .map(|k| serde_json::Value::String((*k).to_string()))
                    .collect(),
            ),
        );
        Ok(())
    }
}

/// Case-insensitive substring search over every string leaf of `v`
/// (`needle_lower` must already be lowercased).
fn json_contains_text(v: &serde_json::Value, needle_lower: &str) -> bool {
    match v {
        serde_json::Value::String(s) => s.to_lowercase().contains(needle_lower),
        serde_json::Value::Array(a) => a.iter().any(|x| json_contains_text(x, needle_lower)),
        serde_json::Value::Object(o) => o.values().any(|x| json_contains_text(x, needle_lower)),
        _ => false,
    }
}

/// Short JSON type label for post-filter shape-mismatch errors.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
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

/// Applies `tail` to `body` and returns the resulting substring.
///
/// - `None`                    — returns `body` unchanged
/// - [`TailSpec::LastSection`] — returns from the last `## ` h2 heading line to the end
/// - [`TailSpec::LastN`]       — returns the last N lines joined with `"\n"`
fn apply_tail(body: &str, tail: Option<&TailSpec>) -> String {
    match tail {
        None => body.to_string(),
        Some(TailSpec::LastSection) => {
            let pos = last_h2_pos(body);
            body[pos..].to_string()
        }
        Some(TailSpec::LastN(n)) => {
            let lines: Vec<&str> = body.lines().collect();
            let skip = lines.len().saturating_sub(*n);
            lines[skip..].join("\n")
        }
    }
}

/// Applies `?lines=FROM-TO` (1-origin inclusive) to `body`. `to` is clamped
/// to the total line count (graceful over-range); `from` beyond the total
/// line count returns an empty string.
fn apply_lines(body: &str, from: usize, to: usize) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let total = lines.len();
    if from > total {
        return String::new();
    }
    let start = from - 1;
    let end = to.min(total);
    lines[start..end].join("\n")
}

/// Returns the byte position of the last markdown h2 heading (a line starting
/// with `## `) in `body`. Returns `0` when none is found (= return the whole body).
fn last_h2_pos(body: &str) -> usize {
    let needle = "\n## ";
    if let Some(pos) = body.rfind(needle) {
        // Return from the byte after `\n` (= the leading `#`)
        return pos + 1;
    }
    if body.starts_with("## ") {
        return 0;
    }
    0
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

    // ---- apply_to_text: shared text-slicing engine ----
    // (moved from the FileAdapter-private helpers with the GH #6 Phase 3
    // promotion; behavior must stay byte-identical)

    #[test]
    fn apply_to_text_no_filter_returns_body_unchanged() {
        let body = "a\nb\nc\n";
        assert_eq!(WireFilters::default().apply_to_text(body), body);
    }

    #[test]
    fn apply_to_text_lines_normal_range() {
        let f = WireFilters {
            line_range: Some((2, 4)),
            ..Default::default()
        };
        assert_eq!(f.apply_to_text("a\nb\nc\nd\ne\n"), "b\nc\nd");
    }

    #[test]
    fn apply_to_text_lines_to_beyond_total_clamps() {
        let f = WireFilters {
            line_range: Some((1, 100)),
            ..Default::default()
        };
        assert_eq!(f.apply_to_text("a\nb\nc\n"), "a\nb\nc");
    }

    #[test]
    fn apply_to_text_lines_from_beyond_total_returns_empty() {
        let f = WireFilters {
            line_range: Some((10, 20)),
            ..Default::default()
        };
        assert_eq!(f.apply_to_text("a\nb\n"), "");
    }

    #[test]
    fn apply_to_text_tail_last_n_returns_last_lines() {
        let f = WireFilters {
            tail: Some(TailSpec::LastN(3)),
            ..Default::default()
        };
        assert_eq!(f.apply_to_text("a\nb\nc\nd\ne\n"), "c\nd\ne");
    }

    #[test]
    fn apply_to_text_tail_last_n_exceeding_line_count_returns_all() {
        let f = WireFilters {
            tail: Some(TailSpec::LastN(1000)),
            ..Default::default()
        };
        assert_eq!(f.apply_to_text("x\ny\n"), "x\ny");
    }

    #[test]
    fn apply_to_text_tail_last_section_returns_from_last_h2() {
        let f = WireFilters {
            tail: Some(TailSpec::LastSection),
            ..Default::default()
        };
        let body = "# Title\n\n## S1\n\nContent\n\n## S2\n\nEnd\n";
        assert!(f.apply_to_text(body).starts_with("## S2"));
    }

    #[test]
    fn apply_to_text_tail_last_section_no_h2_returns_whole_body() {
        let f = WireFilters {
            tail: Some(TailSpec::LastSection),
            ..Default::default()
        };
        let body = "No heading here\n";
        assert_eq!(f.apply_to_text(body), body);
    }

    #[test]
    fn apply_to_text_tail_last_section_h2_at_start() {
        let f = WireFilters {
            tail: Some(TailSpec::LastSection),
            ..Default::default()
        };
        let body = "## Only\n\nContent\n";
        assert_eq!(f.apply_to_text(body), body);
    }

    #[test]
    fn apply_to_text_line_range_takes_precedence_over_tail() {
        let f = WireFilters {
            line_range: Some((1, 1)),
            tail: Some(TailSpec::LastN(2)),
            ..Default::default()
        };
        assert_eq!(f.apply_to_text("a\nb\nc\n"), "a");
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
    fn parse_limit_zero_errs() {
        let caps = [FilterCap::Limit { max: Some(40) }];
        let r = WireFilters::parse(&uri("mini-app://t?limit=0"), &caps);
        assert!(r.is_err());
        let msg = format!("{}", r.unwrap_err());
        assert!(msg.contains("limit"), "message should name the key: {msg}");
        assert!(
            msg.contains("positive"),
            "message should say positive: {msg}"
        );
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

    // ---- split_post / PostFilterPlan (GH #10 wire-layer post-filter) ----

    #[test]
    fn split_post_none_when_no_vocab_keys_requested() {
        let plan = WireFilters::split_post(&uri("rss://example.com/feed"), &[]).unwrap();
        assert_eq!(plan, None);
    }

    #[test]
    fn split_post_none_when_all_requested_keys_are_declared() {
        let caps = [FilterCap::Limit { max: None }, FilterCap::TextQuery];
        let plan =
            WireFilters::split_post(&uri("mini-app://t?limit=5&query=hello"), &caps).unwrap();
        assert_eq!(plan, None, "native-capability requests must not split");
    }

    #[test]
    fn split_post_claims_undeclared_query_and_keeps_native_limit() {
        let caps = [FilterCap::Limit { max: None }];
        let plan = WireFilters::split_post(&uri("rss://h/feed?limit=3&query=Foo%20bar"), &caps)
            .unwrap()
            .expect("undeclared query must produce a post plan");
        assert_eq!(plan.filters.query.as_deref(), Some("Foo bar"));
        assert_eq!(
            plan.filters.limit, None,
            "native limit stays out of the post set"
        );
        assert_eq!(plan.strip_keys, vec!["query"]);
        assert_eq!(plan.applied_keys, vec!["query"]);
    }

    #[test]
    fn split_post_undeclared_limit_still_fails_loud() {
        let r = WireFilters::split_post(&uri("note://h?limit=3&query=a"), &[]);
        let err = r.expect_err("undeclared limit is not post-applicable");
        assert!(err.to_string().contains("limit"), "err: {err}");
    }

    #[test]
    fn split_post_undeclared_since_until_still_fails_loud() {
        let r = WireFilters::split_post(&uri("note://h?since=2026-01-01&query=a"), &[]);
        let err = r.expect_err("undeclared since is not post-applicable");
        assert!(err.to_string().contains("since"), "err: {err}");
    }

    #[test]
    fn split_post_validates_post_values_with_shared_grammar() {
        let r = WireFilters::split_post(&uri("note://h?lines=abc"), &[]);
        assert!(
            r.is_err(),
            "malformed post value must fail loud before fetch"
        );
    }

    #[test]
    fn split_post_post_lines_and_tail_mutually_exclusive() {
        let r = WireFilters::split_post(&uri("note://h?lines=1-2&tail_n=3"), &[]);
        let err = r.expect_err("post lines+tail must reject");
        assert!(err.to_string().contains("mutually exclusive"), "err: {err}");
    }

    #[test]
    fn split_post_tail_n_records_requested_key_and_clamps() {
        let plan = WireFilters::split_post(&uri("note://h?tail_n=5000"), &[])
            .unwrap()
            .expect("undeclared tail_n must produce a post plan");
        assert_eq!(
            plan.filters.tail,
            Some(TailSpec::LastN(1000)),
            "clamped to TAIL_N_MAX"
        );
        assert_eq!(plan.applied_keys, vec!["tail_n"]);
        assert!(plan.strip_keys.contains(&"tail") && plan.strip_keys.contains(&"tail_n"));
    }

    #[test]
    fn split_post_native_line_range_with_post_query_splits_only_query() {
        let caps = [FilterCap::LineRange];
        let plan = WireFilters::split_post(&uri("note://h?lines=1-2&query=x"), &caps)
            .unwrap()
            .expect("query is post");
        assert_eq!(plan.filters.line_range, None, "declared lines stays native");
        assert_eq!(plan.strip_keys, vec!["query"]);
    }

    fn plan_query(q: &str) -> PostFilterPlan {
        PostFilterPlan {
            filters: WireFilters {
                query: Some(q.to_string()),
                ..WireFilters::default()
            },
            strip_keys: vec!["query"],
            applied_keys: vec!["query"],
        }
    }

    #[test]
    fn apply_query_filters_items_case_insensitive_and_marks() {
        let mut v = serde_json::json!({
            "scheme": "rss",
            "items": [
                {"title": "Rust 1.90 released", "tags": ["lang"]},
                {"title": "unrelated", "tags": ["misc"]},
                {"title": "nested", "meta": {"note": "about RUST too"}},
            ],
            "has_more": false,
        });
        plan_query("rust").apply(&mut v).unwrap();
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "case-insensitive match over string leaves");
        assert_eq!(v["post_filtered"], serde_json::json!(["query"]));
        assert_eq!(
            v["has_more"],
            serde_json::json!(false),
            "other fields untouched"
        );
    }

    #[test]
    fn apply_query_requires_items_array() {
        let mut v = serde_json::json!({"scheme": "file", "body": "text"});
        let err = plan_query("x")
            .apply(&mut v)
            .expect_err("no items[] must fail loud");
        assert!(err.to_string().contains("items"), "err: {err}");
    }

    #[test]
    fn apply_lines_slices_body_and_marks() {
        let plan = PostFilterPlan {
            filters: WireFilters {
                line_range: Some((2, 3)),
                ..WireFilters::default()
            },
            strip_keys: vec!["lines"],
            applied_keys: vec!["lines"],
        };
        let mut v = serde_json::json!({"scheme": "x", "body": "a\nb\nc\nd"});
        plan.apply(&mut v).unwrap();
        assert_eq!(v["body"], serde_json::json!("b\nc"));
        assert_eq!(v["post_filtered"], serde_json::json!(["lines"]));
    }

    #[test]
    fn apply_lines_requires_body_string() {
        let plan = PostFilterPlan {
            filters: WireFilters {
                line_range: Some((1, 2)),
                ..WireFilters::default()
            },
            strip_keys: vec!["lines"],
            applied_keys: vec!["lines"],
        };
        let mut v = serde_json::json!({"scheme": "x", "body": null});
        let err = plan.apply(&mut v).expect_err("null body must fail loud");
        assert!(err.to_string().contains("body"), "err: {err}");
    }

    #[test]
    fn apply_rejects_non_object_response() {
        let mut v = serde_json::json!(["bare", "array"]);
        let err = plan_query("x")
            .apply(&mut v)
            .expect_err("non-object must fail loud");
        assert!(err.to_string().contains("array"), "err: {err}");
    }
}
