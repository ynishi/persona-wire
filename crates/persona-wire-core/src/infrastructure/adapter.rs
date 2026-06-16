//! Layer 6 Adapter (SoT) — concept-doc §3 Layer 6 + §5 #3 / §P3b の前倒し land。
//!
//! 各 wiring entry node の `metadata.source_uri` を scheme 別に parse + fresh fetch して
//! `serde_json::Value` で返す。 wire は data 本体を持たず、 Adapter が render 時に
//! SoT (mini-app / file / outline / ...) から都度 fetch する。
//!
//! 現状 land 済 scheme:
//! - `mini-app://<table_name>[?scope=user|<project-name>&root=<dir>&alias=<name>&<k>=<v>*&limit=<n>]`
//!   — mini-app-core SDK 経由で table を open + list / QueryAlias 実行。 reserved query keys
//!   は `scope` (= `user` → `AliasScope::User` / 任意 project identifier → `AliasScope::Project`、
//!   省略時 = legacy fallback)、 `root` (= 物理 dir 上書き、 scope=<project-name> 時は必須、
//!   省略時 = `$MINI_APP_USER_DIR` or `~/.mini-app/`)、 `alias` (= per-table `_aliases` 名、
//!   グローバル alias storage / Multi / Pattern source / aggregator は P3b carry)、 `limit`
//!   (= list 上限 override)。 render / parse / list は SDK
//!   (`mini_app_core::alias_run::execute_alias_run`) に完全委譲、 wire は filter / MiniJinja /
//!   ListFilter 意味論を一切解釈しない (= reframe-gate §1 architecture 軸、 Resource × 取り出し方
//!   の連携 layer 役)。
//! - `file://<absolute-or-tilde-path>` — std::fs::read で raw 字面を取得 (json/toml は将来
//!   parse 拡張、 現状は string として返す)。
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

/// `mini-app://` URI から抽出した取り出し指示。 wire scope 内で意味解釈する 4 key
/// (`scope` / `root` / `alias` / `limit`) は専用 field に、 残り query key 全てが `params`
/// (json object) に集約される。 wire は params 値の型 / 意味を一切解釈せず、 mini-app
/// 側の MiniJinja + ListFilter に渡すだけ。
///
/// 内部 URI parse 結果の中間 data structure、 module 外には漏らさない (= 公開 surface
/// 最小化、 rust idiom)。 caller surface は `fetch_via_adapter` (free fn) のみ。
#[derive(Debug, Clone)]
struct MiniAppUriSpec {
    /// `mini-app://<table>` の table 部分。
    table: String,
    /// `?scope=user|<project-name>` の identifier。 `"user"` は `AliasScope::User` に
    /// mapping、 それ以外の任意 string は `AliasScope::Project` に mapping (project name は
    /// `root` field で物理 dir を別途明示する前提)。 不在時は backward compat (legacy
    /// fallback path)。
    scope: Option<String>,
    /// `?root=<dir>` の物理 dir override。 scope=<project-name> 時は必須、 scope=user /
    /// 省略時は任意 (不在時は `$MINI_APP_USER_DIR` or `~/.mini-app/`)。
    root: Option<PathBuf>,
    /// `?alias=<name>` の name 部分。 不在時は `None` で list-all 互換 path。
    alias: Option<String>,
    /// `?limit=<n>` の数値。 invalid 値は parse error。
    limit: Option<u32>,
    /// `scope` / `root` / `alias` / `limit` 以外の query key を全部 string value で集めた
    /// object。 MiniJinja render に渡す。
    params: serde_json::Value,
}

/// `mini-app://<table>[?scope=<s>&root=<dir>&alias=<name>&k=v*&limit=<n>]` を parse する
/// 内部 helper。 scheme prefix は呼び出し側で剥がす前提で受ける (= `<table>[?query]` の
/// rest だけ受け取る)。 query 不在時は全 field が `None` で table only spec を返す。
///
/// scope=<project-name> (= scope != "user" かつ Some) で `root` 不在時は parse error
/// (= 物理 dir 解決不能、 fail-fast)。 scope=user / scope 不在時は `root` 任意。
fn parse_mini_app_uri(rest: &str) -> WireResult<MiniAppUriSpec> {
    let full_uri = format!("mini-app://{rest}");
    let parsed = url::Url::parse(&full_uri)
        .map_err(|e| WireError::Storage(format!("mini-app adapter: bad uri: {full_uri}: {e}")))?;

    // host_str() = table 名 (e.g. `mini-app://mailbox` の `mailbox`)
    // url crate は `mini-app://` を non-special scheme として host を持つ form で扱う。
    let table = parsed
        .host_str()
        .ok_or_else(|| {
            WireError::Storage(format!("mini-app adapter: missing table in {full_uri}"))
        })?
        .to_string();

    let mut scope: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    let mut alias: Option<String> = None;
    let mut limit: Option<u32> = None;
    let mut params_map = serde_json::Map::new();
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "scope" => scope = Some(v.into_owned()),
            "root" => root = Some(resolve_root_path(v.as_ref())?),
            "alias" => alias = Some(v.into_owned()),
            "limit" => {
                let n: u32 = v.parse().map_err(|e| {
                    WireError::Storage(format!(
                        "mini-app adapter: invalid limit '{v}' in {full_uri}: {e}"
                    ))
                })?;
                limit = Some(n);
            }
            _ => {
                params_map.insert(k.into_owned(), serde_json::Value::String(v.into_owned()));
            }
        }
    }

    // scope=<project-name> 時は root 必須 (= 物理 dir 解決不能を fail-fast 化)
    if let Some(s) = scope.as_deref() {
        if s != "user" && root.is_none() {
            return Err(WireError::Storage(format!(
                "mini-app adapter: scope='{s}' requires ?root=<dir> in {full_uri}"
            )));
        }
    }

    Ok(MiniAppUriSpec {
        table,
        scope,
        root,
        alias,
        limit,
        params: serde_json::Value::Object(params_map),
    })
}

/// `?root=<dir>` の値を `PathBuf` に解決。 `~/...` は HOME 展開、 それ以外は as-is。
fn resolve_root_path(raw: &str) -> WireResult<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("mini-app adapter: HOME unset".to_string()))?;
        Ok(PathBuf::from(home).join(rest))
    } else {
        Ok(PathBuf::from(raw))
    }
}

/// Dispatch helper: `source_uri` の scheme prefix を見て対応 Adapter を呼ぶ。
/// `wire_init` use case から 1 行で呼べる shim。
pub async fn fetch_via_adapter(source_uri: &str) -> WireResult<serde_json::Value> {
    if let Some(rest) = source_uri.strip_prefix("mini-app://") {
        let spec = parse_mini_app_uri(rest)?;
        if spec.alias.is_some() {
            MiniAppAdapter.fetch_via_alias(&spec).await
        } else {
            // alias 不在 = 既存 list-all 互換 path (limit 指定があれば反映)
            MiniAppAdapter.fetch_table_via_spec(&spec).await
        }
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
    /// `mini-app://<table_name>` の table_name 部分を受けて、 default user scope の
    /// `~/.mini-app/<table>/<table>.db` + `schema.yaml` を open + list all rows。
    /// 互換 surface (test / 外部 caller の利便性のため public 維持)。
    pub async fn fetch_table(&self, table_name: &str) -> WireResult<serde_json::Value> {
        let spec = MiniAppUriSpec {
            table: table_name.to_string(),
            scope: None,
            root: None,
            alias: None,
            limit: None,
            params: serde_json::Value::Object(Default::default()),
        };
        self.fetch_table_via_spec(&spec).await
    }

    /// spec 経由 list-all path。 `?scope=` / `?root=` / `?limit=` を尊重する。
    /// 内部 helper、 公開 surface は `fetch_via_adapter` / `Adapter::fetch` 経由。
    /// `limit` 不在時は従来通り 1000 上限 (list default 100 / max 1000)。
    async fn fetch_table_via_spec(&self, spec: &MiniAppUriSpec) -> WireResult<serde_json::Value> {
        let (_db_path, store) =
            open_mini_app_store(&spec.table, spec.scope.as_deref(), spec.root.as_deref()).await?;
        let effective_limit = spec.limit.or(Some(1000));
        let rows = store
            .list(effective_limit, None, None)
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
            "table": spec.table,
            "count": json_rows.len(),
            "rows": json_rows,
        }))
    }

    /// `mini-app://<table>?alias=<name>[&k=v]*[&limit=<n>]` を受けて、
    /// mini-app QueryAlias 機能を叩いて filter 済 rows を取得する。
    /// render + parse + list は SDK (mini-app-core v0.12+ `execute_alias_run`) に
    /// 完全委譲、 wire 側に MiniJinja / ListFilter / aggregator 認識は持ち込まない。
    ///
    /// wire scope: per-table `_aliases` (Single source by definition) のみ対応、
    /// グローバル alias storage (`_global.db` / Multi / Pattern source / aggregator)
    /// は P3b carry。
    ///
    /// 内部 helper、 公開 surface は `fetch_via_adapter` / `Adapter::fetch` 経由。
    ///
    /// 流れ:
    /// 1. `Store::open` + `Store::alias_get(name)` で per-table `_aliases` record 取得
    /// 2. per-table `store::AliasRecord` を `alias_storage::AliasRecord` (global form) に変換
    ///    (sources=Single("")、 aggregator=None、 scope=None で legacy fallback path に乗せる)
    /// 3. `TableRegistry::from_entries` で 1 table 分の registry を組み立て
    /// 4. `execute_alias_run(...)` SDK 1 call で render + parse + list を完全委譲
    /// 5. `AliasRunValue::Rows` を取り出して既存戻り値 shape に整形
    async fn fetch_via_alias(&self, spec: &MiniAppUriSpec) -> WireResult<serde_json::Value> {
        let alias_name = spec
            .alias
            .as_deref()
            .ok_or_else(|| WireError::Storage("mini-app adapter: alias key missing".to_string()))?;
        let (db_path, store) =
            open_mini_app_store(&spec.table, spec.scope.as_deref(), spec.root.as_deref()).await?;

        // Step 1: alias_get (per-table _aliases)
        let per_table_rec = store.alias_get(alias_name).await.map_err(|e| {
            WireError::Storage(format!(
                "mini-app adapter: alias_get('{alias_name}') failed: {e}"
            ))
        })?;

        // Step 2: per-table store::AliasRecord → alias_storage::AliasRecord (global form)
        // legacy fallback path で execute_alias_run に乗せるため sources は empty Single
        // sentinel + table_fallback に table 名を渡す form。
        //
        // scope mapping:
        //   - spec.scope = Some("user")           → Some(AliasScope::User)
        //   - spec.scope = Some(<project-name>)   → Some(AliasScope::Project)
        //   - spec.scope = None                   → None (legacy fallback、 既存 path 維持)
        let global_scope = spec.scope.as_deref().map(|s| {
            if s == "user" {
                mini_app_core::alias_storage::AliasScope::User
            } else {
                mini_app_core::alias_storage::AliasScope::Project
            }
        });
        let global_rec = mini_app_core::alias_storage::AliasRecord {
            name: per_table_rec.name,
            sources: mini_app_core::aggregator::SourceSpec::Single(String::new()),
            aggregator: None,
            filter: per_table_rec.filter,
            default_limit: per_table_rec.default_limit,
            description: per_table_rec.description,
            params_schema: per_table_rec.params_schema,
            scope: global_scope,
        };

        // Step 3: schema 再取得 + 1-table registry 組み立て
        let schema_path = db_path
            .parent()
            .ok_or_else(|| {
                WireError::Storage(format!(
                    "mini-app adapter: cannot resolve schema dir from {}",
                    db_path.display()
                ))
            })?
            .join("schema.yaml");
        let schema = mini_app_core::schema::load_from_path(&schema_path)
            .map_err(|e| WireError::Storage(format!("mini-app adapter: schema load: {e}")))?;
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            spec.table.clone(),
            mini_app_core::registry::TableEntry {
                store: std::sync::Arc::new(store),
                schema: std::sync::Arc::new(schema),
                schema_path: std::sync::Arc::new(schema_path),
            },
        );
        let registry =
            mini_app_core::registry::TableRegistry::from_entries(entries, Some(spec.table.clone()));

        // Step 4: SDK execute_alias_run 1 call (= render + parse + list を SDK に完全委譲)
        // params は wire side が URI query から組んだ json object (alias / limit 除外済)。
        let value = mini_app_core::alias_run::execute_alias_run(
            &registry,
            global_rec,
            Some(spec.params.clone()),
            Some(&spec.table),
            spec.limit,
            None,
            None,
        )
        .await
        .map_err(|e| {
            WireError::Storage(format!(
                "mini-app adapter: alias '{alias_name}' execute_alias_run failed: {e}"
            ))
        })?;

        // Step 5: AliasRunValue::Rows を取り出して既存戻り値 shape に整形。
        // wire は plain Rows path 専用 (= per-table _aliases の aggregator=None 前提)、
        // Aggregate variant は将来 carry。
        let rows = match value {
            mini_app_core::alias_run::AliasRunValue::Rows(r) => r,
            mini_app_core::alias_run::AliasRunValue::Aggregate(_) => {
                return Err(WireError::Storage(format!(
                    "mini-app adapter: alias '{alias_name}' returned Aggregate variant — \
                     wire scope 外 (P3b carry)"
                )));
            }
        };

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
            "table": spec.table,
            "alias": alias_name,
            "count": json_rows.len(),
            "rows": json_rows,
        }))
    }
}

impl Adapter for MiniAppAdapter {
    async fn fetch(&self, source_uri: &str) -> WireResult<serde_json::Value> {
        let rest = source_uri.strip_prefix("mini-app://").ok_or_else(|| {
            WireError::Storage(format!("mini-app adapter: bad uri: {source_uri}"))
        })?;
        let spec = parse_mini_app_uri(rest)?;
        if spec.alias.is_some() {
            self.fetch_via_alias(&spec).await
        } else {
            self.fetch_table_via_spec(&spec).await
        }
    }
}

/// `~/.mini-app/<table>/<table>.db` + `schema.yaml` を open する共通 helper。
/// `fetch_table_via_spec` / `fetch_via_alias` から共有される。
/// `scope` / `root_override` は URI query 由来、 `resolve_mini_app_table_dir` に flow。
async fn open_mini_app_store(
    table_name: &str,
    scope: Option<&str>,
    root_override: Option<&std::path::Path>,
) -> WireResult<(PathBuf, mini_app_core::store::Store)> {
    let base = resolve_mini_app_table_dir(table_name, scope, root_override)?;
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
    Ok((db_path, store))
}

/// 物理 dir 解決:
/// - `root_override = Some(<path>)`           → そのまま base (scope 種別問わず)
/// - `root_override = None` + scope=user/None → `$MINI_APP_USER_DIR` or `~/.mini-app/`
/// - `root_override = None` + scope=<project> → parse 段階で弾かれてるはず (defensive で error)
fn resolve_mini_app_table_dir(
    table: &str,
    scope: Option<&str>,
    root_override: Option<&std::path::Path>,
) -> WireResult<PathBuf> {
    let base = if let Some(root) = root_override {
        root.to_path_buf()
    } else {
        // scope=<project-name> (= scope != "user" かつ Some) + root 不在は parse 段階で弾く
        // 想定だが、 defensive に同型 error を返す。
        if let Some(s) = scope {
            if s != "user" {
                return Err(WireError::Storage(format!(
                    "mini-app adapter: scope='{s}' requires ?root=<dir> (table={table})"
                )));
            }
        }
        // env override 順序は mini-app-mcp instructions と同型:
        //   1. MINI_APP_USER_DIR  (default `~/.mini-app/`)
        //   2. MINI_APP_PROJECT_DIR は wire の責務外 (= 各 project に scoped data 無い前提
        //      で、 project scope に乗せたい場合は URI 側で ?scope=<name>&root=<dir> 経由)
        match std::env::var("MINI_APP_USER_DIR") {
            Ok(p) if !p.is_empty() => PathBuf::from(p),
            _ => {
                let home = std::env::var("HOME")
                    .map_err(|_| WireError::Storage("mini-app adapter: HOME unset".to_string()))?;
                PathBuf::from(home).join(".mini-app")
            }
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

    // ---- URI parse unit tests ----

    #[test]
    fn parse_mini_app_uri_table_only() {
        let spec = parse_mini_app_uri("mailbox").unwrap();
        assert_eq!(spec.table, "mailbox");
        assert_eq!(spec.alias, None);
        assert_eq!(spec.limit, None);
        assert_eq!(spec.params, serde_json::json!({}));
    }

    #[test]
    fn parse_mini_app_uri_with_alias_no_params() {
        let spec = parse_mini_app_uri("mia_active_context?alias=active").unwrap();
        assert_eq!(spec.table, "mia_active_context");
        assert_eq!(spec.alias.as_deref(), Some("active"));
        assert_eq!(spec.limit, None);
        assert_eq!(spec.params, serde_json::json!({}));
    }

    #[test]
    fn parse_mini_app_uri_with_alias_and_params() {
        let spec = parse_mini_app_uri("mailbox?alias=unread_for&persona=mia&kind=info").unwrap();
        assert_eq!(spec.table, "mailbox");
        assert_eq!(spec.alias.as_deref(), Some("unread_for"));
        assert_eq!(spec.limit, None);
        // params は alias / limit 以外の query key だけ
        assert_eq!(
            spec.params,
            serde_json::json!({"persona": "mia", "kind": "info"})
        );
    }

    #[test]
    fn parse_mini_app_uri_with_limit() {
        let spec = parse_mini_app_uri("mia_trigger?alias=due&persona=mia&limit=5").unwrap();
        assert_eq!(spec.table, "mia_trigger");
        assert_eq!(spec.alias.as_deref(), Some("due"));
        assert_eq!(spec.limit, Some(5));
        assert_eq!(spec.params, serde_json::json!({"persona": "mia"}));
    }

    #[test]
    fn parse_mini_app_uri_invalid_limit_rejects() {
        let r = parse_mini_app_uri("mailbox?limit=abc");
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("invalid limit"));
    }

    #[test]
    fn parse_mini_app_uri_reserved_keys_isolated_from_params() {
        // `alias` と `limit` は params に漏れない (= wire の意味解釈責務、 mini-app に渡さない)
        let spec = parse_mini_app_uri("t?alias=a&limit=10&alias_extra=x&limit_extra=y").unwrap();
        assert_eq!(spec.alias.as_deref(), Some("a"));
        assert_eq!(spec.limit, Some(10));
        // alias_extra / limit_extra は **別 key** なので params に残る
        assert_eq!(
            spec.params,
            serde_json::json!({"alias_extra": "x", "limit_extra": "y"})
        );
    }

    // ---- scope / root reserved key tests ----

    #[test]
    fn parse_mini_app_uri_scope_absent_is_legacy_path() {
        // scope 不在 = legacy fallback (backward compat、 既存 caller 全件影響なし)
        let spec = parse_mini_app_uri("mailbox?alias=unread").unwrap();
        assert_eq!(spec.scope, None);
        assert_eq!(spec.root, None);
    }

    #[test]
    fn parse_mini_app_uri_with_scope_user() {
        let spec = parse_mini_app_uri("mailbox?scope=user&alias=unread").unwrap();
        assert_eq!(spec.scope.as_deref(), Some("user"));
        assert_eq!(spec.root, None);
        assert_eq!(spec.alias.as_deref(), Some("unread"));
        // scope は params に漏れない
        assert_eq!(spec.params, serde_json::json!({}));
    }

    #[test]
    fn parse_mini_app_uri_with_scope_project_and_root() {
        let spec = parse_mini_app_uri(
            "session_log?scope=persona-wire&root=/opt/data/pw&alias=recent&limit=5",
        )
        .unwrap();
        assert_eq!(spec.scope.as_deref(), Some("persona-wire"));
        assert_eq!(
            spec.root.as_deref(),
            Some(std::path::Path::new("/opt/data/pw"))
        );
        assert_eq!(spec.alias.as_deref(), Some("recent"));
        assert_eq!(spec.limit, Some(5));
        // scope / root は params に漏れない
        assert_eq!(spec.params, serde_json::json!({}));
    }

    #[test]
    fn parse_mini_app_uri_scope_project_without_root_rejects() {
        // scope=<project-name> + root 不在 = parse error (fail-fast)
        let r = parse_mini_app_uri("t?scope=algocline&alias=x");
        assert!(r.is_err());
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("scope='algocline' requires ?root="),
            "expected scope+root error, got: {msg}"
        );
    }

    #[test]
    fn parse_mini_app_uri_scope_user_without_root_is_ok() {
        // scope=user + root 不在 = OK (default dir に fallback)
        let spec = parse_mini_app_uri("t?scope=user").unwrap();
        assert_eq!(spec.scope.as_deref(), Some("user"));
        assert_eq!(spec.root, None);
    }

    #[test]
    fn parse_mini_app_uri_with_root_tilde_expands_home() {
        // SAFETY: tests run sequentially in `cargo test -- --test-threads=1` by default
        // for `current_thread` flavour, but unit tests share process. We snapshot HOME,
        // set a known value, parse, then restore.
        let original = std::env::var("HOME").ok();
        // SAFETY: unit test process, single-threaded mutation of env for the duration
        // of this scope.
        unsafe {
            std::env::set_var("HOME", "/tmp/test-home");
        }
        let spec = parse_mini_app_uri("t?scope=foo&root=~/.mini-app-foo").unwrap();
        match original {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        assert_eq!(
            spec.root.as_deref(),
            Some(std::path::Path::new("/tmp/test-home/.mini-app-foo"))
        );
    }

    #[test]
    fn resolve_dir_with_root_override_wins_over_env() {
        let root = std::path::PathBuf::from("/var/wire-data");
        let r = resolve_mini_app_table_dir("kv", Some("foo"), Some(&root)).unwrap();
        // root_override が base、 table 名が後ろに付く
        assert_eq!(r, std::path::PathBuf::from("/var/wire-data/kv"));
    }

    #[test]
    fn resolve_dir_scope_project_without_root_defensive_error() {
        // parse 段階で弾く想定だが、 直接 resolve を呼んだ場合の defensive 検査。
        let r = resolve_mini_app_table_dir("kv", Some("algocline"), None);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("requires ?root="));
    }

    // alias 経路の実機 verify は `crates/persona-wire/tests/e2e_alias_mcp.rs`
    // で実 binary spawn + stdio JSON-RPC 経由で行う (= 上位互換、 env var race
    // 問題も独立 process で解消)。 旧 ignored integration test 2 件はそちらに
    // 移動済 (2026-06-16)。
}
