//! Projection name derivation — single SoT for `(persona_id, axis)` → registered
//! NamedProjection name mapping.
//!
//! Consumers:
//! - `wire_prompt_context` (use_cases.rs) at runtime resolve
//! - `WorkflowEmitTargetUnregistered` doctor Probe at check time
//!
//! Keeping this rule in one place prevents the doctor Probe from
//! re-implementing (and getting wrong) the format the use case actually
//! applies — origin of the false-positive in `workflow.emit_target_unregistered`.

/// Compute the NamedProjection name that the runtime looks up in
/// `ProjectionRegistry` for a given persona + emit axis.
///
/// Workflow `action.projection_names` entries are **axis names**, not
/// literal projection names; the runtime derives `<persona>.section.<axis>`
/// before hitting the registry.
pub fn workflow_emit_projection_name(persona_id: &str, axis: &str) -> String {
    format!("{persona_id}.section.{axis}")
}
