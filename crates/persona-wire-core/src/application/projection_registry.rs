//! Projection registry — PoEAA Registry pattern (Fowler PoEAA Ch.18) for
//! named [`Projection`] lookup, routing through the Data Mapper boundary
//! at [`projection_mapper`](super::projection_mapper).
//!
//! # Pattern selection (SoT)
//!
//! - **PoEAA Registry** (Fowler PoEAA Ch.18) — application-layer service
//!   that provides named access to well-known objects, a structured
//!   alternative to global access. `ProjectionRegistry::register / get /
//!   list` is the typed lookup surface; the persona-wire CLI / MCP / use
//!   cases consume `Projection` through this Registry, never by reaching
//!   into `SqliteStorage` directly. **This is the chosen pattern.**
//! - **DDD Repository** (Evans DDD Ch.6 / Vernon IDDD Ch.12) — a Domain
//!   Port (trait) that abstracts Aggregate persistence so the Domain
//!   depends only on the abstraction. **Not adopted.** Replacing the
//!   Registry with a Repository trait would move persistence vocabulary
//!   into `domain/port/` and collapse the application service into a
//!   pass-through, breaking the PoEAA-narrow stance recorded below.
//!
//! Fowler PoEAA's Data Mapper (Ch.10) requires *some* mapper to translate
//! between persistence shape and Domain shape. The literal pattern has an
//! independent Mapper class; persona-wire takes the **narrow** reading and
//! lets the Registry own that bridge through the
//! [`projection_mapper`](super::projection_mapper) module. That keeps one application-layer entry point for everything
//! `name`-addressable about a Projection: lookup, persistence, and DTO
//! translation are co-located rather than spread across an artificial
//! Repository / Mapper / Registry trio.
//!
//! # Layering
//!
//! ```text
//! CLI / MCP / use_cases.rs
//!         │
//!         ▼
//! ProjectionRegistry          ← PoEAA Registry (this module)
//!         │
//!         ▼
//! projection_mapper           ← Data Mapper boundary (DTO ↔ Entity)
//!         │
//!         ▼
//! SqliteStorage               ← Infrastructure (column tuple primitives)
//! ```
//!
//! The DTO (`NamedProjection`) + Entity round-trip lives in
//! [`projection_mapper`](super::projection_mapper). This module owns only
//! the SQLite column tuple ↔ DTO translation (`upsert_dto` / `get_dto`)
//! and the `register / get / list` flow surface. A follow-up carry pushes
//! the column-tuple half down to `projection_mapper` as well, leaving the
//! Registry as a pure named-lookup facade.
//!
//! # Sibling consumers
//!
//! [`wiring_mapper`](super::wiring_mapper) /
//! [`workflow_mapper`](super::workflow_mapper) are sibling Data Mappers
//! invoked directly from `use_cases.rs` against the Math backend Node
//! Repository — Wiring / Workflow do **not** have a Registry counterpart
//! because they are persisted as graph nodes, not as a separately-named
//! table. The Registry layer is Projection-specific by design.

pub use super::projection_mapper::NamedProjection;
pub use crate::domain::entity::TargetForm;

use std::time::{SystemTime, UNIX_EPOCH};

use super::projection_mapper::{dto_to_projection, projection_to_dto};
use crate::domain::entity::projection::{Projection, ProjectionId};
use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::storage::SqliteStorage;

/// Full registry row read surface for `wire_projection_get` /
/// `wire_projection_list` — carries `spec_ref` / `template` / `target_form`
/// as raw persistence fields (no `PluginDispatch` decode) alongside id / name
/// / timestamps, mirroring `bundle_registry::Bundle` returning raw fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRow {
    pub id: ProjectionId,
    pub name: String,
    pub spec_ref: String,
    pub target_form: TargetForm,
    pub template: String,
    /// Unix epoch seconds.
    pub created_at: i64,
    /// Unix epoch seconds.
    pub updated_at: i64,
}

pub struct ProjectionRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> ProjectionRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    /// Persist a Domain Entity through the Data Mapper boundary. Returns
    /// the row's ULID `id` (new on insert; preserved on overwrite).
    pub fn register(
        &self,
        p: &Projection,
    ) -> WireResult<crate::domain::entity::projection::ProjectionId> {
        let dto = projection_to_dto(p);
        let now = current_epoch_secs()?;
        self.upsert_dto(&dto, now)
    }

    /// Load a Domain Entity by name. Returns `Ok(None)` if absent, or a
    /// `DomainError::*` wrapped in `WireError` if the persisted row violates
    /// any Entity invariant.
    pub fn get(&self, name: &str) -> WireResult<Option<Projection>> {
        let Some(dto) = self.get_dto(name)? else {
            return Ok(None);
        };
        Some(dto_to_projection(dto)).transpose()
    }

    pub fn list(&self) -> WireResult<Vec<String>> {
        self.storage.list_projections()
    }

    /// Read a full row (raw persistence fields, no `PluginDispatch` decode)
    /// by `name`. Powers `wire_projection_get`.
    pub fn get_full_by_name(&self, name: &str) -> WireResult<Option<ProjectionRow>> {
        self.storage
            .get_projection_full_by_name(name)?
            .map(row_to_projection_row)
            .transpose()
    }

    /// Read a full row by ULID `id`. Powers `wire_projection_get`.
    pub fn get_full_by_id(&self, id: ProjectionId) -> WireResult<Option<ProjectionRow>> {
        self.storage
            .get_projection_full_by_id(id)?
            .map(row_to_projection_row)
            .transpose()
    }

    /// Resolve a caller-friendly `id_or_name` (ULID tried first, name
    /// fallback) to a full row. Powers `wire_projection_get`.
    pub fn get_full_by_ref(&self, id_or_name: &str) -> WireResult<Option<ProjectionRow>> {
        match self.storage.resolve_projection_id_or_name(id_or_name)? {
            Some(id) => self.get_full_by_id(id),
            None => Ok(None),
        }
    }

    /// List full rows in `created_at`-descending order. Powers
    /// `wire_projection_list`.
    pub fn list_full(&self, limit: i64, offset: i64) -> WireResult<Vec<ProjectionRow>> {
        self.storage
            .list_projections_full(limit, offset)?
            .into_iter()
            .map(row_to_projection_row)
            .collect()
    }

    // -- Column tuple ↔ DTO internals (kept private; DTO does not leak past
    //    the boundary except via the mapper re-export).

    fn upsert_dto(
        &self,
        p: &NamedProjection,
        now_secs: i64,
    ) -> WireResult<crate::domain::entity::projection::ProjectionId> {
        let cfg_text = match &p.projection_config {
            Some(v) => {
                Some(serde_json::to_string(v).map_err(|e| WireError::Storage(e.to_string()))?)
            }
            None => None,
        };
        self.storage.upsert_projection(
            &p.name,
            &p.spec_ref,
            &p.template,
            p.target_form.as_str(),
            p.template_engine.as_deref(),
            p.projection_kind.as_deref(),
            cfg_text.as_deref(),
            now_secs,
        )
    }

    fn get_dto(&self, name: &str) -> WireResult<Option<NamedProjection>> {
        let Some((spec_ref, template, target_form_str, te, pk, pc)) =
            self.storage.get_projection(name)?
        else {
            return Ok(None);
        };
        let target_form = TargetForm::parse(&target_form_str)?;
        let projection_config = match pc {
            Some(s) => Some(
                serde_json::from_str::<serde_json::Value>(&s)
                    .map_err(|e| WireError::Storage(e.to_string()))?,
            ),
            None => None,
        };
        Ok(Some(NamedProjection {
            name: name.to_string(),
            spec_ref,
            template,
            target_form,
            template_engine: te,
            projection_kind: pk,
            projection_config,
        }))
    }
}

fn row_to_projection_row(
    row: crate::infrastructure::storage::ProjectionFullRow,
) -> WireResult<ProjectionRow> {
    let (id, name, spec_ref, template, target_form_str, created_at, updated_at) = row;
    let target_form = TargetForm::parse(&target_form_str)?;
    Ok(ProjectionRow {
        id,
        name,
        spec_ref,
        target_form,
        template,
        created_at,
        updated_at,
    })
}

fn current_epoch_secs() -> WireResult<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| WireError::Other(format!("system clock before unix epoch: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::entity::projection::PluginDispatch;

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
    }

    fn sample_projection() -> Projection {
        Projection::from_parts(
            "_persona_toc",
            "active_personas",
            "Active personas ({{count}}): {{names}}",
            TargetForm::Prompt,
            PluginDispatch::Default,
        )
        .unwrap()
    }

    #[test]
    fn register_and_get_roundtrip() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        let p = sample_projection();
        reg.register(&p).unwrap();
        let got = reg.get("_persona_toc").unwrap().expect("exists");
        assert_eq!(got, p);
    }

    #[test]
    fn register_and_get_custom_plugin_roundtrip() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        let cfg = serde_json::json!({"endpoint": "https://example/api"});
        let p = Projection::from_parts(
            "summary_view",
            "active_personas",
            "{{summary}}",
            TargetForm::Markdown,
            PluginDispatch::custom("handlebars", "llm", Some(cfg)).unwrap(),
        )
        .unwrap();
        reg.register(&p).unwrap();
        let got = reg.get("summary_view").unwrap().expect("exists");
        assert_eq!(got, p);
    }

    #[test]
    fn list_returns_names() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        for name in ["b_view", "a_view"] {
            let p = Projection::from_parts(
                name,
                "s",
                "t",
                TargetForm::Markdown,
                PluginDispatch::Default,
            )
            .unwrap();
            reg.register(&p).unwrap();
        }
        assert_eq!(reg.list().unwrap(), vec!["a_view", "b_view"]);
    }

    #[test]
    fn get_full_by_name_and_by_id_and_ref_agree() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        let p = sample_projection();
        let id = reg.register(&p).unwrap();

        let by_name = reg
            .get_full_by_name("_persona_toc")
            .unwrap()
            .expect("by name");
        assert_eq!(by_name.id, id);
        assert_eq!(by_name.name, "_persona_toc");
        assert_eq!(by_name.spec_ref, "active_personas");
        assert_eq!(by_name.target_form, TargetForm::Prompt);
        assert_eq!(by_name.template, "Active personas ({{count}}): {{names}}");
        assert!(by_name.created_at > 0);
        assert_eq!(by_name.created_at, by_name.updated_at);

        let by_id = reg.get_full_by_id(id).unwrap().expect("by id");
        assert_eq!(by_id, by_name);

        let by_ref_id = reg
            .get_full_by_ref(&id.to_string())
            .unwrap()
            .expect("by ref id");
        assert_eq!(by_ref_id, by_name);
        let by_ref_name = reg
            .get_full_by_ref("_persona_toc")
            .unwrap()
            .expect("by ref name");
        assert_eq!(by_ref_name, by_name);
    }

    #[test]
    fn get_full_by_ref_returns_none_for_missing() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        assert!(reg.get_full_by_ref("missing").unwrap().is_none());
        assert!(reg
            .get_full_by_ref(&crate::domain::graph::Ulid::new().to_string())
            .unwrap()
            .is_none());
    }

    #[test]
    fn list_full_returns_created_at_desc() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        let p1 = Projection::from_parts(
            "b_view",
            "s",
            "t",
            TargetForm::Markdown,
            PluginDispatch::Default,
        )
        .unwrap();
        reg.register(&p1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let p2 = Projection::from_parts(
            "a_view",
            "s",
            "t",
            TargetForm::Markdown,
            PluginDispatch::Default,
        )
        .unwrap();
        reg.register(&p2).unwrap();

        let rows = reg.list_full(100, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "a_view", "most recently registered first");
        assert_eq!(rows[1].name, "b_view");
    }

    #[test]
    fn target_form_parse_rejects_unknown() {
        assert!(TargetForm::parse("yaml").is_err());
        assert_eq!(TargetForm::parse("prompt").unwrap(), TargetForm::Prompt);
        assert_eq!(TargetForm::parse("ascii").unwrap(), TargetForm::Ascii);
    }

    #[test]
    fn get_missing_returns_none() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        assert!(reg.get("nope").unwrap().is_none());
    }
}
