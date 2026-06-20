//! Domain Entity Layer — persona-wire vocabulary as first-class entities.
//!
//! Sits on top of [`crate::domain::graph`] (Math backend SDK) and below the
//! application layer. See `docs/design/render-trinity-domain-entity.md`.
//!
//! ## Composition
//!
//! ```text
//! ContextWiring (Aggregate Root)
//!   ├ persona_id: PersonaId (owner ref)
//!   ├ wirings: Vec<Wiring>
//!   │   ├ source: Source
//!   │   └ projection: Projection
//!   └ workflows: Vec<Workflow>   (sibling of Wiring)
//! ```
//!
//! Step B lands skeleton modules. Field / invariant / behaviour land in
//! Step C, one entity at a time (PersonaId → Source → Projection → Wiring →
//! ContextWiring → Workflow).

pub mod context_wiring;
pub mod persona_id;
pub mod projection;
pub mod source;
pub mod wiring;
pub mod workflow;

pub use persona_id::PersonaId;
pub use projection::{
    PluginDispatch, Projection, ProjectionName, ProjectionTemplate, SpecRef, TargetForm,
};
pub use source::Source;
