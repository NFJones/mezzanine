//! Provider-independent agent context validation contracts.
//!
//! This module owns deterministic validation failures for context blocks and
//! model-profile selection. Product prompt assets, persistence, and provider
//! execution remain outside this crate and adapt these errors at their
//! composition boundaries.

use std::fmt;

use crate::{AgentPromptError, AgentPromptErrorKind};

/// Result type returned by deterministic agent-context operations.
pub type AgentContextResult<T> = Result<T, AgentContextError>;

/// Result type returned while assembling one provider model request.
pub type AgentRequestAssemblyResult<T> = Result<T, AgentRequestAssemblyError>;

/// Stable categories for provider-independent request assembly failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRequestAssemblyErrorKind {
    /// A required request, context, or prompt-profile input was malformed.
    InvalidArgs,
    /// A product-supplied prompt asset was unavailable or invalid.
    InvalidState,
}

/// A typed failure returned while assembling one provider model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequestAssemblyError {
    kind: AgentRequestAssemblyErrorKind,
    message: String,
}

impl AgentRequestAssemblyError {
    /// Returns the stable request-assembly failure category.
    pub fn kind(&self) -> AgentRequestAssemblyErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<AgentContextError> for AgentRequestAssemblyError {
    fn from(error: AgentContextError) -> Self {
        Self {
            kind: AgentRequestAssemblyErrorKind::InvalidArgs,
            message: error.to_string(),
        }
    }
}

impl From<AgentPromptError> for AgentRequestAssemblyError {
    fn from(error: AgentPromptError) -> Self {
        let kind = match error.kind() {
            AgentPromptErrorKind::InvalidArgs => AgentRequestAssemblyErrorKind::InvalidArgs,
            AgentPromptErrorKind::InvalidState => AgentRequestAssemblyErrorKind::InvalidState,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for AgentRequestAssemblyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentRequestAssemblyError {}

/// A malformed provider-independent agent-context value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContextError {
    message: String,
}

impl AgentContextError {
    /// Creates a context contract error with a stable diagnostic message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentContextError {}

/// Validates one required context field after trimming surrounding whitespace.
pub fn validate_context_required(field: &str, value: &str) -> AgentContextResult<()> {
    if value.trim().is_empty() {
        return Err(AgentContextError::new(format!("{field} must not be empty")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentContextError, AgentRequestAssemblyError, AgentRequestAssemblyErrorKind,
        validate_context_required,
    };
    use crate::AgentPromptError;

    /// Required context validation accepts substantive values and rejects
    /// whitespace-only values with a stable field-specific diagnostic.
    #[test]
    fn context_required_validation_rejects_whitespace() {
        assert!(validate_context_required("model", "gpt-5").is_ok());
        let error = validate_context_required("model", " \t ").unwrap_err();
        assert_eq!(error.to_string(), "model must not be empty");
    }

    /// Request assembly preserves invalid-argument classification when either
    /// context validation or prompt-profile validation rejects an input.
    #[test]
    fn request_assembly_preserves_invalid_argument_errors() {
        let context_error = AgentRequestAssemblyError::from(AgentContextError::new("bad model"));
        let prompt_error =
            AgentRequestAssemblyError::from(AgentPromptError::invalid_args("bad profile"));

        assert_eq!(
            context_error.kind(),
            AgentRequestAssemblyErrorKind::InvalidArgs
        );
        assert_eq!(
            prompt_error.kind(),
            AgentRequestAssemblyErrorKind::InvalidArgs
        );
    }

    /// Request assembly retains invalid-state classification for failures in
    /// product-supplied prompt assets so the composition layer can adapt it.
    #[test]
    fn request_assembly_preserves_prompt_asset_errors() {
        let error =
            AgentRequestAssemblyError::from(AgentPromptError::invalid_state("asset missing"));

        assert_eq!(error.kind(), AgentRequestAssemblyErrorKind::InvalidState);
        assert_eq!(error.message(), "asset missing");
    }
}
