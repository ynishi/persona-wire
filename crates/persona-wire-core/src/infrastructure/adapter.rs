//! Layer 6 Adapter (SoT) — concept-doc §3 Layer 6 + §5 #3 / §P3b の前倒し land。
//!
//! 各 wiring entry node の `metadata.source_uri` を scheme 別に parse + fresh fetch して
//! `serde_json::Value` で返す。 wire は data 本体を持たず、 Adapter が render 時に
//! SoT (mini-app / file / outline / ...) から都度 fetch する。
//!
//! 現状 land 済 scheme:
//! - `mini-app://<table_name>` — mini-app-core SDK 経由で `~/.mini-app/<table>/<table>.db`
//!   + `schema.yaml` を open + `Store::list(...)` で row 全件取得
//! - `file://<absolute-or-tilde-path>` — std::fs::read で raw 字面を取得
//!   (json/toml は将来 parse 拡張、 現状は string として返す)
//!
//! outline / persona-pack / journal scheme は P3b carry。

use std::path::PathBuf;

use crate::domain::error::{WireError, WireResult};

/// Adapter trait. async fn を持つので async-trait の代わりに `Pin<Box<Future>>`
/// 返却で表現 (wire-core を async-trait dep から守るため)。
#[allow(async_fn_in_trait)]
pub trait Adapter: Send + Sync {
    /// `source_uri` を scheme 別に解釈し、 fresh data を `serde_json::Value` で返す。
    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value>;
}

/// Dispatch helper: `source_uri` の scheme prefix を見て対応 Adapter を呼ぶ。
/// `wire_init` use case から 1 行で呼べる shim。
pub async fn fetch_via_adapter(source_uri: &str) -> WireResult<serde_json::Value> {
    if let Some(rest) = source_uri.strip_prefix("mini-app://") {
        MiniAppAdapter.fetch_table(rest).await
    } else if let Some(rest) = source_uri.strip_prefix("file://") {
        FileAdapter.fetch_file(rest).await
    } else if let Some(rest) = source_uri.strip_prefix("file:") {
        // `file:~/...` style (no `//`)
        FileAdapter.fetch_file(rest).await
    } else {
        Err(WireError::Storage(format!(
            "adapter: unsupported source_uri scheme: {source_uri}"
        )))
    }
}

// ---- mini-app adapter (SDK 経由) ----

pub struct MiniAppAdapter;

impl MiniAppAdapter {
    /// `mini-app://<table_name>` の table_name 部分を受けて、
    /// `~/.mini-app/<table>/<table>.db` + `schema.yaml` を open + list all rows。
    pub async fn fetch_table(&self, table_name: &str) -> WireResult<serde_json::Value> {
        let base = resolve_mini_app_table_dir(table_name)?;
        let db_path = base.join(format!("{table_name}.db"));
        let schema_path = base.join("schema.yaml");

        if !db_path.exists() {
            return Err(WireError::Storage(format!(
                "mini-app adapter: db not found: {}",
                db_path.display()
            )));
        }
        if !schema_path.exists() {
            return Err(WireError::Storage(format!(
                "mini-app adapter: schema.yaml not found: {}",
                schema_path.display()
            )));
        }

        let schema = mini_app_core::schema::load_from_path(&schema_path)
            .map_err(|e| WireError::Storage(format!("mini-app adapter: schema load: {e}")))?;
        let store = mini_app_core::store::Store::open(&db_path, schema)
            .await
            .map_err(|e| WireError::Storage(format!("mini-app adapter: store open: {e}")))?;
        // P0: unlimited (list の default は 100、 max 1000)。 大きい table は将来別 surface。
        let rows = store
            .list(Some(1000), None, None)
            .await
            .map_err(|e| WireError::Storage(format!("mini-app adapter: list: {e}")))?;

        let json_rows: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "data": r.data,
                    "created_at": r.created_at,
                    "updated_at": r.updated_at,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "scheme": "mini-app",
            "table": table_name,
            "count": json_rows.len(),
            "rows": json_rows,
        }))
    }
}

impl Adapter for MiniAppAdapter {
    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value> {
        let table = source_uri.strip_prefix("mini-app://").ok_or_else(|| {
            WireError::Storage(format!("mini-app adapter: bad uri: {source_uri}"))
        })?;
        self.fetch_table(table).await
    }
}

fn resolve_mini_app_table_dir(table: &str) -> WireResult<PathBuf> {
    // env override 順序は mini-app-mcp instructions と同型:
    //   1. MINI_APP_USER_DIR  (default `~/.mini-app/`)
    //   2. MINI_APP_PROJECT_DIR は wire の責務外 (= 各 project に scoped data 無い前提)
    let base = match std::env::var("MINI_APP_USER_DIR") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let home = std::env::var("HOME")
                .map_err(|_| WireError::Storage("mini-app adapter: HOME unset".to_string()))?;
            PathBuf::from(home).join(".mini-app")
        }
    };
    Ok(base.join(table))
}

// ---- file adapter (std::fs) ----

pub struct FileAdapter;

impl FileAdapter {
    /// `file://<path>` or `file:<path>` の path 部分を受けて、 std::fs::read で raw 字面を取得。
    /// `~/` で始まる場合は HOME 展開。 directory が渡された場合は最新 mtime の child file 1 件を読む。
    pub async fn fetch_file(&self, raw_path: &str) -> WireResult<serde_json::Value> {
        let resolved = resolve_file_path(raw_path)?;
        let meta = std::fs::metadata(&resolved)
            .map_err(|e| WireError::Storage(format!("file adapter: stat: {e}")))?;
        if meta.is_dir() {
            // newest mtime child を 1 件選ぶ (handoff dir のような形式)
            let newest = newest_child(&resolved)?;
            let body = std::fs::read_to_string(&newest)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "newest_in_dir",
                "dir": resolved.display().to_string(),
                "path": newest.display().to_string(),
                "body": body,
            }))
        } else {
            let body = std::fs::read_to_string(&resolved)
                .map_err(|e| WireError::Storage(format!("file adapter: read: {e}")))?;
            Ok(serde_json::json!({
                "scheme": "file",
                "kind": "file",
                "path": resolved.display().to_string(),
                "body": body,
            }))
        }
    }
}

impl Adapter for FileAdapter {
    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value> {
        let rest = source_uri
            .strip_prefix("file://")
            .or_else(|| source_uri.strip_prefix("file:"))
            .ok_or_else(|| WireError::Storage(format!("file adapter: bad uri: {source_uri}")))?;
        self.fetch_file(rest).await
    }
}

fn resolve_file_path(raw: &str) -> WireResult<PathBuf> {
    // `~/...` -> $HOME 展開、 `#fragment` を path から剥がす (anchor は wire 内で無視)
    let stripped = raw.split('#').next().unwrap_or(raw);
    let expanded = if let Some(rest) = stripped.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("file adapter: HOME unset".to_string()))?;
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(stripped)
    };
    Ok(expanded)
}

fn newest_child(dir: &std::path::Path) -> WireResult<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| WireError::Storage(format!("file adapter: read_dir: {e}")))?
        .filter_map(|r| r.ok())
        .filter(|e| e.path().is_file())
        .collect();
    if entries.is_empty() {
        return Err(WireError::Storage(format!(
            "file adapter: empty dir: {}",
            dir.display()
        )));
    }
    entries.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .ok()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    Ok(entries
        .last()
        .map(|e| e.path())
        .expect("non-empty sorted entries"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_rejects_unknown_scheme() {
        let r = fetch_via_adapter("ssh://nope").await;
        assert!(r.is_err());
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("unsupported"));
    }

    #[tokio::test]
    async fn file_adapter_reads_existing_file() {
        // self file (この adapter.rs 自身) を読んで body に "Layer 6 Adapter" が含まれるか
        let me = file!(); // 相対パスが返ることがあるので CARGO_MANIFEST_DIR と合成
        let abs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(me);
        let uri = format!("file://{}", abs.display());
        let v = fetch_via_adapter(&uri).await.unwrap();
        let body = v["body"].as_str().unwrap();
        assert!(body.contains("Layer 6 Adapter"));
    }
}
