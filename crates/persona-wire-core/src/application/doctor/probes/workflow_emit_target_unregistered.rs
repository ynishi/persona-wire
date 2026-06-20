//! workflow.emit_target_unregistered — `action.projection_names` axis を
//! runtime と同じ name derive rule で `<persona>.section.<axis>` に format
//! してから `ProjectionRegistry` を引き、 未登録なら error finding。
//!
//! design.md §7 entry。 emit しても受け手不在 = 永久空撃ち を検出する。
//!
//! `action.projection_names` の各 entry は literal name ではなく **axis 名**
//! (= use_cases::wire_prompt_context が `projection_naming::workflow_emit_projection_name`
//! で resolve する rule と同じ SoT を共有)。 旧実装はこの derive を行わず、
//! literal name を ProjectionRegistry に直接突き合わせて false positive を
//! 量産していた (origin: 2026-06-19 外形検証)。

use crate::application::doctor::finding::{Axis, Finding, Kind, Location, Severity};
use crate::application::doctor::probe::{FindingSink, Probe, ProbeCtx};
use crate::application::projection_naming::workflow_emit_projection_name;
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
            // persona_id 不在 workflow は名前 derive 不能。 別 kind で扱うべきだが
            // 現状は skip (MCP layer が emit_projection action では reject する path)。
            let Some(persona) = w.persona_id.as_deref() else {
                continue;
            };
            for n in names {
                let Some(axis) = n.as_str() else { continue };
                let resolved = workflow_emit_projection_name(persona, axis);
                if registered.contains(&resolved) {
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
                        projection_name: Some(resolved.clone()),
                        ..Default::default()
                    },
                    description: format!(
                        "workflow `{wid}` emits axis `{axis}` for persona `{persona}` \
                         but no NamedProjection `{resolved}` is registered",
                        wid = w.id,
                    ),
                    fix: format!(
                        "register: `mcp__persona-wire__wire_projection_register(name=\"{resolved}\", ...)` \
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
    use crate::domain::entity::projection::{PluginDispatch, Projection};
    use crate::domain::entity::TargetForm;
    use serde_json::json;

    #[test]
    fn emits_error_when_resolved_name_is_not_registered() {
        let s = setup();
        // axis "ghost" → resolved name "alpha.section.ghost" → not in registry
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
        assert_eq!(
            f[0].location.projection_name.as_deref(),
            Some("alpha.section.ghost"),
            "Probe must report the resolved <persona>.section.<axis> name"
        );
    }

    #[test]
    fn quiet_when_resolved_name_is_registered() {
        let s = setup();
        // pre-register a spec referenced by the named projection
        crate::application::spec_registry::SpecRegistry::new(&s)
            .register(
                "any_persona",
                &crate::domain::specification::Specification::TypeIs("persona".into()),
            )
            .unwrap();
        let reg = ProjectionRegistry::new(&s);
        // register the *resolved* name shape the runtime would look up.
        reg.register(
            &Projection::from_parts(
                "alpha.section.active",
                "any_persona",
                "x",
                TargetForm::Prompt,
                PluginDispatch::Default,
            )
            .unwrap(),
        )
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

    #[test]
    fn quiet_for_workflow_without_persona_id() {
        // Without persona_id we cannot derive `<persona>.section.<axis>`;
        // MCP layer rejects this anyway so the Probe stays quiet.
        let s = setup();
        s.insert_node(&workflow_node(
            "wf1",
            None,
            json!({"kind": "on_demand"}),
            json!({"kind": "emit_projection", "projection_names": ["active"]}),
            true,
        ))
        .unwrap();
        let f = scan(&WorkflowEmitTargetUnregistered, &s, None).unwrap();
        assert!(f.is_empty());
    }
}
