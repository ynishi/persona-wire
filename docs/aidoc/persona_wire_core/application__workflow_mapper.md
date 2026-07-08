# persona-wire-core::application::workflow_mapper

Mapper boundary: [`Workflow`] Domain Entity ↔ `workflow_def` [`Node`].

Fowler PoEAA Data Mapper — Node JSON metadata is the persistence form
(storage column-equivalent), [`Workflow`] is the Domain Entity carrying
invariants. This module is the **single SoT** for translating between
the two shapes; `wire_workflow_register` / `wire_workflow_list` (and any
future workflow use case) route through here instead of inlining
`metadata["trigger"]["kind"]` / `metadata["action"]["projection_names"]`
string surgery.

Storage form (cf. `domain/entity/workflow.rs` module docs):

```text
Node {
  id: "<workflow_id>",
  type: "workflow_def",
  metadata: {
    "persona":  Option<String>,
    "trigger":  { "kind": "on_demand" | "on_event", "event"?: String },
    "action":   { "kind": "no_op" | "emit_projection",
                  "projection_names"?: [<slot>] },
    "enabled":  bool,
  },
}
```

Round-trip property: `node_to_workflow(workflow_to_node(w))? == w` for
any [`Workflow`] constructed through this module's parsers.

## Functions

- `action_to_json` — Render an [`Action`] to the persistence JSON shape.
- `extract_action_value` — Borrow the `action` descriptor JSON; returns a Null borrow when missing.
- `extract_enabled` — Read the `enabled` flag, defaulting to `true` when missing or non-boolean
- `extract_persona` — Borrow the optional `persona` field as `&str`.
- `extract_trigger_value` — Borrow the `trigger` descriptor JSON; returns a Null borrow when missing.
- `node_to_workflow` — Translate a persisted [`Node`] back into a [`Workflow`] Entity. Surfaces
- `parse_action` — Parse a JSON action descriptor into a typed [`Action`]. Surfaces a
- `parse_trigger` — Parse a JSON trigger descriptor into a typed [`Trigger`]. Surfaces a
- `trigger_to_json` — Render a [`Trigger`] to the persistence JSON shape.
- `workflow_to_node` — Translate a [`Workflow`] Entity into the persistence [`Node`] (Math

## Constants

- `META_ACTION` — `metadata.action` key — action descriptor JSON.
- `META_ENABLED` — `metadata.enabled` key — enable flag (defaults to `true` on missing).
- `META_PERSONA` — `metadata.persona` key — owning persona (optional). Single SoT for the
- `META_TRIGGER` — `metadata.trigger` key — trigger descriptor JSON.
- `WORKFLOW_TYPE` — Storage `Node.r#type` literal for a Workflow. Single SoT — internal

