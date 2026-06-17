//! Use cases — orchestration of Domain + Infrastructure for wire_* flows.

use crate::application::projection_registry::{ProjectionRegistry, TargetForm};
use crate::application::spec_registry::SpecRegistry;
use crate::domain::error::WireResult;
use crate::domain::graph::Node;
use crate::domain::specification::Specification;
use crate::infrastructure::rendering::render;
use crate::infrastructure::storage::SqliteStorage;

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
pub fn wire_init(input: WireInitInput, storage: &SqliteStorage) -> WireResult<WireInitOutput> {
    let spec_reg = SpecRegistry::new(storage);
    let proj_reg = ProjectionRegistry::new(storage);

    let mut projections = Vec::new();
    let mut warnings = Vec::new();

    for name in proj_reg.list()? {
        let Some(proj) = proj_reg.get(&name)? else {
            continue;
        };
        let Some(spec) = spec_reg.get(&proj.spec_ref)? else {
            warnings.push(format!(
                "projection '{name}': spec_ref '{}' not registered",
                proj.spec_ref
            ));
            continue;
        };

        let matched = collect_matching_nodes(storage, &spec)?;
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
        let data = serde_json::json!({
            "count": matched.len(),
            "names": names.join(", "),
            "nodes": nodes_json,
            "persona_id": input.persona_id,
        });
        let rendered = render(proj.target_form, &proj.template, &data);
        projections.push(RenderedProjection {
            name: proj.name,
            target_form: proj.target_form,
            rendered,
        });
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
    /// `Some(["active", "ng"])` で該当 axis のみ render、 `None` で全 axis。
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

/// 各 axis 1 件の Phase 1 sync collect 結果。
struct CollectedAxis {
    axis: String,
    source_uri: String,
    target_form: TargetForm,
    template: String,
}

/// 全 builtin axis (or projection_names で subset) を iterate し、 各 axis の
/// **配線 (source_uri)** を **wire DB の wiring entry `<persona>.<axis>`** から取得、
/// **template** を 3 段優先 (1: persona-pack overlay × `MergeStrategy.merge` / 2: wire
/// DB の動的 register projection `<persona>.section.<axis>` / 3: `BUILTIN_PROJECTIONS`)
/// で解決して Adapter で fresh fetch + render し、 全 axis を concat した
/// **PromptContext** を 1 call で return する `/wake` 用 entry。
///
/// 設計確定 (2026-06-16 reframe):
/// - 配線 SoT = **wire DB wiring entry**。 persona-pack には書かない (= 二重管理 drift 防止)
/// - persona-pack `[extra.persona_wire.projections.<axis>]` は **Projection template の
///   Overlay only** (persona 固有 emote / register 等を `MergeStrategy` 指定で被せる)
/// - `projection_names: Some([...])` で subset 指定可能 (= 動的 Selection)
pub async fn wire_prompt_context(
    input: WirePromptContextInput,
    storage: std::sync::Arc<std::sync::Mutex<SqliteStorage>>,
) -> WireResult<WirePromptContextOutput> {
    use crate::application::persona_pack_resolver::read_projection_overlays;

    // ---- Phase 0: persona-pack overlay 解決 (best-effort、 不在は空 BTreeMap) ----
    let overlays = read_projection_overlays(&input.persona_id)?.unwrap_or_default();

    // ---- Phase 1: sync collect (MutexGuard を await 跨がない form) ----
    //   axis list = projection_names で subset 指定があればそれ、
    //               None なら wire DB の wiring entry (= persona-scoped Node) を
    //               spec query で全件取得し metadata.axis を抽出する。
    let mut warnings = Vec::new();
    let collected: Vec<CollectedAxis> = {
        let s = storage.lock().map_err(|_| {
            crate::domain::error::WireError::Storage("storage mutex poisoned".to_string())
        })?;
        let proj_reg = ProjectionRegistry::new(&s);

        let axes: Vec<String> = if let Some(names) = input.projection_names.as_ref() {
            names.clone()
        } else {
            // wire DB から persona の wiring entry 全件 spec query → axis 抽出
            let spec = Specification::And(vec![
                Specification::TypeIs("outline_node".to_string()),
                Specification::MetadataEq {
                    path: "persona".to_string(),
                    value: serde_json::json!(input.persona_id),
                },
            ]);
            let nodes = collect_matching_nodes(&s, &spec)?;
            nodes
                .iter()
                .filter_map(|n| {
                    n.metadata
                        .get("axis")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        };

        let mut out: Vec<CollectedAxis> = Vec::new();
        for axis in &axes {
            // 配線 SoT = wire DB Node `<persona>.<axis>` の metadata.source_uri
            let node_id = format!("{}.{}", input.persona_id, axis);
            let Some(node) = s.get_node(&node_id)? else {
                continue; // 未配線 = silent skip
            };
            let Some(source_uri) = node.metadata.get("source_uri").and_then(|v| v.as_str()) else {
                warnings.push(format!(
                    "wiring entry '{node_id}' lacks metadata.source_uri — axis skipped"
                ));
                continue;
            };

            // base template = wire DB 動的 register `<persona>.section.<axis>` のみ。
            //                 不在は skip + warning (= builtin hardcode 廃止)。
            let projection_name = format!("{}.section.{}", input.persona_id, axis);
            let (base_template, base_target) = match proj_reg.get(&projection_name)? {
                Some(proj) => (proj.template, proj.target_form),
                None => {
                    warnings.push(format!(
                        "axis '{axis}' has no registered projection \
                         '{projection_name}' — axis skipped"
                    ));
                    continue;
                }
            };

            // overlay merge (MergeStrategy 経由)
            let (final_template, final_target) = if let Some(o) = overlays.get(axis) {
                (o.strategy.merge(&base_template, &o.template), o.target_form)
            } else {
                (base_template, base_target)
            };

            out.push(CollectedAxis {
                axis: axis.clone(),
                source_uri: source_uri.to_string(),
                target_form: final_target,
                template: final_template,
            });
        }
        out
    };

    // ---- Phase 2: async fetch + render (Adapter 経由) ----
    let mut projections = Vec::new();
    for c in collected {
        let fetched = match crate::infrastructure::adapter::fetch_via_adapter(&c.source_uri).await {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!(
                    "adapter fetch failed for axis '{}' (uri={}): {e}",
                    c.axis, c.source_uri
                ));
                serde_json::Value::Null
            }
        };
        let entries = vec![serde_json::json!({
            "wiring_entry": {
                "axis": c.axis,
                "source_uri": c.source_uri,
            },
            "fetched_data": fetched,
        })];
        let data = serde_json::json!({
            "count": 1,
            "axis": c.axis,
            "entries": entries,
            "persona_id": input.persona_id,
        });
        let rendered = render(c.target_form, &c.template, &data);
        projections.push(RenderedProjection {
            name: format!("{}.section.{}", input.persona_id, c.axis),
            target_form: c.target_form,
            rendered,
        });
    }

    // ---- Phase 3: concat (= PromptContext として 1 string) ----
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

/// Walk every node type and tally totals + orphan count (nodes with no
/// in- or out-edges). Shared scan primitive for `wire_close` / `wire_doctor`;
/// P3 daemon will extend this with stale / asymmetric / high-fanout checks.
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
            if out_edges.is_empty() && in_edges.is_empty() {
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
         - orphan nodes (0 in + 0 out): {orphan}\n",
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

pub struct WireDoctorOutput {
    pub orphan_node_count: usize,
    pub total_node_count: usize,
    pub total_edge_count: usize,
    pub report_markdown: String,
}

/// Graph-wide health diagnostic. P2a scope: orphan count + totals only
/// (= same scan as `wire_close`, but not persona-scoped + framed as a
/// standalone health check). Future expansion (stale / asymmetric /
/// high-fanout) carried to P3 daemon.
pub fn wire_doctor(storage: &SqliteStorage) -> WireResult<WireDoctorOutput> {
    let summary = graph_scan_summary(storage)?;
    let report_markdown = format!(
        "# wire_doctor report\n\n\
         - total nodes: {total_nodes}\n\
         - total edges: {total_edges}\n\
         - orphan nodes (0 in + 0 out): {orphan}\n",
        total_nodes = summary.total_node_count,
        total_edges = summary.total_edge_count,
        orphan = summary.orphan_node_count,
    );

    Ok(WireDoctorOutput {
        orphan_node_count: summary.orphan_node_count,
        total_node_count: summary.total_node_count,
        total_edge_count: summary.total_edge_count,
        report_markdown,
    })
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
        (None, Some(name)) => SpecRegistry::new(storage)
            .get(name)?
            .ok_or_else(|| crate::domain::error::WireError::NotFound(format!("spec: {name}")))?,
        (Some(_), Some(_)) => {
            return Err(crate::domain::error::WireError::InvalidSpec(
                "spec and spec_ref are mutually exclusive".into(),
            ));
        }
        (None, None) => {
            return Err(crate::domain::error::WireError::InvalidSpec(
                "either spec or spec_ref is required".into(),
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
) -> WireResult<WireRenderOutput> {
    let proj = ProjectionRegistry::new(storage)
        .get(&input.projection_ref)?
        .ok_or_else(|| {
            crate::domain::error::WireError::NotFound(format!(
                "projection: {}",
                input.projection_ref
            ))
        })?;
    let spec = SpecRegistry::new(storage)
        .get(&proj.spec_ref)?
        .ok_or_else(|| {
            crate::domain::error::WireError::NotFound(format!(
                "spec_ref (dangling): {}",
                proj.spec_ref
            ))
        })?;
    let matched = collect_matching_nodes(storage, &spec)?;
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
    let data = serde_json::json!({
        "count": matched.len(),
        "names": names.join(", "),
        "nodes": nodes_json,
    });
    let rendered = render(proj.target_form, &proj.template, &data);
    Ok(WireRenderOutput {
        name: proj.name,
        target_form: proj.target_form,
        rendered,
    })
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

/// Delete a node by id. Edges are not cascade-deleted; surviving edges referencing
/// the removed id become dangling — wire_doctor surfaces them on the next scan.
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

const WORKFLOW_TYPE: &str = "workflow_def";
const TRIGGER_KINDS_P5A: &[&str] = &["on_demand", "on_event"];
const ACTION_KINDS_P5A: &[&str] = &["no_op", "emit_projection"];

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

/// Register a Workflow as a `workflow_def` Node. Validates the trigger /
/// action shape (P5-a kind subset) and stores `{persona, trigger, action,
/// enabled}` in `metadata`. Implementation = thin wrapper around
/// `storage.insert_node` so observability via `wire_query({TypeIs:
/// "workflow_def"})` works out of the box.
pub fn wire_workflow_register(
    input: WireWorkflowRegisterInput,
    storage: &SqliteStorage,
) -> WireResult<WireWorkflowRegisterOutput> {
    let trigger_kind = read_kind(&input.trigger, "trigger")?;
    if !TRIGGER_KINDS_P5A.contains(&trigger_kind.as_str()) {
        return Err(crate::domain::error::WireError::InvalidSpec(format!(
            "trigger.kind '{trigger_kind}' not supported in P5-a (allowed: {:?})",
            TRIGGER_KINDS_P5A
        )));
    }
    if trigger_kind == "on_event" {
        require_string_field(&input.trigger, "event", "trigger.event")?;
    }

    let action_kind = read_kind(&input.action, "action")?;
    if !ACTION_KINDS_P5A.contains(&action_kind.as_str()) {
        return Err(crate::domain::error::WireError::InvalidSpec(format!(
            "action.kind '{action_kind}' not supported in P5-a (allowed: {:?})",
            ACTION_KINDS_P5A
        )));
    }
    if action_kind == "emit_projection" {
        let names = input
            .action
            .get("projection_names")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                crate::domain::error::WireError::InvalidSpec(
                    "action.projection_names (array) is required for action.kind \
                     'emit_projection'"
                        .to_string(),
                )
            })?;
        if names.is_empty() {
            return Err(crate::domain::error::WireError::InvalidSpec(
                "action.projection_names must contain at least one axis name".to_string(),
            ));
        }
        for n in names {
            if !n.is_string() {
                return Err(crate::domain::error::WireError::InvalidSpec(
                    "action.projection_names entries must all be strings".to_string(),
                ));
            }
        }
    }

    let mut metadata = serde_json::Map::new();
    if let Some(p) = input.persona_id.as_ref() {
        metadata.insert("persona".to_string(), serde_json::json!(p));
    }
    metadata.insert("trigger".to_string(), input.trigger);
    metadata.insert("action".to_string(), input.action);
    metadata.insert(
        "enabled".to_string(),
        serde_json::json!(input.enabled.unwrap_or(true)),
    );

    let node = Node {
        id: input.id.clone(),
        r#type: WORKFLOW_TYPE.to_string(),
        sot_ref: None,
        confidence: None,
        applicability: None,
        last_verified_at: None,
        review_due: None,
        version: 1,
        prev_id: None,
        metadata: serde_json::Value::Object(metadata),
    };
    storage.insert_node(&node)?;
    Ok(WireWorkflowRegisterOutput { id: input.id })
}

fn read_kind(value: &serde_json::Value, label: &str) -> WireResult<String> {
    value
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            crate::domain::error::WireError::InvalidSpec(format!(
                "{label}.kind (string) is required"
            ))
        })
}

fn require_string_field(value: &serde_json::Value, field: &str, label: &str) -> WireResult<String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            crate::domain::error::WireError::InvalidSpec(format!("{label} (string) is required"))
        })
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
        return Err(crate::domain::error::WireError::InvalidSpec(
            "exactly one of `id` or `event` is required".to_string(),
        ));
    }
    let dry_run = input.dry_run.unwrap_or(false);

    // Collect candidate workflows.
    let candidates: Vec<WorkflowSummary> = if let Some(id) = input.id.as_ref() {
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
        vec![node_to_summary(node)?]
    } else {
        // event-driven: match every on_event workflow whose trigger.event == event
        let spec = Specification::TypeIs(WORKFLOW_TYPE.to_string());
        collect_matching_nodes(storage, &spec)?
            .into_iter()
            .filter_map(|n| node_to_summary(n).ok())
            .collect()
    };

    let mut fired = Vec::new();
    let mut skipped = Vec::new();
    let event = input.event.as_deref();

    for w in candidates {
        if !w.enabled {
            skipped.push((w.id.clone(), "enabled=false".to_string()));
            continue;
        }
        if let Some(persona_filter) = input.persona_id.as_ref() {
            if w.persona_id.as_deref() != Some(persona_filter.as_str()) {
                skipped.push((
                    w.id.clone(),
                    format!("persona scope mismatch (want={persona_filter})"),
                ));
                continue;
            }
        }
        // Trigger gating
        let trigger_kind = w
            .trigger
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(ev) = event {
            // event-driven fire path: skip non-on_event workflows
            if trigger_kind != "on_event" {
                skipped.push((
                    w.id.clone(),
                    format!("trigger.kind='{trigger_kind}' does not match event fan-out"),
                ));
                continue;
            }
            let wf_event = w.trigger.get("event").and_then(|v| v.as_str()).unwrap_or("");
            if wf_event != ev {
                skipped.push((
                    w.id.clone(),
                    format!("trigger.event='{wf_event}' != '{ev}'"),
                ));
                continue;
            }
        }
        // Resolve action
        let action_kind = w
            .action
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let action_emit_projection_names = if action_kind == "emit_projection" {
            w.action
                .get("projection_names")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
        } else {
            None
        };
        fired.push(ResolvedFire {
            id: w.id,
            persona_id: w.persona_id,
            action_kind,
            action_emit_projection_names,
            dry_run,
        });
    }

    Ok(WireWorkflowFireOutput { fired, skipped })
}

// ---- wire_workflow_check (P5-a' audit sibling) -------------------------
//
// `docs/wire-workflow-spec.md §6.5` — 「宣言された maintenance plan と実配線
// (workflow_def + spec) の差分」 を audit する read tool。 onboarding §6b
// workflow が「発火 path」 だったのに対し、 本 check は「発火 path が宣言通り
// に張れているかの coverage」 を担う sibling。
//
// Coverage semantic (= 初期 scope サックリ版):
//   workflow が Node X を cover する iff
//     workflow.action.kind == "emit_projection"
//   AND workflow.metadata.persona == X.metadata.persona
//   AND X.metadata.axis ∈ workflow.action.projection_names
//
// = 既存 wire_prompt_context の Node iteration semantic (= persona scope
// + axis 名 match) と整合させている。 完全な Spec 評価は将来 carry。
//
// Stale 検出 (verified_at / cadence 経過判定) は P5-e carry。

#[derive(Debug)]
pub struct WireWorkflowCheckInput {
    pub persona_id: Option<String>,
    pub include_exempt: Option<bool>,
    pub include_covered: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct UncoveredNode {
    pub node_id: String,
    pub r#type: String,
    pub persona: Option<String>,
    pub axis: Option<String>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct UndeclaredNode {
    pub node_id: String,
    pub r#type: String,
    pub persona: Option<String>,
    pub axis: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExemptNode {
    pub node_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CoveredNode {
    pub node_id: String,
    pub axis: Option<String>,
    pub covering_workflow_id: String,
}

#[derive(Debug)]
pub struct WireWorkflowCheckOutput {
    pub total_nodes: usize,
    pub declared_covered_count: usize,
    pub declared_covered: Vec<CoveredNode>, // populated only if include_covered=true
    pub declared_uncovered: Vec<UncoveredNode>,
    pub undeclared: Vec<UndeclaredNode>,
    pub exempt: Vec<ExemptNode>, // populated only if include_exempt=true
    pub workflows_observed: usize,
}

/// Audit graph coverage: for every Node (optionally scoped to a persona),
/// classify into declared_covered / declared_uncovered / undeclared / exempt
/// based on `metadata.maintained_by` / `metadata.maintenance_exempt` and
/// the set of registered workflow_def Nodes.
pub fn wire_workflow_check(
    input: WireWorkflowCheckInput,
    storage: &SqliteStorage,
) -> WireResult<WireWorkflowCheckOutput> {
    let include_exempt = input.include_exempt.unwrap_or(false);
    let include_covered = input.include_covered.unwrap_or(false);

    // Collect all workflows (enabled only — disabled ones don't count as coverage).
    let wf_spec = Specification::TypeIs(WORKFLOW_TYPE.to_string());
    let workflow_nodes = collect_matching_nodes(storage, &wf_spec)?;
    let workflows: Vec<WorkflowSummary> = workflow_nodes
        .into_iter()
        .filter_map(|n| node_to_summary(n).ok())
        .filter(|w| w.enabled)
        .collect();
    let workflows_observed = workflows.len();

    // Collect candidate Nodes — every Node *except* workflow_def. Spec AST
    // has no NotTypeIs leaf, so we sweep type_registry (kind=node) and union
    // collect_matching_nodes(TypeIs=<t>) for each non-workflow type. Stable
    // across schema growth (new types auto-picked up).
    let mut all_nodes: Vec<Node> = Vec::new();
    for t in storage.list_types_by_kind("node")? {
        if t == WORKFLOW_TYPE {
            continue;
        }
        all_nodes.extend(collect_matching_nodes(storage, &Specification::TypeIs(t))?);
    }
    // Apply optional persona scope.
    let candidate_nodes: Vec<Node> = all_nodes
        .into_iter()
        .filter(|n| {
            if let Some(p) = input.persona_id.as_ref() {
                n.metadata.get("persona").and_then(|v| v.as_str()) == Some(p.as_str())
            } else {
                true
            }
        })
        .collect();
    let total_nodes = candidate_nodes.len();

    let mut declared_covered: Vec<CoveredNode> = Vec::new();
    let mut declared_covered_count: usize = 0;
    let mut declared_uncovered: Vec<UncoveredNode> = Vec::new();
    let mut undeclared: Vec<UndeclaredNode> = Vec::new();
    let mut exempt: Vec<ExemptNode> = Vec::new();

    for n in candidate_nodes {
        let persona = n
            .metadata
            .get("persona")
            .and_then(|v| v.as_str())
            .map(String::from);
        let axis = n
            .metadata
            .get("axis")
            .and_then(|v| v.as_str())
            .map(String::from);
        let node_id = n.id.clone();
        let node_type = n.r#type.clone();

        // Exempt path
        if n.metadata
            .get("maintenance_exempt")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let reason = n
                .metadata
                .get("maintenance_exempt_reason")
                .and_then(|v| v.as_str())
                .map(String::from);
            exempt.push(ExemptNode { node_id, reason });
            continue;
        }

        // Declared path
        let maintained_by = n.metadata.get("maintained_by");
        if let Some(mb) = maintained_by {
            let declared_event = mb.get("event").and_then(|v| v.as_str()).map(String::from);
            let declared_workflow_ref = mb
                .get("workflow_ref")
                .and_then(|v| v.as_str())
                .map(String::from);

            // Find candidate covering workflows
            let mut covering: Option<&WorkflowSummary> = None;
            let mut reasons: Vec<String> = Vec::new();

            for w in &workflows {
                // Explicit workflow_ref takes priority
                if let Some(want_ref) = declared_workflow_ref.as_ref() {
                    if &w.id != want_ref {
                        continue;
                    }
                } else if let Some(want_event) = declared_event.as_ref() {
                    // Match via event
                    let kind = w.trigger.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    if kind != "on_event" {
                        continue;
                    }
                    let ev = w.trigger.get("event").and_then(|v| v.as_str()).unwrap_or("");
                    if ev != want_event {
                        continue;
                    }
                } else {
                    // No event / workflow_ref declared — cannot match
                    continue;
                }
                // Check action covers this Node (= emit_projection + axis match + persona match)
                let action_kind = w.action.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if action_kind != "emit_projection" {
                    continue;
                }
                if w.persona_id != persona {
                    continue;
                }
                let axis_match = w
                    .action
                    .get("projection_names")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .any(|a| Some(a) == axis.as_deref())
                    })
                    .unwrap_or(false);
                if !axis_match {
                    continue;
                }
                covering = Some(w);
                break;
            }

            if let Some(w) = covering {
                declared_covered_count += 1;
                if include_covered {
                    declared_covered.push(CoveredNode {
                        node_id,
                        axis,
                        covering_workflow_id: w.id.clone(),
                    });
                }
                continue;
            }

            // Diagnose why not covered.
            if let Some(want_ref) = declared_workflow_ref.as_ref() {
                if !workflows.iter().any(|w| &w.id == want_ref) {
                    reasons.push(format!(
                        "maintained_by.workflow_ref='{want_ref}' not found among enabled workflow_def"
                    ));
                } else {
                    reasons.push(format!(
                        "workflow '{want_ref}' exists but its action does not cover this Node \
                         (persona/axis/projection_names mismatch)"
                    ));
                }
            } else if let Some(want_event) = declared_event.as_ref() {
                reasons.push(format!(
                    "no enabled on_event workflow with trigger.event='{want_event}' covers \
                     persona='{persona}' axis='{axis}'",
                    persona = persona.as_deref().unwrap_or("<none>"),
                    axis = axis.as_deref().unwrap_or("<none>")
                ));
            } else {
                reasons.push(
                    "maintained_by present but neither event nor workflow_ref declared"
                        .to_string(),
                );
            }

            declared_uncovered.push(UncoveredNode {
                node_id,
                r#type: node_type,
                persona,
                axis,
                reasons,
            });
            continue;
        }

        // Undeclared (no maintained_by, no exempt)
        undeclared.push(UndeclaredNode {
            node_id,
            r#type: node_type,
            persona,
            axis,
        });
    }

    let exempt_out = if include_exempt {
        exempt
    } else {
        Vec::new()
    };
    Ok(WireWorkflowCheckOutput {
        total_nodes,
        declared_covered_count,
        declared_covered,
        declared_uncovered,
        undeclared,
        exempt: exempt_out,
        workflows_observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::projection_registry::NamedProjection;
    use crate::domain::graph::{Edge, Node};
    use serde_json::json;

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
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
            .register(&NamedProjection {
                name: "_persona_toc".into(),
                spec_ref: "active_personas".into(),
                template: "Personas ({{count}}): {{names}}".into(),
                target_form: TargetForm::Prompt,
            })
            .unwrap();

        let out = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
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
            .register(&NamedProjection {
                name: "broken".into(),
                spec_ref: "no_such_spec".into(),
                template: "x".into(),
                target_form: TargetForm::Prompt,
            })
            .unwrap();
        let out = wire_init(
            WireInitInput {
                persona_id: "alpha".into(),
            },
            &s,
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
            .contains("orphan nodes (0 in + 0 out): 1"));
        assert!(out.report_markdown.contains("total nodes: 3"));
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
            .register(&NamedProjection {
                name: "doomed".into(),
                spec_ref: "p".into(),
                template: "x".into(),
                target_form: TargetForm::Prompt,
            })
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
                action: json!({"kind":"emit_projection","projection_names":["axis_a","axis_b"]}),
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
            Some(&["axis_a".to_string(), "axis_b".to_string()][..])
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

    // ---- wire_workflow_check (P5-a') tests ----

    fn make_outline_node(id: &str, persona: &str, axis: &str, extra: serde_json::Value) -> Node {
        let mut meta = serde_json::json!({
            "persona": persona,
            "axis": axis,
            "source_uri": format!("file:~/test/{id}")
        });
        if let serde_json::Value::Object(ref mut m) = meta {
            if let serde_json::Value::Object(extra_map) = extra {
                for (k, v) in extra_map {
                    m.insert(k, v);
                }
            }
        }
        Node {
            id: id.into(),
            r#type: "outline_node".into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: meta,
        }
    }

    fn register_emit_workflow(s: &SqliteStorage, id: &str, persona: &str, event: &str, axes: &[&str]) {
        let axes_json = serde_json::Value::Array(
            axes.iter().map(|a| serde_json::json!(a)).collect(),
        );
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: id.into(),
                persona_id: Some(persona.into()),
                trigger: json!({"kind":"on_event","event":event}),
                action: json!({"kind":"emit_projection","projection_names": axes_json}),
                enabled: None,
            },
            s,
        )
        .unwrap();
    }

    #[test]
    fn workflow_check_buckets_a_node_as_declared_covered_when_workflow_matches() {
        let s = setup();
        register_emit_workflow(&s, "alpha.workflow.close", "alpha", "session_close", &["handoff"]);
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({"maintained_by": {"event": "session_close"}}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: Some(true),
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.total_nodes, 1);
        assert_eq!(out.declared_covered_count, 1);
        assert_eq!(out.declared_covered.len(), 1);
        assert_eq!(out.declared_covered[0].covering_workflow_id, "alpha.workflow.close");
        assert!(out.declared_uncovered.is_empty());
        assert!(out.undeclared.is_empty());
    }

    #[test]
    fn workflow_check_flags_declared_uncovered_when_event_has_no_matching_workflow() {
        let s = setup();
        // workflow exists but for a *different* event
        register_emit_workflow(&s, "alpha.workflow.open", "alpha", "session_open", &["handoff"]);
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({"maintained_by": {"event": "session_close"}}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.declared_uncovered.len(), 1);
        assert!(out.declared_uncovered[0].reasons[0].contains("session_close"));
    }

    #[test]
    fn workflow_check_flags_declared_uncovered_when_workflow_ref_missing() {
        let s = setup();
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({"maintained_by": {"workflow_ref": "alpha.workflow.ghost"}}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.declared_uncovered.len(), 1);
        assert!(out.declared_uncovered[0].reasons[0].contains("ghost"));
        assert!(out.declared_uncovered[0].reasons[0].contains("not found"));
    }

    #[test]
    fn workflow_check_flags_declared_uncovered_when_axis_not_in_projection_names() {
        let s = setup();
        // Workflow covers axis "active", but Node declares axis "handoff"
        register_emit_workflow(&s, "alpha.workflow.close", "alpha", "session_close", &["active"]);
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({"maintained_by": {"event": "session_close"}}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.declared_uncovered.len(), 1);
    }

    #[test]
    fn workflow_check_classifies_node_without_maintained_by_as_undeclared() {
        let s = setup();
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.undeclared.len(), 1);
        assert_eq!(out.undeclared[0].node_id, "alpha.handoff");
        assert!(out.declared_covered.is_empty());
        assert!(out.declared_uncovered.is_empty());
    }

    #[test]
    fn workflow_check_classifies_exempt_node_and_returns_only_when_include_exempt() {
        let s = setup();
        s.insert_node(&make_outline_node(
            "alpha.static_sot",
            "alpha",
            "static_sot",
            json!({"maintenance_exempt": true, "maintenance_exempt_reason": "external static SoT"}),
        ))
        .unwrap();
        // include_exempt = false → list empty
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert!(out.exempt.is_empty());
        assert!(out.undeclared.is_empty());
        // include_exempt = true → returned
        let out2 = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: Some(true),
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out2.exempt.len(), 1);
        assert_eq!(out2.exempt[0].reason.as_deref(), Some("external static SoT"));
    }

    #[test]
    fn workflow_check_skips_disabled_workflows_when_resolving_coverage() {
        let s = setup();
        // Workflow exists but disabled
        wire_workflow_register(
            WireWorkflowRegisterInput {
                id: "alpha.workflow.close".into(),
                persona_id: Some("alpha".into()),
                trigger: json!({"kind":"on_event","event":"session_close"}),
                action: json!({"kind":"emit_projection","projection_names":["handoff"]}),
                enabled: Some(false),
            },
            &s,
        )
        .unwrap();
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({"maintained_by": {"event": "session_close"}}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.workflows_observed, 0);
        assert_eq!(out.declared_uncovered.len(), 1);
    }

    #[test]
    fn workflow_check_persona_scope_filters_other_personas() {
        let s = setup();
        s.insert_node(&make_outline_node("alpha.h", "alpha", "handoff", json!({}))).unwrap();
        s.insert_node(&make_outline_node("beta.h", "beta", "handoff", json!({}))).unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: Some("alpha".into()),
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        assert_eq!(out.total_nodes, 1);
        assert_eq!(out.undeclared.len(), 1);
        assert_eq!(out.undeclared[0].node_id, "alpha.h");
    }

    #[test]
    fn workflow_check_excludes_workflow_def_nodes_from_total() {
        let s = setup();
        register_emit_workflow(&s, "alpha.workflow.x", "alpha", "session_close", &["handoff"]);
        s.insert_node(&make_outline_node(
            "alpha.handoff",
            "alpha",
            "handoff",
            json!({"maintained_by": {"event": "session_close"}}),
        ))
        .unwrap();
        let out = wire_workflow_check(
            WireWorkflowCheckInput {
                persona_id: None,
                include_exempt: None,
                include_covered: None,
            },
            &s,
        )
        .unwrap();
        // total_nodes counts the outline_node only (not the workflow_def)
        assert_eq!(out.total_nodes, 1);
        assert_eq!(out.workflows_observed, 1);
    }

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
}
