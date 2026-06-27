//! persona-wire Adapter for mini-app SoT (scheme `mini-app://`).
//!
//! P3b roadmap deliverable: external adapter crate split out from
//! `persona-wire-core`. Consumers wire this adapter into their `PluginRegistry`
//! by chaining `.with_adapter(MiniAppAdapter)` on top of
//! [`persona_wire_core::application::plugin_registry::PluginRegistry::default_builder_for_wire`].
//!
//! Supported URI:
//! `mini-app://<table>[?scope=user|<project-name>&root=<dir>&alias=<name>&<k>=<v>*&limit=<n>]`
//!
//! - `scope` (`user` → `AliasScope::User` / 任意 project identifier → `AliasScope::Project`、
//!   省略時 = global storage (User scope) → per-table `_aliases` fallback)
//! - `root` (= 物理 dir 上書き、 `scope=<project-name>` 時は必須)
//! - `alias` (= global `_global.db` 内 `_global_aliases` (mini-app v0.12.1+ default) +
//!   legacy per-table `_aliases` (backward compat) 双方解決対応)
//! - `limit` (= list 上限 override)
//!
//! render / parse / list は SDK (`mini_app_core::alias_run::execute_alias_run`) に完全委譲、
//! wire は filter / MiniJinja / ListFilter 意味論を一切解釈しない。

use std::path::PathBuf;

use async_trait::async_trait;
use persona_wire_core::infrastructure::adapter::Adapter;
use persona_wire_core::{WireError, WireResult};

/// `mini-app://` URI から抽出した取り出し指示。 wire scope 内で意味解釈する 4 key
/// (`scope` / `root` / `alias` / `limit`) は専用 field に、 残り query key 全てが `params`
/// (json object) に集約される。 wire は params 値の型 / 意味を一切解釈せず、 mini-app
/// 側の MiniJinja + ListFilter に渡すだけ。
#[derive(Debug, Clone)]
struct MiniAppUriSpec {
    table: String,
    scope: Option<String>,
    root: Option<PathBuf>,
    alias: Option<String>,
    limit: Option<u32>,
    params: serde_json::Value,
}

/// `mini-app://<table>[?scope=<s>&root=<dir>&alias=<name>&k=v*&limit=<n>]` を parse する
/// 内部 helper。 scheme prefix は呼び出し側で剥がす前提で受ける。
fn parse_mini_app_uri(rest: &str) -> WireResult<MiniAppUriSpec> {
    let full_uri = format!("mini-app://{rest}");
    let parsed = url::Url::parse(&full_uri)
        .map_err(|e| WireError::Storage(format!("mini-app adapter: bad uri: {full_uri}: {e}")))?;

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

fn resolve_root_path(raw: &str) -> WireResult<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| WireError::Storage("mini-app adapter: HOME unset".to_string()))?;
        Ok(PathBuf::from(home).join(rest))
    } else {
        Ok(PathBuf::from(raw))
    }
}

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

    async fn fetch_table_via_spec(&self, spec: &MiniAppUriSpec) -> WireResult<serde_json::Value> {
        let (_db_path, store) =
            open_mini_app_store(&spec.table, spec.scope.as_deref(), spec.root.as_deref()).await?;
        let effective_limit = spec.limit.or(Some(1000));
        let rows = store
            .list(effective_limit, None, None, None)
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

    async fn fetch_via_alias(&self, spec: &MiniAppUriSpec) -> WireResult<serde_json::Value> {
        let alias_name = spec
            .alias
            .as_deref()
            .ok_or_else(|| WireError::Storage("mini-app adapter: alias key missing".to_string()))?;

        let global_rec = resolve_alias_record(
            &spec.table,
            spec.scope.as_deref(),
            spec.root.as_deref(),
            alias_name,
        )
        .await?;

        if global_rec.aggregator.is_some() {
            return Err(WireError::Storage(format!(
                "mini-app adapter: alias '{alias_name}' has aggregator — wire scope 外 (P3b carry)"
            )));
        }
        match &global_rec.sources {
            mini_app_core::aggregator::SourceSpec::Single(_) => {}
            mini_app_core::aggregator::SourceSpec::Multi(_)
            | mini_app_core::aggregator::SourceSpec::Pattern(_) => {
                return Err(WireError::Storage(format!(
                    "mini-app adapter: alias '{alias_name}' has Multi / Pattern source — \
                     wire scope 外 (P3b carry)"
                )));
            }
        }

        let (db_path, store) =
            open_mini_app_store(&spec.table, spec.scope.as_deref(), spec.root.as_deref()).await?;

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

        let value = mini_app_core::alias_run::execute_alias_run(
            &registry,
            global_rec,
            Some(spec.params.clone()),
            Some(&spec.table),
            spec.limit,
            None,
            None,
            None,
        )
        .await
        .map_err(|e| {
            WireError::Storage(format!(
                "mini-app adapter: alias '{alias_name}' execute_alias_run failed: {e}"
            ))
        })?;

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

#[async_trait]
impl Adapter for MiniAppAdapter {
    fn scheme(&self) -> &'static str {
        "mini-app"
    }

    async fn fetch(
        &self,
        uri: &persona_wire_core::infrastructure::wire_uri::WireUri,
    ) -> WireResult<serde_json::Value> {
        let source_uri = uri.as_raw();
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

fn resolve_mini_app_table_dir(
    table: &str,
    scope: Option<&str>,
    root_override: Option<&std::path::Path>,
) -> WireResult<PathBuf> {
    let base = if let Some(root) = root_override {
        root.to_path_buf()
    } else {
        if let Some(s) = scope {
            if s != "user" {
                return Err(WireError::Storage(format!(
                    "mini-app adapter: scope='{s}' requires ?root=<dir> (table={table})"
                )));
            }
        }
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

fn resolve_mini_app_user_dir() -> WireResult<PathBuf> {
    match std::env::var("MINI_APP_USER_DIR") {
        Ok(p) if !p.is_empty() => Ok(PathBuf::from(p)),
        _ => {
            let home = std::env::var("HOME")
                .map_err(|_| WireError::Storage("mini-app adapter: HOME unset".to_string()))?;
            Ok(PathBuf::from(home).join(".mini-app"))
        }
    }
}

async fn resolve_alias_record(
    table: &str,
    scope: Option<&str>,
    root_override: Option<&std::path::Path>,
    alias_name: &str,
) -> WireResult<mini_app_core::alias_storage::AliasRecord> {
    let (project_dir, user_dir): (Option<PathBuf>, Option<PathBuf>) = match scope {
        Some("user") => (None, Some(resolve_mini_app_user_dir()?)),
        Some(s) => {
            let root = root_override.ok_or_else(|| {
                WireError::Storage(format!(
                    "mini-app adapter: scope='{s}' requires ?root=<dir> for _global.db resolve"
                ))
            })?;
            (Some(root.to_path_buf()), None)
        }
        None => (None, Some(resolve_mini_app_user_dir()?)),
    };
    let global_storage = mini_app_core::alias_storage::GlobalAliasStorage::open(
        project_dir.as_deref(),
        user_dir.as_deref(),
    )
    .map_err(|e| {
        WireError::Storage(format!(
            "mini-app adapter: GlobalAliasStorage::open failed: {e}"
        ))
    })?;

    match scope {
        Some("user") => {
            let rec = global_storage
                .alias_get_scope(mini_app_core::alias_storage::AliasScope::User, alias_name)
                .await
                .map_err(|e| {
                    WireError::Storage(format!(
                        "mini-app adapter: alias_get_scope(User, '{alias_name}') failed: {e}"
                    ))
                })?;
            rec.ok_or_else(|| {
                WireError::Storage(format!(
                    "mini-app adapter: alias '{alias_name}' not found in User scope _global.db"
                ))
            })
        }
        Some(s) => {
            let rec = global_storage
                .alias_get_scope(
                    mini_app_core::alias_storage::AliasScope::Project,
                    alias_name,
                )
                .await
                .map_err(|e| {
                    WireError::Storage(format!(
                        "mini-app adapter: alias_get_scope(Project, '{alias_name}') failed: {e}"
                    ))
                })?;
            rec.ok_or_else(|| {
                WireError::Storage(format!(
                    "mini-app adapter: alias '{alias_name}' not found in Project scope _global.db (scope='{s}')"
                ))
            })
        }
        None => match global_storage.alias_get(alias_name).await {
            Ok(rec) => Ok(rec),
            Err(mini_app_core::error::MiniAppError::AliasNotFound { .. }) => {
                fetch_per_table_alias_as_global(table, scope, root_override, alias_name).await
            }
            Err(e) => Err(WireError::Storage(format!(
                "mini-app adapter: GlobalAliasStorage::alias_get('{alias_name}') failed: {e}"
            ))),
        },
    }
}

async fn fetch_per_table_alias_as_global(
    table: &str,
    scope: Option<&str>,
    root_override: Option<&std::path::Path>,
    alias_name: &str,
) -> WireResult<mini_app_core::alias_storage::AliasRecord> {
    let (_db_path, store) = open_mini_app_store(table, scope, root_override).await?;
    let per_table_rec = store.alias_get(alias_name).await.map_err(|e| {
        WireError::Storage(format!(
            "mini-app adapter: alias '{alias_name}' not found in _global.db (User scope) nor \
             per-table {table}._aliases fallback: {e}"
        ))
    })?;
    Ok(mini_app_core::alias_storage::AliasRecord {
        name: per_table_rec.name,
        sources: mini_app_core::aggregator::SourceSpec::Single(String::new()),
        aggregator: None,
        filter: per_table_rec.filter,
        default_limit: per_table_rec.default_limit,
        description: per_table_rec.description,
        params_schema: per_table_rec.params_schema,
        fields: None,
        order_by: None,
        scope: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let spec = parse_mini_app_uri("t?alias=a&limit=10&alias_extra=x&limit_extra=y").unwrap();
        assert_eq!(spec.alias.as_deref(), Some("a"));
        assert_eq!(spec.limit, Some(10));
        assert_eq!(
            spec.params,
            serde_json::json!({"alias_extra": "x", "limit_extra": "y"})
        );
    }

    #[test]
    fn parse_mini_app_uri_scope_absent_is_legacy_path() {
        let spec = parse_mini_app_uri("mailbox?alias=unread").unwrap();
        assert_eq!(spec.scope, None);
        assert_eq!(spec.root, None);
    }

    #[test]
    fn parse_mini_app_uri_plain_form_with_scope_project_and_root() {
        let spec =
            parse_mini_app_uri("example_table?scope=example-project&root=/tmp/example-mini-app")
                .unwrap();
        assert_eq!(spec.table, "example_table");
        assert_eq!(spec.scope.as_deref(), Some("example-project"));
        assert_eq!(
            spec.root.as_deref(),
            Some(std::path::Path::new("/tmp/example-mini-app"))
        );
        assert_eq!(spec.alias, None);
        assert_eq!(spec.limit, None);
        assert_eq!(spec.params, serde_json::json!({}));
    }

    #[test]
    fn parse_mini_app_uri_plain_form_scope_project_without_root_rejects() {
        let r = parse_mini_app_uri("example_table?scope=example-project");
        assert!(r.is_err());
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("scope='example-project' requires ?root="),
            "expected scope+root error, got: {msg}"
        );
    }

    #[test]
    fn parse_mini_app_uri_plain_form_with_scope_user_and_limit() {
        let spec = parse_mini_app_uri("mailbox?scope=user&limit=50").unwrap();
        assert_eq!(spec.table, "mailbox");
        assert_eq!(spec.scope.as_deref(), Some("user"));
        assert_eq!(spec.alias, None);
        assert_eq!(spec.limit, Some(50));
    }

    #[test]
    fn parse_mini_app_uri_with_scope_user() {
        let spec = parse_mini_app_uri("mailbox?scope=user&alias=unread").unwrap();
        assert_eq!(spec.scope.as_deref(), Some("user"));
        assert_eq!(spec.root, None);
        assert_eq!(spec.alias.as_deref(), Some("unread"));
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
        assert_eq!(spec.params, serde_json::json!({}));
    }

    #[test]
    fn parse_mini_app_uri_scope_project_without_root_rejects() {
        let r = parse_mini_app_uri("t?scope=example-project&alias=x");
        assert!(r.is_err());
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("scope='example-project' requires ?root="),
            "expected scope+root error, got: {msg}"
        );
    }

    #[test]
    fn parse_mini_app_uri_scope_user_without_root_is_ok() {
        let spec = parse_mini_app_uri("t?scope=user").unwrap();
        assert_eq!(spec.scope.as_deref(), Some("user"));
        assert_eq!(spec.root, None);
    }

    #[test]
    fn parse_mini_app_uri_with_root_tilde_expands_home() {
        let original = std::env::var("HOME").ok();
        // SAFETY: unit test process, single-threaded mutation of env for the duration of this scope.
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
        assert_eq!(r, std::path::PathBuf::from("/var/wire-data/kv"));
    }

    #[test]
    fn resolve_dir_scope_project_without_root_defensive_error() {
        let r = resolve_mini_app_table_dir("kv", Some("example-project"), None);
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("requires ?root="));
    }

    #[tokio::test]
    async fn adapter_scheme_is_mini_app() {
        let a = MiniAppAdapter;
        assert_eq!(a.scheme(), "mini-app");
    }

    #[tokio::test]
    async fn adapter_rejects_non_mini_app_uri() {
        use persona_wire_core::infrastructure::wire_uri::WireUri;
        let a = MiniAppAdapter;
        let uri = WireUri::parse("file:///tmp/x").unwrap();
        let r = a.fetch(&uri).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("bad uri"));
    }
}
