//! Domain layer — pure entities, value objects, and business rules.
//!
//! ## Sub-layers (post-refactor — see `docs/design/render-trinity-domain-entity.md`)
//!
//! - [`graph`] — Math backend Graph (open-vocabulary primitives: Node / Edge /
//!   Severity / CRUD / Compute / Constraint / AutoVersion / Repository /
//!   Specification). Persona-agnostic. Used as a backend SDK by the Domain
//!   Entity layer.
//! - [`error`] — `WireError` / `WireResult` shared across the crate.
//!
//! Backward-compatible re-exports below keep `domain::specification`,
//! `domain::crud` etc. resolvable for existing call sites; Domain Entity
//! layer (`domain::entity`) lands in Step B.

pub mod error;
pub mod graph;

// Backward-compat re-exports — keep old `domain::<sub>` paths working
// until external call sites migrate. Will be revisited at 1.0 bump.
pub use graph::{autoversion, compute, constraint, crud, repository, specification};
