//! Time helpers used by audit writers and retention enforcement.
//!
//! Timestamps are stored as `unix:<seconds>` strings so retention can operate
//! without a full timestamp parser.

use std::time::{SystemTime, UNIX_EPOCH};

/// Runs the unix seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Runs the current timestamp operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn current_timestamp() -> String {
    let seconds = unix_seconds(SystemTime::now());
    format!("unix:{seconds}")
}

/// Runs the record timestamp seconds operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn record_timestamp_seconds(line: &str) -> Option<u64> {
    let marker = r#""timestamp":"unix:"#;
    let start = line.find(marker)? + marker.len();
    let rest = line.get(start..)?;
    let end = rest.find('"')?;
    rest.get(..end)?.parse().ok()
}
