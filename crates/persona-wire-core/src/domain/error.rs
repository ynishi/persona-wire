//! Error types layered by responsibility.
//!
//! - [`DomainError`] — pure domain failures (invalid entity construction,
//!   constraint violations, unresolved references). Step C carry: every
//!   `domain::entity::*` constructor returns `Result<_, DomainError>`.
//! - [`WireError`] — top-level facade that wraps `DomainError` via `From`
//!   plus residual infrastructure / catch-all variants (`Storage` / `Other`).
//!   Application + Infrastructure layers may surface either layer's error.
//!
//! Future split (Application / Infrastructure dedicated enums) is carry —
//! current scope keeps `Storage` / `Other` flat under `WireError`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("invalid persona id: {0}")]
    InvalidPersonaId(String),

    #[error("invalid source uri: {0}")]
    InvalidSource(String),

    #[error("invalid specification: {0}")]
    InvalidSpec(String),

    #[error("invalid projection: {0}")]
    InvalidProjection(String),

    #[error("invalid target_form: {0}")]
    InvalidTargetForm(String),

    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),

    #[error("constraint violation: {0}")]
    ConstraintViolation(String),

    #[error("type not registered: {0}")]
    UnknownType(String),

    #[error("not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("other: {0}")]
    Other(String),
}

pub type WireResult<T> = Result<T, WireError>;
