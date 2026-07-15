//! Subagent cooperation modes and write-scope coordination.
//!
//! Subagents always receive their own shell, but concurrent write access must be
//! coordinated before any worker mutates persistent state. This module validates
//! spawn requests and tracks active write scopes independently from the future
//! pane-creation control call.

/// Exposes product-friendly subagent display names.
///
/// The canonical subagent domain is owned by `mez-agent`; only product
/// presentation naming remains in this module.
mod names;
/// Exposes the validation module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod validation;

pub use names::SUBAGENT_FRIENDLY_NAMES;
pub use validation::{
    AGENT_SUBAGENT_SCOPE_ENFORCEMENT, ProductSubagentScopeEnforcement, SubagentScopeEnforcement,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
