//! Mapper boundary: [`Projection`] Domain Entity ↔ `projections` table row.
//!
//! Fowler PoEAA Data Mapper (Ch.10) — `NamedProjection` is the
//! persistence-shape DTO (anemic row mirror), [`Projection`] is the Domain
//! Entity carrying VO and cross-field invariants. This module is the
//! **single SoT** for translating between the two shapes;
//! [`ProjectionRegistry`](super::projection_registry::ProjectionRegistry)
//! and any future projection consumer route through here instead of
//! touching the DTO struct directly.
//!
//! # Pattern selection (SoT)
//!
//! Persona-wire takes the **narrow** reading of Data Mapper: the Registry
//! (PoEAA Ch.18) acts as the Mapper bridge through this module, instead of
//! introducing an independent `Mapper<Dto, Entity>` trait. See
//! [`projection_registry`](super::projection_registry) module docs for the
//! PoEAA Registry vs DDD Repository decision recorded in code.
//!
//! Promoting this to a literal Fowler Mapper trait is a carry that fires
//! only when a second parallel Mapper (Spec Mapper / overlay Mapper)
//! arrives and the inherent helpers start duplicating shape — until then,
//! the free functions below are intentionally not behind a trait.
//!
//! Sibling of [`wiring_mapper`](super::wiring_mapper) and
//! [`workflow_mapper`](super::workflow_mapper). The three together complete
//! the Data Mapper land for the entity layer.
//!
//! Storage form (cf. `domain/entity/projection.rs` module docs):
//!
//! ```text
//! Row {
//!   name:               String,
//!   spec_ref:           String,
//!   template:           String,
//!   target_form:        "prompt" | "markdown" | "ascii" | ...,
//!   template_engine:    Option<String>,
//!   projection_kind:    Option<String>,
//!   projection_config:  Option<Value>,   // JSON
//! }
//! ```
//!
//! `PluginDispatch` is flattened to the three optional columns at the DTO
//! boundary; the Entity carries the discriminated `Default | Custom { .. }`
//! shape so application code never sees the loose `Option` triple.
//!
//! Round-trip property: `dto_to_projection(projection_to_dto(p))? == p` for
//! any [`Projection`] constructed through its `from_parts` constructor.

use serde::{Deserialize, Serialize};

use crate::domain::entity::projection::{PluginDispatch, Projection};
use crate::domain::entity::TargetForm;
use crate::domain::error::WireResult;

/// Persistence DTO. Anemic by design — invariants live in [`Projection`].
///
/// Kept `pub` only because a few legacy callsites (doctor probes / tests)
/// still hand-build rows; new code should construct [`Projection`] and let
/// the mapper handle the DTO conversion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedProjection {
    pub name: String,
    pub spec_ref: String,
    pub template: String,
    pub target_form: TargetForm,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_config: Option<serde_json::Value>,
}

/// DTO → Domain Entity. Runs all VO validations and rejects illegal
/// `PluginDispatch` combinations at the mapper boundary.
pub fn dto_to_projection(dto: NamedProjection) -> WireResult<Projection> {
    let plugin = PluginDispatch::from_optional_parts(
        dto.template_engine,
        dto.projection_kind,
        dto.projection_config,
    )?;
    Projection::from_parts(
        dto.name,
        dto.spec_ref,
        dto.template,
        dto.target_form,
        plugin,
    )
}

/// Domain Entity → DTO. Total (no failure path) — Entity invariants are
/// strictly stronger than DTO shape, so projection is always defined.
pub fn projection_to_dto(p: &Projection) -> NamedProjection {
    let (engine, kind, config) = p.plugin().to_optional_parts();
    NamedProjection {
        name: p.name().as_str().to_owned(),
        spec_ref: p.spec_ref().as_str().to_owned(),
        template: p.template().as_str().to_owned(),
        target_form: p.target_form(),
        template_engine: engine.map(str::to_owned),
        projection_kind: kind.map(str::to_owned),
        projection_config: config.cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::{DomainError, WireError};

    fn sample_projection() -> Projection {
        Projection::from_parts(
            "_persona_toc",
            "active_personas",
            "Active personas ({{count}}): {{names}}",
            TargetForm::Prompt,
            PluginDispatch::Default,
        )
        .unwrap()
    }

    #[test]
    fn projection_to_dto_to_projection_roundtrip() {
        let p = sample_projection();
        let dto = projection_to_dto(&p);
        let back = dto_to_projection(dto).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn dto_to_projection_rejects_illegal_plugin_state() {
        let dto = NamedProjection {
            name: "p".into(),
            spec_ref: "s".into(),
            template: "t".into(),
            target_form: TargetForm::Prompt,
            template_engine: Some("handlebars".into()),
            projection_kind: None,
            projection_config: None,
        };
        let err = dto_to_projection(dto).expect_err("should reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn dto_to_projection_to_dto_roundtrip_default_plugin() {
        // Reverse direction: DTO → Entity → DTO equality on Default plugin shape.
        let original = NamedProjection {
            name: "_persona_toc".into(),
            spec_ref: "active_personas".into(),
            template: "Active personas ({{count}}): {{names}}".into(),
            target_form: TargetForm::Prompt,
            template_engine: None,
            projection_kind: None,
            projection_config: None,
        };
        let entity = dto_to_projection(original.clone()).unwrap();
        let back = projection_to_dto(&entity);
        assert_eq!(back, original);
    }

    #[test]
    fn roundtrip_preserves_custom_plugin_with_config() {
        // Custom plugin with config Value must round-trip without field loss.
        let cfg = serde_json::json!({"model": "claude-haiku", "temperature": 0.7});
        let p = Projection::from_parts(
            "render_llm",
            "active_personas",
            "{{prompt}}",
            TargetForm::Markdown,
            PluginDispatch::custom("handlebars", "llm", Some(cfg.clone())).unwrap(),
        )
        .unwrap();
        let dto = projection_to_dto(&p);
        // DTO 側に 3 Optional が全部 flatten されている。
        assert_eq!(dto.template_engine.as_deref(), Some("handlebars"));
        assert_eq!(dto.projection_kind.as_deref(), Some("llm"));
        assert_eq!(dto.projection_config, Some(cfg));
        // Entity round-trip 等価。
        let back = dto_to_projection(dto).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn dto_to_projection_propagates_vo_errors() {
        // 空 name / spec_ref / template が DTO 経由でも Domain VO で reject される。
        for (name, spec, tmpl) in [("", "s", "t"), ("n", "", "t"), ("n", "s", "")] {
            let dto = NamedProjection {
                name: name.into(),
                spec_ref: spec.into(),
                template: tmpl.into(),
                target_form: TargetForm::Prompt,
                template_engine: None,
                projection_kind: None,
                projection_config: None,
            };
            let err = dto_to_projection(dto).expect_err("VO empty must reject");
            assert!(matches!(
                err,
                WireError::Domain(DomainError::InvalidProjection(_))
            ));
        }
    }

    #[test]
    fn projection_to_dto_preserves_all_target_form_variants() {
        // TargetForm の 4 variant が DTO 側に丸ごと保たれること (target_form column 損失なし)。
        for tf in [
            TargetForm::Prompt,
            TargetForm::Markdown,
            TargetForm::Json,
            TargetForm::Ascii,
        ] {
            let p = Projection::from_parts("n", "s", "t", tf, PluginDispatch::Default).unwrap();
            let dto = projection_to_dto(&p);
            assert_eq!(dto.target_form, tf);
            let back = dto_to_projection(dto).unwrap();
            assert_eq!(back.target_form(), tf);
        }
    }
}
