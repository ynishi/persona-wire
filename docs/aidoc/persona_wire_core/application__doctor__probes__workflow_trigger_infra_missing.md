# persona-wire-core::application::doctor::probes::workflow_trigger_infra_missing

workflow.trigger_infra_missing — trigger.kind=cron で cron 指定なし、
または kind=on_event で event 文字列なし (error)。

design.md §7 entry。 「受け手不在 / 永遠未 fire」 の構造的検出。
NOTE: `on_event` の hook 実在検査 (agent-profiles 側の登録) は本 Probe
scope 外、 将来 carry。 ここでは event 文字列の完備性のみ。

## Types

- `WorkflowTriggerInfraMissing` — (no documentation)

