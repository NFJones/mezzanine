//! Typed failures for MCP protocol and registry policy.

use std::error::Error;
use std::fmt;

/// Stable category for one MCP domain failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorKind {
    /// Caller supplied a malformed contract.
    InvalidArgs,
    /// Protocol or registry state violated an invariant.
    InvalidState,
    /// Policy or availability denied the operation.
    Forbidden,
    /// The requested server or tool does not exist.
    NotFound,
}

/// Failure returned by storage-independent MCP contracts and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    kind: McpErrorKind,
    message: String,
}

impl McpError {
    /// Constructs a malformed MCP argument error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(McpErrorKind::InvalidArgs, message)
    }

    /// Constructs an invalid MCP protocol or registry state error.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(McpErrorKind::InvalidState, message)
    }

    /// Constructs an MCP policy or availability denial.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(McpErrorKind::Forbidden, message)
    }

    /// Constructs a missing MCP server or tool error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(McpErrorKind::NotFound, message)
    }

    /// Returns the stable error category.
    pub fn kind(&self) -> McpErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without consuming the typed error.
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: McpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for McpError {}

/// Result type for MCP protocol and registry policy.
pub type McpResult<T> = Result<T, McpError>;
