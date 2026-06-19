//! workflow.duplicate_trigger — 同 persona × 同 trigger.event の重複検出 (warn)。
//!
//! design.md §7 entry。 統合 or 1 本に削減すべき状態。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::use_cases::{wire_workflow_list, WireWorkflowListInput};
use crate::domain::error::WireResult;
use std::collections::HashMap;

pub struct WorkflowDuplicateTrigger;

impl Probe for WorkflowDuplicateTrigger {
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

        // (persona, event) → Vec<workflow_id>
        let mut bucket: HashMap<(String, String), Vec<String>> = HashMap::new();
        for w in &out.workflows {
            let Some(event) = w.trigger.get("event").and_then(|v| v.as_str()) else {
                continue;
            };
            let persona = w.persona_id.clone().unwrap_or_default();
            bucket
                .entry((persona, event.to_string()))
                .or_default()
                .push(w.id.clone());
        }

        for ((persona, event), ids) in bucket {
            if ids.len() < 2 {
                continue;
            }
            let kind = Kind::WorkflowDuplicateTrigger;
            for id in &ids {
                sink.push(Finding {
                    severity: Severity::Warn,
                    axis: kind.axis(),
                    kind,
                    location: Location {
                        workflow_id: Some(id.clone()),
                        persona_id: if persona.is_empty() {
                            None
                        } else {
                            Some(persona.clone())
                        },
                        ..Default::default()
                    },
                    description: format!(
                        "workflow `{id}` shares trigger.event=`{event}` with \
                         {n_others} other workflow(s) under persona=`{persona}` \
                         (siblings: {siblings})",
                        n_others = ids.len() - 1,
                        siblings = ids
                            .iter()
                            .filter(|x| x.as_str() != id.as_str())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    fix: "consolidate into 1 workflow, or differentiate triggers".to_string(),
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
    fn emits_warn_for_dup_event_under_same_persona() {
        let s = setup();
        for id in ["wf_a", "wf_b"] {
            s.insert_node(&workflow_node(
                id,
                Some("alpha"),
                json!({"kind": "on_event", "event": "session_close"}),
                json!({"kind": "no_op"}),
                true,
            ))
            .unwrap();
        }
        let f = scan(&WorkflowDuplicateTrigger, &s, None).unwrap();
        // each member of the duplicate group emits 1 finding
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.kind == Kind::WorkflowDuplicateTrigger));
    }

    #[test]
    fn quiet_for_distinct_events() {
        let s = setup();
        s.insert_node(&workflow_node(
            "wf_a",
            Some("alpha"),
            json!({"kind": "on_event", "event": "session_close"}),
            json!({"kind": "no_op"}),
            true,
        ))
        .unwrap();
        s.insert_node(&workflow_node(
            "wf_b",
            Some("alpha"),
            json!({"kind": "on_event", "event": "session_open"}),
            json!({"kind": "no_op"}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowDuplicateTrigger, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
