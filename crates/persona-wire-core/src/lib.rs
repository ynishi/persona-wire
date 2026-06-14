//! persona-wire-core
//!
//! Transport-agnostic core for persona-wire.
//!
//! Layer split (DDD + Hexagonal):
//! - [`domain`]         — pure entities + value objects + business rules
//! - [`application`]    — use cases, registries (Specification / NamedProjection)
//! - [`infrastructure`] — SQLite storage adapter, Rendering adapter
//!
//! No MCP / CLI deps. Both `persona-wire-mcp` and `persona-wire-cli`
//! depend on this crate and adapt their own transport surface.

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use domain::error::WireError;
pub use domain::error::WireResult;
