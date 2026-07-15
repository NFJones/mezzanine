//! Typed failures from deterministic permission policy operations.

use std::error::Error;
use std::fmt;

/// Stable permission-engine failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionErrorKind {
    /// A rule, command, scope, request, or encoded record is malformed.
    InvalidArgs,
    /// A request conflicts with an existing decision or rule.
    Conflict,
    /// A requested approval or rule does not exist.
    NotFound,
}

/// One deterministic permission-engine failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionError {
    kind: PermissionErrorKind,
    message: String,
}

impl PermissionError {
    /// Constructs an error with an explicit stable category.
    pub fn new(kind: PermissionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Constructs a malformed-input error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(PermissionErrorKind::InvalidArgs, message)
    }

    /// Constructs a conflicting-state error.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(PermissionErrorKind::Conflict, message)
    }

    /// Constructs a missing-record error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(PermissionErrorKind::NotFound, message)
    }

    /// Returns the stable failure category.
    pub const fn kind(&self) -> PermissionErrorKind {
        self.kind
    }

    /// Returns the human-readable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PermissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for PermissionError {}

/// Result type for deterministic permission operations.
pub type PermissionResult<T> = Result<T, PermissionError>;
