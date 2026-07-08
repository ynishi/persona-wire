# persona-wire-core::domain::entity::workflow

`Workflow` Entity — trigger-driven autonomous binding within a persona's
context.

Sibling of [`crate::domain::entity::wiring::Wiring`]. Both are persisted
through the existing Math backend Repository (`Node` CRUD) and live
behind the Wire's external rendering surface ([`Projection`]); neither
is re-exported at the entity-module root. See the module-level "Surface
policy" / "Persistence" sections in [`crate::domain::entity`].

[`Projection`]: crate::domain::entity::projection::Projection

# Storage form (legacy bridge)

Persisted as a `Node` with `type = "workflow_def"` and metadata:

```text
Node {
  id: "<workflow_id>",
  type: "workflow_def",
  metadata: {
    "persona":  Option<String>,
    "trigger":  { "kind": "on_demand" | "on_event", "event"?: String },
    "action":   { "kind": "no_op" | "emit_projection", "projection_names"?: [<slot>] },
    "enabled":  bool,
  },
}
```

The mapper boundary (application use cases that build / read this Node)
is responsible for translating `Vec<Slot>` ↔ `metadata["projection_names"]`.

# Trigger / Action vocabulary

- Triggers: `OnDemand`, `OnEvent { event }`
- Actions:  `NoOp`, `EmitProjection { slots: Vec<Slot> }`

The entity holds [`Slot`] directly for the action target. Some legacy
callsites still describe the same field as an "axis name" — that is the
jargon predating the entity layer (see [`crate::domain::entity::slot`]
module docs); the entity converges on `Slot`, and the mapper layer
reconciles the wire-format `Vec<String>` until the storage rename is
performed.

Future-only variants (e.g. cron / metadata_changed triggers, set_metadata
/ fire_mailbox actions) are intentionally **not** added until they have
an actual use case.

## Types

- `Action` — Workflow action — what the workflow does when it fires.
- `Trigger` — Workflow trigger — what causes the workflow to fire.
- `Workflow` — Workflow Domain Entity.
- `WorkflowId` — Workflow surrogate identifier Value Object. Non-empty.

