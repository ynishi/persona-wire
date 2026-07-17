//! workflow.trigger_infra_missing — trigger.kind=cron で cron 指定なし、
//! または kind=on_event で event 文字列なし (error)。
//!
//! design.md §7 entry。 「受け手不在 / 永遠未 fire」 の構造的検出。
//! NOTE: `on_event` の hook 実在検査 (external hook registration 側) は本
//! Probe scope 外、 将来 carry。 ここでは event 文字列の完備性のみ。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::use_cases::{wire_workflow_list, WireWorkflowListInput};
use crate::domain::error::WireResult;

pub struct WorkflowTriggerInfraMissing;

impl Probe for WorkflowTriggerInfraMissing {
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

        for w in out.workflows {
            let trigger_kind = w.trigger.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let problem: Option<&str> = match trigger_kind {
                "cron" => {
                    let has_spec = w
                        .trigger
                        .get("cron")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);
                    if has_spec {
                        None
                    } else {
                        Some("trigger.kind=cron without trigger.cron spec — will never fire")
                    }
                }
                "on_event" => {
                    let has_event = w
                        .trigger
                        .get("event")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);
                    if has_event {
                        None
                    } else {
                        Some("trigger.kind=on_event without trigger.event name — receiver-less")
                    }
                }
                _ => None,
            };
            if let Some(desc) = problem {
                let kind = Kind::WorkflowTriggerInfraMissing;
                sink.push(Finding {
                    severity: Severity::Error,
                    axis: kind.axis(),
                    kind,
                    location: Location {
                        workflow_id: Some(w.id.clone()),
                        persona_id: w.persona_id,
                        ..Default::default()
                    },
                    description: format!("workflow `{}`: {}", w.id, desc),
                    fix: "fix trigger spec (add `cron` / `event`) or change trigger.kind"
                        .to_string(),
                });
            }
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
    fn emits_error_for_cron_without_spec() {
        let s = setup();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "cron"}),
            json!({"kind": "no_op"}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowTriggerInfraMissing, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::WorkflowTriggerInfraMissing);
        assert_eq!(f[0].severity, Severity::Error);
    }

    #[test]
    fn quiet_for_on_demand_trigger() {
        let s = setup();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "on_demand"}),
            json!({"kind": "no_op"}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowTriggerInfraMissing, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
