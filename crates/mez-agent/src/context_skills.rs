//! Compatibility boundary for retired model-selected skill surface mutation.

use crate::ModelRequest;

/// Retains the session-owned action catalog after skill context is loaded.
///
/// Skill context can inform model behavior but cannot alter a provider request
/// schema after the session snapshot has been captured.
pub fn constrain_skill_actions_for_loaded_context(_request: &mut ModelRequest) {}
