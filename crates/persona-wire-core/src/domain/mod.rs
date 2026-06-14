//! Domain layer — pure entities, value objects, and business rules.
//!
//! 6 primitive (v4 BP 採用):
//! - [`graph`]         Node + Edge entity (open vocabulary)
//! - [`crud`]          create / read / update / delete commands
//! - [`compute`]       traversal + execution + constraint eval
//! - [`constraint`]    edge-as-constraint evaluation
//! - [`autoversion`]   append-only version chain
//! - [`specification`] first-class composable query object (BP: Specification pattern)

pub mod autoversion;
pub mod compute;
pub mod constraint;
pub mod crud;
pub mod error;
pub mod graph;
pub mod specification;
