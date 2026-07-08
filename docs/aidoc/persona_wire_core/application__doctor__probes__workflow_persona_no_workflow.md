# persona-wire-core::application::doctor::probes::workflow_persona_no_workflow

workflow.persona_no_workflow — persona に紐づく workflow が 1 件もない (warn)。

design.md §7 entry。 session_close 等の lifecycle hook 不在で更新が回らない。

NOTE: 全 persona 列挙は `metadata.persona` を持つ node を横串で収集。
persona registry が別途 SoT で確定するなら将来 carry で置換可。

## Types

- `WorkflowPersonaNoWorkflow` — (no documentation)

