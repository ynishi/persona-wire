//! Mapper boundary: [`Wiring`] Domain Entity ↔ wiring [`Node`].
//!
//! Fowler PoEAA Data Mapper — `Node.metadata` JSON is the persistence form
//! (storage column-equivalent), [`Wiring`] is the Domain Entity carrying
//! invariants. This module is the **single SoT** for translating between
//! the two shapes; `wire_init` / `wire_render` / `wire_prompt_context` and
//! the doctor probes route through here instead of inlining
//! `metadata.get("axis")` / `metadata.get("source_uri")` string surgery.
//!
//! Storage form (cf. `domain/entity/wiring.rs` module docs):
//!
//! ```text
//! Node {
//!   id: "<persona>.<slot>",        // natural composite key
//!   type: "outline_node",
//!   metadata: {
//!     "persona":     String,
//!     "axis":        String,       // legacy storage key for Slot name
//!     "source_uri":  String,
//!     "maintenance_exempt"?: bool, // self-attach signal (orphan suppression)
//!     ...other passthrough fields...
//!   },
//! }
//! ```
//!
//! The legacy `metadata["axis"]` key carries the [`Slot`] name (see
//! `domain/entity/slot.rs` docs for the axis → Slot vocabulary split). The
//! storage rename to `metadata["slot"]` is a separate persistence migration;
//! until then this module is the only place where the legacy key is read /
//! written, so callsites in `use_cases.rs` / doctor probes / tests are free
//! of the bare `"axis"` literal.
//!
//! Round-trip property: `node_to_wiring(wiring_to_node(w, opts)?)? == w`
//! for any [`Wiring`] constructed through this module's parsers (modulo the
//! `projection_ref` carry, which is stored separately at the
//! `ProjectionRegistry` boundary, not on the wiring Node).

use serde_json::{Map, Value};

use crate::domain::entity::wiring::Wiring;
use crate::domain::entity::{PersonaId, Slot, Source};
use crate::domain::error::{DomainError, WireResult};
use crate::domain::graph::Node;

// -- Storage constants ------------------------------------------------------

/// Storage `Node.r#type` literal for a Wiring entry. Single SoT — internal
/// use-case code and tests reference this constant instead of re-typing the
/// string.
pub const WIRING_TYPE: &str = "outline_node";

/// `metadata.persona` key (PersonaId, natural composite key part 1).
pub const META_PERSONA: &str = "persona";

/// `metadata.axis` key — legacy storage name for the [`Slot`] (natural
/// composite key part 2). The external Domain vocabulary is `Slot`; this
/// `axis` key is the **only** place where the legacy jargon lives, pending
/// the storage rename.
pub const META_SLOT: &str = "axis";

/// `metadata.source_uri` key ([`Source`] URI).
pub const META_SOURCE_URI: &str = "source_uri";

/// `metadata.maintenance_exempt` key — opt-out flag for session-cyclic
/// maintenance (used by `is_self_attached_wiring`).
pub const META_MAINTENANCE_EXEMPT: &str = "maintenance_exempt";

// -- Node → Entity (extract helpers, tolerant) ------------------------------

/// Borrow the `persona` field as `&str` if present and a string.
pub fn extract_persona(node: &Node) -> Option<&str> {
    node.metadata.get(META_PERSONA).and_then(Value::as_str)
}

/// Borrow the slot field (legacy key `axis`) as `&str` if present and a
/// string. Returns the slot name without applying the [`Slot`] non-empty /
/// no-dot invariants — for typed extraction use [`extract_slot_typed`].
pub fn extract_slot(node: &Node) -> Option<&str> {
    node.metadata.get(META_SLOT).and_then(Value::as_str)
}

/// Borrow the `source_uri` field as `&str` if present and a string.
pub fn extract_source_uri(node: &Node) -> Option<&str> {
    node.metadata.get(META_SOURCE_URI).and_then(Value::as_str)
}

/// Read the `maintenance_exempt` flag, defaulting to `false` when missing
/// or not a boolean.
pub fn extract_maintenance_exempt(node: &Node) -> bool {
    node.metadata
        .get(META_MAINTENANCE_EXEMPT)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Validate-and-extract the slot as a typed [`Slot`] VO. Returns `Ok(None)`
/// when the key is missing; `Err(DomainError::InvalidMetadata)` when the
/// value is present but violates [`Slot`] invariants.
pub fn extract_slot_typed(node: &Node) -> WireResult<Option<Slot>> {
    match extract_slot(node) {
        Some(s) => Slot::new(s.to_string()).map(Some),
        None => Ok(None),
    }
}

// -- Node → Entity (strict, full Wiring) ------------------------------------

/// Strict mapper: build a typed [`Wiring`] from a wiring [`Node`].
///
/// `projection_ref` is supplied by the caller because the wiring Node does
/// **not** persist the projection ref — that lives at the
/// `ProjectionRegistry` boundary, looked up via `projection_naming` rules.
/// Pass `None` when the projection ref is not (yet) known.
///
/// Errors with `DomainError::InvalidMetadata` when required keys
/// (`persona` / `axis` / `source_uri`) are missing or violate the VO
/// invariants of [`PersonaId`] / [`Slot`] / [`Source`].
pub fn node_to_wiring(
    node: &Node,
    projection_ref: Option<crate::domain::entity::ProjectionName>,
) -> WireResult<Wiring> {
    let persona = extract_persona(node).ok_or_else(|| {
        DomainError::InvalidMetadata(format!(
            "wiring node '{}' missing metadata.{META_PERSONA}",
            node.name
        ))
    })?;
    let slot = extract_slot(node).ok_or_else(|| {
        DomainError::InvalidMetadata(format!(
            "wiring node '{}' missing metadata.{META_SLOT}",
            node.name
        ))
    })?;
    let source = extract_source_uri(node).ok_or_else(|| {
        DomainError::InvalidMetadata(format!(
            "wiring node '{}' missing metadata.{META_SOURCE_URI}",
            node.name
        ))
    })?;
    Ok(Wiring::new(
        PersonaId::new(persona.to_string())?,
        Slot::new(slot.to_string())?,
        Source::new(source.to_string())?,
        projection_ref,
    ))
}

// -- Entity → Node metadata (build) ----------------------------------------

/// Build a wiring `Node.metadata` object from the natural composite key
/// parts. `extras` is merged shallowly on top (later keys win), allowing
/// callers to attach `maintenance_exempt` or arbitrary passthrough fields
/// without re-typing the canonical key names.
pub fn wiring_metadata_object(
    persona: &PersonaId,
    slot: &Slot,
    source: &Source,
    extras: Option<Map<String, Value>>,
) -> Value {
    let mut m = Map::new();
    m.insert(
        META_PERSONA.to_string(),
        Value::String(persona.as_str().into()),
    );
    m.insert(META_SLOT.to_string(), Value::String(slot.as_str().into()));
    m.insert(
        META_SOURCE_URI.to_string(),
        Value::String(source.as_str().into()),
    );
    if let Some(extra) = extras {
        for (k, v) in extra {
            m.insert(k, v);
        }
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::entity::ProjectionName;
    use crate::domain::graph::ulid_from_seed;
    use serde_json::json;

    fn raw_node(id: &str, metadata: Value) -> Node {
        Node {
            id: ulid_from_seed(id),
            name: id.into(),
            r#type: WIRING_TYPE.into(),
            sot_ref: None,
            confidence: None,
            applicability: None,
            last_verified_at: None,
            review_due: None,
            version: 1,
            prev_id: None,
            metadata,
        }
    }

    #[test]
    fn extract_helpers_read_canonical_keys() {
        let n = raw_node(
            "shi.mailbox",
            json!({
                "persona": "shi",
                "axis": "mailbox",
                "source_uri": "mini-app://mailbox?alias=for_shi",
                "maintenance_exempt": true,
            }),
        );
        assert_eq!(extract_persona(&n), Some("shi"));
        assert_eq!(extract_slot(&n), Some("mailbox"));
        assert_eq!(
            extract_source_uri(&n),
            Some("mini-app://mailbox?alias=for_shi")
        );
        assert!(extract_maintenance_exempt(&n));
    }

    #[test]
    fn extract_maintenance_exempt_defaults_false() {
        let n = raw_node("a.b", json!({}));
        assert!(!extract_maintenance_exempt(&n));
    }

    #[test]
    fn extract_slot_typed_validates_invariants() {
        let bad = raw_node("a.b", json!({"axis": "a.b"}));
        let err = extract_slot_typed(&bad).expect_err("dot must reject");
        assert!(matches!(
            err,
            crate::domain::error::WireError::Domain(DomainError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn node_to_wiring_strict_round_trip() {
        let persona = PersonaId::new("shi").unwrap();
        let slot = Slot::new("mailbox").unwrap();
        let source = Source::new("mini-app://mailbox?alias=for_shi").unwrap();
        let meta = wiring_metadata_object(&persona, &slot, &source, None);
        let node = raw_node("shi.mailbox", meta);
        let projection_ref = Some(ProjectionName::new("shi.section.mailbox").unwrap());
        let w = node_to_wiring(&node, projection_ref.clone()).unwrap();
        assert_eq!(w.persona_id(), &persona);
        assert_eq!(w.slot(), &slot);
        assert_eq!(w.source(), &source);
        assert_eq!(w.projection_ref(), projection_ref.as_ref());
        assert_eq!(w.storage_node_id(), "shi.mailbox");
    }

    #[test]
    fn node_to_wiring_rejects_missing_keys() {
        let n = raw_node("x.y", json!({"persona": "x"}));
        let err = node_to_wiring(&n, None).expect_err("missing axis must reject");
        assert!(matches!(
            err,
            crate::domain::error::WireError::Domain(DomainError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn wiring_metadata_object_merges_extras() {
        let persona = PersonaId::new("p").unwrap();
        let slot = Slot::new("s").unwrap();
        let source = Source::new("mini-app://x").unwrap();
        let mut extras = Map::new();
        extras.insert("maintenance_exempt".into(), json!(true));
        let v = wiring_metadata_object(&persona, &slot, &source, Some(extras));
        assert_eq!(v["persona"], "p");
        assert_eq!(v["axis"], "s");
        assert_eq!(v["source_uri"], "mini-app://x");
        assert_eq!(v["maintenance_exempt"], true);
    }

    #[test]
    fn node_to_wiring_rejects_missing_persona_and_source() {
        // missing axis 以外の reject path: persona 欠落 / source_uri 欠落 をそれぞれ独立検証。
        let no_persona = raw_node(
            "x.y",
            json!({"axis": "mailbox", "source_uri": "mini-app://x"}),
        );
        let err = node_to_wiring(&no_persona, None).expect_err("missing persona must reject");
        assert!(matches!(
            err,
            crate::domain::error::WireError::Domain(DomainError::InvalidMetadata(_))
        ));
        assert!(err.to_string().contains("persona"));

        let no_source = raw_node("x.y", json!({"persona": "x", "axis": "mailbox"}));
        let err = node_to_wiring(&no_source, None).expect_err("missing source must reject");
        assert!(matches!(
            err,
            crate::domain::error::WireError::Domain(DomainError::InvalidMetadata(_))
        ));
        assert!(err.to_string().contains("source_uri"));
    }
}
