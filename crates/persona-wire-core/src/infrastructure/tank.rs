//! Tank Adapter — reads the persisted observation item log as a Source.
//!
//! ## Architecture
//!
//! `wire_materialize` fetches an upstream Source, shreds the response into
//! items, dedups them against the existing timeline, and appends them to the
//! Tank (two SQLite tables owned by [`crate::infrastructure::storage`]). The
//! `tank://` scheme closes the loop: a Tank is itself a Source ("a Source that
//! serves past observations"), so `wire_fetch` / `wire_prompt_context` read it
//! through the same [`crate::infrastructure::adapter::Adapter`] dispatch as any
//! live Source — live vs archive is a `source_uri` swap, not a render-path
//! branch. Unlike the stateless bundled `FileAdapter`, this adapter holds an
//! `Arc<Mutex<SqliteStorage>>` (the same shared store the MCP server built at
//! boot), mirroring the graph-backed `McpAdapter::new(resolver)` precedent.
//!
//! ## URI grammar
//!
//! ```text
//! tank://<persona>/<slot>[?since=&until=&limit=&tail_n=&query=]
//! ```
//!
//! - `host` = persona, `path` = `/<slot>`; the tank key is `"<persona>/<slot>"`.
//! - An empty host, empty slot, or multi-segment path (`/a/b`) fails loud.
//! - Declared [`crate::infrastructure::filter::FilterCap`]s (resolved in SQL,
//!   not post-fetch): `limit` (max 1000), `tail_n` (max 1000), `since` /
//!   `until`, `query`. `tail=last_section` is a document filter and fails loud
//!   (the Tank serves an item timeline, not a document body). `limit` and
//!   `tail_n` together fail loud (mutually exclusive).
//! - `since` / `until` accept three forms: a relative offset from now
//!   (`-30d` / `-24h` / `-15m` / `-60s` / `-2w`), an integer epoch-seconds
//!   value, or an RFC3339 timestamp. Both bounds are `observed_at`-based.
//! - A non-existent tank is graceful: an empty `items` list with `Ok`.
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "scheme": "tank",
//!   "kind": "tank_items",
//!   "tank": "<persona>/<slot>",
//!   "count": <N>,
//!   "has_more": <bool>,
//!   "items": [
//!     {
//!       "uri": "tank://<persona>/<slot>#<identity>",
//!       "mimeType": "application/json",
//!       "identity": "<identity>",
//!       "observed_at": <epoch seconds>,
//!       "annotations": { "lastModified": "<RFC3339>" },
//!       "payload": <item JSON verbatim>
//!     }
//!   ]
//! }
//! ```

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::adapter::Adapter;
use crate::infrastructure::filter::{FilterCap, TailSpec, WireFilters};
use crate::infrastructure::storage::{SqliteStorage, TankQuery};
use crate::infrastructure::wire_uri::WireUri;

/// Cross-cutting filter vocabulary the Tank interprets in SQL. `LineRange` is
/// intentionally absent — the Tank serves an item list, not a document body.
const TANK_FILTER_CAPS: &[FilterCap] = &[
    FilterCap::Limit { max: Some(1000) },
    FilterCap::Tail { n_max: 1000 },
    FilterCap::SinceUntil,
    FilterCap::TextQuery,
];

/// `tank://<persona>/<slot>` [`Adapter`] over the persisted observation log.
/// Holds the shared store so `fetch` can query the Tank tables. See the
/// module-level docs for the URI grammar and output shape.
pub struct TankAdapter {
    storage: Arc<Mutex<SqliteStorage>>,
}

impl TankAdapter {
    /// Build a Tank adapter over the shared storage handle (the same
    /// `Arc<Mutex<SqliteStorage>>` the MCP server passes to its other
    /// stateful plugins).
    pub fn new(storage: Arc<Mutex<SqliteStorage>>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl Adapter for TankAdapter {
    fn scheme(&self) -> &'static str {
        "tank"
    }

    fn filter_caps(&self) -> &'static [FilterCap] {
        TANK_FILTER_CAPS
    }

    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let persona = uri.host().filter(|h| !h.is_empty()).ok_or_else(|| {
            WireError::Storage(format!(
                "tank adapter: uri must be tank://<persona>/<slot>, missing persona: {uri}"
            ))
        })?;
        let path = uri.path();
        let slot = path.strip_prefix('/').unwrap_or(path);
        if slot.is_empty() || slot.contains('/') {
            return Err(WireError::Storage(format!(
                "tank adapter: uri must be tank://<persona>/<slot> (single-segment slot), \
                 got path '{path}'"
            )));
        }
        let tank_key = format!("{persona}/{slot}");

        let filters = WireFilters::parse(uri, TANK_FILTER_CAPS)?;

        // The Tank serves an item timeline, not a document body.
        if matches!(filters.tail, Some(TailSpec::LastSection)) {
            return Err(WireError::Storage(
                "tail=last_section is not supported by tank (item timeline)".to_string(),
            ));
        }
        let tail_n = match &filters.tail {
            Some(TailSpec::LastN(n)) => Some(*n),
            _ => None,
        };
        if filters.limit.is_some() && tail_n.is_some() {
            return Err(WireError::Storage(
                "limit and tail_n are mutually exclusive".to_string(),
            ));
        }

        let since = filters
            .since
            .as_deref()
            .map(resolve_time_bound)
            .transpose()?;
        let until = filters
            .until
            .as_deref()
            .map(resolve_time_bound)
            .transpose()?;

        let query = TankQuery {
            since,
            until,
            query: filters.query.clone(),
            limit: filters.limit,
            tail_n,
        };

        let (items, has_more) = {
            let s = self.storage.lock().map_err(|_| {
                WireError::Storage("tank adapter: storage mutex poisoned".to_string())
            })?;
            s.tank_query_items(&tank_key, &query)?
        };

        let items_json: Vec<serde_json::Value> = items
            .iter()
            .map(|it| {
                serde_json::json!({
                    "uri": format!("tank://{tank_key}#{}", it.identity),
                    "mimeType": it.mime_type,
                    "identity": it.identity,
                    "observed_at": it.observed_at,
                    "annotations": { "lastModified": epoch_to_rfc3339(it.observed_at) },
                    "payload": it.payload,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "scheme": "tank",
            "kind": "tank_items",
            "tank": tank_key,
            "count": items_json.len(),
            "has_more": has_more,
            "items": items_json,
        }))
    }
}

/// Resolve a `?since=` / `?until=` value to epoch seconds. Accepts a relative
/// offset (`-30d` / `-24h` / `-15m` / `-60s` / `-2w`), an integer epoch value,
/// or an RFC3339 timestamp. Fails loud on any other form.
fn resolve_time_bound(raw: &str) -> WireResult<i64> {
    // Relative form: `-<N><unit>`, unit ∈ {s, m, h, d, w}.
    if let Some(rest) = raw.strip_prefix('-') {
        if let Some(unit) = rest.chars().last() {
            if unit.is_ascii_alphabetic() {
                let num_part = &rest[..rest.len() - unit.len_utf8()];
                let n: i64 = num_part.parse().map_err(|_| {
                    WireError::Storage(format!("tank adapter: invalid relative time '{raw}'"))
                })?;
                let secs = match unit {
                    's' => n,
                    'm' => n * 60,
                    'h' => n * 3600,
                    'd' => n * 86_400,
                    'w' => n * 604_800,
                    other => {
                        return Err(WireError::Storage(format!(
                            "tank adapter: unknown time unit '{other}' in '{raw}' \
                             (expected one of s/m/h/d/w)"
                        )))
                    }
                };
                return Ok(current_epoch_secs()? - secs);
            }
        }
    }

    // Integer epoch seconds.
    if let Ok(epoch) = raw.parse::<i64>() {
        return Ok(epoch);
    }

    // RFC3339 timestamp.
    OffsetDateTime::parse(raw, &Rfc3339)
        .map(|dt| dt.unix_timestamp())
        .map_err(|e| {
            WireError::Storage(format!(
                "tank adapter: invalid time '{raw}' (expected a relative offset like -30d, \
                 integer epoch seconds, or an RFC3339 timestamp): {e}"
            ))
        })
}

/// Format epoch seconds as an RFC3339 string, or an empty string when the
/// value is out of range (best-effort annotation, never a hard error).
fn epoch_to_rfc3339(epoch: i64) -> String {
    OffsetDateTime::from_unix_timestamp(epoch)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn current_epoch_secs() -> WireResult<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| WireError::Other(format!("system clock before unix epoch: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graph::Ulid;
    use crate::infrastructure::storage::{TankItemRecord, TankSnapshotRecord};

    fn shared_store() -> Arc<Mutex<SqliteStorage>> {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        Arc::new(Mutex::new(s))
    }

    fn seed_items(store: &Arc<Mutex<SqliteStorage>>, key: &str, items: &[(&str, i64, &str)]) {
        let s = store.lock().unwrap();
        let snap_id = Ulid::new();
        s.tank_insert_snapshot(&TankSnapshotRecord {
            id: snap_id,
            tank_key: key.into(),
            source_uri: "rss://example.com/feed".into(),
            fetched_at: 1000,
            filters_applied: serde_json::json!({}),
            content_hash: "h".into(),
            item_count: items.len(),
            new_item_count: items.len(),
        })
        .unwrap();
        let records: Vec<TankItemRecord> = items
            .iter()
            .map(|(id, observed_at, body)| TankItemRecord {
                id: Ulid::new(),
                identity: (*id).into(),
                observed_at: *observed_at,
                payload: serde_json::json!({"body": body}),
                mime_type: "application/json".into(),
            })
            .collect();
        s.tank_append_items(key, &snap_id, &records).unwrap();
    }

    #[test]
    fn scheme_and_filter_caps() {
        let a = TankAdapter::new(shared_store());
        assert_eq!(a.scheme(), "tank");
        let caps = a.filter_caps();
        assert!(caps.contains(&FilterCap::SinceUntil));
        assert!(caps.contains(&FilterCap::TextQuery));
        assert!(caps.contains(&FilterCap::Tail { n_max: 1000 }));
        assert!(!caps.contains(&FilterCap::LineRange));
    }

    #[tokio::test]
    async fn empty_tank_is_graceful() {
        let a = TankAdapter::new(shared_store());
        let uri = WireUri::parse("tank://alpha/mailbox").unwrap();
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["kind"], "tank_items");
        assert_eq!(v["tank"], "alpha/mailbox");
        assert_eq!(v["count"], 0);
        assert_eq!(v["has_more"], false);
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn rejects_missing_persona() {
        let a = TankAdapter::new(shared_store());
        // `tank:///mailbox` → empty host.
        let uri = WireUri::parse("tank:///mailbox").unwrap();
        assert!(a.fetch(&uri).await.is_err());
    }

    #[tokio::test]
    async fn rejects_multi_segment_path() {
        let a = TankAdapter::new(shared_store());
        let uri = WireUri::parse("tank://alpha/mailbox/extra").unwrap();
        assert!(a.fetch(&uri).await.is_err());
    }

    #[tokio::test]
    async fn tail_last_section_fails_loud() {
        let a = TankAdapter::new(shared_store());
        let uri = WireUri::parse("tank://alpha/mailbox?tail=last_section").unwrap();
        let err = a
            .fetch(&uri)
            .await
            .expect_err("document tail is unsupported");
        assert!(err.to_string().contains("last_section"), "err: {err}");
    }

    #[tokio::test]
    async fn limit_and_tail_n_together_fail_loud() {
        let a = TankAdapter::new(shared_store());
        let uri = WireUri::parse("tank://alpha/mailbox?limit=2&tail_n=1").unwrap();
        let err = a
            .fetch(&uri)
            .await
            .expect_err("limit + tail_n is exclusive");
        assert!(err.to_string().contains("mutually exclusive"), "err: {err}");
    }

    #[tokio::test]
    async fn tail_n_fetches_last_items() {
        let store = shared_store();
        seed_items(
            &store,
            "alpha/mailbox",
            &[("a", 100, "a"), ("b", 200, "b"), ("c", 300, "c")],
        );
        let a = TankAdapter::new(store);
        let uri = WireUri::parse("tank://alpha/mailbox?tail_n=2").unwrap();
        let v = a.fetch(&uri).await.unwrap();
        assert_eq!(v["count"], 2);
        assert_eq!(v["has_more"], true);
        let items = v["items"].as_array().unwrap();
        assert_eq!(items[0]["identity"], "b");
        assert_eq!(items[1]["identity"], "c");
        // Output shape checks.
        assert_eq!(items[1]["mimeType"], "application/json");
        assert_eq!(items[1]["uri"], "tank://alpha/mailbox#c");
        assert_eq!(items[1]["payload"], serde_json::json!({"body": "c"}));
        assert!(items[1]["annotations"]["lastModified"].is_string());
    }

    #[test]
    fn resolve_time_bound_relative_days() {
        let now = current_epoch_secs().unwrap();
        let resolved = resolve_time_bound("-1d").unwrap();
        // Within a small delta of now - 86400 (the two `now` reads differ by <1s).
        assert!((now - 86_400 - resolved).abs() <= 2, "resolved={resolved}");
    }

    #[test]
    fn resolve_time_bound_relative_units() {
        let now = current_epoch_secs().unwrap();
        for (spec, secs) in [
            ("-60s", 60),
            ("-15m", 900),
            ("-24h", 86_400),
            ("-2w", 1_209_600),
        ] {
            let resolved = resolve_time_bound(spec).unwrap();
            assert!(
                (now - secs - resolved).abs() <= 2,
                "spec={spec} resolved={resolved}"
            );
        }
    }

    #[test]
    fn resolve_time_bound_epoch() {
        assert_eq!(resolve_time_bound("1700000000").unwrap(), 1_700_000_000);
    }

    #[test]
    fn resolve_time_bound_rfc3339() {
        let resolved = resolve_time_bound("2023-11-14T22:13:20Z").unwrap();
        assert_eq!(resolved, 1_700_000_000);
    }

    #[test]
    fn resolve_time_bound_invalid_fails_loud() {
        assert!(resolve_time_bound("not-a-time").is_err());
        assert!(resolve_time_bound("-30y").is_err(), "unknown unit");
    }

    #[tokio::test]
    async fn since_relative_filters_timeline() {
        let store = shared_store();
        let now = current_epoch_secs().unwrap();
        // one recent (within 1h), one stale (2d old)
        seed_items(
            &store,
            "alpha/mailbox",
            &[("old", now - 2 * 86_400, "old"), ("new", now - 60, "new")],
        );
        let a = TankAdapter::new(store);
        let uri = WireUri::parse("tank://alpha/mailbox?since=-1d").unwrap();
        let v = a.fetch(&uri).await.unwrap();
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "only the recent item survives since=-1d");
        assert_eq!(items[0]["identity"], "new");
    }
}
