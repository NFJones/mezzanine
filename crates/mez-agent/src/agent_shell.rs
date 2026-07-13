//! Provider-independent agent-shell session error contracts.
//!
//! This module owns deterministic validation and state-transition failures for
//! pane-local agent-shell sessions. Runtime orchestration, persistence, and
//! product error aggregation remain outside this crate and adapt these errors
//! at composition boundaries.

use std::fmt;

/// Result type returned by agent-shell session operations.
pub type AgentShellSessionResult<T> = Result<T, AgentShellSessionError>;

/// Stable category for an agent-shell session failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentShellSessionErrorKind {
    /// A caller supplied a malformed value or mismatched turn identifier.
    InvalidArgs,
    /// The requested pane-local session does not exist.
    NotFound,
    /// The requested transition conflicts with current session state.
    Conflict,
    /// An internal session-state invariant was not preserved.
    InvalidState,
}

/// A deterministic agent-shell session failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentShellSessionError {
    kind: AgentShellSessionErrorKind,
    message: String,
}

impl AgentShellSessionError {
    /// Creates a session failure with a stable category and diagnostic.
    pub fn new(kind: AgentShellSessionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Creates an invalid-argument session failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(AgentShellSessionErrorKind::InvalidArgs, message)
    }

    /// Creates a missing-session failure.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AgentShellSessionErrorKind::NotFound, message)
    }

    /// Creates a conflicting-transition session failure.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(AgentShellSessionErrorKind::Conflict, message)
    }

    /// Creates an invalid-state session failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(AgentShellSessionErrorKind::InvalidState, message)
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentShellSessionErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentShellSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentShellSessionError {}

/// Validates one required agent-shell field after trimming whitespace.
pub fn validate_agent_shell_required(field: &str, value: &str) -> AgentShellSessionResult<()> {
    if value.trim().is_empty() {
        return Err(AgentShellSessionError::invalid_args(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentShellSessionError, AgentShellSessionErrorKind, validate_agent_shell_required,
    };

    /// Agent-shell validation rejects whitespace-only identifiers with a
    /// stable invalid-argument category and field-specific diagnostic.
    #[test]
    fn agent_shell_required_validation_rejects_whitespace() {
        assert!(validate_agent_shell_required("pane id", "%1").is_ok());
        let error = validate_agent_shell_required("pane id", " \t ").unwrap_err();
        assert_eq!(error.kind(), AgentShellSessionErrorKind::InvalidArgs);
        assert_eq!(error.message(), "pane id must not be empty");
    }

    /// Session error constructors preserve stable categories so the product
    /// boundary can convert them without parsing diagnostic text.
    #[test]
    fn agent_shell_session_errors_preserve_categories() {
        assert_eq!(
            AgentShellSessionError::not_found("session missing").kind(),
            AgentShellSessionErrorKind::NotFound
        );
        assert_eq!(
            AgentShellSessionError::conflict("turn running").kind(),
            AgentShellSessionErrorKind::Conflict
        );
        assert_eq!(
            AgentShellSessionError::invalid_state("session lost").kind(),
            AgentShellSessionErrorKind::InvalidState
        );
    }
}
