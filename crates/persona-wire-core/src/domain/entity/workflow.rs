//! `Workflow` Entity — trigger-driven autonomous binding within a persona's
//! context.
//!
//! Sibling of [`crate::domain::entity::wiring::Wiring`]. Both are persisted
//! through the existing Math backend Repository (`Node` CRUD) and live
//! behind the Wire's external rendering surface ([`Projection`]); neither
//! is re-exported at the entity-module root. See the module-level "Surface
//! policy" / "Persistence" sections in [`crate::domain::entity`].
//!
//! [`Projection`]: crate::domain::entity::projection::Projection
//!
//! # Storage form (legacy bridge)
//!
//! Persisted as a `Node` with `type = "workflow_def"` and metadata:
//!
//! ```text
//! Node {
//!   id: "<workflow_id>",
//!   type: "workflow_def",
//!   metadata: {
//!     "persona":  Option<String>,
//!     "trigger":  { "kind": "on_demand" | "on_event", "event"?: String },
//!     "action":   { "kind": "no_op" | "emit_projection", "projection_names"?: [<slot>] },
//!     "enabled":  bool,
//!   },
//! }
//! ```
//!
//! The mapper boundary (application use cases that build / read this Node)
//! is responsible for translating `Vec<Slot>` ↔ `metadata["projection_names"]`.
//!
//! # Trigger / Action vocabulary
//!
//! - Triggers: `OnDemand`, `OnEvent { event }`
//! - Actions:  `NoOp`, `EmitProjection { slots: Vec<Slot> }`
//!
//! The entity holds [`Slot`] directly for the action target. Some legacy
//! callsites still describe the same field as an "axis name" — that is the
//! jargon predating the entity layer (see [`crate::domain::entity::slot`]
//! module docs); the entity converges on `Slot`, and the mapper layer
//! reconciles the wire-format `Vec<String>` until the storage rename is
//! performed.
//!
//! Future-only variants (e.g. cron / metadata_changed triggers, set_metadata
//! / fire_mailbox actions) are intentionally **not** added until they have
//! an actual use case.

use crate::domain::entity::{persona_id::PersonaId, slot::Slot};
use crate::domain::error::{DomainError, WireResult};

// -- WorkflowId --------------------------------------------------------------

/// Workflow surrogate identifier Value Object. Non-empty.
///
/// Unlike `Wiring` (natural composite key `(PersonaId, Slot)`), `Workflow`
/// uses a caller-supplied surrogate string id. Multiple workflows on the
/// same persona / trigger are explicitly allowed by the existing API; the
/// `WorkflowDuplicateTrigger` doctor probe surfaces such cases as advisory
/// findings rather than rejecting them at construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkflowId(String);

impl WorkflowId {
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(DomainError::InvalidSpec("workflow id must not be empty".into()).into());
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkflowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for WorkflowId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<WorkflowId> for String {
    fn from(value: WorkflowId) -> Self {
        value.0
    }
}

impl TryFrom<String> for WorkflowId {
    type Error = crate::domain::error::WireError;
    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for WorkflowId {
    type Error = crate::domain::error::WireError;
    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

// -- Trigger -----------------------------------------------------------------

/// Workflow trigger — what causes the workflow to fire.
///
/// Yaron Minsky "Make Illegal States Unrepresentable" pattern: the legacy
/// `{"kind": ..., "event"?: ...}` JSON shape is rebuilt as a sum type so
/// that `OnEvent` without an `event` payload is unconstructible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    OnDemand,
    OnEvent { event: String },
}

impl Trigger {
    pub fn on_event(event: impl Into<String>) -> WireResult<Self> {
        let event = event.into();
        if event.is_empty() {
            return Err(DomainError::InvalidSpec("trigger.event must not be empty".into()).into());
        }
        Ok(Self::OnEvent { event })
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::OnDemand => "on_demand",
            Self::OnEvent { .. } => "on_event",
        }
    }
}

// -- Action ------------------------------------------------------------------

/// Workflow action — what the workflow does when it fires.
///
/// `EmitProjection.slots` carries `Vec<Slot>` (Domain vocabulary). The
/// legacy storage shape `metadata["action"]["projection_names"]` is a
/// `Vec<String>` of slot names; the mapper boundary translates between
/// the two until the storage rename land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    NoOp,
    EmitProjection { slots: Vec<Slot> },
}

impl Action {
    pub fn emit_projection(slots: Vec<Slot>) -> WireResult<Self> {
        if slots.is_empty() {
            return Err(DomainError::InvalidSpec(
                "action.emit_projection must target at least one slot".into(),
            )
            .into());
        }
        Ok(Self::EmitProjection { slots })
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::NoOp => "no_op",
            Self::EmitProjection { .. } => "emit_projection",
        }
    }
}

// -- Workflow ----------------------------------------------------------------

/// Workflow Domain Entity.
///
/// Immutable once constructed. `enabled` is reflected by reconstructing the
/// Entity; the Math backend Repository (`Node` CRUD) handles persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    id: WorkflowId,
    persona_id: Option<PersonaId>,
    trigger: Trigger,
    action: Action,
    enabled: bool,
}

impl Workflow {
    pub fn new(
        id: WorkflowId,
        persona_id: Option<PersonaId>,
        trigger: Trigger,
        action: Action,
        enabled: bool,
    ) -> Self {
        Self {
            id,
            persona_id,
            trigger,
            action,
            enabled,
        }
    }

    pub fn id(&self) -> &WorkflowId {
        &self.id
    }

    pub fn persona_id(&self) -> Option<&PersonaId> {
        self.persona_id.as_ref()
    }

    pub fn trigger(&self) -> &Trigger {
        &self.trigger
    }

    pub fn action(&self) -> &Action {
        &self.action
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::{DomainError, WireError};

    fn sample_workflow() -> Workflow {
        Workflow::new(
            WorkflowId::new("wf-mailbox-on-event").unwrap(),
            Some(PersonaId::new("test_persona_a").unwrap()),
            Trigger::on_event("mailbox.delivered").unwrap(),
            Action::emit_projection(vec![Slot::new("mailbox").unwrap()]).unwrap(),
            true,
        )
    }

    // -- WorkflowId ----------------------------------------------------------

    #[test]
    fn workflow_id_accepts_valid() {
        let id = WorkflowId::new("wf-1").unwrap();
        assert_eq!(id.as_str(), "wf-1");
        assert_eq!(id.to_string(), "wf-1");
    }

    #[test]
    fn workflow_id_rejects_empty() {
        let err = WorkflowId::new("").expect_err("empty must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidSpec(_))
        ));
    }

    // -- Trigger -------------------------------------------------------------

    #[test]
    fn trigger_on_demand_kind_str() {
        assert_eq!(Trigger::OnDemand.kind_str(), "on_demand");
    }

    #[test]
    fn trigger_on_event_accepts_non_empty() {
        let t = Trigger::on_event("mailbox.delivered").unwrap();
        assert_eq!(t.kind_str(), "on_event");
        match t {
            Trigger::OnEvent { event } => assert_eq!(event, "mailbox.delivered"),
            _ => panic!("expected OnEvent"),
        }
    }

    #[test]
    fn trigger_on_event_rejects_empty() {
        let err = Trigger::on_event("").expect_err("empty event must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidSpec(_))
        ));
    }

    // -- Action --------------------------------------------------------------

    #[test]
    fn action_no_op_kind_str() {
        assert_eq!(Action::NoOp.kind_str(), "no_op");
    }

    #[test]
    fn action_emit_projection_accepts_non_empty() {
        let a = Action::emit_projection(vec![
            Slot::new("mailbox").unwrap(),
            Slot::new("priorities").unwrap(),
        ])
        .unwrap();
        assert_eq!(a.kind_str(), "emit_projection");
        match a {
            Action::EmitProjection { slots } => {
                assert_eq!(slots.len(), 2);
                assert_eq!(slots[0].as_str(), "mailbox");
                assert_eq!(slots[1].as_str(), "priorities");
            }
            _ => panic!("expected EmitProjection"),
        }
    }

    #[test]
    fn action_emit_projection_rejects_empty() {
        let err = Action::emit_projection(vec![]).expect_err("empty slots must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidSpec(_))
        ));
    }

    // -- Workflow ------------------------------------------------------------

    #[test]
    fn workflow_assembles_from_typed_args() {
        let w = sample_workflow();
        assert_eq!(w.id().as_str(), "wf-mailbox-on-event");
        assert_eq!(w.persona_id().map(|p| p.as_str()), Some("test_persona_a"));
        assert_eq!(w.trigger().kind_str(), "on_event");
        assert_eq!(w.action().kind_str(), "emit_projection");
        assert!(w.enabled());
    }

    #[test]
    fn workflow_allows_no_persona() {
        let w = Workflow::new(
            WorkflowId::new("wf-global").unwrap(),
            None,
            Trigger::OnDemand,
            Action::NoOp,
            false,
        );
        assert!(w.persona_id().is_none());
        assert!(!w.enabled());
    }

    #[test]
    fn workflow_equality_is_structural() {
        let a = sample_workflow();
        let b = sample_workflow();
        assert_eq!(a, b);
    }
}
