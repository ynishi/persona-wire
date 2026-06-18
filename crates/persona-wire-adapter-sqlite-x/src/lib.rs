//! persona-wire Adapter for raw SQLite SoT (scheme `sqlite://`).
//!
//! Single-binary OSS distribution / Fly.io self-hosting (P4 roadmap) で
//! 鉄板になる adapter — mini-app schema convention に縛られず、 任意の
//! SQLite file に対して直接 SQL を投げる generic backend。 volume mount
//! 1 個でそのまま動く。
//!
//! ## URI form
//!
//! ```text
//! sqlite://<path>?<query|table>=<value>[&limit=<n>]
//! ```
//!
//! - `<path>` — file path。 `~/` で始まる場合は HOME 展開。 host+path 双方を URL から
//!   組み直すため `sqlite:///abs/path.db` でも `sqlite://./relative.db` でも `sqlite://~/.db`
//!   でも受け取れる
//! - `?query=<URL-encoded SQL>` — primary form。 任意の SELECT (or PRAGMA) を実行
//! - `?table=<name>` — sugar (= `SELECT * FROM "<name>"` に展開)。 `query` と排他
//! - `?limit=<n>` — sugar form の `LIMIT` 句として付与 (primary form では行数 cap として
//!   適用、 SQL 本体には触らない)
//!
//! ## Return shape
//!
//! ```jsonc
//! {
//!   "scheme": "sqlite",
//!   "path": "/abs/path/to.db",
//!   "count": 3,
//!   "rows": [
//!     {"id": 1, "name": "alice"},
//!     {"id": 2, "name": "bob"},
//!     {"id": 3, "name": null}
//!   ]
//! }
//! ```
//!
//! BLOB column は base64 encoded string になる (`data:base64,<...>` prefix なし、
//! 純粋な base64 文字列)。 wire の prompt context に直接埋めるのは想定していない、
//! caller (template / projection) 側で必要なら decode する。

use std::path::PathBuf;

use async_trait::async_trait;
use persona_wire_core::infrastructure::adapter::Adapter;
use persona_wire_core::{WireError, WireResult};

#[derive(Debug, Clone)]
struct SqliteUriSpec {
    path: PathBuf,
    /// `query` (primary) or `table` (sugar) のどちらか必須。 両方指定はエラー。
    query: SqliteQueryKind,
    /// `?limit=<n>` の数値。 sugar form では `LIMIT N` 句に展開、 primary form では
    /// 行数 cap として適用 (SQL 本体には触らない)。
    limit: Option<usize>,
}

#[derive(Debug, Clone)]
enum SqliteQueryKind {
    /// `?query=<SQL>` — URL-decoded SQL literal。
    Sql(String),
    /// `?table=<name>` — `SELECT * FROM "<name>"` に展開。
    Table(String),
}

fn parse_sqlite_uri(source_uri: &str) -> WireResult<SqliteUriSpec> {
    let rest = source_uri
        .strip_prefix("sqlite://")
        .ok_or_else(|| WireError::Storage(format!("sqlite adapter: bad uri: {source_uri}")))?;

    // url crate の non-special scheme 扱いだと `sqlite://<path>?...` の host/path 振り分けが
    // ambiguous (`sqlite:///abs` → host="" / path="/abs"; `sqlite://./rel` → host="." / path="/rel")。
    // 自前 split で `?` 前を path、 `?` 以降を query string として扱う方が単純で正確。
    let (path_part, query_part) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    if path_part.is_empty() {
        return Err(WireError::Storage(format!(
            "sqlite adapter: missing path in {source_uri}"
        )));
    }
    let path = expand_path(path_part)?;

    let mut sql: Option<String> = None;
    let mut table: Option<String> = None;
    let mut limit: Option<usize> = None;
    if let Some(qs) = query_part {
        for pair in qs.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v_raw) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            // url crate の percent decode を使う (form-encoded)
            let v = url::form_urlencoded::parse(format!("{k}={v_raw}").as_bytes())
                .next()
                .map(|(_, val)| val.into_owned())
                .unwrap_or_default();
            match k {
                "query" => sql = Some(v),
                "table" => table = Some(v),
                "limit" => {
                    let n: usize = v.parse().map_err(|e| {
                        WireError::Storage(format!(
                            "sqlite adapter: invalid limit '{v}' in {source_uri}: {e}"
                        ))
                    })?;
                    limit = Some(n);
                }
                _ => {
                    return Err(WireError::Storage(format!(
                        "sqlite adapter: unknown query key '{k}' in {source_uri}"
                    )));
                }
            }
        }
    }

    let query = match (sql, table) {
        (Some(_), Some(_)) => {
            return Err(WireError::Storage(format!(
                "sqlite adapter: `query` and `table` are mutually exclusive in {source_uri}"
            )));
        }
        (Some(q), None) => SqliteQueryKind::Sql(q),
        (None, Some(t)) => SqliteQueryKind::Table(t),
        (None, None) => {
            return Err(WireError::Storage(format!(
                "sqlite adapter: must specify `?query=<sql>` or `?table=<name>` in {source_uri}"
            )));
        }
    };

    Ok(SqliteUriSpec { path, query, limit })
}

fn expand_path(raw: &str) -> WireResult<PathBuf> {
    // url crate の percent decode (path 部分にも `%20` 等が来うる)
    let decoded = url::form_urlencoded::parse(format!("p={raw}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| raw.to_string());
    if let Some(rest) = decoded.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("sqlite adapter: HOME unset".to_string()))?;
        Ok(PathBuf::from(home).join(rest))
    } else {
        Ok(PathBuf::from(decoded))
    }
}

pub struct SqliteAdapter;

impl SqliteAdapter {
    fn execute(&self, spec: &SqliteUriSpec) -> WireResult<serde_json::Value> {
        if !spec.path.exists() {
            return Err(WireError::Storage(format!(
                "sqlite adapter: db not found: {}",
                spec.path.display()
            )));
        }
        let conn = rusqlite::Connection::open(&spec.path).map_err(|e| {
            WireError::Storage(format!("sqlite adapter: open {}: {e}", spec.path.display()))
        })?;

        // ---- SQL 組み立て ----
        // sugar form: `SELECT * FROM "<table>"` (+ LIMIT)、 table 名は二重引用符で quote
        // (= rusqlite 側でも identifier escape は同 form 推奨)
        // primary form: caller の SQL を literal で投げる。 LIMIT は post-cap で適用 (SQL 本体不変)。
        let (sql, post_limit): (String, Option<usize>) = match &spec.query {
            SqliteQueryKind::Table(name) => {
                if name.contains('"') {
                    return Err(WireError::Storage(format!(
                        "sqlite adapter: table name contains double-quote: {name}"
                    )));
                }
                let limited = match spec.limit {
                    Some(n) => format!("SELECT * FROM \"{name}\" LIMIT {n}"),
                    None => format!("SELECT * FROM \"{name}\""),
                };
                (limited, None)
            }
            SqliteQueryKind::Sql(q) => (q.clone(), spec.limit),
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| WireError::Storage(format!("sqlite adapter: prepare: {e}")))?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();

        let mut rows = stmt
            .query([])
            .map_err(|e| WireError::Storage(format!("sqlite adapter: query: {e}")))?;
        let mut out: Vec<serde_json::Value> = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| WireError::Storage(format!("sqlite adapter: row fetch: {e}")))?
        {
            if let Some(cap) = post_limit {
                if out.len() >= cap {
                    break;
                }
            }
            let mut obj = serde_json::Map::with_capacity(col_count);
            for (i, name) in col_names.iter().enumerate() {
                let value = column_value_to_json(row, i)?;
                obj.insert(name.clone(), value);
            }
            out.push(serde_json::Value::Object(obj));
        }

        Ok(serde_json::json!({
            "scheme": "sqlite",
            "path": spec.path.display().to_string(),
            "count": out.len(),
            "rows": out,
        }))
    }
}

fn column_value_to_json(row: &rusqlite::Row<'_>, idx: usize) -> WireResult<serde_json::Value> {
    use rusqlite::types::ValueRef;
    let v = row
        .get_ref(idx)
        .map_err(|e| WireError::Storage(format!("sqlite adapter: get column {idx}: {e}")))?;
    Ok(match v {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::Value::Number(i.into()),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        ValueRef::Text(bytes) => {
            let s = std::str::from_utf8(bytes).map_err(|e| {
                WireError::Storage(format!("sqlite adapter: column {idx} non-utf8: {e}"))
            })?;
            serde_json::Value::String(s.to_string())
        }
        ValueRef::Blob(bytes) => {
            // base64 (RFC 4648 §4 standard, no padding char strip)
            let mut s = String::with_capacity(bytes.len() * 4 / 3 + 4);
            const TABLE: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut i = 0;
            while i + 3 <= bytes.len() {
                let b0 = bytes[i];
                let b1 = bytes[i + 1];
                let b2 = bytes[i + 2];
                s.push(TABLE[(b0 >> 2) as usize] as char);
                s.push(TABLE[((b0 & 0b11) << 4 | b1 >> 4) as usize] as char);
                s.push(TABLE[((b1 & 0b1111) << 2 | b2 >> 6) as usize] as char);
                s.push(TABLE[(b2 & 0b111111) as usize] as char);
                i += 3;
            }
            let rem = bytes.len() - i;
            if rem == 1 {
                let b0 = bytes[i];
                s.push(TABLE[(b0 >> 2) as usize] as char);
                s.push(TABLE[((b0 & 0b11) << 4) as usize] as char);
                s.push_str("==");
            } else if rem == 2 {
                let b0 = bytes[i];
                let b1 = bytes[i + 1];
                s.push(TABLE[(b0 >> 2) as usize] as char);
                s.push(TABLE[((b0 & 0b11) << 4 | b1 >> 4) as usize] as char);
                s.push(TABLE[((b1 & 0b1111) << 2) as usize] as char);
                s.push('=');
            }
            serde_json::Value::String(s)
        }
    })
}

#[async_trait]
impl Adapter for SqliteAdapter {
    fn scheme(&self) -> &'static str {
        "sqlite"
    }

    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value> {
        let spec = parse_sqlite_uri(source_uri)?;
        // rusqlite は同期 API。 tokio runtime を block しないよう spawn_blocking でラップ。
        let self_ = SqliteAdapter;
        tokio::task::spawn_blocking(move || self_.execute(&spec))
            .await
            .map_err(|e| WireError::Storage(format!("sqlite adapter: join: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn make_sample_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE persons (id INTEGER PRIMARY KEY, name TEXT, age INTEGER, weight REAL, photo BLOB);
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persons (name, age, weight, photo) VALUES (?, ?, ?, ?)",
            params!["alice", 30, 55.5, &[0xDE_u8, 0xAD, 0xBE, 0xEF][..]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persons (name, age, weight, photo) VALUES (?, ?, ?, ?)",
            params!["bob", 42, Option::<f64>::None, Option::<Vec<u8>>::None],
        )
        .unwrap();
        (dir, path)
    }

    #[test]
    fn parse_sqlite_uri_table_form() {
        let spec = parse_sqlite_uri("sqlite:///var/data/x.db?table=foo").unwrap();
        assert_eq!(spec.path, PathBuf::from("/var/data/x.db"));
        assert!(matches!(spec.query, SqliteQueryKind::Table(ref n) if n == "foo"));
        assert_eq!(spec.limit, None);
    }

    #[test]
    fn parse_sqlite_uri_query_form() {
        let spec = parse_sqlite_uri(
            "sqlite:///var/data/x.db?query=SELECT%20*%20FROM%20foo%20WHERE%20x%3D1",
        )
        .unwrap();
        match spec.query {
            SqliteQueryKind::Sql(q) => assert_eq!(q, "SELECT * FROM foo WHERE x=1"),
            _ => panic!("expected Sql variant"),
        }
    }

    #[test]
    fn parse_sqlite_uri_with_limit() {
        let spec = parse_sqlite_uri("sqlite:///var/data/x.db?table=foo&limit=10").unwrap();
        assert_eq!(spec.limit, Some(10));
    }

    #[test]
    fn parse_sqlite_uri_query_and_table_conflict() {
        let r = parse_sqlite_uri("sqlite:///var/data/x.db?query=SELECT&table=foo");
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("mutually exclusive"));
    }

    #[test]
    fn parse_sqlite_uri_missing_query_and_table() {
        let r = parse_sqlite_uri("sqlite:///var/data/x.db");
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("must specify"));
    }

    #[test]
    fn parse_sqlite_uri_unknown_key_rejected() {
        let r = parse_sqlite_uri("sqlite:///var/data/x.db?table=foo&hocus=pocus");
        assert!(r.is_err());
        assert!(r
            .unwrap_err()
            .to_string()
            .contains("unknown query key 'hocus'"));
    }

    #[test]
    fn parse_sqlite_uri_tilde_expansion() {
        let original = std::env::var("HOME").ok();
        // SAFETY: unit test process, single-threaded mutation of env for the duration of this scope.
        unsafe {
            std::env::set_var("HOME", "/var/data/test-home");
        }
        let spec = parse_sqlite_uri("sqlite://~/data.db?table=foo").unwrap();
        match original {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert_eq!(spec.path, PathBuf::from("/var/data/test-home/data.db"));
    }

    #[test]
    fn parse_sqlite_uri_missing_path_rejected() {
        let r = parse_sqlite_uri("sqlite://?table=foo");
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("missing path"));
    }

    #[test]
    fn parse_sqlite_uri_bad_scheme_rejected() {
        let r = parse_sqlite_uri("file:///var/data/x.db");
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("bad uri"));
    }

    #[tokio::test]
    async fn adapter_scheme_is_sqlite() {
        assert_eq!(SqliteAdapter.scheme(), "sqlite");
    }

    #[tokio::test]
    async fn adapter_fetch_table_form_lists_rows() {
        let (_dir, path) = make_sample_db();
        let uri = format!("sqlite://{}?table=persons", path.display());
        let v = SqliteAdapter.fetch(&uri).await.unwrap();
        assert_eq!(v["scheme"], "sqlite");
        assert_eq!(v["count"], 2);
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows[0]["name"], "alice");
        assert_eq!(rows[0]["age"], 30);
        // weight (REAL) は f64 → serde_json::Number(f64)
        assert!(rows[0]["weight"].is_number());
        // photo (BLOB) は base64 string
        let photo = rows[0]["photo"].as_str().unwrap();
        assert_eq!(photo, "3q2+7w=="); // base64(DE AD BE EF) = "3q2+7w=="
                                       // bob は weight / photo が NULL
        assert!(rows[1]["weight"].is_null());
        assert!(rows[1]["photo"].is_null());
    }

    #[tokio::test]
    async fn adapter_fetch_table_form_with_limit() {
        let (_dir, path) = make_sample_db();
        let uri = format!("sqlite://{}?table=persons&limit=1", path.display());
        let v = SqliteAdapter.fetch(&uri).await.unwrap();
        assert_eq!(v["count"], 1);
    }

    #[tokio::test]
    async fn adapter_fetch_query_form() {
        let (_dir, path) = make_sample_db();
        let uri = format!(
            "sqlite://{}?query=SELECT%20name%2C%20age%20FROM%20persons%20WHERE%20age%20%3E%2035",
            path.display()
        );
        let v = SqliteAdapter.fetch(&uri).await.unwrap();
        assert_eq!(v["count"], 1);
        assert_eq!(v["rows"][0]["name"], "bob");
        assert_eq!(v["rows"][0]["age"], 42);
        // SELECT した column 以外 (id / weight / photo) は object に含まれない
        assert!(v["rows"][0].as_object().unwrap().get("id").is_none());
    }

    #[tokio::test]
    async fn adapter_fetch_query_form_with_limit_post_caps() {
        let (_dir, path) = make_sample_db();
        // primary form の limit は SQL 本体に触らず、 行数 cap として後段適用
        let uri = format!(
            "sqlite://{}?query=SELECT%20*%20FROM%20persons&limit=1",
            path.display()
        );
        let v = SqliteAdapter.fetch(&uri).await.unwrap();
        assert_eq!(v["count"], 1);
    }

    #[tokio::test]
    async fn adapter_fetch_nonexistent_path_errors() {
        let r = SqliteAdapter
            .fetch("sqlite:///nonexistent/path/x.db?table=foo")
            .await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("db not found"));
    }

    #[tokio::test]
    async fn adapter_fetch_table_name_with_quote_rejected() {
        let (_dir, path) = make_sample_db();
        let uri = format!("sqlite://{}?table=foo%22bar", path.display());
        let r = SqliteAdapter.fetch(&uri).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("double-quote"));
    }
}
