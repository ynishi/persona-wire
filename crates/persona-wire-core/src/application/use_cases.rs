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
/// the rendered context bundle. Used as the `/wake` auto-call entry per
/// concept-doc §3 Application layer (wire_init flow).
pub fn wire_init(input: WireInitInput, storage: &SqliteStorage) -> WireResult<WireInitOutput> {
    let spec_reg = SpecRegistry::new(storage);
    let proj_reg = ProjectionRegistry::new(storage);

    let mut projections = Vec::new();
    let mut warnings = Vec::new();

    for name in proj_reg.list()? {
        let Some(proj) = proj_reg.get(&name)? else {
            // Race: row deleted between list() and get(). Skip silently.
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
        let data = serde_json::json!({
            "count": matched.len(),
            "names": names.join(", "),
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
}
