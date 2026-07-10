# persona-wire-core::application::wiring_mapper

Mapper boundary: [`Wiring`] Domain Entity ↔ wiring [`Node`].

Fowler PoEAA Data Mapper — `Node.metadata` JSON is the persistence form
(storage column-equivalent), [`Wiring`] is the Domain Entity carrying
invariants. This module is the **single SoT** for translating between
the two shapes; `wire_init` / `wire_render` / `wire_prompt_context` and
the doctor probes route through here instead of inlining
`metadata.get("axis")` / `metadata.get("source_uri")` string surgery.

Storage form (cf. `domain/entity/wiring.rs` module docs):

```text
Node {
  id: "<persona>.<slot>",        // natural composite key
  type: "outline_node",
  metadata: {
    "persona":     String,
    "axis":        String,       // legacy storage key for Slot name
    "source_uri":  String,
    "maintenance_exempt"?: bool, // self-attach signal (orphan suppression)
    ...other passthrough fields...
  },
}
```

The legacy `metadata["axis"]` key carries the [`Slot`] name (see
`domain/entity/slot.rs` docs for the axis → Slot vocabulary split). The
storage rename to `metadata["slot"]` is a separate persistence migration;
until then this module is the only place where the legacy key is read /
written, so callsites in `use_cases.rs` / doctor probes / tests are free
of the bare `"axis"` literal.

Round-trip property: `node_to_wiring(wiring_to_node(w, opts)?)? == w`
for any [`Wiring`] constructed through this module's parsers (modulo the
`projection_ref` carry, which is stored separately at the
`ProjectionRegistry` boundary, not on the wiring Node).

## Functions

- `extract_auth` — Borrow the `auth` field (credential reference key, never a secret) as
- `extract_maintenance_exempt` — Read the `maintenance_exempt` flag, defaulting to `false` when missing
- `extract_persona` — Borrow the `persona` field as `&str` if present and a string.
- `extract_slot` — Borrow the slot field (legacy key `axis`) as `&str` if present and a
- `extract_slot_typed` — Validate-and-extract the slot as a typed [`Slot`] VO. Returns `Ok(None)`
- `extract_source_uri` — Borrow the `source_uri` field as `&str` if present and a string.
- `node_to_wiring` — Strict mapper: build a typed [`Wiring`] from a wiring [`Node`].
- `wiring_metadata_object` — Build a wiring `Node.metadata` object from the natural composite key

## Constants

- `META_AUTH` — `metadata.auth` key — optional credential **reference key** (never a
- `META_MAINTENANCE_EXEMPT` — `metadata.maintenance_exempt` key — opt-out flag for session-cyclic
- `META_PERSONA` — `metadata.persona` key (PersonaId, natural composite key part 1).
- `META_SLOT` — `metadata.axis` key — legacy storage name for the [`Slot`] (natural
- `META_SOURCE_URI` — `metadata.source_uri` key ([`Source`] URI).
- `WIRING_TYPE` — Storage `Node.r#type` literal for a Wiring entry. Single SoT — internal

