//! Application layer — use cases and registries.
//!
//! Holds:
//! - [`spec_registry`]       — Specification registry (dynamic / composable selector)
//! - [`projection_registry`] — NamedProjection registry (fixed / named view)
//! - [`use_cases`]           — wire_init / wire_close / wire_doctor / etc. flows

pub mod bundle_install;
pub mod bundle_registry;
pub mod doctor;
pub mod merger;
pub mod plugin_registry;
pub mod projection_mapper;
pub mod projection_naming;
pub mod projection_overlay;
pub mod projection_registry;
pub mod spec_registry;
pub mod use_cases;
pub mod wiring_mapper;
pub mod workflow_mapper;
