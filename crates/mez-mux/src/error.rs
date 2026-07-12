//! Multiplexer-domain errors.
//!
//! The mux crate reports stable error categories without depending on the
//! product-level error aggregate. Product adapters convert these errors at the
//! composition boundary while preserving their category and diagnostic text.

use thiserror::Error;

/// Result type returned by multiplexer-domain operations.
pub type Result<T> = std::result::Result<T, MuxError>;

/// Stable category for a multiplexer-domain failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxErrorKind {
    /// A supplied command argument or restored value is invalid.
    InvalidArgs,
    /// The requested mutation is incompatible with current mux state.
    InvalidState,
    /// A requested pane, window, group, or client does not exist.
    NotFound,
}

/// Error returned by multiplexer-domain operations.
#[derive(Debug, Error)]
#[error("{kind:?}: {message}")]
pub struct MuxError {
    kind: MuxErrorKind,
    message: String,
}

impl MuxError {
    /// Creates an error with a stable category and user-facing diagnostic.
    pub fn new(kind: MuxErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Creates an invalid-argument error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(MuxErrorKind::InvalidArgs, message)
    }

    /// Creates an invalid-state error.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(MuxErrorKind::InvalidState, message)
    }

    /// Returns this error's stable category.
    pub fn kind(&self) -> MuxErrorKind {
        self.kind
    }

    /// Returns this error's diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::{MuxError, MuxErrorKind};

    /// Verifies mux errors retain their stable category and diagnostic so the
    /// product adapter can translate them without parsing display text.
    #[test]
    fn mux_error_preserves_kind_and_message() {
        let error = MuxError::invalid_state("window has no panes");

        assert_eq!(error.kind(), MuxErrorKind::InvalidState);
        assert_eq!(error.message(), "window has no panes");
        assert_eq!(error.to_string(), "InvalidState: window has no panes");
    }
}
