//! workflow.disabled — `enabled=false` の workflow を info で emit。
//!
//! design.md §7 entry。 意図確認用 (不要なら wire_workflow_delete)。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::use_cases::{wire_workflow_list, WireWorkflowListInput};
use crate::domain::error::WireResult;

pub struct WorkflowDisabled;

impl Probe for WorkflowDisabled {
    fn axis(&self) -> Axis {
        Axis::Workflow
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        let out = wire_workflow_list(
            WireWorkflowListInput {
                persona_id: ctx.persona_filter.clone(),
                trigger_kind: None,
                enabled_only: Some(false),
            },
            ctx.storage,
        )?;
        for w in out.workflows.into_iter().filter(|w| !w.enabled) {
            let kind = Kind::WorkflowDisabled;
            sink.push(Finding {
                severity: Severity::Info,
                axis: kind.axis(),
                kind,
                location: Location {
                    workflow_id: Some(w.id.clone()),
                    persona_id: w.persona_id,
                    ..Default::default()
                },
                description: format!("workflow `{}` is disabled (passive)", w.id),
                fix: format!(
                    "intentional? leave as-is; otherwise \
                     `mcp__persona-wire__wire_workflow_delete(id=\"{}\")`",
                    w.id
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::doctor::finding::Kind;
    use crate::application::doctor::test_helpers::*;
    use serde_json::json;

    #[test]
    fn emits_info_for_disabled_workflow() {
        let s = setup();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "on_demand"}),
            json!({"kind": "no_op"}),
            false,
        ))
        .unwrap();
        let f = scan(&WorkflowDisabled, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::WorkflowDisabled);
        assert_eq!(f[0].severity, Severity::Info);
    }

    #[test]
    fn quiet_for_enabled_workflow() {
        let s = setup();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "on_demand"}),
            json!({"kind": "no_op"}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowDisabled, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
