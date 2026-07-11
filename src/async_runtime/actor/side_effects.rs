//! Async runtime side-effect conversion and coalescing helpers.
//!
//! This module owns the pure transformations that convert deferred runtime
//! service work into actor side effects. Keeping these helpers separate leaves
//! the actor facade focused on request handling, event application, and queue
//! draining while preserving the existing side-effect ordering contracts.

use super::*;

/// Runs the deferred pane inputs to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_inputs_to_side_effects(
    deferred_pane_inputs: Vec<DeferredPaneInput>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_inputs
        .into_iter()
        .map(|input| {
            if input.priority {
                RuntimeSideEffect::WritePaneInputPriority {
                    pane_id: input.pane_id,
                    bytes: input.bytes,
                }
            } else {
                RuntimeSideEffect::WritePaneInput {
                    pane_id: input.pane_id,
                    bytes: input.bytes,
                }
            }
        })
        .collect()
}

/// Runs the deferred pane resizes to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_resizes_to_side_effects(
    deferred_pane_resizes: Vec<(String, DeferredPaneResize)>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_resizes
        .into_iter()
        .map(|(pane_id, resize)| RuntimeSideEffect::ResizePane {
            pane_id,
            size: resize.size,
        })
        .collect()
}

/// Runs the deferred pane terminations to side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn deferred_pane_terminations_to_side_effects(
    deferred_pane_terminations: Vec<(String, DeferredPaneTermination)>,
) -> Vec<RuntimeSideEffect> {
    deferred_pane_terminations
        .into_iter()
        .map(|(pane_id, termination)| RuntimeSideEffect::TerminatePane {
            pane_id,
            force: termination.force,
        })
        .collect()
}
