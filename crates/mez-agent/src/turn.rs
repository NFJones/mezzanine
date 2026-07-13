//! Provider-independent agent turn-ledger error contracts.
//!
//! This module owns deterministic validation and state-transition failures for
//! agent turn ledgers. Runtime scheduling, persistence, and product error
//! aggregation remain outside this crate and adapt these errors at composition
//! boundaries.

use std::fmt;

/// Result type returned by agent turn-ledger operations.
pub type AgentTurnLedgerResult<T> = Result<T, AgentTurnLedgerError>;

/// Stable category for an agent turn-ledger failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnLedgerErrorKind {
    /// A caller supplied a malformed value or invalid target state.
    InvalidArgs,
    /// The requested turn does not exist.
    NotFound,
    /// The requested transition conflicts with current ledger state.
    Conflict,
}

/// A deterministic agent turn-ledger failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnLedgerError {
    kind: AgentTurnLedgerErrorKind,
    message: String,
}

impl AgentTurnLedgerError {
    /// Creates a ledger failure with a stable category and diagnostic message.
    pub fn new(kind: AgentTurnLedgerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Creates an invalid-argument ledger failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(AgentTurnLedgerErrorKind::InvalidArgs, message)
    }

    /// Creates a missing-turn ledger failure.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AgentTurnLedgerErrorKind::NotFound, message)
    }

    /// Creates a conflicting-transition ledger failure.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(AgentTurnLedgerErrorKind::Conflict, message)
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentTurnLedgerErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentTurnLedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentTurnLedgerError {}

/// Validates one required turn-ledger field after trimming whitespace.
pub fn validate_turn_required(field: &str, value: &str) -> AgentTurnLedgerResult<()> {
    if value.trim().is_empty() {
        return Err(AgentTurnLedgerError::invalid_args(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AgentTurnLedgerError, AgentTurnLedgerErrorKind, validate_turn_required};

    /// Turn-ledger validation rejects whitespace-only identifiers with the
    /// stable invalid-argument category and a field-specific diagnostic.
    #[test]
    fn turn_required_validation_rejects_whitespace() {
        assert!(validate_turn_required("turn_id", "turn-1").is_ok());
        let error = validate_turn_required("turn_id", " \t ").unwrap_err();
        assert_eq!(error.kind(), AgentTurnLedgerErrorKind::InvalidArgs);
        assert_eq!(error.message(), "turn_id must not be empty");
    }

    /// Ledger constructors preserve stable categories for composition-layer
    /// conversion without requiring callers to parse diagnostic text.
    #[test]
    fn turn_ledger_errors_preserve_categories() {
        assert_eq!(
            AgentTurnLedgerError::not_found("turn not found").kind(),
            AgentTurnLedgerErrorKind::NotFound
        );
        assert_eq!(
            AgentTurnLedgerError::conflict("turn already exists").kind(),
            AgentTurnLedgerErrorKind::Conflict
        );
    }
}
