//! Specification registry — store dynamic / composable Specifications by name.
//!
//! Backed by the storage layer (`specifications` table). Each entry is the
//! JSON-serialised form of a `Specification`. Domain-neutral: callers register
//! arbitrary Specifications (BP: Specification pattern).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::entity::projection::SpecificationId;
use crate::domain::error::{DomainError, WireError, WireResult};
use crate::domain::specification::Specification;
use crate::infrastructure::storage::SqliteStorage;

/// Full registry row read surface for `wire_spec_get` / `wire_spec_list` —
/// carries the raw `json` body (undecoded) alongside id / name / timestamps,
/// mirroring `bundle_registry::Bundle` returning the raw TOML `body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecRow {
    pub id: SpecificationId,
    pub name: String,
    pub json: String,
    /// Unix epoch seconds.
    pub created_at: i64,
    /// Unix epoch seconds.
    pub updated_at: i64,
}

pub struct SpecRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> SpecRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    /// Register (upsert) a Specification by name. Returns the row's ULID id
    /// (newly minted on insert; preserved on overwrite).
    pub fn register(&self, name: &str, spec: &Specification) -> WireResult<SpecificationId> {
        let expr = serde_json::to_string(spec)
            .map_err(|e| WireError::Domain(DomainError::InvalidSpec(e.to_string())))?;
        let now = current_epoch_secs()?;
        self.storage.upsert_specification(name, &expr, now)
    }

    pub fn get(&self, name: &str) -> WireResult<Option<Specification>> {
        let Some(expr) = self.storage.get_specification(name)? else {
            return Ok(None);
        };
        serde_json::from_str(&expr)
            .map(Some)
            .map_err(|e| WireError::Domain(DomainError::InvalidSpec(e.to_string())))
    }

    pub fn list(&self) -> WireResult<Vec<String>> {
        Ok(self
            .storage
            .list_specifications()?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }

    /// Read a full row (raw `json` body, no decode) by `name`. Powers
    /// `wire_spec_get`.
    pub fn get_full_by_name(&self, name: &str) -> WireResult<Option<SpecRow>> {
        Ok(self
            .storage
            .get_specification_full_by_name(name)?
            .map(row_to_spec_row))
    }

    /// Read a full row by ULID `id`. Powers `wire_spec_get`.
    pub fn get_full_by_id(&self, id: SpecificationId) -> WireResult<Option<SpecRow>> {
        Ok(self
            .storage
            .get_specification_full_by_id(id)?
            .map(row_to_spec_row))
    }

    /// Resolve a caller-friendly `id_or_name` (ULID tried first, name
    /// fallback) to a full row. Powers `wire_spec_get`.
    pub fn get_full_by_ref(&self, id_or_name: &str) -> WireResult<Option<SpecRow>> {
        match self.storage.resolve_specification_id_or_name(id_or_name)? {
            Some(id) => self.get_full_by_id(id),
            None => Ok(None),
        }
    }

    /// List full rows in `created_at`-descending order. Powers
    /// `wire_spec_list`.
    pub fn list_full(&self, limit: i64, offset: i64) -> WireResult<Vec<SpecRow>> {
        Ok(self
            .storage
            .list_specifications_full(limit, offset)?
            .into_iter()
            .map(row_to_spec_row)
            .collect())
    }
}

fn row_to_spec_row(row: crate::infrastructure::storage::SpecificationFullRow) -> SpecRow {
    let (id, name, json, created_at, updated_at) = row;
    SpecRow {
        id,
        name,
        json,
        created_at,
        updated_at,
    }
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
    fn get_full_by_name_and_by_id_and_ref_agree() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        let spec = Specification::TypeIs("persona".into());
        let id = reg.register("active_personas", &spec).unwrap();

        let by_name = reg
            .get_full_by_name("active_personas")
            .unwrap()
            .expect("by name");
        assert_eq!(by_name.id, id);
        assert_eq!(by_name.name, "active_personas");
        assert_eq!(by_name.json, r#"{"TypeIs":"persona"}"#);
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
            .get_full_by_ref("active_personas")
            .unwrap()
            .expect("by ref name");
        assert_eq!(by_ref_name, by_name);
    }

    #[test]
    fn get_full_by_ref_returns_none_for_missing() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        assert!(reg.get_full_by_ref("missing").unwrap().is_none());
        assert!(reg
            .get_full_by_ref(&crate::domain::graph::Ulid::new().to_string())
            .unwrap()
            .is_none());
    }

    #[test]
    fn list_full_returns_created_at_desc() {
        let storage = setup();
        let reg = SpecRegistry::new(&storage);
        reg.register("zeta", &Specification::TypeIs("persona".into()))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        reg.register("alpha", &Specification::TypeIs("channel".into()))
            .unwrap();

        let rows = reg.list_full(100, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "alpha", "most recently registered first");
        assert_eq!(rows[1].name, "zeta");
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
