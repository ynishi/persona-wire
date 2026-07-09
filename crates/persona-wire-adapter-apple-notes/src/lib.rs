//! persona-wire Adapter for the local Notes.app database (scheme `applenotes://`).
//!
//! ## Architecture
//!
//! `AppleNotesAdapter` is a stateless [`Adapter`] impl. `Notes.app`'s
//! `NoteStore.sqlite` only exists on macOS, so the crate splits into:
//!
//! - [`parse_apple_notes_uri`] — `WireUri` → `AppleNotesUriSpec` (folder
//!   filter + title substring query + row limit). Platform-independent, so
//!   it is unit-tested on every target.
//! - The `macos` module (behind `#[cfg(target_os = "macos")]`) — resolves
//!   `NoteStore.sqlite`'s on-disk path and runs the read-only filter query
//!   via `rusqlite`, including the Core Data → RFC3339 timestamp
//!   conversion.
//!
//! On non-macOS targets `fetch` returns `WireError::Storage("apple-notes
//! adapter: unsupported platform (macOS only)")` without touching the
//! filesystem, so the crate still compiles workspace-wide.
//!
//! ## URI grammar
//!
//! ```text
//! applenotes://[folder]/?query=<substring>&limit=N
//! ```
//!
//! - `folder` (the URI host) is optional; an absent or empty host means
//!   "all folders".
//! - `?query=<substring>` is a case-insensitive substring match against
//!   note titles (absent = all notes).
//! - `?limit=N` caps the number of items returned (default
//!   [`DEFAULT_LIMIT`]). A non-numeric or zero value fails loud.
//! - Unknown query keys are silently ignored (same forward-compatible
//!   convention as `persona-wire-adapter-rss` / `-obsidian`).
//!
//! ## Output shape
//!
//! ```json
//! {
//!   "folder": "<folder name>|null",
//!   "query":  "<query substring>|null",
//!   "notes": [
//!     { "id": "<Core Data primary key as string>", "title": "...|null",
//!       "folder": "...|null", "created": "<RFC3339>|null",
//!       "modified": "<RFC3339>|null" }
//!   ]
//! }
//! ```
//!
//! `notes` is ordered by `modified` descending (newest first), capped at
//! `limit`. Note bodies are out of MVP scope — Notes.app stores them as
//! gzip'd protobuf in `ZICNOTEDATA` — and may be added later via an
//! AppleScript fallback.

#![warn(missing_docs)]

use async_trait::async_trait;
use persona_wire_core::infrastructure::{adapter::Adapter, wire_uri::WireUri};
use persona_wire_core::{WireError, WireResult};

/// Default `notes` cap when `?limit=` is absent from the URI.
pub const DEFAULT_LIMIT: usize = 50;

/// persona-wire Adapter for the local Notes.app database (`applenotes://` scheme).
pub struct AppleNotesAdapter;

#[async_trait]
impl Adapter for AppleNotesAdapter {
    fn scheme(&self) -> &'static str {
        "applenotes"
    }

    /// Parse `uri` and, on macOS, run the folder / query / limit filter
    /// against `NoteStore.sqlite`. Fails loud with `WireError::Storage` on
    /// every other platform.
    async fn fetch(&self, uri: &WireUri) -> WireResult<serde_json::Value> {
        let spec = parse_apple_notes_uri(uri)?;
        fetch_impl(&spec).await
    }
}

/// Parsed `applenotes://` URI: optional folder filter, optional title
/// substring query, and a row limit.
#[derive(Debug, Clone)]
struct AppleNotesUriSpec {
    folder: Option<String>,
    query: Option<String>,
    limit: usize,
}

/// Parse a `WireUri` (already split into typed components by the registry)
/// into an [`AppleNotesUriSpec`].
///
/// - `host` (the URI authority) is the folder filter; absent or empty means
///   "all folders" (`folder: None`).
/// - `?query=<substring>` is a case-insensitive title substring filter;
///   absent means "all notes" (`query: None`).
/// - `?limit=N` must parse as a positive integer; non-numeric or `0` fails
///   loud with [`WireError::Storage`].
/// - Unknown query keys are silently ignored (forward-compatible
///   convention).
fn parse_apple_notes_uri(uri: &WireUri) -> WireResult<AppleNotesUriSpec> {
    let folder = uri.host().filter(|h| !h.is_empty()).map(|h| h.to_string());
    let query = uri.query_get("query").map(|q| q.to_string());

    let limit = match uri.query_get("limit") {
        Some(raw) => {
            let n: usize = raw.parse().map_err(|_| {
                WireError::Storage(format!(
                    "apple-notes adapter: invalid limit '{raw}' (must be a positive integer)"
                ))
            })?;
            if n == 0 {
                return Err(WireError::Storage(format!(
                    "apple-notes adapter: invalid limit '{raw}' (must be > 0)"
                )));
            }
            n
        }
        None => DEFAULT_LIMIT,
    };

    Ok(AppleNotesUriSpec {
        folder,
        query,
        limit,
    })
}

#[cfg(target_os = "macos")]
async fn fetch_impl(spec: &AppleNotesUriSpec) -> WireResult<serde_json::Value> {
    let db_path = macos::note_store_path()?;
    let spec = spec.clone();
    tokio::task::spawn_blocking(move || macos::query_notes(&db_path, &spec))
        .await
        .map_err(|e| {
            WireError::Storage(format!(
                "apple-notes adapter: blocking task join failed: {e}"
            ))
        })?
}

#[cfg(not(target_os = "macos"))]
async fn fetch_impl(_spec: &AppleNotesUriSpec) -> WireResult<serde_json::Value> {
    Err(WireError::Storage(
        "apple-notes adapter: unsupported platform (macOS only)".to_string(),
    ))
}

/// macOS-only `NoteStore.sqlite` access. Read-only; never touches the write
/// path (invariant — see crate-level docs).
#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};

    use rusqlite::{Connection, OpenFlags};
    use serde_json::json;

    use super::{AppleNotesUriSpec, WireError, WireResult};

    /// Core Data's reference epoch (`2001-01-01 00:00:00 UTC`), expressed as
    /// a UNIX epoch offset in seconds. `ZCREATIONDATE1` / `ZMODIFICATIONDATE1`
    /// store `REAL` seconds relative to this epoch.
    const CORE_DATA_EPOCH_OFFSET: i64 = 978_307_200;

    /// Resolves the on-disk path to Notes.app's `NoteStore.sqlite`:
    /// `~/Library/Group Containers/group.com.apple.notes/NoteStore.sqlite`,
    /// joined from the `HOME` environment variable (no `dirs` /
    /// `shellexpand` dependency).
    pub(super) fn note_store_path() -> WireResult<PathBuf> {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("apple-notes adapter: HOME unset".to_string()))?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Group Containers")
            .join("group.com.apple.notes")
            .join("NoteStore.sqlite"))
    }

    /// Opens `db_path` read-only and runs the folder / query / limit filter
    /// described by `spec`, returning the adapter's canonical JSON shape
    /// (see crate-level docs).
    ///
    /// Schema note: Apple Notes' on-disk schema is undocumented and can
    /// shift between macOS versions; any SQL failure fails loud via
    /// `WireError::Storage` rather than degrading silently.
    pub(super) fn query_notes(
        db_path: &Path,
        spec: &AppleNotesUriSpec,
    ) -> WireResult<serde_json::Value> {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(
            |e| {
                WireError::Storage(format!(
                    "apple-notes adapter: open {}: {e}",
                    db_path.display()
                ))
            },
        )?;

        let like_pattern = spec
            .query
            .as_ref()
            .map(|q| format!("%{}%", q.to_lowercase()));

        let mut stmt = conn
            .prepare(
                "SELECT n.Z_PK, n.ZTITLE1, f.ZTITLE2 as folder_title, \
                     n.ZCREATIONDATE1, n.ZMODIFICATIONDATE1 \
                   FROM ZICCLOUDSYNCINGOBJECT n \
                   LEFT JOIN ZICCLOUDSYNCINGOBJECT f ON n.ZFOLDER = f.Z_PK \
                  WHERE n.ZTITLE1 IS NOT NULL \
                    AND (?1 IS NULL OR f.ZTITLE2 = ?1) \
                    AND (?2 IS NULL OR LOWER(n.ZTITLE1) LIKE ?2) \
                  ORDER BY n.ZMODIFICATIONDATE1 DESC \
                  LIMIT ?3",
            )
            .map_err(|e| WireError::Storage(format!("apple-notes adapter: prepare: {e}")))?;

        let mut rows = stmt
            .query(rusqlite::params![
                spec.folder,
                like_pattern,
                spec.limit as i64
            ])
            .map_err(|e| WireError::Storage(format!("apple-notes adapter: query: {e}")))?;

        let mut notes = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| WireError::Storage(format!("apple-notes adapter: row: {e}")))?
        {
            let id: i64 = row.get(0).map_err(|e| {
                WireError::Storage(format!("apple-notes adapter: column Z_PK: {e}"))
            })?;
            let title: Option<String> = row.get(1).map_err(|e| {
                WireError::Storage(format!("apple-notes adapter: column ZTITLE1: {e}"))
            })?;
            let folder: Option<String> = row.get(2).map_err(|e| {
                WireError::Storage(format!("apple-notes adapter: column folder_title: {e}"))
            })?;
            let created: Option<f64> = row.get(3).map_err(|e| {
                WireError::Storage(format!("apple-notes adapter: column ZCREATIONDATE1: {e}"))
            })?;
            let modified: Option<f64> = row.get(4).map_err(|e| {
                WireError::Storage(format!(
                    "apple-notes adapter: column ZMODIFICATIONDATE1: {e}"
                ))
            })?;

            notes.push(json!({
                "id": id.to_string(),
                "title": title,
                "folder": folder,
                "created": created.and_then(core_data_timestamp_to_rfc3339),
                "modified": modified.and_then(core_data_timestamp_to_rfc3339),
            }));
        }

        Ok(json!({
            "folder": spec.folder,
            "query": spec.query,
            "notes": notes,
        }))
    }

    /// Converts a Core Data timestamp (`REAL` seconds since
    /// `2001-01-01 00:00:00 UTC`) to an RFC3339 string. Returns `None` when
    /// the resulting UNIX timestamp falls outside `time`'s representable
    /// range (never expected in practice for Notes.app data).
    fn core_data_timestamp_to_rfc3339(core_data_secs: f64) -> Option<String> {
        let unix_secs = CORE_DATA_EPOCH_OFFSET.checked_add(core_data_secs.trunc() as i64)?;
        let dt = time::OffsetDateTime::from_unix_timestamp(unix_secs).ok()?;
        dt.format(&time::format_description::well_known::Rfc3339)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_apple_notes_uri (platform-independent) ----

    fn parse(uri: &str) -> WireResult<AppleNotesUriSpec> {
        let wire = WireUri::parse(uri).expect("valid WireUri");
        parse_apple_notes_uri(&wire)
    }

    #[test]
    fn parse_apple_notes_uri_empty_host_means_all_folders() {
        let spec = parse("applenotes://").unwrap();
        assert_eq!(spec.folder, None);
        assert_eq!(spec.query, None);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_apple_notes_uri_host_sets_folder_filter() {
        let spec = parse("applenotes://Work/").unwrap();
        assert_eq!(spec.folder, Some("Work".to_string()));
    }

    #[test]
    fn parse_apple_notes_uri_query_param_sets_query() {
        let spec = parse("applenotes://?query=todo").unwrap();
        assert_eq!(spec.query, Some("todo".to_string()));
    }

    #[test]
    fn parse_apple_notes_uri_limit_override() {
        let spec = parse("applenotes://?limit=10").unwrap();
        assert_eq!(spec.limit, 10);
    }

    #[test]
    fn parse_apple_notes_uri_limit_non_numeric_errors() {
        let err = parse("applenotes://?limit=abc").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_apple_notes_uri_limit_zero_errors() {
        let err = parse("applenotes://?limit=0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid limit"), "unexpected error: {msg}");
    }

    #[test]
    fn parse_apple_notes_uri_unknown_query_key_ignored() {
        let spec = parse("applenotes://?utm_source=foo").unwrap();
        assert_eq!(spec.folder, None);
        assert_eq!(spec.query, None);
        assert_eq!(spec.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn parse_apple_notes_uri_combined_folder_query_limit() {
        let spec = parse("applenotes://Work/?query=todo&limit=5").unwrap();
        assert_eq!(spec.folder, Some("Work".to_string()));
        assert_eq!(spec.query, Some("todo".to_string()));
        assert_eq!(spec.limit, 5);
    }

    // ---- non-macOS stub (only compiled/run on non-macOS targets, e.g. the
    // Linux / Windows CI runners in ci.yml) ----

    #[cfg(not(target_os = "macos"))]
    mod non_macos_stub {
        use super::*;

        #[tokio::test]
        async fn fetch_returns_unsupported_platform_error() {
            let uri = WireUri::parse("applenotes://").unwrap();
            let err = AppleNotesAdapter.fetch(&uri).await.unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains("unsupported platform"),
                "unexpected error: {msg}"
            );
        }
    }

    // ---- macOS-only: query_notes against a fixture NoteStore.sqlite ----

    #[cfg(target_os = "macos")]
    mod macos_query_notes {
        use super::*;
        use rusqlite::Connection;

        /// Core Data seconds for `2001-01-01T00:00:00Z` + `delta_secs`.
        fn core_data_secs(delta_secs: f64) -> f64 {
            delta_secs
        }

        fn spec(folder: Option<&str>, query: Option<&str>, limit: usize) -> AppleNotesUriSpec {
            AppleNotesUriSpec {
                folder: folder.map(|f| f.to_string()),
                query: query.map(|q| q.to_string()),
                limit,
            }
        }

        /// Builds a fixture `NoteStore.sqlite` with the minimal
        /// `ZICCLOUDSYNCINGOBJECT` subset needed by these tests: 2
        /// folders (Work=1, Personal=2) + 4 notes (one missing dates).
        /// Returns the owning `TempDir` (drop = cleanup) and the DB path.
        fn build_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("NoteStore.sqlite");
            let conn = Connection::open(&db_path).expect("open fixture db");
            conn.execute_batch(
                "CREATE TABLE ZICCLOUDSYNCINGOBJECT (
                    Z_PK INTEGER PRIMARY KEY,
                    ZTITLE1 TEXT,
                    ZTITLE2 TEXT,
                    ZFOLDER INTEGER,
                    ZCREATIONDATE1 REAL,
                    ZMODIFICATIONDATE1 REAL
                );",
            )
            .expect("create fixture schema");

            // Folders: identified by ZTITLE2, ZTITLE1 NULL -> excluded from
            // the note filter (`WHERE n.ZTITLE1 IS NOT NULL`).
            conn.execute(
                "INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZTITLE2) VALUES (1, 'Work')",
                [],
            )
            .expect("insert folder Work");
            conn.execute(
                "INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZTITLE2) VALUES (2, 'Personal')",
                [],
            )
            .expect("insert folder Personal");

            // Notes: (Z_PK, ZTITLE1, ZFOLDER, created, modified) — dates are
            // Core Data seconds (offset from the 2001-01-01 epoch).
            conn.execute(
                "INSERT INTO ZICCLOUDSYNCINGOBJECT
                    (Z_PK, ZTITLE1, ZFOLDER, ZCREATIONDATE1, ZMODIFICATIONDATE1)
                 VALUES (10, 'Grocery List', 1, ?1, ?2)",
                rusqlite::params![core_data_secs(0.0), core_data_secs(100.0)],
            )
            .expect("insert note 10");
            conn.execute(
                "INSERT INTO ZICCLOUDSYNCINGOBJECT
                    (Z_PK, ZTITLE1, ZFOLDER, ZCREATIONDATE1, ZMODIFICATIONDATE1)
                 VALUES (11, 'Meeting Notes', 2, ?1, ?2)",
                rusqlite::params![core_data_secs(200.0), core_data_secs(900.0)],
            )
            .expect("insert note 11");
            conn.execute(
                "INSERT INTO ZICCLOUDSYNCINGOBJECT
                    (Z_PK, ZTITLE1, ZFOLDER, ZCREATIONDATE1, ZMODIFICATIONDATE1)
                 VALUES (12, 'Grocery Refill', 1, ?1, ?2)",
                rusqlite::params![core_data_secs(300.0), core_data_secs(500.0)],
            )
            .expect("insert note 12");
            // Missing dates -> both columns NULL (default when omitted).
            conn.execute(
                "INSERT INTO ZICCLOUDSYNCINGOBJECT (Z_PK, ZTITLE1, ZFOLDER) VALUES (13, 'No Dates', 2)",
                [],
            )
            .expect("insert note 13");

            (dir, db_path)
        }

        #[test]
        fn query_notes_orders_by_modified_desc_and_nulls_last() {
            let (_dir, db_path) = build_fixture();
            let v = macos::query_notes(&db_path, &spec(None, None, 50)).unwrap();
            let notes = v["notes"].as_array().unwrap();
            assert_eq!(notes.len(), 4, "all 4 notes returned (folders excluded)");
            let titles: Vec<&str> = notes.iter().map(|n| n["title"].as_str().unwrap()).collect();
            // modified desc: 900, 500, 100, then NULL (SQLite sorts NULL
            // last in DESC order).
            assert_eq!(
                titles,
                vec![
                    "Meeting Notes",
                    "Grocery Refill",
                    "Grocery List",
                    "No Dates"
                ]
            );
        }

        #[test]
        fn query_notes_limit_truncates() {
            let (_dir, db_path) = build_fixture();
            let v = macos::query_notes(&db_path, &spec(None, None, 2)).unwrap();
            let notes = v["notes"].as_array().unwrap();
            assert_eq!(notes.len(), 2, "limit=2 truncates to two newest notes");
            assert_eq!(notes[0]["title"], "Meeting Notes");
            assert_eq!(notes[1]["title"], "Grocery Refill");
        }

        #[test]
        fn query_notes_folder_filter() {
            let (_dir, db_path) = build_fixture();
            let v = macos::query_notes(&db_path, &spec(Some("Work"), None, 50)).unwrap();
            assert_eq!(v["folder"], "Work");
            let notes = v["notes"].as_array().unwrap();
            let titles: Vec<&str> = notes.iter().map(|n| n["title"].as_str().unwrap()).collect();
            assert_eq!(
                titles,
                vec!["Grocery Refill", "Grocery List"],
                "only Work-folder notes, modified desc"
            );
            for n in notes {
                assert_eq!(n["folder"], "Work");
            }
        }

        #[test]
        fn query_notes_query_substring_filter_case_insensitive() {
            let (_dir, db_path) = build_fixture();
            let v = macos::query_notes(&db_path, &spec(None, Some("GROCERY"), 50)).unwrap();
            let notes = v["notes"].as_array().unwrap();
            let titles: Vec<&str> = notes.iter().map(|n| n["title"].as_str().unwrap()).collect();
            assert_eq!(
                titles,
                vec!["Grocery Refill", "Grocery List"],
                "case-insensitive substring match, modified desc"
            );
        }

        #[test]
        fn query_notes_missing_dates_are_null() {
            let (_dir, db_path) = build_fixture();
            let v = macos::query_notes(&db_path, &spec(None, Some("no dates"), 50)).unwrap();
            let notes = v["notes"].as_array().unwrap();
            assert_eq!(notes.len(), 1);
            assert!(notes[0]["created"].is_null());
            assert!(notes[0]["modified"].is_null());
        }

        #[test]
        fn query_notes_core_data_timestamp_converts_to_rfc3339() {
            let (_dir, db_path) = build_fixture();
            let v = macos::query_notes(&db_path, &spec(None, Some("grocery list"), 50)).unwrap();
            let notes = v["notes"].as_array().unwrap();
            assert_eq!(notes.len(), 1);
            let created = notes[0]["created"].as_str().expect("created is a string");
            // core_data_secs(0.0) == the Core Data epoch itself == 2001-01-01T00:00:00Z.
            assert!(
                created.starts_with("2001-01-01T00:00:00"),
                "unexpected created timestamp: {created}"
            );
            assert!(created.ends_with('Z'), "expected UTC 'Z' suffix: {created}");
            let modified = notes[0]["modified"].as_str().expect("modified is a string");
            // core_data_secs(100.0) == epoch + 100s == 2001-01-01T00:01:40Z.
            assert!(
                modified.starts_with("2001-01-01T00:01:40"),
                "unexpected modified timestamp: {modified}"
            );
        }
    }
}
