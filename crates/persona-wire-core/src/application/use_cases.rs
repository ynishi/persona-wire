//! Use cases — orchestration of Domain + Infrastructure for wire_* flows.

use crate::application::plugin_registry::PluginRegistry;
use crate::application::projection_registry::{ProjectionRegistry, TargetForm};
use crate::application::spec_registry::SpecRegistry;
use crate::domain::error::{DomainError, WireError, WireResult};
use crate::domain::graph::Node;
use crate::domain::port::ProjectionInput;
use crate::domain::specification::Specification;
use crate::infrastructure::storage::SqliteStorage;

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
/// async `wire_prompt_context` path; see
/// `docs/design/render-trinity-domain-entity.md` §1.2.
fn build_broadcast_render_data(matched: &[Node], persona_id: Option<&str>) -> serde_json::Value {
    let names: Vec<&str> = matched.iter().map(|n| n.id.as_str()).collect();
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
        let slots = enumerate_slot_names(&s, &input.persona_id, input.projection_names.as_deref())?;
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
/// spec query で全件取得し `metadata.axis` (= storage 互換 key、
/// docs/design/render-trinity-domain-entity.md Appendix B 参照) を抽出する。
fn enumerate_slot_names(
    storage: &SqliteStorage,
    persona_id: &str,
    explicit: Option<&[String]>,
) -> WireResult<Vec<String>> {
    if let Some(names) = explicit {
        return Ok(names.to_vec());
    }
    let spec = Specification::And(vec![
        Specification::TypeIs("outline_node".to_string()),
        Specification::MetadataEq {
            path: "persona".to_string(),
            value: serde_json::json!(persona_id),
        },
    ]);
    let nodes = collect_matching_nodes(storage, &spec)?;
    Ok(nodes
        .iter()
        .filter_map(|n| {
            n.metadata
                .get("axis")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect())
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
    let Some(node) = storage.get_node(&node_id)? else {
        return Ok(None);
    };
    let Some(source_uri) = node.metadata.get("source_uri").and_then(|v| v.as_str()) else {
        warnings.push(format!(
            "wiring entry '{node_id}' lacks metadata.source_uri — slot skipped"
        ));
        return Ok(None);
    };

    let projection_name =
        crate::application::projection_naming::workflow_emit_projection_name(persona_id, slot);
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
    let fetched = match registry.route(&c.source_uri) {
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
    Ok(RenderedProjection {
        name: c.projection_name.clone(),
        target_form: c.target_form,
        rendered,
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
/// (issue `15a46ce6` — 41/41 false-positive on the shi dogfood session).
pub(crate) fn is_self_attached_wiring(node: &crate::domain::graph::Node) -> bool {
    let m = &node.metadata;
    if !m.is_object() {
        return false;
    }
    let has_source_uri = m
        .get("source_uri")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let is_exempt = m
        .get("maintenance_exempt")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    has_source_uri || is_exempt
}

/// Walk every node type and tally totals + orphan count. A node is counted as
/// an orphan only when it has no in- or out-edges **and** is not a
/// self-attached wiring entry (see `is_self_attached_wiring`). Shared scan
/// primitive for `wire_close` / `wire_doctor`; P3 daemon will extend this with
/// stale / asymmetric / high-fanout checks.
pub fn graph_scan_summary(storage: &SqliteStorage) -> WireResult<GraphScanSummary> {
    let mut total_nodes = 0_usize;
    let mut total_edges = 0_usize;
    let mut orphan = 0_usize;

    for t in storage.list_types_by_kind("node")? {
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
/// 数値カウントが必要なら [`graph_scan_summary`] を別途呼ぶ。
pub fn wire_doctor(
    storage: &SqliteStorage,
    persona_id: Option<String>,
) -> WireResult<WireDoctorOutput> {
    let report_markdown = crate::application::doctor::run(storage, persona_id)?;
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
        (None, Some(name)) => SpecRegistry::new(storage).get(name)?.ok_or_else(|| {
            crate::domain::error::WireError::Domain(DomainError::NotFound(format!("spec: {name}")))
        })?,
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
            id: n.id,
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
    let proj = ProjectionRegistry::new(storage)
        .get(&input.projection_ref)?
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
    let Some(existing) = storage.get_node(&input.id)? else {
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

    let updated = storage.update_node_metadata(&input.id, &final_metadata)?;
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
    let deleted = storage.delete_node(&input.id_or_name)?;
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
    let deleted = storage.delete_edge(&input.id_or_name)?;
    Ok(WireDeleteOutput {
        kind: "edge",
        id_or_name: input.id_or_name,
        deleted,
    })
}

/// Delete a Specification by name. Projections referencing it via spec_ref will
/// start returning dangling-spec errors at render time (existing wire_render contract).
pub fn wire_spec_delete(
    input: WireDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireDeleteOutput> {
    let deleted = storage.delete_specification(&input.id_or_name)?;
    Ok(WireDeleteOutput {
        kind: "spec",
        id_or_name: input.id_or_name,
        deleted,
    })
}

/// Delete a NamedProjection by name.
pub fn wire_projection_delete(
    input: WireDeleteInput,
    storage: &SqliteStorage,
) -> WireResult<WireDeleteOutput> {
    let deleted = storage.delete_projection(&input.id_or_name)?;
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
    let meta = node.metadata;
    let persona_id = meta
        .get("persona")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let trigger = meta
        .get("trigger")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let action = meta
        .get("action")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let enabled = meta
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    Ok(WorkflowSummary {
        id: node.id,
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
        let Some(node) = storage.get_node(id)? else {
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
    use crate::domain::graph::{Edge, Node};
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
            id: id.into(),
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
            id: "e1".into(),
            src_node: "a".into(),
            tgt_node: "b".into(),
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
        let mut n1 = bare_node("p.mailbox", "outline_node");
        n1.metadata = json!({
            "persona": "p",
            "axis": "mailbox",
            "source_uri": "mini-app://mailbox?alias=for_p"
        });
        s.insert_node(&n1).unwrap();

        // wiring entry with maintenance_exempt=true — should NOT count as orphan
        let mut n2 = bare_node("p.priorities", "outline_node");
        n2.metadata = json!({
            "persona": "p",
            "axis": "priorities",
            "maintenance_exempt": true
        });
        s.insert_node(&n2).unwrap();

        // bare persona node with no metadata + no edges — SHOULD count as orphan
        s.insert_node(&bare_node("p", "persona")).unwrap();

        let out = wire_doctor(&s, None).unwrap();
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
        let out = wire_doctor(&storage, None).expect("wire_doctor should pass on empty setup");
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
            id: "e1".into(),
            src_node: "a".into(),
            tgt_node: "b".into(),
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
            id: "e_ab".into(),
            src_node: "a".into(),
            tgt_node: "b".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        s.insert_edge(&Edge {
            id: "e_ca".into(),
            src_node: "c".into(),
            tgt_node: "a".into(),
            kind: "routes_to".into(),
            severity: None,
            metadata: json!({}),
            version: 1,
            prev_id: None,
        })
        .unwrap();
        // 無関係 edge
        s.insert_edge(&Edge {
            id: "e_bc".into(),
            src_node: "b".into(),
            tgt_node: "c".into(),
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
        assert!(s.get_edge(&"e_ab".to_string()).unwrap().is_none());
        assert!(s.get_edge(&"e_ca".to_string()).unwrap().is_none());
        assert!(s.get_edge(&"e_bc".to_string()).unwrap().is_some());
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
        s.insert_node(&Node {
            id: id.into(),
            r#type: "outline_node".into(),
            sot_ref: None,
            confidence: Some(1.0),
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: json!({
                "persona": "shi",
                "axis": "mailbox",
                "source_uri": source_uri,
            }),
        })
        .unwrap();
    }

    #[test]
    fn node_update_merge_overwrites_one_key_preserves_others() {
        let s = setup();
        seed_wiring_node(&s, "shi.mailbox", "mini-app://mailbox?alias=for_shi");
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: "shi.mailbox".into(),
                metadata_patch: json!({
                    "source_uri": "mini-app://mailbox?alias=for_shi&limit=10",
                }),
                mode: WireNodeUpdateMode::Merge,
            },
            &s,
        )
        .unwrap();
        // source_uri が新値に、 persona / axis は維持される
        assert_eq!(out.id, "shi.mailbox");
        assert_eq!(out.mode, WireNodeUpdateMode::Merge);
        assert_eq!(
            out.metadata["source_uri"].as_str().unwrap(),
            "mini-app://mailbox?alias=for_shi&limit=10"
        );
        assert_eq!(out.metadata["persona"].as_str().unwrap(), "shi");
        assert_eq!(out.metadata["axis"].as_str().unwrap(), "mailbox");
        // 永続化検証
        let stored = s.get_node(&"shi.mailbox".to_string()).unwrap().unwrap();
        assert_eq!(
            stored.metadata["source_uri"].as_str().unwrap(),
            "mini-app://mailbox?alias=for_shi&limit=10"
        );
    }

    #[test]
    fn node_update_merge_null_value_deletes_key() {
        let s = setup();
        seed_wiring_node(&s, "shi.tmp", "mini-app://x");
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: "shi.tmp".into(),
                metadata_patch: json!({"axis": null}),
                mode: WireNodeUpdateMode::Merge,
            },
            &s,
        )
        .unwrap();
        // axis key は消える、 persona と source_uri は残る
        assert!(out.metadata.get("axis").is_none());
        assert_eq!(out.metadata["persona"].as_str().unwrap(), "shi");
        assert_eq!(out.metadata["source_uri"].as_str().unwrap(), "mini-app://x");
    }

    #[test]
    fn node_update_replace_swaps_metadata_wholesale() {
        let s = setup();
        seed_wiring_node(&s, "shi.tmp", "mini-app://x");
        let out = wire_node_update(
            WireNodeUpdateInput {
                id: "shi.tmp".into(),
                metadata_patch: json!({"only_field": 42}),
                mode: WireNodeUpdateMode::Replace,
            },
            &s,
        )
        .unwrap();
        // 全 key が新値で置き換わる
        assert_eq!(out.metadata, json!({"only_field": 42}));
        assert!(out.metadata.get("persona").is_none());
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
        seed_wiring_node(&s, "shi.tmp", "mini-app://x");
        let result = wire_node_update(
            WireNodeUpdateInput {
                id: "shi.tmp".into(),
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
}
