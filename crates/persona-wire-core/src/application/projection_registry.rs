//! Projection registry — thin storage wrapper that routes through the
//! [`projection_mapper`](super::projection_mapper) Data Mapper.
//!
//! The DTO (`NamedProjection`) + Entity round-trip lives in
//! [`projection_mapper`](super::projection_mapper). This module owns only
//! the SQLite column tuple ↔ DTO translation and the `register / get / list`
//! flow surface.
//!
//! Sibling of [`wiring_mapper`](super::wiring_mapper) /
//! [`workflow_mapper`](super::workflow_mapper) consumers in `use_cases.rs`.

pub use super::projection_mapper::NamedProjection;
pub use crate::domain::entity::TargetForm;

use super::projection_mapper::{dto_to_projection, projection_to_dto};
use crate::domain::entity::projection::Projection;
use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::storage::SqliteStorage;

pub struct ProjectionRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> ProjectionRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    /// Persist a Domain Entity through the Data Mapper boundary.
    pub fn register(&self, p: &Projection) -> WireResult<()> {
        let dto = projection_to_dto(p);
        self.upsert_dto(&dto)
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

    // -- Column tuple ↔ DTO internals (kept private; DTO does not leak past
    //    the boundary except via the mapper re-export).

    fn upsert_dto(&self, p: &NamedProjection) -> WireResult<()> {
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
