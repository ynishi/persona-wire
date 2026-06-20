//! `PersonaId` — owner identity Value Object.
//!
//! Wraps a non-empty `String` that names a persona registered in the external
//! `persona-pack` SoT. The persona's full identity (name / role / overlays)
//! lives in `persona-pack`; `ContextWiring` only carries the id by reference.
//!
//! ## Invariants
//!
//! - **non-empty** — `PersonaId::new("")` returns `DomainError::InvalidPersonaId`.
//!
//! Character set / length bounds are persona-pack's responsibility (external
//! SoT). Domain Entity layer keeps the contract minimal so id values coming
//! from any persona-pack revision remain compatible.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::error::{DomainError, WireResult};

/// Persona identifier Value Object.
///
/// See [module docs](self) for invariants and SoT split with `persona-pack`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PersonaId(String);

impl PersonaId {
    /// Construct a `PersonaId` from any string-like value, validating the
    /// non-empty invariant.
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(DomainError::InvalidPersonaId("persona id must not be empty".into()).into());
        }
        Ok(Self(s))
    }

    /// Borrow the underlying id as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PersonaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PersonaId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<PersonaId> for String {
    fn from(value: PersonaId) -> Self {
        value.0
    }
}

impl TryFrom<String> for PersonaId {
    type Error = crate::domain::error::WireError;

    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for PersonaId {
    type Error = crate::domain::error::WireError;

    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for PersonaId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::WireError;

    #[test]
    fn new_accepts_valid_id() {
        let id = PersonaId::new("alice").expect("valid id");
        assert_eq!(id.as_str(), "alice");
        assert_eq!(id.to_string(), "alice");
    }

    #[test]
    fn new_rejects_empty() {
        let err = PersonaId::new("").expect_err("empty must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidPersonaId(_))
        ));
    }

    #[test]
    fn try_from_str_and_string_roundtrip() {
        let from_str: PersonaId = "bob".try_into().expect("ok");
        let from_string: PersonaId = String::from("bob").try_into().expect("ok");
        assert_eq!(from_str, from_string);
        let back: String = from_str.into();
        assert_eq!(back, "bob");
    }

    #[test]
    fn serde_roundtrip() {
        let id = PersonaId::new("carol").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"carol\"");
        let parsed: PersonaId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn serde_rejects_empty() {
        let err = serde_json::from_str::<PersonaId>("\"\"").expect_err("empty must reject");
        assert!(err.to_string().contains("persona id must not be empty"));
    }
}
