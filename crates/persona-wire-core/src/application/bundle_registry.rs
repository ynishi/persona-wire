//! Bundle registry — PoEAA Registry pattern (Fowler PoEAA Ch.18) for
//! named [`Bundle`] lookup, mirroring the
//! [`crate::application::spec_registry::SpecRegistry`] /
//! [`crate::application::projection_registry::ProjectionRegistry`] stance.
//!
//! # Pattern selection (SoT)
//!
//! - **PoEAA Registry** — application-layer service that provides named
//!   access to well-known objects. `BundleRegistry::register / get / list
//!   / delete` is the typed lookup surface; CLI / MCP / use cases reach a
//!   `Bundle` through this Registry, never by reaching into
//!   `SqliteStorage` directly. **This is the chosen pattern.**
//! - **DDD Repository** — not adopted, same rationale as Projection
//!   (collapses the application service into a pass-through).
//!
//! # Scope (v1)
//!
//! `BundleRegistry` owns **CRUD** only:
//! - `register`  — upsert by name (`-1` / `-2` ... auto-increment lives in
//!   the install use case, not here; `register` itself overwrites on
//!   same-name to match `SpecRegistry::register` semantics).
//! - `get` / `get_by_id` — name- or id-based lookup.
//! - `list` — name-ascending summary.
//! - `delete` — by name or id; install history (`bundle_installs`) is
//!   intentionally preserved across bundle deletion for the future
//!   History UI.
//!
//! Bundle **install** (TOML parse → name resolution → dispatch to
//! Spec/Projection/Wiring/Workflow registries) is a separate use case
//! that consumes `BundleRegistry::get`. It is intentionally not a
//! `register` post-hook because (a) a bundle may be registered, then
//! installed multiple times under different `ConflictMode`s, and (b)
//! parse-time errors should not block bundle registration.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::entity::bundle::{Bundle, BundleId, BundleName, BundleRef, BundleVersion};
use crate::domain::error::{WireError, WireResult};
use crate::infrastructure::storage::SqliteStorage;

pub struct BundleRegistry<'a> {
    storage: &'a SqliteStorage,
}

impl<'a> BundleRegistry<'a> {
    pub fn new(storage: &'a SqliteStorage) -> Self {
        Self { storage }
    }

    /// Register (upsert) a Bundle by name. Returns the row's ULID id
    /// (newly minted on insert; preserved on overwrite of existing name).
    ///
    /// Same-name overwrite is intentional and mirrors
    /// [`SpecRegistry::register`](crate::application::spec_registry::SpecRegistry::register).
    /// Conflict resolution at install time is owned by the install use
    /// case via [`ConflictMode`](crate::domain::entity::bundle::ConflictMode).
    pub fn register(
        &self,
        name: &BundleName,
        version: &BundleVersion,
        description: Option<&str>,
        body: &str,
    ) -> WireResult<BundleId> {
        let now = current_epoch_secs()?;
        self.storage
            .upsert_bundle(name.as_str(), version.as_str(), description, body, now)
    }

    /// Read a full Bundle row by name.
    pub fn get(&self, name: &BundleName) -> WireResult<Option<Bundle>> {
        self.storage.get_bundle_by_name(name.as_str())
    }

    /// Read a full Bundle row by id.
    pub fn get_by_id(&self, id: BundleId) -> WireResult<Option<Bundle>> {
        self.storage.get_bundle_by_id(id)
    }

    /// Resolve a [`BundleRef`] (caller-friendly id-or-name enum) to a row.
    pub fn resolve(&self, r: &BundleRef) -> WireResult<Option<Bundle>> {
        match r {
            BundleRef::Id(id) => self.get_by_id(*id),
            BundleRef::Name(name) => self.get(name),
        }
    }

    /// List bundles in name-ascending order. Returned rows include the
    /// full TOML body — callers that only need summary tuples (id / name
    /// / version / description) can map down to a lighter shape at the
    /// transport boundary.
    pub fn list(&self) -> WireResult<Vec<Bundle>> {
        self.storage.list_bundles()
    }

    /// Delete a Bundle by name. Returns `true` if a row was removed.
    /// Install history in `bundle_installs` is intentionally preserved
    /// (no FK cascade) — see module-level docs.
    pub fn delete(&self, name: &BundleName) -> WireResult<bool> {
        self.storage.delete_bundle_by_name(name.as_str())
    }

    /// Delete a Bundle by id. Returns `true` if a row was removed.
    pub fn delete_by_id(&self, id: BundleId) -> WireResult<bool> {
        self.storage.delete_bundle_by_id(id)
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

    fn setup() -> SqliteStorage {
        let s = SqliteStorage::open_in_memory().unwrap();
        s.migrate().unwrap();
        s
    }

    fn sample(name: &str) -> (BundleName, BundleVersion, &'static str) {
        (
            BundleName::new(name).unwrap(),
            BundleVersion::new("0.1.0").unwrap(),
            "[bundle]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
    }

    #[test]
    fn register_and_get_roundtrip() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        let (name, ver, body) = sample("quickstart");
        let id = reg
            .register(&name, &ver, Some("hello"), body)
            .expect("register");
        let got = reg.get(&name).unwrap().expect("exists");
        assert_eq!(got.id, id);
        assert_eq!(got.name.as_str(), "quickstart");
        assert_eq!(got.version.as_str(), "0.1.0");
        assert_eq!(got.description.as_deref(), Some("hello"));
        assert_eq!(got.body, body);
        assert!(got.created_at > 0);
        assert_eq!(got.created_at, got.updated_at);
    }

    #[test]
    fn register_overwrite_preserves_id_and_created_at() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        let (name, ver, body) = sample("quickstart");
        let id1 = reg.register(&name, &ver, None, body).unwrap();
        let first_created = reg.get(&name).unwrap().unwrap().created_at;

        // Force at least one second of clock movement so updated_at differs.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let id2 = reg
            .register(
                &name,
                &BundleVersion::new("0.2.0").unwrap(),
                Some("updated"),
                "[bundle]\nname = \"x\"\nversion = \"0.2.0\"\n",
            )
            .unwrap();
        assert_eq!(id1, id2, "id is preserved across same-name overwrite");
        let got = reg.get(&name).unwrap().unwrap();
        assert_eq!(got.version.as_str(), "0.2.0");
        assert_eq!(got.description.as_deref(), Some("updated"));
        assert_eq!(got.created_at, first_created, "created_at frozen on insert");
        assert!(got.updated_at >= first_created);
    }

    #[test]
    fn get_by_id_and_resolve_match_get() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        let (name, ver, body) = sample("alpha");
        let id = reg.register(&name, &ver, None, body).unwrap();

        let by_id = reg.get_by_id(id).unwrap().expect("by id");
        let by_name = reg.get(&name).unwrap().expect("by name");
        assert_eq!(by_id, by_name);

        let resolved = reg
            .resolve(&BundleRef::Id(id))
            .unwrap()
            .expect("resolve by id");
        assert_eq!(resolved, by_id);
        let resolved = reg
            .resolve(&BundleRef::Name(name))
            .unwrap()
            .expect("resolve by name");
        assert_eq!(resolved, by_id);
    }

    #[test]
    fn list_returns_name_ascending() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        for n in ["c-bundle", "a-bundle", "b-bundle"] {
            let (name, ver, body) = sample(n);
            reg.register(&name, &ver, None, body).unwrap();
        }
        let names: Vec<_> = reg
            .list()
            .unwrap()
            .into_iter()
            .map(|b| b.name.into_string())
            .collect();
        assert_eq!(names, vec!["a-bundle", "b-bundle", "c-bundle"]);
    }

    #[test]
    fn delete_returns_true_then_false() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        let (name, ver, body) = sample("doomed");
        reg.register(&name, &ver, None, body).unwrap();
        assert!(reg.delete(&name).unwrap());
        assert!(reg.get(&name).unwrap().is_none());
        assert!(!reg.delete(&name).unwrap());
    }

    #[test]
    fn delete_by_id_works() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        let (name, ver, body) = sample("doomed");
        let id = reg.register(&name, &ver, None, body).unwrap();
        assert!(reg.delete_by_id(id).unwrap());
        assert!(reg.get(&name).unwrap().is_none());
    }

    #[test]
    fn get_unknown_returns_none() {
        let storage = setup();
        let reg = BundleRegistry::new(&storage);
        let missing = BundleName::new("missing").unwrap();
        assert!(reg.get(&missing).unwrap().is_none());
    }
}
