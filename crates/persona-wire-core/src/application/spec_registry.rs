//! Specification registry — store dynamic / composable Specifications by name.
//!
//! Backed by the storage layer (`specifications` table). Each entry is the
//! JSON-serialised form of a `Specification`. Domain-neutral: callers register
//! arbitrary Specifications (BP: Specification pattern).

use crate::domain::error::{WireError, WireResult};
use crate::domain::specification::Specification;
use crate::infrastructure::storage::SqliteStorage;

pub struct SpecRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> SpecRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    pub fn register(&self, name: &str, spec: &Specification) -> WireResult<()> {
        let expr =
            serde_json::to_string(spec).map_err(|e| WireError::InvalidSpec(e.to_string()))?;
        self.storage.upsert_specification(name, &expr)
    }

    pub fn get(&self, name: &str) -> WireResult<Option<Specification>> {
        let Some(expr) = self.storage.get_specification(name)? else {
            return Ok(None);
        };
        serde_json::from_str(&expr)
            .map(Some)
            .map_err(|e| WireError::InvalidSpec(e.to_string()))
    }

    pub fn list(&self) -> WireResult<Vec<String>> {
        Ok(self
            .storage
            .list_specifications()?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s.seed_default_types().unwrap();
        s
    }

    #[test]
    fn register_and_get_roundtrip() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        let spec = Specification::TypeIs("persona".into());
        reg.register("active_personas", &spec).unwrap();
        let got = reg.get("active_personas").unwrap().expect("exists");
        match got {
            Specification::TypeIs(t) => assert_eq!(t, "persona"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn register_overwrites_under_same_name() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        reg.register("by_owner", &Specification::TypeIs("persona".into()))
            .unwrap();
        reg.register(
            "by_owner",
            &Specification::MetadataEq {
                path: "owner".into(),
                value: json!("owner_a"),
            },
        )
        .unwrap();
        let got = reg.get("by_owner").unwrap().expect("exists");
        match got {
            Specification::MetadataEq { path, value } => {
                assert_eq!(path, "owner");
                assert_eq!(value, json!("owner_a"));
            }
            _ => panic!("expected MetadataEq after overwrite"),
        }
    }

    #[test]
    fn list_returns_names_sorted() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        reg.register("zeta", &Specification::TypeIs("persona".into()))
            .unwrap();
        reg.register("alpha", &Specification::TypeIs("channel".into()))
            .unwrap();
        let names = reg.list().unwrap();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn get_returns_none_for_missing() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        assert!(reg.get("nope").unwrap().is_none());
    }

    #[test]
    fn composed_spec_roundtrip_preserves_structure() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        let spec = Specification::TypeIs("persona".into()).and(Specification::MetadataEq {
            path: "owner.name".into(),
            value: json!("owner_a"),
        });
        reg.register("personas_owned_by_alpha", &spec).unwrap();
        let got = reg.get("personas_owned_by_alpha").unwrap().expect("exists");
        match got {
            Specification::And(parts) => assert_eq!(parts.len(), 2),
            _ => panic!("expected And"),
        }
    }
}
