# persona-wire-core::application::doctor::probes::workflow_emit_target_unregistered

workflow.emit_target_unregistered — `action.projection_names` slot を
runtime と同じ name derive rule で `<persona>.section.<slot>` に format
してから `ProjectionRegistry` を引き、 未登録なら error finding。

design.md §7 entry。 emit しても受け手不在 = 永久空撃ち を検出する。

`action.projection_names` の各 entry は literal name ではなく **slot 名**
(= use_cases::wire_prompt_context が `projection_naming::workflow_emit_projection_name`
で resolve する rule と同じ SoT を共有)。 旧実装はこの derive を行わず、
literal name を ProjectionRegistry に直接突き合わせて false positive を
量産していた (origin: 2026-06-19 外形検証)。

## Types

- `WorkflowEmitTargetUnregistered` — (no documentation)

