//! Subagent cooperation modes and write-scope coordination.
//!
//! Subagents always receive their own shell, but concurrent write access must be
//! coordinated before any worker mutates persistent state. This module validates
//! spawn requests and tracks active write scopes independently from the future
//! pane-creation control call.

/// Exposes the scope registry module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod scope_registry;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;
/// Exposes the validation module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod validation;

pub use types::{
    ActiveWriteScope, BuiltinSubagentRole, CooperationMode, SUBAGENT_FRIENDLY_NAMES, ScopeConflict,
    ScopeRegistry, SubagentProfile, SubagentScopeDeclaration, SubagentSpawnRequest,
};
pub use validation::{SubagentScopeEnforcement, builtin_role_name, builtin_subagent_profiles};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
