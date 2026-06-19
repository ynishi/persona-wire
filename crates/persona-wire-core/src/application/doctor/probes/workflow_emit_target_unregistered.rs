//! workflow.emit_target_unregistered — `action.projection_names` に
//! `wire_projection_register` 済でない名前が含まれる (error)。
//!
//! design.md §7 entry。 emit しても受け手不在 = 永久空撃ち。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::projection_registry::ProjectionRegistry;
use crate::application::use_cases::{wire_workflow_list, WireWorkflowListInput};
use crate::domain::error::WireResult;
use std::collections::HashSet;

pub struct WorkflowEmitTargetUnregistered;

impl Probe for WorkflowEmitTargetUnregistered {
    fn axis(&self) -> Axis {
        Axis::Workflow
    }

    fn scan(&self, ctx: &ProbeCtx, sink: &mut FindingSink) -> WireResult<()> {
        let registered: HashSet<String> = ProjectionRegistry::new(ctx.storage)
            .list()?
            .into_iter()
            .collect();

        let out = wire_workflow_list(
            WireWorkflowListInput {
                persona_id: ctx.persona_filter.clone(),
                trigger_kind: None,
                enabled_only: Some(false),
            },
            ctx.storage,
        )?;

        for w in out.workflows {
            if w.action.get("kind").and_then(|v| v.as_str()) != Some("emit_projection") {
                continue;
            }
            let Some(names) = w.action.get("projection_names").and_then(|v| v.as_array()) else {
                continue;
            };
            for n in names {
                let Some(name) = n.as_str() else { continue };
                if registered.contains(name) {
                    continue;
                }
                let kind = Kind::WorkflowEmitTargetUnregistered;
                sink.push(Finding {
                    severity: Severity::Error,
                    axis: kind.axis(),
                    kind,
                    location: Location {
                        workflow_id: Some(w.id.clone()),
                        persona_id: w.persona_id.clone(),
                        projection_name: Some(name.to_string()),
                        ..Default::default()
                    },
                    description: format!(
                        "workflow `{wid}` action.projection_names contains `{name}` \
                         but no projection by that name is registered",
                        wid = w.id,
                    ),
                    fix: format!(
                        "register: `mcp__persona-wire__wire_projection_register(name=\"{name}\", ...)` \
                         or fix workflow action.projection_names"
                    ),
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
    use crate::application::projection_registry::NamedProjection;
    use serde_json::json;

    #[test]
    fn emits_error_for_unregistered_projection_name() {
        let s = setup();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "on_demand"}),
            json!({"kind": "emit_projection", "projection_names": ["ghost"]}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowEmitTargetUnregistered, &s, None).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::WorkflowEmitTargetUnregistered);
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[0].location.projection_name.as_deref(), Some("ghost"));
    }

    #[test]
    fn quiet_when_projection_is_registered() {
        let s = setup();
        // pre-register a spec referenced by the named projection
        crate::application::spec_registry::SpecRegistry::new(&s)
            .register(
                "any_persona",
                &crate::domain::specification::Specification::TypeIs("persona".into()),
            )
            .unwrap();
        let reg = ProjectionRegistry::new(&s);
        reg.register(&NamedProjection {
            name: "active".into(),
            spec_ref: "any_persona".into(),
            template: "x".into(),
            target_form: crate::application::projection_registry::TargetForm::Prompt,
            template_engine: None,
            projection_kind: None,
            projection_config: None,
        })
        .unwrap();
        s.insert_node(&workflow_node(
            "wf1",
            Some("alpha"),
            json!({"kind": "on_demand"}),
            json!({"kind": "emit_projection", "projection_names": ["active"]}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowEmitTargetUnregistered, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
