//! Projection registry — Data Mapper (Fowler PoEAA) between the
//! [`Projection`] Domain Entity and the SQLite `projections` table.
//!
//! `NamedProjection` is the persistence-shape DTO: an anemic row mirror used
//! only inside this module for serde + column round-trip. Application code
//! outside the registry consumes the typed [`Projection`] Entity exclusively;
//! all VO + cross-field invariants live in the Entity layer. BP: CQRS Read
//! Model + Data Mapper.

pub use crate::domain::entity::TargetForm;

use crate::domain::entity::projection::{PluginDispatch, Projection};
use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::storage::SqliteStorage;
use serde::{Deserialize, Serialize};

/// Persistence DTO. Anemic by design — invariants live in [`Projection`].
///
/// Kept `pub` only because a few legacy callsites (doctor probes / tests)
/// still hand-build rows; new code should construct [`Projection`] and let
/// the registry handle the DTO conversion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedProjection {
    pub name: String,
    pub spec_ref: String,
    pub template: String,
    pub target_form: TargetForm,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_config: Option<serde_json::Value>,
}

impl NamedProjection {
    /// DTO → Domain Entity. Runs all VO validations and rejects illegal
    /// PluginDispatch combinations at the mapper boundary.
    pub fn into_entity(self) -> WireResult<Projection> {
        let plugin = PluginDispatch::from_optional_parts(
            self.template_engine,
            self.projection_kind,
            self.projection_config,
        )?;
        Projection::from_parts(
            self.name,
            self.spec_ref,
            self.template,
            self.target_form,
            plugin,
        )
    }

    /// Domain Entity → DTO. Total (no failure path) — Entity invariants are
    /// strictly stronger than DTO shape, so projection is always defined.
    pub fn from_entity(p: &Projection) -> Self {
        let (engine, kind, config) = p.plugin().to_optional_parts();
        Self {
            name: p.name().as_str().to_owned(),
            spec_ref: p.spec_ref().as_str().to_owned(),
            template: p.template().as_str().to_owned(),
            target_form: p.target_form(),
            template_engine: engine.map(str::to_owned),
            projection_kind: kind.map(str::to_owned),
            projection_config: config.cloned(),
        }
    }
}

pub struct ProjectionRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> ProjectionRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    /// Persist a Domain Entity through the Data Mapper boundary.
    pub fn register(&self, p: &Projection) -> WireResult<()> {
        let dto = NamedProjection::from_entity(p);
        self.upsert_dto(&dto)
    }

    /// Load a Domain Entity by name. Returns `Ok(None)` if absent, or a
    /// `DomainError::*` wrapped in `WireError` if the persisted row violates
    /// any Entity invariant.
    pub fn get(&self, name: &str) -> WireResult<Option<Projection>> {
        let Some(dto) = self.get_dto(name)? else {
            return Ok(None);
        };
        Some(dto.into_entity()).transpose()
    }

    pub fn list(&self) -> WireResult<Vec<String>> {
        self.storage.list_projections()
    }

    // -- Mapper internals (kept private; DTO does not leak past the boundary).

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
    use crate::domain::error::DomainError;

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

    #[test]
    fn into_entity_rejects_illegal_plugin_state() {
        // engine without kind — illegal at the mapper boundary
        let dto = NamedProjection {
            name: "p".into(),
            spec_ref: "s".into(),
            template: "t".into(),
            target_form: TargetForm::Prompt,
            template_engine: Some("handlebars".into()),
            projection_kind: None,
            projection_config: None,
        };
        let err = dto.into_entity().expect_err("should reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn from_entity_into_entity_pure_roundtrip() {
        let p = sample_projection();
        let dto = NamedProjection::from_entity(&p);
        let back = dto.into_entity().unwrap();
        assert_eq!(back, p);
    }
}
