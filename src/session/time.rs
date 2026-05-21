//! Shared session timestamp helpers.
//!
//! Session state is intentionally in-memory, so lifecycle timestamps are
//! captured from the host clock at mutation boundaries and serialized by the
//! control layer when no runtime-owned timestamp overlay is available.

use std::time::{SystemTime, UNIX_EPOCH};

/// Runs the current unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
