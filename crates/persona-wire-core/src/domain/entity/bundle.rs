//! `Bundle` Domain Entity — scaffolding template that bundles
//! Spec / Projection / Wiring / Workflow (+ optional Node / Edge) into a
//! single TOML document for one-shot install.
//!
//! Aggregate Root identified by [`BundleName`]. The body is the literal TOML
//! payload — parsing happens at install time, not at register time, so a
//! malformed bundle can be registered, inspected, then discarded without
//! corrupting the registry.
//!
//! ## Persistence pattern (SoT)
//!
//! Same PoEAA Registry stance as `Projection`: the application-layer
//! [`crate::application::bundle_registry::BundleRegistry`] is the single
//! lookup surface. Persistence lives in the `bundles` SQLite table (one row
//! per bundle, `name` unique). Install history lives in `bundle_installs`
//! (one row per `install` use-case invocation) and feeds the future
//! History / Force / Undo carry.
//!
//! ## Invariants
//!
//! - [`BundleName`] / [`BundleVersion`] — non-empty.
//! - `body` — non-empty TOML literal. Schema validity is **not** enforced
//!   at construction; the install use case parses and reports per-entity
//!   errors at dispatch time.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::error::{DomainError, WireResult};
use crate::domain::graph::Ulid;

// -- Surrogate id ------------------------------------------------------------
//
// Same shape as `SpecificationId` / `ProjectionId` / `NodeId` / `EdgeId`:
// a `ulid::Ulid` alias. Identity-by-name lookups use `BundleName`; the ULID
// is the rename-resistant reference used by `bundle_installs.bundle_id` FK.

pub type BundleId = Ulid;

// -- BundleName --------------------------------------------------------------

/// Bundle identifier Value Object. Non-empty.
///
/// Name conflicts on `register` are resolved by auto-increment (`-1`, `-2`,
/// ... suffix) at the use-case layer; the entity itself only enforces
/// non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BundleName(String);

impl BundleName {
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(DomainError::ConstraintViolation("bundle name must not be empty".into()).into());
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for BundleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BundleName {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        BundleName::new(s).map_err(serde::de::Error::custom)
    }
}

// -- BundleVersion -----------------------------------------------------------

/// Bundle version Value Object. Non-empty. SemVer comparison logic is v2
/// carry — v1 stores the literal string and round-trips it untouched.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BundleVersion(String);

impl BundleVersion {
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(DomainError::ConstraintViolation("bundle version must not be empty".into()).into());
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for BundleVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BundleVersion {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        BundleVersion::new(s).map_err(serde::de::Error::custom)
    }
}

// -- Bundle ------------------------------------------------------------------

/// Registry-shaped Bundle row. `body` carries the literal TOML payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub id: BundleId,
    pub name: BundleName,
    pub version: BundleVersion,
    pub description: Option<String>,
    pub body: String,
    /// Unix epoch seconds.
    pub created_at: i64,
    /// Unix epoch seconds.
    pub updated_at: i64,
}

impl Bundle {
    /// Domain constructor. Enforces non-empty `body`; per-section schema
    /// validation lives at the install use case (parse-time).
    pub fn new(
        id: BundleId,
        name: BundleName,
        version: BundleVersion,
        description: Option<String>,
        body: impl Into<String>,
        created_at: i64,
        updated_at: i64,
    ) -> WireResult<Self> {
        let body = body.into();
        if body.is_empty() {
            return Err(DomainError::ConstraintViolation("bundle body must not be empty".into()).into());
        }
        Ok(Self {
            id,
            name,
            version,
            description,
            body,
            created_at,
            updated_at,
        })
    }
}

// -- ConflictMode ------------------------------------------------------------

/// Conflict resolution mode for `wire_bundle_install`.
///
/// - [`ConflictMode::Increment`] (default) — entity name auto-increments on
///   collision (`name` → `name-1` → `name-2` ...). Non-destructive; safe
///   to re-install. The id (ULID) is always newly minted, so the registry
///   never collides on id.
/// - [`ConflictMode::Skip`] — leave the existing entity untouched and record
///   the collision in the install report. Idempotent for fixed-name
///   bundles.
/// - [`ConflictMode::Error`] — abort the entire install on the first
///   collision. Nothing is written. Strict mode for scaffold-into-empty
///   environments.
///
/// Force / override is v2 carry — requires History view + install log
/// (`bundle_installs` table is already provisioned).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictMode {
    Increment,
    Skip,
    Error,
}

impl Default for ConflictMode {
    fn default() -> Self {
        Self::Increment
    }
}

impl fmt::Display for ConflictMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Increment => "increment",
            Self::Skip => "skip",
            Self::Error => "error",
        };
        f.write_str(s)
    }
}

impl ConflictMode {
    pub fn parse(s: &str) -> WireResult<Self> {
        match s.to_ascii_lowercase().as_str() {
            "increment" => Ok(Self::Increment),
            "skip" => Ok(Self::Skip),
            "error" => Ok(Self::Error),
            other => Err(DomainError::ConstraintViolation(format!(
                "unknown conflict mode: {} (expected increment/skip/error)",
                other
            ))
            .into()),
        }
    }
}

// -- BundleRef ---------------------------------------------------------------

/// Lookup key for bundle CRUD use cases.
///
/// MCP / CLI callers pass a single string; the use case parses it as ULID
/// first (Crockford base32, 26 chars) and falls back to `BundleName` on
/// parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleRef {
    Id(BundleId),
    Name(BundleName),
}

impl BundleRef {
    /// Caller-friendly parse: try ULID first, fall back to name.
    pub fn parse(s: &str) -> WireResult<Self> {
        if let Ok(id) = Ulid::from_string(s) {
            return Ok(Self::Id(id));
        }
        BundleName::new(s).map(Self::Name)
    }
}

// -- BundleInstallReport -----------------------------------------------------

/// Per-entity outcome of one bundle install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledItem {
    /// `"node"` / `"edge"` / `"spec"` / `"projection"` / `"wiring"` / `"workflow"`.
    pub kind: String,
    pub original_name: String,
    pub final_name: String,
    /// ULID rendered as Crockford base32.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedItem {
    pub kind: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorItem {
    pub kind: String,
    pub name: String,
    pub error: String,
}

/// Result of one bundle install. Always written to the `bundle_installs`
/// table verbatim alongside the bundle id and mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleInstallReport {
    pub install_id: String,
    pub bundle_id: String,
    pub mode: ConflictMode,
    pub installed: Vec<InstalledItem>,
    pub skipped: Vec<SkippedItem>,
    pub errors: Vec<ErrorItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_name_rejects_empty() {
        assert!(BundleName::new("").is_err());
        assert!(BundleName::new("quickstart").is_ok());
    }

    #[test]
    fn bundle_version_rejects_empty() {
        assert!(BundleVersion::new("").is_err());
        assert!(BundleVersion::new("0.1.0").is_ok());
    }

    #[test]
    fn bundle_rejects_empty_body() {
        let id = Ulid::new();
        let name = BundleName::new("quickstart").unwrap();
        let ver = BundleVersion::new("0.1.0").unwrap();
        assert!(Bundle::new(id, name.clone(), ver.clone(), None, "", 0, 0).is_err());
        assert!(Bundle::new(id, name, ver, None, "[bundle]\nname=\"x\"", 0, 0).is_ok());
    }

    #[test]
    fn conflict_mode_parse_roundtrip() {
        assert_eq!(ConflictMode::parse("increment").unwrap(), ConflictMode::Increment);
        assert_eq!(ConflictMode::parse("SKIP").unwrap(), ConflictMode::Skip);
        assert_eq!(ConflictMode::parse("Error").unwrap(), ConflictMode::Error);
        assert!(ConflictMode::parse("force").is_err());
    }

    #[test]
    fn conflict_mode_default_is_increment() {
        assert_eq!(ConflictMode::default(), ConflictMode::Increment);
    }

    #[test]
    fn bundle_ref_parses_ulid_first_then_name() {
        let id = Ulid::new();
        match BundleRef::parse(&id.to_string()).unwrap() {
            BundleRef::Id(parsed) => assert_eq!(parsed, id),
            BundleRef::Name(_) => panic!("expected Id"),
        }
        match BundleRef::parse("quickstart").unwrap() {
            BundleRef::Name(n) => assert_eq!(n.as_str(), "quickstart"),
            BundleRef::Id(_) => panic!("expected Name"),
        }
        assert!(BundleRef::parse("").is_err());
    }
}
