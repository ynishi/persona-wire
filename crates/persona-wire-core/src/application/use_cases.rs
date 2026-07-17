//! Use cases — orchestration of Domain + Infrastructure for wire_* flows.

use crate::application::plugin_registry::PluginRegistry;
use crate::application::projection_registry::{ProjectionRegistry, TargetForm};
use crate::application::spec_registry::SpecRegistry;
use crate::domain::error::{DomainError, WireError, WireResult};
use crate::domain::graph::Node;
use crate::domain::port::ProjectionInput;
use crate::domain::specification::Specification;
use crate::infrastructure::storage::SqliteStorage;
use crate::infrastructure::wire_uri::WireUri;

/// Resolve a `TemplateEngine` from `registry` by id, falling back to the
/// `"handlebars"` default when `hint` is `None`. Surfaces a structured
/// `WireError::Storage` when neither the hinted id nor the default is
/// registered. P3a Phase 2 (b) — common helper for the 3 use_case render sites.
fn resolve_engine_render(
    registry: &PluginRegistry,
    hint: Option<&str>,
    template: &str,
    data: &serde_json::Value,
) -> WireResult<String> {
    let id = hint.unwrap_or("handlebars");
    let engine = registry
        .engine(id)
        .ok_or_else(|| WireError::Storage(format!("template engine '{id}' not registered")))?;
    engine.render(template, data)
}

/// Assert that `projection_kind` is one of the synchronous-safe values
/// (`None` or `Some("static")`). Returns a structured error otherwise so the
/// caller surfaces a clear "use the async path" message instead of silently
/// falling back to the engine-direct sync path.
///
/// P3a Phase 2 (c) — guards `wire_init` / `wire_render` (both sync). Any
/// `projection_kind` other than `"static"` only animates through
/// `wire_prompt_context`, which is async.
fn assert_static_projection_kind(
    projection_name: &str,
    projection_kind: Option<&str>,
) -> WireResult<()> {
    match projection_kind {
        None | Some("static") => Ok(()),
        Some(other) => Err(WireError::Other(format!(
            "projection '{projection_name}' has projection_kind '{other}' — \
             non-static kinds require the async path; use wire_prompt_context instead"
        ))),
    }
}

/// Build the broadcast-shape render data JSON for sync use cases
/// (`wire_init` / `wire_render`).
///
/// Shape:
/// ```json
/// { "count": N, "names": "id1, id2, …", "nodes": [...], "persona_id": "…" }
/// ```
///
/// `persona_id` is included only when `Some` is passed; `wire_render` calls
/// with `None` since it is name-addressed (no implicit persona scope).
///
/// Step C-6 phase 2 — broadcast data shape (graph spec result aggregated
/// into a single object) is distinct from the per-slot shape used by the
/// async `wire_prompt_context` path; see the crate-level "Slot vocabulary"
/// rationale in [`crate`] docs.
fn build_broadcast_render_data(matched: &[Node], persona_id: Option<&str>) -> serde_json::Value {
    let names: Vec<&str> = matched.iter().map(|n| n.name.as_str()).collect();
    let nodes_json: Vec<serde_json::Value> = matched
        .iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id,
                "type": n.r#type,
                "metadata": n.metadata,
            })
        })
        .collect();
    let mut obj = serde_json::json!({
        "count": matched.len(),
        "names": names.join(", "),
        "nodes": nodes_json,
    });
    if let Some(pid) = persona_id {
        obj.as_object_mut()
            .expect("json!({...}) constructs an object")
            .insert("persona_id".to_string(), serde_json::json!(pid));
    }
    obj
}

/// Render a Projection against a pre-built data JSON via the **sync** engine
/// path (`wire_init` / `wire_render`). Encapsulates the shared post-spec
/// dispatch: plugin parts extraction, static-kind guard, engine render, and
/// `RenderedProjection` construction.
///
/// Non-static projection kinds (e.g. `llm`) surface a structured error so the
/// caller hops to `wire_prompt_context` (async) instead of silently falling
/// back to engine-direct rendering.
///
/// Step C-6 phase 2 — shared by both sync use cases; the async path uses
/// `resolve_projection_render_async` (full plugin Projection dispatch).
fn render_named_projection_sync(
    proj: &crate::domain::entity::Projection,
    data: &serde_json::Value,
    registry: &PluginRegistry,
) -> WireResult<RenderedProjection> {
    let (engine_hint, kind_hint, _config) = proj.plugin().to_optional_parts();
    assert_static_projection_kind(proj.name().as_str(), kind_hint)?;
    let rendered = resolve_engine_render(registry, engine_hint, proj.template().as_str(), data)?;
    Ok(RenderedProjection {
        name: proj.name().as_str().to_owned(),
        target_form: proj.target_form(),
        rendered,
    })
}

/// Async render path that dispatches through `PluginRegistry`'s `Projection`
/// axis. Used by `wire_prompt_context` (already async). Sync use cases
/// short-circuit through `resolve_engine_render` after
/// `assert_static_projection_kind`.
///
/// Resolution order:
/// - `template_engine_hint` (defaults to `"handlebars"`) — sanity-checked against
///   the registry to surface unknown-engine errors early; the actual engine is
///   held by the resolved [`ProjectionRenderer`] adapter (Hole-1 解消).
/// - `projection_kind_hint` (defaults to `"static"`)
///
/// Both must be registered in `registry`; missing ids surface a structured
/// `WireError::Storage`.
///
/// P3a Phase 2 (c) — the actual consumer of `NamedProjection.projection_kind`.
///
/// [`ProjectionRenderer`]: crate::domain::port::ProjectionRenderer
#[allow(clippy::too_many_arguments)]
async fn resolve_projection_render_async(
    registry: &PluginRegistry,
    template_engine_hint: Option<&str>,
    projection_kind_hint: Option<&str>,
    template: &str,
    target_form: TargetForm,
    spec_result: &serde_json::Value,
    persona_id: Option<&str>,
    config: Option<&serde_json::Value>,
) -> WireResult<String> {
    let engine_id = template_engine_hint.unwrap_or("handlebars");
    if registry.engine(engine_id).is_none() {
        return Err(WireError::Storage(format!(
            "template engine '{engine_id}' not registered"
        )));
    }
    let kind_id = projection_kind_hint.unwrap_or("static");
    let projection = registry
        .projection(kind_id)
        .ok_or_else(|| WireError::Storage(format!("projection kind '{kind_id}' not registered")))?;
    let null = serde_json::Value::Null;
    let input = ProjectionInput {
        spec_result,
        template,
        target_form,
        persona_id,
        config: config.unwrap_or(&null),
    };
    projection.render(input).await
}

// ---- wire_init ----

pub struct WireInitInput {
    pub persona_id: String,
}

#[derive(Debug)]
pub struct RenderedProjection {
    pub name: String,
    pub target_form: TargetForm,
    pub rendered: String,
}

pub struct WireInitOutput {
    pub persona_id: String,
    pub projections: Vec<RenderedProjection>,
    pub warnings: Vec<String>,
}

/// Run every registered NamedProjection against the current graph and return
/// the rendered context bundle. **P1 互換 (sync)** = wire 内 graph の data 本体
/// を render する旧 path。 新規 `wire_prompt_context` (async + Adapter 経由) で
/// Layer 6 Adapter fresh fetch 経路に置き換える前提、 本 fn は P1 contract / test
/// 維持のため sync で残す。
pub fn wire_init(
    input: WireInitInput,
    storage: &SqliteStorage,
    registry: &PluginRegistry,
) -> WireResult<WireInitOutput> {
    let spec_reg = SpecRegistry::new(storage);
    let proj_reg = ProjectionRegistry::new(storage);

    let mut projections = Vec::new();
    let mut warnings = Vec::new();

    for name in proj_reg.list()? {
        let Some(proj) = proj_reg.get(&name)? else {
            continue;
        };
        let Some(spec) = spec_reg.get(proj.spec_ref().as_str())? else {
            warnings.push(format!(
                "projection '{name}': spec_ref '{}' not registered",
                proj.spec_ref()
            ));
            continue;
        };

        let matched = collect_matching_nodes(storage, &spec)?;
        let data = build_broadcast_render_data(&matched, Some(input.persona_id.as_str()));
        projections.push(render_named_projection_sync(&proj, &data, registry)?);
    }

    Ok(WireInitOutput {
        persona_id: input.persona_id,
        projections,
        warnings,
    })
}

// ---- wire_prompt_context (Layer 6 Adapter + persona-pack 配線 SoT 経路) ----

#[derive(Debug)]
pub struct WirePromptContextInput {
    pub persona_id: String,
    /// `Some(["active", "ng"])` で該当 slot のみ render、 `None` で全 slot。
    pub projection_names: Option<Vec<String>>,
    /// `Some(["mail", "news"])` で該当 slot を除外、 `None` で除外なし。
    /// `projection_names` と組み合わせ可 — semantics は AND NOT:
    ///
    /// | projection_names | projection_exclude_names | 結果集合                            |
    /// |------------------|--------------------------|-------------------------------------|
    /// | None             | None                     | 全 projection (現挙動互換)          |
    /// | Some([...])      | None                     | include 集合のみ (現挙動互換)       |
    /// | None             | Some([...])              | 全 projection \ exclude             |
    /// | Some([...])      | Some([...])              | include \ exclude (AND NOT)         |
    ///
    /// 交差時は exclude が優先 (= 明示除外が勝つ)、 未登録 name は無視
    /// (warning なし、 後方互換性優先)、 結果空集合は空 context 返却。
    pub projection_exclude_names: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct WirePromptContextOutput {
    pub persona_id: String,
    /// 全 projection を rendered block 化して concat した PromptContext literal。
    pub prompt_context: String,
    /// 個別 rendered block (= 各 projection 1 件)。
    pub projections: Vec<RenderedProjection>,
    pub warnings: Vec<String>,
}

/// 各 slot 1 件の Phase 1 sync collect 結果。
struct CollectedSlot {
    slot: String,
    source_uri: String,
    target_form: TargetForm,
    template: String,
    /// P3a Phase 2 (b) — NamedProjection 由来の `template_engine` を Phase 2
    /// render dispatch まで運ぶ。 None → `"handlebars"` default。
    template_engine: Option<String>,
    /// P3a Phase 2 (c) — NamedProjection 由来の `projection_kind` を Phase 2
    /// render dispatch まで運ぶ。 None → `"static"` default。
    projection_kind: Option<String>,
    /// P3a Phase 2 (c) — NamedProjection 由来の `projection_config` を Phase 2
    /// render dispatch まで運ぶ (例: LLM endpoint / cache TTL)。
    projection_config: Option<serde_json::Value>,
    /// Projection 名 (= `<persona>.section.<slot>`)。 エラー / warning メッセージで
    /// projection を指し示すのに使う。
    projection_name: String,
    /// Wiring entry's `metadata.auth` (credential reference key, never a
    /// secret — see `application::auth` module docs). `None` when the entry
    /// authenticates via the adapter's literal default service name.
    /// Consumed by `render_collected_slot_async` via [`merge_auth_query`].
    auth: Option<String>,
}

/// 全 builtin slot (or projection_names で subset) を iterate し、 各 slot の
/// **配線 (source_uri)** を **wire DB の wiring entry `<persona>.<slot>`** から取得、
/// **template** を 3 段優先 (1: persona-pack overlay × `MergeStrategy.merge` / 2: wire
/// DB の動的 register projection `<persona>.section.<slot>` / 3: `BUILTIN_PROJECTIONS`)
/// で解決して Adapter で fresh fetch + render し、 全 slot を concat した
/// **PromptContext** を 1 call で return する `/wake` 用 entry。
///
/// 設計確定 (2026-06-16 reframe):
/// - 配線 SoT = **wire DB wiring entry**。 persona-pack には書かない (= 二重管理 drift 防止)
/// - persona-pack `[extra.persona_wire.projections.<slot>]` は **Projection template の
///   Overlay only** (persona 固有 emote / register 等を `MergeStrategy` 指定で被せる)
/// - `projection_names: Some([...])` で subset 指定可能 (= 動的 Selection)
pub async fn wire_prompt_context(
    input: WirePromptContextInput,
    storage: std::sync::Arc<std::sync::Mutex<SqliteStorage>>,
    registry: &PluginRegistry,
) -> WireResult<WirePromptContextOutput> {
    let overlays = resolve_persona_overlays(&input.persona_id, registry).await;

    let mut warnings = Vec::new();
    let collected: Vec<CollectedSlot> = {
        let s = storage.lock().map_err(|_| {
            crate::domain::error::WireError::Storage("storage mutex poisoned".to_string())
        })?;
        let proj_reg = ProjectionRegistry::new(&s);
        let slots = enumerate_slot_names(
            &s,
            &input.persona_id,
            input.projection_names.as_deref(),
            input.projection_exclude_names.as_deref(),
        )?;
        let mut out: Vec<CollectedSlot> = Vec::new();
        for slot in &slots {
            if let Some(c) = collect_slot(
                slot,
                &input.persona_id,
                &s,
                &proj_reg,
                &overlays,
                &mut warnings,
            )? {
                out.push(c);
            }
        }
        out
    };

    let mut projections = Vec::new();
    for c in &collected {
        projections.push(
            render_collected_slot_async(c, &input.persona_id, registry, &mut warnings).await?,
        );
    }

    let prompt_context = projections
        .iter()
        .map(|p| p.rendered.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(WirePromptContextOutput {
        persona_id: input.persona_id,
        prompt_context,
        projections,
        warnings,
    })
}

/// Phase 0 — async overlay resolution via PluginRegistry adapter dispatch.
///
/// URI 形式 = `persona-pack://<persona_id>/projections`。 persona-pack scheme は
/// 外部 adapter crate (`persona-wire-adapter-persona-pack`) が提供する ACL Facade。
/// boot 側 (`persona-wire-mcp` / `persona-wire` bin) で registry に inject 済
/// (未 inject = scheme 未登録 → overlay 空で fallback、 adapter fetch fail も同様)。
async fn resolve_persona_overlays(
    persona_id: &str,
    registry: &PluginRegistry,
) -> std::collections::BTreeMap<String, crate::application::projection_overlay::ProjectionOverlay> {
    use crate::application::projection_overlay::parse_overlay_response;
    let overlay_uri = format!("persona-pack://{}/projections", persona_id);
    match registry.route(&overlay_uri) {
        Ok((adapter, uri)) => match adapter.fetch(&uri).await {
            Ok(v) => parse_overlay_response(&v).unwrap_or_default(),
            Err(_) => std::collections::BTreeMap::new(),
        },
        Err(_) => std::collections::BTreeMap::new(),
    }
}

/// Phase 1 helper — slot 名集合を確定する。 `explicit` で subset 指定があれば
/// そのまま使い、 None なら wire DB の wiring entry (= persona-scoped Node) を
/// spec query で全件取得し、 `wiring_mapper::extract_slot` 経由で slot 名を
/// 抽出する (storage 互換 key `metadata.axis` の直リードは禁止、
/// crate-level "Slot vocabulary" rationale 参照)。
///
/// `exclude` で除外 slot 名集合を指定すると、 `explicit` / 全件 enumerate の
/// 結果から exclude 集合を引いた残りを返す (`WirePromptContextInput`
/// docstring の AND NOT semantics 参照)。 交差時は exclude 優先 (= 明示除外が
/// 勝つ)、 未登録 name は無視。
fn enumerate_slot_names(
    storage: &SqliteStorage,
    persona_id: &str,
    explicit: Option<&[String]>,
    exclude: Option<&[String]>,
) -> WireResult<Vec<String>> {
    use crate::application::wiring_mapper;
    let base: Vec<String> = if let Some(names) = explicit {
        names.to_vec()
    } else {
        let spec = Specification::And(vec![
            Specification::TypeIs(wiring_mapper::WIRING_TYPE.to_string()),
            Specification::MetadataEq {
                path: wiring_mapper::META_PERSONA.to_string(),
                value: serde_json::json!(persona_id),
            },
        ]);
        let nodes = collect_matching_nodes(storage, &spec)?;
        nodes
            .iter()
            .filter_map(|n| wiring_mapper::extract_slot(n).map(str::to_owned))
            .collect()
    };
    if let Some(skip) = exclude {
        if !skip.is_empty() {
            let skip_set: std::collections::BTreeSet<&str> =
                skip.iter().map(String::as_str).collect();
            return Ok(base
                .into_iter()
                .filter(|s| !skip_set.contains(s.as_str()))
                .collect());
        }
    }
    Ok(base)
}

/// Phase 1 per-slot collect — 1 slot 分の wiring entry resolve + base projection
/// lookup + overlay merge を行い、 Phase 2 (async) に渡す `CollectedSlot` を返す。
///
/// 返値:
/// - `Ok(Some(_))` — 配線 + projection 完備、 render 対象
/// - `Ok(None)` — 未配線 (silent skip)、 source_uri 不在 (warning push)、
///   projection 未登録 (warning push) のいずれか
///
/// 名前 derive は `application::projection_naming` に集約 (doctor Probe 等が
/// 同じ rule で resolve できるよう single SoT 化、 issue 19d888ee / 25544968)。
fn collect_slot(
    slot: &str,
    persona_id: &str,
    storage: &SqliteStorage,
    proj_reg: &ProjectionRegistry,
    overlays: &std::collections::BTreeMap<
        String,
        crate::application::projection_overlay::ProjectionOverlay,
    >,
    warnings: &mut Vec<String>,
) -> WireResult<Option<CollectedSlot>> {
    let node_id = format!("{}.{}", persona_id, slot);
    let Some(node) = storage.get_node_by_name(&node_id)? else {
        return Ok(None);
    };
    let Some(source_uri) = crate::application::wiring_mapper::extract_source_uri(&node) else {
        warnings.push(format!(
            "wiring entry '{node_id}' lacks metadata.source_uri — slot skipped"
        ));
        return Ok(None);
    };
    let auth = crate::application::wiring_mapper::extract_auth(&node).map(str::to_owned);

    // Explicit `metadata.projection_ref` wins over the naming convention —
    // this is what lets one registered projection serve multiple personas
    // and makes the bundle `[[wirings]].projection_ref` field functional.
    // Absent → `<persona>.section.<slot>` convention (the common case).
    let projection_name = match crate::application::wiring_mapper::extract_projection_ref(&node) {
        Some(explicit) => explicit.to_owned(),
        None => {
            crate::application::projection_naming::workflow_emit_projection_name(persona_id, slot)
        }
    };
    let (base_template, base_target, base_engine, base_kind, base_config) =
        match proj_reg.get(&projection_name)? {
            Some(proj) => {
                let (engine, kind, config) = proj.plugin().to_optional_parts();
                (
                    proj.template().as_str().to_owned(),
                    proj.target_form(),
                    engine.map(str::to_owned),
                    kind.map(str::to_owned),
                    config.cloned(),
                )
            }
            None => {
                warnings.push(format!(
                    "slot '{slot}' has no registered projection \
                     '{projection_name}' — slot skipped"
                ));
                return Ok(None);
            }
        };

    // overlay merge (MergeStrategy 経由)。 template_engine / projection_kind
    // / projection_config は overlay schema にまだ field がないため、
    // NamedProjection 由来をそのまま運ぶ (P3a Phase 2 (c))。
    let (final_template, final_target) = if let Some(o) = overlays.get(slot) {
        (o.strategy.merge(&base_template, &o.template), o.target_form)
    } else {
        (base_template, base_target)
    };

    Ok(Some(CollectedSlot {
        slot: slot.to_string(),
        source_uri: source_uri.to_string(),
        target_form: final_target,
        template: final_template,
        template_engine: base_engine,
        projection_kind: base_kind,
        projection_config: base_config,
        projection_name,
        auth,
    }))
}

/// Phase 2 per-slot async fetch + render — Adapter dispatch で fresh fetch、
/// `Projection` trait dispatch で render、 `RenderedProjection` を返す。
///
/// fetch fail / route fail は `serde_json::Value::Null` に倒して warning push
/// で先に進む (= 個別 slot の失敗で全体を落とさない best-effort)。
///
/// P3a Phase 2 (c) — `projection_kind` default は `"static"` (=
/// `StaticProjection` = engine-direct 相当)、 外部 Projection plugin (例 `llm`)
/// はここを経由してのみ animate する (sync use cases は通らない)。
async fn render_collected_slot_async(
    c: &CollectedSlot,
    persona_id: &str,
    registry: &PluginRegistry,
    warnings: &mut Vec<String>,
) -> WireResult<RenderedProjection> {
    // Indirect auth reference layer — `c.auth` is the
    // wiring entry's `metadata.auth` (credential reference key, never a
    // secret). Merge it into the raw `source_uri` as `?auth=<key>` for this
    // one fetch, unless the URI already declares its own `auth` param (URI
    // wins, never overwritten). `c.source_uri` itself (and the
    // `wiring_entry.source_uri` field surfaced below) stays the stored,
    // unmerged value — only the URI actually routed/fetched carries the
    // merge.
    let fetch_uri = merge_auth_query(&c.source_uri, c.auth.as_deref());
    let fetched = match registry.route(&fetch_uri) {
        Ok((adapter, uri)) => match adapter.fetch(&uri).await {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!(
                    "adapter fetch failed for slot '{}' (uri={}): {e}",
                    c.slot, c.source_uri
                ));
                serde_json::Value::Null
            }
        },
        Err(e) => {
            warnings.push(format!(
                "registry route failed for slot '{}' (uri={}): {e}",
                c.slot, c.source_uri
            ));
            serde_json::Value::Null
        }
    };
    let fetched_is_null = fetched.is_null();
    let entries = vec![serde_json::json!({
        "wiring_entry": {
            "slot": c.slot,
            "source_uri": c.source_uri,
        },
        "fetched_data": fetched,
    })];
    let data = serde_json::json!({
        "count": 1,
        "slot": c.slot,
        "entries": entries,
        "persona_id": persona_id,
    });
    let rendered = resolve_projection_render_async(
        registry,
        c.template_engine.as_deref(),
        c.projection_kind.as_deref(),
        &c.template,
        c.target_form,
        &data,
        Some(persona_id),
        c.projection_config.as_ref(),
    )
    .await?;
    // Adapter returned data but the template produced nothing — the typical
    // cause is a field-path typo against the adapter's return shape (e.g.
    // `fetched_data.content` vs the file adapter's `fetched_data.body`).
    // Handlebars resolves missing paths to empty strings, so without this
    // warning the mistake is invisible. Preview the raw shape via wire_fetch.
    if !fetched_is_null && rendered.trim().is_empty() {
        warnings.push(format!(
            "slot '{}' rendered empty output despite non-null fetched_data — \
             check the template's field paths against `wire_fetch` output \
             (projection '{}')",
            c.slot, c.projection_name
        ));
    }
    Ok(RenderedProjection {
        name: c.projection_name.clone(),
        target_form: c.target_form,
        rendered,
    })
}

/// Merges a wiring entry's `metadata.auth` service key into `source_uri` as
/// an `?auth=<key>` query param, per the convention documented in
/// `infrastructure::adapter`'s "External service integration policy".
///
/// - `meta_auth: None` (no `metadata.auth` on the wiring entry) → `source_uri`
///   unchanged.
/// - `meta_auth: Some(key)` and `source_uri` has **no** `auth` query param →
///   `key` is appended.
/// - `meta_auth: Some(key)` but `source_uri` **already** declares its own
///   `auth` query param → `source_uri` unchanged (URI wins, never
///   overwritten — a URI-declared `auth` always takes precedence over the
///   wiring entry's `metadata.auth`).
/// - `source_uri` fails to parse as a [`WireUri`] → unchanged; the real
///   parse error surfaces downstream from `PluginRegistry::route` instead
///   of being masked here.
fn merge_auth_query(source_uri: &str, meta_auth: Option<&str>) -> String {
    let Some(key) = meta_auth else {
        return source_uri.to_string();
    };
    match WireUri::parse(source_uri) {
        Ok(parsed) if parsed.query_get("auth").is_none() => {
            append_query_param(source_uri, "auth", key)
        }
        _ => source_uri.to_string(),
    }
}

/// Appends a `key=value` query param to `raw_uri`, choosing `?` or `&` based
/// on whether a query string is already present, and re-inserting any
/// `#fragment` after the new param (fragments always trail the query
/// string per RFC 3986). Operates on the raw string only — does not
/// re-interpret scheme/host/path, which stay the exclusive concern of
/// [`WireUri::parse`] and the adapters.
fn append_query_param(raw_uri: &str, key: &str, value: &str) -> String {
    let (base, fragment) = match raw_uri.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (raw_uri, None),
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    let merged = format!("{base}{sep}{key}={value}");
    match fragment {
        Some(f) => format!("{merged}#{f}"),
        None => merged,
    }
}

// ---- wire_slot_register / wire_slot_delete (one-shot slot setup) ----

/// Input for [`wire_slot_register`] — the minimal real information a slot
/// needs. Everything else (node name, spec body, projection name) is derived
/// from the `(persona_id, slot)` pair by the same conventions the render
/// path applies.
#[derive(Debug)]
pub struct WireSlotRegisterInput {
    pub persona_id: String,
    pub slot: String,
    pub source_uri: String,
    /// Handlebars template rendered against the `wire_prompt_context` data
    /// shape (`entries[].fetched_data` — preview via [`wire_fetch`]).
    pub template: String,
    pub target_form: TargetForm,
    /// `Some(_)` overwrites the flag; `None` leaves an existing value alone.
    pub maintenance_exempt: Option<bool>,
    /// Optional credential reference key (never a secret) — stored as
    /// `metadata.auth`, merged into the fetch URI at render time.
    pub auth: Option<String>,
}

#[derive(Debug)]
pub struct WireSlotRegisterOutput {
    /// Wiring node name (`<persona>.<slot>`).
    pub node_name: String,
    /// Wiring node ULID (fresh on create; preserved on upsert).
    pub node_id: String,
    /// `true` when the wiring node was created; `false` when an existing
    /// node's metadata was updated in place.
    pub node_created: bool,
    /// Auto-registered boilerplate spec name (`<persona>.spec.<slot>`).
    pub spec_name: String,
    /// Registered projection name (`<persona>.section.<slot>`).
    pub projection_name: String,
}

/// One-shot slot setup — a macro over the three registrations the onboarding
/// guide walks through by hand (`wire_node_create` + `wire_spec_register` +
/// `wire_projection_register`), collapsing the caller-facing surface to the
/// five values that carry real information: persona / slot / source_uri /
/// template / target_form.
///
/// Derivations (single SoT with the render path):
/// - node name = `<persona>.<slot>` (`Wiring::storage_node_id` form)
/// - spec name = `<persona>.spec.<slot>`, body = the standard 3-clause shape
///   (`TypeIs(outline_node) AND persona AND axis`)
/// - projection name = `<persona>.section.<slot>`
///   (`projection_naming::workflow_emit_projection_name`)
///
/// Upsert semantics: the spec / projection registries already upsert by
/// name; the wiring node is matched by name and its canonical metadata keys
/// are merged in place (ULID preserved, passthrough keys kept). Re-invoking
/// with changed values tunes the slot without a delete + recreate dance.
pub fn wire_slot_register(
    input: WireSlotRegisterInput,
    storage: &SqliteStorage,
) -> WireResult<WireSlotRegisterOutput> {
    use crate::application::wiring_mapper;
    use crate::domain::entity::{PersonaId, Slot, Source};

    // VO validation up front — fail before any write.
    let persona = PersonaId::new(input.persona_id.clone())?;
    let slot = Slot::new(input.slot.clone())?;
    let source = Source::new(input.source_uri.clone())?;

    let node_name = format!("{}.{}", persona.as_str(), slot.as_str());
    let spec_name = format!("{}.spec.{}", persona.as_str(), slot.as_str());
    let projection_name = crate::application::projection_naming::workflow_emit_projection_name(
        persona.as_str(),
        slot.as_str(),
    );

    // 1) Wiring node — upsert by name (merge canonical keys, keep the rest).
    let mut extras = serde_json::Map::new();
    if let Some(flag) = input.maintenance_exempt {
        extras.insert(
            wiring_mapper::META_MAINTENANCE_EXEMPT.to_string(),
            serde_json::Value::Bool(flag),
        );
    }
    if let Some(auth) = &input.auth {
        extras.insert(
            wiring_mapper::META_AUTH.to_string(),
            serde_json::Value::String(auth.clone()),
        );
    }
    let canonical = wiring_mapper::wiring_metadata_object(&persona, &slot, &source, Some(extras));

    let (node_id, node_created) = match storage.get_node_by_name(&node_name)? {
        Some(existing) => {
            let mut base = match existing.metadata {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            if let serde_json::Value::Object(patch) = &canonical {
                for (k, v) in patch {
                    base.insert(k.clone(), v.clone());
                }
            }
            let updated =
                storage.update_node_metadata(&existing.id, &serde_json::Value::Object(base))?;
            if !updated {
                return Err(WireError::Storage(format!(
                    "wire_slot_register: node '{node_name}' vanished between read and write"
                )));
            }
            (existing.id, false)
        }
        None => {
            let node = Node {
                id: crate::domain::graph::Ulid::new(),
                name: node_name.clone(),
                r#type: wiring_mapper::WIRING_TYPE.to_string(),
                sot_ref: None,
                confidence: None,
                applicability: None,
                last_verified_at: None,
                review_due: None,
                version: 1,
                prev_id: None,
                metadata: canonical,
            };
            storage.insert_node(&node)?;
            (node.id, true)
        }
    };

    // 2) Boilerplate spec — the standard 3-clause per-slot shape. Registered
    //    for compatibility with the granular surface (`wire_query` /
    //    `wire_render` / `wire_projection_register.spec_ref`); the
    //    prompt-context hot path resolves by name convention and does not
    //    evaluate it.
    let spec = Specification::And(vec![
        Specification::TypeIs(wiring_mapper::WIRING_TYPE.to_string()),
        Specification::MetadataEq {
            path: wiring_mapper::META_PERSONA.to_string(),
            value: serde_json::json!(persona.as_str()),
        },
        Specification::MetadataEq {
            path: wiring_mapper::META_SLOT.to_string(),
            value: serde_json::json!(slot.as_str()),
        },
    ]);
    SpecRegistry::new(storage).register(&spec_name, &spec)?;

    // 3) Projection — convention name, referencing the auto spec.
    let projection = crate::domain::entity::projection::Projection::from_parts(
        projection_name.clone(),
        spec_name.clone(),
        input.template,
        input.target_form,
        crate::domain::entity::projection::PluginDispatch::Default,
    )?;
    ProjectionRegistry::new(storage).register(&projection)?;

    Ok(WireSlotRegisterOutput {
        node_name,
        node_id: node_id.to_string(),
        node_created,
        spec_name,
        projection_name,
    })
}

#[derive(Debug)]
pub struct WireSlotDeleteInput {
    pub persona_id: String,
    pub slot: String,
}

#[derive(Debug)]
pub struct WireSlotDeleteOutput {
    pub node_name: String,
    pub node_deleted: bool,
    pub spec_name: String,
    pub spec_deleted: bool,
    pub projection_name: String,
    pub projection_deleted: bool,
}

/// Counterpart to [`wire_slot_register`] — removes the wiring node, the
/// auto-registered spec, and the convention-named projection. Idempotent:
/// missing artifacts report `false` instead of erroring.
pub fn wire_slot_delete(
    input: WireSlotDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireSlotDeleteOutput> {
    let node_name = format!("{}.{}", input.persona_id, input.slot);
    let spec_name = format!("{}.spec.{}", input.persona_id, input.slot);
    let projection_name = crate::application::projection_naming::workflow_emit_projection_name(
        &input.persona_id,
        &input.slot,
    );

    let node_deleted = match storage.lookup_node_id_by_name(&node_name)? {
        Some(id) => storage.delete_node(&id)?,
        None => false,
    };
    let spec_deleted = match storage.resolve_specification_id_or_name(&spec_name)? {
        Some(id) => storage.delete_specification(&id)?,
        None => false,
    };
    let projection_deleted = match storage.resolve_projection_id_or_name(&projection_name)? {
        Some(id) => storage.delete_projection(&id)?,
        None => false,
    };

    Ok(WireSlotDeleteOutput {
        node_name,
        node_deleted,
        spec_name,
        spec_deleted,
        projection_name,
        projection_deleted,
    })
}

// ---- wire_fetch (raw adapter preview) ----

/// Input for [`wire_fetch`] — either a raw `source_uri`, or a
/// `(persona_id, slot)` pair resolving an existing wiring entry (which also
/// applies the entry's `metadata.auth` merge, matching what the render path
/// fetches). Exactly one of the two forms must be supplied.
#[derive(Debug)]
pub struct WireFetchInput {
    pub source_uri: Option<String>,
    pub persona_id: Option<String>,
    pub slot: Option<String>,
}

#[derive(Debug)]
pub struct WireFetchOutput {
    /// The stored / supplied URI (auth merge, when any, is not echoed —
    /// mirrors `wiring_entry.source_uri` staying unmerged in render context).
    pub source_uri: String,
    /// The adapter's return value verbatim — exactly what templates see as
    /// `entries[].fetched_data`.
    pub fetched_data: serde_json::Value,
}

/// Raw adapter preview — routes a URI through the same `PluginRegistry`
/// dispatch the render path uses and returns the adapter output verbatim.
/// This closes the template-authoring loop: preview the `fetched_data`
/// shape, then write the handlebars against it. Adapter errors fail loud
/// (no silent `Null` fallback — unlike the render path's best-effort mode,
/// a preview call wants to see the failure).
pub async fn wire_fetch(
    input: WireFetchInput,
    storage: std::sync::Arc<std::sync::Mutex<SqliteStorage>>,
    registry: &PluginRegistry,
) -> WireResult<WireFetchOutput> {
    let (stored_uri, fetch_uri) = match (&input.source_uri, &input.persona_id, &input.slot) {
        (Some(uri), None, None) => (uri.clone(), uri.clone()),
        (None, Some(persona), Some(slot)) => {
            let node_name = format!("{persona}.{slot}");
            let node = {
                let s = storage
                    .lock()
                    .map_err(|_| WireError::Storage("storage mutex poisoned".to_string()))?;
                s.get_node_by_name(&node_name)?.ok_or_else(|| {
                    WireError::Domain(DomainError::NotFound(format!("wiring entry: {node_name}")))
                })?
            };
            let source_uri = crate::application::wiring_mapper::extract_source_uri(&node)
                .ok_or_else(|| {
                    WireError::Domain(DomainError::InvalidMetadata(format!(
                        "wiring entry '{node_name}' lacks metadata.source_uri"
                    )))
                })?
                .to_owned();
            let auth = crate::application::wiring_mapper::extract_auth(&node);
            let merged = merge_auth_query(&source_uri, auth);
            (source_uri, merged)
        }
        _ => {
            return Err(WireError::Other(
                "wire_fetch: supply either `source_uri` alone, or `persona_id` + `slot`"
                    .to_string(),
            ))
        }
    };

    let (adapter, uri) = registry.route(&fetch_uri)?;
    let fetched_data = adapter.fetch(&uri).await?;

    Ok(WireFetchOutput {
        source_uri: stored_uri,
        fetched_data,
    })
}

/// Iterate every registered node type and collect nodes matching `spec`.
fn collect_matching_nodes(storage: &SqliteStorage, spec: &Specification) -> WireResult<Vec<Node>> {
    let mut out = Vec::new();
    for t in storage.list_types_by_kind("node")? {
        for n in storage.list_nodes_by_type(&t)? {
            if spec.is_satisfied_by(&n) {
                out.push(n);
            }
        }
    }
    Ok(out)
}

// ---- graph scan (shared by wire_close + wire_doctor) ----

/// Shared graph health summary used by `wire_close` (persona-scoped report)
/// and `wire_doctor` (orphan-only diagnostic).
pub struct GraphScanSummary {
    pub orphan_node_count: usize,
    pub total_node_count: usize,
    pub total_edge_count: usize,
}

/// A wiring entry is "self-attached" — and therefore not an orphan — when it
/// carries either a `metadata.source_uri` (it points at an external SoT via
/// Layer 6 Adapter and stands alone without edges; per onboarding §2 edges
/// are "optional but recommended") or `metadata.maintenance_exempt = true`
/// (the node is explicitly opted-out of session-cyclic maintenance, e.g.
/// `priorities` / `tick_log` / `journal` slots).
///
/// Without this guard, `wire_doctor` reports every wiring entry as orphan
/// (41/41 false-positive observed on a real dogfood session).
pub(crate) fn is_self_attached_wiring(node: &crate::domain::graph::Node) -> bool {
    use crate::application::wiring_mapper;
    if !node.metadata.is_object() {
        return false;
    }
    let has_source_uri = wiring_mapper::extract_source_uri(node)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let is_exempt = wiring_mapper::extract_maintenance_exempt(node);
    has_source_uri || is_exempt
}

/// Walk every node type and tally totals + orphan count. A node is counted as
/// an orphan only when it has no in- or out-edges **and** is not a
/// self-attached wiring entry (see `is_self_attached_wiring`). Shared scan
/// primitive for `wire_close` / `wire_doctor`; P3 daemon will extend this with
/// stale / asymmetric / high-fanout checks.
///
/// `workflow_def` Node は graph axis 検知対象集合に含まれない (issue
/// `f3bb100e` — Workflow Entity は trigger / action で動作完結、 edge を
/// 持たないのが正常) ため、 本集計でも除外する。 さもなくば `wire_close`
/// 経路でも workflow node を orphan として false-positive 算入していた。
pub fn graph_scan_summary(storage: &SqliteStorage) -> WireResult<GraphScanSummary> {
    use crate::application::workflow_mapper::WORKFLOW_TYPE;
    let mut total_nodes = 0_usize;
    let mut total_edges = 0_usize;
    let mut orphan = 0_usize;

    for t in storage.list_types_by_kind("node")? {
        if t == WORKFLOW_TYPE {
            continue;
        }
        for n in storage.list_nodes_by_type(&t)? {
            total_nodes += 1;
            let out_edges = storage.list_edges_from(&n.id)?;
            let in_edges = storage.list_edges_to(&n.id)?;
            total_edges += out_edges.len();
            if out_edges.is_empty() && in_edges.is_empty() && !is_self_attached_wiring(&n) {
                orphan += 1;
            }
        }
    }

    Ok(GraphScanSummary {
        orphan_node_count: orphan,
        total_node_count: total_nodes,
        total_edge_count: total_edges,
    })
}

// ---- wire_close ----

pub struct WireCloseInput {
    pub persona_id: String,
}

pub struct WireCloseOutput {
    pub persona_id: String,
    pub orphan_node_count: usize,
    pub total_node_count: usize,
    pub total_edge_count: usize,
    pub report_markdown: String,
}

/// Minimal lifecycle scan for the `/work-close` auto-call. P1 reports orphan
/// nodes (no in- or out-edges) and graph totals. P3 will expand this to
/// stale / asymmetric / high-fanout scan + Daily report emit.
pub fn wire_close(input: WireCloseInput, storage: &SqliteStorage) -> WireResult<WireCloseOutput> {
    let summary = graph_scan_summary(storage)?;
    let persona = &input.persona_id;
    let report_markdown = format!(
        "# wire_close report for `{persona}`\n\n\
         - total nodes: {total_nodes}\n\
         - total edges: {total_edges}\n\
         - orphan nodes (no edges, not self-attached): {orphan}\n",
        total_nodes = summary.total_node_count,
        total_edges = summary.total_edge_count,
        orphan = summary.orphan_node_count,
    );

    Ok(WireCloseOutput {
        persona_id: input.persona_id,
        orphan_node_count: summary.orphan_node_count,
        total_node_count: summary.total_node_count,
        total_edge_count: summary.total_edge_count,
        report_markdown,
    })
}

// ---- wire_doctor ----

/// Finding-driven 2-axis (graph / workflow) health diagnostic output.
pub struct WireDoctorOutput {
    pub report_markdown: String,
}

/// Finding-driven 2-axis (graph / workflow) health diagnostic (design §3-§8).
///
/// `persona_id = None` → Full mode (全 persona 横串)。
/// `persona_id = Some(id)` → Persona-scoped mode (当該 persona に紐づく
/// Finding のみ列挙、 main thread context を汚さない用)。
///
/// 内部は [`crate::application::doctor::run`] (Probe registry) に完全委譲、
/// Finding 列挙 + verdict 集約形式の Markdown を返す (design §5 / §8)。
/// `registry` は登録 adapter の scheme + filter capability 一覧を `## Adapters`
/// 節に反映するために使う (adapter-filter-if Phase 1)。
/// 数値カウントが必要なら [`graph_scan_summary`] を別途呼ぶ。
pub fn wire_doctor(
    storage: &SqliteStorage,
    persona_id: Option<String>,
    registry: &PluginRegistry,
) -> WireResult<WireDoctorOutput> {
    let report_markdown = crate::application::doctor::run(storage, persona_id, registry)?;
    Ok(WireDoctorOutput { report_markdown })
}

// ---- wire_query ----

#[derive(Debug)]
pub struct WireQueryInput {
    /// Either an inline `Specification` AST or a reference to a registered
    /// spec by name. Exactly one of the two must be Some (validated at
    /// the entry).
    pub spec: Option<Specification>,
    pub spec_ref: Option<String>,
    /// Maximum number of matched nodes to return. `None` = unlimited.
    pub limit: Option<usize>,
    /// Number of leading matched nodes to skip. `None` = 0.
    pub offset: Option<usize>,
}

#[derive(Debug)]
pub struct WireQueryNode {
    pub id: String,
    pub name: String,
    pub r#type: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug)]
pub struct WireQueryOutput {
    pub matched: Vec<WireQueryNode>,
    pub total_count: usize,
    pub returned_count: usize,
}

/// Ad-hoc query: evaluate `spec` (inline or by registered name) against the
/// whole graph and return matched nodes in a slim form (id + type + metadata
/// only). Field-level output filtering is a separate concern carried to a
/// future "output values filter" surface (mirrors mini-app's `output_fields`).
pub fn wire_query(input: WireQueryInput, storage: &SqliteStorage) -> WireResult<WireQueryOutput> {
    let resolved: Specification = match (input.spec, input.spec_ref.as_deref()) {
        (Some(s), None) => s,
        (None, Some(id_or_name)) => {
            // spec_ref accepts ULID id OR registered name.
            let name = match storage.resolve_specification_id_or_name(id_or_name)? {
                Some(id) => storage.get_specification_name_by_id(&id)?.ok_or_else(|| {
                    crate::domain::error::WireError::Domain(DomainError::NotFound(format!(
                        "spec: {id_or_name} (resolved id {id} has no row)"
                    )))
                })?,
                None => {
                    return Err(crate::domain::error::WireError::Domain(
                        DomainError::NotFound(format!("spec: {id_or_name}")),
                    ));
                }
            };
            SpecRegistry::new(storage).get(&name)?.ok_or_else(|| {
                crate::domain::error::WireError::Domain(DomainError::NotFound(format!(
                    "spec: {name}"
                )))
            })?
        }
        (Some(_), Some(_)) => {
            return Err(crate::domain::error::WireError::Domain(
                DomainError::InvalidSpec("spec and spec_ref are mutually exclusive".into()),
            ));
        }
        (None, None) => {
            return Err(crate::domain::error::WireError::Domain(
                DomainError::InvalidSpec("either spec or spec_ref is required".into()),
            ));
        }
    };

    let all = collect_matching_nodes(storage, &resolved)?;
    let total_count = all.len();
    let offset = input.offset.unwrap_or(0);
    let slice: Vec<Node> = match input.limit {
        Some(lim) => all.into_iter().skip(offset).take(lim).collect(),
        None => all.into_iter().skip(offset).collect(),
    };
    let returned_count = slice.len();
    let matched = slice
        .into_iter()
        .map(|n| WireQueryNode {
            id: n.id.to_string(),
            name: n.name,
            r#type: n.r#type,
            metadata: n.metadata,
        })
        .collect();

    Ok(WireQueryOutput {
        matched,
        total_count,
        returned_count,
    })
}

// ---- wire_render ----

#[derive(Debug)]
pub struct WireRenderInput {
    /// Name of a registered NamedProjection to evaluate + render.
    pub projection_ref: String,
}

#[derive(Debug)]
pub struct WireRenderOutput {
    pub name: String,
    pub target_form: TargetForm,
    pub rendered: String,
}

/// Render a single registered NamedProjection by name. Counterpart to
/// `wire_init` (which renders every projection at once): use `wire_render`
/// when you want exactly one rendered context, identified by name.
///
/// Ad-hoc inline rendering (spec + template + target_form passed inline,
/// without registration) is carried to a follow-up surface — see
/// `docs/wire-query-spec.md` §8 Future expansion.
pub fn wire_render(
    input: WireRenderInput,
    storage: &SqliteStorage,
    registry: &PluginRegistry,
) -> WireResult<WireRenderOutput> {
    // projection_ref accepts ULID id OR name (v0.7+ id_or_name resolver).
    let projection_name = match storage.resolve_projection_id_or_name(&input.projection_ref)? {
        Some(id) => storage.get_projection_name_by_id(&id)?.ok_or_else(|| {
            crate::domain::error::WireError::Domain(DomainError::NotFound(format!(
                "projection: {} (resolved id {} has no row)",
                input.projection_ref, id
            )))
        })?,
        None => {
            return Err(crate::domain::error::WireError::Domain(
                DomainError::NotFound(format!("projection: {}", input.projection_ref)),
            ));
        }
    };
    let proj = ProjectionRegistry::new(storage)
        .get(&projection_name)?
        .ok_or_else(|| {
            crate::domain::error::WireError::Domain(DomainError::NotFound(format!(
                "projection: {}",
                input.projection_ref
            )))
        })?;
    let spec = SpecRegistry::new(storage)
        .get(proj.spec_ref().as_str())?
        .ok_or_else(|| {
            crate::domain::error::WireError::Domain(DomainError::NotFound(format!(
                "spec_ref (dangling): {}",
                proj.spec_ref()
            )))
        })?;
    let matched = collect_matching_nodes(storage, &spec)?;
    let data = build_broadcast_render_data(&matched, None);
    let r = render_named_projection_sync(&proj, &data, registry)?;
    Ok(WireRenderOutput {
        name: r.name,
        target_form: r.target_form,
        rendered: r.rendered,
    })
}

// ---- wire_context_get (ContextWiring read-aggregate / persona 1-call) ----

/// Input for `wire_context_get`. Just the persona scope.
#[derive(Debug)]
pub struct WireContextGetInput {
    pub persona_id: String,
}

/// Application-layer summary of one `Wiring`. Carries only the fields a
/// caller (MCP / CLI / orchestrator) needs to make routing decisions —
/// the typed `Wiring` Domain Entity stays internal to the entity layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiringSummary {
    pub slot: String,
    pub source_uri: String,
    /// Registered NamedProjection for this slot, derived as
    /// `<persona>.section.<slot>` (see
    /// [`crate::application::projection_naming::workflow_emit_projection_name`]).
    /// `None` when no projection is registered for the slot yet.
    pub projection_ref: Option<String>,
    pub maintenance_exempt: bool,
}

/// 1-call read view of a `ContextWiring` (per-persona Aggregate boundary).
///
/// Returns the persona's `Wiring` set + `Workflow` set as application-layer
/// summary DTOs. This is the structured counterpart of
/// `wire_prompt_context` (which returns rendered string instead of raw
/// aggregate). Use it when an orchestrator needs the persona's complete
/// wiring topology in one call — e.g. to plan a reset, to inspect routing,
/// or as the pre-render snapshot consumed by future write-side use cases.
#[derive(Debug)]
pub struct WireContextGetOutput {
    pub persona_id: String,
    pub wirings: Vec<WiringSummary>,
    pub workflows: Vec<WorkflowSummary>,
}

/// Walk one persona's consistency boundary and return a structured
/// snapshot. Wiring nodes whose metadata cannot be parsed (drift) are
/// skipped silently — the doctor probes are the surface that flag them.
///
/// Layering: the `ContextWiring` Aggregate Root stays an identity marker
/// in `domain::entity`; this use case owns the Repository traversal so the
/// domain layer keeps no dependency on `application` / `infrastructure`.
pub fn wire_context_get(
    input: WireContextGetInput,
    storage: &SqliteStorage,
) -> WireResult<WireContextGetOutput> {
    use crate::application::wiring_mapper;
    use crate::application::workflow_mapper::WORKFLOW_TYPE;
    use crate::domain::entity::context_wiring::ContextWiring;
    use crate::domain::entity::persona_id::PersonaId;

    let persona = PersonaId::new(input.persona_id.clone())?;
    let context = ContextWiring::new(persona.clone());

    let wirings = list_persona_wirings(&context, storage)?;
    let workflows = list_persona_workflow_summaries(&context, storage)?;

    // Sort by slot / id for stable output (callers compare snapshots).
    let mut wirings = wirings;
    wirings.sort_by(|a, b| a.slot.cmp(&b.slot));
    let mut workflows = workflows;
    workflows.sort_by(|a, b| a.id.cmp(&b.id));

    // Touch the constants once so the wiring spec helper keeps the
    // workflow_def literal aligned with the mapper SoT.
    let _ = (wiring_mapper::WIRING_TYPE, WORKFLOW_TYPE);

    Ok(WireContextGetOutput {
        persona_id: context.persona_id().as_str().to_owned(),
        wirings,
        workflows,
    })
}

/// Persona-scoped wiring summaries. Translates wiring nodes via the
/// `wiring_mapper` and resolves `projection_ref` against the registered
/// `<persona>.section.<slot>` convention.
fn list_persona_wirings(
    context: &crate::domain::entity::context_wiring::ContextWiring,
    storage: &SqliteStorage,
) -> WireResult<Vec<WiringSummary>> {
    use crate::application::projection_naming::workflow_emit_projection_name;
    use crate::application::wiring_mapper::{self, WIRING_TYPE};
    use crate::domain::specification::Specification;

    let spec = Specification::And(vec![
        Specification::TypeIs(WIRING_TYPE.to_string()),
        Specification::MetadataEq {
            path: wiring_mapper::META_PERSONA.to_string(),
            value: serde_json::Value::String(context.persona_id().as_str().to_owned()),
        },
    ]);
    let nodes = collect_matching_nodes(storage, &spec)?;
    let registry = ProjectionRegistry::new(storage);

    let mut out = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let Some(slot) = wiring_mapper::extract_slot(node) else {
            continue;
        };
        let Some(source_uri) = wiring_mapper::extract_source_uri(node) else {
            continue;
        };
        // Explicit `metadata.projection_ref` is reported verbatim (it is what
        // the render path will use); otherwise the convention-derived name is
        // reported only when actually registered.
        let projection_ref = match wiring_mapper::extract_projection_ref(node) {
            Some(explicit) => Some(explicit.to_owned()),
            None => {
                let derived = workflow_emit_projection_name(context.persona_id().as_str(), slot);
                if registry.get(&derived)?.is_some() {
                    Some(derived)
                } else {
                    None
                }
            }
        };
        out.push(WiringSummary {
            slot: slot.to_owned(),
            source_uri: source_uri.to_owned(),
            projection_ref,
            maintenance_exempt: wiring_mapper::extract_maintenance_exempt(node),
        });
    }
    Ok(out)
}

/// Persona-scoped workflow summaries. Reuses the tolerant `node_to_summary`
/// path so doctor-surfaced drift rows still appear in the snapshot.
fn list_persona_workflow_summaries(
    context: &crate::domain::entity::context_wiring::ContextWiring,
    storage: &SqliteStorage,
) -> WireResult<Vec<WorkflowSummary>> {
    use crate::application::workflow_mapper::{self, WORKFLOW_TYPE};
    use crate::domain::specification::Specification;

    let spec = Specification::And(vec![
        Specification::TypeIs(WORKFLOW_TYPE.to_string()),
        Specification::MetadataEq {
            path: workflow_mapper::META_PERSONA.to_string(),
            value: serde_json::Value::String(context.persona_id().as_str().to_owned()),
        },
    ]);
    let nodes = collect_matching_nodes(storage, &spec)?;
    let summaries = nodes
        .into_iter()
        .filter_map(|n| node_to_summary(n).ok())
        .collect();
    Ok(summaries)
}

// ---- wire_node_update (P3a Phase 2 (d) — wiring-entry metadata tuning) ----

/// Merge strategy for `wire_node_update`. Mirrors RFC 7396 shallow merge for
/// `Merge` and a full replacement for `Replace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireNodeUpdateMode {
    /// Shallow merge: top-level keys in `metadata_patch` overwrite the
    /// corresponding keys on the existing node metadata; keys absent from the
    /// patch are preserved. `null` values in the patch DELETE the matching key
    /// (RFC 7396 §1).
    Merge,
    /// Full replacement: the existing metadata is discarded and the patch
    /// becomes the new metadata.
    Replace,
}

impl WireNodeUpdateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WireNodeUpdateMode::Merge => "merge",
            WireNodeUpdateMode::Replace => "replace",
        }
    }

    pub fn parse(s: &str) -> WireResult<Self> {
        match s {
            "merge" => Ok(WireNodeUpdateMode::Merge),
            "replace" => Ok(WireNodeUpdateMode::Replace),
            other => Err(WireError::Other(format!(
                "unknown wire_node_update mode '{other}' — expected 'merge' or 'replace'"
            ))),
        }
    }
}

#[derive(Debug)]
pub struct WireNodeUpdateInput {
    pub id: String,
    /// Object whose top-level keys are applied to the existing node metadata
    /// per `mode`. Non-object values are rejected.
    pub metadata_patch: serde_json::Value,
    pub mode: WireNodeUpdateMode,
}

#[derive(Debug)]
pub struct WireNodeUpdateOutput {
    pub id: String,
    pub mode: WireNodeUpdateMode,
    /// Final metadata after the update (= what is now persisted on the node).
    pub metadata: serde_json::Value,
}

/// Update a node's `metadata` in place.
///
/// `mode = Merge`: shallow top-level merge over the existing metadata
/// (RFC 7396); `null` values in the patch delete the matching key.
/// `mode = Replace`: full replacement of the node metadata with `metadata_patch`.
///
/// Other node fields (`type` / `sot_ref` / lifecycle timestamps) are NOT
/// touched on this path — the UC backing this surface is wiring-entry tuning
/// (`source_uri` / `axis` / `maintained_by`), which is metadata-only. To
/// change the node type or lifecycle fields, delete + re-create.
///
/// Errors:
/// - `metadata_patch` is not a JSON object
/// - `id` does not match any existing node row (returns `WireError::NotFound`)
pub fn wire_node_update(
    input: WireNodeUpdateInput,
    storage: &SqliteStorage,
) -> WireResult<WireNodeUpdateOutput> {
    if !input.metadata_patch.is_object() {
        return Err(WireError::Other(format!(
            "wire_node_update: metadata_patch must be a JSON object, got {}",
            type_name_of(&input.metadata_patch)
        )));
    }
    let resolved = storage
        .resolve_node_id_or_name(&input.id)?
        .ok_or_else(|| WireError::Domain(DomainError::NotFound(format!("node: {}", input.id))))?;
    let Some(existing) = storage.get_node(&resolved)? else {
        return Err(WireError::Domain(DomainError::NotFound(format!(
            "node: {}",
            input.id
        ))));
    };

    let final_metadata = match input.mode {
        WireNodeUpdateMode::Replace => input.metadata_patch.clone(),
        WireNodeUpdateMode::Merge => {
            let mut base = match existing.metadata {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            if let serde_json::Value::Object(patch_obj) = &input.metadata_patch {
                for (k, v) in patch_obj {
                    if v.is_null() {
                        base.remove(k);
                    } else {
                        base.insert(k.clone(), v.clone());
                    }
                }
            }
            serde_json::Value::Object(base)
        }
    };

    let updated = storage.update_node_metadata(&resolved, &final_metadata)?;
    if !updated {
        // Defensive: get_node saw a row but UPDATE matched 0 — should not
        // happen under single-writer SQLite, but surface explicitly if it does.
        return Err(WireError::Storage(format!(
            "wire_node_update: row '{}' vanished between read and write",
            input.id
        )));
    }
    Ok(WireNodeUpdateOutput {
        id: input.id,
        mode: input.mode,
        metadata: final_metadata,
    })
}

fn type_name_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ---- delete surface (P2c-bis、 メンテ運用必須) ----

#[derive(Debug)]
pub struct WireDeleteInput {
    /// Node id / Edge id / Spec name / Projection name (kind に応じた identifier)
    pub id_or_name: String,
}

#[derive(Debug)]
pub struct WireDeleteOutput {
    pub kind: &'static str,
    pub id_or_name: String,
    pub deleted: bool,
}

/// Delete a node by id. Edges referencing the node (as src or tgt) are
/// **cascade-deleted in the same storage transaction** — edges table FK is
/// NOT-NULL (`REFERENCES nodes(id)`) so dangling state is not representable
/// in normal operation. The `graph.dangling_edge` Probe is retained as a
/// defensive sensor against external DB drift / migration corruption /
/// direct SQL writes that bypass this transaction.
pub fn wire_node_delete(
    input: WireDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireDeleteOutput> {
    let deleted = match storage.resolve_node_id_or_name(&input.id_or_name)? {
        None => false,
        Some(id) => storage.delete_node(&id)?,
    };
    Ok(WireDeleteOutput {
        kind: "node",
        id_or_name: input.id_or_name,
        deleted,
    })
}

/// Delete an edge by id.
pub fn wire_edge_delete(
    input: WireDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireDeleteOutput> {
    let deleted = match storage.resolve_edge_id_or_name(&input.id_or_name)? {
        None => false,
        Some(id) => storage.delete_edge(&id)?,
    };
    Ok(WireDeleteOutput {
        kind: "edge",
        id_or_name: input.id_or_name,
        deleted,
    })
}

/// Delete a Specification by ULID id or name. Projections referencing it via
/// spec_ref will start returning dangling-spec errors at render time
/// (existing wire_render contract).
pub fn wire_spec_delete(
    input: WireDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireDeleteOutput> {
    let deleted = match storage.resolve_specification_id_or_name(&input.id_or_name)? {
        Some(id) => storage.delete_specification(&id)?,
        None => false,
    };
    Ok(WireDeleteOutput {
        kind: "spec",
        id_or_name: input.id_or_name,
        deleted,
    })
}

/// Delete a NamedProjection by ULID id or name.
pub fn wire_projection_delete(
    input: WireDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireDeleteOutput> {
    let deleted = match storage.resolve_projection_id_or_name(&input.id_or_name)? {
        Some(id) => storage.delete_projection(&id)?,
        None => false,
    };
    Ok(WireDeleteOutput {
        kind: "projection",
        id_or_name: input.id_or_name,
        deleted,
    })
}

// ---- wire_nodes_create_batch ----

pub struct WireNodesCreateBatchInput {
    pub nodes: Vec<Node>,
}

pub struct WireBatchOutput {
    pub inserted_count: usize,
    /// 0-based index of the first item that failed; `None` if all succeeded.
    pub failed_at: Option<usize>,
    pub error_message: Option<String>,
}

/// Insert a batch of nodes by iterating `insert_node` 1 row at a time. Stops
/// on the first failure (non-atomic), reports counts so the caller can
/// decide whether to retry / patch / rollback. P2c scope: minimal bulk
/// surface; atomic SQLite Tx wrap is carried until usage observation.
pub fn wire_nodes_create_batch(
    input: WireNodesCreateBatchInput,
    storage: &SqliteStorage,
) -> WireResult<WireBatchOutput> {
    for (i, n) in input.nodes.iter().enumerate() {
        if let Err(e) = storage.insert_node(n) {
            return Ok(WireBatchOutput {
                inserted_count: i,
                failed_at: Some(i),
                error_message: Some(e.to_string()),
            });
        }
    }
    Ok(WireBatchOutput {
        inserted_count: input.nodes.len(),
        failed_at: None,
        error_message: None,
    })
}

// ---- wire_edges_create_batch ----

pub struct WireEdgesCreateBatchInput {
    pub edges: Vec<crate::domain::graph::Edge>,
}

/// Insert a batch of edges by iterating `insert_edge` 1 row at a time. Same
/// non-atomic semantics as `wire_nodes_create_batch`.
pub fn wire_edges_create_batch(
    input: WireEdgesCreateBatchInput,
    storage: &SqliteStorage,
) -> WireResult<WireBatchOutput> {
    for (i, e) in input.edges.iter().enumerate() {
        if let Err(err) = storage.insert_edge(e) {
            return Ok(WireBatchOutput {
                inserted_count: i,
                failed_at: Some(i),
                error_message: Some(err.to_string()),
            });
        }
    }
    Ok(WireBatchOutput {
        inserted_count: input.edges.len(),
        failed_at: None,
        error_message: None,
    })
}

// ---- wire_workflow_* (P5-a seed) ---------------------------------------
//
// `docs/wire-workflow-spec.md` の declarative WorkflowEngine seed。 Workflow を
// 既存 Node type `workflow_def` に metadata で trigger + action を埋める form で
// 表現する (新 store / 新 type 追加なし)。
//
// 本 P5-a scope:
//   - register / list / delete + fire の resolution (= どの workflow が hit し
//     て、 どんな action を取るか の descriptor 返却)
//   - trigger: on_demand / on_event の 2 kind
//   - action: no_op / emit_projection の 2 kind (validate のみ、 emit_projection
//     の実 invocation は呼び出し側 = MCP layer が wire_prompt_context を叩く)
//
// carry (P5-b 以降):
//   - cron / metadata_changed trigger (daemon 前提)
//   - set_metadata / fire_mailbox action
//   - wire_update (cross-ref 自動維持)

use crate::application::workflow_mapper::{
    node_to_workflow, parse_action, parse_trigger, workflow_to_node, WORKFLOW_TYPE,
};
use crate::domain::entity::workflow::{Action, Trigger, Workflow, WorkflowId};
use crate::domain::entity::PersonaId;

#[derive(Debug)]
pub struct WireWorkflowRegisterInput {
    pub id: String,
    pub persona_id: Option<String>,
    pub trigger: serde_json::Value,
    pub action: serde_json::Value,
    pub enabled: Option<bool>,
}

#[derive(Debug)]
pub struct WireWorkflowRegisterOutput {
    pub id: String,
}

/// Register a Workflow as a `workflow_def` Node. Routes through the
/// [`Workflow`] Domain Entity for all invariant checks (trigger / action
/// shape, P5-a kind subset) and through [`workflow_mapper`] for the
/// Entity ↔ Node mapping; this function is now a thin orchestrator over
/// the mapper boundary so observability via `wire_query({TypeIs:
/// "workflow_def"})` continues to work out of the box.
///
/// design §7.3 Phase 5 — `register / fire / delete` lifecycle invariants
/// are owned by the Entity (`Workflow::new` + `Trigger::on_event` +
/// `Action::emit_projection` constructors); use cases are pure orchestration.
pub fn wire_workflow_register(
    input: WireWorkflowRegisterInput,
    storage: &SqliteStorage,
) -> WireResult<WireWorkflowRegisterOutput> {
    let workflow = build_workflow_from_register_input(input)?;
    let node = workflow_to_node(&workflow);
    storage.insert_node(&node)?;
    Ok(WireWorkflowRegisterOutput {
        id: workflow.id().as_str().to_owned(),
    })
}

/// Construct a [`Workflow`] from the raw register input JSON, applying all
/// VO invariants at the Entity boundary. Surfaces structured
/// `DomainError::InvalidSpec` (via the mapper / Entity constructors) on
/// invalid trigger / action shape.
fn build_workflow_from_register_input(input: WireWorkflowRegisterInput) -> WireResult<Workflow> {
    let id = WorkflowId::new(input.id)?;
    let persona_id = match input.persona_id {
        Some(p) => Some(PersonaId::new(p)?),
        None => None,
    };
    let trigger = parse_trigger(&input.trigger)?;
    let action = parse_action(&input.action)?;
    Ok(Workflow::new(
        id,
        persona_id,
        trigger,
        action,
        input.enabled.unwrap_or(true),
    ))
}

#[derive(Debug)]
pub struct WireWorkflowListInput {
    pub persona_id: Option<String>,
    pub trigger_kind: Option<String>,
    pub enabled_only: Option<bool>,
}

#[derive(Debug)]
pub struct WorkflowSummary {
    pub id: String,
    pub persona_id: Option<String>,
    pub trigger: serde_json::Value,
    pub action: serde_json::Value,
    pub enabled: bool,
}

#[derive(Debug)]
pub struct WireWorkflowListOutput {
    pub workflows: Vec<WorkflowSummary>,
}

/// List registered Workflows (= Nodes of type `workflow_def`), with optional
/// `persona_id` / `trigger.kind` / enabled filtering applied in-memory.
pub fn wire_workflow_list(
    input: WireWorkflowListInput,
    storage: &SqliteStorage,
) -> WireResult<WireWorkflowListOutput> {
    let spec = Specification::TypeIs(WORKFLOW_TYPE.to_string());
    let nodes = collect_matching_nodes(storage, &spec)?;
    let enabled_only = input.enabled_only.unwrap_or(true);
    let workflows = nodes
        .into_iter()
        .filter_map(|n| node_to_summary(n).ok())
        .filter(|w| {
            if enabled_only && !w.enabled {
                return false;
            }
            if let Some(p) = input.persona_id.as_ref() {
                if w.persona_id.as_deref() != Some(p.as_str()) {
                    return false;
                }
            }
            if let Some(tk) = input.trigger_kind.as_ref() {
                if w.trigger.get("kind").and_then(|v| v.as_str()) != Some(tk.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    Ok(WireWorkflowListOutput { workflows })
}

/// Translate a persisted `workflow_def` Node into a [`WorkflowSummary`].
///
/// **Tolerant listing path**: `wire_workflow_list` is consumed by
/// `wire_doctor` probes that explicitly surface drift (= persisted
/// workflows whose trigger / action shape doesn't match the current
/// P5-a Entity invariants — e.g. a `cron` trigger kind injected by
/// future tooling or test scenarios). Routing this through the strict
/// `node_to_workflow` mapper would silently filter such rows out, which
/// is exactly what the doctor probes need to see. So this stays on raw
/// JSON extraction; only `wire_workflow_register` (write path) and
/// `wire_workflow_fire` (typed gating) go through the Entity mapper.
fn node_to_summary(node: Node) -> WireResult<WorkflowSummary> {
    use crate::application::workflow_mapper;
    let persona_id = workflow_mapper::extract_persona(&node).map(str::to_owned);
    let trigger = workflow_mapper::extract_trigger_value(&node);
    let action = workflow_mapper::extract_action_value(&node);
    let enabled = workflow_mapper::extract_enabled(&node);
    Ok(WorkflowSummary {
        id: node.name,
        persona_id,
        trigger,
        action,
        enabled,
    })
}

#[derive(Debug)]
pub struct WireWorkflowFireInput {
    /// Single-workflow fire by id (mutually exclusive with `event`).
    pub id: Option<String>,
    /// Event-name fan-out (matches every `on_event` workflow whose
    /// `trigger.event` equals this value).
    pub event: Option<String>,
    /// Optional scoping for event fan-out (matches metadata.persona).
    pub persona_id: Option<String>,
    pub dry_run: Option<bool>,
}

/// A workflow resolved for firing, with its action descriptor surfaced so the
/// caller (= MCP layer or external orchestrator) can dispatch the side
/// effect. P5-a keeps action invocation out of core to avoid the
/// async/Arc<Mutex> coupling — `emit_projection` is dispatched by calling
/// `wire_prompt_context` from the caller using `action_emit_projection_names`.
#[derive(Debug)]
pub struct ResolvedFire {
    pub id: String,
    pub persona_id: Option<String>,
    pub action_kind: String,
    /// Populated when `action_kind == "emit_projection"`; else None.
    pub action_emit_projection_names: Option<Vec<String>>,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct WireWorkflowFireOutput {
    pub fired: Vec<ResolvedFire>,
    pub skipped: Vec<(String, String)>, // (id, reason)
}

/// Resolve the workflows that would fire for the given input. **Does not**
/// invoke the action itself in P5-a; the returned `ResolvedFire` describes
/// what should happen so the caller can dispatch (= keeps core sync, keeps
/// emit_projection's async machinery at the MCP layer).
pub fn wire_workflow_fire(
    input: WireWorkflowFireInput,
    storage: &SqliteStorage,
) -> WireResult<WireWorkflowFireOutput> {
    if input.id.is_some() == input.event.is_some() {
        return Err(crate::domain::error::WireError::Domain(
            DomainError::InvalidSpec("exactly one of `id` or `event` is required".to_string()),
        ));
    }
    let dry_run = input.dry_run.unwrap_or(false);

    // Collect candidate workflows as Domain Entities. Gating below dispatches
    // on the typed `Trigger` / `Action` sum types instead of JSON probing.
    let candidates: Vec<Workflow> = if let Some(id) = input.id.as_ref() {
        let resolved = storage.resolve_node_id_or_name(id)?;
        let Some(node_id) = resolved else {
            return Ok(WireWorkflowFireOutput {
                fired: vec![],
                skipped: vec![(id.clone(), "workflow not found".to_string())],
            });
        };
        let Some(node) = storage.get_node(&node_id)? else {
            return Ok(WireWorkflowFireOutput {
                fired: vec![],
                skipped: vec![(id.clone(), "workflow not found".to_string())],
            });
        };
        if node.r#type != WORKFLOW_TYPE {
            return Ok(WireWorkflowFireOutput {
                fired: vec![],
                skipped: vec![(
                    id.clone(),
                    format!("node type is '{}', expected '{WORKFLOW_TYPE}'", node.r#type),
                )],
            });
        }
        vec![node_to_workflow(&node)?]
    } else {
        // event-driven: match every on_event workflow whose trigger.event == event
        let spec = Specification::TypeIs(WORKFLOW_TYPE.to_string());
        collect_matching_nodes(storage, &spec)?
            .iter()
            .filter_map(|n| node_to_workflow(n).ok())
            .collect()
    };

    let mut fired = Vec::new();
    let mut skipped = Vec::new();
    let event = input.event.as_deref();

    for w in candidates {
        let id_str = w.id().as_str().to_owned();
        if !w.enabled() {
            skipped.push((id_str, "enabled=false".to_string()));
            continue;
        }
        if let Some(persona_filter) = input.persona_id.as_ref() {
            if w.persona_id().map(|p| p.as_str()) != Some(persona_filter.as_str()) {
                skipped.push((
                    id_str,
                    format!("persona scope mismatch (want={persona_filter})"),
                ));
                continue;
            }
        }
        // Trigger gating (typed)
        if let Some(ev) = event {
            match w.trigger() {
                Trigger::OnEvent { event: wf_event } => {
                    if wf_event != ev {
                        skipped.push((id_str, format!("trigger.event='{wf_event}' != '{ev}'")));
                        continue;
                    }
                }
                Trigger::OnDemand => {
                    skipped.push((
                        id_str,
                        "trigger.kind='on_demand' does not match event fan-out".to_string(),
                    ));
                    continue;
                }
            }
        }
        // Resolve action (typed)
        let (action_kind, action_emit_projection_names) = match w.action() {
            Action::NoOp => ("no_op".to_string(), None),
            Action::EmitProjection { slots } => (
                "emit_projection".to_string(),
                Some(slots.iter().map(|s| s.as_str().to_owned()).collect()),
            ),
        };
        fired.push(ResolvedFire {
            id: w.id().as_str().to_owned(),
            persona_id: w.persona_id().map(|p| p.as_str().to_owned()),
            action_kind,
            action_emit_projection_names,
            dry_run,
        });
    }

    Ok(WireWorkflowFireOutput { fired, skipped })
}

// wire_workflow_check (P5-a') 関数 + 関連 struct は削除 (2026-06-20、 issue 7069dede)。
// 同等の coverage audit は Probe registry の Workflow Probes 経由で wire_doctor から行う
// (declared_covered / declared_uncovered / undeclared / exempt の 4 bucket は
// Workflow Probes の Finding emit に置換)。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::entity::projection::{PluginDispatch, Projection};
    use crate::domain::graph::{ulid_from_seed, Edge, Node};
    use serde_json::json;

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
    }

    fn default_registry() -> PluginRegistry {
        PluginRegistry::default_for_wire().unwrap()
    }

    fn bare_node(id: &str, type_: &str) -> Node {
        Node {
            id: ulid_from_seed(id),
            name: id.into(),
            r#type: type_.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: json!({}),
        }
    }

    #[test]
    fn wire_init_with_no_projections_yields_empty() {
        let s = setup();
        let out = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
            &default_registry(),
        )
        .unwrap();
        assert_eq!(out.persona_id, "alpha");
        assert!(out.projections.is_empty());
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn wire_init_renders_registered_projection() {
        let s = setup();
        // Insert 2 personas
        s.insert_node(&bare_node("alpha", "persona")).unwrap();
        s.insert_node(&bare_node("beta", "persona")).unwrap();
        // Register Specification
        SpecRegistry::new(&s)
            .register("active_personas", &Specification::TypeIs("persona".into()))
            .unwrap();
        // Register Projection
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "_persona_toc",
                    "active_personas",
                    "Personas ({{count}}): {{names}}",
                    TargetForm::Prompt,
                    PluginDispatch::Default,
                )
                .unwrap(),
            )
            .unwrap();

        let out = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
            &default_registry(),
        )
        .unwrap();
        assert_eq!(out.projections.len(), 1);
        let p = &out.projections[0];
        assert_eq!(p.name, "_persona_toc");
        assert_eq!(p.target_form, TargetForm::Prompt);
        assert!(p.rendered.contains("Personas (2):"));
        assert!(p.rendered.contains("beta"));
        assert!(p.rendered.contains("alpha"));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn wire_init_warns_on_unknown_spec_ref() {
        let s = setup();
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "broken",
                    "no_such_spec",
                    "x",
                    TargetForm::Prompt,
                    PluginDispatch::Default,
                )
                .unwrap(),
            )
            .unwrap();
        let out = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
            &default_registry(),
        )
        .unwrap();
        assert!(out.projections.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("no_such_spec"));
    }

    #[test]
    fn wire_close_reports_orphans_and_totals() {
        let s = setup();
        // 3 personas, 1 directional edge: a -> b. c is orphan.
        for id in ["a", "b", "c"] {
            s.insert_node(&bare_node(id, "persona")).unwrap();
        }
        s.insert_edge(&Edge {
            id: ulid_from_seed("e1"),
            name: Some("e1".into()),
            src_node: ulid_from_seed("a"),
            tgt_node: ulid_from_seed("b"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();

        let out = wire_close(
            WireCloseInput {
                persona_id: "alpha".into(),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.total_node_count, 3);
        assert_eq!(out.total_edge_count, 1);
        assert_eq!(out.orphan_node_count, 1);
        assert!(out
            .report_markdown
            .contains("orphan nodes (no edges, not self-attached): 1"));
        assert!(out.report_markdown.contains("total nodes: 3"));
    }

    #[test]
    fn graph_scan_excludes_self_attached_wiring_from_orphans() {
        // issue 15a46ce6 regression: wiring entries that hold metadata.source_uri
        // or metadata.maintenance_exempt=true must NOT be reported as orphans,
        // even when they carry no edges (onboarding §2 — edges are optional).
        let s = setup();

        // wiring entry with source_uri — should NOT count as orphan
        use crate::application::wiring_mapper;
        use crate::domain::entity::{PersonaId, Slot, Source};
        let mut n1 = bare_node("p.mailbox", wiring_mapper::WIRING_TYPE);
        n1.metadata = wiring_mapper::wiring_metadata_object(
            &PersonaId::new("p").unwrap(),
            &Slot::new("mailbox").unwrap(),
            &Source::new("mini-app://mailbox?alias=for_p").unwrap(),
            None,
        );
        s.insert_node(&n1).unwrap();

        // wiring entry with maintenance_exempt=true — should NOT count as orphan.
        // mapper has no first-class Source for the maintenance-only sketch, so
        // construct the metadata Map directly via the mapper key constants and
        // pass it as `extras` against a placeholder Source.
        let mut n2 = bare_node("p.priorities", wiring_mapper::WIRING_TYPE);
        let mut extras = serde_json::Map::new();
        extras.insert(wiring_mapper::META_MAINTENANCE_EXEMPT.into(), json!(true));
        // build metadata without a real source_uri; remove the placeholder
        // afterwards so the legacy sketch (source_uri absent + maintenance
        // exempt) survives the round-trip.
        let mut metadata = wiring_mapper::wiring_metadata_object(
            &PersonaId::new("p").unwrap(),
            &Slot::new("priorities").unwrap(),
            &Source::new("placeholder://x").unwrap(),
            Some(extras),
        );
        metadata
            .as_object_mut()
            .unwrap()
            .remove(wiring_mapper::META_SOURCE_URI);
        n2.metadata = metadata;
        s.insert_node(&n2).unwrap();

        // bare persona node with no metadata + no edges — SHOULD count as orphan
        s.insert_node(&bare_node("p", "persona")).unwrap();

        let out = wire_doctor(&s, None, &default_registry()).unwrap();
        let summary = graph_scan_summary(&s).unwrap();
        assert_eq!(summary.total_node_count, 3);
        assert_eq!(summary.total_edge_count, 0);
        assert_eq!(
            summary.orphan_node_count, 1,
            "only the bare persona node is orphan; the 2 wiring entries are self-attached"
        );
        // Finding-driven format (design §8): orphan Probe land 後に再導入。
        assert!(out.report_markdown.contains("scope: full"));
    }

    // ---- wire_doctor 2-axis regression tests ----

    #[test]
    fn wire_doctor_returns_2axis_integrated_report() {
        let storage = setup();
        let out = wire_doctor(&storage, None, &default_registry())
            .expect("wire_doctor should pass on empty setup");
        // Finding-driven format (design §8): scope + verdict + axis sections。
        assert!(
            out.report_markdown.contains("## Graph axis"),
            "report_markdown should contain '## Graph axis' header"
        );
        assert!(
            out.report_markdown.contains("## Workflow axis"),
            "report_markdown should contain '## Workflow axis' header"
        );
        assert!(out.report_markdown.contains("scope: full"));
        // empty setup → GraphEdgesZero probe fires (error) → BROKEN。
        assert!(out.report_markdown.contains("verdict: BROKEN"));
        assert!(out.report_markdown.contains("graph.edges_zero"));
    }

    #[test]
    fn wire_doctor_report_includes_adapters_section() {
        // adapter-filter-if Phase 1: wire_doctor renders a `## Adapters`
        // section describing registered adapter scheme + filter_caps.
        let storage = setup();
        let out = wire_doctor(&storage, None, &default_registry()).unwrap();
        assert!(
            out.report_markdown.contains("## Adapters"),
            "report_markdown should contain '## Adapters' header; got: {}",
            out.report_markdown
        );
        assert!(
            out.report_markdown
                .contains("- file: lines, tail(n_max=1000)"),
            "report_markdown should list the bundled FileAdapter's filter caps; got: {}",
            out.report_markdown
        );
    }

    #[test]
    fn wire_close_empty_graph_zero_everything() {
        let s = setup();
        let out = wire_close(
            WireCloseInput {
                persona_id: "alpha".into(),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.total_node_count, 0);
        assert_eq!(out.total_edge_count, 0);
        assert_eq!(out.orphan_node_count, 0);
    }

    // ---- delete surface tests ----

    #[test]
    fn wire_node_delete_returns_true_when_row_exists() {
        let s = setup();
        s.insert_node(&bare_node("a", "persona")).unwrap();
        let out = wire_node_delete(
            WireDeleteInput {
                id_or_name: "a".into(),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.kind, "node");
        assert_eq!(out.id_or_name, "a");
        assert!(out.deleted);
        // 二重削除 → false
        let out2 = wire_node_delete(
            WireDeleteInput {
                id_or_name: "a".into(),
            },
            &s,
        )
        .unwrap();
        assert!(!out2.deleted);
    }

    #[test]
    fn wire_node_delete_returns_false_when_row_missing() {
        let s = setup();
        let out = wire_node_delete(
            WireDeleteInput {
                id_or_name: "ghost".into(),
            },
            &s,
        )
        .unwrap();
        assert!(!out.deleted);
    }

    #[test]
    fn wire_edge_delete_returns_true_when_row_exists() {
        let s = setup();
        s.insert_node(&bare_node("a", "persona")).unwrap();
        s.insert_node(&bare_node("b", "persona")).unwrap();
        s.insert_edge(&Edge {
            id: ulid_from_seed("e1"),
            name: Some("e1".into()),
            src_node: ulid_from_seed("a"),
            tgt_node: ulid_from_seed("b"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        let out = wire_edge_delete(
            WireDeleteInput {
                id_or_name: "e1".into(),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.kind, "edge");
        assert!(out.deleted);
    }

    #[test]
    fn wire_spec_delete_returns_true_when_row_exists() {
        let s = setup();
        SpecRegistry::new(&s)
            .register("active_personas", &Specification::TypeIs("persona".into()))
            .unwrap();
        let out = wire_spec_delete(
            WireDeleteInput {
                id_or_name: "active_personas".into(),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.kind, "spec");
        assert!(out.deleted);
    }

    #[test]
    fn wire_projection_delete_returns_true_when_row_exists() {
        let s = setup();
        SpecRegistry::new(&s)
            .register("p", &Specification::TypeIs("persona".into()))
            .unwrap();
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "doomed",
                    "p",
                    "x",
                    TargetForm::Prompt,
                    PluginDispatch::Default,
                )
                .unwrap(),
            )
            .unwrap();
        let out = wire_projection_delete(
            WireDeleteInput {
                id_or_name: "doomed".into(),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.kind, "projection");
        assert!(out.deleted);
        // 削除済 projection は wire_init / wire_render の list() から消える
        assert!(ProjectionRegistry::new(&s).list().unwrap().is_empty());
    }

    // ---- wire_workflow_* (P5-a) tests ----

    #[test]
    fn workflow_register_round_trips_via_list() {
        let s = setup();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "alpha.workflow.review_close".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_event","event":"session_close"}),
                action: json!({"kind":"emit_projection","projection_names":["review_pending"]}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        let out = wire_workflow_list(
            WireWorkflowListInput {
                persona_id: Some("alpha".into()),
                trigger_kind: None,
                enabled_only: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.workflows.len(), 1);
        let w = &out.workflows[0];
        assert_eq!(w.id, "alpha.workflow.review_close");
        assert_eq!(w.persona_id.as_deref(), Some("alpha"));
        assert!(w.enabled);
        assert_eq!(w.trigger["kind"], "on_event");
        assert_eq!(w.action["kind"], "emit_projection");
    }

    #[test]
    fn workflow_register_rejects_unsupported_trigger_kind() {
        let s = setup();
        let err = wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "x".into(),
                persona_id: None,
                trigger: json!({"kind":"cron","cron_spec":"0 9 * * *"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap_err();
        assert!(err.to_string().contains("cron"));
    }

    #[test]
    fn workflow_register_rejects_on_event_without_event_field() {
        let s = setup();
        let err = wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "x".into(),
                persona_id: None,
                trigger: json!({"kind":"on_event"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap_err();
        assert!(err.to_string().contains("event"));
    }

    #[test]
    fn workflow_register_rejects_emit_projection_without_names() {
        let s = setup();
        let err = wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "x".into(),
                persona_id: None,
                trigger: json!({"kind":"on_demand"}),
                action: json!({"kind":"emit_projection"}),
                enabled: None,
            },
            &s,
        )
        .unwrap_err();
        assert!(err.to_string().contains("projection_names"));
    }

    #[test]
    fn workflow_list_filters_by_trigger_kind_and_enabled() {
        let s = setup();
        for (id, kind, enabled) in [
            ("w1", "on_demand", true),
            ("w2", "on_event", true),
            ("w3", "on_demand", false),
        ] {
            let trig = if kind == "on_event" {
                json!({"kind":"on_event","event":"e"})
            } else {
                json!({"kind":"on_demand"})
            };
            wire_workflow_register(
                WireWorkflowRegisterInput {
                    id: id.into(),
                    persona_id: None,
                    trigger: trig,
                    action: json!({"kind":"no_op"}),
                    enabled: Some(enabled),
                },
                &s,
            )
            .unwrap();
        }
        // default: enabled_only = true
        let out = wire_workflow_list(
            WireWorkflowListInput {
                persona_id: None,
                trigger_kind: Some("on_demand".into()),
                enabled_only: None,
            },
            &s,
        )
        .unwrap();
        let ids: Vec<&str> = out.workflows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["w1"]);
        // enabled_only=false includes the disabled one
        let out2 = wire_workflow_list(
            WireWorkflowListInput {
                persona_id: None,
                trigger_kind: Some("on_demand".into()),
                enabled_only: Some(false),
            },
            &s,
        )
        .unwrap();
        let mut ids2: Vec<&str> = out2.workflows.iter().map(|w| w.id.as_str()).collect();
        ids2.sort();
        assert_eq!(ids2, vec!["w1", "w3"]);
    }

    #[test]
    fn workflow_fire_by_id_returns_resolved_emit_projection() {
        let s = setup();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "w1".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_demand"}),
                action: json!({"kind":"emit_projection","projection_names":["slot_a","slot_b"]}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        let out = wire_workflow_fire(
            WireWorkflowFireInput {
                id: Some("w1".into()),
                event: None,
                persona_id: None,
                dry_run: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.fired.len(), 1);
        assert!(out.skipped.is_empty());
        let f = &out.fired[0];
        assert_eq!(f.id, "w1");
        assert_eq!(f.action_kind, "emit_projection");
        assert_eq!(
            f.action_emit_projection_names.as_deref(),
            Some(&["slot_a".to_string(), "slot_b".to_string()][..])
        );
    }

    #[test]
    fn workflow_fire_by_event_skips_unrelated_and_disabled() {
        let s = setup();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "match_open".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_event","event":"session_open"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "match_close".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_event","event":"session_close"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "disabled_close".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_event","event":"session_close"}),
                action: json!({"kind":"no_op"}),
                enabled: Some(false),
            },
            &s,
        )
        .unwrap();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "demand_only".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_demand"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        let out = wire_workflow_fire(
            WireWorkflowFireInput {
                id: None,
                event: Some("session_close".into()),
                persona_id: Some("alpha".into()),
                dry_run: None,
            },
            &s,
        )
        .unwrap();
        let fired_ids: Vec<&str> = out.fired.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(fired_ids, vec!["match_close"]);
        // 3 skipped: match_open (event mismatch), disabled_close (enabled=false),
        // demand_only (trigger kind mismatch)
        assert_eq!(out.skipped.len(), 3);
    }

    #[test]
    fn workflow_fire_requires_exactly_one_of_id_or_event() {
        let s = setup();
        let err = wire_workflow_fire(
            WireWorkflowFireInput {
                id: None,
                event: None,
                persona_id: None,
                dry_run: None,
            },
            &s,
        )
        .unwrap_err();
        assert!(err.to_string().contains("id"));
    }

    #[test]
    fn workflow_fire_by_id_handles_missing() {
        let s = setup();
        let out = wire_workflow_fire(
            WireWorkflowFireInput {
                id: Some("ghost".into()),
                event: None,
                persona_id: None,
                dry_run: None,
            },
            &s,
        )
        .unwrap();
        assert!(out.fired.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].0, "ghost");
    }

    #[test]
    fn workflow_delete_uses_node_delete() {
        let s = setup();
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "w1".into(),
                persona_id: None,
                trigger: json!({"kind":"on_demand"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        let out = wire_node_delete(
            WireDeleteInput {
                id_or_name: "w1".into(),
            },
            &s,
        )
        .unwrap();
        assert!(out.deleted);
        // gone from list
        assert!(wire_workflow_list(
            WireWorkflowListInput {
                persona_id: None,
                trigger_kind: None,
                enabled_only: Some(false),
            },
            &s,
        )
        .unwrap()
        .workflows
        .is_empty());
    }

    // wire_workflow_check tests deleted (2026-06-20、 issue 7069dede)。

    #[test]
    fn wire_node_delete_cascades_to_referencing_edges() {
        // node 削除は src / tgt どちらで参照されている edge も同 Tx 内で削除する
        // (schema が NOT-NULL FK edges→nodes なので orphan edge は表現不能、 cascade 一択)。
        let s = setup();
        s.insert_node(&bare_node("a", "persona")).unwrap();
        s.insert_node(&bare_node("b", "persona")).unwrap();
        s.insert_node(&bare_node("c", "persona")).unwrap();
        s.insert_edge(&Edge {
            id: ulid_from_seed("e_ab"),
            name: Some("e_ab".into()),
            src_node: ulid_from_seed("a"),
            tgt_node: ulid_from_seed("b"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        s.insert_edge(&Edge {
            id: ulid_from_seed("e_ca"),
            name: Some("e_ca".into()),
            src_node: ulid_from_seed("c"),
            tgt_node: ulid_from_seed("a"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        // 無関係 edge
        s.insert_edge(&Edge {
            id: ulid_from_seed("e_bc"),
            name: Some("e_bc".into()),
            src_node: ulid_from_seed("b"),
            tgt_node: ulid_from_seed("c"),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();

        wire_node_delete(
            WireDeleteInput {
                id_or_name: "a".into(),
            },
            &s,
        )
        .unwrap();
        // a を参照する edge は両方消える、 無関係 edge は残る
        assert!(s.get_edge(&ulid_from_seed("e_ab")).unwrap().is_none());
        assert!(s.get_edge(&ulid_from_seed("e_ca")).unwrap().is_none());
        assert!(s.get_edge(&ulid_from_seed("e_bc")).unwrap().is_some());
    }

    // ---- P3a Phase 2 (c) — projection_kind dispatch ----

    #[test]
    fn wire_init_rejects_non_static_projection_kind() {
        // P3a Phase 2 (c) — sync use_cases (wire_init / wire_render) only
        // permit projection_kind None / Some("static"). Non-static kinds
        // surface a structured error so the caller hops to wire_prompt_context.
        let s = setup();
        SpecRegistry::new(&s)
            .register("p", &Specification::TypeIs("persona".into()))
            .unwrap();
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "async_only",
                    "p",
                    "x",
                    TargetForm::Prompt,
                    PluginDispatch::custom("handlebars", "llm", None).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let result = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
            &default_registry(),
        );
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected non-static projection_kind to fail"),
        };
        assert!(err.contains("async_only"), "err: {err}");
        assert!(err.contains("llm"), "err: {err}");
        assert!(err.contains("wire_prompt_context"), "err: {err}");
    }

    #[test]
    fn wire_render_rejects_non_static_projection_kind() {
        let s = setup();
        SpecRegistry::new(&s)
            .register("p", &Specification::TypeIs("persona".into()))
            .unwrap();
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "summarized",
                    "p",
                    "x",
                    TargetForm::Prompt,
                    PluginDispatch::custom("handlebars", "cache", None).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let result = wire_render(
            WireRenderInput {
                projection_ref: "summarized".into(),
            },
            &s,
            &default_registry(),
        );
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected non-static projection_kind to fail"),
        };
        assert!(err.contains("summarized"), "err: {err}");
        assert!(err.contains("cache"), "err: {err}");
        assert!(err.contains("wire_prompt_context"), "err: {err}");
    }

    #[test]
    fn wire_init_accepts_explicit_static_projection_kind() {
        // explicit Some("static") must behave identically to None (= default).
        let s = setup();
        s.insert_node(&bare_node("alpha", "persona")).unwrap();
        SpecRegistry::new(&s)
            .register("p", &Specification::TypeIs("persona".into()))
            .unwrap();
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "explicit_static",
                    "p",
                    "n={{count}}",
                    TargetForm::Prompt,
                    PluginDispatch::custom("handlebars", "static", None).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let out = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
            &default_registry(),
        )
        .unwrap();
        assert_eq!(out.projections.len(), 1);
        assert_eq!(out.projections[0].rendered, "n=1");
    }

    // ---- P3a Phase 2 (d) — wire_node_update ----

    fn seed_wiring_node(s: &SqliteStorage, id: &str, source_uri: &str) {
        use crate::application::wiring_mapper;
        use crate::domain::entity::{PersonaId, Slot, Source};
        s.insert_node(&Node {
            id: ulid_from_seed(id),
            name: id.into(),
            r#type: wiring_mapper::WIRING_TYPE.into(),
            sot_ref: None,
            confidence: Some(1.0),
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: wiring_mapper::wiring_metadata_object(
                &PersonaId::new("alice").unwrap(),
                &Slot::new("mailbox").unwrap(),
                &Source::new(source_uri).unwrap(),
                None,
            ),
        })
        .unwrap();
    }

    #[test]
    fn node_update_merge_overwrites_one_key_preserves_others() {
        let s = setup();
        seed_wiring_node(&s, "alice.mailbox", "mini-app://mailbox?alias=for_alice");
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: "alice.mailbox".into(),
                metadata_patch: json!({
                    "source_uri": "mini-app://mailbox?alias=for_alice&limit=10",
                }),
                mode: WireNodeUpdateMode::Merge,
            },
            &s,
        )
        .unwrap();
        // source_uri が新値に、 persona / slot (= 旧 axis 互換 key) は維持される
        use crate::application::wiring_mapper;
        assert_eq!(out.id, "alice.mailbox");
        assert_eq!(out.mode, WireNodeUpdateMode::Merge);
        let synthetic = Node {
            id: ulid_from_seed(&out.id),
            name: out.id.clone(),
            r#type: wiring_mapper::WIRING_TYPE.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: out.metadata.clone(),
        };
        assert_eq!(
            wiring_mapper::extract_source_uri(&synthetic),
            Some("mini-app://mailbox?alias=for_alice&limit=10")
        );
        assert_eq!(wiring_mapper::extract_persona(&synthetic), Some("alice"));
        assert_eq!(wiring_mapper::extract_slot(&synthetic), Some("mailbox"));
        // 永続化検証
        let stored = s.get_node_by_name("alice.mailbox").unwrap().unwrap();
        assert_eq!(
            wiring_mapper::extract_source_uri(&stored),
            Some("mini-app://mailbox?alias=for_alice&limit=10")
        );
    }

    #[test]
    fn node_update_merge_null_value_deletes_key() {
        use crate::application::wiring_mapper;
        let s = setup();
        seed_wiring_node(&s, "alice.tmp", "mini-app://x");
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: "alice.tmp".into(),
                metadata_patch: json!({ wiring_mapper::META_SLOT: null }),
                mode: WireNodeUpdateMode::Merge,
            },
            &s,
        )
        .unwrap();
        // slot key (legacy `axis`) は消える、 persona と source_uri は残る
        let synthetic = Node {
            id: ulid_from_seed(&out.id),
            name: out.id.clone(),
            r#type: wiring_mapper::WIRING_TYPE.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: out.metadata.clone(),
        };
        assert!(wiring_mapper::extract_slot(&synthetic).is_none());
        assert_eq!(wiring_mapper::extract_persona(&synthetic), Some("alice"));
        assert_eq!(
            wiring_mapper::extract_source_uri(&synthetic),
            Some("mini-app://x")
        );
    }

    #[test]
    fn node_update_replace_swaps_metadata_wholesale() {
        let s = setup();
        seed_wiring_node(&s, "alice.tmp", "mini-app://x");
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: "alice.tmp".into(),
                metadata_patch: json!({"only_field": 42}),
                mode: WireNodeUpdateMode::Replace,
            },
            &s,
        )
        .unwrap();
        // 全 key が新値で置き換わる
        use crate::application::wiring_mapper;
        assert_eq!(out.metadata, json!({"only_field": 42}));
        let synthetic = Node {
            id: ulid_from_seed(&out.id),
            name: out.id.clone(),
            r#type: wiring_mapper::WIRING_TYPE.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: out.metadata.clone(),
        };
        assert!(wiring_mapper::extract_persona(&synthetic).is_none());
    }

    #[test]
    fn node_update_unknown_id_returns_not_found() {
        let s = setup();
        let result = wire_node_update(
            WireNodeUpdateInput {
                id: "does.not.exist".into(),
                metadata_patch: json!({"x": 1}),
                mode: WireNodeUpdateMode::Merge,
            },
            &s,
        );
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected NotFound"),
        };
        assert!(err.contains("does.not.exist"), "err: {err}");
    }

    #[test]
    fn node_update_rejects_non_object_patch() {
        let s = setup();
        seed_wiring_node(&s, "alice.tmp", "mini-app://x");
        let result = wire_node_update(
            WireNodeUpdateInput {
                id: "alice.tmp".into(),
                metadata_patch: json!("not an object"),
                mode: WireNodeUpdateMode::Merge,
            },
            &s,
        );
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected non-object patch to fail"),
        };
        assert!(err.contains("must be a JSON object"), "err: {err}");
    }

    #[test]
    fn node_update_mode_parse_rejects_unknown() {
        assert_eq!(
            WireNodeUpdateMode::parse("merge").unwrap(),
            WireNodeUpdateMode::Merge
        );
        assert_eq!(
            WireNodeUpdateMode::parse("replace").unwrap(),
            WireNodeUpdateMode::Replace
        );
        assert!(WireNodeUpdateMode::parse("upsert").is_err());
    }

    // ---- wire_context_get (ContextWiring read-aggregate) tests ----

    /// Helper: insert a wiring node `<persona>.<slot>` with optional
    /// `maintenance_exempt` flag.
    fn seed_wiring(
        s: &SqliteStorage,
        persona: &str,
        slot: &str,
        source_uri: &str,
        maintenance_exempt: bool,
    ) {
        let meta = if maintenance_exempt {
            json!({
                "persona": persona,
                "axis": slot,
                "source_uri": source_uri,
                "maintenance_exempt": true,
            })
        } else {
            json!({
                "persona": persona,
                "axis": slot,
                "source_uri": source_uri,
            })
        };
        let mut n = bare_node(&format!("{persona}.{slot}"), "outline_node");
        n.metadata = meta;
        s.insert_node(&n).unwrap();
    }

    #[test]
    fn context_get_returns_wirings_and_workflows_for_persona() {
        let s = setup();
        seed_wiring(
            &s,
            "alpha",
            "mailbox",
            "mini-app://mailbox?alias=for_alpha",
            false,
        );
        seed_wiring(&s, "alpha", "mail", "mini-app://mail?alias=for_alpha", true);
        // Different persona — must NOT appear in alpha's snapshot.
        seed_wiring(
            &s,
            "beta",
            "mailbox",
            "mini-app://mailbox?alias=for_beta",
            false,
        );

        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "alpha.workflow.session_close".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_event","event":"session_close"}),
                action: json!({"kind":"emit_projection","projection_names":["mailbox"]}),
                enabled: None,
            },
            &s,
        )
        .unwrap();
        // Different persona's workflow — also excluded.
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "beta.workflow.session_close".into(),
                persona_id: Some("beta".into()),
                trigger: json!({"kind":"on_demand"}),
                action: json!({"kind":"no_op"}),
                enabled: None,
            },
            &s,
        )
        .unwrap();

        let out = wire_context_get(
            WireContextGetInput {
                persona_id: "alpha".into(),
            },
            &s,
        )
        .unwrap();

        assert_eq!(out.persona_id, "alpha");
        // Sorted by slot: "mail" < "mailbox".
        assert_eq!(out.wirings.len(), 2);
        assert_eq!(out.wirings[0].slot, "mail");
        assert!(out.wirings[0].maintenance_exempt);
        assert_eq!(out.wirings[1].slot, "mailbox");
        assert!(!out.wirings[1].maintenance_exempt);

        assert_eq!(out.workflows.len(), 1);
        assert_eq!(out.workflows[0].id, "alpha.workflow.session_close");
        assert_eq!(out.workflows[0].persona_id.as_deref(), Some("alpha"));
    }

    #[test]
    fn context_get_resolves_projection_ref_via_naming_convention() {
        let s = setup();
        seed_wiring(
            &s,
            "alpha",
            "mailbox",
            "mini-app://mailbox?alias=for_alpha",
            false,
        );
        // Register the projection at the convention-derived name.
        ProjectionRegistry::new(&s)
            .register(
                &Projection::from_parts(
                    "alpha.section.mailbox",
                    "spec_ignored_here",
                    "tpl",
                    TargetForm::Prompt,
                    PluginDispatch::Default,
                )
                .unwrap(),
            )
            .unwrap();

        let out = wire_context_get(
            WireContextGetInput {
                persona_id: "alpha".into(),
            },
            &s,
        )
        .unwrap();

        assert_eq!(out.wirings.len(), 1);
        assert_eq!(
            out.wirings[0].projection_ref.as_deref(),
            Some("alpha.section.mailbox"),
            "projection_ref must resolve via <persona>.section.<slot> naming convention",
        );
    }

    #[test]
    fn context_get_leaves_projection_ref_none_when_not_registered() {
        let s = setup();
        seed_wiring(
            &s,
            "alpha",
            "mailbox",
            "mini-app://mailbox?alias=for_alpha",
            false,
        );

        let out = wire_context_get(
            WireContextGetInput {
                persona_id: "alpha".into(),
            },
            &s,
        )
        .unwrap();

        assert_eq!(out.wirings.len(), 1);
        assert!(out.wirings[0].projection_ref.is_none());
    }

    #[test]
    fn context_get_returns_empty_for_unknown_persona() {
        let s = setup();
        seed_wiring(
            &s,
            "alpha",
            "mailbox",
            "mini-app://mailbox?alias=for_alpha",
            false,
        );

        let out = wire_context_get(
            WireContextGetInput {
                persona_id: "ghost".into(),
            },
            &s,
        )
        .unwrap();

        assert_eq!(out.persona_id, "ghost");
        assert!(out.wirings.is_empty());
        assert!(out.workflows.is_empty());
    }

    #[test]
    fn context_get_rejects_empty_persona_id() {
        let s = setup();
        let err = wire_context_get(
            WireContextGetInput {
                persona_id: String::new(),
            },
            &s,
        )
        .expect_err("empty persona id must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidPersonaId(_))
        ));
    }

    #[test]
    fn context_get_skips_drift_wiring_nodes_silently() {
        let s = setup();
        // Wiring node missing the source_uri metadata — drift case.
        let mut drift = bare_node("alpha.mailbox", "outline_node");
        drift.metadata = json!({
            "persona": "alpha",
            "axis": "mailbox",
            // source_uri missing
        });
        s.insert_node(&drift).unwrap();
        // Valid wiring alongside.
        seed_wiring(
            &s,
            "alpha",
            "mail",
            "mini-app://mail?alias=for_alpha",
            false,
        );

        let out = wire_context_get(
            WireContextGetInput {
                persona_id: "alpha".into(),
            },
            &s,
        )
        .unwrap();

        // Only the valid wiring survives; drift is doctored, not surfaced.
        assert_eq!(out.wirings.len(), 1);
        assert_eq!(out.wirings[0].slot, "mail");
    }

    // ---- enumerate_slot_names: projection_names / projection_exclude_names filter ----
    //
    // 4 case (両 None / include only / exclude only / both) + 3 edge (交差優先 /
    // 未登録 name / 空集合)。 `WirePromptContextInput` docstring の AND NOT
    // semantics に対応する。

    fn seed_three_slots(s: &SqliteStorage) {
        seed_wiring(s, "alpha", "news", "mini-app://news?alias=for_alpha", false);
        seed_wiring(s, "alpha", "mail", "mini-app://mail?alias=for_alpha", false);
        seed_wiring(s, "alpha", "todo", "mini-app://todo?alias=for_alpha", false);
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn enumerate_slots_both_none_returns_all() {
        let s = setup();
        seed_three_slots(&s);
        let got = enumerate_slot_names(&s, "alpha", None, None).unwrap();
        assert_eq!(
            sorted(got),
            vec!["mail".to_string(), "news".into(), "todo".into()]
        );
    }

    #[test]
    fn enumerate_slots_include_only_returns_explicit_set() {
        let s = setup();
        seed_three_slots(&s);
        let include = vec!["news".to_string(), "mail".into()];
        let got = enumerate_slot_names(&s, "alpha", Some(&include), None).unwrap();
        // explicit はそのままの順序 (= 現挙動互換、 ソート前提にしない)。
        assert_eq!(got, vec!["news".to_string(), "mail".into()]);
    }

    #[test]
    fn enumerate_slots_exclude_only_subtracts_from_all() {
        let s = setup();
        seed_three_slots(&s);
        let exclude = vec!["mail".to_string()];
        let got = enumerate_slot_names(&s, "alpha", None, Some(&exclude)).unwrap();
        assert_eq!(sorted(got), vec!["news".to_string(), "todo".into()]);
    }

    #[test]
    fn enumerate_slots_both_include_and_exclude_and_not() {
        let s = setup();
        seed_three_slots(&s);
        let include = vec!["news".to_string(), "mail".into(), "todo".into()];
        let exclude = vec!["mail".to_string()];
        let got = enumerate_slot_names(&s, "alpha", Some(&include), Some(&exclude)).unwrap();
        // include の順序を保ったまま exclude を引く。
        assert_eq!(got, vec!["news".to_string(), "todo".into()]);
    }

    #[test]
    fn enumerate_slots_intersection_exclude_wins() {
        // include / exclude が交差した name (= "mail") は exclude が優先 (除外)。
        let s = setup();
        seed_three_slots(&s);
        let include = vec!["news".to_string(), "mail".into()];
        let exclude = vec!["mail".to_string()];
        let got = enumerate_slot_names(&s, "alpha", Some(&include), Some(&exclude)).unwrap();
        assert_eq!(got, vec!["news".to_string()]);
    }

    #[test]
    fn enumerate_slots_unknown_exclude_name_is_ignored() {
        // exclude に未登録 name を含めても warning なく無視 (後方互換性優先)。
        let s = setup();
        seed_three_slots(&s);
        let exclude = vec!["nonexistent".to_string()];
        let got = enumerate_slot_names(&s, "alpha", None, Some(&exclude)).unwrap();
        assert_eq!(
            sorted(got),
            vec!["mail".to_string(), "news".into(), "todo".into()]
        );
    }

    #[test]
    fn enumerate_slots_empty_result_returns_empty_vec() {
        // include 集合 ⊆ exclude 集合 のとき結果は空集合。 None 両指定 (= 全件)
        // とは区別され、 caller の明示意図を尊重する。
        let s = setup();
        seed_three_slots(&s);
        let include = vec!["news".to_string(), "mail".into()];
        let exclude = vec!["news".to_string(), "mail".into()];
        let got = enumerate_slot_names(&s, "alpha", Some(&include), Some(&exclude)).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn enumerate_slots_exclude_empty_vec_is_noop() {
        // exclude=Some(&[]) は exclude=None と同等 (= 全件返却)。
        let s = setup();
        seed_three_slots(&s);
        let empty: Vec<String> = vec![];
        let got = enumerate_slot_names(&s, "alpha", None, Some(&empty)).unwrap();
        assert_eq!(
            sorted(got),
            vec!["mail".to_string(), "news".into(), "todo".into()]
        );
    }

    // ---- adapter auth — merge_auth_query / append_query_param ----

    #[test]
    fn merge_auth_query_no_metadata_auth_leaves_uri_unchanged() {
        assert_eq!(
            merge_auth_query("github://octocat/hello-world", None),
            "github://octocat/hello-world"
        );
    }

    #[test]
    fn merge_auth_query_appends_when_uri_has_no_query() {
        assert_eq!(
            merge_auth_query("github://octocat/hello-world", Some("github-alt")),
            "github://octocat/hello-world?auth=github-alt"
        );
    }

    #[test]
    fn merge_auth_query_appends_with_ampersand_when_uri_already_has_query() {
        assert_eq!(
            merge_auth_query(
                "github://octocat/hello-world?kind=issues",
                Some("github-alt")
            ),
            "github://octocat/hello-world?kind=issues&auth=github-alt"
        );
    }

    #[test]
    fn merge_auth_query_uri_side_auth_wins_no_overwrite() {
        // URI-declared auth wins: metadata.auth must never overwrite it.
        assert_eq!(
            merge_auth_query(
                "github://octocat/hello-world?auth=from-uri",
                Some("from-metadata")
            ),
            "github://octocat/hello-world?auth=from-uri"
        );
    }

    #[test]
    fn merge_auth_query_preserves_fragment_after_merged_query() {
        assert_eq!(
            merge_auth_query("file:///tmp/x#frag", Some("svc")),
            "file:///tmp/x?auth=svc#frag"
        );
    }

    #[test]
    fn merge_auth_query_unparsable_uri_left_unchanged() {
        // Not `WireUri::parse`-able (no scheme separator) — surfaced later
        // by `PluginRegistry::route`'s own parse error, not masked here.
        assert_eq!(
            merge_auth_query("not-a-uri", Some("svc")),
            "not-a-uri",
            "unparsable source_uri must pass through unchanged"
        );
    }

    #[test]
    fn append_query_param_uses_question_mark_when_absent() {
        assert_eq!(
            append_query_param("mini-app://mailbox", "auth", "k"),
            "mini-app://mailbox?auth=k"
        );
    }

    #[test]
    fn append_query_param_uses_ampersand_when_query_present() {
        assert_eq!(
            append_query_param("mini-app://mailbox?alias=x", "auth", "k"),
            "mini-app://mailbox?alias=x&auth=k"
        );
    }

    // ---- adapter auth — collect_slot auth extraction ----

    fn register_stub_projection(s: &SqliteStorage, name: &str) {
        use crate::domain::entity::projection::{PluginDispatch, Projection};
        ProjectionRegistry::new(s)
            .register(
                &Projection::from_parts(
                    name,
                    "unused_spec_ref",
                    "n={{count}}",
                    TargetForm::Prompt,
                    PluginDispatch::Default,
                )
                .unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn collect_slot_extracts_auth_from_wiring_metadata() {
        use crate::application::wiring_mapper;
        use crate::domain::entity::{PersonaId, Slot, Source};

        let s = setup();
        let mut extras = serde_json::Map::new();
        extras.insert("auth".to_string(), json!("svc-x"));
        let mut node = bare_node("p.issues", wiring_mapper::WIRING_TYPE);
        node.metadata = wiring_mapper::wiring_metadata_object(
            &PersonaId::new("p").unwrap(),
            &Slot::new("issues").unwrap(),
            &Source::new("github://o/r").unwrap(),
            Some(extras),
        );
        s.insert_node(&node).unwrap();
        register_stub_projection(&s, "p.section.issues");

        let proj_reg = ProjectionRegistry::new(&s);
        let overlays = std::collections::BTreeMap::new();
        let mut warnings = Vec::new();
        let collected = collect_slot("issues", "p", &s, &proj_reg, &overlays, &mut warnings)
            .unwrap()
            .expect("wiring entry should collect");
        assert_eq!(collected.source_uri, "github://o/r");
        assert_eq!(collected.auth.as_deref(), Some("svc-x"));
        assert!(warnings.is_empty(), "warnings: {warnings:?}");
    }

    #[test]
    fn collect_slot_auth_none_when_metadata_lacks_auth() {
        use crate::application::wiring_mapper;
        use crate::domain::entity::{PersonaId, Slot, Source};

        let s = setup();
        let mut node = bare_node("p.mailbox", wiring_mapper::WIRING_TYPE);
        node.metadata = wiring_mapper::wiring_metadata_object(
            &PersonaId::new("p").unwrap(),
            &Slot::new("mailbox").unwrap(),
            &Source::new("mini-app://mailbox").unwrap(),
            None,
        );
        s.insert_node(&node).unwrap();
        register_stub_projection(&s, "p.section.mailbox");

        let proj_reg = ProjectionRegistry::new(&s);
        let overlays = std::collections::BTreeMap::new();
        let mut warnings = Vec::new();
        let collected = collect_slot("mailbox", "p", &s, &proj_reg, &overlays, &mut warnings)
            .unwrap()
            .expect("wiring entry should collect");
        assert_eq!(collected.auth, None);
    }

    // ---- wire_slot_register / wire_slot_delete ----

    fn slot_register_input(
        persona: &str,
        slot: &str,
        uri: &str,
        template: &str,
    ) -> WireSlotRegisterInput {
        WireSlotRegisterInput {
            persona_id: persona.into(),
            slot: slot.into(),
            source_uri: uri.into(),
            template: template.into(),
            target_form: TargetForm::Markdown,
            maintenance_exempt: None,
            auth: None,
        }
    }

    #[test]
    fn wire_slot_register_creates_node_spec_and_projection() {
        use crate::application::wiring_mapper;

        let s = setup();
        let out = wire_slot_register(
            slot_register_input("alpha", "notes", "file:~/notes.md", "## Notes\n{{count}}"),
            &s,
        )
        .unwrap();

        assert_eq!(out.node_name, "alpha.notes");
        assert!(out.node_created);
        assert_eq!(out.spec_name, "alpha.spec.notes");
        assert_eq!(out.projection_name, "alpha.section.notes");

        let node = s.get_node_by_name("alpha.notes").unwrap().expect("node");
        assert_eq!(wiring_mapper::extract_persona(&node), Some("alpha"));
        assert_eq!(wiring_mapper::extract_slot(&node), Some("notes"));
        assert_eq!(
            wiring_mapper::extract_source_uri(&node),
            Some("file:~/notes.md")
        );

        let spec = SpecRegistry::new(&s)
            .get("alpha.spec.notes")
            .unwrap()
            .expect("spec");
        assert!(matches!(spec, Specification::And(parts) if parts.len() == 3));

        let proj = ProjectionRegistry::new(&s)
            .get("alpha.section.notes")
            .unwrap()
            .expect("projection");
        assert_eq!(proj.template().as_str(), "## Notes\n{{count}}");
        assert_eq!(proj.spec_ref().as_str(), "alpha.spec.notes");
    }

    #[test]
    fn wire_slot_register_upserts_in_place_preserving_node_id() {
        use crate::application::wiring_mapper;

        let s = setup();
        let first = wire_slot_register(
            slot_register_input("alpha", "notes", "file:~/a.md", "v1 {{count}}"),
            &s,
        )
        .unwrap();
        // Attach a passthrough metadata key to prove merge keeps it.
        let node = s.get_node_by_name("alpha.notes").unwrap().unwrap();
        let mut meta = node.metadata.as_object().cloned().unwrap();
        meta.insert("custom_flag".into(), json!(true));
        s.update_node_metadata(&node.id, &serde_json::Value::Object(meta))
            .unwrap();

        let second = wire_slot_register(
            WireSlotRegisterInput {
                maintenance_exempt: Some(true),
                ..slot_register_input("alpha", "notes", "file:~/b.md", "v2 {{count}}")
            },
            &s,
        )
        .unwrap();

        assert!(!second.node_created, "second call must be an upsert");
        assert_eq!(first.node_id, second.node_id, "node ULID preserved");

        let node = s.get_node_by_name("alpha.notes").unwrap().unwrap();
        assert_eq!(
            wiring_mapper::extract_source_uri(&node),
            Some("file:~/b.md"),
            "canonical key overwritten"
        );
        assert!(
            wiring_mapper::extract_maintenance_exempt(&node),
            "maintenance_exempt applied"
        );
        assert_eq!(
            node.metadata.get("custom_flag"),
            Some(&json!(true)),
            "passthrough key kept"
        );

        let proj = ProjectionRegistry::new(&s)
            .get("alpha.section.notes")
            .unwrap()
            .unwrap();
        assert_eq!(proj.template().as_str(), "v2 {{count}}");
    }

    #[test]
    fn wire_slot_register_rejects_invalid_slot() {
        let s = setup();
        let err = wire_slot_register(slot_register_input("alpha", "a.b", "file:~/x.md", "t"), &s)
            .expect_err("dotted slot must reject");
        assert!(err.to_string().contains("."), "err: {err}");
        // Nothing was written.
        assert!(s.get_node_by_name("alpha.a.b").unwrap().is_none());
    }

    #[test]
    fn wire_slot_delete_removes_all_three_and_is_idempotent() {
        let s = setup();
        wire_slot_register(
            slot_register_input("alpha", "notes", "file:~/n.md", "{{count}}"),
            &s,
        )
        .unwrap();

        let del = wire_slot_delete(
            WireSlotDeleteInput {
                persona_id: "alpha".into(),
                slot: "notes".into(),
            },
            &s,
        )
        .unwrap();
        assert!(del.node_deleted && del.spec_deleted && del.projection_deleted);
        assert!(s.get_node_by_name("alpha.notes").unwrap().is_none());
        assert!(SpecRegistry::new(&s)
            .get("alpha.spec.notes")
            .unwrap()
            .is_none());
        assert!(ProjectionRegistry::new(&s)
            .get("alpha.section.notes")
            .unwrap()
            .is_none());

        let again = wire_slot_delete(
            WireSlotDeleteInput {
                persona_id: "alpha".into(),
                slot: "notes".into(),
            },
            &s,
        )
        .unwrap();
        assert!(
            !again.node_deleted && !again.spec_deleted && !again.projection_deleted,
            "second delete reports false everywhere"
        );
    }

    // ---- collect_slot: explicit projection_ref ----

    #[test]
    fn collect_slot_honors_explicit_projection_ref_over_convention() {
        use crate::application::wiring_mapper;

        let s = setup();
        let mut node = bare_node("p.mailbox", wiring_mapper::WIRING_TYPE);
        node.metadata = json!({
            "persona": "p",
            "axis": "mailbox",
            "source_uri": "mini-app://mailbox",
            "projection_ref": "shared.section.mailbox",
        });
        s.insert_node(&node).unwrap();
        // Register BOTH names — the explicit ref must win.
        register_stub_projection(&s, "p.section.mailbox");
        register_stub_projection(&s, "shared.section.mailbox");

        let proj_reg = ProjectionRegistry::new(&s);
        let overlays = std::collections::BTreeMap::new();
        let mut warnings = Vec::new();
        let collected = collect_slot("mailbox", "p", &s, &proj_reg, &overlays, &mut warnings)
            .unwrap()
            .expect("wiring entry should collect");
        assert_eq!(collected.projection_name, "shared.section.mailbox");
        assert!(warnings.is_empty(), "warnings: {warnings:?}");
    }

    #[test]
    fn collect_slot_missing_explicit_projection_ref_warns_and_skips() {
        use crate::application::wiring_mapper;

        let s = setup();
        let mut node = bare_node("p.mailbox", wiring_mapper::WIRING_TYPE);
        node.metadata = json!({
            "persona": "p",
            "axis": "mailbox",
            "source_uri": "mini-app://mailbox",
            "projection_ref": "nowhere.section.mailbox",
        });
        s.insert_node(&node).unwrap();
        // Convention name IS registered — but the explicit ref points elsewhere,
        // so the slot must NOT silently fall back (that would rebuild the trap).
        register_stub_projection(&s, "p.section.mailbox");

        let proj_reg = ProjectionRegistry::new(&s);
        let overlays = std::collections::BTreeMap::new();
        let mut warnings = Vec::new();
        let collected =
            collect_slot("mailbox", "p", &s, &proj_reg, &overlays, &mut warnings).unwrap();
        assert!(collected.is_none(), "slot must skip");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("nowhere.section.mailbox")),
            "warning names the missing explicit ref: {warnings:?}"
        );
    }

    // ---- wire_fetch ----

    #[tokio::test]
    async fn wire_fetch_returns_raw_adapter_output_for_file_uri() {
        let dir = std::env::temp_dir().join(format!("wire-fetch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("body.md");
        std::fs::write(&file, "hello wire_fetch").unwrap();

        let s = std::sync::Arc::new(std::sync::Mutex::new(setup()));
        let registry = default_registry();
        let out = wire_fetch(
            WireFetchInput {
                source_uri: Some(format!("file:{}", file.display())),
                persona_id: None,
                slot: None,
            },
            s,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(out.fetched_data["body"], json!("hello wire_fetch"));
        assert_eq!(out.fetched_data["scheme"], json!("file"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn wire_fetch_resolves_wiring_entry_by_persona_and_slot() {
        let dir = std::env::temp_dir().join(format!("wire-fetch-slot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.md");
        std::fs::write(&file, "slot preview").unwrap();

        let storage = setup();
        wire_slot_register(
            slot_register_input(
                "alpha",
                "notes",
                &format!("file:{}", file.display()),
                "{{count}}",
            ),
            &storage,
        )
        .unwrap();

        let s = std::sync::Arc::new(std::sync::Mutex::new(storage));
        let registry = default_registry();
        let out = wire_fetch(
            WireFetchInput {
                source_uri: None,
                persona_id: Some("alpha".into()),
                slot: Some("notes".into()),
            },
            s,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(out.fetched_data["body"], json!("slot preview"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn wire_fetch_rejects_ambiguous_or_empty_input() {
        let s = std::sync::Arc::new(std::sync::Mutex::new(setup()));
        let registry = default_registry();
        let err = wire_fetch(
            WireFetchInput {
                source_uri: None,
                persona_id: None,
                slot: None,
            },
            s.clone(),
            &registry,
        )
        .await
        .expect_err("empty input must reject");
        assert!(err.to_string().contains("wire_fetch"), "err: {err}");

        let err = wire_fetch(
            WireFetchInput {
                source_uri: Some("file:~/x.md".into()),
                persona_id: Some("alpha".into()),
                slot: Some("notes".into()),
            },
            s,
            &registry,
        )
        .await
        .expect_err("both forms at once must reject");
        assert!(err.to_string().contains("wire_fetch"), "err: {err}");
    }

    // ---- empty-render warning ----

    #[tokio::test]
    async fn wire_prompt_context_warns_on_empty_render_with_non_null_fetch() {
        let dir = std::env::temp_dir().join(format!("wire-empty-warn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.md");
        std::fs::write(&file, "real content").unwrap();

        let storage = setup();
        // Template references a field path that does not exist in the file
        // adapter's return shape — renders to empty (the dogfooded mistake).
        wire_slot_register(
            slot_register_input(
                "alpha",
                "notes",
                &format!("file:{}", file.display()),
                "{{#each entries}}{{this.fetched_data.content}}{{/each}}",
            ),
            &storage,
        )
        .unwrap();

        let s = std::sync::Arc::new(std::sync::Mutex::new(storage));
        let registry = default_registry();
        let out = wire_prompt_context(
            WirePromptContextInput {
                persona_id: "alpha".into(),
                projection_names: None,
                projection_exclude_names: None,
            },
            s,
            &registry,
        )
        .await
        .unwrap();
        assert!(
            out.warnings.iter().any(|w| w.contains("rendered empty")),
            "warnings: {:?}",
            out.warnings
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn wire_prompt_context_no_warning_when_template_renders_content() {
        let dir = std::env::temp_dir().join(format!("wire-nonempty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.md");
        std::fs::write(&file, "real content").unwrap();

        let storage = setup();
        wire_slot_register(
            slot_register_input(
                "alpha",
                "notes",
                &format!("file:{}", file.display()),
                "{{#each entries}}{{this.fetched_data.body}}{{/each}}",
            ),
            &storage,
        )
        .unwrap();

        let s = std::sync::Arc::new(std::sync::Mutex::new(storage));
        let registry = default_registry();
        let out = wire_prompt_context(
            WirePromptContextInput {
                persona_id: "alpha".into(),
                projection_names: None,
                projection_exclude_names: None,
            },
            s,
            &registry,
        )
        .await
        .unwrap();
        assert!(
            out.warnings.is_empty(),
            "no warnings expected: {:?}",
            out.warnings
        );
        assert!(out.prompt_context.contains("real content"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- end-to-end: explicit projection_ref renders through shared projection ----

    #[tokio::test]
    async fn wire_prompt_context_renders_through_explicit_projection_ref() {
        use crate::application::wiring_mapper;
        use crate::domain::entity::projection::{PluginDispatch, Projection};

        let dir = std::env::temp_dir().join(format!("wire-projref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shared.md");
        std::fs::write(&file, "shared body").unwrap();

        let storage = setup();
        // A shared, non-convention projection…
        ProjectionRegistry::new(&storage)
            .register(
                &Projection::from_parts(
                    "shared.section.notes",
                    "unused_spec_ref",
                    "SHARED: {{#each entries}}{{this.fetched_data.body}}{{/each}}",
                    TargetForm::Markdown,
                    PluginDispatch::Default,
                )
                .unwrap(),
            )
            .unwrap();
        // …bound explicitly from the wiring entry (bundle [[wirings]] shape).
        let mut node = bare_node("beta.notes", wiring_mapper::WIRING_TYPE);
        node.metadata = json!({
            "persona": "beta",
            "axis": "notes",
            "source_uri": format!("file:{}", file.display()),
            "projection_ref": "shared.section.notes",
        });
        storage.insert_node(&node).unwrap();

        let s = std::sync::Arc::new(std::sync::Mutex::new(storage));
        let registry = default_registry();
        let out = wire_prompt_context(
            WirePromptContextInput {
                persona_id: "beta".into(),
                projection_names: None,
                projection_exclude_names: None,
            },
            s,
            &registry,
        )
        .await
        .unwrap();
        assert!(
            out.prompt_context.contains("SHARED: shared body"),
            "rendered: {}",
            out.prompt_context
        );
        assert_eq!(out.projections[0].name, "shared.section.notes");
        std::fs::remove_dir_all(&dir).ok();
    }
}
