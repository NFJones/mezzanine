//! Typed errors for deterministic local-agent message protocol behavior.
//!
//! The messaging domain reports protocol and state failures without depending
//! on product error types. Transport adapters map these categories into their
//! own diagnostics while body dispatch uses them to construct MMP errors.

use std::error::Error;
use std::fmt;

/// Stable failure categories produced by MMP validation and service state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageErrorKind {
    /// An envelope, filter, cursor, or snapshot field is malformed.
    InvalidArgs,
    /// A valid request cannot be applied in the current service state.
    InvalidState,
    /// A request conflicts with previously accepted protocol state.
    Conflict,
    /// A requested agent or recipient does not exist or is unavailable.
    NotFound,
    /// Authenticated connection state does not authorize the operation.
    Forbidden,
}

/// One typed failure from MMP body processing or deterministic service state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageError {
    kind: MessageErrorKind,
    message: String,
}

impl MessageError {
    /// Constructs an error with an explicit stable category and diagnostic.
    pub fn new(kind: MessageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Constructs a malformed-request error.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(MessageErrorKind::InvalidArgs, message)
    }

    /// Constructs an invalid service-state error.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(MessageErrorKind::InvalidState, message)
    }

    /// Constructs a conflict with previously accepted state.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(MessageErrorKind::Conflict, message)
    }

    /// Constructs an unavailable-identity or recipient error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(MessageErrorKind::NotFound, message)
    }

    /// Constructs an authorization error.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(MessageErrorKind::Forbidden, message)
    }

    /// Returns the stable failure category.
    pub const fn kind(&self) -> MessageErrorKind {
        self.kind
    }

    /// Returns the human-readable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for MessageError {}

/// Result type for deterministic MMP operations.
pub type Result<T> = std::result::Result<T, MessageError>;
