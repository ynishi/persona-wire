//! `Slot` Value Object — 1 binding name within a persona's context.
//!
//! A `Slot` identifies a single [`crate::domain::entity::wiring::Wiring`]
//! inside one persona. Concrete values seen in production are `mailbox` /
//! `mail` / `news` / `priorities` etc. — each is **one binding name**, not
//! an orthogonal axis. The legacy storage shape (and several application
//! callsites) carries this same concept under the field name `axis`; that
//! is a jargon symptom from before the entity layer existed — `mailbox` and
//! `mail` are not direction-of-variance "axes", they are sibling slots in
//! one persona's context. `Slot` is the proper Domain vocabulary.
//!
//! # Invariants
//!
//! - **non-empty**
//! - **no `.`** — the natural composite key with `PersonaId` is rendered as
//!   `format!("{persona_id}.{slot}")` at the storage boundary (`Node.id`).
//!   Allowing `.` inside a slot name would make the concatenation ambiguous.
//!
//! Character set / length bounds beyond the above are intentionally left to
//! the persistence boundary so future storage rename / id scheme migrations
//! stay local.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::error::{DomainError, WireResult};

/// Wiring slot name Value Object.
///
/// See [module docs](self) for invariants and the Slot ↔ legacy `axis`
/// vocabulary split.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Slot(String);

impl Slot {
    /// Construct a `Slot` from any string-like value, validating the
    /// non-empty + no-`.` invariants.
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        validate(&s)?;
        Ok(Self(s))
    }

    /// Borrow the underlying slot name as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate(s: &str) -> WireResult<()> {
    if s.is_empty() {
        return Err(DomainError::InvalidMetadata("slot must not be empty".into()).into());
    }
    if s.contains('.') {
        return Err(DomainError::InvalidMetadata(format!("slot must not contain '.': {s}")).into());
    }
    Ok(())
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Slot {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Slot> for String {
    fn from(value: Slot) -> Self {
        value.0
    }
}

impl TryFrom<String> for Slot {
    type Error = crate::domain::error::WireError;

    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Slot {
    type Error = crate::domain::error::WireError;

    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for Slot {
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
    fn new_accepts_valid_slot() {
        let s = Slot::new("mailbox").expect("valid slot");
        assert_eq!(s.as_str(), "mailbox");
        assert_eq!(s.to_string(), "mailbox");
    }

    #[test]
    fn new_rejects_empty() {
        let err = Slot::new("").expect_err("empty must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn new_rejects_dot() {
        let err = Slot::new("foo.bar").expect_err("dot must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn try_from_str_and_string_roundtrip() {
        let from_str: Slot = "news".try_into().expect("ok");
        let from_string: Slot = String::from("news").try_into().expect("ok");
        assert_eq!(from_str, from_string);
        let back: String = from_str.into();
        assert_eq!(back, "news");
    }

    #[test]
    fn serde_roundtrip() {
        let s = Slot::new("priorities").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"priorities\"");
        let parsed: Slot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn serde_rejects_empty() {
        let err = serde_json::from_str::<Slot>("\"\"").expect_err("empty must reject");
        assert!(err.to_string().contains("slot must not be empty"));
    }

    #[test]
    fn serde_rejects_dot() {
        let err = serde_json::from_str::<Slot>("\"a.b\"").expect_err("dot must reject");
        assert!(err.to_string().contains("must not contain '.'"));
    }
}
