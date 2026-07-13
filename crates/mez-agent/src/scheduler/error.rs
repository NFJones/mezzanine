//! Scheduler-specific error contracts.
//!
//! The scheduler reports stable, dependency-neutral categories so product
//! adapters can preserve their own aggregate error representation.

use std::error::Error;
use std::fmt;

/// Stable scheduler failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerErrorKind {
    /// A caller supplied malformed scheduler input.
    InvalidArgs,
    /// Internal scheduler state violated an expected invariant.
    InvalidState,
    /// A turn identifier conflicts with active scheduler state.
    Conflict,
    /// The requested turn identifier is not present.
    NotFound,
}

/// A typed scheduler failure with a human-readable diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerError {
    kind: SchedulerErrorKind,
    message: String,
}

impl SchedulerError {
    /// Creates a scheduler error in the supplied category.
    pub fn new(kind: SchedulerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Creates an invalid-arguments scheduler error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(SchedulerErrorKind::InvalidArgs, message)
    }

    /// Creates an invalid-state scheduler error.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(SchedulerErrorKind::InvalidState, message)
    }

    /// Creates a conflict scheduler error.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(SchedulerErrorKind::Conflict, message)
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> SchedulerErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for SchedulerError {}

/// Result type returned by scheduler operations.
pub type SchedulerResult<T> = Result<T, SchedulerError>;
