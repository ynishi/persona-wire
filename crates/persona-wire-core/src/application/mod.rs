//! Application layer — use cases and registries.
//!
//! Holds:
//! - [`spec_registry`]       — Specification registry (dynamic / composable axis)
//! - [`projection_registry`] — NamedProjection registry (fixed / named axis)
//! - [`use_cases`]           — wire_init / wire_close / wire_doctor / etc. flows

pub mod doctor;
pub mod merger;
pub mod plugin_registry;
pub mod projection;
pub mod projection_naming;
pub mod projection_overlay;
pub mod projection_registry;
pub mod spec_registry;
pub mod use_cases;
