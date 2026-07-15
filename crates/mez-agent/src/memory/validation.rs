//! Validation helpers for canonical memory records.
//!
//! The validation layer centralizes scope ownership and non-empty constraints.

use super::{MemoryScope, MezError, Result};

/// Runs the validate scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_scope(scope: &MemoryScope) -> Result<()> {
    match scope {
        MemoryScope::Global => Ok(()),
        MemoryScope::Project { root } => validate_non_empty("project memory root", root),
        MemoryScope::Session { session_id } => validate_non_empty("session memory id", session_id),
        MemoryScope::Window {
            session_id,
            window_id,
        } => {
            validate_non_empty("window memory session id", session_id)?;
            validate_non_empty("window memory id", window_id)
        }
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => {
            validate_non_empty("pane memory session id", session_id)?;
            validate_non_empty("pane memory id", pane_id)
        }
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => {
            validate_non_empty("agent memory session id", session_id)?;
            validate_non_empty("agent memory id", agent_id)
        }
    }
}

/// Runs the scope belongs to session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn scope_belongs_to_session(scope: &MemoryScope, session_id: &str) -> bool {
    match scope {
        MemoryScope::Session { session_id: id }
        | MemoryScope::Window { session_id: id, .. }
        | MemoryScope::Pane { session_id: id, .. }
        | MemoryScope::Agent { session_id: id, .. } => id == session_id,
        MemoryScope::Global | MemoryScope::Project { .. } => false,
    }
}

/// Runs the validate non empty operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_non_empty(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(MezError::invalid_args(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}
