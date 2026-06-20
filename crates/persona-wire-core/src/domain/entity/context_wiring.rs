//! `ContextWiring` — per-persona Composition Root (Aggregate Root identity).
//!
//! Marks the consistency boundary for one persona's context (its
//! [`crate::domain::entity::wiring::Wiring`] set and
//! [`crate::domain::entity::workflow::Workflow`] set). The boundary is
//! represented by [`PersonaId`] alone — there is exactly one
//! `ContextWiring` per persona, no surrogate id.
//!
//! # Skinny by design
//!
//! `ContextWiring` does **not** hold `Vec<Wiring>` / `Vec<Workflow>` in
//! memory. The wirings and workflows that belong to a persona live in the
//! Math backend graph (`Node` rows of type `outline_node` / `workflow_def`)
//! and are reached through the Repository (`crate::domain::graph`) when a
//! caller actually needs them. The Aggregate Root only carries the identity
//! that says "these are the rows that move together as one consistency
//! unit".
//!
//! This is a deliberate choice. Holding an in-memory `Vec<Wiring>` would
//! force load / mutate / save round-trips for every change and pull the
//! Aggregate into operations that don't need transactional consistency.
//! Today no application call site requires multi-`Wiring` atomic updates;
//! batch operations are explicitly non-atomic at the application surface.
//! Until such a requirement appears, the Aggregate Root stays an identity
//! marker — invariant-checking methods, atomic multi-wiring updates, and
//! collection ownership are deferred and added when they are actually
//! needed.
//!
//! # Surface
//!
//! Not re-exported at the entity module root. The Wire's external surface
//! is [`Projection`]; the Aggregate Root is internal vocabulary, used by
//! entity-layer composition (today) and by future application code that
//! needs an explicit consistency-boundary handle.
//!
//! [`PersonaId`]: crate::domain::entity::persona_id::PersonaId
//! [`Projection`]: crate::domain::entity::projection::Projection

use crate::domain::entity::persona_id::PersonaId;

/// Per-persona Composition Root.
///
/// Identity-only Aggregate Root. See the [module docs](self) for why it
/// does not hold an in-memory `Vec<Wiring>` / `Vec<Workflow>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextWiring {
    persona_id: PersonaId,
}

impl ContextWiring {
    /// Construct a `ContextWiring` for the given persona.
    pub fn new(persona_id: PersonaId) -> Self {
        Self { persona_id }
    }

    /// The persona this Composition Root governs.
    pub fn persona_id(&self) -> &PersonaId {
        &self.persona_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_carries_persona_id() {
        let cw = ContextWiring::new(PersonaId::new("test_persona_a").unwrap());
        assert_eq!(cw.persona_id().as_str(), "test_persona_a");
    }

    #[test]
    fn equality_is_structural_on_persona_id() {
        let a = ContextWiring::new(PersonaId::new("test_persona_a").unwrap());
        let b = ContextWiring::new(PersonaId::new("test_persona_a").unwrap());
        let c = ContextWiring::new(PersonaId::new("test_persona_b").unwrap());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn clone_preserves_identity() {
        let a = ContextWiring::new(PersonaId::new("test_persona_a").unwrap());
        let b = a.clone();
        assert_eq!(a, b);
        assert_eq!(b.persona_id().as_str(), "test_persona_a");
    }
}
