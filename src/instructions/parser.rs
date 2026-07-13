//! Product error adapter for dependency-neutral instruction record parsing.
//!
//! The escaped record grammar belongs to `mez-agent`; this module preserves
//! Mezzanine's aggregate error contract for product-owned discovery callers.

use crate::error::{MezError, Result};
use mez_agent::instructions::DiscoveredInstructionFile;

/// Parses escaped instruction discovery output through the agent contract.
pub fn parse_instruction_discovery_output(output: &str) -> Result<Vec<DiscoveredInstructionFile>> {
    mez_agent::instructions::parse_instruction_discovery_output(output)
        .map_err(MezError::invalid_args)
}
