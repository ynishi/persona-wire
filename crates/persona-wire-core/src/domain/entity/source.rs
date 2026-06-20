//! `Source` Entity — SoT location (URI) that a `Wiring` points at.
//!
//! Wraps a URI string with minimal Domain-side invariants. The detailed parse
//! (typed scheme / host / path / query view) lives in the infrastructure layer
//! ([`crate::infrastructure::wire_uri::WireUri`]) and is used by the adapter
//! dispatcher (`PluginRegistry::route`). Domain Entity layer intentionally
//! avoids depending on infrastructure types.
//!
//! ## Invariants
//!
//! - **non-empty**
//! - **scheme prefix present** — value matches `<scheme>:<rest>` where
//!   `<scheme>` is non-empty.
//!
//! Strict scheme grammar validation (ALPHA-first, ALPHA/DIGIT/`+-.`) is the
//! infrastructure layer's concern; surfacing it on construction belongs to
//! the adapter route step, not to the Domain Entity. This keeps Source
//! cheap to construct and free from infra coupling.
//!
//! Owned by [`crate::domain::entity::wiring::Wiring`] (Step C land carry).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::error::{DomainError, WireResult};

/// Source URI Value Object.
///
/// See [module docs](self) for invariants and the rationale behind the
/// minimal Domain-side validation.
///
/// `scheme_len` caches the byte length of the scheme prefix (the substring
/// before `:`). Computed once at construction and reused by [`Source::scheme`]
/// so the accessor stays `panic!` / `expect` free by construction — no
/// runtime re-parse, no `expect("invariant")` on a method that is called
/// after deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Source(#[serde(serialize_with = "serialize_uri")] Inner);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Inner {
    uri: String,
    /// Byte index of the `:` separator in `uri`. Always `< uri.len()` and
    /// `> 0` (enforced by [`parse_scheme_len`]).
    scheme_len: usize,
}

fn serialize_uri<S>(inner: &Inner, ser: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    ser.serialize_str(&inner.uri)
}

impl Source {
    /// Construct a `Source` from any string-like value, validating the
    /// non-empty + scheme-prefix invariants.
    pub fn new(uri: impl Into<String>) -> WireResult<Self> {
        let uri = uri.into();
        let scheme_len = parse_scheme_len(&uri)?;
        Ok(Self(Inner { uri, scheme_len }))
    }

    /// Borrow the underlying URI as `&str`.
    pub fn as_str(&self) -> &str {
        &self.0.uri
    }

    /// Return the scheme prefix (`<scheme>:` の前段)。
    ///
    /// `scheme_len` is computed at construction time, so this accessor is
    /// `panic!` / `expect` free — the slice bound is structurally guaranteed
    /// by [`Inner`]'s invariant (`scheme_len < uri.len()`).
    pub fn scheme(&self) -> &str {
        &self.0.uri[..self.0.scheme_len]
    }
}

/// Validate non-empty + scheme-prefix invariants and return the byte length
/// of the scheme (the substring before `:`). Used by both `Source::new` and
/// the `Deserialize` impl as the single validation entry point.
fn parse_scheme_len(s: &str) -> WireResult<usize> {
    if s.is_empty() {
        return Err(DomainError::InvalidSource("source uri must not be empty".into()).into());
    }
    match s.split_once(':') {
        Some((scheme, _)) if !scheme.is_empty() => Ok(scheme.len()),
        _ => Err(DomainError::InvalidSource(format!(
            "source uri must carry a scheme prefix (`<scheme>:<rest>`): {s}"
        ))
        .into()),
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.uri)
    }
}

impl AsRef<str> for Source {
    fn as_ref(&self) -> &str {
        &self.0.uri
    }
}

impl From<Source> for String {
    fn from(value: Source) -> Self {
        value.0.uri
    }
}

impl TryFrom<String> for Source {
    type Error = crate::domain::error::WireError;

    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Source {
    type Error = crate::domain::error::WireError;

    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for Source {
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
    fn new_accepts_valid_uri() {
        let src = Source::new("mini-app://mailbox?alias=for_alice").expect("valid uri");
        assert_eq!(src.as_str(), "mini-app://mailbox?alias=for_alice");
        assert_eq!(src.scheme(), "mini-app");
    }

    #[test]
    fn new_rejects_empty() {
        let err = Source::new("").expect_err("empty must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidSource(_))
        ));
    }

    #[test]
    fn new_rejects_missing_scheme() {
        let err = Source::new("mailbox/for_alice").expect_err("no scheme must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidSource(_))
        ));
    }

    #[test]
    fn scheme_returns_prefix() {
        let src = Source::new("persona-pack://carol/projections").unwrap();
        assert_eq!(src.scheme(), "persona-pack");
    }

    #[test]
    fn serde_roundtrip() {
        let src = Source::new("outline://book/x?alias=for_bob").unwrap();
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, "\"outline://book/x?alias=for_bob\"");
        let parsed: Source = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, src);
    }

    #[test]
    fn serde_rejects_empty() {
        let err = serde_json::from_str::<Source>("\"\"").expect_err("empty must reject");
        assert!(err.to_string().contains("source uri must not be empty"));
    }

    #[test]
    fn serde_rejects_missing_scheme() {
        let err = serde_json::from_str::<Source>("\"plain_value\"")
            .expect_err("missing scheme must reject");
        assert!(err.to_string().contains("scheme prefix"));
    }
}
