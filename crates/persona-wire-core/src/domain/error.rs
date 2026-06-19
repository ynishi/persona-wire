//! Core error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WireError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid specification: {0}")]
    InvalidSpec(String),

    #[error("constraint violation: {0}")]
    ConstraintViolation(String),

    #[error("type not registered: {0}")]
    UnknownType(String),

    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),

    #[error("other: {0}")]
    Other(String),
}

pub type WireResult<T> = Result<T, WireError>;
