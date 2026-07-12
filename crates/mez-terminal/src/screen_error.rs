//! Configuration errors for one emulated terminal screen.
//!
//! Screen construction and live configuration expose terminal-owned errors so
//! product adapters can classify failures without coupling emulation to the
//! Mezzanine error aggregate.

use std::error::Error;
use std::fmt;

use crate::HistoryConfigError;

/// Reports invalid configuration for one emulated terminal screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalScreenConfigError {
    message: &'static str,
}

impl TerminalScreenConfigError {
    /// Returns the stable validation message for product-level error adapters.
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl From<HistoryConfigError> for TerminalScreenConfigError {
    fn from(error: HistoryConfigError) -> Self {
        Self {
            message: error.message(),
        }
    }
}

impl fmt::Display for TerminalScreenConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for TerminalScreenConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HistoryBuffer;

    /// Verifies screen configuration preserves terminal history diagnostics.
    #[test]
    fn screen_config_error_preserves_history_validation_message() {
        let history_error = HistoryBuffer::new(0).unwrap_err();
        let screen_error = TerminalScreenConfigError::from(history_error);

        assert_eq!(
            screen_error.message(),
            "history buffer limit must be greater than zero"
        );
    }
}
