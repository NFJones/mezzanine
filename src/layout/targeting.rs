//! Pane targeting predicates for layout navigation and selection.
//!
//! Targeting is kept separate from window mutation so callers can reason about
//! selection behavior without touching pane geometry updates.

use super::Pane;

/// Runs the pane matches target operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn pane_matches_target(pane: &Pane, target: &str) -> bool {
    pane.id.as_str() == target || pane.index.to_string() == target
}
