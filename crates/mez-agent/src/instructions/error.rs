//! Typed validation errors for project instruction discovery.

use std::error::Error;
use std::fmt;

/// Stable category for an instruction discovery failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionDiscoveryErrorKind {
    /// Discovery configuration, paths, or escaped output are malformed.
    InvalidArgs,
}

/// Failure to plan or parse project instruction discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionDiscoveryError {
    message: String,
}

impl InstructionDiscoveryError {
    /// Constructs a malformed discovery input error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the stable error category.
    pub const fn kind(&self) -> InstructionDiscoveryErrorKind {
        InstructionDiscoveryErrorKind::InvalidArgs
    }

    /// Returns the human-readable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InstructionDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for InstructionDiscoveryError {}

/// Result type for instruction discovery parsing and planning.
pub type InstructionDiscoveryResult<T> = Result<T, InstructionDiscoveryError>;
