//! Application layer — use cases and registries.
//!
//! Holds:
//! - [`spec_registry`]       — Specification registry (dynamic / composable axis)
//! - [`projection_registry`] — NamedProjection registry (fixed / named axis)
//! - [`use_cases`]           — pnet_init / pnet_close / pnet_doctor / etc. flows

pub mod projection_registry;
pub mod spec_registry;
pub mod use_cases;
