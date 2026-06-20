//! NamedProjection registry — fixed named query + template + target form.
//!
//! Backed by the storage layer (`projections` table). BP: CQRS Read Model.
//!
//! `TargetForm` is owned by the Domain Entity layer
//! ([`crate::domain::entity::projection::TargetForm`]) and re-exported here
//! for source-compat during the Data Mapper migration. The Entity-typed
//! mapper boundary lands in step 3.

pub use crate::domain::entity::TargetForm;

use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::storage::SqliteStorage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedProjection {
    pub name: String,
    /// Reference to a registered Specification by name (see SpecRegistry).
    pub spec_ref: String,
    /// Template body — minimal mustache-like substitution (see infrastructure::rendering).
    pub template: String,
    pub target_form: TargetForm,

    // P3a Phase 2 (a) — Plugin dispatch hints. All three are `Option`; when
    // `None`, the use-case layer falls back to PluginRegistry defaults
    // (`template_engine` = `"handlebars"`, `projection_kind` = `"static"`,
    // `projection_config` = `null`). Existing rows persisted before Phase 2
    // load with `None` for all three and therefore preserve prior behaviour.
    /// Identifier of the `TemplateEngine` impl to render with (e.g. `"handlebars"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_engine: Option<String>,
    /// Identifier of the `Projection` impl to dispatch through (e.g. `"static"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<String>,
    /// Projection-specific config (LLM endpoint, cache TTL, …). Schema is
    /// owned by the consuming `Projection` impl; wire-core is opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_config: Option<serde_json::Value>,
}

pub struct ProjectionRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> ProjectionRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    pub fn register(&self, p: &NamedProjection) -> WireResult<()> {
        // `projection_config` is stored as the canonical JSON string form so
        // any `Value` shape round-trips losslessly through SQLite TEXT.
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

    pub fn get(&self, name: &str) -> WireResult<Option<NamedProjection>> {
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

    pub fn list(&self) -> WireResult<Vec<String>> {
        self.storage.list_projections()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
    }

    #[test]
    fn register_and_get_roundtrip() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        let p = NamedProjection {
            name: "_persona_toc".into(),
            spec_ref: "active_personas".into(),
            template: "Active personas ({{count}}): {{names}}".into(),
            target_form: TargetForm::Prompt,
            template_engine: None,
            projection_kind: None,
            projection_config: None,
        };
        reg.register(&p).unwrap();
        let got = reg.get("_persona_toc").unwrap().expect("exists");
        assert_eq!(got, p);
    }

    #[test]
    fn list_returns_names() {
        let storage = setup();
        let reg = ProjectionRegistry::new(&storage);
        for name in ["b_view", "a_view"] {
            reg.register(&NamedProjection {
                name: name.into(),
                spec_ref: "s".into(),
                template: "t".into(),
                target_form: TargetForm::Markdown,
                template_engine: None,
                projection_kind: None,
                projection_config: None,
            })
            .unwrap();
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
