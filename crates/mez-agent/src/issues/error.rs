//! Typed failures for canonical issue contracts.

use std::error::Error;
use std::fmt;

/// Failure to parse or validate an issue record, update, or query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueError {
    message: String,
}

impl IssueError {
    /// Constructs an invalid issue-contract error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the stable human-readable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for IssueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for IssueError {}

/// Result type for canonical issue contracts.
pub type IssueResult<T> = Result<T, IssueError>;
