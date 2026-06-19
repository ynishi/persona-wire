//! workflow.persona_no_workflow — persona に紐づく workflow が 1 件もない (warn)。
//!
//! design.md §7 entry。 session_close 等の lifecycle hook 不在で更新が回らない。
//!
//! NOTE: 全 persona 列挙は `metadata.persona` を持つ node を横串で収集。
//! persona registry が別途 SoT で確定するなら将来 carry で置換可。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::use_cases::{wire_workflow_list, WireWorkflowListInput};
use crate::domain::error::WireResult;
use std::collections::HashSet;

pub struct WorkflowPersonaNoWorkflow;

impl Probe for WorkflowPersonaNoWorkflow {
    fn axis(&self) -> Axis {
        Axis::Workflow
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        let storage = ctx.storage;

        // 1. 観察可能な persona 集合: 全 node の metadata.persona を union。
        //    persona-scoped mode では当該 persona 1 件のみに絞る。
        let mut personas: HashSet<String> = HashSet::new();
        if let Some(ref p) = ctx.persona_filter {
            personas.insert(p.clone());
        } else {
            for t in storage.list_types_by_kind("node")? {
                for n in storage.list_nodes_by_type(&t)? {
                    if let Some(p) = n
                        .metadata
                        .as_object()
                        .and_then(|m| m.get("persona"))
                        .and_then(|v| v.as_str())
                    {
                        personas.insert(p.to_string());
                    }
                }
            }
        }

        // 2. 各 persona の workflow 件数を引いて 0 件を emit。
        for persona in personas {
            let out = wire_workflow_list(
                WireWorkflowListInput {
                    persona_id: Some(persona.clone()),
                    trigger_kind: None,
                    enabled_only: Some(false),
                },
                storage,
            )?;
            if !out.workflows.is_empty() {
                continue;
            }
            let kind = Kind::WorkflowPersonaNoWorkflow;
            sink.push(Finding {
                severity: Severity::Warn,
                axis: kind.axis(),
                kind,
                location: Location {
                    persona_id: Some(persona.clone()),
                    ..Default::default()
                },
                description: format!(
                    "persona `{persona}` has zero registered workflows \
                     (no session_close / lifecycle hook to drive updates)"
                ),
                fix: format!(
                    "`mcp__persona-wire__wire_workflow_register(id=\"...\", persona_id=\"{persona}\", ...)`"
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
    fn emits_warn_for_persona_without_workflow() {
        let s = setup();
        s.insert_node(&persona_node("p_lonely", "alpha")).unwrap();
        let f = scan(&WorkflowPersonaNoWorkflow, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::WorkflowPersonaNoWorkflow);
        assert_eq!(f[0].severity, Severity::Warn);
        assert_eq!(f[0].location.persona_id.as_deref(), Some("alpha"));
    }

    #[test]
    fn quiet_when_persona_has_a_workflow() {
        let s = setup();
        s.insert_node(&persona_node("p1", "alpha")).unwrap();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "on_demand"}),
            json!({"kind": "no_op"}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowPersonaNoWorkflow, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
