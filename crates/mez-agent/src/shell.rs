//! Provider-independent shell-source helpers used by agent action planning.
//!
//! This module owns deterministic shell text construction that does not read
//! product configuration, inspect the filesystem, or execute a process.

use std::fmt;
use std::path::Path;

/// Categorizes deterministic shell-source validation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentShellValidationErrorKind {
    /// A caller supplied invalid shell transaction input.
    InvalidArgs,
}

/// Reports invalid provider-independent shell transaction input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentShellValidationError {
    kind: AgentShellValidationErrorKind,
    message: String,
}

impl AgentShellValidationError {
    /// Creates an invalid-arguments shell validation failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: AgentShellValidationErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> AgentShellValidationErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentShellValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentShellValidationError {}

/// Result returned by provider-independent shell-source validation.
pub type AgentShellValidationResult<T> = Result<T, AgentShellValidationError>;

/// Validates the hexadecimal marker used to delimit one shell transaction.
pub fn validate_shell_marker_token(token: &str) -> AgentShellValidationResult<()> {
    if token.len() < 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AgentShellValidationError::invalid_args(
            "marker token must contain at least 128 bits encoded as 32 or more hex characters",
        ));
    }
    Ok(())
}

/// Validates that a transaction uses an absolute resolved shell path.
pub fn validate_resolved_shell_path(shell_path: &Path) -> AgentShellValidationResult<()> {
    if !shell_path.is_absolute() {
        return Err(AgentShellValidationError::invalid_args(
            "shell transaction wrapper requires an absolute resolved shell path",
        ));
    }
    Ok(())
}

/// Quotes one value as a POSIX shell word.
///
/// The returned text is safe to embed as one literal shell argument. Empty
/// values remain explicit empty arguments, and embedded single quotes use the
/// standard close-double-quote-reopen sequence.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::{
        AgentShellValidationErrorKind, shell_quote, validate_resolved_shell_path,
        validate_shell_marker_token,
    };
    use std::path::Path;

    /// Verifies shell quoting preserves empty values and embedded single
    /// quotes as one literal POSIX shell argument.
    #[test]
    fn shell_quote_preserves_literal_arguments() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("plain value"), "'plain value'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    /// Shell transaction validation accepts strong markers and absolute
    /// shell paths while rejecting malformed product inputs.
    #[test]
    fn shell_transaction_inputs_are_validated() {
        validate_shell_marker_token("0123456789abcdef0123456789abcdef")
            .expect("a 128-bit hexadecimal marker should be valid");
        validate_resolved_shell_path(Path::new("/bin/sh"))
            .expect("an absolute shell path should be valid");

        let marker_error = validate_shell_marker_token("not-hex")
            .expect_err("a short non-hexadecimal marker should fail");
        assert_eq!(
            marker_error.kind(),
            AgentShellValidationErrorKind::InvalidArgs
        );
        let path_error = validate_resolved_shell_path(Path::new("bin/sh"))
            .expect_err("a relative shell path should fail");
        assert_eq!(
            path_error.kind(),
            AgentShellValidationErrorKind::InvalidArgs
        );
    }
}
