//! Bundle install use case — TOML parse → name resolution → registry
//! dispatch → install report → install log append.
//!
//! Consumed by `wire_bundle_install` (MCP / CLI surface). The Bundle
//! [`BundleRegistry`](crate::application::bundle_registry::BundleRegistry)
//! owns CRUD on the `bundles` table; this module owns the parse +
//! dispatch flow that turns one Bundle row into many registry writes.
//!
//! # Sections handled (v1)
//!
//! - `[[specs]]` → [`SpecRegistry`]
//! - `[[projections]]` → [`ProjectionRegistry`]
//! - `[[nodes]]` → `SqliteStorage::insert_node`
//! - `[[edges]]` → `SqliteStorage::insert_edge`
//!
//! `[[wirings]]` / `[[workflows]]` dispatch is the same shape — handled
//! through the existing `wire_workflow_register` flow when wired up at
//! the MCP surface. The install report carries per-entity rows so each
//! section can be extended without changing the public report shape.
//!
//! # Conflict resolution
//!
//! Name conflict policy is selected per-install via
//! [`ConflictMode`](crate::domain::entity::bundle::ConflictMode):
//!
//! - `Increment` (default) — entity name auto-increments (`-1` / `-2` ...)
//!   until a free slot is found. Internal references inside the same
//!   bundle (e.g. `projections.spec_ref` pointing at `specs.name`) are
//!   rewritten to the final name.
//! - `Skip` — leave the existing entity, record the collision in the
//!   install report's `skipped[]`.
//! - `Error` — abort on first collision. Nothing is written.
//!
//! # Atomicity
//!
//! v1 is **non-transactional** — dispatch iterates section-by-section
//! against the registries. Failures partway through leave previously
//! installed entities in place; the install report's `errors[]` lists
//! the boundary. SQLite transaction wrapping is a follow-up carry.

use serde::Deserialize;

use crate::application::projection_registry::ProjectionRegistry;
use crate::application::spec_registry::SpecRegistry;
use crate::domain::entity::bundle::{
    Bundle, BundleId, BundleInstallReport, ConflictMode, ErrorItem, InstalledItem, SkippedItem,
};
use crate::domain::entity::projection::{
    PluginDispatch, Projection, ProjectionName, ProjectionTemplate, SpecName, TargetForm,
};
use crate::domain::error::{DomainError, WireError, WireResult};
use crate::domain::graph::Ulid;
use crate::domain::specification::Specification;
use crate::infrastructure::storage::SqliteStorage;

// -- TOML manifest deserialization ------------------------------------------

/// Top-level TOML manifest deserialized from `Bundle.body`. All section
/// arrays are optional; an empty bundle (header only) parses successfully
/// and dispatches as a no-op.
#[derive(Debug, Deserialize)]
pub struct BundleManifest {
    #[serde(default)]
    pub specs: Vec<SpecEntry>,
    #[serde(default)]
    pub projections: Vec<ProjectionEntry>,
    #[serde(default)]
    pub nodes: Vec<NodeEntry>,
    #[serde(default)]
    pub edges: Vec<EdgeEntry>,
    #[serde(default)]
    pub wirings: Vec<WiringEntry>,
    #[serde(default)]
    pub workflows: Vec<WorkflowEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SpecEntry {
    pub name: String,
    /// Specification body in `toml::Value` form. The install dispatcher
    /// round-trips this through `serde_json::Value` → [`Specification`]
    /// so TOML callers write the existing externally-tagged serde shape,
    /// e.g. `spec = { TypeIs = "persona" }` or
    /// `spec = { MetadataEq = { path = "owner", value = "owner_a" } }`.
    pub spec: toml::Value,
}

#[derive(Debug, Deserialize)]
pub struct ProjectionEntry {
    pub name: String,
    pub spec_ref: String,
    pub template: String,
    /// Lowercase by convention (`prompt` / `markdown` / `json` / `ascii`),
    /// but parsing is case-insensitive at the dispatch boundary.
    pub target_form: String,
}

#[derive(Debug, Deserialize)]
pub struct NodeEntry {
    pub name: String,
    pub node_type: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct EdgeEntry {
    pub from_name: String,
    pub to_name: String,
    pub edge_type: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Wiring section entry — one slot binding for one persona.
///
/// Persisted as a `Node` of type `outline_node` whose name is
/// `format!("{persona_id}.{slot}")` and whose metadata carries
/// `persona` / `axis` (= slot) / `source_uri`. Conflict resolution
/// operates on the synthesized node name; the auto-increment suffix
/// goes on the slot half so the persona half stays readable
/// (`alice.mailbox` → `alice.mailbox-1`).
#[derive(Debug, Deserialize)]
pub struct WiringEntry {
    pub persona_id: String,
    pub slot: String,
    pub source_uri: String,
    /// Optional reference to a `ProjectionName` registered elsewhere
    /// (same bundle or pre-existing). Carried into metadata when set;
    /// stored as `metadata.projection_ref`.
    #[serde(default)]
    pub projection_ref: Option<String>,
    /// Optional opt-in for `metadata.maintenance_exempt = true`. The
    /// `wire_doctor` graph axis treats nodes carrying this flag as
    /// self-attached and excludes them from the orphan count (matches
    /// the `wiring_mapper` read-side convention). v0.8.0 dropped the
    /// field at the TOML deserialize boundary because it was missing
    /// from the struct — the v0.8.1 fix surfaces it as a first-class
    /// entry-level knob so a bundle can declare exemption without
    /// hand-writing the `metadata` table.
    #[serde(default)]
    pub maintenance_exempt: Option<bool>,
    /// Optional credential **reference key** (never a secret) carried into
    /// `metadata.auth`. Consumed at fetch time by
    /// `use_cases::render_collected_slot_async`, which merges it into the
    /// `source_uri` as an `?auth=<key>` query param (see
    /// `application::auth` module docs) unless the URI already declares its
    /// own `auth` param.
    #[serde(default)]
    pub auth: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Workflow section entry — mirrors the shape consumed by
/// `wire_workflow_register` so the bundle installer is a thin
/// composition over the existing use case.
#[derive(Debug, Deserialize)]
pub struct WorkflowEntry {
    pub id: String,
    #[serde(default)]
    pub persona_id: Option<String>,
    /// Trigger payload — externally-tagged enum (see
    /// `wire_workflow_register` docs). Pass through TOML as `toml::Value`
    /// and round-trip into `serde_json::Value` at dispatch time.
    pub trigger: toml::Value,
    /// Action payload — externally-tagged enum.
    pub action: toml::Value,
    #[serde(default)]
    pub enabled: Option<bool>,
}

// -- Install entry point ----------------------------------------------------

/// Install the entities declared in `bundle.body` into the storage's
/// registries under the supplied [`ConflictMode`]. The bundle row itself
/// is **not** modified; install history is appended to `bundle_installs`.
pub fn install_bundle(
    bundle: &Bundle,
    mode: ConflictMode,
    storage: &SqliteStorage,
) -> WireResult<BundleInstallReport> {
    let manifest: BundleManifest = toml::from_str(&bundle.body).map_err(|e| {
        WireError::Domain(DomainError::InvalidSpec(format!(
            "bundle TOML parse: {}",
            e
        )))
    })?;

    let install_id = Ulid::new();
    let mut report = BundleInstallReport {
        install_id: install_id.to_string(),
        bundle_id: bundle.id.to_string(),
        mode,
        installed: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };

    // Per-section rename maps. Populated as each section dispatches so
    // later sections can rewrite their references.
    let mut spec_rename: Vec<(String, String)> = Vec::new();
    let mut node_rename: Vec<(String, String)> = Vec::new();

    // ---- specs ----
    let spec_reg = SpecRegistry::new(storage);
    for entry in &manifest.specs {
        // toml::Value → serde_json::Value → Specification, with all errors
        // surfaced as per-entity rows so one malformed spec does not abort
        // the whole install (mirrors target_form parse handling for projections).
        let spec_parsed: Result<Specification, String> = serde_json::to_value(&entry.spec)
            .map_err(|e| format!("toml→json: {}", e))
            .and_then(|v| {
                serde_json::from_value::<Specification>(v).map_err(|e| format!("spec: {}", e))
            });
        let spec = match spec_parsed {
            Ok(s) => s,
            Err(e) => {
                report.errors.push(ErrorItem {
                    kind: "spec".into(),
                    name: entry.name.clone(),
                    error: e,
                });
                continue;
            }
        };
        match resolve_name(&entry.name, mode, |n| Ok(spec_reg.get(n)?.is_some()))? {
            Resolution::Use(final_name) => match spec_reg.register(&final_name, &spec) {
                Ok(id) => {
                    if final_name != entry.name {
                        spec_rename.push((entry.name.clone(), final_name.clone()));
                    }
                    report.installed.push(InstalledItem {
                        kind: "spec".into(),
                        original_name: entry.name.clone(),
                        final_name,
                        id: id.to_string(),
                    });
                }
                Err(e) => report.errors.push(ErrorItem {
                    kind: "spec".into(),
                    name: entry.name.clone(),
                    error: e.to_string(),
                }),
            },
            Resolution::Skip => report.skipped.push(SkippedItem {
                kind: "spec".into(),
                name: entry.name.clone(),
                reason: "name exists (skip mode)".into(),
            }),
            Resolution::Abort => {
                report.errors.push(ErrorItem {
                    kind: "spec".into(),
                    name: entry.name.clone(),
                    error: "name exists (error mode)".into(),
                });
                finalize(&report, install_id, bundle.id, storage)?;
                return Ok(report);
            }
        }
    }

    // ---- projections ----
    let proj_reg = ProjectionRegistry::new(storage);
    for entry in &manifest.projections {
        // Rewrite spec_ref via spec_rename map if it was renamed.
        let resolved_spec_ref = lookup_rename(&spec_rename, &entry.spec_ref);
        match resolve_name(&entry.name, mode, |n| Ok(proj_reg.get(n)?.is_some()))? {
            Resolution::Use(final_name) => {
                match build_projection(
                    &final_name,
                    &resolved_spec_ref,
                    &entry.template,
                    &entry.target_form,
                ) {
                    Ok(proj) => match proj_reg.register(&proj) {
                        Ok(id) => report.installed.push(InstalledItem {
                            kind: "projection".into(),
                            original_name: entry.name.clone(),
                            final_name,
                            id: id.to_string(),
                        }),
                        Err(e) => report.errors.push(ErrorItem {
                            kind: "projection".into(),
                            name: entry.name.clone(),
                            error: e.to_string(),
                        }),
                    },
                    Err(e) => report.errors.push(ErrorItem {
                        kind: "projection".into(),
                        name: entry.name.clone(),
                        error: e.to_string(),
                    }),
                }
            }
            Resolution::Skip => report.skipped.push(SkippedItem {
                kind: "projection".into(),
                name: entry.name.clone(),
                reason: "name exists (skip mode)".into(),
            }),
            Resolution::Abort => {
                report.errors.push(ErrorItem {
                    kind: "projection".into(),
                    name: entry.name.clone(),
                    error: "name exists (error mode)".into(),
                });
                finalize(&report, install_id, bundle.id, storage)?;
                return Ok(report);
            }
        }
    }

    // ---- nodes ----
    for entry in &manifest.nodes {
        match resolve_name(&entry.name, mode, |n| {
            Ok(storage.lookup_node_id_by_name(n)?.is_some())
        })? {
            Resolution::Use(final_name) => {
                let node = build_node(&final_name, &entry.node_type, &entry.metadata);
                match storage.insert_node(&node) {
                    Ok(()) => {
                        if final_name != entry.name {
                            node_rename.push((entry.name.clone(), final_name.clone()));
                        }
                        report.installed.push(InstalledItem {
                            kind: "node".into(),
                            original_name: entry.name.clone(),
                            final_name,
                            id: node.id.to_string(),
                        });
                    }
                    Err(e) => report.errors.push(ErrorItem {
                        kind: "node".into(),
                        name: entry.name.clone(),
                        error: e.to_string(),
                    }),
                }
            }
            Resolution::Skip => report.skipped.push(SkippedItem {
                kind: "node".into(),
                name: entry.name.clone(),
                reason: "name exists (skip mode)".into(),
            }),
            Resolution::Abort => {
                report.errors.push(ErrorItem {
                    kind: "node".into(),
                    name: entry.name.clone(),
                    error: "name exists (error mode)".into(),
                });
                finalize(&report, install_id, bundle.id, storage)?;
                return Ok(report);
            }
        }
    }

    // ---- edges ----
    // Edges are name-less at the storage layer (only ULID id), so the
    // `Increment` / `Skip` / `Error` mode does not gate insertion. They
    // only need from/to name resolution against node_rename.
    for entry in &manifest.edges {
        let src_name = lookup_rename(&node_rename, &entry.from_name);
        let tgt_name = lookup_rename(&node_rename, &entry.to_name);
        match build_edge(
            storage,
            &src_name,
            &tgt_name,
            &entry.edge_type,
            &entry.metadata,
        ) {
            Ok(edge) => match storage.insert_edge(&edge) {
                Ok(()) => report.installed.push(InstalledItem {
                    kind: "edge".into(),
                    original_name: format!("{}->{}", entry.from_name, entry.to_name),
                    final_name: format!("{}->{}", src_name, tgt_name),
                    id: edge.id.to_string(),
                }),
                Err(e) => report.errors.push(ErrorItem {
                    kind: "edge".into(),
                    name: format!("{}->{}", entry.from_name, entry.to_name),
                    error: e.to_string(),
                }),
            },
            Err(e) => report.errors.push(ErrorItem {
                kind: "edge".into(),
                name: format!("{}->{}", entry.from_name, entry.to_name),
                error: e.to_string(),
            }),
        }
    }

    // ---- wirings ----
    // Wirings are persisted as `outline_node` Nodes whose name is
    // `format!("{persona}.{slot}")`. Conflict resolution operates on the
    // composite name; auto-increment appends `-N` to the whole synthesized
    // name (`alice.mailbox-1`), the persona / slot field stay verbatim in
    // metadata so the entity carrier keeps its identity even after rename.
    for entry in &manifest.wirings {
        let composed = format!("{}.{}", entry.persona_id, entry.slot);
        match resolve_name(&composed, mode, |n| {
            Ok(storage.lookup_node_id_by_name(n)?.is_some())
        })? {
            Resolution::Use(final_name) => {
                let mut meta = serde_json::Map::new();
                meta.insert(
                    "persona".to_string(),
                    serde_json::Value::String(entry.persona_id.clone()),
                );
                meta.insert(
                    "axis".to_string(),
                    serde_json::Value::String(entry.slot.clone()),
                );
                meta.insert(
                    "source_uri".to_string(),
                    serde_json::Value::String(entry.source_uri.clone()),
                );
                if let Some(p) = &entry.projection_ref {
                    meta.insert(
                        "projection_ref".to_string(),
                        serde_json::Value::String(p.clone()),
                    );
                }
                if let Some(flag) = entry.maintenance_exempt {
                    meta.insert(
                        "maintenance_exempt".to_string(),
                        serde_json::Value::Bool(flag),
                    );
                }
                if let Some(auth) = &entry.auth {
                    meta.insert("auth".to_string(), serde_json::Value::String(auth.clone()));
                }
                if let serde_json::Value::Object(extra) = &entry.metadata {
                    for (k, v) in extra {
                        meta.insert(k.clone(), v.clone());
                    }
                }
                let node = build_node_with_metadata(
                    &final_name,
                    "outline_node",
                    serde_json::Value::Object(meta),
                );
                match storage.insert_node(&node) {
                    Ok(()) => report.installed.push(InstalledItem {
                        kind: "wiring".into(),
                        original_name: composed.clone(),
                        final_name,
                        id: node.id.to_string(),
                    }),
                    Err(e) => report.errors.push(ErrorItem {
                        kind: "wiring".into(),
                        name: composed.clone(),
                        error: e.to_string(),
                    }),
                }
            }
            Resolution::Skip => report.skipped.push(SkippedItem {
                kind: "wiring".into(),
                name: composed,
                reason: "name exists (skip mode)".into(),
            }),
            Resolution::Abort => {
                report.errors.push(ErrorItem {
                    kind: "wiring".into(),
                    name: composed,
                    error: "name exists (error mode)".into(),
                });
                finalize(&report, install_id, bundle.id, storage)?;
                return Ok(report);
            }
        }
    }

    // ---- workflows ----
    // Workflows are dispatched through the existing
    // [`crate::application::use_cases::wire_workflow_register`] flow so all
    // trigger / action invariants are enforced by the Workflow entity,
    // not duplicated here. `id` doubles as the name for conflict
    // resolution (workflow nodes are stored with the id as their Node
    // name).
    for entry in &manifest.workflows {
        let trigger_json = match toml_to_json(&entry.trigger) {
            Ok(v) => v,
            Err(e) => {
                report.errors.push(ErrorItem {
                    kind: "workflow".into(),
                    name: entry.id.clone(),
                    error: format!("trigger: {}", e),
                });
                continue;
            }
        };
        let action_json = match toml_to_json(&entry.action) {
            Ok(v) => v,
            Err(e) => {
                report.errors.push(ErrorItem {
                    kind: "workflow".into(),
                    name: entry.id.clone(),
                    error: format!("action: {}", e),
                });
                continue;
            }
        };

        match resolve_name(&entry.id, mode, |n| {
            Ok(storage.lookup_node_id_by_name(n)?.is_some())
        })? {
            Resolution::Use(final_id) => {
                use crate::application::use_cases::{
                    wire_workflow_register, WireWorkflowRegisterInput,
                };
                let input = WireWorkflowRegisterInput {
                    id: final_id.clone(),
                    persona_id: entry.persona_id.clone(),
                    trigger: trigger_json,
                    action: action_json,
                    enabled: entry.enabled,
                };
                match wire_workflow_register(input, storage) {
                    Ok(out) => report.installed.push(InstalledItem {
                        kind: "workflow".into(),
                        original_name: entry.id.clone(),
                        final_name: final_id,
                        id: out.id,
                    }),
                    Err(e) => report.errors.push(ErrorItem {
                        kind: "workflow".into(),
                        name: entry.id.clone(),
                        error: e.to_string(),
                    }),
                }
            }
            Resolution::Skip => report.skipped.push(SkippedItem {
                kind: "workflow".into(),
                name: entry.id.clone(),
                reason: "name exists (skip mode)".into(),
            }),
            Resolution::Abort => {
                report.errors.push(ErrorItem {
                    kind: "workflow".into(),
                    name: entry.id.clone(),
                    error: "name exists (error mode)".into(),
                });
                finalize(&report, install_id, bundle.id, storage)?;
                return Ok(report);
            }
        }
    }

    finalize(&report, install_id, bundle.id, storage)?;
    Ok(report)
}

// -- helpers ----------------------------------------------------------------

enum Resolution {
    Use(String),
    Skip,
    Abort,
}

/// Resolve `desired` against the registry under the supplied mode.
///
/// `exists(name)` returns `Ok(true)` if the registry already has an entity
/// of that name. For `Increment` mode the function probes
/// `name-1` / `name-2` / ... until a free slot is found.
fn resolve_name<F>(desired: &str, mode: ConflictMode, mut exists: F) -> WireResult<Resolution>
where
    F: FnMut(&str) -> WireResult<bool>,
{
    if !exists(desired)? {
        return Ok(Resolution::Use(desired.to_string()));
    }
    match mode {
        ConflictMode::Skip => Ok(Resolution::Skip),
        ConflictMode::Error => Ok(Resolution::Abort),
        ConflictMode::Increment => {
            for n in 1.. {
                let candidate = format!("{}-{}", desired, n);
                if !exists(&candidate)? {
                    return Ok(Resolution::Use(candidate));
                }
            }
            unreachable!("for-loop with i64 range terminates by exists() success")
        }
    }
}

fn lookup_rename(map: &[(String, String)], original: &str) -> String {
    for (orig, renamed) in map {
        if orig == original {
            return renamed.clone();
        }
    }
    original.to_string()
}

fn build_projection(
    name: &str,
    spec_ref: &str,
    template: &str,
    target_form_raw: &str,
) -> WireResult<Projection> {
    let target_form = match target_form_raw.to_ascii_lowercase().as_str() {
        "prompt" => TargetForm::Prompt,
        "markdown" => TargetForm::Markdown,
        "json" => TargetForm::Json,
        "ascii" => TargetForm::Ascii,
        other => {
            return Err(WireError::Domain(DomainError::InvalidTargetForm(
                other.to_string(),
            )))
        }
    };
    Ok(Projection::new(
        ProjectionName::new(name)?,
        SpecName::new(spec_ref)?,
        ProjectionTemplate::new(template)?,
        target_form,
        PluginDispatch::Default,
    ))
}

fn build_node(
    name: &str,
    node_type: &str,
    metadata: &serde_json::Value,
) -> crate::domain::graph::Node {
    build_node_with_metadata(
        name,
        node_type,
        if metadata.is_null() {
            serde_json::json!({})
        } else {
            metadata.clone()
        },
    )
}

fn build_node_with_metadata(
    name: &str,
    node_type: &str,
    metadata: serde_json::Value,
) -> crate::domain::graph::Node {
    use crate::domain::graph::Node;
    Node {
        id: Ulid::new(),
        name: name.to_string(),
        r#type: node_type.to_string(),
        sot_ref: None,
        confidence: None,
        applicability: None,
        last_verified_at: None,
        review_due: None,
        version: 1,
        prev_id: None,
        metadata,
    }
}

/// Convert a `toml::Value` payload into the `serde_json::Value` shape the
/// existing register use cases consume. Uses `serde_json::to_value` on the
/// `toml::Value`'s Serialize impl so TOML inline tables round-trip into
/// JSON objects, arrays into arrays, scalars 1:1.
fn toml_to_json(v: &toml::Value) -> WireResult<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| WireError::Other(format!("toml→json: {}", e)))
}

fn build_edge(
    storage: &SqliteStorage,
    src_name: &str,
    tgt_name: &str,
    edge_type: &str,
    metadata: &serde_json::Value,
) -> WireResult<crate::domain::graph::Edge> {
    use crate::domain::graph::Edge;
    let src_id = storage
        .lookup_node_id_by_name(src_name)?
        .ok_or_else(|| WireError::Domain(DomainError::NotFound(format!("node:{}", src_name))))?;
    let tgt_id = storage
        .lookup_node_id_by_name(tgt_name)?
        .ok_or_else(|| WireError::Domain(DomainError::NotFound(format!("node:{}", tgt_name))))?;
    Ok(Edge {
        id: Ulid::new(),
        name: None,
        src_node: src_id,
        tgt_node: tgt_id,
        kind: edge_type.to_string(),
        severity: None,
        metadata: if metadata.is_null() {
            serde_json::json!({})
        } else {
            metadata.clone()
        },
        version: 1,
        prev_id: None,
    })
}

fn finalize(
    report: &BundleInstallReport,
    install_id: BundleId,
    bundle_id: BundleId,
    storage: &SqliteStorage,
) -> WireResult<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let report_json = serde_json::to_string(report)
        .map_err(|e| WireError::Other(format!("report serialize: {}", e)))?;
    storage.append_bundle_install(
        install_id,
        bundle_id,
        &report.mode.to_string(),
        now,
        &report_json,
    )
}

// ---- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::bundle_registry::BundleRegistry;
    use crate::domain::entity::bundle::{BundleName, BundleVersion};

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
    }

    fn register_bundle(storage: &SqliteStorage, name: &str, body: &str) -> Bundle {
        let reg = BundleRegistry::new(storage);
        let id = reg
            .register(
                &BundleName::new(name).unwrap(),
                &BundleVersion::new("0.1.0").unwrap(),
                None,
                body,
            )
            .unwrap();
        reg.get_by_id(id).unwrap().unwrap()
    }

    #[test]
    fn install_header_only_bundle_is_noop() {
        let s = setup();
        // `Bundle::new` rejects empty body, so the smallest installable
        // payload is a single-line comment / metadata stub. No section
        // arrays → dispatch is a no-op for all kinds.
        let bundle = register_bundle(&s, "empty", "# header only\n");
        let rpt = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert!(rpt.installed.is_empty());
        assert!(rpt.skipped.is_empty());
        assert!(rpt.errors.is_empty());
    }

    #[test]
    fn install_specs_and_projections_increment_default() {
        let s = setup();
        let body = r###"
[[specs]]
name = "active_personas"
spec = { TypeIs = "persona" }

[[projections]]
name = "personas_overview"
spec_ref = "active_personas"
template = "## Personas\n{{#each nodes}}- {{name}}\n{{/each}}"
target_form = "prompt"
"###;
        let bundle = register_bundle(&s, "quickstart", body);

        // first install
        let r1 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r1.installed.len(), 2);
        assert!(r1.errors.is_empty());
        assert_eq!(r1.installed[0].final_name, "active_personas");
        assert_eq!(r1.installed[1].final_name, "personas_overview");

        // second install — same bundle, increment mode → suffix -1
        let r2 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r2.installed.len(), 2);
        assert_eq!(r2.installed[0].final_name, "active_personas-1");
        assert_eq!(r2.installed[1].final_name, "personas_overview-1");
        // projection spec_ref was rewritten to the renamed spec.
        let proj = ProjectionRegistry::new(&s)
            .get("personas_overview-1")
            .unwrap()
            .unwrap();
        assert_eq!(proj.spec_ref().as_str(), "active_personas-1");

        // third install — suffix -2
        let r3 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r3.installed[0].final_name, "active_personas-2");
        assert_eq!(r3.installed[1].final_name, "personas_overview-2");
    }

    #[test]
    fn install_skip_mode_leaves_existing_alone() {
        let s = setup();
        let body = r#"
[[specs]]
name = "by_owner"
spec = { TypeIs = "persona" }
"#;
        let bundle = register_bundle(&s, "b", body);
        install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        let r = install_bundle(&bundle, ConflictMode::Skip, &s).unwrap();
        assert!(r.installed.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].kind, "spec");
        assert_eq!(r.skipped[0].name, "by_owner");
    }

    #[test]
    fn install_error_mode_aborts_on_collision() {
        let s = setup();
        let body = r#"
[[specs]]
name = "first"
spec = { TypeIs = "persona" }

[[specs]]
name = "second"
spec = { TypeIs = "persona" }
"#;
        let bundle = register_bundle(&s, "b", body);
        install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();

        let r = install_bundle(&bundle, ConflictMode::Error, &s).unwrap();
        // Aborts on first collision; nothing installed, one error row.
        assert!(r.installed.is_empty());
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].name, "first");
        // SpecRegistry still only holds the originals (no second-install side effect).
        let names = SpecRegistry::new(&s).list().unwrap();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"first".to_string()));
        assert!(names.contains(&"second".to_string()));
    }

    #[test]
    fn install_nodes_and_edges_with_increment_rewrite() {
        let s = setup();
        let body = r#"
[[nodes]]
name = "alice"
node_type = "persona"
metadata = { owner = "owner_a" }

[[nodes]]
name = "bob"
node_type = "persona"

[[edges]]
from_name = "alice"
to_name = "bob"
edge_type = "routes_to"
"#;
        let bundle = register_bundle(&s, "b", body);

        let r1 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r1.installed.len(), 3, "report: {:?}", r1);
        assert!(r1.errors.is_empty(), "errors: {:?}", r1.errors);

        // re-install → node names auto-increment, edge from/to rewritten
        let r2 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        let final_names: Vec<_> = r2.installed.iter().map(|i| i.final_name.clone()).collect();
        assert!(final_names.contains(&"alice-1".to_string()));
        assert!(final_names.contains(&"bob-1".to_string()));
        assert!(final_names.contains(&"alice-1->bob-1".to_string()));
    }

    #[test]
    fn install_writes_log_to_bundle_installs_table() {
        let s = setup();
        let body = r#"
[[specs]]
name = "x"
spec = { TypeIs = "persona" }
"#;
        let bundle = register_bundle(&s, "logged", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        // Verify the report was persisted by counting rows.
        let count: i64 = s
            .conn_for_test()
            .query_row(
                "SELECT COUNT(*) FROM bundle_installs WHERE bundle_id = ?1",
                rusqlite::params![bundle.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        // The install_id matches the report.
        let stored_install_id: String = s
            .conn_for_test()
            .query_row(
                "SELECT install_id FROM bundle_installs WHERE bundle_id = ?1",
                rusqlite::params![bundle.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_install_id, r.install_id);
    }

    #[test]
    fn install_invalid_toml_returns_err() {
        let s = setup();
        let bundle = register_bundle(&s, "broken", "this is = not valid toml [[[");
        let err = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bundle TOML parse"), "got: {}", msg);
    }

    #[test]
    fn install_wirings_writes_outline_nodes() {
        let s = setup();
        let body = r#"
[[wirings]]
persona_id = "alice"
slot = "mailbox"
source_uri = "mini-app://mailbox?alias=for_alice"

[[wirings]]
persona_id = "alice"
slot = "news"
source_uri = "mini-app://news"
projection_ref = "news_overview"
"#;
        let bundle = register_bundle(&s, "b", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r.installed.len(), 2, "report: {:?}", r);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.installed[0].kind, "wiring");
        assert_eq!(r.installed[0].final_name, "alice.mailbox");
        assert_eq!(r.installed[1].final_name, "alice.news");

        // re-install → auto-increment composite names
        let r2 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        let final_names: Vec<_> = r2.installed.iter().map(|i| i.final_name.clone()).collect();
        assert!(final_names.contains(&"alice.mailbox-1".to_string()));
        assert!(final_names.contains(&"alice.news-1".to_string()));
    }

    #[test]
    fn install_wiring_persists_maintenance_exempt_flag() {
        // Regression test for issue e8b444a6 — v0.8.0 dropped the
        // top-level `maintenance_exempt = true` value at TOML
        // deserialize boundary because the WiringEntry struct did not
        // declare the field. Caught during the carol.anchor_files
        // configuration round-trip (`wire_context_get` showed
        // `maintenance_exempt: false` after a bundle install that
        // explicitly set it true).
        let s = setup();
        let body = r#"
[[wirings]]
persona_id = "carol"
slot = "anchor_files"
source_uri = "mini-app://anchor_file?alias=for_carol_anchors"
projection_ref = "carol.section.anchor_files"
maintenance_exempt = true
"#;
        let bundle = register_bundle(&s, "carol.anchor_files.v1", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r.installed.len(), 1, "report: {:?}", r);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

        let node_id = s
            .lookup_node_id_by_name("carol.anchor_files")
            .unwrap()
            .expect("wiring node row");
        let node = s.get_node(&node_id).unwrap().expect("get_node");
        assert_eq!(
            node.metadata
                .get("maintenance_exempt")
                .and_then(|v| v.as_bool()),
            Some(true),
            "maintenance_exempt should round-trip into the node metadata, got: {:?}",
            node.metadata
        );
    }

    #[test]
    fn install_wiring_omits_maintenance_exempt_when_absent() {
        // Coverage for the default branch — wirings that do not opt in
        // must not synthesize a metadata.maintenance_exempt key, so
        // existing graph callers that gate on `Value::Bool` presence
        // keep their behaviour.
        let s = setup();
        let body = r#"
[[wirings]]
persona_id = "alice"
slot = "mailbox"
source_uri = "mini-app://mailbox?alias=for_alice"
"#;
        let bundle = register_bundle(&s, "alice.mailbox.v1", body);
        install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        let node_id = s
            .lookup_node_id_by_name("alice.mailbox")
            .unwrap()
            .expect("wiring node row");
        let node = s.get_node(&node_id).unwrap().expect("get_node");
        assert!(
            node.metadata.get("maintenance_exempt").is_none(),
            "absent flag should not synthesize metadata key, got: {:?}",
            node.metadata
        );
    }

    #[test]
    fn install_wiring_persists_auth_service_key() {
        // `auth` (credential reference key, never a secret) must round-trip
        // into node metadata alongside persona/axis/source_uri.
        let s = setup();
        let body = r#"
[[wirings]]
persona_id = "alice"
slot = "issues"
source_uri = "github://ynishi/persona-wire"
auth = "github-alt"
"#;
        let bundle = register_bundle(&s, "alice.issues.v1", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r.installed.len(), 1, "report: {:?}", r);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

        let node_id = s
            .lookup_node_id_by_name("alice.issues")
            .unwrap()
            .expect("wiring node row");
        let node = s.get_node(&node_id).unwrap().expect("get_node");
        assert_eq!(
            node.metadata.get("auth").and_then(|v| v.as_str()),
            Some("github-alt"),
            "auth service_key should round-trip into node metadata, got: {:?}",
            node.metadata
        );
    }

    #[test]
    fn install_wiring_omits_auth_when_absent() {
        // Coverage for the default branch — wirings that do not set `auth`
        // must not synthesize a metadata.auth key (full backward
        // compatibility for existing bundles).
        let s = setup();
        let body = r#"
[[wirings]]
persona_id = "alice"
slot = "mailbox"
source_uri = "mini-app://mailbox?alias=for_alice"
"#;
        let bundle = register_bundle(&s, "alice.mailbox.noauth.v1", body);
        install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        let node_id = s
            .lookup_node_id_by_name("alice.mailbox")
            .unwrap()
            .expect("wiring node row");
        let node = s.get_node(&node_id).unwrap().expect("get_node");
        assert!(
            node.metadata.get("auth").is_none(),
            "absent auth should not synthesize metadata key, got: {:?}",
            node.metadata
        );
    }

    #[test]
    fn install_workflows_via_existing_register_use_case() {
        let s = setup();
        let body = r#"
[[workflows]]
id = "alice-wake"
persona_id = "alice"
trigger = { kind = "on_demand" }
action = { kind = "no_op" }
"#;
        let bundle = register_bundle(&s, "b", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r.installed.len(), 1, "report: {:?}", r);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert_eq!(r.installed[0].kind, "workflow");
        assert_eq!(r.installed[0].final_name, "alice-wake");

        // re-install → workflow id collision triggers auto-increment via
        // node name (workflow_def Node name == workflow id).
        let r2 = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r2.installed.len(), 1, "report: {:?}", r2);
        assert_eq!(r2.installed[0].final_name, "alice-wake-1");
    }

    #[test]
    fn install_workflow_bad_trigger_records_error() {
        let s = setup();
        let body = r#"
[[workflows]]
id = "broken"
trigger = { kind = "unknown_kind" }
action = { kind = "no_op" }
"#;
        let bundle = register_bundle(&s, "b", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert!(r.installed.is_empty(), "report: {:?}", r);
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].kind, "workflow");
        assert_eq!(r.errors[0].name, "broken");
    }

    #[test]
    fn install_unknown_target_form_records_error_not_panic() {
        let s = setup();
        let body = r#"
[[specs]]
name = "s"
spec = { TypeIs = "persona" }

[[projections]]
name = "p"
spec_ref = "s"
template = "x"
target_form = "yaml"
"#;
        let bundle = register_bundle(&s, "b", body);
        let r = install_bundle(&bundle, ConflictMode::Increment, &s).unwrap();
        assert_eq!(r.installed.len(), 1); // spec succeeded
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].kind, "projection");
        assert!(
            r.errors[0].error.contains("yaml"),
            "got: {}",
            r.errors[0].error
        );
    }
}
