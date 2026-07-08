# persona-wire-core::domain::entity::slot

`Slot` Value Object — 1 binding name within a persona's context.

A `Slot` identifies a single [`crate::domain::entity::wiring::Wiring`]
inside one persona. Concrete values seen in production are `mailbox` /
`mail` / `news` / `priorities` etc. — each is **one binding name**, not
an orthogonal axis. The legacy storage shape (and several application
callsites) carries this same concept under the field name `axis`; that
is a jargon symptom from before the entity layer existed — `mailbox` and
`mail` are not direction-of-variance "axes", they are sibling slots in
one persona's context. `Slot` is the proper Domain vocabulary.

# Invariants

- **non-empty**
- **no `.`** — the natural composite key with `PersonaId` is rendered as
  `format!("{persona_id}.{slot}")` at the storage boundary (`Node.id`).
  Allowing `.` inside a slot name would make the concatenation ambiguous.

Character set / length bounds beyond the above are intentionally left to
the persistence boundary so future storage rename / id scheme migrations
stay local.

## Types

- `Slot` — Wiring slot name Value Object.

