//! Typed failures for canonical transcript contracts.

use std::error::Error;
use std::fmt;

/// Error returned when a canonical transcript contract is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptContractError {
    message: String,
}

impl TranscriptContractError {
    /// Constructs a malformed transcript contract error.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TranscriptContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TranscriptContractError {}
