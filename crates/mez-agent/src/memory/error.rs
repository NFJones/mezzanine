//! Typed validation and codec failures for canonical memory records.

use std::error::Error;
use std::fmt;

/// Failure to validate or decode a canonical memory record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecordError {
    message: String,
}

impl MemoryRecordError {
    /// Constructs a malformed memory record or codec input error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the human-readable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MemoryRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for MemoryRecordError {}

/// Result type for canonical memory records and codecs.
pub type MemoryRecordResult<T> = Result<T, MemoryRecordError>;
