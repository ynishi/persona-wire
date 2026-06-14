//! NamedProjection registry — fixed named query + template + target form.
//!
//! Backed by the storage layer (`projections` table). BP: CQRS Read Model.

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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TargetForm {
    Prompt,
    Markdown,
    Json,
    Ascii,
}

impl TargetForm {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetForm::Prompt => "prompt",
            TargetForm::Markdown => "markdown",
            TargetForm::Json => "json",
            TargetForm::Ascii => "ascii",
        }
    }

    pub fn parse(s: &str) -> WireResult<Self> {
        match s {
            "prompt" => Ok(TargetForm::Prompt),
            "markdown" => Ok(TargetForm::Markdown),
            "json" => Ok(TargetForm::Json),
            "ascii" => Ok(TargetForm::Ascii),
            other => Err(WireError::Other(format!("unknown target_form: {other}"))),
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

    pub fn register(&self, p: &NamedProjection) -> WireResult<()> {
        self.storage
            .upsert_projection(&p.name, &p.spec_ref, &p.template, p.target_form.as_str())
    }

    pub fn get(&self, name: &str) -> WireResult<Option<NamedProjection>> {
        let Some((spec_ref, template, target_form_str)) = self.storage.get_projection(name)? else {
            return Ok(None);
        };
        let target_form = TargetForm::parse(&target_form_str)?;
        Ok(Some(NamedProjection {
            name: name.to_string(),
            spec_ref,
            template,
            target_form,
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
