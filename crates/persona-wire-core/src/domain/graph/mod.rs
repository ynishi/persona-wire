//! Math backend Graph — open-vocabulary graph primitives.
//!
//! Pure graph primitives (Node / Edge / Severity / CRUD / Compute / Constraint /
//! AutoVersion / Repository / Specification). Tenant-agnostic, persona-agnostic.
//!
//! Domain knowledge (persona-wire vocabulary) lives in `domain::entity` and uses
//! this module as a backend SDK. See the crate-level "Three-layer split"
//! rationale in [`crate`] docs.

pub mod autoversion;
pub mod compute;
pub mod constraint;
pub mod crud;
pub mod node;
pub mod repository;
pub mod specification;

pub use node::*;
