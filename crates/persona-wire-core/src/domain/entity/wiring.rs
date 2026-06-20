//! `Wiring` Entity — 1 slot binding within one persona's context.
//!
//! A `Wiring` carries the natural composite key `(PersonaId, Slot)`, owns a
//! [`Source`] directly, and refers to a registered
//! [`crate::domain::entity::projection::Projection`] by identity
//! ([`ProjectionName`]) — Vernon IDDD Rule 3 (cross-aggregate reference by
//! identity).
//!
//! `Wiring` is **internal** to the entity layer: the Wire's external surface
//! is `Projection`, and reading raw `Wiring` data from the application would
//! bypass the rendering boundary. See the module-level "Surface policy" in
//! [`crate::domain::entity`] for details.
//!
//! # Vocabulary (Slot vs. legacy `axis`)
//!
//! Several legacy callsites and the storage shape carry the slot concept
//! under the field name `axis` (`Node.metadata["axis"]`,
//! `application::projection_naming::workflow_emit_projection_name`,
//! `<persona>.section.<axis>` derive). That name is a jargon placeholder:
//! `mailbox` / `mail` / `news` are not orthogonal axes, they are sibling
//! slot names inside one persona's context (see
//! [`crate::domain::entity::slot`] module docs). The entity carries
//! [`Slot`]; the mapper boundary translates `Slot ↔ metadata["axis"]` until
//! the storage rename is performed.
//!
//! # Identity
//!
//! Identity is the **natural composite key** `(PersonaId, Slot)`. The
//! existing graph storage keys wiring nodes by `format!("{persona}.{slot}")`,
//! and [`Wiring::storage_node_id`] exposes that legacy node-id form as the
//! bridge. A surrogate key (UUID) shape is plausible long-term but is a
//! separate persistence migration; the entity layer itself does not commit
//! to surrogate keys.
//!
//! # Invariants
//!
//! - `persona_id` + `slot` are validated through their VO constructors.
//! - `source` carries the SoT URI ([`Source`] enforces non-empty + scheme).
//! - `projection_ref` is optional — a wiring may exist before its renderer
//!   projection is registered. The runtime treats a missing projection as a
//!   skip + warning rather than a hard error.
//!
//! # Persistence
//!
//! Persisted through the existing Math backend Repository (`Node` CRUD via
//! [`crate::domain::graph`]). No dedicated Registry / DTO / table — see the
//! "Persistence" section in [`crate::domain::entity`] for the rationale.

use crate::domain::entity::{persona_id::PersonaId, projection::ProjectionName, slot::Slot};
use crate::domain::entity::source::Source;
use crate::domain::error::WireResult;

/// Wiring Domain Entity.
///
/// Constructed via [`Wiring::new`] (typed VO args) or [`Wiring::from_parts`]
/// (raw string args, applies all VO validations). Immutable — updates are
/// expressed by constructing a new instance and re-persisting through the
/// (future) Wiring mapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wiring {
    persona_id: PersonaId,
    slot: Slot,
    source: Source,
    projection_ref: Option<ProjectionName>,
}

impl Wiring {
    pub fn new(
        persona_id: PersonaId,
        slot: Slot,
        source: Source,
        projection_ref: Option<ProjectionName>,
    ) -> Self {
        Self {
            persona_id,
            slot,
            source,
            projection_ref,
        }
    }

    /// Convenience constructor: takes raw strings, applies all VO validations.
    pub fn from_parts(
        persona_id: impl Into<String>,
        slot: impl Into<String>,
        source_uri: impl Into<String>,
        projection_ref: Option<String>,
    ) -> WireResult<Self> {
        let projection_ref = projection_ref.map(ProjectionName::new).transpose()?;
        Ok(Self::new(
            PersonaId::new(persona_id)?,
            Slot::new(slot)?,
            Source::new(source_uri)?,
            projection_ref,
        ))
    }

    pub fn persona_id(&self) -> &PersonaId {
        &self.persona_id
    }

    pub fn slot(&self) -> &Slot {
        &self.slot
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn projection_ref(&self) -> Option<&ProjectionName> {
        self.projection_ref.as_ref()
    }

    /// Derive the legacy storage node id (`<persona>.<slot>`).
    ///
    /// Bridge to the existing graph storage that keys wiring nodes by the
    /// natural composite key concatenated with `.`. Removing this bridge
    /// requires a storage migration (carry).
    pub fn storage_node_id(&self) -> String {
        format!("{}.{}", self.persona_id.as_str(), self.slot.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::{DomainError, WireError};

    fn sample() -> Wiring {
        Wiring::from_parts(
            "test_persona_a",
            "mailbox",
            "mini-app://mailbox?alias=for_test_persona_a",
            Some("test_persona_a.section.mailbox".to_string()),
        )
        .expect("valid wiring")
    }

    #[test]
    fn from_parts_accepts_valid() {
        let w = sample();
        assert_eq!(w.persona_id().as_str(), "test_persona_a");
        assert_eq!(w.slot().as_str(), "mailbox");
        assert_eq!(w.source().as_str(), "mini-app://mailbox?alias=for_test_persona_a");
        assert_eq!(
            w.projection_ref().map(|p| p.as_str()),
            Some("test_persona_a.section.mailbox")
        );
    }

    #[test]
    fn from_parts_allows_missing_projection_ref() {
        let w =
            Wiring::from_parts("test_persona_a", "mail", "mini-app://mail?alias=for_test_persona_a", None).unwrap();
        assert!(w.projection_ref().is_none());
    }

    #[test]
    fn from_parts_rejects_empty_persona_id() {
        let err = Wiring::from_parts("", "mailbox", "mini-app://x", None)
            .expect_err("empty persona must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidPersonaId(_))
        ));
    }

    #[test]
    fn from_parts_rejects_slot_with_dot() {
        let err = Wiring::from_parts("test_persona_a", "a.b", "mini-app://x", None)
            .expect_err("dot slot must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn from_parts_rejects_invalid_source() {
        let err = Wiring::from_parts("test_persona_a", "mailbox", "no_scheme", None)
            .expect_err("scheme-less source must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidSource(_))
        ));
    }

    #[test]
    fn from_parts_rejects_empty_projection_ref() {
        let err =
            Wiring::from_parts("test_persona_a", "mailbox", "mini-app://x", Some(String::new()))
                .expect_err("empty projection ref must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn storage_node_id_concatenates_natural_key() {
        let w = sample();
        assert_eq!(w.storage_node_id(), "test_persona_a.mailbox");
    }

    #[test]
    fn immutable_equality() {
        let a = sample();
        let b = sample();
        assert_eq!(a, b);
    }
}
