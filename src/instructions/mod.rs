//! Project instruction discovery planning.
//!
//! The live harness must discover project guidance through the pane shell so
//! container and SSH contexts are honored. This module builds a POSIX-compatible
//! shell command that walks from the task directory to the project root, checks
//! configured instruction filenames in precedence order, and emits escaped TSV
//! records that can be parsed after execution.

/// Exposes the parser module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod parser;
/// Exposes the planning module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod planning;
/// Exposes the shell module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod shell;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use parser::parse_instruction_discovery_output;
pub use planning::plan_instruction_discovery;
pub use types::{InstructionDiscoveryConfig, InstructionDiscoveryPlan};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
