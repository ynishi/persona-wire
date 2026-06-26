//! Mapper boundary: [`Workflow`] Domain Entity ↔ `workflow_def` [`Node`].
//!
//! Fowler PoEAA Data Mapper — Node JSON metadata is the persistence form
//! (storage column-equivalent), [`Workflow`] is the Domain Entity carrying
//! invariants. This module is the **single SoT** for translating between
//! the two shapes; `wire_workflow_register` / `wire_workflow_list` (and any
//! future workflow use case) route through here instead of inlining
//! `metadata["trigger"]["kind"]` / `metadata["action"]["projection_names"]`
//! string surgery.
//!
//! Storage form (cf. `domain/entity/workflow.rs` module docs):
//!
//! ```text
//! Node {
//!   id: "<workflow_id>",
//!   type: "workflow_def",
//!   metadata: {
//!     "persona":  Option<String>,
//!     "trigger":  { "kind": "on_demand" | "on_event", "event"?: String },
//!     "action":   { "kind": "no_op" | "emit_projection",
//!                   "projection_names"?: [<slot>] },
//!     "enabled":  bool,
//!   },
//! }
//! ```
//!
//! Round-trip property: `node_to_workflow(workflow_to_node(w))? == w` for
//! any [`Workflow`] constructed through this module's parsers.

use serde_json::{Map, Value};

use crate::domain::entity::workflow::{Action, Trigger, Workflow, WorkflowId};
use crate::domain::entity::{PersonaId, Slot};
use crate::domain::error::{DomainError, WireError, WireResult};
use crate::domain::graph::{ulid_from_seed, Node};

/// Storage `Node.r#type` literal for a Workflow. Single SoT — internal
/// use-case code and tests reference this constant instead of re-typing the
/// string.
pub const WORKFLOW_TYPE: &str = "workflow_def";

/// `metadata.persona` key — owning persona (optional). Single SoT for the
/// metadata key name so callers do not re-type the bare literal.
pub const META_PERSONA: &str = "persona";

/// `metadata.trigger` key — trigger descriptor JSON.
pub const META_TRIGGER: &str = "trigger";

/// `metadata.action` key — action descriptor JSON.
pub const META_ACTION: &str = "action";

/// `metadata.enabled` key — enable flag (defaults to `true` on missing).
pub const META_ENABLED: &str = "enabled";

const TRIGGER_KINDS_P5A: &[&str] = &["on_demand", "on_event"];
const ACTION_KINDS_P5A: &[&str] = &["no_op", "emit_projection"];

// -- Node → tolerant field reads --------------------------------------------

/// Borrow the optional `persona` field as `&str`.
pub fn extract_persona(node: &crate::domain::graph::Node) -> Option<&str> {
    node.metadata.get(META_PERSONA).and_then(Value::as_str)
}

/// Borrow the `trigger` descriptor JSON; returns a Null borrow when missing.
pub fn extract_trigger_value(node: &crate::domain::graph::Node) -> Value {
    node.metadata
        .get(META_TRIGGER)
        .cloned()
        .unwrap_or(Value::Null)
}

/// Borrow the `action` descriptor JSON; returns a Null borrow when missing.
pub fn extract_action_value(node: &crate::domain::graph::Node) -> Value {
    node.metadata
        .get(META_ACTION)
        .cloned()
        .unwrap_or(Value::Null)
}

/// Read the `enabled` flag, defaulting to `true` when missing or non-boolean
/// (matches existing tolerant-listing semantics of `wire_workflow_list`).
pub fn extract_enabled(node: &crate::domain::graph::Node) -> bool {
    node.metadata
        .get(META_ENABLED)
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

// -- JSON → Entity -----------------------------------------------------------

/// Parse a JSON trigger descriptor into a typed [`Trigger`]. Surfaces a
/// structured `DomainError::InvalidSpec` on missing `kind` / unsupported
/// kind / missing `event` for `on_event`.
pub fn parse_trigger(value: &Value) -> WireResult<Trigger> {
    let kind = read_kind(value, "trigger")?;
    match kind.as_str() {
        "on_demand" => Ok(Trigger::OnDemand),
        "on_event" => {
            let event = read_string_field(value, "event", "trigger.event")?;
            Trigger::on_event(event)
        }
        other => Err(invalid_spec(format!(
            "trigger.kind '{other}' not supported in P5-a (allowed: {TRIGGER_KINDS_P5A:?})"
        ))),
    }
}

/// Parse a JSON action descriptor into a typed [`Action`]. Surfaces a
/// structured `DomainError::InvalidSpec` on missing `kind` / unsupported
/// kind / missing or empty `projection_names` for `emit_projection`.
pub fn parse_action(value: &Value) -> WireResult<Action> {
    let kind = read_kind(value, "action")?;
    match kind.as_str() {
        "no_op" => Ok(Action::NoOp),
        "emit_projection" => {
            let names = value
                .get("projection_names")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    invalid_spec(
                        "action.projection_names (array) is required for action.kind \
                         'emit_projection'",
                    )
                })?;
            let mut slots = Vec::with_capacity(names.len());
            for n in names {
                let name = n.as_str().ok_or_else(|| {
                    invalid_spec("action.projection_names entries must all be strings")
                })?;
                slots.push(Slot::new(name)?);
            }
            Action::emit_projection(slots)
        }
        other => Err(invalid_spec(format!(
            "action.kind '{other}' not supported in P5-a (allowed: {ACTION_KINDS_P5A:?})"
        ))),
    }
}

// -- Entity → JSON -----------------------------------------------------------

/// Render a [`Trigger`] to the persistence JSON shape.
pub fn trigger_to_json(t: &Trigger) -> Value {
    match t {
        Trigger::OnDemand => serde_json::json!({ "kind": "on_demand" }),
        Trigger::OnEvent { event } => serde_json::json!({
            "kind": "on_event",
            "event": event,
        }),
    }
}

/// Render an [`Action`] to the persistence JSON shape.
pub fn action_to_json(a: &Action) -> Value {
    match a {
        Action::NoOp => serde_json::json!({ "kind": "no_op" }),
        Action::EmitProjection { slots } => {
            let names: Vec<String> = slots.iter().map(|s| s.as_str().to_owned()).collect();
            serde_json::json!({
                "kind": "emit_projection",
                "projection_names": names,
            })
        }
    }
}

// -- Entity ↔ Node -----------------------------------------------------------

/// Translate a [`Workflow`] Entity into the persistence [`Node`] (Math
/// backend `workflow_def` shape).
pub fn workflow_to_node(w: &Workflow) -> Node {
    let mut metadata = Map::new();
    if let Some(p) = w.persona_id() {
        metadata.insert("persona".to_owned(), Value::String(p.as_str().to_owned()));
    }
    metadata.insert("trigger".to_owned(), trigger_to_json(w.trigger()));
    metadata.insert("action".to_owned(), action_to_json(w.action()));
    metadata.insert("enabled".to_owned(), Value::Bool(w.enabled()));
    Node {
        id: ulid_from_seed(w.id().as_str()),
        name: w.id().as_str().to_owned(),
        r#type: WORKFLOW_TYPE.to_owned(),
        sot_ref: None,
        confidence: None,
        applicability: None,
        last_verified_at: None,
        review_due: None,
        version: 1,
        prev_id: None,
        metadata: Value::Object(metadata),
    }
}

/// Translate a persisted [`Node`] back into a [`Workflow`] Entity. Surfaces
/// structured `DomainError::InvalidSpec` when the metadata shape is not a
/// valid workflow (= the Node was not produced by [`workflow_to_node`] or
/// has been corrupted).
pub fn node_to_workflow(node: &Node) -> WireResult<Workflow> {
    if node.r#type != WORKFLOW_TYPE {
        return Err(invalid_spec(format!(
            "Node.type '{}' is not '{WORKFLOW_TYPE}'",
            node.r#type
        )));
    }
    let id = WorkflowId::new(node.name.clone())?;
    let meta = node
        .metadata
        .as_object()
        .ok_or_else(|| invalid_spec("Node.metadata must be a JSON object"))?;

    let persona_id = match meta.get("persona") {
        Some(Value::String(s)) => Some(PersonaId::new(s.clone())?),
        Some(Value::Null) | None => None,
        Some(other) => {
            return Err(invalid_spec(format!(
                "metadata.persona must be a string, got: {other}"
            )));
        }
    };
    let trigger_value = meta
        .get("trigger")
        .ok_or_else(|| invalid_spec("metadata.trigger is required"))?;
    let trigger = parse_trigger(trigger_value)?;
    let action_value = meta
        .get("action")
        .ok_or_else(|| invalid_spec("metadata.action is required"))?;
    let action = parse_action(action_value)?;
    let enabled = meta
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    Ok(Workflow::new(id, persona_id, trigger, action, enabled))
}

// -- helpers -----------------------------------------------------------------

fn read_kind(value: &Value, label: &str) -> WireResult<String> {
    value
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| invalid_spec(format!("{label}.kind (string) is required")))
}

fn read_string_field(value: &Value, field: &str, label: &str) -> WireResult<String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| invalid_spec(format!("{label} (string) is required")))
}

fn invalid_spec(msg: impl Into<String>) -> WireError {
    WireError::Domain(DomainError::InvalidSpec(msg.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_workflow() -> Workflow {
        Workflow::new(
            WorkflowId::new("wf-mailbox").unwrap(),
            Some(PersonaId::new("test_persona_a").unwrap()),
            Trigger::on_event("mailbox.delivered").unwrap(),
            Action::emit_projection(vec![Slot::new("mailbox").unwrap()]).unwrap(),
            true,
        )
    }

    #[test]
    fn parse_trigger_on_demand() {
        let t = parse_trigger(&serde_json::json!({"kind": "on_demand"})).unwrap();
        assert!(matches!(t, Trigger::OnDemand));
    }

    #[test]
    fn parse_trigger_on_event_requires_event_field() {
        let err = parse_trigger(&serde_json::json!({"kind": "on_event"}))
            .expect_err("missing event must reject");
        assert!(err.to_string().contains("trigger.event"));
    }

    #[test]
    fn parse_trigger_rejects_unsupported_kind() {
        let err =
            parse_trigger(&serde_json::json!({"kind": "cron"})).expect_err("cron not in P5-a");
        assert!(err.to_string().contains("trigger.kind 'cron'"));
    }

    #[test]
    fn parse_action_emit_projection_requires_non_empty_names() {
        let err =
            parse_action(&serde_json::json!({"kind": "emit_projection", "projection_names": []}))
                .expect_err("empty must reject");
        assert!(err.to_string().contains("at least one slot"));
    }

    #[test]
    fn parse_action_rejects_non_string_projection_name() {
        let err =
            parse_action(&serde_json::json!({"kind": "emit_projection", "projection_names": [42]}))
                .expect_err("non-string must reject");
        assert!(err.to_string().contains("must all be strings"));
    }

    #[test]
    fn workflow_round_trip_through_node() {
        let w = sample_workflow();
        let node = workflow_to_node(&w);
        assert_eq!(node.r#type, WORKFLOW_TYPE);
        let back = node_to_workflow(&node).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn node_to_workflow_rejects_wrong_type() {
        let node = Node {
            id: ulid_from_seed("n1"),
            name: "n1".into(),
            r#type: "outline_node".into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: serde_json::json!({}),
        };
        let err = node_to_workflow(&node).expect_err("wrong type must reject");
        assert!(err.to_string().contains("workflow_def"));
    }

    #[test]
    fn workflow_to_node_round_trip_preserves_no_op_and_no_persona() {
        let w = Workflow::new(
            WorkflowId::new("wf-global").unwrap(),
            None,
            Trigger::OnDemand,
            Action::NoOp,
            false,
        );
        let back = node_to_workflow(&workflow_to_node(&w)).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn node_to_workflow_propagates_empty_workflow_id() {
        // 空 Node.name が WorkflowId::new の VO invariant に当たって reject される (DTO → Entity 経路の VO propagation)。
        let node = Node {
            id: ulid_from_seed(""),
            name: String::new(),
            r#type: WORKFLOW_TYPE.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata: serde_json::json!({
                "trigger": {"kind": "on_demand"},
                "action": {"kind": "no_op"},
                "enabled": true,
            }),
        };
        let err = node_to_workflow(&node).expect_err("empty workflow id must reject");
        assert!(err.to_string().to_lowercase().contains("workflow"));
    }
}
