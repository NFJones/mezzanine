//! Product error adapter for mux command-language parsing.
//!
//! Canonical invocation records, tokenization, quoting, and sequence parsing
//! live in `mez-mux`. This module converts lower invalid-argument failures into
//! the product error used by runtime and configuration consumers.

use super::{CommandInvocation, MezError, Result};

/// Parses one or more semicolon-separated command invocations.
pub fn parse_command_sequence(input: &str) -> Result<Vec<CommandInvocation>> {
    mez_mux::command::parse_command_sequence(input)
        .map_err(|error| MezError::invalid_args(error.message()))
}
