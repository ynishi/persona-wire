//! Domain layer — pure entities, value objects, and business rules.
//!
//! ## Sub-layers
//!
//! - [`graph`] — Math backend Graph (open-vocabulary primitives: Node / Edge /
//!   Severity / CRUD / Compute / Constraint / AutoVersion / Repository /
//!   Specification). Persona-agnostic. Used as a backend SDK by the Domain
//!   Entity layer.
//! - [`entity`] — Domain Entity layer (`PersonaId` / `Slot` / `Source` /
//!   `Wiring` / `Workflow` / `Projection`). Owns persona-wire vocabulary,
//!   invariants, and behavior.
//! - [`error`] — `WireError` / `WireResult` shared across the crate.
//!
//! See the crate-level "Three-layer split" rationale in [`crate`] docs for
//! the design intent behind the split.
//!
//! Backward-compatible re-exports below keep `domain::specification`,
//! `domain::crud` etc. resolvable for existing call sites.

pub mod entity;
pub mod error;
pub mod graph;
pub mod port;

// Backward-compat re-exports — keep old `domain::<sub>` paths working
// until external call sites migrate. Will be revisited at 1.0 bump.
pub use graph::{autoversion, compute, constraint, crud, repository, specification};
